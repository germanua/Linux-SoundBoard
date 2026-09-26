#[test]
fn manual_and_folder_edits_are_atomic_bounded_and_immediately_visible() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
        path: "/music".to_string(),
        position: 0,
    }])));
    wait(store.apply_batch(LibraryBatch::Folders(vec![FolderRecord {
        root_path: "/music".to_string(),
        relative_path: "album".to_string(),
        parent_relative_path: None,
        name: "Album".to_string(),
        position: 0,
    }])));
    wait(store.apply_batch(LibraryBatch::Sounds(vec![
        SoundRecord {
            sound: sound("first", "First", "/music/album/first.flac"),
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: Some("album".to_string()),
                relative_path: "album/first.flac".to_string(),
            }],
        },
        SoundRecord {
            sound: sound("second", "Second", "/elsewhere/second.flac"),
            general_position: 1,
            locations: Vec::new(),
        },
    ])));

    assert!(wait(store.upsert_manual_tab(ManualTabRecord {
        public_id: "favourites".to_string(),
        name: "Favourites".to_string(),
        position: 0,
    })));
    assert!(wait(store.set_manual_membership(ManualMembershipRecord {
        tab_public_id: "favourites".to_string(),
        sound_public_id: "second".to_string(),
        position: 0,
    })));
    assert!(wait(store.set_manual_membership(ManualMembershipRecord {
        tab_public_id: "favourites".to_string(),
        sound_public_id: "first".to_string(),
        position: 1,
    })));
    let tabs = wait(store.manual_tabs(0));
    assert_eq!(tabs.total, 1);
    assert_eq!(tabs.tabs[0].name, "Favourites");
    assert_eq!(tabs.tabs[0].sound_count, 2);
    let favourites = wait(store.page(LibraryScope::ManualTab("favourites".to_string()), "", 0));
    assert_eq!(
        favourites
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["second", "first"]
    );

    assert!(wait(store.set_folder_override(FolderOverrideRecord {
        root_path: "/music".to_string(),
        folder_relative_path: "album".to_string(),
        sound_public_id: "second".to_string(),
        action: FolderOverrideAction::Include,
    })));
    assert_eq!(
        wait(store.count(
            LibraryScope::Folder {
                root_path: "/music".to_string(),
                relative_path: "album".to_string(),
            },
            ""
        )),
        2
    );
    assert!(wait(
        store.clear_folder_override("/music", "album", "second")
    ));
    assert_eq!(
        wait(store.count(
            LibraryScope::Folder {
                root_path: "/music".to_string(),
                relative_path: "album".to_string(),
            },
            ""
        )),
        1
    );

    assert!(wait(store.set_folder_preferences(
        "/music",
        "album",
        Some("Renamed Album"),
        Some(3),
        true,
    )));
    let folders = wait(store.folder_children("/music", None, 0));
    assert_eq!(folders.folders[0].name, "Renamed Album");
    assert!(folders.folders[0].expanded);
    assert!(wait(store.set_folder_expanded("/music", "album", false)));
    let folders = wait(store.folder_children("/music", None, 0));
    assert_eq!(folders.folders[0].name, "Renamed Album");
    assert!(!folders.folders[0].expanded);
    assert!(wait(store.set_folder_display_name(
        "/music",
        "album",
        Some("Album shortcut")
    )));
    let folders = wait(store.folder_children("/music", None, 0));
    assert_eq!(folders.folders[0].name, "Album shortcut");
    assert!(!folders.folders[0].expanded);

    assert!(wait(store.remove_manual_membership("favourites", "second")));
    assert_eq!(
        wait(store.count(LibraryScope::ManualTab("favourites".to_string()), "")),
        1
    );
    let oversized = store
        .remove_manual_memberships(
            "favourites",
            vec!["first".to_string(); crate::library_store::MAX_BATCH_ROWS + 1],
        )
        .recv()
        .expect_err("oversized removal must fail before mutation");
    assert!(oversized.to_string().contains("limited"));
    assert_eq!(
        wait(store.count(LibraryScope::ManualTab("favourites".to_string()), "")),
        1
    );
    assert!(wait(store.remove_manual_memberships(
        "favourites",
        vec!["first".to_string()]
    )));
    assert_eq!(
        wait(store.count(LibraryScope::ManualTab("favourites".to_string()), "")),
        0
    );
    assert!(wait(store.delete_manual_tab("favourites")));
    assert_eq!(wait(store.manual_tabs(0)).total, 0);
}

#[test]
fn active_root_generation_hides_partial_scan_until_atomic_switch() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    let store = LibraryStore::open(path.clone()).expect("open store");
    wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
        path: "/music".to_string(),
        position: 0,
    }])));
    wait(store.apply_batch(LibraryBatch::Folders(vec![FolderRecord {
        root_path: "/music".to_string(),
        relative_path: "old".to_string(),
        parent_relative_path: None,
        name: "Old".to_string(),
        position: 0,
    }])));
    wait(store.apply_batch(LibraryBatch::Sounds(vec![
        SoundRecord {
            sound: sound("old", "Old", "/music/old/old.flac"),
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: Some("old".to_string()),
                relative_path: "old/old.flac".to_string(),
            }],
        },
        SoundRecord {
            sound: sound("standalone", "Standalone", "/imports/standalone.flac"),
            general_position: 1,
            locations: Vec::new(),
        },
    ])));
    drop(store);

    let connection = rusqlite::Connection::open(&path).expect("open raw database");
    connection
        .execute_batch(
            "PRAGMA foreign_keys = ON;
             INSERT INTO folders(root_id, parent_id, relative_path, name, position)
             SELECT id, NULL, 'new', 'New', 0 FROM roots WHERE path = '/music';
             INSERT INTO folder_presence(folder_id, generation)
             SELECT id, 1 FROM folders WHERE relative_path = 'new';
             INSERT INTO folder_closure(ancestor_id, descendant_id, depth)
             SELECT id, id, 0 FROM folders WHERE relative_path = 'new';
             INSERT INTO sounds(
                 public_id, name, search_name, path, source_path, duration_ms,
                 volume, enabled, loudness_lufs, loudness_state, loudness_confidence,
                 loudness_fingerprint, loudness_true_peak_dbtp, general_position, standalone
             ) VALUES(
                 'new', 'New', 'new', '/music/new/new.flac', NULL, NULL,
                 100, 1, NULL, 'pending', NULL, NULL, NULL, 2, 0
             );
             INSERT INTO sound_locations(sound_id, root_id, generation, folder_id, relative_path)
             SELECT sound.rowid, root.id, 1, folder.id, 'new/new.flac'
             FROM sounds AS sound, roots AS root, folders AS folder
             WHERE sound.public_id = 'new' AND root.path = '/music'
               AND folder.root_id = root.id AND folder.relative_path = 'new';",
        )
        .expect("stage inactive generation");
    drop(connection);

    let store = LibraryStore::open(path.clone()).expect("reopen store");
    let before = wait(store.page(LibraryScope::General, "", 0));
    assert_eq!(
        before
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["old", "standalone"]
    );
    assert_eq!(
        wait(store.folder_children("/music", None, 0)).folders[0].name,
        "Old"
    );
    drop(store);

    let connection = rusqlite::Connection::open(&path).expect("open raw database");
    connection
        .execute(
            "UPDATE roots SET active_generation = 1 WHERE path = '/music'",
            [],
        )
        .expect("switch active generation");
    drop(connection);

    let store = LibraryStore::open(path).expect("reopen switched store");
    let after = wait(store.page(LibraryScope::General, "", 0));
    assert_eq!(
        after
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["standalone", "new"]
    );
    assert_eq!(
        wait(store.folder_children("/music", None, 0)).folders[0].name,
        "New"
    );
}

#[test]
fn root_scan_api_stages_batches_and_switches_visibility_atomically() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    let generation = wait(store.begin_root_scan("/music", 0));
    wait(store.apply_root_scan_batch(
        "/music",
        generation,
        vec![FolderRecord {
            root_path: "/music".to_string(),
            relative_path: "album/deep".to_string(),
            parent_relative_path: Some("album".to_string()),
            name: "deep".to_string(),
            position: 0,
        }],
        vec![SoundRecord {
            sound: sound("first", "First", "/music/album/deep/first.flac"),
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: Some("album/deep".to_string()),
                relative_path: "album/deep/first.flac".to_string(),
            }],
        }],
    ));

    assert_eq!(wait(store.count(LibraryScope::General, "")), 0);
    assert!(wait(store.folder_children("/music", None, 0))
        .folders
        .is_empty());

    assert!(wait(store.finish_root_scan("/music", generation)));
    assert_eq!(wait(store.count(LibraryScope::General, "")), 1);
    let top = wait(store.folder_children("/music", None, 0));
    assert_eq!(top.folders[0].relative_path, "album");
    let deep = wait(store.folder_children("/music", Some("album"), 0));
    assert_eq!(deep.folders[0].relative_path, "album/deep");
}

#[test]
fn customized_disappeared_folder_becomes_an_exact_manual_tab() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: sound("included", "Included", "/elsewhere/included.flac"),
        general_position: 2,
        locations: Vec::new(),
    }])));

    let first_generation = wait(store.begin_root_scan("/music", 0));
    wait(store.apply_root_scan_batch(
        "/music",
        first_generation,
        vec![
            FolderRecord {
                root_path: "/music".to_string(),
                relative_path: "album".to_string(),
                parent_relative_path: None,
                name: "album".to_string(),
                position: 0,
            },
            FolderRecord {
                root_path: "/music".to_string(),
                relative_path: "plain".to_string(),
                parent_relative_path: None,
                name: "plain".to_string(),
                position: 1,
            },
        ],
        vec![
            SoundRecord {
                sound: sound("kept", "Kept", "/music/album/kept.flac"),
                general_position: 0,
                locations: vec![SoundLocationRecord {
                    root_path: "/music".to_string(),
                    folder_relative_path: Some("album".to_string()),
                    relative_path: "album/kept.flac".to_string(),
                }],
            },
            SoundRecord {
                sound: sound("excluded", "Excluded", "/music/album/excluded.flac"),
                general_position: 1,
                locations: vec![SoundLocationRecord {
                    root_path: "/music".to_string(),
                    folder_relative_path: Some("album".to_string()),
                    relative_path: "album/excluded.flac".to_string(),
                }],
            },
            SoundRecord {
                sound: sound("plain", "Plain", "/music/plain/plain.flac"),
                general_position: 3,
                locations: vec![SoundLocationRecord {
                    root_path: "/music".to_string(),
                    folder_relative_path: Some("plain".to_string()),
                    relative_path: "plain/plain.flac".to_string(),
                }],
            },
        ],
    ));
    assert!(wait(store.finish_root_scan("/music", first_generation)));
    assert!(wait(store.set_folder_display_name(
        "/music",
        "album",
        Some("Saved Album")
    )));
    assert!(wait(store.set_folder_override(FolderOverrideRecord {
        root_path: "/music".to_string(),
        folder_relative_path: "album".to_string(),
        sound_public_id: "excluded".to_string(),
        action: FolderOverrideAction::Exclude,
    })));
    assert!(wait(store.set_folder_override(FolderOverrideRecord {
        root_path: "/music".to_string(),
        folder_relative_path: "album".to_string(),
        sound_public_id: "included".to_string(),
        action: FolderOverrideAction::Include,
    })));

    let empty_generation = wait(store.begin_root_scan("/music", 0));
    assert!(wait(store.finish_root_scan("/music", empty_generation)));

    assert!(wait(store.folder_children("/music", None, 0))
        .folders
        .is_empty());
    let tabs = wait(store.manual_tabs(0));
    assert_eq!(tabs.total, 1);
    assert_eq!(tabs.tabs[0].name, "Saved Album");
    let converted = wait(store.page(
        LibraryScope::ManualTab(tabs.tabs[0].public_id.clone()),
        "",
        0,
    ));
    assert_eq!(
        converted
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["kept", "included"]
    );
}

#[test]
fn generated_folder_order_can_move_one_sibling_at_a_time() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
        path: "/music".to_string(),
        position: 0,
    }])));
    wait(
        store.apply_batch(LibraryBatch::Folders(
            ["a", "b", "c"]
                .into_iter()
                .enumerate()
                .map(|(position, name)| FolderRecord {
                    root_path: "/music".to_string(),
                    relative_path: name.to_string(),
                    parent_relative_path: None,
                    name: name.to_string(),
                    position,
                })
                .collect(),
        )),
    );

    assert!(wait(store.move_folder("/music", "c", -1)));
    assert_eq!(
        wait(store.folder_children("/music", None, 0))
            .folders
            .iter()
            .map(|folder| folder.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["a", "c", "b"]
    );
    assert!(wait(store.move_folder("/music", "c", -1)));
    assert!(!wait(store.move_folder("/music", "c", -1)));
    assert_eq!(
        wait(store.folder_children("/music", None, 0))
            .folders
            .iter()
            .map(|folder| folder.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["c", "a", "b"]
    );
    drop(store);
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("reopen store");
    assert_eq!(
        wait(store.folder_children("/music", None, 0))
            .folders
            .iter()
            .map(|folder| folder.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["c", "a", "b"]
    );
}

#[test]
fn cancelled_root_scan_preserves_the_previous_generation() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    let first_generation = wait(store.begin_root_scan("/music", 0));
    wait(store.apply_root_scan_batch(
        "/music",
        first_generation,
        Vec::new(),
        vec![SoundRecord {
            sound: sound("old", "Old", "/music/old.flac"),
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: None,
                relative_path: "old.flac".to_string(),
            }],
        }],
    ));
    assert!(wait(store.finish_root_scan("/music", first_generation)));

    let staged_generation = wait(store.begin_root_scan("/music", 0));
    wait(store.apply_root_scan_batch(
        "/music",
        staged_generation,
        Vec::new(),
        vec![SoundRecord {
            sound: sound("new", "New", "/music/new.flac"),
            general_position: 0,
            locations: vec![SoundLocationRecord {
                root_path: "/music".to_string(),
                folder_relative_path: None,
                relative_path: "new.flac".to_string(),
            }],
        }],
    ));

    assert!(wait(store.cancel_root_scan("/music", staged_generation)));
    let visible = wait(store.page(LibraryScope::General, "", 0));
    assert_eq!(
        visible
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["old"]
    );
}

#[test]
fn removing_a_root_hides_its_orphaned_sounds_but_preserves_manual_sounds() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    let generation = wait(store.begin_root_scan("/music", 0));
    wait(store.apply_root_scan_batch(
        "/music",
        generation,
        Vec::new(),
        vec![
            SoundRecord {
                sound: sound("orphan", "Orphan", "/music/orphan.flac"),
                general_position: 0,
                locations: vec![SoundLocationRecord {
                    root_path: "/music".to_string(),
                    folder_relative_path: None,
                    relative_path: "orphan.flac".to_string(),
                }],
            },
            SoundRecord {
                sound: sound("manual", "Manual", "/music/manual.flac"),
                general_position: 1,
                locations: vec![SoundLocationRecord {
                    root_path: "/music".to_string(),
                    folder_relative_path: None,
                    relative_path: "manual.flac".to_string(),
                }],
            },
        ],
    ));
    assert!(wait(store.finish_root_scan("/music", generation)));
    wait(
        store.apply_batch(LibraryBatch::ManualTabs(vec![ManualTabRecord {
            public_id: "kept".to_string(),
            name: "Kept".to_string(),
            position: 0,
        }])),
    );
    wait(store.apply_batch(LibraryBatch::ManualMemberships(vec![
        ManualMembershipRecord {
            tab_public_id: "kept".to_string(),
            sound_public_id: "manual".to_string(),
            position: 0,
        },
    ])));

    assert!(wait(store.remove_root("/music")));

    assert!(wait(store.roots(0)).roots.is_empty());
    let general = wait(store.page(LibraryScope::General, "", 0));
    assert_eq!(
        general
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["manual"]
    );
    assert!(wait(store.sound_by_id("orphan")).is_none());
}

#[test]
fn first_root_scan_converts_legacy_generated_membership_to_sparse_overrides() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: sound("included", "Included", "/music/included.flac"),
        general_position: 0,
        locations: Vec::new(),
    }])));
    wait(store.apply_batch(LibraryBatch::LegacyGeneratedTabs(vec![
        LegacyGeneratedTabRecord {
            public_id: "legacy-album".to_string(),
            root_path: "/music".to_string(),
            relative_path: "album".to_string(),
            name: "My Album".to_string(),
            position: 4,
        },
    ])));
    wait(
        store.apply_batch(LibraryBatch::LegacyGeneratedMemberships(vec![
            LegacyGeneratedMembershipRecord {
                tab_public_id: "legacy-album".to_string(),
                sound_public_id: "included".to_string(),
                position: 0,
            },
        ])),
    );

    let generation = wait(store.begin_root_scan("/music", 0));
    wait(store.apply_root_scan_batch(
        "/music",
        generation,
        vec![FolderRecord {
            root_path: "/music".to_string(),
            relative_path: "album".to_string(),
            parent_relative_path: None,
            name: "album".to_string(),
            position: 0,
        }],
        vec![
            SoundRecord {
                sound: sound("included", "Included", "/music/included.flac"),
                general_position: 0,
                locations: vec![SoundLocationRecord {
                    root_path: "/music".to_string(),
                    folder_relative_path: None,
                    relative_path: "included.flac".to_string(),
                }],
            },
            SoundRecord {
                sound: sound("excluded", "Excluded", "/music/album/excluded.flac"),
                general_position: 1,
                locations: vec![SoundLocationRecord {
                    root_path: "/music".to_string(),
                    folder_relative_path: Some("album".to_string()),
                    relative_path: "album/excluded.flac".to_string(),
                }],
            },
        ],
    ));
    assert!(wait(store.finish_root_scan("/music", generation)));

    let album = wait(store.page(
        LibraryScope::Folder {
            root_path: "/music".to_string(),
            relative_path: "album".to_string(),
        },
        "",
        0,
    ));
    assert_eq!(
        album
            .sounds
            .iter()
            .map(|sound| sound.id.as_str())
            .collect::<Vec<_>>(),
        ["included"]
    );
    assert_eq!(
        wait(store.folder_children("/music", None, 0)).folders[0].name,
        "My Album"
    );
}

#[test]
fn schema_one_migration_preserves_duplicate_hotkeys_for_user_resolution() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    let connection = rusqlite::Connection::open(&path).expect("create schema one database");
    connection
        .execute_batch(
            "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO meta VALUES('schema_version', '1');
             INSERT INTO meta VALUES('schema_flavor', 'bounded-generation-v1');
             CREATE TABLE sounds(
                 rowid INTEGER PRIMARY KEY,
                 public_id TEXT NOT NULL UNIQUE,
                 hotkey TEXT
             );
             CREATE INDEX sounds_hotkey ON sounds(hotkey) WHERE hotkey IS NOT NULL;
             INSERT INTO sounds(public_id, hotkey) VALUES
                 ('first', 'Ctrl+KeyA'), ('second', 'ctrl+keya'), ('third', 'Alt+KeyB');
             CREATE TABLE folder_prefs(
                 folder_id INTEGER PRIMARY KEY,
                 display_name TEXT,
                 sibling_position INTEGER,
                 expanded INTEGER NOT NULL DEFAULT 0 CHECK(expanded IN (0, 1))
             );
             PRAGMA user_version = 1;",
        )
        .expect("seed schema one database");
    drop(connection);

    let store = LibraryStore::open(path.clone()).expect("migrate schema one database");
    drop(store);

    let connection = rusqlite::Connection::open(path).expect("inspect migrated database");
    type MigratedHotkeyRow = (String, String, Option<String>, String, Option<String>);
    let rows: Vec<MigratedHotkeyRow> = connection
        .prepare(
            "SELECT binding_id, accelerator, normalized, state, issue
             FROM hotkey_bindings ORDER BY binding_id",
        )
        .expect("prepare migrated hotkeys")
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .expect("read migrated hotkeys")
        .collect::<Result<_, _>>()
        .expect("collect migrated hotkeys");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].2, None);
    assert_eq!(rows[0].3, "needs_attention");
    assert_eq!(rows[1].2, None);
    assert_eq!(rows[2].2.as_deref(), Some("alt+keyb"));
    assert_eq!(rows[2].3, "active");

    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read migrated schema version");
    assert_eq!(version, crate::library_store::DATABASE_SCHEMA_VERSION);
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
