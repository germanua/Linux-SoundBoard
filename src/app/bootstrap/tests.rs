#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Sound;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("lsb-bootstrap-{label}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn config_path(&self) -> PathBuf {
            self.0.join("config.json")
        }

        fn library_path(&self) -> PathBuf {
            self.0.join("library.sqlite3")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prepared_application_can_cross_the_startup_worker_boundary() {
        fn assert_send<T: Send>() {}

        assert_send::<PreparedApplication>();
    }

    #[test]
    fn migration_progress_disables_cancellation_before_publication() {
        use crate::legacy_migration::LegacyMigrationProgress;

        assert!(migration_progress_can_cancel(
            LegacyMigrationProgress::Importing
        ));
        assert!(!migration_progress_can_cancel(
            LegacyMigrationProgress::PublishingDatabase
        ));
        assert_eq!(
            migration_progress_message(LegacyMigrationProgress::Verifying),
            "Verifying the upgraded library…"
        );
    }

    fn write_legacy_config(path: &Path) -> Vec<u8> {
        let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        config.sound_folders.push("/music".to_string());
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        let bytes = serde_json::to_vec(&config).expect("serialize legacy config");
        fs::write(path, &bytes).expect("write legacy config");
        bytes
    }

    #[test]
    fn startup_preflight_creates_one_matching_empty_library() {
        let temp = TestDir::new("new-storage");

        assert_eq!(
            prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
                .expect("prepare new storage"),
            StoragePreparation::Ready
        );

        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.config_path()).expect("read settings"))
                .expect("parse settings");
        let identity = crate::legacy_migration::database_identity(&temp.library_path())
            .expect("load database identity");
        assert!(persisted.get("library_id").is_none());
        assert!(!identity.library_id.is_empty());
    }

    #[test]
    fn startup_preflight_leaves_unconfirmed_legacy_config_untouched() {
        let temp = TestDir::new("legacy-prompt");
        let original = write_legacy_config(&temp.config_path());

        assert_eq!(
            prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
                .expect("inspect legacy storage"),
            StoragePreparation::NeedsLegacyMigration
        );
        assert_eq!(fs::read(temp.config_path()).unwrap(), original);
        assert!(!temp.library_path().exists());
    }

    #[test]
    fn startup_preflight_resumes_after_database_publication() {
        let temp = TestDir::new("resume-cutover");
        write_legacy_config(&temp.config_path());
        let report = crate::legacy_migration::migrate_legacy_database(
            &temp.config_path(),
            &temp.library_path(),
        )
        .expect("publish database");

        assert_eq!(
            prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
                .expect("resume cutover"),
            StoragePreparation::Ready
        );
        let config = Config::load_from_path(&temp.config_path()).expect("load schema-8 settings");
        assert_eq!(config.schema_version, crate::config::CURRENT_SCHEMA_VERSION);
        assert!(!report.library_id.is_empty());
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.config_path()).expect("read settings"))
                .expect("parse settings");
        assert!(persisted.get("library_id").is_none());
    }

    #[test]
    fn startup_preflight_offers_explicit_recovery_for_schema_8_without_its_database() {
        let temp = TestDir::new("missing-database");
        let mut config = Config::default();
        config
            .save_to_path(&temp.config_path())
            .expect("write schema-8 settings");

        let preparation = prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
            .expect("missing database should offer an explicit recovery choice");

        assert!(matches!(
            preparation,
            StoragePreparation::NeedsEmptyLibraryRecovery { ref library_id }
                if !library_id.is_empty()
        ));
        assert!(!temp.library_path().exists());
    }

    #[test]
    fn startup_preflight_uses_ready_database_when_obsolete_json_identity_differs() {
        let temp = TestDir::new("mismatched-database");
        let mut config = Config::default();
        config
            .save_to_path(&temp.config_path())
            .expect("write schema-8 settings");
        let mut persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(temp.config_path()).expect("read settings"))
                .expect("parse settings");
        persisted["library_id"] = serde_json::Value::String("obsolete-library".to_string());
        std::fs::write(
            temp.config_path(),
            serde_json::to_vec_pretty(&persisted).expect("serialize settings"),
        )
        .expect("write obsolete identity");
        crate::legacy_migration::initialize_empty_library(
            &temp.library_path(),
            "different-library",
        )
        .expect("create mismatched database");

        let preparation = prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
            .expect("ready canonical database must remain authoritative");

        assert!(matches!(preparation, StoragePreparation::Ready));
    }

    #[test]
    fn storage_lock_excludes_a_second_writer_until_drop() {
        let temp = TestDir::new("storage-lock");
        let lock_path = temp.0.join("storage.lock");

        let first = acquire_storage_lock_at(&lock_path).expect("acquire first lock");
        assert!(acquire_storage_lock_at(&lock_path).is_err());
        drop(first);
        assert!(acquire_storage_lock_at(&lock_path).is_ok());
    }

    #[test]
    fn x11_and_vmware_choose_the_bounded_fallback_renderer() {
        assert!(should_use_fallback_renderer(Some(X11_BACKEND), false));
        assert!(should_use_fallback_renderer(Some(WAYLAND_BACKEND), true));
        assert!(!should_use_fallback_renderer(Some(WAYLAND_BACKEND), false));
    }

    #[test]
    fn audio_engine_service_renders_quoted_exec() {
        let service = render_audio_engine_service(Path::new("/tmp/Linux Soundboard.AppImage"));
        assert!(service.contains("ExecStart=\"/tmp/Linux Soundboard.AppImage\" --audio-engine"));
        assert!(service.contains("StartLimitBurst=5"));
        assert!(service.contains(
            "After=pipewire.service pipewire-pulse.service wireplumber.service pulseaudio.service"
        ));
        assert!(service.contains("X-LinuxSoundBoard-Managed=true"));
        assert!(service.contains(&format!("PartOf={ENGINE_TARGET_UNIT}")));
        assert!(service.contains("RefuseManualStop=yes"));
        assert!(!service.contains("WantedBy=default.target"));
        assert!(service.contains("RestartPreventExitStatus=2"));
    }

    #[test]
    fn appimage_engine_service_drops_no_new_privileges() {
        let temp = TestDir::new("engine-unit");
        let native = temp.0.join("linux-soundboard");
        let appimage = temp.0.join("soundboard.AppImage");
        let mut header = [0u8; 16];
        header[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        fs::write(&native, header).expect("write native binary");
        header[8..11].copy_from_slice(b"AI\x02");
        fs::write(&appimage, header).expect("write appimage");

        let native_service = render_audio_engine_service(&native);
        assert!(native_service.contains("NoNewPrivileges=yes"));
        assert!(native_service.contains("RestrictSUIDSGID=yes"));
        assert!(native_service.contains("LockPersonality=yes"));

        let service = render_audio_engine_service(&appimage);
        assert!(!service.contains("NoNewPrivileges"));
        assert!(!service.contains("RestrictSUIDSGID"));
        assert!(!service.contains("LockPersonality"));
        assert!(service.contains("--audio-engine"));
    }

    #[test]
    fn packaged_engine_target_owns_the_protected_service() {
        let target = include_str!("../../../packaging/linux/linux-soundboard-engine.target");
        if crate::app_meta::BUILD_PROFILE == "stable" {
            assert!(target.contains("Wants=linux-soundboard-engine.service"));
        }
        assert!(target.contains("WantedBy=default.target"));
    }

    #[test]
    fn installation_kind_uses_only_system_and_stable_user_paths() {
        let home = Path::new("/home/test");
        assert_eq!(
            installation_kind_for(Path::new("/usr/bin/linux-soundboard"), home, false),
            if crate::app_meta::BUILD_PROFILE == "stable" {
                InstallationKind::Stable
            } else {
                InstallationKind::PortableOrDevelopment
            }
        );
        let managed = home.join(".local/opt").join(APP_BINARY).join(APP_BINARY);
        assert_eq!(
            installation_kind_for(&managed, home, true),
            InstallationKind::Stable
        );
        assert_eq!(
            installation_kind_for(
                Path::new("/home/test/Downloads/Soundboard.AppImage"),
                home,
                true
            ),
            InstallationKind::DirectAppImage
        );
        assert_eq!(
            installation_kind_for(Path::new("/tmp/target/debug/linux-soundboard"), home, false),
            InstallationKind::PortableOrDevelopment
        );
    }

    #[test]
    fn direct_appimage_auto_updates_an_existing_user_install() {
        assert_eq!(
            appimage_startup_action(
                InstallationKind::DirectAppImage,
                false,
                true,
                Some("2.1.0"),
                "2.1.1",
            ),
            AppImageStartupAction::AutoUpdate
        );
        assert_eq!(
            appimage_startup_action(InstallationKind::DirectAppImage, false, true, None, "2.1.1",),
            AppImageStartupAction::AutoUpdate
        );
    }

    #[test]
    fn direct_appimage_only_prompts_for_a_first_user_install() {
        assert_eq!(
            appimage_startup_action(
                InstallationKind::DirectAppImage,
                false,
                false,
                None,
                "2.1.1",
            ),
            AppImageStartupAction::Prompt
        );
        assert_eq!(
            appimage_startup_action(InstallationKind::DirectAppImage, true, false, None, "2.1.1",),
            AppImageStartupAction::StartPersistent
        );
    }

    #[test]
    fn direct_appimage_does_not_downgrade_a_newer_user_install() {
        assert_eq!(
            appimage_startup_action(
                InstallationKind::DirectAppImage,
                false,
                true,
                Some("v2.2.0"),
                "2.1.1",
            ),
            AppImageStartupAction::LaunchInstalled
        );
        assert_eq!(
            appimage_startup_action(
                InstallationKind::DirectAppImage,
                false,
                true,
                Some("2.1.1"),
                "2.1.1",
            ),
            AppImageStartupAction::StartPersistent
        );
    }

    #[test]
    fn release_version_comparison_is_numeric() {
        assert_eq!(
            compare_release_versions("2.10.0", "2.9.9"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            compare_release_versions("3.0.0", "3.0.0"),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(
            compare_release_versions("v3.0.0", "3.0.0"),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(
            compare_release_versions("2.4.4", "v2.4.5"),
            Some(std::cmp::Ordering::Less)
        );
        assert_eq!(
            compare_release_versions("2.4.6", "v2.4.5"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(compare_release_versions("not-a-version", "3.0.0"), None);
    }

    #[test]
    fn matching_engine_connects_without_service_changes() {
        let events = RefCell::new(Vec::new());

        let engine = connect_or_start_audio_engine(
            || {
                events.borrow_mut().push("connect");
                Some(7)
            },
            || panic!("matching engine must not be reprobed"),
            || panic!("matching engine must not be stopped"),
            |_| panic!("matching engine must not change the service"),
            || panic!("matching engine must not run local cleanup"),
            1,
            || {},
        );

        assert_eq!(engine, Some(7));
        assert_eq!(*events.borrow(), ["connect"]);
    }

    #[test]
    fn absent_engine_starts_service_and_connects() {
        let attempts = Cell::new(0);
        let events = RefCell::new(Vec::new());

        let engine = connect_or_start_audio_engine(
            || {
                events.borrow_mut().push("connect");
                attempts.set(attempts.get() + 1);
                (attempts.get() == 2).then_some(7)
            },
            || {
                events.borrow_mut().push("engine-running");
                false
            },
            || panic!("absent engine must not be stopped"),
            |action| {
                events.borrow_mut().push(match action {
                    ServiceAction::Start => "start-service",
                    ServiceAction::Restart => "restart-service",
                });
                true
            },
            || panic!("compatible service must not run local cleanup"),
            1,
            || {},
        );

        assert_eq!(engine, Some(7));
        assert_eq!(
            *events.borrow(),
            ["connect", "engine-running", "start-service", "connect"]
        );
    }

    #[test]
    fn incompatible_engine_is_stopped_restarted_and_reconnected() {
        let attempts = Cell::new(0);
        let events = RefCell::new(Vec::new());

        let engine = connect_or_start_audio_engine(
            || {
                events.borrow_mut().push("connect");
                attempts.set(attempts.get() + 1);
                (attempts.get() == 2).then_some(7)
            },
            || {
                events.borrow_mut().push("engine-running");
                true
            },
            || {
                events.borrow_mut().push("stop-incompatible");
                true
            },
            |action| {
                assert_eq!(action, ServiceAction::Restart);
                events.borrow_mut().push("restart-service");
                true
            },
            || panic!("compatible restarted service must not fall back locally"),
            1,
            || {},
        );

        assert_eq!(engine, Some(7));
        assert_eq!(
            *events.borrow(),
            [
                "connect",
                "engine-running",
                "stop-incompatible",
                "restart-service",
                "connect"
            ]
        );
    }

    #[test]
    fn restarted_incompatible_engine_is_stopped_once_before_local_fallback() {
        let events = RefCell::new(Vec::new());
        let cleanup_count = Cell::new(0);

        let engine = connect_or_start_audio_engine(
            || {
                events.borrow_mut().push("connect");
                None::<u8>
            },
            || {
                events.borrow_mut().push("engine-running");
                true
            },
            || {
                events.borrow_mut().push("stop-incompatible");
                true
            },
            |action| {
                assert_eq!(action, ServiceAction::Restart);
                events.borrow_mut().push("restart-service");
                true
            },
            || {
                cleanup_count.set(cleanup_count.get() + 1);
                events.borrow_mut().push("cleanup-before-local");
            },
            1,
            || {},
        );

        assert_eq!(engine, None);
        assert_eq!(cleanup_count.get(), 1);
        assert_eq!(
            *events.borrow(),
            [
                "connect",
                "engine-running",
                "stop-incompatible",
                "restart-service",
                "connect",
                "cleanup-before-local"
            ]
        );
    }

    #[test]
    fn unavailable_systemd_falls_back_to_one_local_owner() {
        let events = RefCell::new(Vec::new());
        let cleanup_count = Cell::new(0);

        let engine = connect_or_start_audio_engine(
            || None::<u8>,
            || false,
            || panic!("absent engine must not be stopped"),
            |action| {
                assert_eq!(action, ServiceAction::Start);
                events.borrow_mut().push("systemd-unavailable");
                false
            },
            || cleanup_count.set(cleanup_count.get() + 1),
            1,
            || {},
        );

        assert_eq!(engine, None);
        assert_eq!(*events.borrow(), ["systemd-unavailable"]);
        assert_eq!(cleanup_count.get(), 1);
    }

    #[test]
    fn successful_stale_engine_restart_reports_update() {
        assert_eq!(
            engine_update_notice(Some("2.0.0".to_string()), true),
            Some(EngineUpdateNotice::Updated {
                previous_version: "2.0.0".to_string(),
            })
        );
    }

    #[test]
    fn failed_stale_engine_restart_reports_temporary_fallback() {
        assert_eq!(
            engine_update_notice(Some("2.0.0".to_string()), false),
            Some(EngineUpdateNotice::Failed {
                previous_version: "2.0.0".to_string(),
            })
        );
    }

    #[test]
    fn matching_or_absent_engine_reports_no_update() {
        assert_eq!(engine_update_notice(None, true), None);
        assert_eq!(engine_update_notice(None, false), None);
    }
}
