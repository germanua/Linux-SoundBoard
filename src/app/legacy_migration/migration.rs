pub fn migrate_legacy_database(
    source: &Path,
    destination: &Path,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    migrate_legacy_database_observed(source, destination, noop_observer())
}

fn migrate_legacy_database_observed(
    source: &Path,
    destination: &Path,
    observer: MigrationObserver,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    if destination.exists() {
        return Err(LegacyMigrationError::Invalid(format!(
            "refusing to replace existing database '{}'",
            destination.display()
        )));
    }
    let source_sha256 = sha256_observed(
        source,
        &observer,
        LegacyMigrationBoundary::BeforeBackupWrite,
    )?;
    ensure_backup(source, &source_sha256, &observer)?;
    let parent = destination.parent().ok_or_else(|| {
        LegacyMigrationError::Invalid("library database has no parent directory".to_string())
    })?;
    remove_stale_candidates(parent, ".library.sqlite3.importing.")?;
    let candidate = destination.with_file_name(format!(
        ".library.sqlite3.importing.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> Result<LegacyMigrationReport, LegacyMigrationError> {
        observer(LegacyMigrationBoundary::BeforeDatabaseWrite)?;
        let store = LibraryStore::open(candidate.clone())?;
        let mut state = ImportState::new(store, Arc::clone(&observer));
        parse_pass(source, &mut state, false)?;
        state.finish_first_pass()?;
        parse_pass(source, &mut state, true)?;
        state.flush_memberships()?;
        state.flush_generated_memberships()?;
        let report = LegacyMigrationReport {
            sounds: state.sound_count,
            roots: state.root_count,
            manual_tabs: state.manual_tab_count,
            manual_memberships: state.membership_count,
            generated_tabs_deferred: state.generated_tab_count,
            hotkeys: state.hotkey_count,
            source_sha256: source_sha256.clone(),
            library_id: uuid::Uuid::new_v4().to_string(),
            settings: state.migrated_settings.take().ok_or_else(|| {
                LegacyMigrationError::Invalid("legacy settings were not imported".to_string())
            })?,
        };
        drop(state);
        finalize_database(
            &candidate,
            &report.source_sha256,
            &report.library_id,
            report.sounds,
            report.hotkeys,
            &observer,
        )?;
        if sha256_observed(source, &observer, LegacyMigrationBoundary::DatabaseSynced)?
            != source_sha256
        {
            return Err(LegacyMigrationError::Invalid(
                "legacy config changed while migration was running".to_string(),
            ));
        }
        observer(LegacyMigrationBoundary::BeforeDatabasePublish)?;
        fs::rename(&candidate, destination)?;
        if let Some(parent) = destination.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        observer(LegacyMigrationBoundary::DatabasePublished)?;
        Ok(report)
    })();
    if result.is_err() {
        let _ = fs::remove_file(candidate);
    }
    result
}

pub fn database_identity(path: &Path) -> Result<DatabaseIdentity, LegacyMigrationError> {
    if !path.is_file() {
        return Err(LegacyMigrationError::Invalid(format!(
            "library database '{}' is missing",
            path.display()
        )));
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id != 0 && application_id != crate::library_store::DATABASE_APPLICATION_ID {
        return Err(LegacyMigrationError::Invalid(format!(
            "library database application id is {application_id}, expected {}",
            crate::library_store::DATABASE_APPLICATION_ID
        )));
    }
    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !(1..=crate::library_store::DATABASE_SCHEMA_VERSION).contains(&schema_version) {
        return Err(LegacyMigrationError::Invalid(format!(
            "library database schema is {schema_version}, expected 1 to {}",
            crate::library_store::DATABASE_SCHEMA_VERSION
        )));
    }
    let ready: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'database_ready'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if ready.as_deref() != Some("1") {
        return Err(LegacyMigrationError::Invalid(
            "library database is not marked ready".to_string(),
        ));
    }
    let flavor: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_flavor'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let expected_flavor = format!("bounded-generation-v{schema_version}");
    if flavor.as_deref() != Some(expected_flavor.as_str()) {
        return Err(LegacyMigrationError::Invalid(
            "library database metadata does not match the bounded schema".to_string(),
        ));
    }
    let library_id: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'library_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let library_id = library_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            LegacyMigrationError::Invalid("library database has no identity".to_string())
        })?;
    let source_sha256 = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'source_sha256'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if application_id == 0 {
        connection.pragma_update(
            None,
            "application_id",
            crate::library_store::DATABASE_APPLICATION_ID,
        )?;
        fs::File::open(path)?.sync_all()?;
    }
    Ok(DatabaseIdentity {
        library_id,
        source_sha256,
    })
}

fn write_schema_8_config(path: &Path, settings: Settings) -> Result<(), LegacyMigrationError> {
    write_schema_8_config_observed(path, settings, &noop_observer())
}

fn write_schema_8_config_observed(
    path: &Path,
    settings: Settings,
    observer: &MigrationObserver,
) -> Result<(), LegacyMigrationError> {
    let mut config = crate::config::Config {
        settings,
        ..crate::config::Config::default()
    };
    let result = config.save_to_path_observed(path, |boundary| {
        observer(match boundary {
            crate::config::ConfigSaveBoundary::CandidateSynced => {
                LegacyMigrationBoundary::SettingsSynced
            }
            crate::config::ConfigSaveBoundary::Renamed => LegacyMigrationBoundary::SettingsRenamed,
            crate::config::ConfigSaveBoundary::DirectorySynced => {
                LegacyMigrationBoundary::SettingsPublished
            }
        })
    });
    match result {
        Ok(()) => Ok(()),
        Err(error) => match error.downcast::<LegacyMigrationError>() {
            Ok(error) => Err(*error),
            Err(error) => Err(LegacyMigrationError::Invalid(error.to_string())),
        },
    }
}

pub fn complete_legacy_settings_cutover(
    source: &Path,
    destination: &Path,
) -> Result<DatabaseIdentity, LegacyMigrationError> {
    let identity = database_identity(destination)?;
    let expected_source = identity.source_sha256.as_deref().ok_or_else(|| {
        LegacyMigrationError::Invalid("migrated database has no source checksum".to_string())
    })?;
    if sha256(source)? != expected_source {
        return Err(LegacyMigrationError::Invalid(
            "legacy config does not match the ready database".to_string(),
        ));
    }
    let settings = read_legacy_runtime_settings(source)?;
    write_schema_8_config(source, settings)?;
    let config = crate::config::Config::load_from_path(source)
        .map_err(|error| LegacyMigrationError::Invalid(error.to_string()))?;
    if config.schema_version != crate::config::CURRENT_SCHEMA_VERSION {
        return Err(LegacyMigrationError::Invalid(
            "settings schema verification failed".to_string(),
        ));
    }
    Ok(identity)
}

pub fn migrate_legacy_config(
    source: &Path,
    destination: &Path,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    migrate_legacy_config_observed(source, destination, noop_observer())
}

pub fn migrate_legacy_config_controlled(
    source: &Path,
    destination: &Path,
    cancelled: Arc<AtomicBool>,
    on_progress: Arc<dyn Fn(LegacyMigrationProgress) + Send + Sync>,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    let last_progress = AtomicU8::new(u8::MAX);
    migrate_legacy_config_observed(
        source,
        destination,
        Arc::new(move |boundary| {
            if cancelled.load(Ordering::Relaxed) {
                return Err(LegacyMigrationError::Cancelled);
            }
            let progress = match boundary {
                LegacyMigrationBoundary::BeforeBackupWrite
                | LegacyMigrationBoundary::BackupSynced
                | LegacyMigrationBoundary::BackupPublished => LegacyMigrationProgress::BackingUp,
                LegacyMigrationBoundary::BeforeDatabaseWrite
                | LegacyMigrationBoundary::DatabaseBatchCommitted => {
                    LegacyMigrationProgress::Importing
                }
                LegacyMigrationBoundary::DatabaseSynced => LegacyMigrationProgress::Verifying,
                LegacyMigrationBoundary::BeforeDatabasePublish
                | LegacyMigrationBoundary::DatabasePublished => {
                    LegacyMigrationProgress::PublishingDatabase
                }
                LegacyMigrationBoundary::BeforeSettingsWrite
                | LegacyMigrationBoundary::SettingsSynced
                | LegacyMigrationBoundary::SettingsRenamed => {
                    LegacyMigrationProgress::PublishingSettings
                }
                LegacyMigrationBoundary::SettingsPublished => LegacyMigrationProgress::Complete,
            };
            let progress_id = progress as u8;
            if last_progress.swap(progress_id, Ordering::Relaxed) != progress_id {
                on_progress(progress);
            }
            if cancelled.load(Ordering::Relaxed) {
                Err(LegacyMigrationError::Cancelled)
            } else {
                Ok(())
            }
        }),
    )
}

fn migrate_legacy_config_observed(
    source: &Path,
    destination: &Path,
    observer: MigrationObserver,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    let report = migrate_legacy_database_observed(source, destination, Arc::clone(&observer))?;
    observer(LegacyMigrationBoundary::BeforeSettingsWrite)?;
    write_schema_8_config_observed(source, report.settings.clone(), &observer)?;
    let identity = database_identity(destination)?;
    if identity.library_id != report.library_id {
        return Err(LegacyMigrationError::Invalid(
            "published settings/database identity mismatch".to_string(),
        ));
    }
    Ok(report)
}

pub fn restore_legacy_backup(
    config_path: &Path,
    library_path: &Path,
) -> Result<LegacyRestoreReport, LegacyMigrationError> {
    let backup = config_path.with_file_name("config.json.pre-v8-backup");
    if !backup.is_file() {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy backup '{}' is missing",
            backup.display()
        )));
    }
    let version = config_schema_version(&backup)?;
    if version > crate::config::LAST_LEGACY_SCHEMA_VERSION {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy backup schema {version} is not restorable"
        )));
    }

    let suffix = uuid::Uuid::new_v4();
    let archived_config = config_path
        .exists()
        .then(|| config_path.with_file_name(format!("config.json.pre-restore-{suffix}")));
    let archived_database = library_path
        .exists()
        .then(|| library_path.with_file_name(format!("library.sqlite3.pre-restore-{suffix}")));
    let candidate = config_path.with_file_name(format!(".config.json.restoring-{suffix}"));

    let result = (|| -> Result<(), LegacyMigrationError> {
        let mut reader = BufReader::new(fs::File::open(&backup)?);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)?;
        let mut writer = BufWriter::new(&file);
        std::io::copy(&mut reader, &mut writer)?;
        writer.flush()?;
        drop(writer);
        file.sync_all()?;

        if let Some(path) = &archived_database {
            fs::rename(library_path, path)?;
        }
        if let Some(path) = &archived_config {
            if let Err(error) = fs::rename(config_path, path) {
                if let Some(database) = &archived_database {
                    let _ = fs::rename(database, library_path);
                }
                return Err(error.into());
            }
        }
        if let Err(error) = fs::rename(&candidate, config_path) {
            if let Some(path) = &archived_config {
                let _ = fs::rename(path, config_path);
            }
            if let Some(path) = &archived_database {
                let _ = fs::rename(path, library_path);
            }
            return Err(error.into());
        }
        fs::set_permissions(config_path, fs::Permissions::from_mode(0o600))?;
        if let Some(parent) = config_path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(candidate);
    }
    result?;
    Ok(LegacyRestoreReport {
        archived_config,
        archived_database,
    })
}

pub fn initialize_empty_library(path: &Path, library_id: &str) -> Result<(), LegacyMigrationError> {
    if path.exists() {
        return Err(LegacyMigrationError::Invalid(format!(
            "refusing to replace existing database '{}'",
            path.display()
        )));
    }
    let store = LibraryStore::open(path.to_path_buf())?;
    drop(store);
    let connection = Connection::open(path)?;
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
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
