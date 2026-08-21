use super::*;
use crate::test_support::audio_fixtures::{cleanup_test_audio_path, create_test_audio_file};
use std::fs;

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
fn store_refresh_streams_nested_folders_without_generated_tabs() {
    let root = std::env::temp_dir().join(format!("lsb-store-refresh-{}", uuid::Uuid::new_v4()));
    let cyberpunk = root
        .join("sounds")
        .join("Cyberpunk 2077 Soundtrack Collection by Various Artists");
    fs::create_dir_all(&cyberpunk).expect("create nested music folder");
    fs::write(cyberpunk.join("track.opus"), []).expect("write sound placeholder");

    let library = create_test_library_with(&[root.to_string_lossy().into_owned()], &[]);

    let projection = crate::hotkeys::HotkeyProjectionCoordinator::new(
        library.clone(),
        create_mock_hotkey_manager(),
    );
    let summary = commands::refresh_sounds_with_store(
        create_test_config_state(),
        library.clone(),
        projection,
        &commands::LoudnessCoordinators::new(),
    )
    .expect("store refresh succeeds");

    assert_eq!(summary.added, 1);
    // Generated tabs are no longer materialised anywhere: folders come from the
    // store's folder tree, so a refresh must not create manual tabs.
    assert_eq!(
        library
            .manual_tabs(0)
            .recv()
            .expect("load manual tabs")
            .total,
        0
    );
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

    let library = create_test_library_with(&[root.to_string_lossy().into_owned()], &[]);
    let projection = crate::hotkeys::HotkeyProjectionCoordinator::new(
        library.clone(),
        create_mock_hotkey_manager(),
    );

    let summary = commands::refresh_sounds_with_store(
        create_test_config_state(),
        library.clone(),
        projection,
        &commands::LoudnessCoordinators::new(),
    )
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
    let library = create_test_library_with(&[], &[]);
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
    let library = create_test_library_with(&[], &[]);
    let tab = commands::create_tab_with_store("Imported".to_string(), library.clone())
        .expect("create store-backed tab");

    let imported = commands::import_files_to_tab_with_store(
        vec![
            first.to_string_lossy().into_owned(),
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ],
        Some(tab.id.clone()),
        create_test_config_state(),
        library.clone(),
        &commands::LoudnessCoordinators::new(),
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
fn store_refresh_imports_folders_larger_than_one_scan_batch() {
    // A scan batch is capped at MAX_BATCH_ROWS rows, where a sound costs two
    // rows and a folder costs one per path component. A folder holding more
    // sounds than fit in one batch has to be split across several, and every
    // batch that follows a flush must re-send the folder row.
    let root = std::env::temp_dir().join(format!("lsb-big-folder-{}", uuid::Uuid::new_v4()));
    let disc = root.join("disc");
    fs::create_dir_all(&disc).expect("create scan folder");
    let file_count = 600;
    for index in 0..file_count {
        fs::write(disc.join(format!("track-{index:04}.wav")), []).expect("write placeholder");
    }

    let library = create_test_library_with(&[root.to_string_lossy().into_owned()], &[]);
    let projection = crate::hotkeys::HotkeyProjectionCoordinator::new(
        library.clone(),
        create_mock_hotkey_manager(),
    );

    let summary = commands::refresh_sounds_with_store(
        create_test_config_state(),
        library.clone(),
        projection,
        &commands::LoudnessCoordinators::new(),
    )
    .expect("a folder larger than one batch must still import");

    assert_eq!(summary.added, file_count);
    assert_eq!(
        library
            .count(crate::library_store::LibraryScope::General, "")
            .recv()
            .expect("count imported sounds"),
        file_count
    );

    fs::remove_dir_all(&root).ok();
}
