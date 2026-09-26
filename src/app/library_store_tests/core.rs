use std::path::{Path, PathBuf};

use crate::config::{LoudnessAnalysisState, Sound};
use crate::library_store::{
    FolderOverrideAction, FolderOverrideRecord, FolderRecord, HotkeyBindingOwner,
    HotkeyBindingRecord, LegacyGeneratedMembershipRecord, LegacyGeneratedTabRecord, LibraryBatch,
    LibraryScope, LibraryStore, LoudnessUpdate, ManualMembershipRecord, ManualTabRecord,
    RootRecord, SoundLocationRecord, SoundRecord, MAX_BATCH_ROWS, PAGE_SIZE,
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
fn adding_an_existing_root_is_idempotent() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    let root = || RootRecord {
        path: "/music".to_string(),
        position: 0,
    };

    wait(store.apply_batch(LibraryBatch::Roots(vec![root()])));
    wait(store.apply_batch(LibraryBatch::Roots(vec![root()])));

    let roots = wait(store.roots(0));
    assert_eq!(roots.total, 1);
    assert_eq!(roots.roots[0].path, "/music");
}

#[test]
fn loudness_backfill_is_keyset_paged_and_updated_in_bounded_batches() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    let rows = (0..300)
        .map(|index| {
            let mut item = sound(
                &format!("sound-{index:03}"),
                &format!("Sound {index:03}"),
                &format!("/music/sound-{index:03}.wav"),
            );
            item.loudness_lufs = None;
            item.loudness_analysis_state = LoudnessAnalysisState::Pending;
            item.loudness_confidence = None;
            SoundRecord {
                sound: item,
                general_position: index,
                locations: Vec::new(),
            }
        })
        .collect();
    wait(store.apply_batch(LibraryBatch::Sounds(rows)));

    let first = wait(store.loudness_backfill_after(None));
    assert_eq!(first.sounds.len(), PAGE_SIZE);
    assert_eq!(first.sounds.first().unwrap().id, "sound-000");
    assert_eq!(first.sounds.last().unwrap().id, "sound-255");
    let second = wait(store.loudness_backfill_after(Some("sound-255")));
    assert_eq!(second.sounds.len(), 44);
    assert_eq!(second.sounds.first().unwrap().id, "sound-256");

    assert_eq!(
        wait(store.apply_loudness_updates(vec![
            LoudnessUpdate {
                sound_id: "sound-000".to_string(),
                lufs: Some(-14.0),
                state: LoudnessAnalysisState::Refined,
                confidence: Some(1.0),
                true_peak_dbtp: Some(-1.0),
            },
            LoudnessUpdate {
                sound_id: "sound-001".to_string(),
                lufs: Some(-15.0),
                state: LoudnessAnalysisState::Estimated,
                confidence: Some(0.7),
                true_peak_dbtp: Some(-2.0),
            },
        ])),
        2
    );
    let stats = wait(store.loudness_stats());
    assert_eq!(stats.total, 300);
    assert_eq!(stats.missing, 298);
    assert_eq!(stats.pending, 298);
    assert_eq!(stats.estimated, 1);
    assert_eq!(stats.refined, 1);
}

#[test]
fn forced_loudness_refinement_uses_stable_keyset_pages() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    let rows = (0..3)
        .map(|index| {
            let mut item = sound(
                &format!("sound-{index}"),
                &format!("Sound {index}"),
                &format!("/music/sound-{index}.wav"),
            );
            item.loudness_confidence = Some(if index == 1 { 0.95 } else { 0.5 });
            SoundRecord {
                sound: item,
                general_position: index,
                locations: Vec::new(),
            }
        })
        .collect();
    wait(store.apply_batch(LibraryBatch::Sounds(rows)));

    let normal = wait(store.loudness_refinement_candidates(false, None, 10));
    assert_eq!(normal.sounds.len(), 2);
    let first = wait(store.loudness_refinement_candidates(true, None, 2));
    assert_eq!(
        first
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["sound-0", "sound-1"]
    );
    let second = wait(store.loudness_refinement_candidates(true, Some("sound-1"), 2));
    assert_eq!(second.sounds[0].id, "sound-2");
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
    assert_eq!(wait(store.count(LibraryScope::General, "ALP")), 1);
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
    assert_eq!(album.sounds.len(), 1);
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
    assert_eq!(renamed.sounds.len(), 1);
    assert_eq!(renamed.sounds[0].path, "/music/gamma.flac");
    assert_eq!(renamed.sounds[0].volume, 42);
    assert!(renamed.sounds[0].enabled);

    let hotkeys = wait(store.hotkey_page(0));
    assert_eq!(hotkeys.sounds.len(), 2);
    assert_eq!(hotkeys.sounds[0].id, "first");
    assert_eq!(hotkeys.sounds[1].id, "second");

    assert!(wait(store.delete_sound("second")));
    assert!(!wait(store.delete_sound("second")));
    assert!(wait(store.sound_by_id("second")).is_none());
    assert_eq!(wait(store.count(LibraryScope::General, "beta")), 0);
    let hotkeys = wait(store.hotkey_page(0));
    assert_eq!(hotkeys.sounds.len(), 1);
    assert_eq!(hotkeys.sounds[0].id, "first");
}

#[test]
fn hotkey_bindings_have_one_owner() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    let store = LibraryStore::open(path.clone()).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: sound("first", "First", "/music/first.flac"),
        general_position: 0,
        locations: Vec::new(),
    }])));
    drop(store);

    let connection = rusqlite::Connection::open(path).expect("open raw database");
    let sound_id: i64 = connection
        .query_row(
            "SELECT rowid FROM sounds WHERE public_id = 'first'",
            [],
            |row| row.get(0),
        )
        .expect("read sound row id");
    let stored: (String, String) = connection
        .query_row(
            "SELECT accelerator, state FROM hotkey_bindings WHERE sound_id = ?1",
            [sound_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("sound hotkey is stored in the unified table");
    assert_eq!(stored, ("Ctrl+first".to_string(), "active".to_string()));

    connection
        .execute(
            "INSERT INTO hotkey_bindings(
                 binding_id, control_action, accelerator, normalized, state, issue
             ) VALUES('control:needs-attention', 'needs-attention', 'Ctrl+first', NULL,
                      'needs_attention', 'duplicate legacy binding')",
            [],
        )
        .expect("invalid legacy bindings remain recoverable without becoming active");

    let owner_error = connection.execute(
        "INSERT INTO hotkey_bindings(
             binding_id, sound_id, control_action, accelerator, normalized, state
         ) VALUES('invalid-owner', ?1, 'stop', 'Ctrl+KeyS', 'ctrl+keys', 'active')",
        [sound_id],
    );
    assert!(
        owner_error.is_err(),
        "a binding must have exactly one owner"
    );

    let sound_columns: Vec<String> = connection
        .prepare("PRAGMA table_info(sounds)")
        .expect("inspect sounds table")
        .query_map([], |row| row.get(1))
        .expect("query sounds columns")
        .collect::<Result<_, _>>()
        .expect("collect sounds columns");
    assert!(!sound_columns.iter().any(|column| column == "hotkey"));
}

#[test]
fn hotkey_binding_api_pages_and_replaces_control_bindings_atomically() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "control:stop".to_string(),
        owner: HotkeyBindingOwner::Control("stop".to_string()),
        accelerator: "Ctrl+KeyS".to_string(),
        normalized: Some("Ctrl+KeyS".to_string()),
        issue: None,
        tab_scope: None,
    })));
    let page = wait(store.hotkey_bindings_after(None));
    assert_eq!(page.bindings.len(), 1);
    assert_eq!(page.bindings[0].binding_id, "control:stop");
    assert_eq!(page.bindings[0].normalized.as_deref(), Some("Ctrl+KeyS"));

    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "control:stop".to_string(),
        owner: HotkeyBindingOwner::Control("stop".to_string()),
        accelerator: "Alt+KeyS".to_string(),
        normalized: Some("Alt+KeyS".to_string()),
        issue: None,
        tab_scope: None,
    })));
    let replaced = wait(store.hotkey_bindings_after(None));
    assert_eq!(replaced.bindings.len(), 1);
    assert_eq!(replaced.bindings[0].accelerator, "Alt+KeyS");

    let invalid = store
        .set_hotkey_binding(HotkeyBindingRecord {
            binding_id: "control:stop".to_string(),
            owner: HotkeyBindingOwner::Control("stop".to_string()),
            accelerator: "Ctrl+KeyA".to_string(),
            normalized: Some("Alt+KeyB".to_string()),
            issue: None,
            tab_scope: None,
        })
        .recv()
        .expect_err("normalized binding must match its accelerator");
    assert!(invalid.to_string().contains("canonical"));
    assert_eq!(
        wait(store.hotkey_bindings_after(None)).bindings[0].accelerator,
        "Alt+KeyS"
    );

    assert!(wait(store.delete_hotkey_binding("control:stop")));
    assert!(wait(store.hotkey_bindings_after(None)).bindings.is_empty());
}

#[test]
fn hotkey_projection_excludes_stale_scan_sounds_but_keeps_controls() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "control:stop".to_string(),
        owner: HotkeyBindingOwner::Control("stop".to_string()),
        accelerator: "Alt+KeyS".to_string(),
        normalized: Some("Alt+KeyS".to_string()),
        issue: None,
        tab_scope: None,
    })));

    let staged_generation = wait(store.begin_root_scan("/music", 0));
    let mut staged_sound = sound("staged", "Staged", "/music/staged.flac");
    staged_sound.hotkey = Some("Ctrl+KeyP".to_string());
    wait(store.apply_root_scan_batch(
        "/music",
        staged_generation,
        Vec::new(),
        vec![SoundRecord {
            sound: staged_sound,
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: None,
                relative_path: "staged.flac".to_string(),
            }],
        }],
    ));
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "staged".to_string(),
        owner: HotkeyBindingOwner::Sound("staged".to_string()),
        accelerator: "Ctrl+KeyP".to_string(),
        normalized: Some("Ctrl+KeyP".to_string()),
        issue: None,
        tab_scope: None,
    })));

    let bindings = wait(store.hotkey_bindings_after(None));
    assert_eq!(bindings.bindings.len(), 1);
    assert_eq!(bindings.bindings[0].binding_id, "control:stop");
}

#[test]
fn completed_scan_releases_stale_sound_hotkey_for_reassignment() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    let first_generation = wait(store.begin_root_scan("/music", 0));
    wait(store.apply_root_scan_batch(
        "/music",
        first_generation,
        Vec::new(),
        vec![SoundRecord {
            sound: sound("removed", "Removed", "/music/removed.flac"),
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: None,
                relative_path: "removed.flac".to_string(),
            }],
        }],
    ));
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "removed".to_string(),
        owner: HotkeyBindingOwner::Sound("removed".to_string()),
        accelerator: "Ctrl+KeyP".to_string(),
        normalized: Some("Ctrl+KeyP".to_string()),
        issue: None,
        tab_scope: None,
    })));
    assert!(wait(store.finish_root_scan("/music", first_generation)));

    let empty_generation = wait(store.begin_root_scan("/music", 0));
    assert!(wait(store.finish_root_scan("/music", empty_generation)));

    assert!(wait(store.hotkey_conflict("control:stop", "Ctrl+KeyP", false, None)).is_none());
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "control:stop".to_string(),
        owner: HotkeyBindingOwner::Control("stop".to_string()),
        accelerator: "Ctrl+KeyP".to_string(),
        normalized: Some("Ctrl+KeyP".to_string()),
        issue: None,
        tab_scope: None,
    })));
}

#[test]
fn moving_a_sound_between_staged_roots_preserves_its_hotkey() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    let first_generation = wait(store.begin_root_scan("/first", 0));
    let record = SoundRecord {
        sound: sound("moving", "Moving", "/shared/moving.flac"),
        general_position: 0,
        locations: vec![SoundLocationRecord {
            root_path: "/first".to_string(),
            folder_relative_path: None,
            relative_path: "moving.flac".to_string(),
        }],
    };
    wait(store.apply_root_scan_batch("/first", first_generation, Vec::new(), vec![record]));
    assert!(wait(store.finish_root_scan("/first", first_generation)));
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "moving".to_string(),
        owner: HotkeyBindingOwner::Sound("moving".to_string()),
        accelerator: "Ctrl+KeyP".to_string(),
        normalized: Some("Ctrl+KeyP".to_string()),
        issue: None,
        tab_scope: None,
    })));

    let second_generation = wait(store.begin_root_scan("/second", 1));
    wait(store.apply_root_scan_batch(
        "/second",
        second_generation,
        Vec::new(),
        vec![SoundRecord {
            sound: sound("moving", "Moving", "/shared/moving.flac"),
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/second".to_string(),
                folder_relative_path: None,
                relative_path: "moving.flac".to_string(),
            }],
        }],
    ));

    let empty_generation = wait(store.begin_root_scan("/first", 0));
    assert!(wait(store.finish_root_scan("/first", empty_generation)));
    assert!(wait(store.finish_root_scan("/second", second_generation)));

    let bindings = wait(store.hotkey_bindings_after(None));
    assert_eq!(bindings.bindings.len(), 1);
    assert_eq!(bindings.bindings[0].binding_id, "moving");
}

#[test]
fn playback_lookup_resolves_only_active_sound_bindings() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: sound("first", "First", "/music/first.flac"),
        general_position: 0,
        locations: Vec::new(),
    }])));

    let resolved = wait(store.sound_for_binding("first")).expect("active sound binding");
    assert_eq!(resolved.id, "first");
    assert!(wait(store.sound_for_binding("missing")).is_none());

    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "first".to_string(),
        owner: HotkeyBindingOwner::Sound("first".to_string()),
        accelerator: "not valid".to_string(),
        normalized: None,
        issue: Some("invalid legacy binding".to_string()),
        tab_scope: None,
    })));
    assert!(wait(store.sound_for_binding("first")).is_none());
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
fn sound_position_respects_scope_search_and_stable_order() {
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

    assert_eq!(
        wait(store.position_for_sound(LibraryScope::General, "match", "first")),
        Some(0)
    );
    assert_eq!(
        wait(store.position_for_sound(LibraryScope::General, "match", "third")),
        Some(1)
    );
    assert_eq!(
        wait(store.position_for_sound(LibraryScope::General, "match", "second")),
        None
    );
    assert_eq!(
        wait(store.position_for_sound(LibraryScope::General, "match", "missing")),
        None
    );
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
