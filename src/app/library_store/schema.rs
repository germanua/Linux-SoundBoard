fn open_connection(path: &Path) -> Result<Connection, LibraryError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
    }
    let existed = path.exists();
    if existed {
        let metadata = std::fs::symlink_metadata(path)
            .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
        if !metadata.file_type().is_file() {
            return Err(LibraryError::InvalidData(format!(
                "library database '{}' is not a regular file",
                path.display()
            )));
        }
    }
    let connection = Connection::open(path)?;
    let metadata =
        std::fs::metadata(path).map_err(|error| LibraryError::InvalidData(error.to_string()))?;
    let mode = if existed {
        metadata.permissions().mode() & 0o600
    } else {
        0o600
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if schema_version != 0 && application_id != 0 && application_id != DATABASE_APPLICATION_ID {
        return Err(LibraryError::InvalidData(format!(
            "library database application id is {application_id}, expected {DATABASE_APPLICATION_ID}"
        )));
    }
    if schema_version > DATABASE_SCHEMA_VERSION {
        return Err(LibraryError::InvalidData(format!(
            "library schema {schema_version} is newer than supported schema {DATABASE_SCHEMA_VERSION}"
        )));
    }
    connection.pragma_update(None, "journal_mode", "DELETE")?;
    connection.pragma_update(None, "synchronous", "EXTRA")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "cache_size", -2048_i64)?;
    connection.pragma_update(None, "temp_store", "FILE")?;
    connection.pragma_update(None, "mmap_size", 0_i64)?;
    connection.pragma_update(None, "journal_size_limit", 0_i64)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    if schema_version == 0 {
        create_schema(&connection)?;
    } else if schema_version == 1 {
        migrate_schema_1_to_2(&connection)?;
        migrate_schema_2_to_3(&connection)?;
        migrate_schema_3_to_4(&connection)?;
        migrate_schema_4_to_5(&connection)?;
        migrate_schema_5_to_6(&connection)?;
    } else if schema_version == 2 {
        migrate_schema_2_to_3(&connection)?;
        migrate_schema_3_to_4(&connection)?;
        migrate_schema_4_to_5(&connection)?;
        migrate_schema_5_to_6(&connection)?;
    } else if schema_version == 3 {
        migrate_schema_3_to_4(&connection)?;
        migrate_schema_4_to_5(&connection)?;
        migrate_schema_5_to_6(&connection)?;
    } else if schema_version == 4 {
        migrate_schema_4_to_5(&connection)?;
        migrate_schema_5_to_6(&connection)?;
    } else if schema_version == 5 {
        migrate_schema_5_to_6(&connection)?;
    } else {
        let meta_version: String = connection.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        let flavor: String = connection.query_row(
            "SELECT value FROM meta WHERE key = 'schema_flavor'",
            [],
            |row| row.get(0),
        )?;
        if meta_version != DATABASE_SCHEMA_VERSION.to_string() || flavor != DATABASE_SCHEMA_FLAVOR {
            return Err(LibraryError::InvalidData(
                "library metadata does not match the bounded schema".to_string(),
            ));
        }
    }
    if application_id == 0 {
        connection.pragma_update(None, "application_id", DATABASE_APPLICATION_ID)?;
    }
    connection.execute_batch("PRAGMA optimize;")?;
    Ok(connection)
}

const HOTKEY_BINDINGS_SCHEMA: &str = "\
         CREATE TABLE hotkey_bindings(
             binding_id TEXT PRIMARY KEY,
             sound_id INTEGER REFERENCES sounds(rowid) ON DELETE CASCADE,
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
         CREATE UNIQUE INDEX hotkey_bindings_sound_scope
             ON hotkey_bindings(sound_id, IFNULL(tab_scope, ''))
             WHERE sound_id IS NOT NULL;
         CREATE UNIQUE INDEX hotkey_bindings_target_tab
             ON hotkey_bindings(target_tab)
             WHERE target_tab IS NOT NULL;";

fn create_schema(connection: &Connection) -> Result<(), LibraryError> {
    connection.pragma_update(None, "application_id", DATABASE_APPLICATION_ID)?;
    connection.execute_batch(&format!(
        "BEGIN IMMEDIATE;
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
{HOTKEY_BINDINGS_SCHEMA}
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
         INSERT INTO meta(key, value) VALUES('schema_version', '6');
         INSERT INTO meta(key, value) VALUES('schema_flavor', 'bounded-generation-v6');
         PRAGMA user_version = 6;
         COMMIT;",
    ))?;
    Ok(())
}

fn migrate_schema_1_to_2(connection: &Connection) -> Result<(), LibraryError> {
    let flavor: String = connection.query_row(
        "SELECT value FROM meta WHERE key = 'schema_flavor'",
        [],
        |row| row.get(0),
    )?;
    if flavor != "bounded-generation-v1" {
        return Err(LibraryError::InvalidData(
            "library metadata does not match schema 1".to_string(),
        ));
    }
    connection.execute_batch(
        "BEGIN IMMEDIATE;
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
         INSERT INTO hotkey_bindings(
             binding_id, sound_id, accelerator, normalized, state, issue
         )
         SELECT public_id, rowid, hotkey,
                CASE WHEN COUNT(*) OVER (PARTITION BY lower(trim(hotkey))) = 1
                     THEN lower(trim(hotkey)) END,
                CASE WHEN COUNT(*) OVER (PARTITION BY lower(trim(hotkey))) = 1
                     THEN 'active' ELSE 'needs_attention' END,
                CASE WHEN COUNT(*) OVER (PARTITION BY lower(trim(hotkey))) > 1
                     THEN 'duplicate legacy binding' END
         FROM sounds WHERE hotkey IS NOT NULL AND trim(hotkey) <> '';
         CREATE UNIQUE INDEX hotkey_bindings_active_normalized
             ON hotkey_bindings(normalized)
             WHERE state = 'active';
         DROP INDEX sounds_hotkey;
         ALTER TABLE sounds DROP COLUMN hotkey;
         UPDATE meta SET value = '2' WHERE key = 'schema_version';
         UPDATE meta SET value = 'bounded-generation-v2' WHERE key = 'schema_flavor';
         PRAGMA user_version = 2;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_schema_2_to_3(connection: &Connection) -> Result<(), LibraryError> {
    let flavor: String = connection.query_row(
        "SELECT value FROM meta WHERE key = 'schema_flavor'",
        [],
        |row| row.get(0),
    )?;
    if flavor != "bounded-generation-v2" {
        return Err(LibraryError::InvalidData(
            "library metadata does not match schema 2".to_string(),
        ));
    }
    connection.execute_batch(
        "BEGIN IMMEDIATE;
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
         UPDATE meta SET value = '3' WHERE key = 'schema_version';
         UPDATE meta SET value = 'bounded-generation-v3' WHERE key = 'schema_flavor';
         PRAGMA user_version = 3;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_schema_3_to_4(connection: &Connection) -> Result<(), LibraryError> {
    let flavor: String = connection.query_row(
        "SELECT value FROM meta WHERE key = 'schema_flavor'",
        [],
        |row| row.get(0),
    )?;
    if flavor != "bounded-generation-v3" {
        return Err(LibraryError::InvalidData(
            "library metadata does not match schema 3".to_string(),
        ));
    }
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         ALTER TABLE folder_prefs
             ADD COLUMN hidden INTEGER NOT NULL DEFAULT 0 CHECK(hidden IN (0, 1));
         UPDATE meta SET value = '4' WHERE key = 'schema_version';
         UPDATE meta SET value = 'bounded-generation-v4' WHERE key = 'schema_flavor';
         PRAGMA user_version = 4;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_schema_4_to_5(connection: &Connection) -> Result<(), LibraryError> {
    let flavor: String = connection.query_row(
        "SELECT value FROM meta WHERE key = 'schema_flavor'",
        [],
        |row| row.get(0),
    )?;
    if flavor != "bounded-generation-v4" {
        return Err(LibraryError::InvalidData(
            "library metadata does not match schema 4".to_string(),
        ));
    }
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         ALTER TABLE hotkey_bindings RENAME TO hotkey_bindings_v4;
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
             binding_id, sound_id, control_action, accelerator, normalized, state, issue
         )
         SELECT binding_id, sound_id, control_action, accelerator, normalized, state, issue
         FROM hotkey_bindings_v4;
         DROP TABLE hotkey_bindings_v4;
         UPDATE meta SET value = '5' WHERE key = 'schema_version';
         UPDATE meta SET value = 'bounded-generation-v5' WHERE key = 'schema_flavor';
         PRAGMA user_version = 5;
         COMMIT;",
    )?;
    Ok(())
}

fn migrate_schema_5_to_6(connection: &Connection) -> Result<(), LibraryError> {
    let flavor: String = connection.query_row(
        "SELECT value FROM meta WHERE key = 'schema_flavor'",
        [],
        |row| row.get(0),
    )?;
    if flavor != "bounded-generation-v5" {
        return Err(LibraryError::InvalidData(
            "library metadata does not match schema 5".to_string(),
        ));
    }
    connection.execute_batch(&format!(
        "BEGIN IMMEDIATE;
         ALTER TABLE hotkey_bindings RENAME TO hotkey_bindings_v5;
         -- A renamed table keeps its indexes, and schema 5 already used these
         -- names, so they have to go before the new table claims them.
         DROP INDEX hotkey_bindings_active_lookup;
         DROP INDEX hotkey_bindings_target_tab;
         {HOTKEY_BINDINGS_SCHEMA}
         INSERT INTO hotkey_bindings(
             binding_id, sound_id, control_action, target_tab, tab_scope,
             accelerator, normalized, state, issue
         )
         SELECT binding_id, sound_id, control_action, target_tab, tab_scope,
                accelerator, normalized, state, issue
         FROM hotkey_bindings_v5;
         DROP TABLE hotkey_bindings_v5;
         UPDATE meta SET value = '6' WHERE key = 'schema_version';
         UPDATE meta SET value = 'bounded-generation-v6' WHERE key = 'schema_flavor';
         PRAGMA user_version = 6;
         COMMIT;",
    ))?;
    Ok(())
}
