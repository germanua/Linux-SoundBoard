use std::path::{Path, PathBuf};

use crate::config::{FolderTabBinding, LoudnessAnalysisState, Sound, SoundTab};
use crate::library_store::{LibraryScope, LibrarySnapshot, LibraryStore, PAGE_SIZE};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("lsb-library-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create test directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn sound(id: &str, name: &str, path: &str) -> Sound {
    let mut sound = Sound::new(name.to_string(), path.to_string());
    sound.id = id.to_string();
    sound.source_path = Some(format!("{path}.source"));
    sound.hotkey = Some(format!("Ctrl+{id}"));
    sound.duration_ms = Some(123_456);
    sound.volume = 73;
    sound.enabled = false;
    sound.loudness_lufs = Some(-13.5);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(0.75);
    sound.loudness_source_fingerprint = Some(format!("fingerprint-{id}"));
    sound.loudness_true_peak_dbtp = Some(-1.25);
    sound
}

#[test]
fn sqlite_store_round_trips_and_pages_library_data() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    let first = sound("first", "Alpha", "/music/album/alpha.flac");
    let second = sound("second", "beta", "/music/album/beta.flac");
    let mut manual_tab = SoundTab::new("Favourites".to_string(), 4);
    manual_tab.id = "favourites".to_string();
    manual_tab.sound_ids = vec![second.id.clone(), first.id.clone()];
    let mut generated_tab = SoundTab::new("Album".to_string(), 5);
    generated_tab.id = "generated".to_string();
    generated_tab.folder_binding = Some(FolderTabBinding {
        root_folder: "/music".to_string(),
        relative_subfolder: "album".to_string(),
    });

    store
        .replace_all(LibrarySnapshot {
            sound_folders: vec!["/music".to_string()],
            sounds: vec![second.clone(), first.clone()],
            tabs: vec![manual_tab, generated_tab],
        })
        .recv()
        .expect("replace library");

    assert_eq!(
        store
            .count(LibraryScope::General, "")
            .recv()
            .expect("count all sounds"),
        2
    );
    let page = store
        .page(LibraryScope::General, "ALP", 0)
        .recv()
        .expect("query filtered page");
    assert_eq!(page.total, 1);
    assert_eq!(page.sounds.len(), 1);
    assert_eq!(page.sounds[0].id, first.id);
    assert_eq!(page.sounds[0].source_path, first.source_path);
    assert_eq!(page.sounds[0].hotkey, first.hotkey);
    assert_eq!(page.sounds[0].duration_ms, first.duration_ms);
    assert_eq!(page.sounds[0].volume, first.volume);
    assert_eq!(page.sounds[0].enabled, first.enabled);
    assert_eq!(page.sounds[0].loudness_lufs, first.loudness_lufs);
    assert_eq!(
        page.sounds[0].loudness_analysis_state,
        first.loudness_analysis_state
    );
    assert_eq!(
        page.sounds[0].loudness_confidence,
        first.loudness_confidence
    );
    assert_eq!(
        page.sounds[0].loudness_source_fingerprint,
        first.loudness_source_fingerprint
    );
    assert_eq!(
        page.sounds[0].loudness_true_peak_dbtp,
        first.loudness_true_peak_dbtp
    );

    let favourites = store
        .page(LibraryScope::Tab("favourites".to_string()), "", 0)
        .recv()
        .expect("query tab page");
    assert_eq!(favourites.total, 2);
    assert_eq!(
        favourites
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );
    assert_eq!(PAGE_SIZE, 256);

    let loaded = store
        .sound_by_id("second")
        .recv()
        .expect("look up sound")
        .expect("sound exists");
    assert_eq!(loaded.name, second.name);
    assert_eq!(loaded.path, second.path);
}

#[test]
fn failed_snapshot_replacement_rolls_back_the_whole_library() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    store
        .replace_all(LibrarySnapshot {
            sound_folders: vec!["/music".to_string()],
            sounds: vec![sound("existing", "Existing", "/music/existing.flac")],
            tabs: Vec::new(),
        })
        .recv()
        .expect("seed library");

    let duplicate_path = "/music/duplicate.flac";
    let error = store
        .replace_all(LibrarySnapshot {
            sound_folders: vec!["/other".to_string()],
            sounds: vec![
                sound("duplicate-a", "Duplicate A", duplicate_path),
                sound("duplicate-b", "Duplicate B", duplicate_path),
            ],
            tabs: Vec::new(),
        })
        .recv()
        .expect_err("duplicate paths must reject the replacement");
    assert!(error.to_string().contains("UNIQUE constraint failed"));

    let remaining = store
        .page(LibraryScope::General, "", 0)
        .recv()
        .expect("load library after rollback");
    assert_eq!(remaining.total, 1);
    assert_eq!(remaining.sounds[0].id, "existing");
}

#[test]
fn opening_a_newer_database_schema_is_read_only_and_fails() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    let connection = rusqlite::Connection::open(&path).expect("create future database");
    connection
        .execute_batch("CREATE TABLE future_data(value TEXT); PRAGMA user_version = 99;")
        .expect("seed future schema");
    drop(connection);

    let error = match LibraryStore::open(path.clone()) {
        Ok(_) => panic!("future schema must not open"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("newer than supported"));

    let connection = rusqlite::Connection::open(path).expect("reopen future database");
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read schema version");
    let marker: i64 = connection
        .query_row("SELECT COUNT(*) FROM future_data", [], |row| row.get(0))
        .expect("read future table");
    assert_eq!(version, 99);
    assert_eq!(marker, 0);
}
