use super::*;

#[test]
fn test_list_sounds_returns_empty_when_no_sounds() {
    let config = create_test_config_state();
    let sounds = commands::list_sounds(config);
    assert!(sounds.is_empty());
}

#[test]
fn test_list_sounds_with_sounds() {
    let mut config = create_test_config();
    config.sounds.push(Sound::new(
        "Test Sound".to_string(),
        "/tmp/test.mp3".to_string(),
    ));
    let config = Arc::new(Mutex::new(config));

    let sounds = commands::list_sounds(config);
    assert_eq!(sounds.len(), 1);
    assert_eq!(sounds[0].name, "Test Sound");
}

#[test]
fn test_play_sound_not_found() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::play_sound("nonexistent-id".to_string(), config, player);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), commands::CommandError::SoundNotFound);
}

#[test]
fn test_play_sound_disabled() {
    let mut config = create_test_config();
    let mut sound = Sound::new("Test".to_string(), "/tmp/test.mp3".to_string());
    sound.enabled = false;
    config.sounds.push(sound);
    let sound_id = config.sounds[0].id.clone();
    let config = Arc::new(Mutex::new(config));
    let player = create_test_audio_player();

    let result = commands::play_sound(sound_id, config, player);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), commands::CommandError::SoundDisabled);
}

#[test]
fn test_set_allow_multiple_playbacks_is_ignored() {
    let config = create_test_config_state();
    let result = commands::set_allow_multiple_playbacks(true, config.clone());
    assert!(result.is_ok());

    let cfg = config.lock();
    assert!(!cfg.settings.allow_multiple_playbacks);
}

#[test]
fn test_set_skip_delete_confirm() {
    let config = create_test_config_state();
    let result = commands::set_skip_delete_confirm(true, config.clone());
    assert!(result.is_ok());

    let cfg = config.lock();
    assert!(cfg.settings.skip_delete_confirm);
}

#[test]
fn test_add_sound_file_not_exist() {
    let config = create_test_config_state();
    let result = commands::add_sound(
        "Test".to_string(),
        "/nonexistent/path/audio.mp3".to_string(),
        config,
        &commands::LoudnessCoordinators::new(),
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("exist"));
}

#[test]
fn test_add_sound_populates_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let config = create_test_config_state();

    let sound = commands::add_sound(
        "Test".to_string(),
        audio_path.to_string_lossy().to_string(),
        config,
        &commands::LoudnessCoordinators::new(),
    )
    .expect("add sound succeeds");

    assert!(sound.duration_ms.is_some());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_add_sound_folder_valid_path() {
    let config = create_test_config_state();
    let result = commands::add_sound_folder("/tmp".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock();
    assert!(cfg.sound_folders.contains(&"/tmp".to_string()));
}

#[test]
fn test_add_sound_folder_nonexistent() {
    let config = create_test_config_state();
    let result = commands::add_sound_folder("/nonexistent/folder".to_string(), config);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("exist"));
}

#[test]
fn test_remove_sound_folder() {
    let mut config = create_test_config();
    config.sound_folders.push("/tmp".to_string());
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let result = commands::remove_sound_folder("/tmp".to_string(), config.clone(), hotkeys);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert!(!cfg.sound_folders.contains(&"/tmp".to_string()));
}

#[test]
fn test_remove_sound_folder_removes_matching_sounds_and_tab_membership() {
    let removed_folder = "/tmp/Завантажене".to_string();
    let kept_folder = "/tmp/other".to_string();

    let mut config = create_test_config();
    config.sound_folders.push(removed_folder.clone());
    config.sound_folders.push(kept_folder.clone());

    let removed_sound_1 = Sound::new(
        "Removed 1".to_string(),
        format!("{}/one.mp3", removed_folder),
    );
    let removed_sound_2 = Sound::new(
        "Removed 2".to_string(),
        format!("{}/nested/two.wav", removed_folder),
    );
    let kept_sound = Sound::new("Kept".to_string(), format!("{}/keep.ogg", kept_folder));

    let kept_id = kept_sound.id.clone();
    let removed_id_1 = removed_sound_1.id.clone();
    let removed_id_2 = removed_sound_2.id.clone();

    let mut tab = SoundTab::new("Custom".to_string(), 1);
    tab.sound_ids = vec![removed_id_1.clone(), kept_id.clone(), removed_id_2.clone()];

    config.sounds = vec![removed_sound_1, removed_sound_2, kept_sound];
    config.tabs.push(tab);

    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let result = commands::remove_sound_folder(removed_folder.clone(), config.clone(), hotkeys);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert!(!cfg.sound_folders.contains(&removed_folder));
    assert!(cfg.sound_folders.contains(&kept_folder));
    assert_eq!(cfg.sounds.len(), 1);
    assert_eq!(cfg.sounds[0].id, kept_id);
    assert_eq!(cfg.tabs[0].sound_ids, vec![kept_id]);
}

#[test]
fn test_remove_sound_folder_matches_non_ascii_source_path() {
    let removed_folder = "/tmp/Тести".to_string();

    let mut config = create_test_config();
    config.sound_folders.push(removed_folder.clone());

    let mut removed_via_source = Sound::new(
        "Removed via source".to_string(),
        "/tmp/cache/removed.mp3".to_string(),
    );
    removed_via_source.source_path = Some(format!("{}/orig.mp3", removed_folder));

    let mut kept_sound = Sound::new("Kept".to_string(), "/tmp/cache/kept.mp3".to_string());
    kept_sound.source_path = Some("/tmp/another-folder/kept.mp3".to_string());

    let removed_id = removed_via_source.id.clone();
    let kept_id = kept_sound.id.clone();

    config.sounds = vec![removed_via_source, kept_sound];

    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let result = commands::remove_sound_folder(removed_folder, config.clone(), hotkeys);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.sounds.len(), 1);
    assert_eq!(cfg.sounds[0].id, kept_id);
    assert!(cfg.get_sound(&removed_id).is_none());
}

#[test]
fn test_remove_sound_removes_tab_membership() {
    let mut config = create_test_config();
    let sound_a = Sound::new("A".to_string(), "/tmp/a.wav".to_string());
    let sound_b = Sound::new("B".to_string(), "/tmp/b.wav".to_string());
    let removed_id = sound_b.id.clone();
    let kept_id = sound_a.id.clone();

    let mut tab = SoundTab::new("Custom".to_string(), 1);
    tab.sound_ids = vec![kept_id.clone(), removed_id.clone()];

    config.sounds = vec![sound_a, sound_b];
    config.tabs.push(tab);

    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    commands::remove_sound(removed_id.clone(), config.clone(), hotkeys).expect("remove succeeds");

    let cfg = config.lock();
    assert_eq!(cfg.sounds.len(), 1);
    assert_eq!(cfg.sounds[0].id, kept_id);
    assert_eq!(cfg.tabs[0].sound_ids, vec![cfg.sounds[0].id.clone()]);
}

#[test]
fn test_remove_sounds_batch_removes_multiple_sounds_once() {
    let source_path =
        std::env::temp_dir().join(format!("lsb-remove-source-{}.wav", uuid::Uuid::new_v4()));
    fs::write(&source_path, b"source audio remains untouched").expect("create source file");
    let mut config = create_test_config();
    let sound_a = Sound::new("A".to_string(), "/tmp/a.wav".to_string());
    let mut sound_b = Sound::new("B".to_string(), "/tmp/b.wav".to_string());
    sound_b.hotkey = Some("Ctrl+Alt+KeyB".to_string());
    let sound_c = Sound::new("C".to_string(), source_path.to_string_lossy().to_string());

    let kept_id = sound_a.id.clone();
    let remove_b = sound_b.id.clone();
    let remove_c = sound_c.id.clone();

    let mut tab = SoundTab::new("Custom".to_string(), 1);
    tab.sound_ids = vec![kept_id.clone(), remove_b.clone(), remove_c.clone()];

    config.sounds = vec![sound_a, sound_b, sound_c];
    config.tabs.push(tab);

    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    commands::remove_sounds(
        vec![remove_b.clone(), "missing-id".to_string(), remove_c.clone()],
        config.clone(),
        hotkeys,
    )
    .expect("batch remove succeeds");

    let cfg = config.lock();
    assert_eq!(cfg.sounds.len(), 1);
    assert_eq!(cfg.sounds[0].id, kept_id);
    assert_eq!(cfg.tabs[0].sound_ids, vec![cfg.sounds[0].id.clone()]);
    assert!(source_path.exists(), "removal must not delete source files");
    fs::remove_file(source_path).expect("clean up source file");
}

#[test]
fn test_remove_sounds_async_completes_and_updates_config() {
    let _serial = main_context_test_lock();
    let context = glib::MainContext::default();
    let _guard = context.acquire().expect("acquire default main context");
    let mut config = create_test_config();
    let sound = Sound::new("Remove".to_string(), "/tmp/remove.mp3".to_string());
    let sound_id = sound.id.clone();
    config.sounds.push(sound);
    let config = Arc::new(Mutex::new(config));
    let (tx, rx) = std::sync::mpsc::channel();

    commands::remove_sounds_async(
        vec![sound_id],
        Arc::clone(&config),
        create_mock_hotkey_manager(),
        move |result| tx.send(result).expect("send removal result"),
    )
    .expect("dispatch async removal");

    wait_for_async_result(&context, rx).expect("async removal succeeds");
    assert!(config.lock().sounds.is_empty());
}

#[test]
fn test_remove_sound_folder_async_completes_and_updates_config() {
    let _serial = main_context_test_lock();
    let context = glib::MainContext::default();
    let _guard = context.acquire().expect("acquire default main context");
    let mut config = create_test_config();
    config.sound_folders.push("/tmp".to_string());
    let config = Arc::new(Mutex::new(config));
    let (tx, rx) = std::sync::mpsc::channel();

    commands::remove_sound_folder_async(
        "/tmp".to_string(),
        Arc::clone(&config),
        create_mock_hotkey_manager(),
        move |result| tx.send(result).expect("send folder removal result"),
    )
    .expect("dispatch async folder removal");

    wait_for_async_result(&context, rx).expect("async folder removal succeeds");
    assert!(config.lock().sound_folders.is_empty());
}

#[test]
fn test_add_sound_folder_async_completes_and_updates_config() {
    let _serial = main_context_test_lock();
    let context = glib::MainContext::default();
    let _guard = context.acquire().expect("acquire default main context");
    let config = create_test_config_state();
    let (tx, rx) = std::sync::mpsc::channel();

    commands::add_sound_folder_async("/tmp".to_string(), Arc::clone(&config), move |result| {
        tx.send(result).expect("send folder addition result")
    })
    .expect("dispatch async folder addition");

    wait_for_async_result(&context, rx).expect("async folder addition succeeds");
    assert_eq!(config.lock().sound_folders, ["/tmp"]);
}

#[test]
fn test_add_sound_folder_async_can_dispatch_refresh_from_completion() {
    let _serial = main_context_test_lock();
    let context = glib::MainContext::default();
    let _guard = context.acquire().expect("acquire default main context");
    let root = std::env::temp_dir().join(format!("lsb-add-refresh-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create sound folder");
    let folder = root.to_string_lossy().to_string();
    let config = create_test_config_state();
    let config_for_refresh = Arc::clone(&config);
    let (tx, rx) = std::sync::mpsc::channel();

    commands::add_sound_folder_async(folder.clone(), Arc::clone(&config), move |result| {
        result.expect("async folder addition succeeds");
        commands::refresh_sounds_async(
            config_for_refresh,
            create_mock_hotkey_manager(),
            commands::LoudnessCoordinators::new(),
            move |result| tx.send(result).expect("send refresh result"),
        )
        .expect("dispatch refresh from completion");
    })
    .expect("dispatch async folder addition");

    wait_for_async_result(&context, rx).expect("refresh succeeds");
    assert_eq!(config.lock().sound_folders, [folder]);
    fs::remove_dir(root).expect("clean up sound folder");
}

#[test]
fn test_refresh_sounds_empty_folders() {
    let config = create_test_config_state();
    let hotkeys = create_mock_hotkey_manager();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        commands::refresh_sounds(
            config.clone(),
            hotkeys,
            &commands::LoudnessCoordinators::new(),
        )
    }));

    assert!(result.is_ok());
    let summary = result.unwrap().unwrap();
    assert_eq!(summary.added, 0);
    assert_eq!(summary.removed, 0);
}

#[test]
fn test_refresh_sounds_with_folder() {
    let mut config = create_test_config();
    config.sound_folders.push("/tmp".to_string());
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        commands::refresh_sounds(
            config.clone(),
            hotkeys,
            &commands::LoudnessCoordinators::new(),
        )
    }));

    assert!(result.is_ok());
}

#[test]
fn test_refresh_sounds_reconciles_generated_tabs_and_root_removal() {
    let root = std::env::temp_dir().join(format!("lsb-folder-tabs-{}", uuid::Uuid::new_v4()));
    let alerts = root.join("Alerts");
    let memes_nested = root.join("Memes").join("Nested");
    fs::create_dir_all(&alerts).expect("create Alerts folder");
    fs::create_dir_all(&memes_nested).expect("create nested Memes folder");
    let root_file = root.join("root.mp3");
    let alert_file = alerts.join("alert.mp3");
    let meme_file = memes_nested.join("meme.ogg");
    fs::write(&root_file, []).expect("write root file");
    fs::write(&alert_file, []).expect("write alert file");
    fs::write(&meme_file, []).expect("write meme file");

    let mut config = create_test_config();
    config
        .sound_folders
        .push(root.to_string_lossy().to_string());
    let mut existing_alert = Sound::new(
        "Existing Alert".to_string(),
        alert_file.to_string_lossy().to_string(),
    );
    existing_alert.id = "existing-alert".to_string();
    config.sounds.push(existing_alert);
    let mut manual = SoundTab::new("Manual".to_string(), 1);
    manual.id = "manual-tab".to_string();
    manual.sound_ids.push("existing-alert".to_string());
    config.tabs.push(manual);
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();
    let coords = commands::LoudnessCoordinators::new();

    let first = commands::refresh_sounds(config.clone(), hotkeys.clone(), &coords)
        .expect("first refresh succeeds");
    assert_eq!(first.added, 2);
    assert_eq!(first.tabs_created, 3);
    assert_eq!(first.tab_memberships_added, 3);
    {
        let cfg = config.lock();
        assert_eq!(cfg.sounds.len(), 3);
        assert_eq!(cfg.tabs.len(), 4);
        let alerts_tab = cfg.tabs.iter().find(|tab| tab.name == "Alerts").unwrap();
        assert_eq!(alerts_tab.sound_ids, ["existing-alert"]);
        assert_eq!(
            alerts_tab.folder_binding,
            Some(FolderTabBinding {
                root_folder: root.to_string_lossy().to_string(),
                relative_subfolder: "Alerts".to_string(),
            })
        );
        let memes_tab = cfg.tabs.iter().find(|tab| tab.name == "Memes").unwrap();
        let nested_tab = cfg
            .tabs
            .iter()
            .find(|tab| tab.name == "Memes/Nested")
            .unwrap();
        assert_eq!(memes_tab.sound_ids, nested_tab.sound_ids);
        assert_eq!(memes_tab.sound_ids.len(), 1);
        let root_sound = cfg
            .sounds
            .iter()
            .find(|sound| sound.path == root_file.to_string_lossy())
            .unwrap();
        assert!(cfg
            .tabs
            .iter()
            .all(|tab| !tab.sound_ids.contains(&root_sound.id)));
        assert_eq!(
            cfg.get_tab("manual-tab").unwrap().sound_ids,
            ["existing-alert"]
        );
    }

    let second = commands::refresh_sounds(config.clone(), hotkeys.clone(), &coords)
        .expect("repeat refresh succeeds");
    assert_eq!(second.added, 0);
    assert_eq!(second.tabs_created, 0);
    assert_eq!(second.tab_memberships_added, 0);

    let added_alert = alerts.join("second.mp3");
    fs::write(&added_alert, []).expect("write second alert");
    let third = commands::refresh_sounds(config.clone(), hotkeys.clone(), &coords)
        .expect("new-file refresh succeeds");
    assert_eq!(third.added, 1);
    assert_eq!(third.tabs_created, 0);
    assert_eq!(third.tab_memberships_added, 1);

    fs::remove_dir_all(root.join("Memes")).expect("remove Memes folder");
    let fourth = commands::refresh_sounds(config.clone(), hotkeys.clone(), &coords)
        .expect("deleted-subfolder refresh succeeds");
    assert_eq!(fourth.removed, 1);
    assert_eq!(fourth.tabs_removed, 2);

    let unrelated = root
        .parent()
        .unwrap()
        .join(format!("lsb-unrelated-{}.mp3", uuid::Uuid::new_v4()));
    fs::write(&unrelated, []).expect("write unrelated sound");
    {
        let mut cfg = config.lock();
        let mut sound = Sound::new(
            "Unrelated".to_string(),
            unrelated.to_string_lossy().to_string(),
        );
        sound.id = "unrelated".to_string();
        cfg.sounds.push(sound);
        cfg.tabs
            .iter_mut()
            .find(|tab| tab.name == "Alerts")
            .unwrap()
            .sound_ids
            .push("unrelated".to_string());
    }

    commands::remove_sound_folder(root.to_string_lossy().to_string(), config.clone(), hotkeys)
        .expect("remove configured root succeeds");
    {
        let cfg = config.lock();
        assert_eq!(cfg.sounds.len(), 1);
        assert_eq!(cfg.sounds[0].id, "unrelated");
        let retained = cfg.tabs.iter().find(|tab| tab.name == "Alerts").unwrap();
        assert_eq!(retained.sound_ids, ["unrelated"]);
        assert_eq!(retained.folder_binding, None);
        assert!(cfg.get_tab("manual-tab").is_some());
    }

    fs::remove_dir_all(root).expect("cleanup root");
    fs::remove_file(unrelated).expect("cleanup unrelated sound");
}

#[test]
fn store_refresh_streams_nested_folders_without_generated_tabs() {
    let root = std::env::temp_dir().join(format!("lsb-store-refresh-{}", uuid::Uuid::new_v4()));
    let cyberpunk = root
        .join("sounds")
        .join("Cyberpunk 2077 Soundtrack Collection by Various Artists");
    fs::create_dir_all(&cyberpunk).expect("create nested music folder");
    fs::write(cyberpunk.join("track.opus"), []).expect("write sound placeholder");

    let mut config = create_test_config();
    config
        .sound_folders
        .push(root.to_string_lossy().into_owned());
    let config = Arc::new(Mutex::new(config));
    let library = create_test_library(&config);

    let projection = crate::hotkeys::HotkeyProjectionCoordinator::new(
        library.clone(),
        create_mock_hotkey_manager(),
    );
    let summary = commands::refresh_sounds_with_store(library.clone(), projection)
        .expect("store refresh succeeds");

    assert_eq!(summary.added, 1);
    assert!(config.lock().tabs.is_empty());
    let root_path = root.to_string_lossy();
    let top = library
        .folder_children(&root_path, None, 0)
        .recv()
        .expect("load top folders");
    assert_eq!(top.folders[0].relative_path, "sounds");
    let children = library
        .folder_children(&root_path, Some("sounds"), 0)
        .recv()
        .expect("load nested folders");
    assert_eq!(
        children.folders[0].relative_path,
        "sounds/Cyberpunk 2077 Soundtrack Collection by Various Artists"
    );
    assert_eq!(
        library
            .count(
                crate::library_store::LibraryScope::Folder {
                    root_path: root_path.into_owned(),
                    relative_path: "sounds".to_string(),
                },
                "",
            )
            .recv()
            .expect("count aggregate folder"),
        1
    );

    fs::remove_dir_all(root).expect("remove test folder");
}

#[test]
fn store_refresh_metadata_chunks_keep_deterministic_sound_order() {
    let root = std::env::temp_dir().join(format!("lsb-store-chunks-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create music folder");
    for index in 0..40 {
        fs::write(root.join(format!("{index:02}.wav")), []).expect("write sound placeholder");
    }

    let mut config = create_test_config();
    config
        .sound_folders
        .push(root.to_string_lossy().into_owned());
    let config = Arc::new(Mutex::new(config));
    let library = create_test_library(&config);
    let projection = crate::hotkeys::HotkeyProjectionCoordinator::new(
        library.clone(),
        create_mock_hotkey_manager(),
    );

    let summary = commands::refresh_sounds_with_store(library.clone(), projection)
        .expect("store refresh succeeds");
    let page = library
        .page(crate::library_store::LibraryScope::General, "", 0)
        .recv()
        .expect("load sound page");

    assert_eq!(summary.added, 40);
    assert_eq!(
        page.sounds
            .iter()
            .map(|sound| sound.name.as_str())
            .collect::<Vec<_>>(),
        (0..40)
            .map(|index| format!("{index:02}"))
            .collect::<Vec<_>>()
    );

    fs::remove_dir_all(root).expect("remove music folder");
}

#[test]
fn store_backed_rename_and_remove_work_for_scanned_sounds_absent_from_legacy_json() {
    let audio_path = create_test_audio_file("mp3");
    let config = create_test_config_state();
    let library = create_test_library(&config);
    let mut scanned = Sound::new(
        "Scanned".to_string(),
        audio_path.to_string_lossy().into_owned(),
    );
    scanned.id = "scanned".to_string();
    library
        .apply_batch(crate::library_store::LibraryBatch::Sounds(vec![
            crate::library_store::SoundRecord {
                sound: scanned,
                general_position: 0,
                locations: Vec::new(),
            },
        ]))
        .recv()
        .expect("insert scanned sound");

    let renamed = commands::rename_sound_with_store(
        "scanned".to_string(),
        "Renamed".to_string(),
        library.clone(),
    )
    .expect("rename database-only sound");
    assert_eq!(renamed.name, "Renamed");
    assert_eq!(
        library
            .sound_by_id("scanned")
            .recv()
            .expect("lookup renamed sound")
            .expect("renamed sound exists")
            .name,
        "Renamed"
    );

    let projection = crate::hotkeys::HotkeyProjectionCoordinator::new(
        library.clone(),
        create_mock_hotkey_manager(),
    );
    commands::remove_sounds_with_store(vec!["scanned".to_string()], library.clone(), projection)
        .expect("remove database-only sound");
    assert!(library
        .sound_by_id("scanned")
        .recv()
        .expect("lookup removed sound")
        .is_none());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn store_backed_import_is_bounded_and_skips_duplicate_paths() {
    let first = create_test_audio_file("mp3");
    let second = create_test_audio_file("ogg");
    let config = create_test_config_state();
    let library = create_test_library(&config);
    let tab = commands::create_tab_with_store("Imported".to_string(), library.clone())
        .expect("create store-backed tab");

    let imported = commands::import_files_to_tab_with_store(
        vec![
            first.to_string_lossy().into_owned(),
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ],
        Some(tab.id.clone()),
        library.clone(),
    )
    .expect("import files");

    assert_eq!(imported, 2);
    assert_eq!(
        library
            .count(crate::library_store::LibraryScope::General, "")
            .recv()
            .expect("count imported sounds"),
        2
    );
    assert_eq!(
        library
            .count(
                crate::library_store::LibraryScope::ManualTab(tab.id.clone()),
                "",
            )
            .recv()
            .expect("count tab sounds"),
        2
    );
    let sound_ids = library
        .page(
            crate::library_store::LibraryScope::ManualTab(tab.id.clone()),
            "",
            0,
        )
        .recv()
        .expect("load imported tab")
        .sounds
        .into_iter()
        .map(|sound| sound.id)
        .collect();
    commands::remove_sounds_from_tab_with_store(tab.id.clone(), sound_ids, library.clone())
        .expect("remove imported sounds from store-backed tab");
    assert_eq!(
        library
            .count(crate::library_store::LibraryScope::ManualTab(tab.id), "")
            .recv()
            .expect("count emptied tab"),
        0
    );

    cleanup_test_audio_path(&first);
    cleanup_test_audio_path(&second);
}

#[test]
fn test_refresh_sounds_disambiguates_duplicate_subfolder_names() {
    let base = std::env::temp_dir().join(format!("lsb-duplicate-tabs-{}", uuid::Uuid::new_v4()));
    let first_root = base.join("First");
    let second_root = base.join("Second");
    fs::create_dir_all(first_root.join("Alerts")).expect("create first Alerts");
    fs::create_dir_all(second_root.join("Alerts")).expect("create second Alerts");
    fs::write(first_root.join("Alerts/one.mp3"), []).expect("write first sound");
    fs::write(second_root.join("Alerts/two.mp3"), []).expect("write second sound");

    let mut config = create_test_config();
    config.sound_folders = vec![
        second_root.to_string_lossy().to_string(),
        first_root.to_string_lossy().to_string(),
    ];
    let config = Arc::new(Mutex::new(config));

    let summary = commands::refresh_sounds(
        config.clone(),
        create_mock_hotkey_manager(),
        &commands::LoudnessCoordinators::new(),
    )
    .expect("duplicate-name refresh succeeds");

    assert_eq!(summary.tabs_created, 2);
    let cfg = config.lock();
    assert_eq!(
        cfg.tabs
            .iter()
            .map(|tab| tab.name.as_str())
            .collect::<Vec<_>>(),
        ["Alerts (First)", "Alerts (Second)"]
    );
    assert_ne!(cfg.tabs[0].folder_binding, cfg.tabs[1].folder_binding);
    drop(cfg);
    fs::remove_dir_all(base).expect("cleanup duplicate roots");
}

#[test]
fn test_refresh_sounds_uses_one_tab_owner_for_overlapping_roots() {
    let root = std::env::temp_dir().join(format!("lsb-overlap-tabs-{}", uuid::Uuid::new_v4()));
    let alerts = root.join("Alerts");
    let nested = alerts.join("Nested");
    fs::create_dir_all(&nested).expect("create test tree");
    fs::write(nested.join("alert.mp3"), []).expect("write sound");

    let mut config = create_test_config();
    config.sound_folders = vec![
        root.to_string_lossy().to_string(),
        alerts.to_string_lossy().to_string(),
    ];
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();
    let coords = commands::LoudnessCoordinators::new();

    let first = commands::refresh_sounds(config.clone(), hotkeys.clone(), &coords)
        .expect("refresh parent-owned roots");
    assert_eq!(first.added, 1);
    assert_eq!(first.tabs_created, 2);
    {
        let cfg = config.lock();
        assert_eq!(cfg.tabs.len(), 2);
        assert_eq!(cfg.tabs[0].name, "Alerts");
        assert_eq!(cfg.tabs[1].name, "Alerts/Nested");
        assert_eq!(cfg.tabs[0].sound_ids, cfg.tabs[1].sound_ids);
        assert_eq!(cfg.tabs[0].sound_ids.len(), 1);
    }

    commands::remove_sound_folder(root.to_string_lossy().to_string(), config.clone(), hotkeys)
        .expect("remove owning root");
    let second = commands::refresh_sounds(config.clone(), create_mock_hotkey_manager(), &coords)
        .expect("refresh child root after handover");
    assert_eq!(second.added, 1);
    assert_eq!(second.tabs_created, 1);
    {
        let cfg = config.lock();
        assert_eq!(cfg.tabs.len(), 1);
        assert_eq!(cfg.tabs[0].name, "Nested");
        assert_eq!(cfg.tabs[0].sound_ids.len(), 1);
    }

    fs::remove_dir_all(root).expect("cleanup test tree");
}

#[cfg(unix)]
#[test]
fn test_refresh_sounds_deduplicates_symbolic_link_root_tabs() {
    use std::os::unix::fs::symlink;

    let base = std::env::temp_dir().join(format!("lsb-alias-tabs-{}", uuid::Uuid::new_v4()));
    let root = base.join("actual");
    let alias = base.join("alias");
    let alerts = root.join("Alerts");
    fs::create_dir_all(&alerts).expect("create test tree");
    fs::write(alerts.join("alert.mp3"), []).expect("write sound");
    symlink(&root, &alias).expect("create root alias");

    let mut config = create_test_config();
    config.sound_folders = vec![
        root.to_string_lossy().to_string(),
        alias.to_string_lossy().to_string(),
    ];
    let config = Arc::new(Mutex::new(config));

    let summary = commands::refresh_sounds(
        config.clone(),
        create_mock_hotkey_manager(),
        &commands::LoudnessCoordinators::new(),
    )
    .expect("refresh alias roots");

    assert_eq!(summary.added, 1);
    assert_eq!(summary.tabs_created, 1);
    let cfg = config.lock();
    assert_eq!(cfg.sounds.len(), 1);
    assert_eq!(cfg.tabs.len(), 1);
    assert_eq!(cfg.tabs[0].sound_ids.len(), 1);
    drop(cfg);

    fs::remove_file(alias).expect("remove root alias");
    fs::remove_dir_all(base).expect("cleanup test tree");
}

#[test]
fn test_refresh_sounds_populates_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.sound_folders.push(
        audio_path
            .parent()
            .expect("audio temp dir")
            .to_string_lossy()
            .to_string(),
    );
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let summary = commands::refresh_sounds(
        Arc::clone(&config),
        hotkeys,
        &commands::LoudnessCoordinators::new(),
    )
    .expect("refresh succeeds");

    assert_eq!(summary.added, 1);
    let cfg = config.lock();
    assert_eq!(cfg.sounds.len(), 1);
    assert!(cfg.sounds[0].duration_ms.is_some());
    assert!(cfg.sounds[0].loudness_source_fingerprint.is_some());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_import_dropped_files_populates_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let target_root =
        std::env::temp_dir().join(format!("lsb-import-target-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&target_root).expect("create target dir");

    let mut config = create_test_config();
    config
        .sound_folders
        .push(target_root.to_string_lossy().to_string());
    let config = Arc::new(Mutex::new(config));

    let imported = commands::import_dropped_files(
        vec![audio_path.to_string_lossy().to_string()],
        config,
        &commands::LoudnessCoordinators::new(),
    )
    .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert!(imported[0].duration_ms.is_some());
    assert!(imported[0].loudness_source_fingerprint.is_some());

    cleanup_test_audio_path(&audio_path);
    let _ = fs::remove_dir_all(&target_root);
}

#[test]
fn test_import_dropped_files_deduplicates_one_batch() {
    let audio_path = create_test_audio_file("mp3");
    let path = audio_path.to_string_lossy().to_string();
    let target_root =
        std::env::temp_dir().join(format!("lsb-import-target-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&target_root).expect("create target dir");

    let mut config = create_test_config();
    config
        .sound_folders
        .push(target_root.to_string_lossy().to_string());
    let config = Arc::new(Mutex::new(config));

    let imported = commands::import_dropped_files(
        vec![path.clone(), path],
        Arc::clone(&config),
        &commands::LoudnessCoordinators::new(),
    )
    .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert_eq!(config.lock().sounds.len(), 1);

    cleanup_test_audio_path(&audio_path);
    let _ = fs::remove_dir_all(&target_root);
}

#[test]
fn test_import_files_to_tab_populates_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let config = create_test_config_state();

    let imported = commands::import_files_to_tab(
        vec![audio_path.to_string_lossy().to_string()],
        None,
        config,
        &commands::LoudnessCoordinators::new(),
    )
    .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert!(imported[0].duration_ms.is_some());
    assert!(imported[0].loudness_source_fingerprint.is_some());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_import_files_to_tab_deduplicates_one_batch() {
    let audio_path = create_test_audio_file("mp3");
    let path = audio_path.to_string_lossy().to_string();
    let config = create_test_config_state();

    let imported = commands::import_files_to_tab(
        vec![path.clone(), path],
        None,
        Arc::clone(&config),
        &commands::LoudnessCoordinators::new(),
    )
    .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert_eq!(config.lock().sounds.len(), 1);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_import_files_to_tab_async_completes_and_updates_config() {
    let _serial = main_context_test_lock();
    let context = glib::MainContext::default();
    let _guard = context.acquire().expect("acquire default main context");
    let audio_path = create_test_audio_file("mp3");
    let config = create_test_config_state();
    let (tx, rx) = std::sync::mpsc::channel();

    commands::import_files_to_tab_async(
        vec![audio_path.to_string_lossy().to_string()],
        None,
        Arc::clone(&config),
        commands::LoudnessCoordinators::new(),
        move |result| {
            tx.send(result.map(|sounds| sounds.len()))
                .expect("send async import result");
        },
    )
    .expect("dispatch async import");

    let imported_count = wait_for_async_result(&context, rx).expect("async import succeeds");

    assert_eq!(imported_count, 1);
    assert_eq!(config.lock().sounds.len(), 1);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_update_sound_source_refreshes_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.sounds.push(Sound::new(
        "Test".to_string(),
        "/tmp/original.mp3".to_string(),
    ));
    let sound_id = config.sounds[0].id.clone();
    let config = Arc::new(Mutex::new(config));

    let updated = commands::update_sound_source(
        sound_id,
        audio_path.to_string_lossy().to_string(),
        config,
        &commands::LoudnessCoordinators::new(),
    )
    .expect("update succeeds");

    assert!(updated.duration_ms.is_some());
    assert!(updated.loudness_source_fingerprint.is_some());

    cleanup_test_audio_path(&audio_path);
}

fn assert_audio_import_paths(audio_path: &std::path::Path) {
    let path = audio_path.to_string_lossy().to_string();
    let coords = commands::LoudnessCoordinators::new();

    let direct_config = create_test_config_state();
    let added = commands::add_sound(
        "Direct".to_string(),
        path.clone(),
        Arc::clone(&direct_config),
        &coords,
    )
    .expect("direct add succeeds");
    assert!(added.duration_ms.is_some());
    assert!(commands::add_sound(
        "Duplicate".to_string(),
        path.clone(),
        direct_config,
        &coords,
    )
    .is_err());

    let mut folder_config = create_test_config();
    folder_config.sound_folders.push(
        audio_path
            .parent()
            .expect("audio fixture parent")
            .to_string_lossy()
            .to_string(),
    );
    let folder_config = Arc::new(Mutex::new(folder_config));
    let first_refresh = commands::refresh_sounds(
        Arc::clone(&folder_config),
        create_mock_hotkey_manager(),
        &coords,
    )
    .expect("folder refresh succeeds");
    let second_refresh = commands::refresh_sounds(
        Arc::clone(&folder_config),
        create_mock_hotkey_manager(),
        &coords,
    )
    .expect("later folder refresh succeeds");
    assert_eq!(first_refresh.added, 1);
    assert_eq!(second_refresh.added, 0);
    assert_eq!(folder_config.lock().sounds.len(), 1);

    let copy_target =
        std::env::temp_dir().join(format!("lsb-opus-copy-target-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&copy_target).expect("create copy target");
    let mut copy_config = create_test_config();
    copy_config
        .sound_folders
        .push(copy_target.to_string_lossy().to_string());
    let copied = commands::import_dropped_files(
        vec![path.clone()],
        Arc::new(Mutex::new(copy_config)),
        &coords,
    )
    .expect("copy import succeeds");
    assert_eq!(copied.len(), 1);
    assert!(copied[0].duration_ms.is_some());
    fs::remove_dir_all(copy_target).expect("cleanup copy target");

    let linked =
        commands::import_files_as_links(vec![path.clone()], create_test_config_state(), &coords)
            .expect("link import succeeds");
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].path, path);

    let mut tab_config = create_test_config();
    let tab = SoundTab::new("Opus".to_string(), 1);
    let tab_id = tab.id.clone();
    tab_config.tabs.push(tab);
    let tab_config = Arc::new(Mutex::new(tab_config));
    let tab_imported = commands::import_files_to_tab(
        vec![path.clone()],
        Some(tab_id.clone()),
        Arc::clone(&tab_config),
        &coords,
    )
    .expect("tab import succeeds");
    assert_eq!(tab_imported.len(), 1);
    assert!(tab_config
        .lock()
        .get_tab(&tab_id)
        .expect("tab exists")
        .sound_ids
        .contains(&tab_imported[0].id));

    let mut replacement_config = create_test_config();
    let missing = Sound::new(
        "Missing".to_string(),
        format!("/tmp/lsb-missing-{}.mp3", uuid::Uuid::new_v4()),
    );
    let missing_id = missing.id.clone();
    replacement_config.sounds.push(missing);
    let replaced = commands::update_sound_source(
        missing_id,
        path.clone(),
        Arc::new(Mutex::new(replacement_config)),
        &coords,
    )
    .expect("source replacement succeeds");
    assert_eq!(replaced.path, path);
    assert!(replaced.duration_ms.is_some());
}

#[test]
fn opus_and_vorbis_use_every_import_path() {
    let fixtures = [
        create_test_ogg_opus_file(TestOggOpusFixture {
            extension: "opus",
            ..Default::default()
        }),
        create_test_ogg_opus_file(TestOggOpusFixture {
            extension: "OPUS",
            ..Default::default()
        }),
        create_test_ogg_opus_file(TestOggOpusFixture::default()),
        create_test_vorbis_file(TestVorbisFixture::Mono44100),
    ];

    for audio_path in fixtures {
        assert_audio_import_paths(&audio_path);
        cleanup_test_audio_path(&audio_path);
    }
}
