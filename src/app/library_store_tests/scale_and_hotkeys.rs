#[test]
#[ignore = "production-scale SQLite timing and memory gate"]
#[allow(clippy::print_stderr)]
fn benchmark_156k_bounded_store() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
        path: "/music".to_string(),
        position: 0,
    }])));
    wait(store.apply_batch(LibraryBatch::Folders(vec![FolderRecord {
        root_path: "/music".to_string(),
        relative_path: "long/unicode/Шлях".to_string(),
        parent_relative_path: None,
        name: "Шлях".to_string(),
        position: 0,
    }])));
    wait(
        store.apply_batch(LibraryBatch::ManualTabs(
            (0..8)
                .map(|index| ManualTabRecord {
                    public_id: format!("tab-{index}"),
                    name: format!("Tab {index}"),
                    position: index,
                })
                .collect(),
        )),
    );
    let started = std::time::Instant::now();
    let sounds_per_batch = MAX_BATCH_ROWS / 2;
    for batch_start in (0..156_000).step_by(sounds_per_batch) {
        let batch_end = (batch_start + sounds_per_batch).min(156_000);
        let rows = (batch_start..batch_end)
            .map(|index| {
                let relative_path = format!("long/unicode/Шлях/Sound-{index:06}.flac");
                SoundRecord {
                    sound: sound(
                        &format!("sound-{index:06}"),
                        &format!("Sound {index:06}"),
                        &format!("/music/{relative_path}"),
                    ),
                    general_position: index,
                    locations: vec![SoundLocationRecord {
                        root_path: "/music".to_string(),
                        folder_relative_path: Some("long/unicode/Шлях".to_string()),
                        relative_path,
                    }],
                }
            })
            .collect();
        wait(store.apply_batch(LibraryBatch::Sounds(rows)));
        assert!(wait(
            store.apply_manual_memberships(
                (batch_start..batch_end)
                    .map(|index| ManualMembershipRecord {
                        tab_public_id: format!("tab-{}", index % 8),
                        sound_public_id: format!("sound-{index:06}"),
                        position: index / 8,
                    })
                    .collect(),
                Vec::new(),
            )
        ));
    }
    let import_elapsed = started.elapsed();
    assert!(import_elapsed < std::time::Duration::from_secs(30));
    assert_eq!(wait(store.count(LibraryScope::General, "")), 156_000);

    let mut slowest_query = std::time::Duration::ZERO;
    for (search, page) in [("", 0), ("", 609), ("155999", 0), ("99", 0)] {
        let query_started = std::time::Instant::now();
        let result = wait(store.page(LibraryScope::General, search, page));
        let elapsed = query_started.elapsed();
        slowest_query = slowest_query.max(elapsed);
        assert!(!result.sounds.is_empty());
        assert!(
            elapsed < std::time::Duration::from_millis(100),
            "General search={search:?} page={page} took {elapsed:?}"
        );
    }
    let query_started = std::time::Instant::now();
    let tabs = wait(store.manual_tabs(0));
    let elapsed = query_started.elapsed();
    slowest_query = slowest_query.max(elapsed);
    assert_eq!(tabs.total, 8);
    assert!(tabs.tabs.iter().all(|tab| tab.sound_count == 19_500));
    assert!(
        elapsed < std::time::Duration::from_millis(100),
        "manual tab counts took {elapsed:?}"
    );

    let stats_started = std::time::Instant::now();
    let stats = wait(store.stats());
    let stats_elapsed = stats_started.elapsed();
    slowest_query = slowest_query.max(stats_elapsed);
    assert_eq!(stats.sounds, 156_000);
    assert!(
        stats_elapsed < std::time::Duration::from_millis(100),
        "library stats recount took {stats_elapsed:?}"
    );

    let smaps = std::fs::read_to_string("/proc/self/smaps_rollup").expect("read smaps_rollup");
    let pss_kib = smaps
        .lines()
        .find_map(|line| line.strip_prefix("Pss:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<usize>().ok())
        .expect("parse PSS");
    eprintln!(
        "156k SQLite gate: import={import_elapsed:?}, slowest_query={slowest_query:?}, stats_recount={stats_elapsed:?}, pss={pss_kib} KiB"
    );
    assert!(pss_kib < 102_400, "store process PSS was {pss_kib} KiB");
    if let Some(output) = std::env::var_os("LSB_BENCHMARK_LIBRARY_OUT") {
        std::fs::copy(temp.path().join("library.sqlite3"), output)
            .expect("copy retained benchmark database");
    }
}

#[test]
#[ignore = "production-scale GUI folder-tree fixture generator"]
#[allow(clippy::print_stderr)]
fn benchmark_20k_wide_folder_tree_store() {
    const FOLDER_COUNT: usize = 20_000;
    const ROOT_PATH: &str = "/home/flinux/Музика";

    let temp = TestDir::new();
    let db_path = temp.path().join("library.sqlite3");

    {
        let store = LibraryStore::open(db_path.clone()).expect("open store");
        wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
            path: ROOT_PATH.to_string(),
            position: 0,
        }])));

        let started = std::time::Instant::now();

        let folders_per_batch = 500;
        for batch_start in (0..FOLDER_COUNT).step_by(folders_per_batch) {
            let batch_end = (batch_start + folders_per_batch).min(FOLDER_COUNT);
            let rows = (batch_start..batch_end)
                .map(|index| {
                    let name = format!("Album {index:05}");
                    FolderRecord {
                        root_path: ROOT_PATH.to_string(),
                        relative_path: name.clone(),
                        parent_relative_path: None,
                        name,
                        position: index,
                    }
                })
                .collect();
            wait(store.apply_batch(LibraryBatch::Folders(rows)));
        }

        wait(store.apply_batch(LibraryBatch::Folders(vec![
            FolderRecord {
                root_path: ROOT_PATH.to_string(),
                relative_path: "Album 00000/Disc 1".to_string(),
                parent_relative_path: Some("Album 00000".to_string()),
                name: "Disc 1".to_string(),
                position: 0,
            },
            FolderRecord {
                root_path: ROOT_PATH.to_string(),
                relative_path: "Album 00000/Disc 1/Bonus".to_string(),
                parent_relative_path: Some("Album 00000/Disc 1".to_string()),
                name: "Bonus".to_string(),
                position: 0,
            },
        ])));

        let sounds_per_batch = MAX_BATCH_ROWS / 2;
        for batch_start in (0..FOLDER_COUNT).step_by(sounds_per_batch) {
            let batch_end = (batch_start + sounds_per_batch).min(FOLDER_COUNT);
            let rows = (batch_start..batch_end)
                .map(|index| {
                    let folder = format!("Album {index:05}");
                    let relative_path = format!("{folder}/Track.flac");
                    SoundRecord {
                        sound: sound(
                            &format!("sound-{index:05}"),
                            &format!("Track {index:05}"),
                            &format!("{ROOT_PATH}/{relative_path}"),
                        ),
                        general_position: index,
                        locations: vec![SoundLocationRecord {
                            root_path: ROOT_PATH.to_string(),
                            folder_relative_path: Some(folder),
                            relative_path,
                        }],
                    }
                })
                .collect();
            wait(store.apply_batch(LibraryBatch::Sounds(rows)));
        }

        wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
            sound: sound(
                "sound-bonus",
                "Bonus Track",
                &format!("{ROOT_PATH}/Album 00000/Disc 1/Bonus/Bonus.flac"),
            ),
            general_position: FOLDER_COUNT,
            locations: vec![SoundLocationRecord {
                root_path: ROOT_PATH.to_string(),
                folder_relative_path: Some("Album 00000/Disc 1/Bonus".to_string()),
                relative_path: "Album 00000/Disc 1/Bonus/Bonus.flac".to_string(),
            }],
        }])));

        let import_elapsed = started.elapsed();

        let top = wait(store.folder_children(ROOT_PATH, None, 0));
        assert_eq!(top.total, FOLDER_COUNT);
        assert_eq!(top.folders.len(), PAGE_SIZE);

        let last_page = (FOLDER_COUNT - 1) / PAGE_SIZE;
        let last_page_rows = FOLDER_COUNT - last_page * PAGE_SIZE;
        let tail = wait(store.folder_children(ROOT_PATH, None, last_page));
        assert_eq!(tail.folders.len(), last_page_rows);

        let deep_page_started = std::time::Instant::now();
        let deep_page = wait(store.folder_children(ROOT_PATH, None, last_page));
        let deep_page_elapsed = deep_page_started.elapsed();
        assert_eq!(deep_page.folders.len(), last_page_rows);
        assert!(
            deep_page_elapsed < std::time::Duration::from_millis(2000),
            "deep folder page took {deep_page_elapsed:?}; the EXISTS subquery has likely lost its index (see folders_parent_order)"
        );

        let smaps = std::fs::read_to_string("/proc/self/smaps_rollup").expect("read smaps_rollup");
        let pss_kib = smaps
            .lines()
            .find_map(|line| line.strip_prefix("Pss:"))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<usize>().ok())
            .expect("parse PSS");
        eprintln!(
            "20k wide folder tree gate: import={import_elapsed:?}, deep_page={deep_page_elapsed:?}, pss={pss_kib} KiB"
        );
        assert!(pss_kib < 102_400, "store process PSS was {pss_kib} KiB");
    }
    {
        let connection = rusqlite::Connection::open(&db_path).expect("open raw connection");
        connection
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES('library_id', ?1)",
                [uuid::Uuid::new_v4().to_string()],
            )
            .expect("stamp library_id");
        connection
            .execute(
                "INSERT OR REPLACE INTO meta(key, value) VALUES('database_ready', '1')",
                [],
            )
            .expect("stamp database_ready");
    }

    if let Some(output) = std::env::var_os("LSB_BENCHMARK_LIBRARY_OUT") {
        std::fs::copy(&db_path, output).expect("copy retained benchmark database");
    }
}

#[test]
fn a_folder_can_be_reordered_to_an_arbitrary_slot_without_touching_other_preferences() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
        path: "/music".to_string(),
        position: 0,
    }])));
    wait(
        store.apply_batch(LibraryBatch::Folders(
            ["a", "b", "c", "d"]
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
    assert!(wait(store.set_folder_preferences(
        "/music",
        "d",
        Some("Renamed D"),
        None,
        true
    )));

    assert!(wait(store.reorder_folder("/music", "d", 1)));

    let order = |store: &LibraryStore| {
        wait(store.folder_children("/music", None, 0))
            .folders
            .iter()
            .map(|folder| folder.relative_path.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(order(&store), ["a", "d", "b", "c"]);

    let moved = wait(store.folder_children("/music", None, 0))
        .folders
        .into_iter()
        .find(|folder| folder.relative_path == "d")
        .expect("moved folder still present");
    assert_eq!(moved.name, "Renamed D");
    assert!(moved.expanded, "reordering must not collapse the folder");

    drop(store);
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("reopen store");
    assert_eq!(order(&store), ["a", "d", "b", "c"]);
}

#[test]
fn hiding_a_folder_removes_its_subtree_and_sounds_until_it_is_restored() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Roots(vec![RootRecord {
        path: "/music".to_string(),
        position: 0,
    }])));
    wait(store.apply_batch(LibraryBatch::Folders(vec![
        FolderRecord {
            root_path: "/music".to_string(),
            relative_path: "albums".to_string(),
            parent_relative_path: None,
            name: "albums".to_string(),
            position: 0,
        },
        FolderRecord {
            root_path: "/music".to_string(),
            relative_path: "albums/singles".to_string(),
            parent_relative_path: Some("albums".to_string()),
            name: "singles".to_string(),
            position: 0,
        },
        FolderRecord {
            root_path: "/music".to_string(),
            relative_path: "effects".to_string(),
            parent_relative_path: None,
            name: "effects".to_string(),
            position: 1,
        },
    ])));
    let located = |id: &str, folder: Option<&str>, general_position: usize| SoundRecord {
        sound: sound(id, id, &format!("/music/{id}.wav")),
        general_position,
        locations: vec![SoundLocationRecord {
            root_path: "/music".to_string(),
            folder_relative_path: folder.map(str::to_string),
            relative_path: format!("{id}.wav"),
        }],
    };
    wait(store.apply_batch(LibraryBatch::Sounds(vec![
        located("in-singles", Some("albums/singles"), 0),
        located("in-effects", Some("effects"), 1),
        located("in-root", None, 2),
    ])));

    let visible_ids = || {
        wait(store.page(LibraryScope::General, "", 0))
            .sounds
            .into_iter()
            .map(|sound| sound.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(visible_ids(), ["in-singles", "in-effects", "in-root"]);

    assert!(wait(store.set_folder_hidden("/music", "albums", true)));

    let top = wait(store.folder_children("/music", None, 0));
    assert_eq!(
        top.folders
            .iter()
            .map(|folder| folder.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["effects"]
    );
    assert_eq!(top.total, 1);

    assert_eq!(visible_ids(), ["in-effects", "in-root"]);

    let hidden = wait(store.hidden_folders(0));
    assert_eq!(hidden.total, 1);
    assert_eq!(hidden.folders[0].root_path, "/music");
    assert_eq!(hidden.folders[0].relative_path, "albums");

    assert!(wait(store.set_folder_hidden("/music", "albums", false)));
    assert_eq!(wait(store.hidden_folders(0)).total, 0);
    assert_eq!(visible_ids(), ["in-singles", "in-effects", "in-root"]);
    assert_eq!(wait(store.folder_children("/music", None, 0)).total, 2);
}

const SCHEMA_V4_SQL: &str = r#"BEGIN IMMEDIATE;
         CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE roots(
             id INTEGER PRIMARY KEY,
             path TEXT NOT NULL UNIQUE,
             position INTEGER NOT NULL,
             active_generation INTEGER NOT NULL DEFAULT 0 CHECK(active_generation >= 0)
         );
         CREATE TABLE folders(
             id INTEGER PRIMARY KEY,
             root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
             parent_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
             relative_path TEXT NOT NULL,
             name TEXT NOT NULL,
             position INTEGER NOT NULL,
             UNIQUE(root_id, relative_path)
         );
         CREATE INDEX folders_parent_order ON folders(root_id, parent_id, position, id);
         CREATE TABLE folder_presence(
             folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
             generation INTEGER NOT NULL CHECK(generation >= 0),
             PRIMARY KEY(folder_id, generation)
         );
         CREATE INDEX folder_presence_generation ON folder_presence(generation, folder_id);
         CREATE TABLE folder_closure(
             ancestor_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
             descendant_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
             depth INTEGER NOT NULL CHECK(depth >= 0),
             PRIMARY KEY(ancestor_id, descendant_id)
         );
         CREATE INDEX folder_closure_descendant ON folder_closure(descendant_id, ancestor_id);
         CREATE TABLE sounds(
             rowid INTEGER PRIMARY KEY,
             public_id TEXT NOT NULL UNIQUE,
             name TEXT NOT NULL,
             search_name TEXT NOT NULL,
             path TEXT NOT NULL UNIQUE,
             source_path TEXT,
             duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms >= 0),
             volume INTEGER NOT NULL CHECK(volume BETWEEN 0 AND 100),
             enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
             loudness_lufs REAL,
             loudness_state TEXT NOT NULL,
             loudness_confidence REAL,
             loudness_fingerprint TEXT,
             loudness_true_peak_dbtp REAL,
             general_position INTEGER NOT NULL,
             standalone INTEGER NOT NULL CHECK(standalone IN (0, 1))
         );
         CREATE INDEX sounds_general_order ON sounds(general_position, public_id);
         CREATE INDEX sounds_standalone ON sounds(rowid) WHERE standalone = 1;
         CREATE TABLE hotkey_bindings(
             binding_id TEXT PRIMARY KEY,
             sound_id INTEGER UNIQUE REFERENCES sounds(rowid) ON DELETE CASCADE,
             control_action TEXT UNIQUE,
             accelerator TEXT NOT NULL,
             normalized TEXT,
             state TEXT NOT NULL CHECK(state IN ('active', 'needs_attention')),
             issue TEXT,
             CHECK((sound_id IS NOT NULL) <> (control_action IS NOT NULL)),
             CHECK((state = 'active' AND normalized IS NOT NULL)
                   OR (state = 'needs_attention' AND normalized IS NULL))
         );
         CREATE UNIQUE INDEX hotkey_bindings_active_normalized
             ON hotkey_bindings(normalized)
             WHERE state = 'active';
         CREATE TABLE sound_locations(
             sound_id INTEGER NOT NULL REFERENCES sounds(rowid) ON DELETE CASCADE,
             root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
             generation INTEGER NOT NULL CHECK(generation >= 0),
             folder_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
             relative_path TEXT NOT NULL,
             PRIMARY KEY(sound_id, root_id, generation)
         );
         CREATE INDEX sound_locations_folder
             ON sound_locations(root_id, generation, folder_id, sound_id);
         CREATE TABLE manual_tabs(
             id INTEGER PRIMARY KEY,
             public_id TEXT NOT NULL UNIQUE,
             name TEXT NOT NULL,
             position INTEGER NOT NULL
         );
         CREATE TABLE manual_memberships(
             tab_id INTEGER NOT NULL REFERENCES manual_tabs(id) ON DELETE CASCADE,
             sound_id INTEGER NOT NULL REFERENCES sounds(rowid) ON DELETE CASCADE,
             position INTEGER NOT NULL,
             PRIMARY KEY(tab_id, sound_id)
         );
         CREATE INDEX manual_memberships_order ON manual_memberships(tab_id, position, sound_id);
         CREATE INDEX manual_memberships_sound ON manual_memberships(sound_id);
         CREATE TABLE legacy_generated_tabs(
             id INTEGER PRIMARY KEY,
             public_id TEXT NOT NULL UNIQUE,
             root_path TEXT NOT NULL,
             relative_path TEXT NOT NULL,
             name TEXT NOT NULL,
             position INTEGER NOT NULL
         );
         CREATE INDEX legacy_generated_tabs_root
             ON legacy_generated_tabs(root_path, relative_path, id);
         CREATE TABLE legacy_generated_memberships(
             tab_id INTEGER NOT NULL REFERENCES legacy_generated_tabs(id) ON DELETE CASCADE,
             sound_id INTEGER NOT NULL REFERENCES sounds(rowid) ON DELETE CASCADE,
             position INTEGER NOT NULL,
             PRIMARY KEY(tab_id, sound_id)
         );
         CREATE TABLE folder_prefs(
             folder_id INTEGER PRIMARY KEY REFERENCES folders(id) ON DELETE CASCADE,
             display_name TEXT,
             sibling_position INTEGER,
             expanded INTEGER NOT NULL DEFAULT 0 CHECK(expanded IN (0, 1)),
             hidden INTEGER NOT NULL DEFAULT 0 CHECK(hidden IN (0, 1))
         );
         CREATE TABLE folder_overrides(
             folder_id INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
             sound_id INTEGER NOT NULL REFERENCES sounds(rowid) ON DELETE CASCADE,
             action TEXT NOT NULL CHECK(action IN ('include', 'exclude')),
             PRIMARY KEY(folder_id, sound_id)
         );
         CREATE VIRTUAL TABLE sound_search USING fts5(
             search_name,
             content='sounds',
             content_rowid='rowid',
             tokenize='trigram'
         );
         CREATE TRIGGER sounds_search_insert AFTER INSERT ON sounds BEGIN
             INSERT INTO sound_search(rowid, search_name) VALUES(new.rowid, new.search_name);
         END;
         CREATE TRIGGER sounds_search_delete AFTER DELETE ON sounds BEGIN
             INSERT INTO sound_search(sound_search, rowid, search_name)
             VALUES('delete', old.rowid, old.search_name);
         END;
         CREATE TRIGGER sounds_search_update AFTER UPDATE OF search_name ON sounds BEGIN
             INSERT INTO sound_search(sound_search, rowid, search_name)
             VALUES('delete', old.rowid, old.search_name);
             INSERT INTO sound_search(rowid, search_name) VALUES(new.rowid, new.search_name);
         END;
         INSERT INTO meta(key, value) VALUES('schema_version', '4');
         INSERT INTO meta(key, value) VALUES('schema_flavor', 'bounded-generation-v4');
         PRAGMA user_version = 4;
         COMMIT;"#;

fn open_schema_four_database(path: &Path) {
    let connection = rusqlite::Connection::open(path).expect("create schema four database");
    connection
        .execute_batch(SCHEMA_V4_SQL)
        .expect("seed schema four database");
    connection
        .execute_batch(
            "INSERT INTO sounds(
                 public_id, name, search_name, path, source_path, duration_ms, volume,
                 enabled, loudness_lufs, loudness_state, loudness_confidence,
                 loudness_fingerprint, loudness_true_peak_dbtp, general_position, standalone
             ) VALUES
                 ('sound-one', 'One', 'one', '/a.wav', NULL, NULL, 100, 1, NULL,
                  'pending', NULL, NULL, NULL, 0, 1),
                 ('sound-two', 'Two', 'two', '/b.wav', NULL, NULL, 100, 1, NULL,
                  'pending', NULL, NULL, NULL, 1, 1);
             INSERT INTO hotkey_bindings(
                 binding_id, sound_id, control_action, accelerator, normalized, state, issue
             ) VALUES
                 ('sound-one', 1, NULL, 'Ctrl+KeyA', 'ctrl+keya', 'active', NULL),
                 ('control:stop_all', NULL, 'stop_all', 'Alt+KeyB', 'alt+keyb', 'active', NULL),
                 ('sound-two', 2, NULL, 'Ctrl+KeyC', NULL, 'needs_attention',
                  'duplicate legacy binding');",
        )
        .expect("seed schema four bindings");
}

#[test]
fn schema_four_migration_preserves_every_binding() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    open_schema_four_database(&path);

    let store = LibraryStore::open(path.clone()).expect("migrate schema four database");
    drop(store);

    let connection = rusqlite::Connection::open(&path).expect("inspect migrated database");
    type BindingRow = (
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let rows: Vec<BindingRow> = connection
        .prepare(
            "SELECT binding_id, accelerator, normalized, state, issue, tab_scope, target_tab
             FROM hotkey_bindings ORDER BY binding_id",
        )
        .expect("prepare migrated bindings")
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .expect("read migrated bindings")
        .collect::<Result<_, _>>()
        .expect("collect migrated bindings");

    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].0, "control:stop_all");
    assert_eq!(rows[0].2.as_deref(), Some("alt+keyb"));
    assert_eq!(rows[1].0, "sound-one");
    assert_eq!(rows[1].1, "Ctrl+KeyA");
    assert_eq!(rows[1].3, "active");

    assert_eq!(rows[2].0, "sound-two");
    assert_eq!(rows[2].2, None);
    assert_eq!(rows[2].3, "needs_attention");
    assert_eq!(rows[2].4.as_deref(), Some("duplicate legacy binding"));

    for row in &rows {
        assert_eq!(row.5, None, "{} gained a tab scope", row.0);
        assert_eq!(row.6, None, "{} gained a tab target", row.0);
    }

    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("read migrated schema version");
    assert_eq!(version, crate::library_store::DATABASE_SCHEMA_VERSION);
    let flavor: String = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_flavor'",
            [],
            |row| row.get(0),
        )
        .expect("read migrated flavor");
    assert_eq!(flavor, crate::library_store::DATABASE_SCHEMA_FLAVOR);
}

#[test]
fn a_migrated_database_matches_a_freshly_created_one() {
    let temp = TestDir::new();
    let migrated_path = temp.path().join("migrated.sqlite3");
    open_schema_four_database(&migrated_path);
    drop(LibraryStore::open(migrated_path.clone()).expect("migrate schema four database"));

    let fresh_path = temp.path().join("fresh.sqlite3");
    drop(LibraryStore::open(fresh_path.clone()).expect("create fresh database"));

    let read_schema = |path: &Path| -> Vec<String> {
        let connection = rusqlite::Connection::open(path).expect("open database");
        let mut statement = connection
            .prepare(
                "SELECT sql FROM sqlite_master
                 WHERE tbl_name = 'hotkey_bindings' AND sql IS NOT NULL
                 ORDER BY type, name",
            )
            .expect("prepare schema read");
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("read schema")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect schema");
        rows
    };

    assert_eq!(read_schema(&migrated_path), read_schema(&fresh_path));
}

#[test]
fn one_chord_may_be_shared_by_several_sounds() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    open_schema_four_database(&path);
    drop(LibraryStore::open(path.clone()).expect("migrate schema four database"));

    let connection = rusqlite::Connection::open(&path).expect("open migrated database");
    connection
        .execute_batch(
            "UPDATE hotkey_bindings
                SET normalized = 'ctrl+keya', state = 'active', issue = NULL,
                    tab_scope = 'tab:party'
              WHERE binding_id = 'sound-two';",
        )
        .expect("share one chord between two sounds");

    let shared: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM hotkey_bindings
              WHERE normalized = 'ctrl+keya' AND state = 'active'",
            [],
            |row| row.get(0),
        )
        .expect("count shared bindings");
    assert_eq!(shared, 2);
}

#[test]
fn a_tab_may_only_be_bound_once() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    open_schema_four_database(&path);
    drop(LibraryStore::open(path.clone()).expect("migrate schema four database"));

    let connection = rusqlite::Connection::open(&path).expect("open migrated database");
    connection
        .execute_batch(
            "INSERT INTO hotkey_bindings(
                 binding_id, target_tab, accelerator, normalized, state
             ) VALUES('tab:party', 'tab:party', 'Ctrl+Digit1', 'ctrl+digit1', 'active');",
        )
        .expect("bind a tab");

    let second = connection.execute_batch(
        "INSERT INTO hotkey_bindings(
             binding_id, target_tab, accelerator, normalized, state
         ) VALUES('tab:party-again', 'tab:party', 'Ctrl+Digit2', 'ctrl+digit2', 'active');",
    );
    assert!(second.is_err(), "a tab must not carry two hotkeys");
}

#[test]
fn a_binding_owns_exactly_one_target() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    open_schema_four_database(&path);
    drop(LibraryStore::open(path.clone()).expect("migrate schema four database"));

    let connection = rusqlite::Connection::open(&path).expect("open migrated database");
    let both = connection.execute_batch(
        "INSERT INTO hotkey_bindings(
             binding_id, sound_id, target_tab, accelerator, normalized, state
         ) VALUES('mixed', 1, 'tab:party', 'Ctrl+Digit3', 'ctrl+digit3', 'active');",
    );
    assert!(both.is_err(), "a binding must not own a sound and a tab");

    let neither = connection.execute_batch(
        "INSERT INTO hotkey_bindings(
             binding_id, accelerator, normalized, state
         ) VALUES('empty', 'Ctrl+Digit4', 'ctrl+digit4', 'active');",
    );
    assert!(neither.is_err(), "a binding must own something");
}

#[test]
fn a_tab_binding_is_live_in_every_tab() {
    let temp = TestDir::new();
    let path = temp.path().join("library.sqlite3");
    open_schema_four_database(&path);
    drop(LibraryStore::open(path.clone()).expect("migrate schema four database"));

    let connection = rusqlite::Connection::open(&path).expect("open migrated database");
    let scoped = connection.execute_batch(
        "INSERT INTO hotkey_bindings(
             binding_id, target_tab, tab_scope, accelerator, normalized, state
         ) VALUES('tab:party', 'tab:party', 'tab:other', 'Ctrl+Digit5', 'ctrl+digit5', 'active');",
    );
    assert!(
        scoped.is_err(),
        "a tab hotkey must stay reachable from every tab"
    );
}

#[test]
fn several_sounds_on_one_chord_project_one_entry() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![
        SoundRecord {
            sound: sound("first", "First", "/music/first.flac"),
            general_position: 0,
            locations: Vec::new(),
        },
        SoundRecord {
            sound: sound("second", "Second", "/music/second.flac"),
            general_position: 1,
            locations: Vec::new(),
        },
    ])));
    for id in ["first", "second"] {
        assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
            binding_id: id.to_string(),
            owner: HotkeyBindingOwner::Sound(id.to_string()),
            accelerator: "Ctrl+KeyA".to_string(),
            normalized: Some("Ctrl+KeyA".to_string()),
            issue: None,
            tab_scope: None,
        })));
    }

    let page = wait(store.hotkey_bindings_after(None));
    assert_eq!(page.bindings.len(), 1);
    assert_eq!(page.bindings[0].normalized.as_deref(), Some("Ctrl+KeyA"));
}

#[test]
fn a_hotkey_group_lists_every_sound_on_the_chord() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![
        SoundRecord {
            sound: sound("second", "Second", "/music/second.flac"),
            general_position: 1,
            locations: Vec::new(),
        },
        SoundRecord {
            sound: sound("first", "First", "/music/first.flac"),
            general_position: 0,
            locations: Vec::new(),
        },
    ])));
    for id in ["first", "second"] {
        assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
            binding_id: id.to_string(),
            owner: HotkeyBindingOwner::Sound(id.to_string()),
            accelerator: "Ctrl+KeyA".to_string(),
            normalized: Some("Ctrl+KeyA".to_string()),
            issue: None,
            tab_scope: None,
        })));
    }

    let members = wait(store.hotkey_group("first"));
    let ids: Vec<&str> = members.iter().map(|m| m.sound_id.as_str()).collect();
    assert_eq!(ids, ["first", "second"], "members follow library order");
    assert!(members.iter().all(|member| member.tab_scope.is_none()));

    assert_eq!(wait(store.hotkey_group("second")), members);
}

#[test]
fn an_unshared_chord_is_a_group_of_one() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: sound("first", "First", "/music/first.flac"),
        general_position: 0,
        locations: Vec::new(),
    }])));
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "first".to_string(),
        owner: HotkeyBindingOwner::Sound("first".to_string()),
        accelerator: "Ctrl+KeyA".to_string(),
        normalized: Some("Ctrl+KeyA".to_string()),
        issue: None,
        tab_scope: None,
    })));

    let members = wait(store.hotkey_group("first"));
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].sound_id, "first");
}

#[test]
fn a_control_binding_represents_a_chord_it_shares() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: sound("first", "First", "/music/first.flac"),
        general_position: 0,
        locations: Vec::new(),
    }])));
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "first".to_string(),
        owner: HotkeyBindingOwner::Sound("first".to_string()),
        accelerator: "Ctrl+KeyA".to_string(),
        normalized: Some("Ctrl+KeyA".to_string()),
        issue: None,
        tab_scope: None,
    })));
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "control:stop_all".to_string(),
        owner: HotkeyBindingOwner::Control("stop_all".to_string()),
        accelerator: "Ctrl+KeyA".to_string(),
        normalized: Some("Ctrl+KeyA".to_string()),
        issue: None,
        tab_scope: None,
    })));

    let page = wait(store.hotkey_bindings_after(None));
    assert_eq!(page.bindings.len(), 1);
    assert_eq!(page.bindings[0].binding_id, "control:stop_all");
}

#[test]
fn every_kind_of_tab_has_a_scope_key() {
    use crate::library_store::scope_key;

    assert_eq!(scope_key(&LibraryScope::General), "general");
    assert_eq!(
        scope_key(&LibraryScope::ManualTab("party".to_string())),
        "tab:party"
    );
    let nested = scope_key(&LibraryScope::Folder {
        root_path: "/music".to_string(),
        relative_path: "memes/loud".to_string(),
    });
    let ambiguous = scope_key(&LibraryScope::Folder {
        root_path: "/music/memes".to_string(),
        relative_path: "loud".to_string(),
    });
    assert_ne!(nested, ambiguous);
    assert!(nested.starts_with("folder:"));
}

#[test]
fn a_hotkey_bound_in_one_tab_is_not_shown_in_another() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    let mut shared = sound("shared", "Shared", "/music/shared.flac");
    shared.hotkey = None;
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: shared,
        general_position: 0,
        locations: Vec::new(),
    }])));
    wait(store.apply_batch(LibraryBatch::ManualTabs(vec![
        ManualTabRecord {
            public_id: "one".to_string(),
            name: "One".to_string(),
            position: 0,
        },
        ManualTabRecord {
            public_id: "two".to_string(),
            name: "Two".to_string(),
            position: 1,
        },
    ])));
    wait(store.apply_batch(LibraryBatch::ManualMemberships(vec![
        ManualMembershipRecord {
            tab_public_id: "one".to_string(),
            sound_public_id: "shared".to_string(),
            position: 0,
        },
        ManualMembershipRecord {
            tab_public_id: "two".to_string(),
            sound_public_id: "shared".to_string(),
            position: 0,
        },
    ])));
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "binding-in-one".to_string(),
        owner: HotkeyBindingOwner::Sound("shared".to_string()),
        accelerator: "Ctrl+KeyA".to_string(),
        normalized: Some("Ctrl+KeyA".to_string()),
        issue: None,
        tab_scope: Some("tab:one".to_string()),
    })));

    let in_one = wait(store.page(LibraryScope::ManualTab("one".to_string()), "", 0));
    assert_eq!(in_one.sounds[0].hotkey.as_deref(), Some("Ctrl+KeyA"));

    let in_two = wait(store.page(LibraryScope::ManualTab("two".to_string()), "", 0));
    assert_eq!(in_two.sounds[0].hotkey, None);

    let in_general = wait(store.page(LibraryScope::General, "", 0));
    assert_eq!(in_general.sounds[0].hotkey, None);
}

#[test]
fn a_binding_that_is_live_everywhere_shows_in_every_tab() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    let mut shared = sound("shared", "Shared", "/music/shared.flac");
    shared.hotkey = None;
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: shared,
        general_position: 0,
        locations: Vec::new(),
    }])));
    wait(
        store.apply_batch(LibraryBatch::ManualTabs(vec![ManualTabRecord {
            public_id: "one".to_string(),
            name: "One".to_string(),
            position: 0,
        }])),
    );
    wait(store.apply_batch(LibraryBatch::ManualMemberships(vec![
        ManualMembershipRecord {
            tab_public_id: "one".to_string(),
            sound_public_id: "shared".to_string(),
            position: 0,
        },
    ])));
    assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
        binding_id: "everywhere".to_string(),
        owner: HotkeyBindingOwner::Sound("shared".to_string()),
        accelerator: "Ctrl+KeyB".to_string(),
        normalized: Some("Ctrl+KeyB".to_string()),
        issue: None,
        tab_scope: None,
    })));

    for scope in [
        LibraryScope::General,
        LibraryScope::ManualTab("one".to_string()),
    ] {
        let page = wait(store.page(scope, "", 0));
        assert_eq!(page.sounds[0].hotkey.as_deref(), Some("Ctrl+KeyB"));
    }
}

#[test]
fn a_tab_binding_wins_over_one_that_is_live_everywhere() {
    let temp = TestDir::new();
    let store = LibraryStore::open(temp.path().join("library.sqlite3")).expect("open store");

    let mut shared = sound("shared", "Shared", "/music/shared.flac");
    shared.hotkey = None;
    wait(store.apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
        sound: shared,
        general_position: 0,
        locations: Vec::new(),
    }])));
    wait(
        store.apply_batch(LibraryBatch::ManualTabs(vec![ManualTabRecord {
            public_id: "one".to_string(),
            name: "One".to_string(),
            position: 0,
        }])),
    );
    wait(store.apply_batch(LibraryBatch::ManualMemberships(vec![
        ManualMembershipRecord {
            tab_public_id: "one".to_string(),
            sound_public_id: "shared".to_string(),
            position: 0,
        },
    ])));
    for (binding_id, accelerator, scope) in [
        ("everywhere", "Ctrl+KeyB", None),
        ("in-one", "Ctrl+KeyC", Some("tab:one".to_string())),
    ] {
        assert!(wait(store.set_hotkey_binding(HotkeyBindingRecord {
            binding_id: binding_id.to_string(),
            owner: HotkeyBindingOwner::Sound("shared".to_string()),
            accelerator: accelerator.to_string(),
            normalized: Some(accelerator.to_string()),
            issue: None,
            tab_scope: scope,
        })));
    }

    let page = wait(store.page(LibraryScope::ManualTab("one".to_string()), "", 0));
    assert_eq!(page.sounds[0].hotkey.as_deref(), Some("Ctrl+KeyC"));
}
