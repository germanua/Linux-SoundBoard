fn search_query(search: &str) -> String {
    format!("\"{}\"", search.replace('"', "\"\""))
}

const SEARCH_FILTER: &str =
    "CASE WHEN ?1 = '' THEN 1 WHEN length(?1) < 3 THEN instr(sound.search_name, ?1) > 0 ELSE sound.rowid IN (SELECT rowid FROM sound_search WHERE sound_search MATCH ?2) END";

const LIVE_SOUND_FILTER: &str = "(sound.standalone = 1
      OR EXISTS(SELECT 1 FROM manual_memberships AS live_manual
                WHERE live_manual.sound_id = sound.rowid)
      OR EXISTS(SELECT 1 FROM sound_locations AS live_location
                JOIN roots AS live_root ON live_root.id = live_location.root_id
                WHERE live_location.sound_id = sound.rowid
                  AND live_location.generation = live_root.active_generation
                  AND NOT EXISTS(SELECT 1 FROM folder_closure AS hidden_closure
                                 JOIN folder_prefs AS hidden_pref
                                     ON hidden_pref.folder_id = hidden_closure.ancestor_id
                                 WHERE hidden_closure.descendant_id = live_location.folder_id
                                   AND hidden_pref.hidden = 1)))";

const HIDDEN_FOLDER_FILTER: &str = "EXISTS(
          SELECT 1 FROM folder_closure AS hidden_closure
          JOIN folder_prefs AS hidden_pref ON hidden_pref.folder_id = hidden_closure.ancestor_id
          WHERE hidden_closure.descendant_id = folder.id AND hidden_pref.hidden = 1)";

fn sound_fields(binding_filter: &str) -> String {
    format!(
        "sound.public_id, sound.name, sound.path, sound.source_path,
    (SELECT binding.accelerator FROM hotkey_bindings AS binding
     WHERE binding.sound_id = sound.rowid AND binding.state = 'active'
       AND ({binding_filter})
     ORDER BY binding.tab_scope IS NULL
     LIMIT 1),
    sound.duration_ms, sound.volume, sound.enabled, sound.loudness_lufs,
    sound.loudness_state, sound.loudness_confidence, sound.loudness_fingerprint,
    sound.loudness_true_peak_dbtp"
    )
}

fn any_sound_fields() -> String {
    sound_fields("1")
}

fn load_loudness_stats(connection: &Connection) -> Result<LoudnessStats, LibraryError> {
    let sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(sound.loudness_state = 'pending'), 0),
                COALESCE(SUM(sound.loudness_state = 'estimated'), 0),
                COALESCE(SUM(sound.loudness_state = 'refined'), 0),
                COALESCE(SUM(sound.loudness_state = 'unavailable'), 0),
                COALESCE(SUM(sound.loudness_lufs IS NULL
                    AND sound.loudness_state <> 'unavailable'), 0)
         FROM sounds AS sound WHERE {LIVE_SOUND_FILTER}"
    );
    let values = connection.query_row(&sql, [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let convert = |value: i64| {
        usize::try_from(value)
            .map_err(|_| LibraryError::InvalidData("negative loudness count".to_string()))
    };
    Ok(LoudnessStats {
        total: convert(values.0)?,
        pending: convert(values.1)?,
        estimated: convert(values.2)?,
        refined: convert(values.3)?,
        unavailable: convert(values.4)?,
        missing: convert(values.5)?,
    })
}

fn load_library_stats(connection: &Connection) -> Result<LibraryStats, LibraryError> {
    let sound_sql = format!("SELECT COUNT(*) FROM sounds AS sound WHERE {LIVE_SOUND_FILTER}");
    let sounds: i64 = connection.query_row(&sound_sql, [], |row| row.get(0))?;
    let roots: i64 = connection.query_row("SELECT COUNT(*) FROM roots", [], |row| row.get(0))?;
    let manual_tabs: i64 =
        connection.query_row("SELECT COUNT(*) FROM manual_tabs", [], |row| row.get(0))?;
    let hotkey_sql = format!(
        "SELECT COUNT(*) FROM hotkey_bindings AS binding
         LEFT JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active'
           AND (binding.control_action IS NOT NULL
                OR binding.target_tab IS NOT NULL
                OR {LIVE_SOUND_FILTER})"
    );
    let active_hotkeys: i64 = connection.query_row(&hotkey_sql, [], |row| row.get(0))?;
    let convert = |value: i64| {
        usize::try_from(value)
            .map_err(|_| LibraryError::InvalidData("negative library count".to_string()))
    };
    Ok(LibraryStats {
        sounds: convert(sounds)?,
        roots: convert(roots)?,
        manual_tabs: convert(manual_tabs)?,
        active_hotkeys: convert(active_hotkeys)?,
    })
}

fn load_loudness_backfill_after(
    connection: &Connection,
    after: Option<&str>,
) -> Result<SoundPage, LibraryError> {
    let fields = any_sound_fields();
    let sql = format!(
        "SELECT {fields} FROM sounds AS sound
         WHERE sound.loudness_lufs IS NULL
           AND sound.loudness_state <> 'unavailable'
           AND {LIVE_SOUND_FILTER}
           AND (?1 IS NULL OR sound.public_id > ?1)
         ORDER BY sound.public_id LIMIT ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![after, usize_to_i64(PAGE_SIZE)?], sound_from_row)?;
    let mut sounds = Vec::with_capacity(PAGE_SIZE);
    for sound in rows {
        sounds.push(sound?);
    }
    Ok(SoundPage { sounds })
}

fn load_loudness_refinement_candidates(
    connection: &Connection,
    force: bool,
    after: Option<&str>,
    limit: usize,
) -> Result<SoundPage, LibraryError> {
    let fields = any_sound_fields();
    let (sql, threshold) = if force {
        (
            format!(
                "SELECT {fields} FROM sounds AS sound
                 WHERE sound.loudness_state = 'estimated'
                   AND sound.loudness_lufs IS NOT NULL
                   AND {LIVE_SOUND_FILTER}
                   AND (?1 IS NULL OR sound.public_id > ?1)
                 ORDER BY sound.public_id LIMIT ?3"
            ),
            0.0,
        )
    } else {
        (
            format!(
                "SELECT {fields} FROM sounds AS sound
                 WHERE sound.loudness_state = 'estimated'
                   AND sound.loudness_lufs IS NOT NULL
                   AND {LIVE_SOUND_FILTER}
                   AND COALESCE(sound.loudness_confidence, 0.0) <= ?2
                 ORDER BY COALESCE(sound.loudness_confidence, 0.0),
                          COALESCE(sound.duration_ms, 0) DESC, sound.public_id
                 LIMIT ?3"
            ),
            FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD_FOR_STORE,
        )
    };
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        params![after, threshold, usize_to_i64(limit)?],
        sound_from_row,
    )?;
    let mut sounds = Vec::with_capacity(limit);
    for sound in rows {
        sounds.push(sound?);
    }
    Ok(SoundPage { sounds })
}

const FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD_FOR_STORE: f32 = 0.80;

fn apply_loudness_updates(
    connection: &mut Connection,
    updates: Vec<LoudnessUpdate>,
) -> Result<usize, LibraryError> {
    let transaction = connection.transaction()?;
    let mut statement = transaction.prepare(
        "UPDATE sounds SET loudness_lufs = ?2, loudness_state = ?3,
             loudness_confidence = ?4, loudness_true_peak_dbtp = ?5
         WHERE public_id = ?1",
    )?;
    let mut changed = 0_usize;
    for update in updates {
        changed = changed.saturating_add(statement.execute(params![
            update.sound_id,
            update.lufs,
            update.state.as_str(),
            update.confidence,
            update.true_peak_dbtp,
        ])?);
    }
    drop(statement);
    transaction.commit()?;
    Ok(changed)
}

fn count_sounds(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
) -> Result<usize, LibraryError> {
    let fts = search_query(search);
    let count: i64 = match scope {
        LibraryScope::General => connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM sounds AS sound
                 WHERE {LIVE_SOUND_FILTER} AND {SEARCH_FILTER}"
            ),
            params![search, fts],
            |row| row.get(0),
        )?,
        LibraryScope::ManualTab(tab_id) => connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM manual_memberships AS membership
                 JOIN manual_tabs AS tab ON tab.id = membership.tab_id
                 JOIN sounds AS sound ON sound.rowid = membership.sound_id
                 WHERE tab.public_id = ?3 AND {SEARCH_FILTER}"
            ),
            params![search, fts, tab_id],
            |row| row.get(0),
        )?,
        LibraryScope::Folder {
            root_path,
            relative_path,
        } => connection.query_row(
            &format!(
                "WITH selected(folder_id, root_id, generation) AS (
                     SELECT folder.id, root.id, root.active_generation
                     FROM folders AS folder
                     JOIN roots AS root ON root.id = folder.root_id
                     JOIN folder_presence AS presence ON presence.folder_id = folder.id
                         AND presence.generation = root.active_generation
                     WHERE root.path = ?3 AND folder.relative_path = ?4
                 ), effective(sound_id) AS (
                     SELECT location.sound_id FROM selected
                     JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                     JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                         AND location.root_id = selected.root_id
                         AND location.generation = selected.generation
                     UNION
                     SELECT override.sound_id FROM selected
                     JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                     WHERE override.action = 'include'
                     EXCEPT
                     SELECT override.sound_id FROM selected
                     JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                     WHERE override.action = 'exclude'
                 )
                 SELECT COUNT(*) FROM effective
                 JOIN sounds AS sound ON sound.rowid = effective.sound_id
                 WHERE {SEARCH_FILTER}"
            ),
            params![search, fts, root_path, relative_path],
            |row| row.get(0),
        )?,
    };
    usize::try_from(count)
        .map_err(|_| LibraryError::InvalidData("negative sound count".to_string()))
}

fn load_page(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
    page: usize,
) -> Result<SoundPage, LibraryError> {
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let limit = usize_to_i64(PAGE_SIZE)?;
    let offset = usize_to_i64(offset)?;
    let fts = search_query(search);
    let scope_key = scope_key(scope);
    let mut sounds = Vec::with_capacity(PAGE_SIZE);
    match scope {
        LibraryScope::General => {
            let fields = sound_fields("binding.tab_scope IS NULL OR binding.tab_scope = ?5");
            let sql = format!(
                "SELECT {fields} FROM sounds AS sound
                 WHERE {LIVE_SOUND_FILTER} AND {SEARCH_FILTER}
                 ORDER BY sound.general_position, sound.public_id LIMIT ?3 OFFSET ?4"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(
                params![search, fts, limit, offset, scope_key],
                sound_from_row,
            )?;
            for sound in rows {
                sounds.push(sound?);
            }
        }
        LibraryScope::ManualTab(tab_id) => {
            let fields = sound_fields("binding.tab_scope IS NULL OR binding.tab_scope = ?6");
            let sql = format!(
                "SELECT {fields} FROM manual_memberships AS membership
                 JOIN manual_tabs AS tab ON tab.id = membership.tab_id
                 JOIN sounds AS sound ON sound.rowid = membership.sound_id
                 WHERE {SEARCH_FILTER} AND tab.public_id = ?3
                 ORDER BY membership.position, sound.public_id LIMIT ?4 OFFSET ?5"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(
                params![search, fts, tab_id, limit, offset, scope_key],
                sound_from_row,
            )?;
            for sound in rows {
                sounds.push(sound?);
            }
        }
        LibraryScope::Folder {
            root_path,
            relative_path,
        } => {
            let fields = sound_fields("binding.tab_scope IS NULL OR binding.tab_scope = ?7");
            let sql = format!(
                "WITH selected(folder_id, root_id, generation) AS (
                     SELECT folder.id, root.id, root.active_generation
                     FROM folders AS folder
                     JOIN roots AS root ON root.id = folder.root_id
                     JOIN folder_presence AS presence ON presence.folder_id = folder.id
                         AND presence.generation = root.active_generation
                     WHERE root.path = ?3 AND folder.relative_path = ?4
                 ), effective(sound_id) AS (
                     SELECT location.sound_id FROM selected
                     JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                     JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                         AND location.root_id = selected.root_id
                         AND location.generation = selected.generation
                     UNION
                     SELECT override.sound_id FROM selected
                     JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                     WHERE override.action = 'include'
                     EXCEPT
                     SELECT override.sound_id FROM selected
                     JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                     WHERE override.action = 'exclude'
                 )
                 SELECT {fields} FROM effective
                 JOIN sounds AS sound ON sound.rowid = effective.sound_id
                 WHERE {SEARCH_FILTER}
                 ORDER BY sound.general_position, sound.public_id LIMIT ?5 OFFSET ?6"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(
                params![
                    search,
                    fts,
                    root_path,
                    relative_path,
                    limit,
                    offset,
                    scope_key
                ],
                sound_from_row,
            )?;
            for sound in rows {
                sounds.push(sound?);
            }
        }
    }
    Ok(SoundPage { sounds })
}

fn load_sound(connection: &Connection, id: &str) -> Result<Option<Sound>, LibraryError> {
    let fields = any_sound_fields();
    let sql = format!(
        "SELECT {fields}
         FROM sounds AS sound
         WHERE public_id = ?1 AND {LIVE_SOUND_FILTER}"
    );
    connection
        .query_row(&sql, [id], sound_from_row)
        .optional()
        .map_err(LibraryError::from)
}

fn load_sound_by_path(connection: &Connection, path: &str) -> Result<Option<Sound>, LibraryError> {
    let fields = any_sound_fields();
    let sql = format!(
        "SELECT {fields}
         FROM sounds AS sound
         WHERE path = ?1 AND {LIVE_SOUND_FILTER}"
    );
    connection
        .query_row(&sql, [path], sound_from_row)
        .optional()
        .map_err(LibraryError::from)
}

fn load_sound_for_binding(
    connection: &Connection,
    binding_id: &str,
) -> Result<Option<Sound>, LibraryError> {
    let fields = any_sound_fields();
    let sql = format!(
        "SELECT {fields}
         FROM hotkey_bindings AS binding
         JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.binding_id = ?1 AND binding.state = 'active'
           AND {LIVE_SOUND_FILTER}"
    );
    connection
        .query_row(&sql, [binding_id], sound_from_row)
        .optional()
        .map_err(LibraryError::from)
}

fn update_sound(connection: &mut Connection, sound: Sound) -> Result<bool, LibraryError> {
    let search_name = sound.name.to_lowercase();
    let duration_ms = sound
        .duration_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| LibraryError::InvalidData("sound duration exceeds SQLite range".into()))?;
    let transaction = connection.transaction()?;
    let changed = transaction.execute(
        "UPDATE sounds SET
             name = ?2, search_name = ?3, path = ?4, source_path = ?5,
             duration_ms = ?6, volume = ?7, enabled = ?8, loudness_lufs = ?9,
             loudness_state = ?10, loudness_confidence = ?11,
             loudness_fingerprint = ?12, loudness_true_peak_dbtp = ?13
         WHERE public_id = ?1",
        params![
            &sound.id,
            &sound.name,
            search_name,
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
        ],
    )?;
    transaction.commit()?;
    Ok(changed == 1)
}

fn delete_sound(connection: &Connection, id: &str) -> Result<bool, LibraryError> {
    Ok(connection.execute("DELETE FROM sounds WHERE public_id = ?1", [id])? == 1)
}

fn load_roots(connection: &Connection, page: usize) -> Result<RootPage, LibraryError> {
    let count: i64 = connection.query_row("SELECT COUNT(*) FROM roots", [], |row| row.get(0))?;
    let total = usize::try_from(count)
        .map_err(|_| LibraryError::InvalidData("negative root count".to_string()))?;
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let mut statement = connection.prepare(
        "SELECT id, path FROM roots
         ORDER BY position, id LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        |row| {
            Ok(RootItem {
                id: row.get(0)?,
                path: row.get(1)?,
            })
        },
    )?;
    let mut roots = Vec::with_capacity(PAGE_SIZE.min(total.saturating_sub(offset)));
    for root in rows {
        roots.push(root?);
    }
    Ok(RootPage { total, roots })
}

fn load_manual_tabs(connection: &Connection, page: usize) -> Result<ManualTabPage, LibraryError> {
    let count: i64 =
        connection.query_row("SELECT COUNT(*) FROM manual_tabs", [], |row| row.get(0))?;
    let total = usize::try_from(count)
        .map_err(|_| LibraryError::InvalidData("negative manual tab count".to_string()))?;
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let mut statement = connection.prepare(
        "SELECT public_id, name,
                (SELECT COUNT(*) FROM manual_memberships AS membership
                 WHERE membership.tab_id = manual_tabs.id), position
         FROM manual_tabs
         ORDER BY position, id LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        |row| {
            let sound_count = usize::try_from(row.get::<_, i64>(2)?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?;
            Ok(ManualTabItem {
                public_id: row.get(0)?,
                name: row.get(1)?,
                sound_count,
                position: usize::try_from(row.get::<_, i64>(3)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
            })
        },
    )?;
    let mut tabs = Vec::with_capacity(PAGE_SIZE.min(total.saturating_sub(offset)));
    for tab in rows {
        tabs.push(tab?);
    }
    Ok(ManualTabPage { total, tabs })
}

fn load_folder_children(
    connection: &Connection,
    root_path: &str,
    parent_relative_path: Option<&str>,
    page: usize,
) -> Result<FolderPage, LibraryError> {
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let limit = usize_to_i64(PAGE_SIZE)?;
    let offset = usize_to_i64(offset)?;
    let fields = "folder.id, folder.relative_path,
                  COALESCE(pref.display_name, folder.name), COALESCE(pref.expanded, 0),
                  EXISTS(SELECT 1 FROM folders AS child
                         JOIN folder_presence AS child_presence
                           ON child_presence.folder_id = child.id
                          AND child_presence.generation = root.active_generation
                         WHERE child.root_id = root.id AND child.parent_id = folder.id
                           AND NOT EXISTS(
                               SELECT 1 FROM folder_closure AS hidden_closure
                               JOIN folder_prefs AS hidden_pref
                                   ON hidden_pref.folder_id = hidden_closure.ancestor_id
                               WHERE hidden_closure.descendant_id = child.id
                                 AND hidden_pref.hidden = 1))";
    let (total, folders) = if let Some(parent_relative_path) = parent_relative_path {
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             WHERE root.path = ?1 AND folder.parent_id = (
                 SELECT parent.id FROM folders AS parent
                 JOIN folder_presence AS parent_presence ON parent_presence.folder_id = parent.id
                     AND parent_presence.generation = root.active_generation
                 WHERE parent.root_id = root.id AND parent.relative_path = ?2
             ) AND NOT EXISTS(
                 SELECT 1 FROM folder_closure AS hidden_closure
                 JOIN folder_prefs AS hidden_pref ON hidden_pref.folder_id = hidden_closure.ancestor_id
                 WHERE hidden_closure.descendant_id = folder.id AND hidden_pref.hidden = 1
             )",
            params![root_path, parent_relative_path],
            |row| row.get(0),
        )?;
        let sql = format!(
            "SELECT {fields} FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE root.path = ?1 AND folder.parent_id = (
                 SELECT parent.id FROM folders AS parent
                 JOIN folder_presence AS parent_presence ON parent_presence.folder_id = parent.id
                     AND parent_presence.generation = root.active_generation
                 WHERE parent.root_id = root.id AND parent.relative_path = ?2
             ) AND NOT {HIDDEN_FOLDER_FILTER}
             ORDER BY COALESCE(pref.sibling_position, folder.position), folder.id
             LIMIT ?3 OFFSET ?4"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(
            params![root_path, parent_relative_path, limit, offset],
            folder_from_row,
        )?;
        (count, collect_folders(rows)?)
    } else {
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             WHERE root.path = ?1 AND folder.parent_id IS NULL
                 AND NOT EXISTS(
                     SELECT 1 FROM folder_closure AS hidden_closure
                     JOIN folder_prefs AS hidden_pref
                         ON hidden_pref.folder_id = hidden_closure.ancestor_id
                     WHERE hidden_closure.descendant_id = folder.id AND hidden_pref.hidden = 1
                 )",
            [root_path],
            |row| row.get(0),
        )?;
        let sql = format!(
            "SELECT {fields} FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE root.path = ?1 AND folder.parent_id IS NULL AND NOT {HIDDEN_FOLDER_FILTER}
             ORDER BY COALESCE(pref.sibling_position, folder.position), folder.id
             LIMIT ?2 OFFSET ?3"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![root_path, limit, offset], folder_from_row)?;
        (count, collect_folders(rows)?)
    };
    Ok(FolderPage {
        total: usize::try_from(total)
            .map_err(|_| LibraryError::InvalidData("negative folder count".to_string()))?,
        folders,
    })
}

fn load_hidden_folders(
    connection: &Connection,
    page: usize,
) -> Result<HiddenFolderPage, LibraryError> {
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM folder_prefs AS pref
         JOIN folders AS folder ON folder.id = pref.folder_id
         WHERE pref.hidden = 1",
        [],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT root.path, folder.relative_path, COALESCE(pref.display_name, folder.name)
         FROM folder_prefs AS pref
         JOIN folders AS folder ON folder.id = pref.folder_id
         JOIN roots AS root ON root.id = folder.root_id
         WHERE pref.hidden = 1
         ORDER BY root.path, folder.relative_path
         LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        |row| {
            Ok(HiddenFolderItem {
                root_path: row.get(0)?,
                relative_path: row.get(1)?,
                name: row.get(2)?,
            })
        },
    )?;
    let mut folders = Vec::new();
    for folder in rows {
        folders.push(folder?);
    }
    Ok(HiddenFolderPage {
        total: usize::try_from(total)
            .map_err(|_| LibraryError::InvalidData("negative folder count".to_string()))?,
        folders,
    })
}

fn folder_from_row(row: &Row<'_>) -> rusqlite::Result<FolderItem> {
    Ok(FolderItem {
        id: row.get(0)?,
        relative_path: row.get(1)?,
        name: row.get(2)?,
        expanded: row.get::<_, i64>(3)? != 0,
        has_children: row.get::<_, i64>(4)? != 0,
    })
}

fn collect_folders(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<FolderItem>>,
) -> Result<Vec<FolderItem>, LibraryError> {
    let mut folders = Vec::with_capacity(PAGE_SIZE);
    for folder in rows {
        folders.push(folder?);
    }
    Ok(folders)
}

fn load_hotkey_page(connection: &Connection, page: usize) -> Result<SoundPage, LibraryError> {
    let fields = any_sound_fields();
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let sql = format!(
        "SELECT {fields}
         FROM hotkey_bindings AS binding
         JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active' AND {LIVE_SOUND_FILTER}
         ORDER BY general_position, public_id LIMIT ?1 OFFSET ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        sound_from_row,
    )?;
    let mut sounds = Vec::with_capacity(PAGE_SIZE);
    for sound in rows {
        sounds.push(sound?);
    }
    Ok(SoundPage { sounds })
}

fn load_hotkey_bindings_after(
    connection: &Connection,
    after: Option<&str>,
) -> Result<HotkeyBindingPage, LibraryError> {
    let sql = format!(
        "SELECT binding.binding_id, sound.public_id, binding.control_action,
                binding.accelerator, binding.normalized, binding.issue,
                binding.target_tab, binding.tab_scope
         FROM hotkey_bindings AS binding
         LEFT JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active' AND (?1 IS NULL OR binding.binding_id > ?1)
           AND (binding.control_action IS NOT NULL
                OR binding.target_tab IS NOT NULL
                OR {LIVE_SOUND_FILTER})
           AND binding.binding_id = (
               SELECT representative.binding_id
               FROM hotkey_bindings AS representative
               LEFT JOIN sounds AS sound ON sound.rowid = representative.sound_id
               WHERE representative.state = 'active'
                 AND representative.normalized = binding.normalized
                 AND (representative.control_action IS NOT NULL
                      OR representative.target_tab IS NOT NULL
                      OR (sound.rowid IS NOT NULL AND {LIVE_SOUND_FILTER}))
               ORDER BY CASE WHEN representative.control_action IS NOT NULL THEN 0
                             WHEN representative.target_tab IS NOT NULL THEN 1
                             ELSE 2 END,
                        representative.binding_id
               LIMIT 1)
         ORDER BY binding.binding_id LIMIT ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![after, usize_to_i64(PAGE_SIZE)?], |row| {
        Ok(HotkeyBindingRecord {
            binding_id: row.get(0)?,
            owner: binding_owner(row.get(1)?, row.get(2)?, row.get(6)?)?,
            accelerator: row.get(3)?,
            normalized: row.get(4)?,
            issue: row.get(5)?,
            tab_scope: row.get(7)?,
        })
    })?;
    let mut bindings = Vec::with_capacity(PAGE_SIZE);
    for binding in rows {
        bindings.push(binding?);
    }
    Ok(HotkeyBindingPage { bindings })
}

fn binding_owner(
    sound_id: Option<String>,
    control_action: Option<String>,
    target_tab: Option<String>,
) -> Result<HotkeyBindingOwner, rusqlite::Error> {
    match (sound_id, control_action, target_tab) {
        (Some(id), None, None) => Ok(HotkeyBindingOwner::Sound(id)),
        (None, Some(action), None) => Ok(HotkeyBindingOwner::Control(action)),
        (None, None, Some(tab)) => Ok(HotkeyBindingOwner::Tab(tab)),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn load_hotkey_bindings_for_sound(
    connection: &Connection,
    sound_id: &str,
) -> Result<Vec<HotkeyBindingRecord>, LibraryError> {
    let sql = "SELECT binding.binding_id, sound.public_id, binding.control_action,
                      binding.accelerator, binding.normalized, binding.issue,
                      binding.target_tab, binding.tab_scope
               FROM hotkey_bindings AS binding
               JOIN sounds AS sound ON sound.rowid = binding.sound_id
               WHERE sound.public_id = ?1
               ORDER BY binding.tab_scope IS NULL DESC, binding.tab_scope";
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map([sound_id], |row| {
        Ok(HotkeyBindingRecord {
            binding_id: row.get(0)?,
            owner: binding_owner(row.get(1)?, row.get(2)?, row.get(6)?)?,
            accelerator: row.get(3)?,
            normalized: row.get(4)?,
            issue: row.get(5)?,
            tab_scope: row.get(7)?,
        })
    })?;
    let mut bindings = Vec::new();
    for binding in rows {
        bindings.push(binding?);
    }
    Ok(bindings)
}

fn load_hotkey_group(
    connection: &Connection,
    binding_id: &str,
) -> Result<Vec<HotkeyGroupMember>, LibraryError> {
    let sql = format!(
        "SELECT binding.binding_id, sound.public_id, binding.tab_scope
         FROM hotkey_bindings AS binding
         JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active'
           AND binding.normalized = (SELECT pressed.normalized
                                     FROM hotkey_bindings AS pressed
                                     WHERE pressed.binding_id = ?1
                                       AND pressed.state = 'active')
           AND {LIVE_SOUND_FILTER}
         ORDER BY sound.general_position, sound.public_id"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([binding_id], |row| {
        Ok(HotkeyGroupMember {
            binding_id: row.get(0)?,
            sound_id: row.get(1)?,
            tab_scope: row.get(2)?,
        })
    })?;
    let mut members = Vec::new();
    for member in rows {
        members.push(member?);
    }
    Ok(members)
}

fn load_hotkey_binding(
    connection: &Connection,
    binding_id: &str,
) -> Result<Option<HotkeyBindingRecord>, LibraryError> {
    let sql = format!(
        "SELECT binding.binding_id, sound.public_id, binding.control_action,
                binding.accelerator, binding.normalized, binding.issue,
                binding.target_tab, binding.tab_scope
         FROM hotkey_bindings AS binding
         LEFT JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.binding_id = ?1
           AND (binding.control_action IS NOT NULL
                OR binding.target_tab IS NOT NULL
                OR {LIVE_SOUND_FILTER})"
    );
    connection
        .query_row(&sql, [binding_id], |row| {
            Ok(HotkeyBindingRecord {
                binding_id: row.get(0)?,
                owner: binding_owner(row.get(1)?, row.get(2)?, row.get(6)?)?,
                accelerator: row.get(3)?,
                normalized: row.get(4)?,
                issue: row.get(5)?,
                tab_scope: row.get(7)?,
            })
        })
        .optional()
        .map_err(Into::into)
}

fn load_hotkey_conflict(
    connection: &Connection,
    binding_id: &str,
    normalized: &str,
    sounds_may_share: bool,
    tab_scope: Option<&str>,
    excluded_sound_ids: &[String],
) -> Result<Option<String>, LibraryError> {
    let exclusion = if excluded_sound_ids.is_empty() {
        String::new()
    } else {
        let placeholders = (0..excluded_sound_ids.len())
            .map(|offset| format!("?{}", 5 + offset))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "AND NOT (binding.sound_id IS NOT NULL
                     AND binding.sound_id IN (SELECT rowid FROM sounds
                                              WHERE public_id IN ({placeholders}))
                     AND IFNULL(binding.tab_scope, '') = IFNULL(?4, ''))"
        )
    };
    let sql = format!(
        "SELECT sound.name, binding.control_action, binding.target_tab
         FROM hotkey_bindings AS binding
         LEFT JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active'
           AND binding.normalized = ?1
           AND (?2 = '' OR binding.binding_id <> ?2)
           AND (binding.control_action IS NOT NULL
                OR binding.target_tab IS NOT NULL
                OR {LIVE_SOUND_FILTER})
           AND (?3 = 0 OR binding.sound_id IS NULL)
           {exclusion}
           AND (binding.tab_scope IS NULL OR ?4 IS NULL OR binding.tab_scope = ?4)
         LIMIT 1"
    );
    let mut values: Vec<&dyn rusqlite::ToSql> =
        vec![&normalized, &binding_id, &sounds_may_share, &tab_scope];
    for sound_id in excluded_sound_ids {
        values.push(sound_id);
    }
    let conflict = connection
        .query_row(&sql, rusqlite::params_from_iter(values), |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .optional()?;
    Ok(conflict.map(|(sound_name, control_action, target_tab)| {
        if let Some(sound_name) = sound_name {
            format!("sound \"{sound_name}\"")
        } else if target_tab.is_some() {
            "a tab hotkey".to_string()
        } else if let Some(action) = control_action
            .as_deref()
            .and_then(ControlHotkeyAction::from_id)
        {
            format!("control action \"{}\"", action.title())
        } else {
            "another action".to_string()
        }
    }))
}

fn set_hotkey_binding(
    connection: &mut Connection,
    binding: HotkeyBindingRecord,
) -> Result<bool, LibraryError> {
    if binding.binding_id.trim().is_empty() || binding.accelerator.trim().is_empty() {
        return Err(LibraryError::InvalidData(
            "hotkey binding id and accelerator cannot be empty".to_string(),
        ));
    }
    if binding
        .normalized
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(LibraryError::InvalidData(
            "normalized hotkey cannot be empty".to_string(),
        ));
    }
    if let Some(normalized) = binding.normalized.as_deref() {
        let canonical = crate::hotkeys::canonicalize_hotkey_string(&binding.accelerator)
            .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
        if normalized != canonical {
            return Err(LibraryError::InvalidData(
                "normalized hotkey must equal the canonical accelerator".to_string(),
            ));
        }
    }
    if binding.normalized.is_none() && binding.issue.as_deref().is_none_or(str::is_empty) {
        return Err(LibraryError::InvalidData(
            "a binding needing attention must include an issue".to_string(),
        ));
    }

    if matches!(binding.owner, HotkeyBindingOwner::Tab(_)) && binding.tab_scope.is_some() {
        return Err(LibraryError::InvalidData(
            "a tab hotkey cannot itself be scoped to a tab".to_string(),
        ));
    }

    let transaction = connection.transaction()?;
    let (sound_id, control_action, target_tab) = match &binding.owner {
        HotkeyBindingOwner::Sound(public_id) => {
            let sound_id = transaction
                .query_row(
                    "SELECT rowid FROM sounds WHERE public_id = ?1",
                    [public_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    LibraryError::InvalidData(format!("unknown sound binding owner: {public_id}"))
                })?;
            transaction.execute(
                "DELETE FROM hotkey_bindings
                  WHERE binding_id = ?1
                     OR (sound_id = ?2 AND IFNULL(tab_scope, '') = IFNULL(?3, ''))",
                params![&binding.binding_id, sound_id, &binding.tab_scope],
            )?;
            (Some(sound_id), None, None)
        }
        HotkeyBindingOwner::Control(action) => {
            if action.trim().is_empty() {
                return Err(LibraryError::InvalidData(
                    "control hotkey action cannot be empty".to_string(),
                ));
            }
            transaction.execute(
                "DELETE FROM hotkey_bindings WHERE binding_id = ?1 OR control_action = ?2",
                params![&binding.binding_id, action],
            )?;
            (None, Some(action.as_str()), None)
        }
        HotkeyBindingOwner::Tab(tab) => {
            if tab.trim().is_empty() {
                return Err(LibraryError::InvalidData(
                    "tab hotkey target cannot be empty".to_string(),
                ));
            }
            transaction.execute(
                "DELETE FROM hotkey_bindings WHERE binding_id = ?1 OR target_tab = ?2",
                params![&binding.binding_id, tab],
            )?;
            (None, None, Some(tab.as_str()))
        }
    };
    let state = if binding.normalized.is_some() {
        "active"
    } else {
        "needs_attention"
    };
    let issue = if binding.normalized.is_some() {
        None
    } else {
        binding.issue.as_deref()
    };
    let changed = transaction.execute(
        "INSERT INTO hotkey_bindings(
             binding_id, sound_id, control_action, target_tab, tab_scope,
             accelerator, normalized, state, issue
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            binding.binding_id,
            sound_id,
            control_action,
            target_tab,
            binding.tab_scope,
            binding.accelerator,
            binding.normalized,
            state,
            issue,
        ],
    )?;
    transaction.commit()?;
    Ok(changed == 1)
}

fn set_sound_hotkeys(
    connection: &mut Connection,
    sound_ids: &[String],
    accelerator: Option<&str>,
    tab_scope: Option<&str>,
) -> Result<usize, LibraryError> {
    let normalized = match accelerator {
        Some(accelerator) => Some(
            crate::hotkeys::canonicalize_hotkey_string(accelerator)
                .map_err(|error| LibraryError::InvalidData(error.to_string()))?,
        ),
        None => None,
    };

    let transaction = connection.transaction()?;
    for public_id in sound_ids {
        let sound_id: i64 = transaction
            .query_row(
                "SELECT rowid FROM sounds WHERE public_id = ?1",
                [public_id],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                LibraryError::InvalidData(format!("unknown sound binding owner: {public_id}"))
            })?;
        transaction.execute(
            "DELETE FROM hotkey_bindings
              WHERE sound_id = ?1 AND IFNULL(tab_scope, '') = IFNULL(?2, '')",
            params![sound_id, tab_scope],
        )?;
        if let Some(accelerator) = accelerator {
            transaction.execute(
                "INSERT INTO hotkey_bindings(
                     binding_id, sound_id, control_action, target_tab, tab_scope,
                     accelerator, normalized, state, issue
                 ) VALUES(?1, ?2, NULL, NULL, ?3, ?4, ?5, 'active', NULL)",
                params![
                    uuid::Uuid::new_v4().to_string(),
                    sound_id,
                    tab_scope,
                    accelerator,
                    normalized.as_deref()
                ],
            )?;
        }
    }
    transaction.commit()?;
    Ok(sound_ids.len())
}

fn delete_hotkey_binding(connection: &Connection, binding_id: &str) -> Result<bool, LibraryError> {
    Ok(connection.execute(
        "DELETE FROM hotkey_bindings WHERE binding_id = ?1",
        [binding_id],
    )? == 1)
}

fn load_sound_position(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
    sound_id: &str,
) -> Result<Option<usize>, LibraryError> {
    let fts = search_query(search);
    let position: Option<i64> = match scope {
        LibraryScope::General => connection
            .query_row(
                &format!(
                    "WITH anchor(position, public_id) AS (
                         SELECT sound.general_position, sound.public_id
                         FROM sounds AS sound
                         WHERE sound.public_id = ?3
                           AND {LIVE_SOUND_FILTER}
                           AND {SEARCH_FILTER}
                     )
                     SELECT (
                         SELECT COUNT(*)
                         FROM sounds AS sound
                         WHERE {LIVE_SOUND_FILTER}
                           AND {SEARCH_FILTER}
                           AND (sound.general_position < anchor.position
                                OR (sound.general_position = anchor.position
                                    AND sound.public_id < anchor.public_id))
                     )
                     FROM anchor"
                ),
                params![search, fts, sound_id],
                |row| row.get(0),
            )
            .optional()?,
        LibraryScope::ManualTab(tab_id) => connection
            .query_row(
                &format!(
                    "WITH anchor(position, public_id) AS (
                         SELECT membership.position, sound.public_id
                         FROM manual_memberships AS membership
                         JOIN manual_tabs AS tab ON tab.id = membership.tab_id
                         JOIN sounds AS sound ON sound.rowid = membership.sound_id
                         WHERE tab.public_id = ?3
                           AND sound.public_id = ?4
                           AND {SEARCH_FILTER}
                     )
                     SELECT (
                         SELECT COUNT(*)
                         FROM manual_memberships AS membership
                         JOIN manual_tabs AS tab ON tab.id = membership.tab_id
                         JOIN sounds AS sound ON sound.rowid = membership.sound_id
                         WHERE tab.public_id = ?3
                           AND {SEARCH_FILTER}
                           AND (membership.position < anchor.position
                                OR (membership.position = anchor.position
                                    AND sound.public_id < anchor.public_id))
                     )
                     FROM anchor"
                ),
                params![search, fts, tab_id, sound_id],
                |row| row.get(0),
            )
            .optional()?,
        LibraryScope::Folder {
            root_path,
            relative_path,
        } => connection
            .query_row(
                &format!(
                    "WITH selected(folder_id, root_id, generation) AS (
                         SELECT folder.id, root.id, root.active_generation
                         FROM folders AS folder
                         JOIN roots AS root ON root.id = folder.root_id
                         JOIN folder_presence AS presence ON presence.folder_id = folder.id
                             AND presence.generation = root.active_generation
                         WHERE root.path = ?3 AND folder.relative_path = ?4
                     ), effective(sound_id) AS (
                         SELECT location.sound_id FROM selected
                         JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                         JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                             AND location.root_id = selected.root_id
                             AND location.generation = selected.generation
                         UNION
                         SELECT override.sound_id FROM selected
                         JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                         WHERE override.action = 'include'
                         EXCEPT
                         SELECT override.sound_id FROM selected
                         JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                         WHERE override.action = 'exclude'
                     ), anchor(position, public_id) AS (
                         SELECT sound.general_position, sound.public_id
                         FROM effective
                         JOIN sounds AS sound ON sound.rowid = effective.sound_id
                         WHERE sound.public_id = ?5 AND {SEARCH_FILTER}
                     )
                     SELECT (
                         SELECT COUNT(*)
                         FROM effective
                         JOIN sounds AS sound ON sound.rowid = effective.sound_id
                         WHERE {SEARCH_FILTER}
                           AND (sound.general_position < anchor.position
                                OR (sound.general_position = anchor.position
                                    AND sound.public_id < anchor.public_id))
                     )
                     FROM anchor"
                ),
                params![search, fts, root_path, relative_path, sound_id],
                |row| row.get(0),
            )
            .optional()?,
    };

    position
        .map(|value| {
            usize::try_from(value).map_err(|_| {
                LibraryError::InvalidData("negative sound position returned by library".to_string())
            })
        })
        .transpose()
}

fn load_adjacent_sound(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
    position: usize,
    offset: i32,
) -> Result<Option<Sound>, LibraryError> {
    let distance = usize::try_from(offset.unsigned_abs())
        .map_err(|_| LibraryError::InvalidData("adjacent offset overflow".to_string()))?;
    let Some(target) = (if offset.is_negative() {
        position.checked_sub(distance)
    } else {
        position.checked_add(distance)
    }) else {
        return Ok(None);
    };
    load_sound_at(connection, scope, search, target)
}

fn load_sound_at(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
    position: usize,
) -> Result<Option<Sound>, LibraryError> {
    let fts = search_query(search);
    let offset = usize_to_i64(position)?;
    let fields = any_sound_fields();
    match scope {
        LibraryScope::General => connection
            .query_row(
                &format!(
                    "SELECT {fields} FROM sounds AS sound
                     WHERE {LIVE_SOUND_FILTER} AND {SEARCH_FILTER}
                     ORDER BY sound.general_position, sound.public_id LIMIT 1 OFFSET ?3"
                ),
                params![search, fts, offset],
                sound_from_row,
            )
            .optional()
            .map_err(LibraryError::from),
        LibraryScope::ManualTab(tab_id) => connection
            .query_row(
                &format!(
                    "SELECT {fields} FROM manual_memberships AS membership
                     JOIN manual_tabs AS tab ON tab.id = membership.tab_id
                     JOIN sounds AS sound ON sound.rowid = membership.sound_id
                     WHERE {SEARCH_FILTER} AND tab.public_id = ?3
                     ORDER BY membership.position, sound.public_id LIMIT 1 OFFSET ?4"
                ),
                params![search, fts, tab_id, offset],
                sound_from_row,
            )
            .optional()
            .map_err(LibraryError::from),
        LibraryScope::Folder {
            root_path,
            relative_path,
        } => connection
            .query_row(
                &format!(
                    "WITH selected(folder_id, root_id, generation) AS (
                         SELECT folder.id, root.id, root.active_generation
                         FROM folders AS folder
                         JOIN roots AS root ON root.id = folder.root_id
                         JOIN folder_presence AS presence ON presence.folder_id = folder.id
                             AND presence.generation = root.active_generation
                         WHERE root.path = ?3 AND folder.relative_path = ?4
                     ), effective(sound_id) AS (
                         SELECT location.sound_id FROM selected
                         JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                         JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                             AND location.root_id = selected.root_id
                             AND location.generation = selected.generation
                         UNION
                         SELECT override.sound_id FROM selected
                         JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                         WHERE override.action = 'include'
                         EXCEPT
                         SELECT override.sound_id FROM selected
                         JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                         WHERE override.action = 'exclude'
                     )
                     SELECT {fields} FROM effective
                     JOIN sounds AS sound ON sound.rowid = effective.sound_id
                     WHERE {SEARCH_FILTER}
                     ORDER BY sound.general_position, sound.public_id LIMIT 1 OFFSET ?5"
                ),
                params![search, fts, root_path, relative_path, offset],
                sound_from_row,
            )
            .optional()
            .map_err(LibraryError::from),
    }
}

fn sound_from_row(row: &Row<'_>) -> rusqlite::Result<Sound> {
    let duration_ms = row
        .get::<_, Option<i64>>(5)?
        .map(|value| {
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    let volume = u8::try_from(row.get::<_, i64>(6)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    let loudness_state = LoudnessAnalysisState::from_str(&row.get::<_, String>(9)?)
        .unwrap_or(LoudnessAnalysisState::Pending);
    Ok(Sound {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        source_path: row.get(3)?,
        hotkey: row.get(4)?,
        duration_ms,
        volume,
        enabled: row.get::<_, i64>(7)? != 0,
        loudness_lufs: row.get(8)?,
        loudness_analysis_state: loudness_state,
        loudness_confidence: row.get(10)?,
        loudness_source_fingerprint: row.get(11)?,
        loudness_true_peak_dbtp: row.get(12)?,
    })
}

fn usize_to_i64(value: usize) -> Result<i64, LibraryError> {
    i64::try_from(value)
        .map_err(|_| LibraryError::InvalidData("library index exceeds SQLite range".to_string()))
}
