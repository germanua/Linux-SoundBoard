use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use crate::config::{Config, ControlHotkeyAction, LoudnessAnalysisState, Sound};

pub const PAGE_SIZE: usize = 256;
pub const MAX_BATCH_ROWS: usize = 512;
const DATABASE_SCHEMA_VERSION: i64 = 3;
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
pub struct ManualTabItem {
    pub public_id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct ManualTabPage {
    pub total: usize,
    pub tabs: Vec<ManualTabItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotkeyBindingOwner {
    Sound(String),
    Control(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyBindingRecord {
    pub binding_id: String,
    pub owner: HotkeyBindingOwner,
    pub accelerator: String,
    pub normalized: Option<String>,
    pub issue: Option<String>,
}

#[derive(Debug)]
pub struct HotkeyBindingPage {
    pub bindings: Vec<HotkeyBindingRecord>,
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

#[derive(Debug)]
pub struct LegacyGeneratedTabRecord {
    pub public_id: String,
    pub root_path: String,
    pub relative_path: String,
    pub name: String,
    pub position: usize,
}

#[derive(Debug)]
pub struct LegacyGeneratedMembershipRecord {
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
    LegacyGeneratedTabs(Vec<LegacyGeneratedTabRecord>),
    LegacyGeneratedMemberships(Vec<LegacyGeneratedMembershipRecord>),
    FolderOverrides(Vec<FolderOverrideRecord>),
    HotkeyBindings(Vec<HotkeyBindingRecord>),
}

enum LibraryEdit {
    UpsertManualTab(ManualTabRecord),
    DeleteManualTab(String),
    SetManualMembership(ManualMembershipRecord),
    RemoveManualMembership {
        tab_public_id: String,
        sound_public_id: String,
    },
    ApplyManualMemberships {
        additions: Vec<ManualMembershipRecord>,
        removals: Vec<(String, String)>,
    },
    SetFolderOverride(FolderOverrideRecord),
    ClearFolderOverride {
        root_path: String,
        folder_relative_path: String,
        sound_public_id: String,
    },
    SetFolderPreferences {
        root_path: String,
        folder_relative_path: String,
        display_name: Option<String>,
        sibling_position: Option<usize>,
        expanded: bool,
    },
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
            Self::LegacyGeneratedTabs(rows) => rows.len(),
            Self::LegacyGeneratedMemberships(rows) => rows.len(),
            Self::FolderOverrides(rows) => rows.len(),
            Self::HotkeyBindings(rows) => rows.len(),
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
    SoundByPath {
        path: String,
        reply: mpsc::SyncSender<Result<Option<Sound>, LibraryError>>,
    },
    SoundForBinding {
        binding_id: String,
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
    HotkeyBindingsAfter {
        after: Option<String>,
        reply: mpsc::SyncSender<Result<HotkeyBindingPage, LibraryError>>,
    },
    SetHotkeyBinding {
        binding: HotkeyBindingRecord,
        reply: mpsc::SyncSender<Result<bool, LibraryError>>,
    },
    DeleteHotkeyBinding {
        binding_id: String,
        reply: mpsc::SyncSender<Result<bool, LibraryError>>,
    },
    HotkeyConflict {
        binding_id: String,
        normalized: String,
        reply: mpsc::SyncSender<Result<Option<String>, LibraryError>>,
    },
    BeginRootScan {
        root_path: String,
        position: usize,
        reply: mpsc::SyncSender<Result<i64, LibraryError>>,
    },
    RootScanBatch {
        root_path: String,
        generation: i64,
        folders: Vec<FolderRecord>,
        sounds: Vec<SoundRecord>,
        reply: mpsc::SyncSender<Result<(), LibraryError>>,
    },
    FinishRootScan {
        root_path: String,
        generation: i64,
        reply: mpsc::SyncSender<Result<bool, LibraryError>>,
    },
    CancelRootScan {
        root_path: String,
        generation: i64,
        reply: mpsc::SyncSender<Result<bool, LibraryError>>,
    },
    RemoveRoot {
        root_path: String,
        reply: mpsc::SyncSender<Result<bool, LibraryError>>,
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
    ManualTabs {
        page: usize,
        reply: mpsc::SyncSender<Result<ManualTabPage, LibraryError>>,
    },
    Edit {
        edit: LibraryEdit,
        reply: mpsc::SyncSender<Result<bool, LibraryError>>,
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
            Request::SoundById { .. }
            | Request::SoundByPath { .. }
            | Request::SoundForBinding { .. }
            | Request::Adjacent { .. }
            | Request::HotkeyPage { .. }
            | Request::HotkeyBindingsAfter { .. }
            | Request::SetHotkeyBinding { .. }
            | Request::DeleteHotkeyBinding { .. } => (&mut state.control, CONTROL_QUEUE_CAPACITY),
            Request::HotkeyConflict { .. } => (&mut state.control, CONTROL_QUEUE_CAPACITY),
            Request::Count { .. }
            | Request::Page { .. }
            | Request::Roots { .. }
            | Request::FolderChildren { .. }
            | Request::ManualTabs { .. }
            | Request::Edit { .. }
            | Request::UpdateSound { .. }
            | Request::DeleteSound { .. } => (&mut state.visible, VISIBLE_QUEUE_CAPACITY),
            Request::BeginRootScan { .. }
            | Request::FinishRootScan { .. }
            | Request::CancelRootScan { .. }
            | Request::RemoveRoot { .. } => (&mut state.visible, VISIBLE_QUEUE_CAPACITY),
            Request::ApplyBatch { .. } | Request::RootScanBatch { .. } => {
                (&mut state.maintenance, MAINTENANCE_QUEUE_CAPACITY)
            }
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

pub(crate) fn legacy_hotkey_binding(
    binding_id: String,
    owner: HotkeyBindingOwner,
    raw: &str,
) -> HotkeyBindingRecord {
    match crate::hotkeys::canonicalize_hotkey_string(raw) {
        Ok(canonical) => HotkeyBindingRecord {
            binding_id,
            owner,
            accelerator: canonical,
            normalized: None,
            issue: Some("valid legacy candidate".to_string()),
        },
        Err(error) => HotkeyBindingRecord {
            binding_id,
            owner,
            accelerator: raw.to_string(),
            normalized: None,
            issue: Some(format!("invalid legacy binding: {error}")),
        },
    }
}

impl LibraryStore {
    pub fn open_seeded(path: PathBuf, legacy: &Config) -> Result<Self, LibraryError> {
        if path.exists() {
            return Self::open(path);
        }

        let candidate = path.with_file_name(format!(
            ".library.sqlite3.importing.{}.{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let seed_result = (|| -> Result<(), LibraryError> {
            let store = Self::open(candidate.clone())?;
            for (batch_index, roots) in legacy.sound_folders.chunks(MAX_BATCH_ROWS).enumerate() {
                let base = batch_index * MAX_BATCH_ROWS;
                store
                    .apply_batch(LibraryBatch::Roots(
                        roots
                            .iter()
                            .enumerate()
                            .map(|(offset, path)| RootRecord {
                                path: path.clone(),
                                position: base + offset,
                            })
                            .collect(),
                    ))
                    .recv()?;
            }

            for (batch_index, sounds) in legacy.sounds.chunks(MAX_BATCH_ROWS).enumerate() {
                let base = batch_index * MAX_BATCH_ROWS;
                store
                    .apply_batch(LibraryBatch::Sounds(
                        sounds
                            .iter()
                            .enumerate()
                            .map(|(offset, sound)| {
                                let mut sound = sound.clone();
                                sound.hotkey = None;
                                SoundRecord {
                                    sound,
                                    general_position: base + offset,
                                    locations: Vec::new(),
                                }
                            })
                            .collect(),
                    ))
                    .recv()?;

                let bindings = sounds
                    .iter()
                    .filter_map(|sound| {
                        sound.hotkey.as_deref().map(|hotkey| {
                            legacy_hotkey_binding(
                                sound.id.clone(),
                                HotkeyBindingOwner::Sound(sound.id.clone()),
                                hotkey,
                            )
                        })
                    })
                    .collect::<Vec<_>>();
                if !bindings.is_empty() {
                    store
                        .apply_batch(LibraryBatch::HotkeyBindings(bindings))
                        .recv()?;
                }
            }

            let manual_tabs = legacy
                .tabs
                .iter()
                .filter(|tab| tab.folder_binding.is_none())
                .collect::<Vec<_>>();
            for tabs in manual_tabs.chunks(MAX_BATCH_ROWS) {
                store
                    .apply_batch(LibraryBatch::ManualTabs(
                        tabs.iter()
                            .map(|tab| ManualTabRecord {
                                public_id: tab.id.clone(),
                                name: tab.name.clone(),
                                position: tab.order as usize,
                            })
                            .collect(),
                    ))
                    .recv()?;
            }
            for tab in manual_tabs {
                for (batch_index, ids) in tab.sound_ids.chunks(MAX_BATCH_ROWS).enumerate() {
                    let base = batch_index * MAX_BATCH_ROWS;
                    store
                        .apply_batch(LibraryBatch::ManualMemberships(
                            ids.iter()
                                .enumerate()
                                .map(|(offset, sound_id)| ManualMembershipRecord {
                                    tab_public_id: tab.id.clone(),
                                    sound_public_id: sound_id.clone(),
                                    position: base + offset,
                                })
                                .collect(),
                        ))
                        .recv()?;
                }
            }

            let control_bindings = ControlHotkeyAction::all()
                .iter()
                .filter_map(|meta| {
                    legacy
                        .settings
                        .control_hotkeys
                        .get_cloned(meta.action)
                        .map(|hotkey| {
                            legacy_hotkey_binding(
                                meta.binding_id.to_string(),
                                HotkeyBindingOwner::Control(meta.id.to_string()),
                                &hotkey,
                            )
                        })
                })
                .collect::<Vec<_>>();
            if !control_bindings.is_empty() {
                store
                    .apply_batch(LibraryBatch::HotkeyBindings(control_bindings))
                    .recv()?;
            }
            drop(store);

            let connection = Connection::open(&candidate)?;
            connection.execute_batch(
                "UPDATE hotkey_bindings AS binding
                 SET issue = 'duplicate legacy binding'
                 WHERE issue = 'valid legacy candidate'
                   AND (SELECT COUNT(*) FROM hotkey_bindings AS other
                        WHERE other.accelerator = binding.accelerator
                          AND other.issue = 'valid legacy candidate') > 1;
                 UPDATE hotkey_bindings AS binding
                 SET normalized = accelerator, state = 'active', issue = NULL
                 WHERE issue = 'valid legacy candidate'
                   AND (SELECT COUNT(*) FROM hotkey_bindings AS other
                        WHERE other.accelerator = binding.accelerator
                          AND other.issue = 'valid legacy candidate') = 1;",
            )?;
            let integrity: String =
                connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
            if integrity != "ok" {
                return Err(LibraryError::InvalidData(format!(
                    "seeded library failed integrity check: {integrity}"
                )));
            }
            drop(connection);
            std::fs::File::open(&candidate)
                .and_then(|file| file.sync_all())
                .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
            std::fs::rename(&candidate, &path)
                .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
            if let Some(parent) = path.parent() {
                std::fs::File::open(parent)
                    .and_then(|directory| directory.sync_all())
                    .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
            }
            Ok(())
        })();
        if seed_result.is_err() {
            let _ = std::fs::remove_file(&candidate);
        }
        seed_result?;
        Self::open(path)
    }

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

    pub fn sound_by_path(&self, path: &str) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::SoundByPath {
                path: path.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn sound_for_binding(&self, binding_id: &str) -> LibraryResponse<Option<Sound>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::SoundForBinding {
                binding_id: binding_id.to_string(),
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

    pub fn hotkey_bindings_after(&self, after: Option<&str>) -> LibraryResponse<HotkeyBindingPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyBindingsAfter {
                after: after.map(str::to_string),
                reply,
            },
            response,
        )
    }

    pub fn set_hotkey_binding(&self, binding: HotkeyBindingRecord) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::SetHotkeyBinding { binding, reply }, response)
    }

    pub fn delete_hotkey_binding(&self, binding_id: &str) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::DeleteHotkeyBinding {
                binding_id: binding_id.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn hotkey_conflict(
        &self,
        binding_id: &str,
        normalized: &str,
    ) -> LibraryResponse<Option<String>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyConflict {
                binding_id: binding_id.to_string(),
                normalized: normalized.to_string(),
                reply,
            },
            response,
        )
    }

    pub fn begin_root_scan(&self, root_path: &str, position: usize) -> LibraryResponse<i64> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::BeginRootScan {
                root_path: root_path.to_string(),
                position,
                reply,
            },
            response,
        )
    }

    pub fn apply_root_scan_batch(
        &self,
        root_path: &str,
        generation: i64,
        folders: Vec<FolderRecord>,
        sounds: Vec<SoundRecord>,
    ) -> LibraryResponse<()> {
        let row_count = folders
            .iter()
            .map(|folder| Path::new(&folder.relative_path).components().count().max(1))
            .fold(0_usize, usize::saturating_add)
            .saturating_add(
                sounds
                    .iter()
                    .map(|sound| 1_usize.saturating_add(sound.locations.len()))
                    .sum(),
            );
        if row_count > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "root scan batches are limited to {MAX_BATCH_ROWS} rows"
            ))));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::RootScanBatch {
                root_path: root_path.to_string(),
                generation,
                folders,
                sounds,
                reply,
            },
            response,
        )
    }

    pub fn finish_root_scan(&self, root_path: &str, generation: i64) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::FinishRootScan {
                root_path: root_path.to_string(),
                generation,
                reply,
            },
            response,
        )
    }

    pub fn cancel_root_scan(&self, root_path: &str, generation: i64) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::CancelRootScan {
                root_path: root_path.to_string(),
                generation,
                reply,
            },
            response,
        )
    }

    pub fn remove_root(&self, root_path: &str) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::RemoveRoot {
                root_path: root_path.to_string(),
                reply,
            },
            response,
        )
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

    pub fn manual_tabs(&self, page: usize) -> LibraryResponse<ManualTabPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::ManualTabs { page, reply }, response)
    }

    pub fn upsert_manual_tab(&self, tab: ManualTabRecord) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::UpsertManualTab(tab))
    }

    pub fn delete_manual_tab(&self, public_id: &str) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::DeleteManualTab(public_id.to_string()))
    }

    pub fn set_manual_membership(
        &self,
        membership: ManualMembershipRecord,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetManualMembership(membership))
    }

    pub fn remove_manual_membership(
        &self,
        tab_public_id: &str,
        sound_public_id: &str,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::RemoveManualMembership {
            tab_public_id: tab_public_id.to_string(),
            sound_public_id: sound_public_id.to_string(),
        })
    }

    pub fn remove_manual_memberships(
        &self,
        tab_public_id: &str,
        sound_public_ids: Vec<String>,
    ) -> LibraryResponse<bool> {
        self.apply_manual_memberships(
            Vec::new(),
            sound_public_ids
                .into_iter()
                .map(|sound_public_id| (tab_public_id.to_string(), sound_public_id))
                .collect(),
        )
    }

    pub fn apply_manual_memberships(
        &self,
        additions: Vec<ManualMembershipRecord>,
        removals: Vec<(String, String)>,
    ) -> LibraryResponse<bool> {
        if additions.len().saturating_add(removals.len()) > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "manual membership batches are limited to {MAX_BATCH_ROWS} rows"
            ))));
        }
        self.edit(LibraryEdit::ApplyManualMemberships {
            additions,
            removals,
        })
    }

    pub fn set_folder_override(&self, record: FolderOverrideRecord) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderOverride(record))
    }

    pub fn clear_folder_override(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        sound_public_id: &str,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::ClearFolderOverride {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            sound_public_id: sound_public_id.to_string(),
        })
    }

    pub fn set_folder_preferences(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        display_name: Option<&str>,
        sibling_position: Option<usize>,
        expanded: bool,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderPreferences {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            display_name: display_name.map(str::to_string),
            sibling_position,
            expanded,
        })
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

    fn edit(&self, edit: LibraryEdit) -> LibraryResponse<bool> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::Edit { edit, reply }, response)
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
        Request::SoundByPath { path, reply } => {
            let _ = reply.send(load_sound_by_path(connection, &path));
        }
        Request::SoundForBinding { binding_id, reply } => {
            let _ = reply.send(load_sound_for_binding(connection, &binding_id));
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
        Request::HotkeyBindingsAfter { after, reply } => {
            let _ = reply.send(load_hotkey_bindings_after(connection, after.as_deref()));
        }
        Request::SetHotkeyBinding { binding, reply } => {
            let _ = reply.send(set_hotkey_binding(connection, binding));
        }
        Request::DeleteHotkeyBinding { binding_id, reply } => {
            let _ = reply.send(delete_hotkey_binding(connection, &binding_id));
        }
        Request::HotkeyConflict {
            binding_id,
            normalized,
            reply,
        } => {
            let _ = reply.send(load_hotkey_conflict(connection, &binding_id, &normalized));
        }
        Request::BeginRootScan {
            root_path,
            position,
            reply,
        } => {
            let _ = reply.send(begin_root_scan(connection, &root_path, position));
        }
        Request::RootScanBatch {
            root_path,
            generation,
            folders,
            sounds,
            reply,
        } => {
            let _ = reply.send(apply_root_scan_batch(
                connection, &root_path, generation, folders, sounds,
            ));
        }
        Request::FinishRootScan {
            root_path,
            generation,
            reply,
        } => {
            let _ = reply.send(finish_root_scan(connection, &root_path, generation));
        }
        Request::CancelRootScan {
            root_path,
            generation,
            reply,
        } => {
            let _ = reply.send(cancel_root_scan(connection, &root_path, generation));
        }
        Request::RemoveRoot { root_path, reply } => {
            let _ = reply.send(remove_root(connection, &root_path));
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
        Request::ManualTabs { page, reply } => {
            let _ = reply.send(load_manual_tabs(connection, page));
        }
        Request::Edit { edit, reply } => {
            let _ = reply.send(apply_edit(connection, edit));
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
    } else if schema_version == 1 {
        migrate_schema_1_to_2(&connection)?;
        migrate_schema_2_to_3(&connection)?;
    } else if schema_version == 2 {
        migrate_schema_2_to_3(&connection)?;
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
        if meta_version != DATABASE_SCHEMA_VERSION.to_string() || flavor != "bounded-generation-v3"
        {
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
         INSERT INTO meta(key, value) VALUES('schema_version', '3');
         INSERT INTO meta(key, value) VALUES('schema_flavor', 'bounded-generation-v3');
         PRAGMA user_version = 3;
         COMMIT;",
    )?;
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

fn apply_batch(connection: &mut Connection, batch: LibraryBatch) -> Result<(), LibraryError> {
    let transaction = connection.transaction()?;
    match batch {
        LibraryBatch::Roots(rows) => insert_roots(&transaction, rows)?,
        LibraryBatch::Folders(rows) => insert_folders(&transaction, rows)?,
        LibraryBatch::Sounds(rows) => insert_sounds(&transaction, rows)?,
        LibraryBatch::ManualTabs(rows) => insert_manual_tabs(&transaction, rows)?,
        LibraryBatch::ManualMemberships(rows) => insert_manual_memberships(&transaction, rows)?,
        LibraryBatch::LegacyGeneratedTabs(rows) => {
            insert_legacy_generated_tabs(&transaction, rows)?
        }
        LibraryBatch::LegacyGeneratedMemberships(rows) => {
            insert_legacy_generated_memberships(&transaction, rows)?
        }
        LibraryBatch::FolderOverrides(rows) => insert_folder_overrides(&transaction, rows)?,
        LibraryBatch::HotkeyBindings(rows) => insert_hotkey_bindings(&transaction, rows)?,
    }
    transaction.commit()?;
    Ok(())
}

fn scan_meta_key(root_path: &str) -> String {
    format!("active_scan:{root_path}")
}

fn begin_root_scan(
    connection: &mut Connection,
    root_path: &str,
    position: usize,
) -> Result<i64, LibraryError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO roots(path, position) VALUES(?1, ?2)
         ON CONFLICT(path) DO UPDATE SET position = excluded.position",
        params![root_path, usize_to_i64(position)?],
    )?;
    let root_id: i64 =
        transaction.query_row("SELECT id FROM roots WHERE path = ?1", [root_path], |row| {
            row.get(0)
        })?;
    let highest: i64 = transaction
        .query_row(
            "SELECT max(generation) FROM (
             SELECT active_generation AS generation FROM roots WHERE id = ?1
             UNION ALL
             SELECT generation FROM folder_presence AS presence
             JOIN folders AS folder ON folder.id = presence.folder_id
             WHERE folder.root_id = ?1
             UNION ALL
             SELECT generation FROM sound_locations WHERE root_id = ?1
         )",
            [root_id],
            |row| row.get::<_, Option<i64>>(0),
        )?
        .unwrap_or(0);
    let generation = highest
        .checked_add(1)
        .ok_or_else(|| LibraryError::InvalidData("root generation overflow".to_string()))?;
    transaction.execute(
        "INSERT INTO meta(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![scan_meta_key(root_path), generation.to_string()],
    )?;
    transaction.commit()?;
    Ok(generation)
}

fn verify_root_scan(
    connection: &Connection,
    root_path: &str,
    generation: i64,
) -> Result<bool, LibraryError> {
    Ok(connection
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [scan_meta_key(root_path)],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .is_some_and(|value| value == generation.to_string()))
}

fn ensure_scan_folder(
    transaction: &Transaction<'_>,
    root_id: i64,
    generation: i64,
    row: &FolderRecord,
) -> Result<i64, LibraryError> {
    let components = Path::new(&row.relative_path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let mut parent_id = None;
    let mut relative = PathBuf::new();
    let mut folder_id = None;
    for (index, component) in components.iter().enumerate() {
        relative.push(component);
        let relative_path = relative.to_string_lossy();
        let is_target = index + 1 == components.len();
        let name = if is_target { &row.name } else { component };
        let position = if is_target { row.position } else { 0 };
        transaction.execute(
            "INSERT INTO folders(root_id, parent_id, relative_path, name, position)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(root_id, relative_path) DO UPDATE SET
                 parent_id = excluded.parent_id,
                 name = CASE WHEN ?6 THEN excluded.name ELSE folders.name END,
                 position = CASE WHEN ?6 THEN excluded.position ELSE folders.position END",
            params![
                root_id,
                parent_id,
                relative_path.as_ref(),
                name,
                usize_to_i64(position)?,
                is_target,
            ],
        )?;
        let current_id: i64 = transaction.query_row(
            "SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2",
            params![root_id, relative_path.as_ref()],
            |result| result.get(0),
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO folder_presence(folder_id, generation) VALUES(?1, ?2)",
            params![current_id, generation],
        )?;
        transaction.execute(
            "INSERT OR IGNORE INTO folder_closure(ancestor_id, descendant_id, depth)
             VALUES(?1, ?1, 0)",
            [current_id],
        )?;
        if let Some(parent) = parent_id {
            transaction.execute(
                "INSERT OR IGNORE INTO folder_closure(ancestor_id, descendant_id, depth)
                 SELECT ancestor_id, ?1, depth + 1
                 FROM folder_closure WHERE descendant_id = ?2",
                params![current_id, parent],
            )?;
        }
        parent_id = Some(current_id);
        folder_id = Some(current_id);
    }
    folder_id.ok_or_else(|| LibraryError::InvalidData("folder path cannot be empty".to_string()))
}

fn insert_scan_sound(
    transaction: &Transaction<'_>,
    root_id: i64,
    root_path: &str,
    generation: i64,
    row: SoundRecord,
) -> Result<(), LibraryError> {
    let sound = row.sound;
    let duration_ms = sound
        .duration_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| LibraryError::InvalidData("sound duration exceeds SQLite range".into()))?;
    transaction.execute(
        "INSERT INTO sounds(
             public_id, name, search_name, path, source_path, duration_ms,
             volume, enabled, loudness_lufs, loudness_state, loudness_confidence,
             loudness_fingerprint, loudness_true_peak_dbtp, general_position, standalone
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0)
         ON CONFLICT(path) DO NOTHING",
        params![
            &sound.id,
            &sound.name,
            sound.name.to_lowercase(),
            &sound.path,
            &sound.source_path,
            duration_ms,
            i64::from(sound.volume),
            i64::from(sound.enabled),
            sound.loudness_lufs,
            sound.loudness_analysis_state.as_str(),
            sound.loudness_confidence,
            sound.loudness_source_fingerprint,
            sound.loudness_true_peak_dbtp,
            usize_to_i64(row.general_position)?,
        ],
    )?;
    let sound_id: i64 = transaction.query_row(
        "SELECT rowid FROM sounds WHERE path = ?1",
        [&sound.path],
        |result| result.get(0),
    )?;
    transaction.execute(
        "UPDATE sounds SET standalone = 0 WHERE rowid = ?1",
        [sound_id],
    )?;
    for location in row.locations {
        if location.root_path != root_path {
            return Err(LibraryError::InvalidData(
                "scan location belongs to a different root".to_string(),
            ));
        }
        let folder_id = location
            .folder_relative_path
            .as_deref()
            .map(|folder| {
                transaction.query_row(
                    "SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2",
                    params![root_id, folder],
                    |result| result.get::<_, i64>(0),
                )
            })
            .transpose()?;
        transaction.execute(
            "INSERT INTO sound_locations(sound_id, root_id, generation, folder_id, relative_path)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(sound_id, root_id, generation) DO UPDATE SET
                 folder_id = excluded.folder_id, relative_path = excluded.relative_path",
            params![
                sound_id,
                root_id,
                generation,
                folder_id,
                location.relative_path,
            ],
        )?;
    }
    Ok(())
}

fn apply_root_scan_batch(
    connection: &mut Connection,
    root_path: &str,
    generation: i64,
    folders: Vec<FolderRecord>,
    sounds: Vec<SoundRecord>,
) -> Result<(), LibraryError> {
    if !verify_root_scan(connection, root_path, generation)? {
        return Err(LibraryError::InvalidData(
            "root scan generation is no longer active".to_string(),
        ));
    }
    let transaction = connection.transaction()?;
    let root_id: i64 =
        transaction.query_row("SELECT id FROM roots WHERE path = ?1", [root_path], |row| {
            row.get(0)
        })?;
    for folder in &folders {
        if folder.root_path != root_path {
            return Err(LibraryError::InvalidData(
                "scan folder belongs to a different root".to_string(),
            ));
        }
        ensure_scan_folder(&transaction, root_id, generation, folder)?;
    }
    for sound in sounds {
        insert_scan_sound(&transaction, root_id, root_path, generation, sound)?;
    }
    transaction.commit()?;
    Ok(())
}

fn finish_root_scan(
    connection: &mut Connection,
    root_path: &str,
    generation: i64,
) -> Result<bool, LibraryError> {
    if !verify_root_scan(connection, root_path, generation)? {
        return Ok(false);
    }
    let transaction = connection.transaction()?;
    reconcile_legacy_generated_tabs(&transaction, root_path, generation)?;
    let changed = transaction.execute(
        "UPDATE roots SET active_generation = ?2 WHERE path = ?1",
        params![root_path, generation],
    )?;
    let root_id: i64 =
        transaction.query_row("SELECT id FROM roots WHERE path = ?1", [root_path], |row| {
            row.get(0)
        })?;
    transaction.execute(
        "DELETE FROM sound_locations WHERE root_id = ?1 AND generation <> ?2",
        params![root_id, generation],
    )?;
    transaction.execute(
        "DELETE FROM folder_presence
         WHERE generation <> ?2
           AND folder_id IN (SELECT id FROM folders WHERE root_id = ?1)",
        params![root_id, generation],
    )?;
    transaction.execute(
        &format!(
            "UPDATE hotkey_bindings
             SET normalized = NULL, state = 'needs_attention',
                 issue = 'sound is no longer in the active library'
             WHERE state = 'active' AND sound_id IS NOT NULL
               AND NOT EXISTS(
                   SELECT 1 FROM sounds AS sound
                   WHERE sound.rowid = hotkey_bindings.sound_id AND {LIVE_SOUND_FILTER}
               )
               AND NOT EXISTS(
                   SELECT 1 FROM sound_locations AS staged_location
                   WHERE staged_location.sound_id = hotkey_bindings.sound_id
               )"
        ),
        [],
    )?;
    transaction.execute(
        "DELETE FROM meta WHERE key = ?1",
        [scan_meta_key(root_path)],
    )?;
    transaction.commit()?;
    Ok(changed == 1)
}

fn reconcile_legacy_generated_tabs(
    transaction: &Transaction<'_>,
    root_path: &str,
    generation: i64,
) -> Result<(), LibraryError> {
    transaction.execute(
        "INSERT INTO folder_prefs(folder_id, display_name, sibling_position, expanded)
         SELECT folder.id, legacy.name, legacy.position, 0
         FROM legacy_generated_tabs AS legacy
         JOIN roots AS root ON root.path = legacy.root_path
         JOIN folders AS folder ON folder.root_id = root.id
             AND folder.relative_path = legacy.relative_path
         JOIN folder_presence AS presence ON presence.folder_id = folder.id
             AND presence.generation = ?2
         WHERE legacy.root_path = ?1
         ON CONFLICT(folder_id) DO UPDATE SET
             display_name = excluded.display_name,
             sibling_position = excluded.sibling_position",
        params![root_path, generation],
    )?;
    transaction.execute(
        "WITH targets(tab_id, folder_id, root_id) AS (
             SELECT legacy.id, folder.id, root.id
             FROM legacy_generated_tabs AS legacy
             JOIN roots AS root ON root.path = legacy.root_path
             JOIN folders AS folder ON folder.root_id = root.id
                 AND folder.relative_path = legacy.relative_path
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = ?2
             WHERE legacy.root_path = ?1
         )
         INSERT INTO folder_overrides(folder_id, sound_id, action)
         SELECT target.folder_id, membership.sound_id, 'include'
         FROM targets AS target
         JOIN legacy_generated_memberships AS membership ON membership.tab_id = target.tab_id
         WHERE NOT EXISTS (
             SELECT 1
             FROM folder_closure AS closure
             JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
             WHERE closure.ancestor_id = target.folder_id
               AND location.root_id = target.root_id
               AND location.generation = ?2
               AND location.sound_id = membership.sound_id
         )
         ON CONFLICT(folder_id, sound_id) DO UPDATE SET action = excluded.action",
        params![root_path, generation],
    )?;
    transaction.execute(
        "WITH targets(tab_id, folder_id, root_id) AS (
             SELECT legacy.id, folder.id, root.id
             FROM legacy_generated_tabs AS legacy
             JOIN roots AS root ON root.path = legacy.root_path
             JOIN folders AS folder ON folder.root_id = root.id
                 AND folder.relative_path = legacy.relative_path
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = ?2
             WHERE legacy.root_path = ?1
         ), physical(tab_id, folder_id, sound_id) AS (
             SELECT DISTINCT target.tab_id, target.folder_id, location.sound_id
             FROM targets AS target
             JOIN folder_closure AS closure ON closure.ancestor_id = target.folder_id
             JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                 AND location.root_id = target.root_id
                 AND location.generation = ?2
         )
         INSERT INTO folder_overrides(folder_id, sound_id, action)
         SELECT physical.folder_id, physical.sound_id, 'exclude'
         FROM physical
         WHERE NOT EXISTS (
             SELECT 1 FROM legacy_generated_memberships AS membership
             WHERE membership.tab_id = physical.tab_id
               AND membership.sound_id = physical.sound_id
         )
         ON CONFLICT(folder_id, sound_id) DO UPDATE SET action = excluded.action",
        params![root_path, generation],
    )?;
    transaction.execute(
        "INSERT INTO manual_tabs(public_id, name, position)
         SELECT legacy.public_id, legacy.name, legacy.position
         FROM legacy_generated_tabs AS legacy
         WHERE legacy.root_path = ?1
           AND NOT EXISTS (
               SELECT 1 FROM roots AS root
               JOIN folders AS folder ON folder.root_id = root.id
               JOIN folder_presence AS presence ON presence.folder_id = folder.id
                   AND presence.generation = ?2
               WHERE root.path = legacy.root_path
                 AND folder.relative_path = legacy.relative_path
           )",
        params![root_path, generation],
    )?;
    transaction.execute(
        "INSERT INTO manual_memberships(tab_id, sound_id, position)
         SELECT manual.id, membership.sound_id, membership.position
         FROM legacy_generated_tabs AS legacy
         JOIN manual_tabs AS manual ON manual.public_id = legacy.public_id
         JOIN legacy_generated_memberships AS membership ON membership.tab_id = legacy.id
         WHERE legacy.root_path = ?1
         ON CONFLICT(tab_id, sound_id) DO UPDATE SET position = excluded.position",
        [root_path],
    )?;
    transaction.execute(
        "DELETE FROM legacy_generated_tabs WHERE root_path = ?1",
        [root_path],
    )?;
    Ok(())
}

fn cancel_root_scan(
    connection: &mut Connection,
    root_path: &str,
    generation: i64,
) -> Result<bool, LibraryError> {
    if !verify_root_scan(connection, root_path, generation)? {
        return Ok(false);
    }
    let transaction = connection.transaction()?;
    let root_id: i64 =
        transaction.query_row("SELECT id FROM roots WHERE path = ?1", [root_path], |row| {
            row.get(0)
        })?;
    transaction.execute(
        "DELETE FROM sound_locations WHERE root_id = ?1 AND generation = ?2",
        params![root_id, generation],
    )?;
    transaction.execute(
        "DELETE FROM folder_presence
         WHERE generation = ?2 AND folder_id IN (SELECT id FROM folders WHERE root_id = ?1)",
        params![root_id, generation],
    )?;
    transaction.execute(
        "DELETE FROM meta WHERE key = ?1",
        [scan_meta_key(root_path)],
    )?;
    transaction.commit()?;
    Ok(true)
}

fn remove_root(connection: &mut Connection, root_path: &str) -> Result<bool, LibraryError> {
    let transaction = connection.transaction()?;
    transaction.execute(
        "DELETE FROM meta WHERE key = ?1",
        [scan_meta_key(root_path)],
    )?;
    let changed = transaction.execute("DELETE FROM roots WHERE path = ?1", [root_path])?;
    transaction.execute(
        "DELETE FROM sounds
         WHERE standalone = 0
           AND NOT EXISTS (
               SELECT 1 FROM sound_locations WHERE sound_id = sounds.rowid
           )
           AND NOT EXISTS (
               SELECT 1 FROM manual_memberships WHERE sound_id = sounds.rowid
           )",
        [],
    )?;
    transaction.commit()?;
    Ok(changed == 1)
}

fn apply_edit(connection: &mut Connection, edit: LibraryEdit) -> Result<bool, LibraryError> {
    let changed = match edit {
        LibraryEdit::UpsertManualTab(row) => connection.execute(
            "INSERT INTO manual_tabs(public_id, name, position) VALUES(?1, ?2, ?3)
             ON CONFLICT(public_id) DO UPDATE SET
                 name = excluded.name, position = excluded.position",
            params![row.public_id, row.name, usize_to_i64(row.position)?],
        )?,
        LibraryEdit::DeleteManualTab(public_id) => {
            connection.execute("DELETE FROM manual_tabs WHERE public_id = ?1", [public_id])?
        }
        LibraryEdit::SetManualMembership(row) => connection.execute(
            "INSERT INTO manual_memberships(tab_id, sound_id, position)
             SELECT tab.id, sound.rowid, ?3
             FROM manual_tabs AS tab, sounds AS sound
             WHERE tab.public_id = ?1 AND sound.public_id = ?2
             ON CONFLICT(tab_id, sound_id) DO UPDATE SET position = excluded.position",
            params![
                row.tab_public_id,
                row.sound_public_id,
                usize_to_i64(row.position)?
            ],
        )?,
        LibraryEdit::RemoveManualMembership {
            tab_public_id,
            sound_public_id,
        } => connection.execute(
            "DELETE FROM manual_memberships
             WHERE tab_id = (SELECT id FROM manual_tabs WHERE public_id = ?1)
               AND sound_id = (SELECT rowid FROM sounds WHERE public_id = ?2)",
            params![tab_public_id, sound_public_id],
        )?,
        LibraryEdit::ApplyManualMemberships {
            additions,
            removals,
        } => {
            let transaction = connection.transaction()?;
            let mut insert = transaction.prepare(
                "INSERT INTO manual_memberships(tab_id, sound_id, position)
                 SELECT tab.id, sound.rowid, ?3
                 FROM manual_tabs AS tab, sounds AS sound
                 WHERE tab.public_id = ?1 AND sound.public_id = ?2
                 ON CONFLICT(tab_id, sound_id) DO UPDATE SET position = excluded.position",
            )?;
            let mut delete = transaction.prepare(
                "DELETE FROM manual_memberships
                 WHERE tab_id = (SELECT id FROM manual_tabs WHERE public_id = ?1)
                   AND sound_id = (SELECT rowid FROM sounds WHERE public_id = ?2)",
            )?;
            let mut changed = 0_usize;
            for row in additions {
                changed = changed.saturating_add(insert.execute(params![
                    row.tab_public_id,
                    row.sound_public_id,
                    usize_to_i64(row.position)?
                ])?);
            }
            for (tab_public_id, sound_public_id) in removals {
                changed = changed
                    .saturating_add(delete.execute(params![tab_public_id, sound_public_id])?);
            }
            drop(insert);
            drop(delete);
            transaction.commit()?;
            changed
        }
        LibraryEdit::SetFolderOverride(row) => {
            let action = match row.action {
                FolderOverrideAction::Include => "include",
                FolderOverrideAction::Exclude => "exclude",
            };
            connection.execute(
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
            )?
        }
        LibraryEdit::ClearFolderOverride {
            root_path,
            folder_relative_path,
            sound_public_id,
        } => connection.execute(
            "DELETE FROM folder_overrides
             WHERE folder_id = (
                 SELECT folder.id FROM folders AS folder
                 JOIN roots AS root ON root.id = folder.root_id
                 WHERE root.path = ?1 AND folder.relative_path = ?2
             ) AND sound_id = (SELECT rowid FROM sounds WHERE public_id = ?3)",
            params![root_path, folder_relative_path, sound_public_id],
        )?,
        LibraryEdit::SetFolderPreferences {
            root_path,
            folder_relative_path,
            display_name,
            sibling_position,
            expanded,
        } => connection.execute(
            "INSERT INTO folder_prefs(folder_id, display_name, sibling_position, expanded)
             SELECT folder.id, ?3, ?4, ?5
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.relative_path = ?2
             ON CONFLICT(folder_id) DO UPDATE SET
                 display_name = excluded.display_name,
                 sibling_position = excluded.sibling_position,
                 expanded = excluded.expanded",
            params![
                root_path,
                folder_relative_path,
                display_name,
                sibling_position.map(usize_to_i64).transpose()?,
                i64::from(expanded),
            ],
        )?,
    };
    Ok(changed == 1)
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
        let (root_id, active_generation): (i64, i64) = transaction.query_row(
            "SELECT id, active_generation FROM roots WHERE path = ?1",
            [&row.root_path],
            |result| Ok((result.get(0)?, result.get(1)?)),
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
            "INSERT INTO folder_presence(folder_id, generation) VALUES(?1, ?2)",
            params![folder_id, active_generation],
        )?;
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
             public_id, name, search_name, path, source_path, duration_ms,
             volume, enabled, loudness_lufs, loudness_state, loudness_confidence,
             loudness_fingerprint, loudness_true_peak_dbtp, general_position, standalone
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
    )?;
    let mut insert_hotkey = transaction.prepare(
        "INSERT INTO hotkey_bindings(
             binding_id, sound_id, accelerator, normalized, state, issue
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let mut find_root =
        transaction.prepare("SELECT id, active_generation FROM roots WHERE path = ?1")?;
    let mut find_folder =
        transaction.prepare("SELECT id FROM folders WHERE root_id = ?1 AND relative_path = ?2")?;
    let mut insert_location = transaction.prepare(
        "INSERT INTO sound_locations(sound_id, root_id, generation, folder_id, relative_path)
         VALUES(?1, ?2, ?3, ?4, ?5)",
    )?;
    for row in rows {
        let standalone = row.locations.is_empty();
        let sound = row.sound;
        let duration_ms = sound
            .duration_ms
            .map(i64::try_from)
            .transpose()
            .map_err(|_| LibraryError::InvalidData("sound duration exceeds SQLite range".into()))?;
        let hotkey = sound
            .hotkey
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let normalized = hotkey.map(|value| value.to_ascii_lowercase());
        insert_sound.execute(params![
            &sound.id,
            &sound.name,
            sound.name.to_lowercase(),
            &sound.path,
            &sound.source_path,
            duration_ms,
            i64::from(sound.volume),
            i64::from(sound.enabled),
            sound.loudness_lufs,
            sound.loudness_analysis_state.as_str(),
            sound.loudness_confidence,
            sound.loudness_source_fingerprint,
            sound.loudness_true_peak_dbtp,
            usize_to_i64(row.general_position)?,
            i64::from(standalone),
        ])?;
        let sound_id = transaction.last_insert_rowid();
        if let (Some(accelerator), Some(normalized)) = (hotkey, normalized) {
            insert_hotkey.execute(params![
                &sound.id,
                sound_id,
                accelerator,
                normalized,
                "active",
                Option::<String>::None,
            ])?;
        }
        for location in row.locations {
            let (root_id, generation): (i64, i64) = find_root
                .query_row([&location.root_path], |result| {
                    Ok((result.get(0)?, result.get(1)?))
                })?;
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
                generation,
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

fn insert_legacy_generated_tabs(
    transaction: &Transaction<'_>,
    rows: Vec<LegacyGeneratedTabRecord>,
) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare(
        "INSERT INTO legacy_generated_tabs(
             public_id, root_path, relative_path, name, position
         ) VALUES(?1, ?2, ?3, ?4, ?5)",
    )?;
    for row in rows {
        statement.execute(params![
            row.public_id,
            row.root_path,
            row.relative_path,
            row.name,
            usize_to_i64(row.position)?
        ])?;
    }
    Ok(())
}

fn insert_legacy_generated_memberships(
    transaction: &Transaction<'_>,
    rows: Vec<LegacyGeneratedMembershipRecord>,
) -> Result<(), LibraryError> {
    for row in rows {
        transaction.execute(
            "INSERT INTO legacy_generated_memberships(tab_id, sound_id, position)
             SELECT tab.id, sound.rowid, ?3
             FROM legacy_generated_tabs AS tab, sounds AS sound
             WHERE tab.public_id = ?1 AND sound.public_id = ?2",
            params![
                row.tab_public_id,
                row.sound_public_id,
                usize_to_i64(row.position)?
            ],
        )?;
        if transaction.changes() != 1 {
            return Err(LibraryError::InvalidData(
                "legacy generated membership references a missing tab or sound".to_string(),
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

fn insert_hotkey_bindings(
    transaction: &Transaction<'_>,
    rows: Vec<HotkeyBindingRecord>,
) -> Result<(), LibraryError> {
    let mut insert = transaction.prepare(
        "INSERT INTO hotkey_bindings(
             binding_id, sound_id, control_action, accelerator, normalized, state, issue
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for binding in rows {
        let (sound_id, control_action) = match &binding.owner {
            HotkeyBindingOwner::Sound(public_id) => (
                Some(
                    transaction
                        .query_row(
                            "SELECT rowid FROM sounds WHERE public_id = ?1",
                            [public_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .optional()?
                        .ok_or_else(|| {
                            LibraryError::InvalidData(format!(
                                "unknown sound binding owner: {public_id}"
                            ))
                        })?,
                ),
                None,
            ),
            HotkeyBindingOwner::Control(action) => (None, Some(action.as_str())),
        };
        let state = if binding.normalized.is_some() {
            "active"
        } else {
            "needs_attention"
        };
        insert.execute(params![
            binding.binding_id,
            sound_id,
            control_action,
            binding.accelerator,
            binding.normalized,
            state,
            binding.issue,
        ])?;
    }
    Ok(())
}

fn search_query(search: &str) -> String {
    format!("\"{}\"", search.replace('"', "\"\""))
}

const SEARCH_FILTER: &str =
    "CASE WHEN ?1 = '' THEN 1 WHEN length(?1) < 3 THEN instr(sound.search_name, ?1) > 0 ELSE sound.rowid IN (SELECT rowid FROM sound_search WHERE sound_search MATCH ?2) END";

const LIVE_SOUND_FILTER: &str = "(sound.standalone = 1
      OR EXISTS(SELECT 1 FROM manual_memberships AS live_manual
                WHERE live_manual.sound_id = sound.rowid)
      OR EXISTS(SELECT 1 FROM sound_locations AS live_location
                JOIN roots AS live_root ON live_root.id = live_location.root_id
                WHERE live_location.sound_id = sound.rowid
                  AND live_location.generation = live_root.active_generation))";

const LIVE_SOUNDS_CTE: &str = "live(sound_id) AS (
         SELECT rowid FROM sounds WHERE standalone = 1
         UNION
         SELECT sound_id FROM manual_memberships
         UNION
         SELECT location.sound_id FROM roots AS root
         JOIN sound_locations AS location ON location.root_id = root.id
             AND location.generation = root.active_generation
)";
const SOUND_FIELDS: &str = "sound.public_id, sound.name, sound.path, sound.source_path,
    (SELECT binding.accelerator FROM hotkey_bindings AS binding
     WHERE binding.sound_id = sound.rowid AND binding.state = 'active'),
    sound.duration_ms, sound.volume, sound.enabled, sound.loudness_lufs,
    sound.loudness_state, sound.loudness_confidence, sound.loudness_fingerprint,
    sound.loudness_true_peak_dbtp";

fn count_sounds(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
) -> Result<usize, LibraryError> {
    let fts = search_query(search);
    let count: i64 = match scope {
        LibraryScope::General => connection.query_row(
            &format!(
                "WITH {LIVE_SOUNDS_CTE}
                 SELECT COUNT(*) FROM live
                 JOIN sounds AS sound ON sound.rowid = live.sound_id
                 WHERE {SEARCH_FILTER}"
            ),
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
                "WITH selected(folder_id, root_id, generation) AS (
                     SELECT folder.id, root.id, root.active_generation
                     FROM folders AS folder
                     JOIN roots AS root ON root.id = folder.root_id
                     JOIN folder_presence AS presence ON presence.folder_id = folder.id
                         AND presence.generation = root.active_generation
                     WHERE root.path = ?3 AND folder.relative_path = ?4
                 ), effective(sound_id) AS (
                     SELECT location.sound_id FROM selected
                     JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                     JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                         AND location.root_id = selected.root_id
                         AND location.generation = selected.generation
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
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let limit = usize_to_i64(PAGE_SIZE)?;
    let offset = usize_to_i64(offset)?;
    let fts = search_query(search);
    let fields = SOUND_FIELDS;
    let mut sounds = Vec::with_capacity(PAGE_SIZE);
    match scope {
        LibraryScope::General => {
            let sql = format!(
                "WITH {LIVE_SOUNDS_CTE}
                 SELECT {fields} FROM live
                 JOIN sounds AS sound ON sound.rowid = live.sound_id
                 WHERE {SEARCH_FILTER}
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
                "WITH selected(folder_id, root_id, generation) AS (
                     SELECT folder.id, root.id, root.active_generation
                     FROM folders AS folder
                     JOIN roots AS root ON root.id = folder.root_id
                     JOIN folder_presence AS presence ON presence.folder_id = folder.id
                         AND presence.generation = root.active_generation
                     WHERE root.path = ?3 AND folder.relative_path = ?4
                 ), effective(sound_id) AS (
                     SELECT location.sound_id FROM selected
                     JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                     JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                         AND location.root_id = selected.root_id
                         AND location.generation = selected.generation
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
    Ok(SoundPage { sounds })
}

fn load_sound(connection: &Connection, id: &str) -> Result<Option<Sound>, LibraryError> {
    let sql = format!(
        "SELECT {SOUND_FIELDS}
         FROM sounds AS sound
         WHERE public_id = ?1 AND {LIVE_SOUND_FILTER}"
    );
    connection
        .query_row(&sql, [id], sound_from_row)
        .optional()
        .map_err(LibraryError::from)
}

fn load_sound_by_path(connection: &Connection, path: &str) -> Result<Option<Sound>, LibraryError> {
    let sql = format!(
        "SELECT {SOUND_FIELDS}
         FROM sounds AS sound
         WHERE path = ?1 AND {LIVE_SOUND_FILTER}"
    );
    connection
        .query_row(&sql, [path], sound_from_row)
        .optional()
        .map_err(LibraryError::from)
}

fn load_sound_for_binding(
    connection: &Connection,
    binding_id: &str,
) -> Result<Option<Sound>, LibraryError> {
    let sql = format!(
        "SELECT {SOUND_FIELDS}
         FROM hotkey_bindings AS binding
         JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.binding_id = ?1 AND binding.state = 'active'
           AND {LIVE_SOUND_FILTER}"
    );
    connection
        .query_row(&sql, [binding_id], sound_from_row)
        .optional()
        .map_err(LibraryError::from)
}

fn update_sound(connection: &mut Connection, sound: Sound) -> Result<bool, LibraryError> {
    let search_name = sound.name.to_lowercase();
    let duration_ms = sound
        .duration_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| LibraryError::InvalidData("sound duration exceeds SQLite range".into()))?;
    let hotkey = sound
        .hotkey
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let normalized = hotkey.as_ref().map(|value| value.to_ascii_lowercase());
    let transaction = connection.transaction()?;
    let changed = transaction.execute(
        "UPDATE sounds SET
             name = ?2, search_name = ?3, path = ?4, source_path = ?5,
             duration_ms = ?6, volume = ?7, enabled = ?8, loudness_lufs = ?9,
             loudness_state = ?10, loudness_confidence = ?11,
             loudness_fingerprint = ?12, loudness_true_peak_dbtp = ?13
         WHERE public_id = ?1",
        params![
            &sound.id,
            &sound.name,
            search_name,
            &sound.path,
            &sound.source_path,
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
    if changed == 1 {
        transaction.execute(
            "DELETE FROM hotkey_bindings
             WHERE sound_id = (SELECT rowid FROM sounds WHERE public_id = ?1)",
            [&sound.id],
        )?;
        if let (Some(accelerator), Some(normalized)) = (hotkey, normalized) {
            transaction.execute(
                "INSERT INTO hotkey_bindings(
                     binding_id, sound_id, accelerator, normalized, state
                 ) SELECT ?1, rowid, ?2, ?3, 'active'
                   FROM sounds WHERE public_id = ?1",
                params![&sound.id, accelerator, normalized],
            )?;
        }
    }
    transaction.commit()?;
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

fn load_manual_tabs(connection: &Connection, page: usize) -> Result<ManualTabPage, LibraryError> {
    let count: i64 =
        connection.query_row("SELECT COUNT(*) FROM manual_tabs", [], |row| row.get(0))?;
    let total = usize::try_from(count)
        .map_err(|_| LibraryError::InvalidData("negative manual tab count".to_string()))?;
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let mut statement = connection.prepare(
        "SELECT public_id, name FROM manual_tabs
         ORDER BY position, id LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        |row| {
            Ok(ManualTabItem {
                public_id: row.get(0)?,
                name: row.get(1)?,
            })
        },
    )?;
    let mut tabs = Vec::with_capacity(PAGE_SIZE.min(total.saturating_sub(offset)));
    for tab in rows {
        tabs.push(tab?);
    }
    Ok(ManualTabPage { total, tabs })
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
                  EXISTS(SELECT 1 FROM folders AS child
                         JOIN folder_presence AS child_presence
                           ON child_presence.folder_id = child.id
                          AND child_presence.generation = root.active_generation
                         WHERE child.parent_id = folder.id)";
    let (total, folders) = if let Some(parent_relative_path) = parent_relative_path {
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             WHERE root.path = ?1 AND folder.parent_id = (
                 SELECT parent.id FROM folders AS parent
                 JOIN folder_presence AS parent_presence ON parent_presence.folder_id = parent.id
                     AND parent_presence.generation = root.active_generation
                 WHERE parent.root_id = root.id AND parent.relative_path = ?2
             )",
            params![root_path, parent_relative_path],
            |row| row.get(0),
        )?;
        let sql = format!(
            "SELECT {fields} FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE root.path = ?1 AND folder.parent_id = (
                 SELECT parent.id FROM folders AS parent
                 JOIN folder_presence AS parent_presence ON parent_presence.folder_id = parent.id
                     AND parent_presence.generation = root.active_generation
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
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             WHERE root.path = ?1 AND folder.parent_id IS NULL",
            [root_path],
            |row| row.get(0),
        )?;
        let sql = format!(
            "SELECT {fields} FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
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
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let sql = format!(
        "SELECT {SOUND_FIELDS}
         FROM hotkey_bindings AS binding
         JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active' AND {LIVE_SOUND_FILTER}
         ORDER BY general_position, public_id LIMIT ?1 OFFSET ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        sound_from_row,
    )?;
    let mut sounds = Vec::with_capacity(PAGE_SIZE);
    for sound in rows {
        sounds.push(sound?);
    }
    Ok(SoundPage { sounds })
}

fn load_hotkey_bindings_after(
    connection: &Connection,
    after: Option<&str>,
) -> Result<HotkeyBindingPage, LibraryError> {
    let sql = format!(
        "SELECT binding.binding_id, sound.public_id, binding.control_action,
                binding.accelerator, binding.normalized, binding.issue
         FROM hotkey_bindings AS binding
         LEFT JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active' AND (?1 IS NULL OR binding.binding_id > ?1)
           AND (binding.control_action IS NOT NULL OR {LIVE_SOUND_FILTER})
         ORDER BY binding.binding_id LIMIT ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![after, usize_to_i64(PAGE_SIZE)?], |row| {
        let sound_id: Option<String> = row.get(1)?;
        let control_action: Option<String> = row.get(2)?;
        let owner = match (sound_id, control_action) {
            (Some(id), None) => HotkeyBindingOwner::Sound(id),
            (None, Some(action)) => HotkeyBindingOwner::Control(action),
            _ => return Err(rusqlite::Error::InvalidQuery),
        };
        Ok(HotkeyBindingRecord {
            binding_id: row.get(0)?,
            owner,
            accelerator: row.get(3)?,
            normalized: row.get(4)?,
            issue: row.get(5)?,
        })
    })?;
    let mut bindings = Vec::with_capacity(PAGE_SIZE);
    for binding in rows {
        bindings.push(binding?);
    }
    Ok(HotkeyBindingPage { bindings })
}

fn load_hotkey_conflict(
    connection: &Connection,
    binding_id: &str,
    normalized: &str,
) -> Result<Option<String>, LibraryError> {
    let sql = format!(
        "SELECT sound.name, binding.control_action
         FROM hotkey_bindings AS binding
         LEFT JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active'
           AND binding.normalized = ?1
           AND binding.binding_id <> ?2
           AND (binding.control_action IS NOT NULL OR {LIVE_SOUND_FILTER})
         LIMIT 1"
    );
    let conflict = connection
        .query_row(&sql, params![normalized, binding_id], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
            ))
        })
        .optional()?;
    Ok(conflict.map(|(sound_name, control_action)| {
        if let Some(sound_name) = sound_name {
            format!("sound \"{sound_name}\"")
        } else if let Some(action) = control_action
            .as_deref()
            .and_then(ControlHotkeyAction::from_id)
        {
            format!("control action \"{}\"", action.title())
        } else {
            "another action".to_string()
        }
    }))
}

fn set_hotkey_binding(
    connection: &mut Connection,
    binding: HotkeyBindingRecord,
) -> Result<bool, LibraryError> {
    if binding.binding_id.trim().is_empty() || binding.accelerator.trim().is_empty() {
        return Err(LibraryError::InvalidData(
            "hotkey binding id and accelerator cannot be empty".to_string(),
        ));
    }
    if binding
        .normalized
        .as_deref()
        .is_some_and(|value| value.trim().is_empty())
    {
        return Err(LibraryError::InvalidData(
            "normalized hotkey cannot be empty".to_string(),
        ));
    }
    if let Some(normalized) = binding.normalized.as_deref() {
        let canonical = crate::hotkeys::canonicalize_hotkey_string(&binding.accelerator)
            .map_err(|error| LibraryError::InvalidData(error.to_string()))?;
        if normalized != canonical {
            return Err(LibraryError::InvalidData(
                "normalized hotkey must equal the canonical accelerator".to_string(),
            ));
        }
    }
    if binding.normalized.is_none() && binding.issue.as_deref().is_none_or(str::is_empty) {
        return Err(LibraryError::InvalidData(
            "a binding needing attention must include an issue".to_string(),
        ));
    }

    let transaction = connection.transaction()?;
    let (sound_id, control_action) = match &binding.owner {
        HotkeyBindingOwner::Sound(public_id) => {
            let sound_id = transaction
                .query_row(
                    "SELECT rowid FROM sounds WHERE public_id = ?1",
                    [public_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or_else(|| {
                    LibraryError::InvalidData(format!("unknown sound binding owner: {public_id}"))
                })?;
            transaction.execute(
                "DELETE FROM hotkey_bindings WHERE binding_id = ?1 OR sound_id = ?2",
                params![&binding.binding_id, sound_id],
            )?;
            (Some(sound_id), None)
        }
        HotkeyBindingOwner::Control(action) => {
            if action.trim().is_empty() {
                return Err(LibraryError::InvalidData(
                    "control hotkey action cannot be empty".to_string(),
                ));
            }
            transaction.execute(
                "DELETE FROM hotkey_bindings WHERE binding_id = ?1 OR control_action = ?2",
                params![&binding.binding_id, action],
            )?;
            (None, Some(action.as_str()))
        }
    };
    let state = if binding.normalized.is_some() {
        "active"
    } else {
        "needs_attention"
    };
    let issue = if binding.normalized.is_some() {
        None
    } else {
        binding.issue.as_deref()
    };
    let changed = transaction.execute(
        "INSERT INTO hotkey_bindings(
             binding_id, sound_id, control_action, accelerator, normalized, state, issue
         ) VALUES(?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            binding.binding_id,
            sound_id,
            control_action,
            binding.accelerator,
            binding.normalized,
            state,
            issue,
        ],
    )?;
    transaction.commit()?;
    Ok(changed == 1)
}

fn delete_hotkey_binding(connection: &Connection, binding_id: &str) -> Result<bool, LibraryError> {
    Ok(connection.execute(
        "DELETE FROM hotkey_bindings WHERE binding_id = ?1",
        [binding_id],
    )? == 1)
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
    let fields = SOUND_FIELDS;
    match scope {
        LibraryScope::General => connection
            .query_row(
                &format!(
                    "WITH {LIVE_SOUNDS_CTE}
                     SELECT {fields} FROM live
                     JOIN sounds AS sound ON sound.rowid = live.sound_id
                     WHERE {SEARCH_FILTER}
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
                    "WITH selected(folder_id, root_id, generation) AS (
                         SELECT folder.id, root.id, root.active_generation
                         FROM folders AS folder
                         JOIN roots AS root ON root.id = folder.root_id
                         JOIN folder_presence AS presence ON presence.folder_id = folder.id
                             AND presence.generation = root.active_generation
                         WHERE root.path = ?3 AND folder.relative_path = ?4
                     ), effective(sound_id) AS (
                         SELECT location.sound_id FROM selected
                         JOIN folder_closure AS closure ON closure.ancestor_id = selected.folder_id
                         JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                             AND location.root_id = selected.root_id
                             AND location.generation = selected.generation
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
