use std::path::{Path, PathBuf};

use crate::config::{LoudnessAnalysisState, Sound};
use crate::library_store::{
    FolderOverrideAction, FolderOverrideRecord, FolderRecord, LibraryBatch, LibraryScope,
    LibraryStore, ManualMembershipRecord, ManualTabRecord, RootRecord, SoundLocationRecord,
    SoundRecord, MAX_BATCH_ROWS, PAGE_SIZE,
};

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

fn wait<T>(response: crate::library_store::LibraryResponse<T>) -> T {
    response.recv().expect("library request")
}

#[test]
fn bounded_store_round_trips_manual_and_recursive_folder_pages() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
        path: "/music".to_string(),
        position: 0,
    }])));
    wait(store.apply_batch(LibraryBatch::Folders(vec![
        FolderRecord {
            root_path: "/music".to_string(),
            relative_path: "album".to_string(),
            parent_relative_path: None,
            name: "Album".to_string(),
            position: 0,
        },
        FolderRecord {
            root_path: "/music".to_string(),
            relative_path: "album/disc-1".to_string(),
            parent_relative_path: Some("album".to_string()),
            name: "Disc 1".to_string(),
            position: 0,
        },
        FolderRecord {
            root_path: "/music".to_string(),
            relative_path: "other".to_string(),
            parent_relative_path: None,
            name: "Other".to_string(),
            position: 1,
        },
    ])));

    let first = sound("first", "Alpha", "/music/album/alpha.flac");
    let second = sound("second", "beta", "/music/album/disc-1/beta.flac");
    let third = sound("third", "Гамма", "/music/other/gamma.flac");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![
        SoundRecord {
            sound: second.clone(),
            general_position: 1,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: Some("album/disc-1".to_string()),
                relative_path: "album/disc-1/beta.flac".to_string(),
            }],
        },
        SoundRecord {
            sound: third,
            general_position: 2,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: Some("other".to_string()),
                relative_path: "other/gamma.flac".to_string(),
            }],
        },
        SoundRecord {
            sound: first.clone(),
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: Some("album".to_string()),
                relative_path: "album/alpha.flac".to_string(),
            }],
        },
    ])));
    wait(
        store.apply_batch(LibraryBatch::ManualTabs(vec![ManualTabRecord {
            public_id: "favourites".to_string(),
            name: "Favourites".to_string(),
            position: 0,
        }])),
    );
    wait(store.apply_batch(LibraryBatch::ManualMemberships(vec![
        ManualMembershipRecord {
            tab_public_id: "favourites".to_string(),
            sound_public_id: "second".to_string(),
            position: 0,
        },
        ManualMembershipRecord {
            tab_public_id: "favourites".to_string(),
            sound_public_id: "first".to_string(),
            position: 1,
        },
    ])));

    let general = wait(store.page(LibraryScope::General, "ALP", 0));
    assert_eq!(general.total, 1);
    assert_eq!(general.sounds[0].id, first.id);
    assert_eq!(general.sounds[0].source_path, first.source_path);
    assert_eq!(general.sounds[0].loudness_lufs, first.loudness_lufs);
    assert_eq!(wait(store.count(LibraryScope::General, "гам")), 1);

    let favourites = wait(store.page(LibraryScope::ManualTab("favourites".to_string()), "", 0));
    assert_eq!(
        favourites
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );

    let album = wait(store.page(
        LibraryScope::Folder {
            root_path: "/music".to_string(),
            relative_path: "album".to_string(),
        },
        "be",
        0,
    ));
    assert_eq!(album.total, 1);
    assert_eq!(album.sounds[0].id, "second");

    wait(store.apply_batch(LibraryBatch::FolderOverrides(vec![
        FolderOverrideRecord {
            root_path: "/music".to_string(),
            folder_relative_path: "album".to_string(),
            sound_public_id: "first".to_string(),
            action: FolderOverrideAction::Exclude,
        },
        FolderOverrideRecord {
            root_path: "/music".to_string(),
            folder_relative_path: "album".to_string(),
            sound_public_id: "third".to_string(),
            action: FolderOverrideAction::Include,
        },
    ])));
    let customized_album = wait(store.page(
        LibraryScope::Folder {
            root_path: "/music".to_string(),
            relative_path: "album".to_string(),
        },
        "",
        0,
    ));
    assert_eq!(
        customized_album
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["second", "third"]
    );
    assert_eq!(PAGE_SIZE, 256);
}

#[test]
fn batch_size_is_rejected_before_reaching_sqlite() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    let roots = (0..=MAX_BATCH_ROWS)
        .map(|position| RootRecord {
            path: format!("/music/{position}"),
            position,
        })
        .collect();
    let error = store
        .apply_batch(LibraryBatch::Roots(roots))
        .recv()
        .expect_err("oversized batch must fail");
    assert!(error.to_string().contains("512"));
}

#[test]
fn duplicate_path_batch_rolls_back_without_removing_existing_rows() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: sound("existing", "Existing", "/music/existing.flac"),
        general_position: 0,
        locations: Vec::new(),
    }])));

    let duplicate_path = "/music/duplicate.flac";
    let error = store
        .apply_batch(LibraryBatch::Sounds(vec![
            SoundRecord {
                sound: sound("duplicate-a", "Duplicate A", duplicate_path),
                general_position: 1,
                locations: Vec::new(),
            },
            SoundRecord {
                sound: sound("duplicate-b", "Duplicate B", duplicate_path),
                general_position: 2,
                locations: Vec::new(),
            },
        ]))
        .recv()
        .expect_err("duplicate paths must reject the batch");
    assert!(error.to_string().contains("UNIQUE constraint failed"));
    assert_eq!(wait(store.count(LibraryScope::General, "")), 1);
}

#[test]
fn direct_sound_edits_keep_search_hotkeys_and_delete_cascades_consistent() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![
        SoundRecord {
            sound: sound("first", "Alpha", "/music/alpha.flac"),
            general_position: 0,
            locations: Vec::new(),
        },
        SoundRecord {
            sound: sound("second", "Beta", "/music/beta.flac"),
            general_position: 1,
            locations: Vec::new(),
        },
    ])));

    let mut updated = sound("first", "Gamma", "/music/gamma.flac");
    updated.hotkey = None;
    updated.volume = 42;
    updated.enabled = true;
    assert!(wait(store.update_sound(updated)));
    assert_eq!(wait(store.count(LibraryScope::General, "alpha")), 0);
    let renamed = wait(store.page(LibraryScope::General, "gamma", 0));
    assert_eq!(renamed.total, 1);
    assert_eq!(renamed.sounds[0].path, "/music/gamma.flac");
    assert_eq!(renamed.sounds[0].volume, 42);
    assert!(renamed.sounds[0].enabled);

    let hotkeys = wait(store.hotkey_page(0));
    assert_eq!(hotkeys.total, 1);
    assert_eq!(hotkeys.sounds[0].id, "second");

    assert!(wait(store.delete_sound("second")));
    assert!(!wait(store.delete_sound("second")));
    assert!(wait(store.sound_by_id("second")).is_none());
    assert_eq!(wait(store.count(LibraryScope::General, "beta")), 0);
    assert_eq!(wait(store.hotkey_page(0)).total, 0);
}

#[test]
fn adjacent_lookup_respects_scope_search_order_and_boundaries() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![
        SoundRecord {
            sound: sound("third", "Match Three", "/music/three.flac"),
            general_position: 2,
            locations: Vec::new(),
        },
        SoundRecord {
            sound: sound("first", "Match One", "/music/one.flac"),
            general_position: 0,
            locations: Vec::new(),
        },
        SoundRecord {
            sound: sound("second", "Skip Two", "/music/two.flac"),
            general_position: 1,
            locations: Vec::new(),
        },
    ])));

    let next =
        wait(store.adjacent(LibraryScope::General, "match", 0, 1)).expect("next filtered sound");
    assert_eq!(next.id, "third");
    let previous = wait(store.adjacent(LibraryScope::General, "match", 1, -1))
        .expect("previous filtered sound");
    assert_eq!(previous.id, "first");
    assert!(wait(store.adjacent(LibraryScope::General, "match", 0, -1)).is_none());
    assert!(wait(store.adjacent(LibraryScope::General, "match", 1, 1)).is_none());
}

#[test]
fn folder_navigation_pages_only_direct_children_at_arbitrary_depth() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
        path: "/home/flinux/Музика".to_string(),
        position: 0,
    }])));
    wait(store.apply_batch(LibraryBatch::Folders(vec![
        FolderRecord {
            root_path: "/home/flinux/Музика".to_string(),
            relative_path: "VIRTUALSURROUND".to_string(),
            parent_relative_path: None,
            name: "VIRTUALSURROUND".to_string(),
            position: 0,
        },
        FolderRecord {
            root_path: "/home/flinux/Музика".to_string(),
            relative_path: "sounds".to_string(),
            parent_relative_path: None,
            name: "sounds".to_string(),
            position: 1,
        },
        FolderRecord {
            root_path: "/home/flinux/Музика".to_string(),
            relative_path:
                "sounds/Cyberpunk 2077 Soundtrack Collection by Various Artists".to_string(),
            parent_relative_path: Some("sounds".to_string()),
            name: "Cyberpunk 2077 Soundtrack Collection by Various Artists".to_string(),
            position: 0,
        },
        FolderRecord {
            root_path: "/home/flinux/Музика".to_string(),
            relative_path:
                "sounds/Cyberpunk 2077 Soundtrack Collection by Various Artists/Disc 1".to_string(),
            parent_relative_path: Some(
                "sounds/Cyberpunk 2077 Soundtrack Collection by Various Artists".to_string(),
            ),
            name: "Disc 1".to_string(),
            position: 0,
        },
    ])));

    let roots = wait(store.roots(0));
    assert_eq!(roots.total, 1);
    assert_eq!(roots.roots[0].path, "/home/flinux/Музика");

    let top = wait(store.folder_children("/home/flinux/Музика", None, 0));
    assert_eq!(top.total, 2);
    assert_eq!(
        top.folders
            .iter()
            .map(|folder| folder.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["VIRTUALSURROUND", "sounds"]
    );
    assert!(!top.folders[0].has_children);
    assert!(top.folders[1].has_children);

    let sounds = wait(store.folder_children("/home/flinux/Музика", Some("sounds"), 0));
    assert_eq!(sounds.total, 1);
    assert_eq!(
        sounds.folders[0].relative_path,
        "sounds/Cyberpunk 2077 Soundtrack Collection by Various Artists"
    );
    assert!(sounds.folders[0].has_children);

    let album = wait(store.folder_children(
        "/home/flinux/Музика",
        Some("sounds/Cyberpunk 2077 Soundtrack Collection by Various Artists"),
        0,
    ));
    assert_eq!(album.total, 1);
    assert_eq!(album.folders[0].name, "Disc 1");
    assert!(!album.folders[0].has_children);
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

#[test]
#[ignore = "production-scale SQLite timing and memory gate"]
#[allow(clippy::print_stderr)]
fn benchmark_156k_bounded_store() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    let started = std::time::Instant::now();
    for batch_start in (0..156_000).step_by(MAX_BATCH_ROWS) {
        let batch_end = (batch_start + MAX_BATCH_ROWS).min(156_000);
        let rows = (batch_start..batch_end)
            .map(|index| SoundRecord {
                sound: sound(
                    &format!("sound-{index:06}"),
                    &format!("Sound {index:06}"),
                    &format!("/music/long/unicode/Шлях/Sound-{index:06}.flac"),
                ),
                general_position: index,
                locations: Vec::new(),
            })
            .collect();
        wait(store.apply_batch(LibraryBatch::Sounds(rows)));
    }
    let import_elapsed = started.elapsed();
    assert!(import_elapsed < std::time::Duration::from_secs(30));
    assert_eq!(wait(store.count(LibraryScope::General, "")), 156_000);

    let mut slowest_query = std::time::Duration::ZERO;
    for (search, page) in [("", 0), ("", 609), ("155999", 0), ("99", 0)] {
        let query_started = std::time::Instant::now();
        let result = wait(store.page(LibraryScope::General, search, page));
        slowest_query = slowest_query.max(query_started.elapsed());
        assert!(!result.sounds.is_empty());
        assert!(slowest_query < std::time::Duration::from_millis(100));
    }

    let smaps = std::fs::read_to_string("/proc/self/smaps_rollup").expect("read smaps_rollup");
    let pss_kib = smaps
        .lines()
        .find_map(|line| line.strip_prefix("Pss:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<usize>().ok())
        .expect("parse PSS");
    eprintln!(
        "156k SQLite gate: import={import_elapsed:?}, slowest_query={slowest_query:?}, pss={pss_kib} KiB"
    );
    assert!(pss_kib < 102_400, "store process PSS was {pss_kib} KiB");
}
