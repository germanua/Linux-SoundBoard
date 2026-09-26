fn sha256(path: &Path) -> Result<String, LegacyMigrationError> {
    sha256_observed(
        path,
        &noop_observer(),
        LegacyMigrationBoundary::BeforeBackupWrite,
    )
}

fn sha256_observed(
    path: &Path,
    observer: &MigrationObserver,
    boundary: LegacyMigrationBoundary,
) -> Result<String, LegacyMigrationError> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        observer(boundary)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn remove_stale_candidates(parent: &Path, prefix: &str) -> Result<(), LegacyMigrationError> {
    let mut removed = false;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name().to_string_lossy().starts_with(prefix) {
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn ensure_backup(
    source: &Path,
    expected_sha256: &str,
    observer: &MigrationObserver,
) -> Result<(), LegacyMigrationError> {
    let backup = source.with_file_name("config.json.pre-v8-backup");
    if backup.exists() {
        if sha256(&backup)? == expected_sha256 {
            fs::set_permissions(backup, fs::Permissions::from_mode(0o600))?;
            return Ok(());
        }
        return Err(LegacyMigrationError::Invalid(
            "the existing pre-v8 backup does not match the migration source".to_string(),
        ));
    }

    let parent = source.parent().ok_or_else(|| {
        LegacyMigrationError::Invalid("legacy config has no parent directory".to_string())
    })?;
    remove_stale_candidates(parent, ".config.json.pre-v8-backup.")?;
    let candidate = source.with_file_name(format!(
        ".config.json.pre-v8-backup.{}",
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        observer(LegacyMigrationBoundary::BeforeBackupWrite)?;
        let mut reader = BufReader::new(fs::File::open(source)?);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            observer(LegacyMigrationBoundary::BeforeBackupWrite)?;
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            file.write_all(&buffer[..count])?;
        }
        file.sync_all()?;
        observer(LegacyMigrationBoundary::BackupSynced)?;
        if sha256_observed(
            &candidate,
            observer,
            LegacyMigrationBoundary::BeforeBackupWrite,
        )? != expected_sha256
        {
            return Err(LegacyMigrationError::Invalid(
                "the legacy config changed while its migration backup was being created"
                    .to_string(),
            ));
        }
        fs::rename(&candidate, &backup)?;
        if let Some(parent) = backup.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        observer(LegacyMigrationBoundary::BackupPublished)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(candidate);
    }
    result
}

fn parse_pass(
    source: &Path,
    state: &mut ImportState,
    memberships: bool,
) -> Result<(), LegacyMigrationError> {
    let reader = BufReader::new(fs::File::open(source)?);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    if memberships {
        MembershipPassSeed(state).deserialize(&mut deserializer)?;
    } else {
        FirstPassSeed(state).deserialize(&mut deserializer)?;
    }
    deserializer.end()?;
    Ok(())
}

#[derive(Default)]
struct LegacySettingsState {
    schema_version: Option<u32>,
    settings: Option<serde_json::Value>,
}

struct LegacySettingsSeed<'a>(&'a mut LegacySettingsState);

impl<'de> DeserializeSeed<'de> for LegacySettingsSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SettingsVisitor<'a>(&'a mut LegacySettingsState);
        impl<'de> Visitor<'de> for SettingsVisitor<'_> {
            type Value = ();

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a legacy Linux SoundBoard configuration object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                while let Some(field) = map.next_key::<String>()? {
                    match field.as_str() {
                        "schema_version" => self.0.schema_version = Some(map.next_value()?),
                        "settings" => self.0.settings = Some(map.next_value()?),
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(())
            }
        }
        deserializer.deserialize_map(SettingsVisitor(self.0))
    }
}

pub fn config_schema_version(path: &Path) -> Result<u32, LegacyMigrationError> {
    Ok(read_legacy_settings_state(path)?
        .schema_version
        .unwrap_or(0))
}

fn read_legacy_settings_state(path: &Path) -> Result<LegacySettingsState, LegacyMigrationError> {
    let reader = BufReader::new(fs::File::open(path)?);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let mut state = LegacySettingsState::default();
    LegacySettingsSeed(&mut state).deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(state)
}

pub(crate) fn read_legacy_runtime_settings(path: &Path) -> Result<Settings, LegacyMigrationError> {
    let mut state = read_legacy_settings_state(path)?;
    let version = state.schema_version.unwrap_or(0);
    if version > crate::config::LAST_LEGACY_SCHEMA_VERSION {
        return Err(LegacyMigrationError::Invalid(format!(
            "schema {version} is not a legacy configuration"
        )));
    }
    let migrated = crate::config::migration::run_migrations(
        serde_json::json!({
            "schema_version": version,
            "sound_folders": [],
            "sounds": [],
            "tabs": [],
            "settings": state.settings.take().unwrap_or_else(|| serde_json::json!({})),
        }),
        version,
    )?;
    let mut settings: Settings = serde_json::from_value(migrated["settings"].clone())?;
    settings.control_hotkeys = Default::default();
    Ok(settings)
}

fn finalize_database(
    path: &Path,
    source_sha256: &str,
    library_id: &str,
    expected_sounds: usize,
    expected_hotkeys: usize,
    observer: &MigrationObserver,
) -> Result<(), LegacyMigrationError> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "INSERT OR IGNORE INTO manual_tabs(public_id, name, position)
         SELECT public_id, name, position FROM legacy_generated_tabs;
         INSERT OR IGNORE INTO manual_memberships(tab_id, sound_id, position)
         SELECT manual.id, legacy.sound_id, legacy.position
         FROM legacy_generated_memberships AS legacy
         JOIN legacy_generated_tabs AS generated ON generated.id = legacy.tab_id
         JOIN manual_tabs AS manual ON manual.public_id = generated.public_id;
         UPDATE hotkey_bindings AS binding
         SET issue = 'duplicate legacy binding'
         WHERE issue = 'valid legacy candidate'
           AND (SELECT COUNT(*) FROM hotkey_bindings AS other
                WHERE other.accelerator = binding.accelerator
                  AND other.issue = 'valid legacy candidate') > 1;
         UPDATE hotkey_bindings AS binding
         SET normalized = accelerator, state = 'active', issue = NULL
         WHERE issue = 'valid legacy candidate'
           AND (SELECT COUNT(*) FROM hotkey_bindings AS other
                WHERE other.accelerator = binding.accelerator
                  AND other.issue = 'valid legacy candidate') = 1;",
    )?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(LegacyMigrationError::Invalid(format!(
            "migrated database failed integrity_check: {integrity}"
        )));
    }
    let foreign_key_errors: usize = connection
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |_| Ok(()))?
        .count();
    if foreign_key_errors != 0 {
        return Err(LegacyMigrationError::Invalid(format!(
            "migrated database has {foreign_key_errors} foreign-key errors"
        )));
    }
    let sounds: i64 = connection.query_row("SELECT count(*) FROM sounds", [], |row| row.get(0))?;
    let hotkeys: i64 =
        connection.query_row("SELECT count(*) FROM hotkey_bindings", [], |row| row.get(0))?;
    let sounds = usize::try_from(sounds)
        .map_err(|_| LegacyMigrationError::Invalid("negative sound count".to_string()))?;
    let hotkeys = usize::try_from(hotkeys)
        .map_err(|_| LegacyMigrationError::Invalid("negative hotkey count".to_string()))?;
    if sounds != expected_sounds || hotkeys != expected_hotkeys {
        return Err(LegacyMigrationError::Invalid(format!(
            "migration verification mismatch: sounds {sounds}/{expected_sounds}, hotkeys {hotkeys}/{expected_hotkeys}"
        )));
    }
    connection.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('source_sha256', ?1)",
        [source_sha256],
    )?;
    connection.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('library_id', ?1)",
        [library_id],
    )?;
    connection.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('database_ready', '1')",
        [],
    )?;
    drop(connection);
    fs::File::open(path)?.sync_all()?;
    observer(LegacyMigrationBoundary::DatabaseSynced)?;
    Ok(())
}
