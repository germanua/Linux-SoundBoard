#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SoundTab};
    use crate::library_store::LibraryScope;

    #[test]
    fn legacy_import_streams_bounded_batches_and_preserves_manual_data() {
        let directory =
            std::env::temp_dir().join(format!("lsb-legacy-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        config.sound_folders.push("/music".to_string());
        for index in 0..1_025 {
            let mut sound = Sound::new(
                format!("Sound {index}"),
                format!("/music/sound-{index}.flac"),
            );
            sound.id = format!("sound-{index}");
            if index < 2 {
                sound.hotkey = Some(format!("Ctrl+Digit{}", index + 1));
            }
            config.sounds.push(sound);
        }
        let mut manual = SoundTab::new("Manual".to_string(), 0);
        manual.id = "manual".to_string();
        manual.sound_ids = config.sounds.iter().map(|sound| sound.id.clone()).collect();
        config.tabs.push(manual);
        let mut generated = SoundTab::new("Generated".to_string(), 1);
        generated.id = "generated".to_string();
        generated.sound_ids.push("sound-0".to_string());
        generated.folder_binding = Some(FolderTabBinding {
            root_folder: "/music".to_string(),
            relative_subfolder: "album".to_string(),
        });
        config.tabs.push(generated);
        serde_json::to_writer(fs::File::create(&source).expect("create config"), &config)
            .expect("write config");

        let report = migrate_legacy_database(&source, &destination).expect("migrate config");

        assert_eq!(report.sounds, 1_025);
        assert_eq!(report.manual_tabs, 1);
        assert_eq!(report.manual_memberships, 1_025);
        assert_eq!(report.generated_tabs_deferred, 1);
        assert_eq!(report.hotkeys, 2);
        assert!(directory.join("config.json.pre-v8-backup").exists());
        let store = LibraryStore::open(destination).expect("open migrated database");
        assert_eq!(
            store
                .count(LibraryScope::General, "")
                .recv()
                .expect("count sounds"),
            1_025
        );
        assert_eq!(
            store
                .manual_tabs(0)
                .recv()
                .expect("load migrated tabs")
                .total,
            2
        );
        assert_eq!(
            store
                .count(LibraryScope::ManualTab("manual".to_string()), "")
                .recv()
                .expect("count manual membership"),
            1_025
        );
        drop(store);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn malformed_legacy_config_never_publishes_a_database() {
        let directory =
            std::env::temp_dir().join(format!("lsb-bad-legacy-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        fs::write(&source, br#"{"schema_version":7,"sounds":["#).expect("write malformed config");

        assert!(migrate_legacy_database(&source, &destination).is_err());
        assert!(!destination.exists());
        assert!(!directory
            .read_dir()
            .expect("read test directory")
            .any(|entry| entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .contains(".importing")));

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn pre_v8_backup_is_independent_from_in_place_source_writes() {
        let directory =
            std::env::temp_dir().join(format!("lsb-backup-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let original = br#"{"schema_version":7,"settings":{}}"#;
        fs::write(&source, original).expect("write source");

        ensure_backup(
            &source,
            &sha256(&source).expect("hash source"),
            &noop_observer(),
        )
        .expect("create backup");
        fs::write(
            &source,
            br#"{"schema_version":7,"settings":{"theme":"light"}}"#,
        )
        .expect("rewrite source in place");

        assert_eq!(
            fs::read(directory.join("config.json.pre-v8-backup")).expect("read backup"),
            original
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn restart_removes_stale_migration_candidates_before_retry() {
        let directory =
            std::env::temp_dir().join(format!("lsb-stale-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let stale_backup = directory.join(".config.json.pre-v8-backup.interrupted");
        let stale_database = directory.join(".library.sqlite3.importing.interrupted");
        let config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        fs::write(&stale_backup, b"partial backup").expect("write stale backup candidate");
        fs::write(&stale_database, b"partial database").expect("write stale database candidate");

        migrate_legacy_database(&source, &destination).expect("retry migration");

        assert!(!stale_backup.exists());
        assert!(!stale_database.exists());
        assert!(destination.exists());
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn read_only_destination_preserves_legacy_source_and_publishes_nothing() {
        let directory =
            std::env::temp_dir().join(format!("lsb-read-only-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        let source_before = fs::read(&source).expect("read legacy source");
        ensure_backup(
            &source,
            &sha256(&source).expect("hash source"),
            &noop_observer(),
        )
        .expect("create backup");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
            .expect("make destination read-only");

        let result = migrate_legacy_database(&source, &destination);

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("restore destination permissions");
        assert!(result.is_err());
        assert_eq!(
            fs::read(&source).expect("read preserved source"),
            source_before
        );
        assert!(!destination.exists());
        assert!(!directory
            .read_dir()
            .expect("read test directory")
            .any(|entry| entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .contains(".importing")));
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn duplicate_and_invalid_hotkeys_survive_as_needs_attention() {
        let directory =
            std::env::temp_dir().join(format!("lsb-legacy-hotkeys-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        for (id, hotkey) in [
            ("first", "Ctrl+KeyA"),
            ("second", "Ctrl+KeyA"),
            ("invalid", "not a hotkey"),
        ] {
            let mut sound = Sound::new(id.to_string(), format!("/music/{id}.flac"));
            sound.id = id.to_string();
            sound.hotkey = Some(hotkey.to_string());
            config.sounds.push(sound);
        }
        serde_json::to_writer(fs::File::create(&source).expect("create config"), &config)
            .expect("write config");

        let report = migrate_legacy_database(&source, &destination).expect("migrate config");

        assert_eq!(report.hotkeys, 3);
        let connection = Connection::open(&destination).expect("open migrated database");
        let preserved: i64 = connection
            .query_row("SELECT count(*) FROM hotkey_bindings", [], |row| row.get(0))
            .expect("count preserved bindings");
        let attention: i64 = connection
            .query_row(
                "SELECT count(*) FROM hotkey_bindings WHERE state = 'needs_attention'",
                [],
                |row| row.get(0),
            )
            .expect("count attention bindings");
        assert_eq!(preserved, 3);
        assert_eq!(attention, 3);
        drop(connection);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn importer_accepts_every_legacy_schema_number_zero_through_seven() {
        for version in 0..=crate::config::LAST_LEGACY_SCHEMA_VERSION {
            let directory = std::env::temp_dir().join(format!(
                "lsb-legacy-schema-{version}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let source = directory.join("config.json");
            let destination = directory.join("library.sqlite3");
            let mut value = serde_json::to_value(Config::default()).expect("serialize config");
            value["schema_version"] = serde_json::json!(version);
            serde_json::to_writer(fs::File::create(&source).expect("create config"), &value)
                .expect("write config");

            migrate_legacy_database(&source, &destination)
                .unwrap_or_else(|error| panic!("schema {version} failed: {error}"));

            assert!(destination.exists());
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn authentic_v2_schema_6_fixture_preserves_library_settings_and_hotkeys() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema6-fixture-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        fs::write(
            &source,
            include_bytes!("../../tests/fixtures/config-v2.0-schema6.json"),
        )
        .expect("write historical fixture");

        let report = migrate_legacy_config(&source, &destination).expect("migrate fixture");

        assert_eq!(report.sounds, 1);
        assert_eq!(report.roots, 1);
        assert_eq!(report.manual_tabs, 1);
        assert_eq!(report.manual_memberships, 1);
        assert_eq!(report.hotkeys, 8);
        assert_eq!(report.settings.theme, crate::config::Theme::Light);
        assert_eq!(report.settings.local_volume, 41);
        let store = LibraryStore::open(destination).expect("open migrated database");
        let sound = store
            .sound_by_id("sound-distinctive")
            .recv()
            .expect("load fixture sound")
            .expect("fixture sound exists");
        assert_eq!(sound.name, "Upgrade fixture");
        assert_eq!(sound.duration_ms, Some(4_321));
        drop(store);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn authentic_v2_1_schema_7_fixture_preserves_generated_folder_state() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema7-fixture-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        fs::write(
            &source,
            include_bytes!("../../tests/fixtures/config-v2.1-schema7.json"),
        )
        .expect("write historical fixture");

        let report = migrate_legacy_config(&source, &destination).expect("migrate fixture");

        assert_eq!(report.sounds, 1);
        assert_eq!(report.roots, 1);
        assert_eq!(report.manual_tabs, 1);
        assert_eq!(report.manual_memberships, 1);
        assert_eq!(report.generated_tabs_deferred, 1);
        assert_eq!(report.hotkeys, 2);
        let store = LibraryStore::open(destination).expect("open migrated database");
        assert_eq!(
            store
                .manual_tabs(0)
                .recv()
                .expect("load migrated tabs")
                .total,
            2
        );
        drop(store);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn full_cutover_publishes_small_schema_8_settings_with_matching_identity() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema8-cutover-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        config.sound_folders.push("/music".to_string());
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        config.settings.control_hotkeys.stop_all = Some("Ctrl+KeyS".to_string());
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();

        let report = migrate_legacy_config(&source, &destination).expect("complete migration");
        let persisted: serde_json::Value =
            serde_json::from_reader(fs::File::open(&source).unwrap()).unwrap();
        assert_eq!(persisted["schema_version"], serde_json::json!(8));
        assert!(persisted.get("library_id").is_none());
        assert!(persisted.get("sounds").is_none());
        assert!(persisted.get("sound_folders").is_none());
        assert!(persisted["settings"].get("control_hotkeys").is_none());
        assert_eq!(
            database_identity(&destination).unwrap().library_id,
            report.library_id
        );
        assert_eq!(report.hotkeys, 1);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn restart_finishes_settings_switch_after_database_publication() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema8-resume-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();

        let report = migrate_legacy_database(&source, &destination).expect("publish database");
        assert_eq!(config_schema_version(&source).unwrap(), 7);
        let identity = complete_legacy_settings_cutover(&source, &destination)
            .expect("resume settings switch");
        assert_eq!(identity.library_id, report.library_id);
        assert_eq!(config_schema_version(&source).unwrap(), 8);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn controlled_migration_reports_ordered_progress() {
        let directory =
            std::env::temp_dir().join(format!("lsb-migration-progress-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&progress);

        migrate_legacy_config_controlled(
            &source,
            &destination,
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |update| captured.lock().unwrap().push(update)),
        )
        .expect("run controlled migration");

        assert_eq!(
            *progress.lock().unwrap(),
            [
                LegacyMigrationProgress::BackingUp,
                LegacyMigrationProgress::Importing,
                LegacyMigrationProgress::Verifying,
                LegacyMigrationProgress::PublishingDatabase,
                LegacyMigrationProgress::PublishingSettings,
                LegacyMigrationProgress::Complete,
            ]
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn controlled_migration_cancels_before_publication_and_retries_cleanly() {
        let directory =
            std::env::temp_dir().join(format!("lsb-migration-cancel-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        let source_before = fs::read(&source).expect("read source");
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_from_progress = Arc::clone(&cancelled);

        let result = migrate_legacy_config_controlled(
            &source,
            &destination,
            cancelled,
            Arc::new(move |progress| {
                if progress == LegacyMigrationProgress::Importing {
                    cancel_from_progress.store(true, Ordering::Relaxed);
                }
            }),
        );

        assert!(matches!(result, Err(LegacyMigrationError::Cancelled)));
        assert_eq!(fs::read(&source).unwrap(), source_before);
        assert!(!destination.exists());
        migrate_legacy_config(&source, &destination).expect("retry cancelled migration");
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn every_durable_migration_boundary_is_restart_safe() {
        for boundary in LegacyMigrationBoundary::ALL {
            let directory = std::env::temp_dir().join(format!(
                "lsb-migration-boundary-{boundary:?}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let source = directory.join("config.json");
            let destination = directory.join("library.sqlite3");
            let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
            config.sounds.push(Sound::new(
                "Tone".to_string(),
                "/music/tone.wav".to_string(),
            ));
            serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
            let source_before = fs::read(&source).expect("read legacy source");

            let result = migrate_legacy_config_observed(
                &source,
                &destination,
                Arc::new(move |current| {
                    if current == boundary {
                        return Err(LegacyMigrationError::Io(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            format!("interrupted at {current:?}"),
                        )));
                    }
                    Ok(())
                }),
            );

            assert!(result.is_err(), "{boundary:?} did not interrupt");
            assert!(
                !directory
                    .read_dir()
                    .expect("read migration directory")
                    .any(|entry| {
                        let name = entry
                            .expect("read migration entry")
                            .file_name()
                            .to_string_lossy()
                            .into_owned();
                        name.starts_with(".library.sqlite3.importing.")
                            || name.starts_with(".config.json.pre-v8-backup.")
                            || name.starts_with("config.json.tmp.")
                    }),
                "{boundary:?} left a disposable candidate"
            );

            if destination.exists() {
                database_identity(&destination).expect("published database remains valid");
                if config_schema_version(&source).expect("read config schema")
                    <= crate::config::LAST_LEGACY_SCHEMA_VERSION
                {
                    complete_legacy_settings_cutover(&source, &destination)
                        .expect("restart completes settings cutover");
                }
                assert_eq!(
                    config_schema_version(&source).expect("read recovered schema"),
                    crate::config::CURRENT_SCHEMA_VERSION
                );
            } else {
                assert_eq!(
                    fs::read(&source).expect("read preserved legacy source"),
                    source_before
                );
                migrate_legacy_config(&source, &destination)
                    .expect("restart retries unpublished migration");
            }

            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn abrupt_exit_at_every_durable_boundary_recovers_on_restart() {
        for boundary in LegacyMigrationBoundary::ALL {
            let directory = std::env::temp_dir().join(format!(
                "lsb-migration-crash-{boundary:?}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let source = directory.join("config.json");
            let destination = directory.join("library.sqlite3");
            let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
            config.sounds.push(Sound::new(
                "Tone".to_string(),
                "/music/tone.wav".to_string(),
            ));
            serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();

            let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = migrate_legacy_config_observed(
                    &source,
                    &destination,
                    Arc::new(move |current| {
                        if current == boundary {
                            panic!("simulated process exit at {current:?}");
                        }
                        Ok(())
                    }),
                );
            }));
            assert!(interrupted.is_err(), "{boundary:?} did not stop");

            if destination.exists() {
                database_identity(&destination).expect("published database remains valid");
                if config_schema_version(&source).unwrap()
                    <= crate::config::LAST_LEGACY_SCHEMA_VERSION
                {
                    complete_legacy_settings_cutover(&source, &destination)
                        .expect("restart completes settings cutover");
                }
            } else {
                migrate_legacy_config(&source, &destination)
                    .expect("restart retries unpublished migration");
            }
            assert_eq!(
                config_schema_version(&source).unwrap(),
                crate::config::CURRENT_SCHEMA_VERSION
            );
            database_identity(&destination).expect("recovered database remains valid");
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn disk_full_at_each_write_phase_preserves_a_restartable_source() {
        for boundary in LegacyMigrationBoundary::WRITE_PHASES {
            let directory = std::env::temp_dir().join(format!(
                "lsb-migration-disk-full-{boundary:?}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let source = directory.join("config.json");
            let destination = directory.join("library.sqlite3");
            let config = crate::test_support::legacy_config::LegacyConfigFixture::default();
            serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
            let source_before = fs::read(&source).expect("read legacy source");

            let result = migrate_legacy_config_observed(
                &source,
                &destination,
                Arc::new(move |current| {
                    if current == boundary {
                        return Err(LegacyMigrationError::Io(std::io::Error::from_raw_os_error(
                            28,
                        )));
                    }
                    Ok(())
                }),
            );

            let error = result.expect_err("disk-full injection must fail");
            let LegacyMigrationError::Io(error) = error else {
                panic!("expected disk-full I/O error, got {error}");
            };
            assert_eq!(error.raw_os_error(), Some(28));
            assert_eq!(fs::read(&source).unwrap(), source_before);
            if boundary == LegacyMigrationBoundary::BeforeSettingsWrite {
                database_identity(&destination).expect("published database remains valid");
                complete_legacy_settings_cutover(&source, &destination)
                    .expect("restart completes settings cutover");
            } else {
                assert!(!destination.exists());
                migrate_legacy_config(&source, &destination)
                    .expect("retry succeeds after storage is available");
            }
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn restart_refuses_a_ready_database_for_a_changed_legacy_source() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema8-mismatch-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        migrate_legacy_database(&source, &destination).expect("publish database");
        fs::write(&source, b"{\"schema_version\":7,\"settings\":{}}")
            .expect("replace legacy source");

        assert!(complete_legacy_settings_cutover(&source, &destination).is_err());
        assert_eq!(config_schema_version(&source).unwrap(), 7);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn a_database_from_an_older_schema_is_accepted_and_migrated_on_open() {
        let directory =
            std::env::temp_dir().join(format!("lsb-older-schema-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let destination = directory.join("library.sqlite3");
        initialize_empty_library(&destination, "library").expect("create database");

        let previous = crate::library_store::DATABASE_SCHEMA_VERSION - 1;
        let connection = Connection::open(&destination).expect("open database");
        connection
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                rusqlite::params![previous.to_string()],
            )
            .expect("downgrade schema version");
        connection
            .execute(
                "UPDATE meta SET value = ?1 WHERE key = 'schema_flavor'",
                rusqlite::params![format!("bounded-generation-v{previous}")],
            )
            .expect("downgrade schema flavor");
        connection
            .execute_batch(&format!("PRAGMA user_version = {previous};"))
            .expect("downgrade user version");
        connection
            .execute_batch(
                "ALTER TABLE hotkey_bindings RENAME TO hotkey_bindings_current;
                 -- the renamed table keeps its indexes, and the names are reused
                 DROP INDEX hotkey_bindings_active_lookup;
                 DROP INDEX hotkey_bindings_target_tab;
                 DROP INDEX hotkey_bindings_sound_scope;
                 CREATE TABLE hotkey_bindings(
                     binding_id TEXT PRIMARY KEY,
                     sound_id INTEGER UNIQUE REFERENCES sounds(rowid) ON DELETE CASCADE,
                     control_action TEXT UNIQUE,
                     target_tab TEXT,
                     tab_scope TEXT,
                     accelerator TEXT NOT NULL,
                     normalized TEXT,
                     state TEXT NOT NULL CHECK(state IN ('active', 'needs_attention')),
                     issue TEXT,
                     CHECK((sound_id IS NOT NULL) + (control_action IS NOT NULL)
                           + (target_tab IS NOT NULL) = 1),
                     CHECK((state = 'active' AND normalized IS NOT NULL)
                           OR (state = 'needs_attention' AND normalized IS NULL)),
                     CHECK(target_tab IS NULL OR tab_scope IS NULL)
                 );
                 CREATE INDEX hotkey_bindings_active_lookup
                     ON hotkey_bindings(normalized, tab_scope)
                     WHERE state = 'active';
                 CREATE UNIQUE INDEX hotkey_bindings_target_tab
                     ON hotkey_bindings(target_tab)
                     WHERE target_tab IS NOT NULL;
                 INSERT INTO hotkey_bindings(
                     binding_id, sound_id, control_action, target_tab, tab_scope,
                     accelerator, normalized, state, issue
                 )
                 SELECT binding_id, sound_id, control_action, target_tab, tab_scope,
                        accelerator, normalized, state, issue
                 FROM hotkey_bindings_current;
                 DROP TABLE hotkey_bindings_current;",
            )
            .expect("restore the hotkey table shape the previous schema had");
        drop(connection);

        database_identity(&destination).expect("an older library must still be accepted");

        let store = crate::library_store::LibraryStore::open(destination.clone())
            .expect("open migrates the store");
        drop(store);
        let connection = Connection::open(&destination).expect("reopen database");
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read migrated version");
        assert_eq!(version, crate::library_store::DATABASE_SCHEMA_VERSION);
        drop(connection);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn ready_database_with_wrong_application_id_is_rejected() {
        let directory =
            std::env::temp_dir().join(format!("lsb-wrong-app-id-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let destination = directory.join("library.sqlite3");
        initialize_empty_library(&destination, "library").expect("create database");
        let connection = Connection::open(&destination).expect("open database");
        connection
            .pragma_update(None, "application_id", 7_i64)
            .expect("write wrong application id");
        drop(connection);

        let error = database_identity(&destination).expect_err("wrong file format must fail");

        assert!(error.to_string().contains("application id"));
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn corrupt_and_wrong_format_databases_fail_closed_without_replacement() {
        for (name, bytes) in [
            ("wrong-format", b"this is not sqlite".as_slice()),
            (
                "corrupt",
                b"SQLite format 3\0deliberately truncated".as_slice(),
            ),
        ] {
            let directory =
                std::env::temp_dir().join(format!("lsb-{name}-database-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&directory).expect("create test directory");
            let destination = directory.join("library.sqlite3");
            fs::write(&destination, bytes).expect("write invalid database");
            let before = fs::read(&destination).expect("read invalid database");

            database_identity(&destination).expect_err("invalid database must fail");

            assert_eq!(
                fs::read(&destination).expect("read preserved invalid database"),
                before
            );
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn unsupported_and_non_ready_databases_fail_closed() {
        for (name, mutate) in [
            ("unsupported-schema", "PRAGMA user_version = 99;"),
            (
                "not-ready",
                "DELETE FROM meta WHERE key = 'database_ready';",
            ),
        ] {
            let directory =
                std::env::temp_dir().join(format!("lsb-{name}-database-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&directory).expect("create test directory");
            let destination = directory.join("library.sqlite3");
            initialize_empty_library(&destination, "library").expect("create database");
            let connection = Connection::open(&destination).expect("open database");
            connection.execute_batch(mutate).expect("mutate database");
            drop(connection);
            let before = fs::read(&destination).expect("read invalid database");

            database_identity(&destination).expect_err("invalid database must fail");

            assert_eq!(fs::read(&destination).unwrap(), before);
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn ready_unmarked_database_is_adopted_once() {
        let directory =
            std::env::temp_dir().join(format!("lsb-zero-app-id-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let destination = directory.join("library.sqlite3");
        initialize_empty_library(&destination, "library").expect("create database");
        let connection = Connection::open(&destination).expect("open database");
        connection
            .pragma_update(None, "application_id", 0_i64)
            .expect("clear application id");
        drop(connection);

        database_identity(&destination).expect("adopt valid interim database");

        let connection = Connection::open(&destination).expect("reopen database");
        let application_id: i64 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .expect("read application id");
        assert_eq!(
            application_id,
            crate::library_store::DATABASE_APPLICATION_ID
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn restore_keeps_current_files_and_reinstates_the_legacy_backup() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema8-restore-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        let legacy_bytes = fs::read(&source).unwrap();
        migrate_legacy_config(&source, &destination).expect("complete migration");

        let restored = restore_legacy_backup(&source, &destination).expect("restore backup");

        assert_eq!(fs::read(&source).unwrap(), legacy_bytes);
        assert!(!destination.exists());
        assert!(restored.archived_config.unwrap().is_file());
        assert!(restored.archived_database.unwrap().is_file());
        assert_eq!(config_schema_version(&source).unwrap(), 7);
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
