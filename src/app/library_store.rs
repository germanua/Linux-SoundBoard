use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use crate::config::{LoudnessAnalysisState, Sound, SoundTab};

pub const PAGE_SIZE: usize = 256;
const DATABASE_SCHEMA_VERSION: i64 = 1;
const INTERACTIVE_QUEUE_CAPACITY: usize = 64;
const BULK_QUEUE_CAPACITY: usize = 2;

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error("library database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("library worker is unavailable")]
    WorkerUnavailable,
    #[error("library worker queue is full")]
    QueueFull,
    #[error("invalid library data: {0}")]
    InvalidData(String),
}

pub struct LibraryResponse<T>(mpsc::Receiver<Result<T, LibraryError>>);

impl<T> LibraryResponse<T> {
    pub fn recv(self) -> Result<T, LibraryError> {
        self.0.recv().map_err(|_| LibraryError::WorkerUnavailable)?
    }

    fn ready(result: Result<T, LibraryError>) -> Self {
        let (sender, receiver) = mpsc::sync_channel(1);
        let _ = sender.send(result);
        Self(receiver)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryScope {
    General,
    Tab(String),
}

#[derive(Debug)]
pub struct SoundPage {
    pub total: usize,
    pub sounds: Vec<Sound>,
}

#[derive(Debug)]
pub struct LibrarySnapshot {
    pub sound_folders: Vec<String>,
    pub sounds: Vec<Sound>,
    pub tabs: Vec<SoundTab>,
}

enum InteractiveRequest {
    Count {
        scope: LibraryScope,
        search: String,
        reply: mpsc::SyncSender<Result<usize, LibraryError>>,
    },
    Page {
        scope: LibraryScope,
        search: String,
        page: usize,
        reply: mpsc::SyncSender<Result<SoundPage, LibraryError>>,
    },
    SoundById {
        id: String,
        reply: mpsc::SyncSender<Result<Option<Sound>, LibraryError>>,
    },
}

enum BulkRequest {
    ReplaceAll {
        snapshot: LibrarySnapshot,
        reply: mpsc::SyncSender<Result<(), LibraryError>>,
    },
}

struct LibraryStoreInner {
    interactive: mpsc::SyncSender<InteractiveRequest>,
    bulk: mpsc::SyncSender<BulkRequest>,
    shutdown: Arc<AtomicBool>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Drop for LibraryStoreInner {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.lock().expect("library worker lock").take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
pub struct LibraryStore(Arc<LibraryStoreInner>);

impl LibraryStore {
    pub fn open(path: PathBuf) -> Result<Self, LibraryError> {
        let (interactive_tx, interactive_rx) = mpsc::sync_channel(INTERACTIVE_QUEUE_CAPACITY);
        let (bulk_tx, bulk_rx) = mpsc::sync_channel(BULK_QUEUE_CAPACITY);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker = thread::Builder::new()
            .name("library-db".to_string())
            .spawn(move || {
                let mut connection = match open_connection(&path) {
                    Ok(connection) => {
                        let _ = ready_tx.send(Ok(()));
                        connection
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                run_worker(&mut connection, interactive_rx, bulk_rx, &worker_shutdown);
            })
            .map_err(|_| LibraryError::WorkerUnavailable)?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self(Arc::new(LibraryStoreInner {
                interactive: interactive_tx,
                bulk: bulk_tx,
                shutdown,
                worker: Mutex::new(Some(worker)),
            }))),
            Ok(Err(error)) => {
                let _ = worker.join();
                Err(LibraryError::InvalidData(error))
            }
            Err(_) => {
                let _ = worker.join();
                Err(LibraryError::WorkerUnavailable)
            }
        }
    }

    pub fn replace_all(&self, snapshot: LibrarySnapshot) -> LibraryResponse<()> {
        let (reply, response) = mpsc::sync_channel(1);
        let request = BulkRequest::ReplaceAll { snapshot, reply };
        match self.0.bulk.try_send(request) {
            Ok(()) => LibraryResponse(response),
            Err(mpsc::TrySendError::Full(_)) => {
                LibraryResponse::ready(Err(LibraryError::QueueFull))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                LibraryResponse::ready(Err(LibraryError::WorkerUnavailable))
            }
        }
    }

    pub fn count(&self, scope: LibraryScope, search: &str) -> LibraryResponse<usize> {
        let (reply, response) = mpsc::sync_channel(1);
        let request = InteractiveRequest::Count {
            scope,
            search: search.to_lowercase(),
            reply,
        };
        match self.0.interactive.try_send(request) {
            Ok(()) => LibraryResponse(response),
            Err(mpsc::TrySendError::Full(_)) => {
                LibraryResponse::ready(Err(LibraryError::QueueFull))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                LibraryResponse::ready(Err(LibraryError::WorkerUnavailable))
            }
        }
    }

    pub fn page(
        &self,
        scope: LibraryScope,
        search: &str,
        page: usize,
    ) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        let request = InteractiveRequest::Page {
            scope,
            search: search.to_lowercase(),
            page,
            reply,
        };
        match self.0.interactive.try_send(request) {
            Ok(()) => LibraryResponse(response),
            Err(mpsc::TrySendError::Full(_)) => {
                LibraryResponse::ready(Err(LibraryError::QueueFull))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                LibraryResponse::ready(Err(LibraryError::WorkerUnavailable))
            }
        }
    }

    pub fn sound_by_id(&self, id: &str) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        let request = InteractiveRequest::SoundById {
            id: id.to_string(),
            reply,
        };
        match self.0.interactive.try_send(request) {
            Ok(()) => LibraryResponse(response),
            Err(mpsc::TrySendError::Full(_)) => {
                LibraryResponse::ready(Err(LibraryError::QueueFull))
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                LibraryResponse::ready(Err(LibraryError::WorkerUnavailable))
            }
        }
    }
}

fn run_worker(
    connection: &mut Connection,
    interactive: mpsc::Receiver<InteractiveRequest>,
    bulk: mpsc::Receiver<BulkRequest>,
    shutdown: &AtomicBool,
) {
    while !shutdown.load(Ordering::Acquire) {
        let mut handled_interactive = false;
        for _ in 0..INTERACTIVE_QUEUE_CAPACITY {
            let Ok(request) = interactive.try_recv() else {
                break;
            };
            handle_interactive(connection, request);
            handled_interactive = true;
        }

        if let Ok(request) = bulk.try_recv() {
            handle_bulk(connection, request);
            continue;
        }
        if handled_interactive {
            continue;
        }

        match interactive.recv_timeout(Duration::from_millis(5)) {
            Ok(request) => handle_interactive(connection, request),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if bulk.try_recv().is_err() {
                    break;
                }
            }
        }
    }
}

fn handle_interactive(connection: &Connection, request: InteractiveRequest) {
    match request {
        InteractiveRequest::Count {
            scope,
            search,
            reply,
        } => {
            let _ = reply.send(count_sounds(connection, &scope, &search));
        }
        InteractiveRequest::Page {
            scope,
            search,
            page,
            reply,
        } => {
            let _ = reply.send(load_page(connection, &scope, &search, page));
        }
        InteractiveRequest::SoundById { id, reply } => {
            let _ = reply.send(load_sound(connection, &id));
        }
    }
}

fn handle_bulk(connection: &mut Connection, request: BulkRequest) {
    match request {
        BulkRequest::ReplaceAll { snapshot, reply } => {
            let _ = reply.send(replace_all(connection, snapshot));
        }
    }
}

fn open_connection(path: &Path) -> Result<Connection, LibraryError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
    }
    let connection = Connection::open(path)?;
    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if schema_version > DATABASE_SCHEMA_VERSION {
        return Err(LibraryError::InvalidData(format!(
            "library schema {schema_version} is newer than supported schema {DATABASE_SCHEMA_VERSION}"
        )));
    }
    connection.pragma_update(None, "journal_mode", "DELETE")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "cache_size", -2048_i64)?;
    connection.pragma_update(None, "temp_store", "FILE")?;
    connection.busy_timeout(Duration::from_secs(5))?;
    if schema_version == 0 {
        connection.execute_batch(
            "BEGIN IMMEDIATE;
         CREATE TABLE meta (
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );
         CREATE TABLE roots (
             path TEXT PRIMARY KEY,
             position INTEGER NOT NULL
         );
         CREATE TABLE sounds (
             id TEXT PRIMARY KEY,
             name TEXT NOT NULL,
             search_name TEXT NOT NULL,
             path TEXT NOT NULL UNIQUE,
             source_path TEXT,
             hotkey TEXT,
             duration_ms INTEGER,
             volume INTEGER NOT NULL CHECK(volume BETWEEN 0 AND 100),
             enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
             loudness_lufs REAL,
             loudness_state TEXT NOT NULL,
             loudness_confidence REAL,
             loudness_fingerprint TEXT,
             loudness_true_peak_dbtp REAL,
             root_folder TEXT,
             relative_path TEXT,
             position INTEGER NOT NULL
         );
         CREATE INDEX sounds_search_order
             ON sounds(search_name, id);
         CREATE TABLE tabs (
             id TEXT PRIMARY KEY,
             name TEXT NOT NULL,
             position INTEGER NOT NULL,
             kind TEXT NOT NULL CHECK(kind IN ('manual', 'generated')),
             root_folder TEXT,
             relative_folder TEXT
         );
         CREATE TABLE tab_memberships (
             tab_id TEXT NOT NULL REFERENCES tabs(id) ON DELETE CASCADE,
             sound_id TEXT NOT NULL REFERENCES sounds(id) ON DELETE CASCADE,
             membership_kind TEXT NOT NULL CHECK(membership_kind IN ('manual', 'include', 'exclude')),
             position INTEGER NOT NULL,
             PRIMARY KEY(tab_id, sound_id, membership_kind)
         );
         CREATE INDEX tab_memberships_order
             ON tab_memberships(tab_id, membership_kind, position);
         INSERT INTO meta(key, value) VALUES('schema_version', '1');
         PRAGMA user_version = 1;
         COMMIT;",
        )?;
    } else {
        let meta_version: String = connection.query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )?;
        if meta_version != DATABASE_SCHEMA_VERSION.to_string() {
            return Err(LibraryError::InvalidData(format!(
                "library metadata schema {meta_version} does not match file schema {schema_version}"
            )));
        }
    }
    Ok(connection)
}

fn replace_all(connection: &mut Connection, snapshot: LibrarySnapshot) -> Result<(), LibraryError> {
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM tab_memberships", [])?;
    transaction.execute("DELETE FROM tabs", [])?;
    transaction.execute("DELETE FROM sounds", [])?;
    transaction.execute("DELETE FROM roots", [])?;

    for (position, root) in snapshot.sound_folders.iter().enumerate() {
        transaction.execute(
            "INSERT INTO roots(path, position) VALUES(?1, ?2)",
            params![root, usize_to_i64(position)?],
        )?;
    }
    for (position, sound) in snapshot.sounds.iter().enumerate() {
        insert_sound(
            &transaction,
            sound,
            &snapshot.sound_folders,
            usize_to_i64(position)?,
        )?;
    }
    for tab in &snapshot.tabs {
        let (kind, root_folder, relative_folder, membership_kind) =
            if let Some(binding) = &tab.folder_binding {
                (
                    "generated",
                    Some(binding.root_folder.as_str()),
                    Some(binding.relative_subfolder.as_str()),
                    "include",
                )
            } else {
                ("manual", None, None, "manual")
            };
        transaction.execute(
            "INSERT INTO tabs(id, name, position, kind, root_folder, relative_folder)
             VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                tab.id,
                tab.name,
                i64::from(tab.order),
                kind,
                root_folder,
                relative_folder
            ],
        )?;
        for (position, sound_id) in tab.sound_ids.iter().enumerate() {
            transaction.execute(
                "INSERT INTO tab_memberships(tab_id, sound_id, membership_kind, position)
                 VALUES(?1, ?2, ?3, ?4)",
                params![tab.id, sound_id, membership_kind, usize_to_i64(position)?],
            )?;
        }
    }
    transaction.commit()?;
    Ok(())
}

fn insert_sound(
    transaction: &Transaction<'_>,
    sound: &Sound,
    roots: &[String],
    position: i64,
) -> Result<(), LibraryError> {
    let (root_folder, relative_path) = sound_location(&sound.path, roots);
    let duration_ms = sound
        .duration_ms
        .map(|value| {
            i64::try_from(value).map_err(|_| {
                LibraryError::InvalidData(format!(
                    "duration for sound '{}' exceeds SQLite INTEGER range",
                    sound.id
                ))
            })
        })
        .transpose()?;
    transaction.execute(
        "INSERT INTO sounds(
             id, name, search_name, path, source_path, hotkey, duration_ms,
             volume, enabled, loudness_lufs, loudness_state,
             loudness_confidence, loudness_fingerprint, loudness_true_peak_dbtp,
             root_folder, relative_path, position
         ) VALUES(
             ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
             ?15, ?16, ?17
         )",
        params![
            sound.id,
            sound.name,
            sound.name.to_lowercase(),
            sound.path,
            sound.source_path,
            sound.hotkey,
            duration_ms,
            i64::from(sound.volume),
            i64::from(sound.enabled),
            sound.loudness_lufs,
            sound.loudness_analysis_state.as_str(),
            sound.loudness_confidence,
            sound.loudness_source_fingerprint,
            sound.loudness_true_peak_dbtp,
            root_folder,
            relative_path,
            position,
        ],
    )?;
    Ok(())
}

fn sound_location(path: &str, roots: &[String]) -> (Option<String>, Option<String>) {
    let sound_path = Path::new(path);
    roots
        .iter()
        .filter_map(|root| {
            sound_path
                .strip_prefix(root)
                .ok()
                .map(|relative| (root, relative))
        })
        .max_by_key(|(root, _)| Path::new(root).components().count())
        .map(|(root, relative)| {
            (
                Some(root.clone()),
                Some(relative.to_string_lossy().into_owned()),
            )
        })
        .unwrap_or((None, None))
}

fn count_sounds(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
) -> Result<usize, LibraryError> {
    let count: i64 = match scope {
        LibraryScope::General => connection.query_row(
            "SELECT COUNT(*) FROM sounds WHERE instr(search_name, ?1) > 0",
            [search],
            |row| row.get(0),
        )?,
        LibraryScope::Tab(tab_id) => connection.query_row(
            "SELECT COUNT(*)
             FROM tab_memberships AS membership
             JOIN sounds AS sound ON sound.id = membership.sound_id
             WHERE membership.tab_id = ?1
               AND membership.membership_kind != 'exclude'
               AND instr(sound.search_name, ?2) > 0",
            params![tab_id, search],
            |row| row.get(0),
        )?,
    };
    usize::try_from(count)
        .map_err(|_| LibraryError::InvalidData("negative sound count".to_string()))
}

fn load_page(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
    page: usize,
) -> Result<SoundPage, LibraryError> {
    let total = count_sounds(connection, scope, search)?;
    let limit = usize_to_i64(PAGE_SIZE)?;
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let offset = usize_to_i64(offset)?;
    let mut sounds = Vec::with_capacity(PAGE_SIZE.min(total));
    match scope {
        LibraryScope::General => {
            let mut statement = connection.prepare(
                "SELECT id, name, path, source_path, hotkey, duration_ms, volume,
                        enabled, loudness_lufs, loudness_state, loudness_confidence,
                        loudness_fingerprint, loudness_true_peak_dbtp
                 FROM sounds
                 WHERE instr(search_name, ?1) > 0
                 ORDER BY search_name, id
                 LIMIT ?2 OFFSET ?3",
            )?;
            let rows = statement.query_map(params![search, limit, offset], sound_from_row)?;
            for sound in rows {
                sounds.push(sound?);
            }
        }
        LibraryScope::Tab(tab_id) => {
            let mut statement = connection.prepare(
                "SELECT sound.id, sound.name, sound.path, sound.source_path,
                        sound.hotkey, sound.duration_ms, sound.volume, sound.enabled,
                        sound.loudness_lufs, sound.loudness_state,
                        sound.loudness_confidence, sound.loudness_fingerprint,
                        sound.loudness_true_peak_dbtp
                 FROM tab_memberships AS membership
                 JOIN sounds AS sound ON sound.id = membership.sound_id
                 WHERE membership.tab_id = ?1
                   AND membership.membership_kind != 'exclude'
                   AND instr(sound.search_name, ?2) > 0
                 ORDER BY membership.position, sound.id
                 LIMIT ?3 OFFSET ?4",
            )?;
            let rows =
                statement.query_map(params![tab_id, search, limit, offset], sound_from_row)?;
            for sound in rows {
                sounds.push(sound?);
            }
        }
    }
    Ok(SoundPage { total, sounds })
}

fn load_sound(connection: &Connection, id: &str) -> Result<Option<Sound>, LibraryError> {
    connection
        .query_row(
            "SELECT id, name, path, source_path, hotkey, duration_ms, volume,
                    enabled, loudness_lufs, loudness_state, loudness_confidence,
                    loudness_fingerprint, loudness_true_peak_dbtp
             FROM sounds WHERE id = ?1",
            [id],
            sound_from_row,
        )
        .optional()
        .map_err(LibraryError::from)
}

fn sound_from_row(row: &Row<'_>) -> rusqlite::Result<Sound> {
    let duration_ms = row
        .get::<_, Option<i64>>(5)?
        .map(|value| {
            u64::try_from(value).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    5,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })
        })
        .transpose()?;
    let volume = u8::try_from(row.get::<_, i64>(6)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })?;
    let loudness_state = LoudnessAnalysisState::from_str(&row.get::<_, String>(9)?)
        .unwrap_or(LoudnessAnalysisState::Pending);
    Ok(Sound {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        source_path: row.get(3)?,
        hotkey: row.get(4)?,
        duration_ms,
        volume,
        enabled: row.get::<_, i64>(7)? != 0,
        loudness_lufs: row.get(8)?,
        loudness_analysis_state: loudness_state,
        loudness_confidence: row.get(10)?,
        loudness_source_fingerprint: row.get(11)?,
        loudness_true_peak_dbtp: row.get(12)?,
    })
}

fn usize_to_i64(value: usize) -> Result<i64, LibraryError> {
    i64::try_from(value)
        .map_err(|_| LibraryError::InvalidData("library index exceeds SQLite range".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_uses_bounded_memory_and_rollback_journal_settings() {
        let path = std::env::temp_dir().join(format!(
            "lsb-library-pragmas-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let connection = open_connection(&path).expect("open configured database");

        let journal_mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        let synchronous: i64 = connection
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("synchronous");
        let foreign_keys: i64 = connection
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .expect("foreign keys");
        let cache_size: i64 = connection
            .query_row("PRAGMA cache_size", [], |row| row.get(0))
            .expect("cache size");
        let temp_store: i64 = connection
            .query_row("PRAGMA temp_store", [], |row| row.get(0))
            .expect("temp store");

        assert_eq!(journal_mode, "delete");
        assert_eq!(synchronous, 2);
        assert_eq!(foreign_keys, 1);
        assert_eq!(cache_size, -2048);
        assert_eq!(temp_store, 1);

        drop(connection);
        let _ = std::fs::remove_file(path);
    }
}
