use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use crate::config::{LoudnessAnalysisState, Sound};

pub const PAGE_SIZE: usize = 256;
pub const MAX_BATCH_ROWS: usize = 512;
const DATABASE_SCHEMA_VERSION: i64 = 1;
const CONTROL_QUEUE_CAPACITY: usize = 16;
const VISIBLE_QUEUE_CAPACITY: usize = 64;
const MAINTENANCE_QUEUE_CAPACITY: usize = 2;

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
    pub fn try_recv(&self) -> Result<Option<T>, LibraryError> {
        match self.0.try_recv() {
            Ok(result) => result.map(Some),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => Err(LibraryError::WorkerUnavailable),
        }
    }

    #[cfg(test)]
    pub(crate) fn recv(self) -> Result<T, LibraryError> {
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
    ManualTab(String),
    Folder {
        root_path: String,
        relative_path: String,
    },
}

#[derive(Debug)]
pub struct SoundPage {
    pub total: usize,
    pub sounds: Vec<Sound>,
}

#[derive(Debug)]
pub struct RootItem {
    pub id: i64,
    pub path: String,
}

#[derive(Debug)]
pub struct RootPage {
    pub total: usize,
    pub roots: Vec<RootItem>,
}

#[derive(Debug)]
pub struct FolderItem {
    pub id: i64,
    pub relative_path: String,
    pub name: String,
    pub expanded: bool,
    pub has_children: bool,
}

#[derive(Debug)]
pub struct FolderPage {
    pub total: usize,
    pub folders: Vec<FolderItem>,
}

#[derive(Debug)]
pub struct RootRecord {
    pub path: String,
    pub position: usize,
}

#[derive(Debug)]
pub struct FolderRecord {
    pub root_path: String,
    pub relative_path: String,
    pub parent_relative_path: Option<String>,
    pub name: String,
    pub position: usize,
}

#[derive(Debug)]
pub struct SoundLocationRecord {
    pub root_path: String,
    pub folder_relative_path: Option<String>,
    pub relative_path: String,
}

#[derive(Debug)]
pub struct SoundRecord {
    pub sound: Sound,
    pub general_position: usize,
    pub locations: Vec<SoundLocationRecord>,
}

#[derive(Debug)]
pub struct ManualTabRecord {
    pub public_id: String,
    pub name: String,
    pub position: usize,
}

#[derive(Debug)]
pub struct ManualMembershipRecord {
    pub tab_public_id: String,
    pub sound_public_id: String,
    pub position: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum FolderOverrideAction {
    Include,
    Exclude,
}

#[derive(Debug)]
pub struct FolderOverrideRecord {
    pub root_path: String,
    pub folder_relative_path: String,
    pub sound_public_id: String,
    pub action: FolderOverrideAction,
}

#[derive(Debug)]
pub enum LibraryBatch {
    Roots(Vec<RootRecord>),
    Folders(Vec<FolderRecord>),
    Sounds(Vec<SoundRecord>),
    ManualTabs(Vec<ManualTabRecord>),
    ManualMemberships(Vec<ManualMembershipRecord>),
    FolderOverrides(Vec<FolderOverrideRecord>),
}

impl LibraryBatch {
    fn row_count(&self) -> usize {
        match self {
            Self::Roots(rows) => rows.len(),
            Self::Folders(rows) => rows.iter().fold(0, |total, row| {
                total.saturating_add(Path::new(&row.relative_path).components().count().max(1))
            }),
            Self::Sounds(rows) => rows
                .iter()
                .map(|row| 1_usize.saturating_add(row.locations.len()))
                .fold(0, usize::saturating_add),
            Self::ManualTabs(rows) => rows.len(),
            Self::ManualMemberships(rows) => rows.len(),
            Self::FolderOverrides(rows) => rows.len(),
        }
    }
}

enum Request {
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
    Adjacent {
        scope: LibraryScope,
        search: String,
        position: usize,
        offset: i32,
        reply: mpsc::SyncSender<Result<Option<Sound>, LibraryError>>,
    },
    HotkeyPage {
        page: usize,
        reply: mpsc::SyncSender<Result<SoundPage, LibraryError>>,
    },
    Roots {
        page: usize,
        reply: mpsc::SyncSender<Result<RootPage, LibraryError>>,
    },
    FolderChildren {
        root_path: String,
        parent_relative_path: Option<String>,
        page: usize,
        reply: mpsc::SyncSender<Result<FolderPage, LibraryError>>,
    },
    UpdateSound {
        sound: Sound,
        reply: mpsc::SyncSender<Result<bool, LibraryError>>,
    },
    DeleteSound {
        id: String,
        reply: mpsc::SyncSender<Result<bool, LibraryError>>,
    },
    ApplyBatch {
        batch: LibraryBatch,
        reply: mpsc::SyncSender<Result<(), LibraryError>>,
    },
}

#[derive(Default)]
struct QueueState {
    control: VecDeque<Request>,
    visible: VecDeque<Request>,
    maintenance: VecDeque<Request>,
    closed: bool,
}

#[derive(Default)]
struct RequestQueue {
    state: Mutex<QueueState>,
    ready: Condvar,
}

impl RequestQueue {
    fn push(&self, request: Request) -> Result<(), LibraryError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| LibraryError::WorkerUnavailable)?;
        if state.closed {
            return Err(LibraryError::WorkerUnavailable);
        }
        let (queue, capacity) = match request {
            Request::SoundById { .. } | Request::Adjacent { .. } | Request::HotkeyPage { .. } => {
                (&mut state.control, CONTROL_QUEUE_CAPACITY)
            }
            Request::Count { .. }
            | Request::Page { .. }
            | Request::Roots { .. }
            | Request::FolderChildren { .. }
            | Request::UpdateSound { .. }
            | Request::DeleteSound { .. } => (&mut state.visible, VISIBLE_QUEUE_CAPACITY),
            Request::ApplyBatch { .. } => (&mut state.maintenance, MAINTENANCE_QUEUE_CAPACITY),
        };
        if queue.len() >= capacity {
            return Err(LibraryError::QueueFull);
        }
        queue.push_back(request);
        self.ready.notify_one();
        Ok(())
    }

    fn pop(&self) -> Option<Request> {
        let mut state = self.state.lock().ok()?;
        loop {
            if let Some(request) = state.control.pop_front() {
                return Some(request);
            }
            if let Some(request) = state.visible.pop_front() {
                return Some(request);
            }
            if let Some(request) = state.maintenance.pop_front() {
                return Some(request);
            }
            if state.closed {
                return None;
            }
            state = self.ready.wait(state).ok()?;
        }
    }

    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.closed = true;
            self.ready.notify_all();
        }
    }
}

struct LibraryStoreInner {
    queue: Arc<RequestQueue>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Drop for LibraryStoreInner {
    fn drop(&mut self) {
        self.queue.close();
        if let Some(worker) = self.worker.lock().expect("library worker lock").take() {
            let _ = worker.join();
        }
    }
}

#[derive(Clone)]
pub struct LibraryStore(Arc<LibraryStoreInner>);

impl LibraryStore {
    pub fn open(path: PathBuf) -> Result<Self, LibraryError> {
        let queue = Arc::new(RequestQueue::default());
        let worker_queue = Arc::clone(&queue);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
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
                while let Some(request) = worker_queue.pop() {
                    handle_request(&mut connection, request);
                }
            })
            .map_err(|_| LibraryError::WorkerUnavailable)?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self(Arc::new(LibraryStoreInner {
                queue,
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

    pub fn apply_batch(&self, batch: LibraryBatch) -> LibraryResponse<()> {
        if batch.row_count() > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "library batches are limited to {MAX_BATCH_ROWS} rows"
            ))));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::ApplyBatch { batch, reply }, response)
    }

    pub fn count(&self, scope: LibraryScope, search: &str) -> LibraryResponse<usize> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::Count {
                scope,
                search: search.to_lowercase(),
                reply,
            },
            response,
        )
    }

    pub fn page(
        &self,
        scope: LibraryScope,
        search: &str,
        page: usize,
    ) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::Page {
                scope,
                search: search.to_lowercase(),
                page,
                reply,
            },
            response,
        )
    }

    pub fn sound_by_id(&self, id: &str) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::SoundById {
                id: id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn adjacent(
        &self,
        scope: LibraryScope,
        search: &str,
        position: usize,
        offset: i32,
    ) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::Adjacent {
                scope,
                search: search.to_lowercase(),
                position,
                offset,
                reply,
            },
            response,
        )
    }

    pub fn hotkey_page(&self, page: usize) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::HotkeyPage { page, reply }, response)
    }

    pub fn roots(&self, page: usize) -> LibraryResponse<RootPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::Roots { page, reply }, response)
    }

    pub fn folder_children(
        &self,
        root_path: &str,
        parent_relative_path: Option<&str>,
        page: usize,
    ) -> LibraryResponse<FolderPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::FolderChildren {
                root_path: root_path.to_string(),
                parent_relative_path: parent_relative_path.map(str::to_string),
                page,
                reply,
            },
            response,
        )
    }

    pub fn update_sound(&self, sound: Sound) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::UpdateSound { sound, reply }, response)
    }

    pub fn delete_sound(&self, id: &str) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::DeleteSound {
                id: id.to_string(),
                reply,
            },
            response,
        )
    }

    fn enqueue<T>(
        &self,
        request: Request,
        response: mpsc::Receiver<Result<T, LibraryError>>,
    ) -> LibraryResponse<T> {
        match self.0.queue.push(request) {
            Ok(()) => LibraryResponse(response),
            Err(error) => LibraryResponse::ready(Err(error)),
        }
    }
}

fn handle_request(connection: &mut Connection, request: Request) {
    match request {
        Request::Count {
            scope,
            search,
            reply,
        } => {
            let _ = reply.send(count_sounds(connection, &scope, &search));
        }
        Request::Page {
            scope,
            search,
            page,
            reply,
        } => {
            let _ = reply.send(load_page(connection, &scope, &search, page));
        }
        Request::SoundById { id, reply } => {
            let _ = reply.send(load_sound(connection, &id));
        }
        Request::Adjacent {
            scope,
            search,
            position,
            offset,
            reply,
        } => {
            let _ = reply.send(load_adjacent_sound(
                connection, &scope, &search, position, offset,
            ));
        }
        Request::HotkeyPage { page, reply } => {
            let _ = reply.send(load_hotkey_page(connection, page));
        }
        Request::Roots { page, reply } => {
            let _ = reply.send(load_roots(connection, page));
        }
        Request::FolderChildren {
            root_path,
            parent_relative_path,
            page,
            reply,
        } => {
            let _ = reply.send(load_folder_children(
                connection,
                &root_path,
                parent_relative_path.as_deref(),
                page,
            ));
        }
        Request::UpdateSound { sound, reply } => {
            let _ = reply.send(update_sound(connection, sound));
        }
        Request::DeleteSound { id, reply } => {
            let _ = reply.send(delete_sound(connection, &id));
        }
        Request::ApplyBatch { batch, reply } => {
            let _ = reply.send(apply_batch(connection, batch));
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
    connection.pragma_update(None, "synchronous", "EXTRA")?;
    connection.pragma_update(None, "foreign_keys", "ON")?;
    connection.pragma_update(None, "cache_size", -2048_i64)?;
    connection.pragma_update(None, "temp_store", "FILE")?;
    connection.pragma_update(None, "mmap_size", 0_i64)?;
    connection.pragma_update(None, "journal_size_limit", 0_i64)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    if schema_version == 0 {
        create_schema(&connection)?;
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
        if meta_version != DATABASE_SCHEMA_VERSION.to_string() || flavor != "bounded-v1" {
            return Err(LibraryError::InvalidData(
                "library metadata does not match the bounded schema".to_string(),
            ));
        }
    }
    connection.execute_batch("PRAGMA optimize;")?;
    Ok(connection)
}

fn create_schema(connection: &Connection) -> Result<(), LibraryError> {
    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
         CREATE TABLE roots(
             id INTEGER PRIMARY KEY,
             path TEXT NOT NULL UNIQUE,
             position INTEGER NOT NULL
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
             hotkey TEXT,
             duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms >= 0),
             volume INTEGER NOT NULL CHECK(volume BETWEEN 0 AND 100),
             enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
             loudness_lufs REAL,
             loudness_state TEXT NOT NULL,
             loudness_confidence REAL,
             loudness_fingerprint TEXT,
             loudness_true_peak_dbtp REAL,
             general_position INTEGER NOT NULL
         );
         CREATE INDEX sounds_general_order ON sounds(general_position, public_id);
         CREATE INDEX sounds_hotkey ON sounds(hotkey) WHERE hotkey IS NOT NULL;
         CREATE TABLE sound_locations(
             sound_id INTEGER NOT NULL REFERENCES sounds(rowid) ON DELETE CASCADE,
             root_id INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
             folder_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
             relative_path TEXT NOT NULL,
             PRIMARY KEY(sound_id, root_id)
         );
         CREATE INDEX sound_locations_folder ON sound_locations(folder_id, sound_id);
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
         CREATE TABLE folder_prefs(
             folder_id INTEGER PRIMARY KEY REFERENCES folders(id) ON DELETE CASCADE,
             display_name TEXT,
             sibling_position INTEGER,
             expanded INTEGER NOT NULL DEFAULT 0 CHECK(expanded IN (0, 1))
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
         INSERT INTO meta(key, value) VALUES('schema_version', '1');
         INSERT INTO meta(key, value) VALUES('schema_flavor', 'bounded-v1');
         PRAGMA user_version = 1;
         COMMIT;",
    )?;
    Ok(())
}

fn apply_batch(connection: &mut Connection, batch: LibraryBatch) -> Result<(), LibraryError> {
    let transaction = connection.transaction()?;
    match batch {
        LibraryBatch::Roots(rows) => insert_roots(&transaction, rows)?,
        LibraryBatch::Folders(rows) => insert_folders(&transaction, rows)?,
        LibraryBatch::Sounds(rows) => insert_sounds(&transaction, rows)?,
        LibraryBatch::ManualTabs(rows) => insert_manual_tabs(&transaction, rows)?,
        LibraryBatch::ManualMemberships(rows) => insert_manual_memberships(&transaction, rows)?,
        LibraryBatch::FolderOverrides(rows) => insert_folder_overrides(&transaction, rows)?,
    }
    transaction.commit()?;
    Ok(())
}

fn insert_roots(transaction: &Transaction<'_>, rows: Vec<RootRecord>) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare("INSERT INTO roots(path, position) VALUES(?1, ?2)")?;
    for row in rows {
        statement.execute(params![row.path, usize_to_i64(row.position)?])?;
    }
    Ok(())
}

fn insert_folders(
    transaction: &Transaction<'_>,
    rows: Vec<FolderRecord>,
) -> Result<(), LibraryError> {
    for row in rows {
        let root_id: i64 = transaction.query_row(
            "SELECT id FROM roots WHERE path = ?1",
            [&row.root_path],
            |result| result.get(0),
        )?;
        let parent_id = row
            .parent_relative_path
            .as_ref()
            .map(|parent| {
                transaction.query_row(
                    "SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2",
                    params![root_id, parent],
                    |result| result.get::<_, i64>(0),
                )
            })
            .transpose()?;
        transaction.execute(
            "INSERT INTO folders(root_id, parent_id, relative_path, name, position)
             VALUES(?1, ?2, ?3, ?4, ?5)",
            params![
                root_id,
                parent_id,
                row.relative_path,
                row.name,
                usize_to_i64(row.position)?
            ],
        )?;
        let folder_id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO folder_closure(ancestor_id, descendant_id, depth)
             VALUES(?1, ?1, 0)",
            [folder_id],
        )?;
        if let Some(parent_id) = parent_id {
            transaction.execute(
                "INSERT INTO folder_closure(ancestor_id, descendant_id, depth)
                 SELECT ancestor_id, ?1, depth + 1
                 FROM folder_closure WHERE descendant_id = ?2",
                params![folder_id, parent_id],
            )?;
        }
    }
    Ok(())
}

fn insert_sounds(
    transaction: &Transaction<'_>,
    rows: Vec<SoundRecord>,
) -> Result<(), LibraryError> {
    let mut insert_sound = transaction.prepare(
        "INSERT INTO sounds(
             public_id, name, search_name, path, source_path, hotkey, duration_ms,
             volume, enabled, loudness_lufs, loudness_state, loudness_confidence,
             loudness_fingerprint, loudness_true_peak_dbtp, general_position
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    let mut find_root = transaction.prepare("SELECT id FROM roots WHERE path = ?1")?;
    let mut find_folder =
        transaction.prepare("SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2")?;
    let mut insert_location = transaction.prepare(
        "INSERT INTO sound_locations(sound_id, root_id, folder_id, relative_path)
         VALUES(?1, ?2, ?3, ?4)",
    )?;
    for row in rows {
        let sound = row.sound;
        let duration_ms = sound
            .duration_ms
            .map(i64::try_from)
            .transpose()
            .map_err(|_| LibraryError::InvalidData("sound duration exceeds SQLite range".into()))?;
        insert_sound.execute(params![
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
            usize_to_i64(row.general_position)?,
        ])?;
        let sound_id = transaction.last_insert_rowid();
        for location in row.locations {
            let root_id: i64 =
                find_root.query_row([&location.root_path], |result| result.get(0))?;
            let folder_id = location
                .folder_relative_path
                .as_ref()
                .map(|folder| {
                    find_folder
                        .query_row(params![root_id, folder], |result| result.get::<_, i64>(0))
                })
                .transpose()?;
            insert_location.execute(params![
                sound_id,
                root_id,
                folder_id,
                location.relative_path
            ])?;
        }
    }
    Ok(())
}

fn insert_manual_tabs(
    transaction: &Transaction<'_>,
    rows: Vec<ManualTabRecord>,
) -> Result<(), LibraryError> {
    let mut statement = transaction
        .prepare("INSERT INTO manual_tabs(public_id, name, position) VALUES(?1, ?2, ?3)")?;
    for row in rows {
        statement.execute(params![
            row.public_id,
            row.name,
            usize_to_i64(row.position)?
        ])?;
    }
    Ok(())
}

fn insert_manual_memberships(
    transaction: &Transaction<'_>,
    rows: Vec<ManualMembershipRecord>,
) -> Result<(), LibraryError> {
    for row in rows {
        transaction.execute(
            "INSERT INTO manual_memberships(tab_id, sound_id, position)
             SELECT tab.id, sound.rowid, ?3
             FROM manual_tabs AS tab, sounds AS sound
             WHERE tab.public_id = ?1 AND sound.public_id = ?2",
            params![
                row.tab_public_id,
                row.sound_public_id,
                usize_to_i64(row.position)?
            ],
        )?;
        if transaction.changes() != 1 {
            return Err(LibraryError::InvalidData(
                "manual membership references a missing tab or sound".to_string(),
            ));
        }
    }
    Ok(())
}

fn insert_folder_overrides(
    transaction: &Transaction<'_>,
    rows: Vec<FolderOverrideRecord>,
) -> Result<(), LibraryError> {
    for row in rows {
        let action = match row.action {
            FolderOverrideAction::Include => "include",
            FolderOverrideAction::Exclude => "exclude",
        };
        transaction.execute(
            "INSERT INTO folder_overrides(folder_id, sound_id, action)
             SELECT folder.id, sound.rowid, ?4
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             CROSS JOIN sounds AS sound
             WHERE root.path = ?1 AND folder.relative_path = ?2 AND sound.public_id = ?3
             ON CONFLICT(folder_id, sound_id) DO UPDATE SET action = excluded.action",
            params![
                row.root_path,
                row.folder_relative_path,
                row.sound_public_id,
                action
            ],
        )?;
        if transaction.changes() != 1 {
            return Err(LibraryError::InvalidData(
                "folder override references a missing folder or sound".to_string(),
            ));
        }
    }
    Ok(())
}

fn search_query(search: &str) -> String {
    format!("\"{}\"", search.replace('"', "\"\""))
}

const SEARCH_FILTER: &str =
    "CASE WHEN ?1 = '' THEN 1 WHEN length(?1) < 3 THEN instr(sound.search_name, ?1) > 0 ELSE sound.rowid IN (SELECT rowid FROM sound_search WHERE sound_search MATCH ?2) END";

fn count_sounds(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
) -> Result<usize, LibraryError> {
    let fts = search_query(search);
    let count: i64 = match scope {
        LibraryScope::General => connection.query_row(
            &format!("SELECT COUNT(*) FROM sounds AS sound WHERE {SEARCH_FILTER}"),
            params![search, fts],
            |row| row.get(0),
        )?,
        LibraryScope::ManualTab(tab_id) => connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM manual_memberships AS membership
                 JOIN manual_tabs AS tab ON tab.id = membership.tab_id
                 JOIN sounds AS sound ON sound.rowid = membership.sound_id
                 WHERE tab.public_id = ?3 AND {SEARCH_FILTER}"
            ),
            params![search, fts, tab_id],
            |row| row.get(0),
        )?,
        LibraryScope::Folder {
            root_path,
            relative_path,
        } => connection.query_row(
            &format!(
                "WITH selected(folder_id) AS (
                     SELECT folder.id FROM folders AS folder
                     JOIN roots AS root ON root.id = folder.root_id
                     WHERE root.path = ?3 AND folder.relative_path = ?4
                 ), effective(sound_id) AS (
                     SELECT location.sound_id FROM selected
                     JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                     JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                     UNION
                     SELECT override.sound_id FROM selected
                     JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                     WHERE override.action = 'include'
                     EXCEPT
                     SELECT override.sound_id FROM selected
                     JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                     WHERE override.action = 'exclude'
                 )
                 SELECT COUNT(*) FROM effective
                 JOIN sounds AS sound ON sound.rowid = effective.sound_id
                 WHERE {SEARCH_FILTER}"
            ),
            params![search, fts, root_path, relative_path],
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
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let limit = usize_to_i64(PAGE_SIZE)?;
    let offset = usize_to_i64(offset)?;
    let fts = search_query(search);
    let fields = "sound.public_id, sound.name, sound.path, sound.source_path, sound.hotkey,
                  sound.duration_ms, sound.volume, sound.enabled, sound.loudness_lufs,
                  sound.loudness_state, sound.loudness_confidence, sound.loudness_fingerprint,
                  sound.loudness_true_peak_dbtp";
    let mut sounds = Vec::with_capacity(PAGE_SIZE.min(total));
    match scope {
        LibraryScope::General => {
            let sql = format!(
                "SELECT {fields} FROM sounds AS sound WHERE {SEARCH_FILTER}
                 ORDER BY sound.general_position, sound.public_id LIMIT ?3 OFFSET ?4"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(params![search, fts, limit, offset], sound_from_row)?;
            for sound in rows {
                sounds.push(sound?);
            }
        }
        LibraryScope::ManualTab(tab_id) => {
            let sql = format!(
                "SELECT {fields} FROM manual_memberships AS membership
                 JOIN manual_tabs AS tab ON tab.id = membership.tab_id
                 JOIN sounds AS sound ON sound.rowid = membership.sound_id
                 WHERE {SEARCH_FILTER} AND tab.public_id = ?3
                 ORDER BY membership.position, sound.public_id LIMIT ?4 OFFSET ?5"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows =
                statement.query_map(params![search, fts, tab_id, limit, offset], sound_from_row)?;
            for sound in rows {
                sounds.push(sound?);
            }
        }
        LibraryScope::Folder {
            root_path,
            relative_path,
        } => {
            let sql = format!(
                "WITH selected(folder_id) AS (
                     SELECT folder.id FROM folders AS folder
                     JOIN roots AS root ON root.id = folder.root_id
                     WHERE root.path = ?3 AND folder.relative_path = ?4
                 ), effective(sound_id) AS (
                     SELECT location.sound_id FROM selected
                     JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                     JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                     UNION
                     SELECT override.sound_id FROM selected
                     JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                     WHERE override.action = 'include'
                     EXCEPT
                     SELECT override.sound_id FROM selected
                     JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                     WHERE override.action = 'exclude'
                 )
                 SELECT {fields} FROM effective
                 JOIN sounds AS sound ON sound.rowid = effective.sound_id
                 WHERE {SEARCH_FILTER}
                 ORDER BY sound.general_position, sound.public_id LIMIT ?5 OFFSET ?6"
            );
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(
                params![search, fts, root_path, relative_path, limit, offset],
                sound_from_row,
            )?;
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
            "SELECT public_id, name, path, source_path, hotkey, duration_ms, volume,
                    enabled, loudness_lufs, loudness_state, loudness_confidence,
                    loudness_fingerprint, loudness_true_peak_dbtp
             FROM sounds WHERE public_id = ?1",
            [id],
            sound_from_row,
        )
        .optional()
        .map_err(LibraryError::from)
}

fn update_sound(connection: &Connection, sound: Sound) -> Result<bool, LibraryError> {
    let search_name = sound.name.to_lowercase();
    let duration_ms = sound
        .duration_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| LibraryError::InvalidData("sound duration exceeds SQLite range".into()))?;
    let changed = connection.execute(
        "UPDATE sounds SET
             name = ?2, search_name = ?3, path = ?4, source_path = ?5, hotkey = ?6,
             duration_ms = ?7, volume = ?8, enabled = ?9, loudness_lufs = ?10,
             loudness_state = ?11, loudness_confidence = ?12,
             loudness_fingerprint = ?13, loudness_true_peak_dbtp = ?14
         WHERE public_id = ?1",
        params![
            sound.id,
            sound.name,
            search_name,
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
        ],
    )?;
    Ok(changed == 1)
}

fn delete_sound(connection: &Connection, id: &str) -> Result<bool, LibraryError> {
    Ok(connection.execute("DELETE FROM sounds WHERE public_id = ?1", [id])? == 1)
}

fn load_roots(connection: &Connection, page: usize) -> Result<RootPage, LibraryError> {
    let count: i64 = connection.query_row("SELECT COUNT(*) FROM roots", [], |row| row.get(0))?;
    let total = usize::try_from(count)
        .map_err(|_| LibraryError::InvalidData("negative root count".to_string()))?;
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let mut statement = connection.prepare(
        "SELECT id, path FROM roots
         ORDER BY position, id LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        |row| {
            Ok(RootItem {
                id: row.get(0)?,
                path: row.get(1)?,
            })
        },
    )?;
    let mut roots = Vec::with_capacity(PAGE_SIZE.min(total.saturating_sub(offset)));
    for root in rows {
        roots.push(root?);
    }
    Ok(RootPage { total, roots })
}

fn load_folder_children(
    connection: &Connection,
    root_path: &str,
    parent_relative_path: Option<&str>,
    page: usize,
) -> Result<FolderPage, LibraryError> {
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let limit = usize_to_i64(PAGE_SIZE)?;
    let offset = usize_to_i64(offset)?;
    let fields = "folder.id, folder.relative_path,
                  COALESCE(pref.display_name, folder.name), COALESCE(pref.expanded, 0),
                  EXISTS(SELECT 1 FROM folders AS child WHERE child.parent_id = folder.id)";
    let (total, folders) = if let Some(parent_relative_path) = parent_relative_path {
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.parent_id = (
                 SELECT parent.id FROM folders AS parent
                 WHERE parent.root_id = root.id AND parent.relative_path = ?2
             )",
            params![root_path, parent_relative_path],
            |row| row.get(0),
        )?;
        let sql = format!(
            "SELECT {fields} FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE root.path = ?1 AND folder.parent_id = (
                 SELECT parent.id FROM folders AS parent
                 WHERE parent.root_id = root.id AND parent.relative_path = ?2
             )
             ORDER BY COALESCE(pref.sibling_position, folder.position), folder.id
             LIMIT ?3 OFFSET ?4"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(
            params![root_path, parent_relative_path, limit, offset],
            folder_from_row,
        )?;
        (count, collect_folders(rows)?)
    } else {
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.parent_id IS NULL",
            [root_path],
            |row| row.get(0),
        )?;
        let sql = format!(
            "SELECT {fields} FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE root.path = ?1 AND folder.parent_id IS NULL
             ORDER BY COALESCE(pref.sibling_position, folder.position), folder.id
             LIMIT ?2 OFFSET ?3"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement.query_map(params![root_path, limit, offset], folder_from_row)?;
        (count, collect_folders(rows)?)
    };
    Ok(FolderPage {
        total: usize::try_from(total)
            .map_err(|_| LibraryError::InvalidData("negative folder count".to_string()))?,
        folders,
    })
}

fn folder_from_row(row: &Row<'_>) -> rusqlite::Result<FolderItem> {
    Ok(FolderItem {
        id: row.get(0)?,
        relative_path: row.get(1)?,
        name: row.get(2)?,
        expanded: row.get::<_, i64>(3)? != 0,
        has_children: row.get::<_, i64>(4)? != 0,
    })
}

fn collect_folders(
    rows: rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<FolderItem>>,
) -> Result<Vec<FolderItem>, LibraryError> {
    let mut folders = Vec::with_capacity(PAGE_SIZE);
    for folder in rows {
        folders.push(folder?);
    }
    Ok(folders)
}

fn load_hotkey_page(connection: &Connection, page: usize) -> Result<SoundPage, LibraryError> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sounds WHERE hotkey IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    let total = usize::try_from(count)
        .map_err(|_| LibraryError::InvalidData("negative hotkey count".to_string()))?;
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let mut statement = connection.prepare(
        "SELECT public_id, name, path, source_path, hotkey, duration_ms, volume,
                enabled, loudness_lufs, loudness_state, loudness_confidence,
                loudness_fingerprint, loudness_true_peak_dbtp
         FROM sounds WHERE hotkey IS NOT NULL
         ORDER BY general_position, public_id LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        sound_from_row,
    )?;
    let mut sounds = Vec::with_capacity(PAGE_SIZE.min(total.saturating_sub(offset)));
    for sound in rows {
        sounds.push(sound?);
    }
    Ok(SoundPage { total, sounds })
}

fn load_adjacent_sound(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
    position: usize,
    offset: i32,
) -> Result<Option<Sound>, LibraryError> {
    let distance = usize::try_from(offset.unsigned_abs())
        .map_err(|_| LibraryError::InvalidData("adjacent offset overflow".to_string()))?;
    let Some(target) = (if offset.is_negative() {
        position.checked_sub(distance)
    } else {
        position.checked_add(distance)
    }) else {
        return Ok(None);
    };
    load_sound_at(connection, scope, search, target)
}

fn load_sound_at(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
    position: usize,
) -> Result<Option<Sound>, LibraryError> {
    let fts = search_query(search);
    let offset = usize_to_i64(position)?;
    let fields = "sound.public_id, sound.name, sound.path, sound.source_path, sound.hotkey,
                  sound.duration_ms, sound.volume, sound.enabled, sound.loudness_lufs,
                  sound.loudness_state, sound.loudness_confidence, sound.loudness_fingerprint,
                  sound.loudness_true_peak_dbtp";
    match scope {
        LibraryScope::General => connection
            .query_row(
                &format!(
                    "SELECT {fields} FROM sounds AS sound WHERE {SEARCH_FILTER}
                     ORDER BY sound.general_position, sound.public_id LIMIT 1 OFFSET ?3"
                ),
                params![search, fts, offset],
                sound_from_row,
            )
            .optional()
            .map_err(LibraryError::from),
        LibraryScope::ManualTab(tab_id) => connection
            .query_row(
                &format!(
                    "SELECT {fields} FROM manual_memberships AS membership
                     JOIN manual_tabs AS tab ON tab.id = membership.tab_id
                     JOIN sounds AS sound ON sound.rowid = membership.sound_id
                     WHERE {SEARCH_FILTER} AND tab.public_id = ?3
                     ORDER BY membership.position, sound.public_id LIMIT 1 OFFSET ?4"
                ),
                params![search, fts, tab_id, offset],
                sound_from_row,
            )
            .optional()
            .map_err(LibraryError::from),
        LibraryScope::Folder {
            root_path,
            relative_path,
        } => connection
            .query_row(
                &format!(
                    "WITH selected(folder_id) AS (
                         SELECT folder.id FROM folders AS folder
                         JOIN roots AS root ON root.id = folder.root_id
                         WHERE root.path = ?3 AND folder.relative_path = ?4
                     ), effective(sound_id) AS (
                         SELECT location.sound_id FROM selected
                         JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                         JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                         UNION
                         SELECT override.sound_id FROM selected
                         JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                         WHERE override.action = 'include'
                         EXCEPT
                         SELECT override.sound_id FROM selected
                         JOIN folder_overrides AS override ON override.folder_id = selected.folder_id
                         WHERE override.action = 'exclude'
                     )
                     SELECT {fields} FROM effective
                     JOIN sounds AS sound ON sound.rowid = effective.sound_id
                     WHERE {SEARCH_FILTER}
                     ORDER BY sound.general_position, sound.public_id LIMIT 1 OFFSET ?5"
                ),
                params![search, fts, root_path, relative_path, offset],
                sound_from_row,
            )
            .optional()
            .map_err(LibraryError::from),
    }
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
    fn request_queue_prioritizes_control_over_visible_and_maintenance() {
        let queue = RequestQueue::default();
        let (maintenance_reply, _) = mpsc::sync_channel(1);
        let (visible_reply, _) = mpsc::sync_channel(1);
        let (control_reply, _) = mpsc::sync_channel(1);
        queue
            .push(Request::ApplyBatch {
                batch: LibraryBatch::Roots(Vec::new()),
                reply: maintenance_reply,
            })
            .expect("queue maintenance");
        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: String::new(),
                reply: visible_reply,
            })
            .expect("queue visible");
        queue
            .push(Request::SoundById {
                id: "sound".to_string(),
                reply: control_reply,
            })
            .expect("queue control");

        assert!(matches!(queue.pop(), Some(Request::SoundById { .. })));
        assert!(matches!(queue.pop(), Some(Request::Count { .. })));
        assert!(matches!(queue.pop(), Some(Request::ApplyBatch { .. })));
    }

    #[test]
    fn connection_uses_bounded_memory_and_durable_rollback_settings() {
        let path = std::env::temp_dir().join(format!(
            "lsb-library-pragmas-{}.sqlite3",
            uuid::Uuid::new_v4()
        ));
        let connection = open_connection(&path).expect("open configured database");

        let text = |pragma| {
            connection
                .query_row(&format!("PRAGMA {pragma}"), [], |row| {
                    row.get::<_, String>(0)
                })
                .expect(pragma)
        };
        let integer = |pragma| {
            connection
                .query_row(&format!("PRAGMA {pragma}"), [], |row| row.get::<_, i64>(0))
                .expect(pragma)
        };
        assert_eq!(text("journal_mode"), "delete");
        assert_eq!(integer("synchronous"), 3);
        assert_eq!(integer("foreign_keys"), 1);
        assert_eq!(integer("cache_size"), -2048);
        assert_eq!(integer("temp_store"), 1);
        assert_eq!(integer("mmap_size"), 0);
        assert_eq!(integer("journal_size_limit"), 0);

        drop(connection);
        let _ = std::fs::remove_file(path);
    }
}
