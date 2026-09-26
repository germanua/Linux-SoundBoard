fn apply_batch(connection: &mut Connection, batch: LibraryBatch) -> Result<(), LibraryError> {
    let transaction = connection.transaction()?;
    match batch {
        LibraryBatch::Roots(rows) => insert_roots(&transaction, rows)?,
        LibraryBatch::Folders(rows) => insert_folders(&transaction, rows)?,
        LibraryBatch::Sounds(rows) => insert_sounds(&transaction, rows)?,
        LibraryBatch::ManualTabs(rows) => insert_manual_tabs(&transaction, rows)?,
        LibraryBatch::ManualMemberships(rows) => insert_manual_memberships(&transaction, rows)?,
        LibraryBatch::LegacyGeneratedTabs(rows) => {
            insert_legacy_generated_tabs(&transaction, rows)?
        }
        LibraryBatch::LegacyGeneratedMemberships(rows) => {
            insert_legacy_generated_memberships(&transaction, rows)?
        }
        LibraryBatch::FolderOverrides(rows) => insert_folder_overrides(&transaction, rows)?,
        LibraryBatch::HotkeyBindings(rows) => insert_hotkey_bindings(&transaction, rows)?,
    }
    transaction.commit()?;
    Ok(())
}

fn scan_meta_key(root_path: &str) -> String {
    format!("active_scan:{root_path}")
}

fn begin_root_scan(
    connection: &mut Connection,
    root_path: &str,
    position: usize,
) -> Result<i64, LibraryError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO roots(path, position) VALUES(?1, ?2)
         ON CONFLICT(path) DO UPDATE SET position = excluded.position",
        params![root_path, usize_to_i64(position)?],
    )?;
    let root_id: i64 =
        transaction.query_row("SELECT id FROM roots WHERE path = ?1", [root_path], |row| {
            row.get(0)
        })?;
    let highest: i64 = transaction
        .query_row(
            "SELECT max(generation) FROM (
             SELECT active_generation AS generation FROM roots WHERE id = ?1
             UNION ALL
             SELECT generation FROM folder_presence AS presence
             JOIN folders AS folder ON folder.id = presence.folder_id
             WHERE folder.root_id = ?1
             UNION ALL
             SELECT generation FROM sound_locations WHERE root_id = ?1
         )",
            [root_id],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .unwrap_or(0);
    let generation = highest
        .checked_add(1)
        .ok_or_else(|| LibraryError::InvalidData("root generation overflow".to_string()))?;
    transaction.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![scan_meta_key(root_path), generation.to_string()],
    )?;
    transaction.commit()?;
    Ok(generation)
}

fn verify_root_scan(
    connection: &Connection,
    root_path: &str,
    generation: i64,
) -> Result<bool, LibraryError> {
    Ok(connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [scan_meta_key(root_path)],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|value| value == generation.to_string()))
}

fn ensure_scan_folder(
    transaction: &Transaction<'_>,
    root_id: i64,
    generation: i64,
    row: &FolderRecord,
) -> Result<i64, LibraryError> {
    let components = Path::new(&row.relative_path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut parent_id = None;
    let mut relative = PathBuf::new();
    let mut folder_id = None;
    for (index, component) in components.iter().enumerate() {
        relative.push(component);
        let relative_path = relative.to_string_lossy();
        let is_target = index + 1 == components.len();
        let name = if is_target { &row.name } else { component };
        let position = if is_target { row.position } else { 0 };
        transaction.execute(
            "INSERT INTO folders(root_id, parent_id, relative_path, name, position)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(root_id, relative_path) DO UPDATE SET
                 parent_id = excluded.parent_id,
                 name = CASE WHEN ?6 THEN excluded.name ELSE folders.name END,
                 position = CASE WHEN ?6 THEN excluded.position ELSE folders.position END",
            params![
                root_id,
                parent_id,
                relative_path.as_ref(),
                name,
                usize_to_i64(position)?,
                is_target,
            ],
        )?;
        let current_id: i64 = transaction.query_row(
            "SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2",
            params![root_id, relative_path.as_ref()],
            |result| result.get(0),
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO folder_presence(folder_id, generation) VALUES(?1, ?2)",
            params![current_id, generation],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO folder_closure(ancestor_id, descendant_id, depth)
             VALUES(?1, ?1, 0)",
            [current_id],
        )?;
        if let Some(parent) = parent_id {
            transaction.execute(
                "INSERT OR IGNORE INTO folder_closure(ancestor_id, descendant_id, depth)
                 SELECT ancestor_id, ?1, depth + 1
                 FROM folder_closure WHERE descendant_id = ?2",
                params![current_id, parent],
            )?;
        }
        parent_id = Some(current_id);
        folder_id = Some(current_id);
    }
    folder_id.ok_or_else(|| LibraryError::InvalidData("folder path cannot be empty".to_string()))
}

fn insert_scan_sound(
    transaction: &Transaction<'_>,
    root_id: i64,
    root_path: &str,
    generation: i64,
    row: SoundRecord,
) -> Result<(), LibraryError> {
    let sound = row.sound;
    let duration_ms = sound
        .duration_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| LibraryError::InvalidData("sound duration exceeds SQLite range".into()))?;
    transaction.execute(
        "INSERT INTO sounds(
             public_id, name, search_name, path, source_path, duration_ms,
             volume, enabled, loudness_lufs, loudness_state, loudness_confidence,
             loudness_fingerprint, loudness_true_peak_dbtp, general_position, standalone
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0)
         ON CONFLICT(path) DO NOTHING",
        params![
            &sound.id,
            &sound.name,
            sound.name.to_lowercase(),
            &sound.path,
            &sound.source_path,
            duration_ms,
            i64::from(sound.volume),
            i64::from(sound.enabled),
            sound.loudness_lufs,
            sound.loudness_analysis_state.as_str(),
            sound.loudness_confidence,
            sound.loudness_source_fingerprint,
            sound.loudness_true_peak_dbtp,
            usize_to_i64(row.general_position)?,
        ],
    )?;
    let sound_id: i64 = transaction.query_row(
        "SELECT rowid FROM sounds WHERE path = ?1",
        [&sound.path],
        |result| result.get(0),
    )?;
    transaction.execute(
        "UPDATE sounds SET standalone = 0 WHERE rowid = ?1",
        [sound_id],
    )?;
    for location in row.locations {
        if location.root_path != root_path {
            return Err(LibraryError::InvalidData(
                "scan location belongs to a different root".to_string(),
            ));
        }
        let folder_id = location
            .folder_relative_path
            .as_deref()
            .map(|folder| {
                transaction.query_row(
                    "SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2",
                    params![root_id, folder],
                    |result| result.get::<_, i64>(0),
                )
            })
            .transpose()?;
        transaction.execute(
            "INSERT INTO sound_locations(sound_id, root_id, generation, folder_id, relative_path)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(sound_id, root_id, generation) DO UPDATE SET
                 folder_id = excluded.folder_id, relative_path = excluded.relative_path",
            params![
                sound_id,
                root_id,
                generation,
                folder_id,
                location.relative_path,
            ],
        )?;
    }
    Ok(())
}

fn apply_root_scan_batch(
    connection: &mut Connection,
    root_path: &str,
    generation: i64,
    folders: Vec<FolderRecord>,
    sounds: Vec<SoundRecord>,
) -> Result<(), LibraryError> {
    if !verify_root_scan(connection, root_path, generation)? {
        return Err(LibraryError::InvalidData(
            "root scan generation is no longer active".to_string(),
        ));
    }
    let transaction = connection.transaction()?;
    let root_id: i64 =
        transaction.query_row("SELECT id FROM roots WHERE path = ?1", [root_path], |row| {
            row.get(0)
        })?;
    for folder in &folders {
        if folder.root_path != root_path {
            return Err(LibraryError::InvalidData(
                "scan folder belongs to a different root".to_string(),
            ));
        }
        ensure_scan_folder(&transaction, root_id, generation, folder)?;
    }
    for sound in sounds {
        insert_scan_sound(&transaction, root_id, root_path, generation, sound)?;
    }
    transaction.commit()?;
    Ok(())
}

fn finish_root_scan(
    connection: &mut Connection,
    root_path: &str,
    generation: i64,
) -> Result<bool, LibraryError> {
    if !verify_root_scan(connection, root_path, generation)? {
        return Ok(false);
    }
    let transaction = connection.transaction()?;
    reconcile_legacy_generated_tabs(&transaction, root_path, generation)?;
    let (root_id, old_generation): (i64, i64) = transaction.query_row(
        "SELECT id, active_generation FROM roots WHERE path = ?1",
        [root_path],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    convert_disappeared_custom_folders(&transaction, root_id, old_generation, generation)?;
    let changed = transaction.execute(
        "UPDATE roots SET active_generation = ?2 WHERE path = ?1",
        params![root_path, generation],
    )?;
    transaction.execute(
        "DELETE FROM sound_locations WHERE root_id = ?1 AND generation <> ?2",
        params![root_id, generation],
    )?;
    transaction.execute(
        "DELETE FROM folder_presence
         WHERE generation <> ?2
           AND folder_id IN (SELECT id FROM folders WHERE root_id = ?1)",
        params![root_id, generation],
    )?;
    transaction.execute(
        "DELETE FROM folders
         WHERE root_id = ?1
           AND NOT EXISTS (
               SELECT 1 FROM folder_presence
               WHERE folder_id = folders.id AND generation = ?2
           )",
        params![root_id, generation],
    )?;
    transaction.execute(
        &format!(
            "UPDATE hotkey_bindings
             SET normalized = NULL, state = 'needs_attention',
                 issue = 'sound is no longer in the active library'
             WHERE state = 'active' AND sound_id IS NOT NULL
               AND NOT EXISTS(
                   SELECT 1 FROM sounds AS sound
                   WHERE sound.rowid = hotkey_bindings.sound_id AND {LIVE_SOUND_FILTER}
               )
               AND NOT EXISTS(
                   SELECT 1 FROM sound_locations AS staged_location
                   WHERE staged_location.sound_id = hotkey_bindings.sound_id
               )"
        ),
        [],
    )?;
    transaction.execute(
        "DELETE FROM meta WHERE key = ?1",
        [scan_meta_key(root_path)],
    )?;
    transaction.commit()?;
    Ok(changed == 1)
}

fn convert_disappeared_custom_folders(
    transaction: &Transaction<'_>,
    root_id: i64,
    old_generation: i64,
    new_generation: i64,
) -> Result<(), LibraryError> {
    let next_position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM manual_tabs",
        [],
        |row| row.get(0),
    )?;
    transaction.execute(
        "WITH customized AS (
             SELECT folder.id, COALESCE(pref.display_name, folder.name) AS name,
                    ROW_NUMBER() OVER (ORDER BY folder.id) - 1 AS position_offset
             FROM folders AS folder
             JOIN folder_presence AS old_presence ON old_presence.folder_id = folder.id
                 AND old_presence.generation = ?2
             LEFT JOIN folder_presence AS new_presence ON new_presence.folder_id = folder.id
                 AND new_presence.generation = ?3
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE folder.root_id = ?1
               AND new_presence.folder_id IS NULL
               AND (
                   pref.folder_id IS NOT NULL
                   OR EXISTS (
                       SELECT 1 FROM folder_overrides
                       WHERE folder_id = folder.id
                   )
               )
         )
         INSERT INTO manual_tabs(public_id, name, position)
         SELECT 'converted-folder-' || id || '-' || ?3,
                name, ?4 + position_offset
         FROM customized",
        params![root_id, old_generation, new_generation, next_position],
    )?;
    transaction.execute(
        "WITH customized AS (
             SELECT folder.id
             FROM folders AS folder
             JOIN folder_presence AS old_presence ON old_presence.folder_id = folder.id
                 AND old_presence.generation = ?2
             LEFT JOIN folder_presence AS new_presence ON new_presence.folder_id = folder.id
                 AND new_presence.generation = ?3
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE folder.root_id = ?1
               AND new_presence.folder_id IS NULL
               AND (
                   pref.folder_id IS NOT NULL
                   OR EXISTS (
                       SELECT 1 FROM folder_overrides
                       WHERE folder_id = folder.id
                   )
               )
         ), effective(folder_id, sound_id) AS (
             SELECT customized.id, location.sound_id
             FROM customized
             JOIN folder_closure AS closure ON closure.ancestor_id = customized.id
             JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                 AND location.root_id = ?1 AND location.generation = ?2
             UNION
             SELECT customized.id, override.sound_id
             FROM customized
             JOIN folder_overrides AS override ON override.folder_id = customized.id
             WHERE override.action = 'include'
             EXCEPT
             SELECT customized.id, override.sound_id
             FROM customized
             JOIN folder_overrides AS override ON override.folder_id = customized.id
             WHERE override.action = 'exclude'
         ), ranked AS (
             SELECT effective.folder_id, effective.sound_id,
                    ROW_NUMBER() OVER (
                        PARTITION BY effective.folder_id
                        ORDER BY sound.general_position, sound.public_id
                    ) - 1 AS position
             FROM effective
             JOIN sounds AS sound ON sound.rowid = effective.sound_id
         )
         INSERT INTO manual_memberships(tab_id, sound_id, position)
         SELECT tab.id, ranked.sound_id, ranked.position
         FROM ranked
         JOIN manual_tabs AS tab
           ON tab.public_id = 'converted-folder-' || ranked.folder_id || '-' || ?3",
        params![root_id, old_generation, new_generation],
    )?;
    Ok(())
}

fn reconcile_legacy_generated_tabs(
    transaction: &Transaction<'_>,
    root_path: &str,
    generation: i64,
) -> Result<(), LibraryError> {
    transaction.execute(
        "INSERT INTO folder_prefs(folder_id, display_name, sibling_position, expanded)
         SELECT folder.id, legacy.name, legacy.position, 0
         FROM legacy_generated_tabs AS legacy
         JOIN roots AS root ON root.path = legacy.root_path
         JOIN folders AS folder ON folder.root_id = root.id
             AND folder.relative_path = legacy.relative_path
         JOIN folder_presence AS presence ON presence.folder_id = folder.id
             AND presence.generation = ?2
         WHERE legacy.root_path = ?1
         ON CONFLICT(folder_id) DO UPDATE SET
             display_name = excluded.display_name,
             sibling_position = excluded.sibling_position",
        params![root_path, generation],
    )?;
    transaction.execute(
        "WITH targets(tab_id, folder_id, root_id) AS (
             SELECT legacy.id, folder.id, root.id
             FROM legacy_generated_tabs AS legacy
             JOIN roots AS root ON root.path = legacy.root_path
             JOIN folders AS folder ON folder.root_id = root.id
                 AND folder.relative_path = legacy.relative_path
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = ?2
             WHERE legacy.root_path = ?1
         )
         INSERT INTO folder_overrides(folder_id, sound_id, action)
         SELECT target.folder_id, membership.sound_id, 'include'
         FROM targets AS target
         JOIN legacy_generated_memberships AS membership ON membership.tab_id = target.tab_id
         WHERE NOT EXISTS (
             SELECT 1
             FROM folder_closure AS closure
             JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
             WHERE closure.ancestor_id = target.folder_id
               AND location.root_id = target.root_id
               AND location.generation = ?2
               AND location.sound_id = membership.sound_id
         )
         ON CONFLICT(folder_id, sound_id) DO UPDATE SET action = excluded.action",
        params![root_path, generation],
    )?;
    transaction.execute(
        "WITH targets(tab_id, folder_id, root_id) AS (
             SELECT legacy.id, folder.id, root.id
             FROM legacy_generated_tabs AS legacy
             JOIN roots AS root ON root.path = legacy.root_path
             JOIN folders AS folder ON folder.root_id = root.id
                 AND folder.relative_path = legacy.relative_path
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = ?2
             WHERE legacy.root_path = ?1
         ), physical(tab_id, folder_id, sound_id) AS (
             SELECT DISTINCT target.tab_id, target.folder_id, location.sound_id
             FROM targets AS target
             JOIN folder_closure AS closure ON closure.ancestor_id = target.folder_id
             JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                 AND location.root_id = target.root_id
                 AND location.generation = ?2
         )
         INSERT INTO folder_overrides(folder_id, sound_id, action)
         SELECT physical.folder_id, physical.sound_id, 'exclude'
         FROM physical
         WHERE NOT EXISTS (
             SELECT 1 FROM legacy_generated_memberships AS membership
             WHERE membership.tab_id = physical.tab_id
               AND membership.sound_id = physical.sound_id
         )
         ON CONFLICT(folder_id, sound_id) DO UPDATE SET action = excluded.action",
        params![root_path, generation],
    )?;
    transaction.execute(
        "INSERT INTO manual_tabs(public_id, name, position)
         SELECT legacy.public_id, legacy.name, legacy.position
         FROM legacy_generated_tabs AS legacy
         WHERE legacy.root_path = ?1
           AND NOT EXISTS (
               SELECT 1 FROM roots AS root
               JOIN folders AS folder ON folder.root_id = root.id
               JOIN folder_presence AS presence ON presence.folder_id = folder.id
                   AND presence.generation = ?2
               WHERE root.path = legacy.root_path
                 AND folder.relative_path = legacy.relative_path
           )",
        params![root_path, generation],
    )?;
    transaction.execute(
        "INSERT INTO manual_memberships(tab_id, sound_id, position)
         SELECT manual.id, membership.sound_id, membership.position
         FROM legacy_generated_tabs AS legacy
         JOIN manual_tabs AS manual ON manual.public_id = legacy.public_id
         JOIN legacy_generated_memberships AS membership ON membership.tab_id = legacy.id
         WHERE legacy.root_path = ?1
         ON CONFLICT(tab_id, sound_id) DO UPDATE SET position = excluded.position",
        [root_path],
    )?;
    transaction.execute(
        "DELETE FROM legacy_generated_tabs WHERE root_path = ?1",
        [root_path],
    )?;
    Ok(())
}

fn cancel_root_scan(
    connection: &mut Connection,
    root_path: &str,
    generation: i64,
) -> Result<bool, LibraryError> {
    if !verify_root_scan(connection, root_path, generation)? {
        return Ok(false);
    }
    let transaction = connection.transaction()?;
    let root_id: i64 =
        transaction.query_row("SELECT id FROM roots WHERE path = ?1", [root_path], |row| {
            row.get(0)
        })?;
    transaction.execute(
        "DELETE FROM sound_locations WHERE root_id = ?1 AND generation = ?2",
        params![root_id, generation],
    )?;
    transaction.execute(
        "DELETE FROM folder_presence
         WHERE generation = ?2 AND folder_id IN (SELECT id FROM folders WHERE root_id = ?1)",
        params![root_id, generation],
    )?;
    transaction.execute(
        "DELETE FROM meta WHERE key = ?1",
        [scan_meta_key(root_path)],
    )?;
    transaction.commit()?;
    Ok(true)
}

fn remove_root(connection: &mut Connection, root_path: &str) -> Result<bool, LibraryError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM meta WHERE key = ?1",
        [scan_meta_key(root_path)],
    )?;
    let changed = transaction.execute("DELETE FROM roots WHERE path = ?1", [root_path])?;
    transaction.execute(
        "DELETE FROM sounds
         WHERE standalone = 0
           AND NOT EXISTS (
               SELECT 1 FROM sound_locations WHERE sound_id = sounds.rowid
           )
           AND NOT EXISTS (
               SELECT 1 FROM manual_memberships WHERE sound_id = sounds.rowid
           )",
        [],
    )?;
    transaction.commit()?;
    Ok(changed == 1)
}

fn apply_edit(connection: &mut Connection, edit: LibraryEdit) -> Result<bool, LibraryError> {
    let changed = match edit {
        LibraryEdit::UpsertManualTab(row) => connection.execute(
            "INSERT INTO manual_tabs(public_id, name, position) VALUES(?1, ?2, ?3)
             ON CONFLICT(public_id) DO UPDATE SET
                 name = excluded.name, position = excluded.position",
            params![row.public_id, row.name, usize_to_i64(row.position)?],
        )?,
        LibraryEdit::DeleteManualTab(public_id) => {
            connection.execute("DELETE FROM manual_tabs WHERE public_id = ?1", [public_id])?
        }
        LibraryEdit::SetManualMembership(row) => connection.execute(
            "INSERT INTO manual_memberships(tab_id, sound_id, position)
             SELECT tab.id, sound.rowid, ?3
             FROM manual_tabs AS tab, sounds AS sound
             WHERE tab.public_id = ?1 AND sound.public_id = ?2
             ON CONFLICT(tab_id, sound_id) DO UPDATE SET position = excluded.position",
            params![
                row.tab_public_id,
                row.sound_public_id,
                usize_to_i64(row.position)?
            ],
        )?,
        LibraryEdit::RemoveManualMembership {
            tab_public_id,
            sound_public_id,
        } => connection.execute(
            "DELETE FROM manual_memberships
             WHERE tab_id = (SELECT id FROM manual_tabs WHERE public_id = ?1)
               AND sound_id = (SELECT rowid FROM sounds WHERE public_id = ?2)",
            params![tab_public_id, sound_public_id],
        )?,
        LibraryEdit::ApplyManualMemberships {
            additions,
            removals,
        } => {
            let transaction = connection.transaction()?;
            let mut insert = transaction.prepare(
                "INSERT INTO manual_memberships(tab_id, sound_id, position)
                 SELECT tab.id, sound.rowid, ?3
                 FROM manual_tabs AS tab, sounds AS sound
                 WHERE tab.public_id = ?1 AND sound.public_id = ?2
                 ON CONFLICT(tab_id, sound_id) DO UPDATE SET position = excluded.position",
            )?;
            let mut delete = transaction.prepare(
                "DELETE FROM manual_memberships
                 WHERE tab_id = (SELECT id FROM manual_tabs WHERE public_id = ?1)
                   AND sound_id = (SELECT rowid FROM sounds WHERE public_id = ?2)",
            )?;
            let mut changed = 0_usize;
            for row in additions {
                changed = changed.saturating_add(insert.execute(params![
                    row.tab_public_id,
                    row.sound_public_id,
                    usize_to_i64(row.position)?
                ])?);
            }
            for (tab_public_id, sound_public_id) in removals {
                changed = changed
                    .saturating_add(delete.execute(params![tab_public_id, sound_public_id])?);
            }
            drop(insert);
            drop(delete);
            transaction.commit()?;
            changed
        }
        LibraryEdit::SetFolderOverride(row) => {
            let action = match row.action {
                FolderOverrideAction::Include => "include",
                FolderOverrideAction::Exclude => "exclude",
            };
            connection.execute(
                "INSERT INTO folder_overrides(folder_id, sound_id, action)
                 SELECT folder.id, sound.rowid, ?4
                 FROM folders AS folder
                 JOIN roots AS root ON root.id = folder.root_id
                 CROSS JOIN sounds AS sound
                 WHERE root.path = ?1 AND folder.relative_path = ?2 AND sound.public_id = ?3
                 ON CONFLICT(folder_id, sound_id) DO UPDATE SET action = excluded.action",
                params![
                    row.root_path,
                    row.folder_relative_path,
                    row.sound_public_id,
                    action
                ],
            )?
        }
        LibraryEdit::ClearFolderOverride {
            root_path,
            folder_relative_path,
            sound_public_id,
        } => connection.execute(
            "DELETE FROM folder_overrides
             WHERE folder_id = (
                 SELECT folder.id FROM folders AS folder
                 JOIN roots AS root ON root.id = folder.root_id
                 WHERE root.path = ?1 AND folder.relative_path = ?2
             ) AND sound_id = (SELECT rowid FROM sounds WHERE public_id = ?3)",
            params![root_path, folder_relative_path, sound_public_id],
        )?,
        LibraryEdit::ClearFolderOverrides {
            root_path,
            folder_relative_path,
            sound_public_ids,
        } => {
            let transaction = connection.transaction()?;
            let mut statement = transaction.prepare(
                "DELETE FROM folder_overrides
                 WHERE folder_id = (
                     SELECT folder.id FROM folders AS folder
                     JOIN roots AS root ON root.id = folder.root_id
                     WHERE root.path = ?1 AND folder.relative_path = ?2
                 ) AND sound_id = (SELECT rowid FROM sounds WHERE public_id = ?3)",
            )?;
            let mut changed = 0_usize;
            for sound_public_id in sound_public_ids {
                changed = changed.saturating_add(statement.execute(params![
                    root_path,
                    folder_relative_path,
                    sound_public_id
                ])?);
            }
            drop(statement);
            transaction.commit()?;
            changed
        }
        LibraryEdit::SetFolderPreferences {
            root_path,
            folder_relative_path,
            display_name,
            sibling_position,
            expanded,
        } => connection.execute(
            "INSERT INTO folder_prefs(folder_id, display_name, sibling_position, expanded)
             SELECT folder.id, ?3, ?4, ?5
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.relative_path = ?2
             ON CONFLICT(folder_id) DO UPDATE SET
                 display_name = excluded.display_name,
                 sibling_position = excluded.sibling_position,
                 expanded = excluded.expanded",
            params![
                root_path,
                folder_relative_path,
                display_name,
                sibling_position.map(usize_to_i64).transpose()?,
                i64::from(expanded),
            ],
        )?,
        LibraryEdit::SetFolderExpanded {
            root_path,
            folder_relative_path,
            expanded,
        } => connection.execute(
            "INSERT INTO folder_prefs(folder_id, expanded)
             SELECT folder.id, ?3
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.relative_path = ?2
             ON CONFLICT(folder_id) DO UPDATE SET expanded = excluded.expanded",
            params![root_path, folder_relative_path, i64::from(expanded)],
        )?,
        LibraryEdit::SetFolderHidden {
            root_path,
            folder_relative_path,
            hidden,
        } => connection.execute(
            "INSERT INTO folder_prefs(folder_id, hidden)
             SELECT folder.id, ?3
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.relative_path = ?2
             ON CONFLICT(folder_id) DO UPDATE SET hidden = excluded.hidden",
            params![root_path, folder_relative_path, i64::from(hidden)],
        )?,
        LibraryEdit::SetFolderDisplayName {
            root_path,
            folder_relative_path,
            display_name,
        } => connection.execute(
            "INSERT INTO folder_prefs(folder_id, display_name)
             SELECT folder.id, ?3
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.relative_path = ?2
             ON CONFLICT(folder_id) DO UPDATE SET display_name = excluded.display_name",
            params![root_path, folder_relative_path, display_name],
        )?,
        LibraryEdit::ReorderFolder {
            root_path,
            folder_relative_path,
            target_index,
        } => return reorder_folder(connection, &root_path, &folder_relative_path, target_index),
        LibraryEdit::MoveFolder {
            root_path,
            folder_relative_path,
            direction,
        } => return move_folder(connection, &root_path, &folder_relative_path, direction),
    };
    Ok(changed != 0)
}

fn reorder_folder(
    connection: &mut Connection,
    root_path: &str,
    folder_relative_path: &str,
    target_index: usize,
) -> Result<bool, LibraryError> {
    let transaction = connection.transaction()?;
    let target_id = transaction
        .query_row(
            "SELECT folder.id
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             WHERE root.path = ?1 AND folder.relative_path = ?2",
            params![root_path, folder_relative_path],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(target_id) = target_id else {
        return Ok(false);
    };

    let mut statement = transaction.prepare(
        "WITH target AS (
             SELECT folder.id, folder.root_id, folder.parent_id, root.active_generation
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE folder.id = ?1
         )
         SELECT folder.id
         FROM folders AS folder
         JOIN target ON target.root_id = folder.root_id
             AND folder.parent_id IS target.parent_id
         JOIN folder_presence AS presence ON presence.folder_id = folder.id
             AND presence.generation = target.active_generation
         LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
         ORDER BY COALESCE(pref.sibling_position, folder.position), folder.id",
    )?;
    let mut siblings = Vec::new();
    for row in statement.query_map(params![target_id], |row| row.get::<_, i64>(0))? {
        siblings.push(row?);
    }
    drop(statement);

    let Some(current_index) = siblings.iter().position(|id| *id == target_id) else {
        return Ok(false);
    };
    let target_index = target_index.min(siblings.len().saturating_sub(1));
    if current_index == target_index {
        return Ok(false);
    }
    let moved = siblings.remove(current_index);
    siblings.insert(target_index, moved);

    {
        let mut upsert = transaction.prepare(
            "INSERT INTO folder_prefs(folder_id, sibling_position)
             VALUES(?1, ?2)
             ON CONFLICT(folder_id) DO UPDATE SET sibling_position = excluded.sibling_position",
        )?;
        for (position, id) in siblings.iter().enumerate() {
            upsert.execute(params![id, i64::try_from(position).unwrap_or(i64::MAX)])?;
        }
    }
    transaction.commit()?;
    Ok(true)
}

fn move_folder(
    connection: &mut Connection,
    root_path: &str,
    folder_relative_path: &str,
    direction: i32,
) -> Result<bool, LibraryError> {
    if !matches!(direction, -1 | 1) {
        return Err(LibraryError::InvalidData(
            "folder move direction must be -1 or 1".to_string(),
        ));
    }
    let transaction = connection.transaction()?;
    let target_id = transaction
        .query_row(
            "SELECT folder.id
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             WHERE root.path = ?1 AND folder.relative_path = ?2",
            params![root_path, folder_relative_path],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(target_id) = target_id else {
        return Ok(false);
    };
    let mut statement = transaction.prepare(
        "WITH target AS (
             SELECT folder.id, folder.root_id, folder.parent_id, root.active_generation
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE folder.id = ?1
         ), ordered AS (
             SELECT folder.id,
                    ROW_NUMBER() OVER (
                        ORDER BY COALESCE(pref.sibling_position, folder.position), folder.id
                    ) - 1 AS position
             FROM folders AS folder
             JOIN target ON target.root_id = folder.root_id
                 AND folder.parent_id IS target.parent_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = target.active_generation
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
         ), target_position AS (
             SELECT position FROM ordered WHERE id = ?1
         )
         SELECT id, position FROM ordered
         WHERE position = (SELECT position FROM target_position)
            OR position = (SELECT position FROM target_position) + ?2",
    )?;
    let rows = statement.query_map(params![target_id, direction], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut pair = Vec::with_capacity(2);
    for row in rows {
        pair.push(row?);
    }
    drop(statement);
    if pair.len() != 2 {
        return Ok(false);
    }
    let target_position = pair
        .iter()
        .find_map(|(id, position)| (*id == target_id).then_some(*position))
        .ok_or_else(|| LibraryError::InvalidData("folder move target disappeared".to_string()))?;
    let (adjacent_id, adjacent_position) = pair
        .iter()
        .find(|(id, _)| *id != target_id)
        .copied()
        .ok_or_else(|| LibraryError::InvalidData("folder move sibling disappeared".to_string()))?;
    transaction.execute(
        "INSERT INTO folder_prefs(folder_id, sibling_position)
         VALUES(?1, ?2), (?3, ?4)
         ON CONFLICT(folder_id) DO UPDATE SET sibling_position = excluded.sibling_position",
        params![target_id, adjacent_position, adjacent_id, target_position],
    )?;
    transaction.commit()?;
    Ok(true)
}

fn insert_roots(transaction: &Transaction<'_>, rows: Vec<RootRecord>) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare(
        "INSERT INTO roots(path, position) VALUES(?1, ?2)
         ON CONFLICT(path) DO NOTHING",
    )?;
    for row in rows {
        statement.execute(params![row.path, usize_to_i64(row.position)?])?;
    }
    Ok(())
}

fn insert_folders(
    transaction: &Transaction<'_>,
    rows: Vec<FolderRecord>,
) -> Result<(), LibraryError> {
    for row in rows {
        let (root_id, active_generation): (i64, i64) = transaction.query_row(
            "SELECT id, active_generation FROM roots WHERE path = ?1",
            [&row.root_path],
            |result| Ok((result.get(0)?, result.get(1)?)),
        )?;
        let parent_id = row
            .parent_relative_path
            .as_ref()
            .map(|parent| {
                transaction.query_row(
                    "SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2",
                    params![root_id, parent],
                    |result| result.get::<_, i64>(0),
                )
            })
            .transpose()?;
        transaction.execute(
            "INSERT INTO folders(root_id, parent_id, relative_path, name, position)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                root_id,
                parent_id,
                row.relative_path,
                row.name,
                usize_to_i64(row.position)?
            ],
        )?;
        let folder_id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO folder_presence(folder_id, generation) VALUES(?1, ?2)",
            params![folder_id, active_generation],
        )?;
        transaction.execute(
            "INSERT INTO folder_closure(ancestor_id, descendant_id, depth)
             VALUES(?1, ?1, 0)",
            [folder_id],
        )?;
        if let Some(parent_id) = parent_id {
            transaction.execute(
                "INSERT INTO folder_closure(ancestor_id, descendant_id, depth)
                 SELECT ancestor_id, ?1, depth + 1
                 FROM folder_closure WHERE descendant_id = ?2",
                params![folder_id, parent_id],
            )?;
        }
    }
    Ok(())
}

fn insert_sounds(
    transaction: &Transaction<'_>,
    rows: Vec<SoundRecord>,
) -> Result<(), LibraryError> {
    let mut insert_sound = transaction.prepare(
        "INSERT INTO sounds(
             public_id, name, search_name, path, source_path, duration_ms,
             volume, enabled, loudness_lufs, loudness_state, loudness_confidence,
             loudness_fingerprint, loudness_true_peak_dbtp, general_position, standalone
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    let mut insert_hotkey = transaction.prepare(
        "INSERT INTO hotkey_bindings(
             binding_id, sound_id, accelerator, normalized, state, issue
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut find_root =
        transaction.prepare("SELECT id, active_generation FROM roots WHERE path = ?1")?;
    let mut find_folder =
        transaction.prepare("SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2")?;
    let mut insert_location = transaction.prepare(
        "INSERT INTO sound_locations(sound_id, root_id, generation, folder_id, relative_path)
         VALUES(?1, ?2, ?3, ?4, ?5)",
    )?;
    for row in rows {
        let standalone = row.locations.is_empty();
        let sound = row.sound;
        let duration_ms = sound
            .duration_ms
            .map(i64::try_from)
            .transpose()
            .map_err(|_| LibraryError::InvalidData("sound duration exceeds SQLite range".into()))?;
        let hotkey = sound
            .hotkey
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let normalized = hotkey.map(|value| value.to_ascii_lowercase());
        insert_sound.execute(params![
            &sound.id,
            &sound.name,
            sound.name.to_lowercase(),
            &sound.path,
            &sound.source_path,
            duration_ms,
            i64::from(sound.volume),
            i64::from(sound.enabled),
            sound.loudness_lufs,
            sound.loudness_analysis_state.as_str(),
            sound.loudness_confidence,
            sound.loudness_source_fingerprint,
            sound.loudness_true_peak_dbtp,
            usize_to_i64(row.general_position)?,
            i64::from(standalone),
        ])?;
        let sound_id = transaction.last_insert_rowid();
        if let (Some(accelerator), Some(normalized)) = (hotkey, normalized) {
            insert_hotkey.execute(params![
                &sound.id,
                sound_id,
                accelerator,
                normalized,
                "active",
                Option::<String>::None,
            ])?;
        }
        for location in row.locations {
            let (root_id, generation): (i64, i64) = find_root
                .query_row([&location.root_path], |result| {
                    Ok((result.get(0)?, result.get(1)?))
                })?;
            let folder_id = location
                .folder_relative_path
                .as_ref()
                .map(|folder| {
                    find_folder
                        .query_row(params![root_id, folder], |result| result.get::<_, i64>(0))
                })
                .transpose()?;
            insert_location.execute(params![
                sound_id,
                root_id,
                generation,
                folder_id,
                location.relative_path
            ])?;
        }
    }
    Ok(())
}

fn insert_manual_tabs(
    transaction: &Transaction<'_>,
    rows: Vec<ManualTabRecord>,
) -> Result<(), LibraryError> {
    let mut statement = transaction
        .prepare("INSERT INTO manual_tabs(public_id, name, position) VALUES(?1, ?2, ?3)")?;
    for row in rows {
        statement.execute(params![
            row.public_id,
            row.name,
            usize_to_i64(row.position)?
        ])?;
    }
    Ok(())
}

fn insert_manual_memberships(
    transaction: &Transaction<'_>,
    rows: Vec<ManualMembershipRecord>,
) -> Result<(), LibraryError> {
    for row in rows {
        transaction.execute(
            "INSERT INTO manual_memberships(tab_id, sound_id, position)
             SELECT tab.id, sound.rowid, ?3
             FROM manual_tabs AS tab, sounds AS sound
             WHERE tab.public_id = ?1 AND sound.public_id = ?2",
            params![
                row.tab_public_id,
                row.sound_public_id,
                usize_to_i64(row.position)?
            ],
        )?;
        if transaction.changes() != 1 {
            return Err(LibraryError::InvalidData(
                "manual membership references a missing tab or sound".to_string(),
            ));
        }
    }
    Ok(())
}

fn insert_legacy_generated_tabs(
    transaction: &Transaction<'_>,
    rows: Vec<LegacyGeneratedTabRecord>,
) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare(
        "INSERT INTO legacy_generated_tabs(
             public_id, root_path, relative_path, name, position
         ) VALUES(?1, ?2, ?3, ?4, ?5)",
    )?;
    for row in rows {
        statement.execute(params![
            row.public_id,
            row.root_path,
            row.relative_path,
            row.name,
            usize_to_i64(row.position)?
        ])?;
    }
    Ok(())
}

fn insert_legacy_generated_memberships(
    transaction: &Transaction<'_>,
    rows: Vec<LegacyGeneratedMembershipRecord>,
) -> Result<(), LibraryError> {
    for row in rows {
        transaction.execute(
            "INSERT INTO legacy_generated_memberships(tab_id, sound_id, position)
             SELECT tab.id, sound.rowid, ?3
             FROM legacy_generated_tabs AS tab, sounds AS sound
             WHERE tab.public_id = ?1 AND sound.public_id = ?2",
            params![
                row.tab_public_id,
                row.sound_public_id,
                usize_to_i64(row.position)?
            ],
        )?;
        if transaction.changes() != 1 {
            return Err(LibraryError::InvalidData(
                "legacy generated membership references a missing tab or sound".to_string(),
            ));
        }
    }
    Ok(())
}

fn insert_folder_overrides(
    transaction: &Transaction<'_>,
    rows: Vec<FolderOverrideRecord>,
) -> Result<(), LibraryError> {
    for row in rows {
        let action = match row.action {
            FolderOverrideAction::Include => "include",
            FolderOverrideAction::Exclude => "exclude",
        };
        transaction.execute(
            "INSERT INTO folder_overrides(folder_id, sound_id, action)
             SELECT folder.id, sound.rowid, ?4
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             CROSS JOIN sounds AS sound
             WHERE root.path = ?1 AND folder.relative_path = ?2 AND sound.public_id = ?3
             ON CONFLICT(folder_id, sound_id) DO UPDATE SET action = excluded.action",
            params![
                row.root_path,
                row.folder_relative_path,
                row.sound_public_id,
                action
            ],
        )?;
        if transaction.changes() != 1 {
            return Err(LibraryError::InvalidData(
                "folder override references a missing folder or sound".to_string(),
            ));
        }
    }
    Ok(())
}

fn insert_hotkey_bindings(
    transaction: &Transaction<'_>,
    rows: Vec<HotkeyBindingRecord>,
) -> Result<(), LibraryError> {
    let mut insert = transaction.prepare(
        "INSERT INTO hotkey_bindings(
             binding_id, sound_id, control_action, target_tab, tab_scope,
             accelerator, normalized, state, issue
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for binding in rows {
        let (sound_id, control_action, target_tab) = match &binding.owner {
            HotkeyBindingOwner::Sound(public_id) => (
                Some(
                    transaction
                        .query_row(
                            "SELECT rowid FROM sounds WHERE public_id = ?1",
                            [public_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()?
                        .ok_or_else(|| {
                            LibraryError::InvalidData(format!(
                                "unknown sound binding owner: {public_id}"
                            ))
                        })?,
                ),
                None,
                None,
            ),
            HotkeyBindingOwner::Control(action) => (None, Some(action.as_str()), None),
            HotkeyBindingOwner::Tab(tab) => (None, None, Some(tab.as_str())),
        };
        let state = if binding.normalized.is_some() {
            "active"
        } else {
            "needs_attention"
        };
        insert.execute(params![
            binding.binding_id,
            sound_id,
            control_action,
            target_tab,
            binding.tab_scope,
            binding.accelerator,
            binding.normalized,
            state,
            binding.issue,
        ])?;
    }
    Ok(())
}
