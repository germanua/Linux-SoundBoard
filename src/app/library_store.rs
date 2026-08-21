use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use crate::config::{ControlHotkeyAction, LoudnessAnalysisState, Sound};

pub const PAGE_SIZE: usize = 256;
pub const MAX_BATCH_ROWS: usize = 512;
pub(crate) const DATABASE_SCHEMA_VERSION: i64 = 5;
/// Metadata tag written beside the schema version. Migrations keep their own
/// historical literals; this is only ever the current one.
pub(crate) const DATABASE_SCHEMA_FLAVOR: &str = "bounded-generation-v5";
pub(crate) const DATABASE_APPLICATION_ID: i64 = 0x4c53_4244;
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

/// A folder the user removed from the library. Carries its root so it can be
/// restored without walking the tree it is currently absent from.
#[derive(Debug, Clone)]
pub struct HiddenFolderItem {
    pub root_path: String,
    pub relative_path: String,
    pub name: String,
}

#[derive(Debug)]
pub struct HiddenFolderPage {
    pub total: usize,
    pub folders: Vec<HiddenFolderItem>,
}

#[derive(Debug, Clone)]
pub struct ManualTabItem {
    pub public_id: String,
    pub name: String,
    pub sound_count: usize,
    pub position: usize,
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

/// One sound that answers to a chord, as seen from a press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyGroupMember {
    pub binding_id: String,
    pub sound_id: String,
    /// The tab this binding is live in; `None` means every tab.
    pub tab_scope: Option<String>,
}

#[derive(Debug)]
pub struct HotkeyBindingPage {
    pub bindings: Vec<HotkeyBindingRecord>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoudnessStats {
    pub total: usize,
    pub pending: usize,
    pub estimated: usize,
    pub refined: usize,
    pub unavailable: usize,
    pub missing: usize,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LibraryStats {
    pub sounds: usize,
    pub roots: usize,
    pub manual_tabs: usize,
    pub active_hotkeys: usize,
}

#[derive(Debug)]
pub struct LoudnessUpdate {
    pub sound_id: String,
    pub lufs: Option<f64>,
    pub state: LoudnessAnalysisState,
    pub confidence: Option<f32>,
    pub true_peak_dbtp: Option<f32>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FolderOverrideAction {
    Include,
    Exclude,
}

#[derive(Debug, Clone)]
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
    ClearFolderOverrides {
        root_path: String,
        folder_relative_path: String,
        sound_public_ids: Vec<String>,
    },
    SetFolderPreferences {
        root_path: String,
        folder_relative_path: String,
        display_name: Option<String>,
        sibling_position: Option<usize>,
        expanded: bool,
    },
    ReorderFolder {
        root_path: String,
        folder_relative_path: String,
        target_index: usize,
    },
    SetFolderExpanded {
        root_path: String,
        folder_relative_path: String,
        expanded: bool,
    },
    SetFolderHidden {
        root_path: String,
        folder_relative_path: String,
        hidden: bool,
    },
    SetFolderDisplayName {
        root_path: String,
        folder_relative_path: String,
        display_name: Option<String>,
    },
    MoveFolder {
        root_path: String,
        folder_relative_path: String,
        direction: i32,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SearchGeneration {
    owner: u64,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PagePriority {
    Visible,
    Prefetch,
}

enum Request {
    Count {
        scope: LibraryScope,
        search: String,
        query_generation: Option<SearchGeneration>,
        reply: mpsc::SyncSender<Result<usize, LibraryError>>,
    },
    Page {
        scope: LibraryScope,
        search: String,
        page: usize,
        query_generation: Option<SearchGeneration>,
        priority: PagePriority,
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
    HotkeyBinding {
        binding_id: String,
        reply: mpsc::SyncSender<Result<Option<HotkeyBindingRecord>, LibraryError>>,
    },
    HotkeyGroup {
        binding_id: String,
        reply: mpsc::SyncSender<Result<Vec<HotkeyGroupMember>, LibraryError>>,
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
    LoudnessStats {
        reply: mpsc::SyncSender<Result<LoudnessStats, LibraryError>>,
    },
    LibraryStats {
        reply: mpsc::SyncSender<Result<LibraryStats, LibraryError>>,
    },
    LoudnessBackfillAfter {
        after: Option<String>,
        reply: mpsc::SyncSender<Result<SoundPage, LibraryError>>,
    },
    LoudnessRefinementCandidates {
        force: bool,
        after: Option<String>,
        limit: usize,
        reply: mpsc::SyncSender<Result<SoundPage, LibraryError>>,
    },
    ApplyLoudnessUpdates {
        updates: Vec<LoudnessUpdate>,
        reply: mpsc::SyncSender<Result<usize, LibraryError>>,
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
    HiddenFolders {
        page: usize,
        reply: mpsc::SyncSender<Result<HiddenFolderPage, LibraryError>>,
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

impl Request {
    fn search_generation(&self) -> Option<SearchGeneration> {
        match self {
            Self::Count {
                query_generation, ..
            }
            | Self::Page {
                query_generation, ..
            } => *query_generation,
            _ => None,
        }
    }
}

#[derive(Default)]
struct QueueState {
    control: VecDeque<Request>,
    visible: VecDeque<Request>,
    maintenance: VecDeque<Request>,
    search_generations: HashMap<u64, u64>,
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
        if let Some(query) = request.search_generation() {
            if state
                .search_generations
                .get(&query.owner)
                .is_some_and(|generation| query.generation < *generation)
            {
                return Ok(());
            }
            if state.search_generations.get(&query.owner) != Some(&query.generation) {
                state
                    .search_generations
                    .insert(query.owner, query.generation);
                state.visible.retain(|queued| {
                    !queued.search_generation().is_some_and(|generation| {
                        generation.owner == query.owner && generation.generation < query.generation
                    })
                });
                state.maintenance.retain(|queued| {
                    !queued.search_generation().is_some_and(|generation| {
                        generation.owner == query.owner && generation.generation < query.generation
                    })
                });
            }
        }
        let (queue, capacity) = match request {
            Request::SoundById { .. }
            | Request::SoundByPath { .. }
            | Request::SoundForBinding { .. }
            | Request::Adjacent { .. }
            | Request::HotkeyPage { .. }
            | Request::HotkeyBindingsAfter { .. }
            | Request::HotkeyBinding { .. }
            | Request::HotkeyGroup { .. }
            | Request::SetHotkeyBinding { .. }
            | Request::DeleteHotkeyBinding { .. } => (&mut state.control, CONTROL_QUEUE_CAPACITY),
            Request::HotkeyConflict { .. }
            | Request::LoudnessStats { .. }
            | Request::LibraryStats { .. }
            | Request::LoudnessBackfillAfter { .. }
            | Request::LoudnessRefinementCandidates { .. } => {
                (&mut state.control, CONTROL_QUEUE_CAPACITY)
            }
            Request::Count { .. }
            | Request::Page {
                priority: PagePriority::Visible,
                ..
            }
            | Request::Roots { .. }
            | Request::FolderChildren { .. }
            | Request::HiddenFolders { .. }
            | Request::ManualTabs { .. }
            | Request::Edit { .. }
            | Request::UpdateSound { .. }
            | Request::DeleteSound { .. } => (&mut state.visible, VISIBLE_QUEUE_CAPACITY),
            Request::ApplyLoudnessUpdates { .. } => (&mut state.visible, VISIBLE_QUEUE_CAPACITY),
            Request::BeginRootScan { .. }
            | Request::FinishRootScan { .. }
            | Request::CancelRootScan { .. }
            | Request::RemoveRoot { .. } => (&mut state.visible, VISIBLE_QUEUE_CAPACITY),
            Request::Page {
                priority: PagePriority::Prefetch,
                ..
            }
            | Request::ApplyBatch { .. }
            | Request::RootScanBatch { .. } => (&mut state.maintenance, MAINTENANCE_QUEUE_CAPACITY),
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

    /// Takes the next request without blocking. `None` means every queue is
    /// drained, which is the worker's cue to run idle maintenance before it
    /// parks in `pop`.
    fn try_pop(&self) -> Option<Request> {
        let mut state = self.state.lock().ok()?;
        state
            .control
            .pop_front()
            .or_else(|| state.visible.pop_front())
            .or_else(|| state.maintenance.pop_front())
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
    pub fn open(path: PathBuf) -> Result<Self, LibraryError> {
        Self::open_inner(path, None)
    }

    pub fn open_authoritative(path: PathBuf, library_id: &str) -> Result<Self, LibraryError> {
        Self::open_inner(path, Some(library_id.to_string()))
    }

    fn open_inner(
        path: PathBuf,
        expected_library_id: Option<String>,
    ) -> Result<Self, LibraryError> {
        let queue = Arc::new(RequestQueue::default());
        let worker_queue = Arc::clone(&queue);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("library-db".to_string())
            .spawn(move || {
                let mut connection = match open_connection(&path) {
                    Ok(connection) => {
                        if let Some(expected) = expected_library_id.as_deref() {
                            let identity = connection
                                .query_row(
                                    "SELECT value FROM meta WHERE key = 'library_id'",
                                    [],
                                    |row| row.get::<_, String>(0),
                                )
                                .optional();
                            let ready = connection
                                .query_row(
                                    "SELECT value FROM meta WHERE key = 'database_ready'",
                                    [],
                                    |row| row.get::<_, String>(0),
                                )
                                .optional();
                            if !matches!(identity, Ok(Some(ref value)) if value == expected)
                                || !matches!(ready, Ok(Some(ref value)) if value == "1")
                            {
                                let _ =
                                    ready_tx
                                        .send(Err("library database identity changed before open"
                                            .to_string()));
                                return;
                            }
                        }
                        let _ = ready_tx.send(Ok(()));
                        connection
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error.to_string()));
                        return;
                    }
                };
                let mut counts_dirty = false;
                let mut counts_published_at: Option<std::time::Instant> = None;
                // open_connection has just optimized, so start the clock here.
                let mut optimized_at = Some(std::time::Instant::now());
                loop {
                    let request = match worker_queue.try_pop() {
                        Some(request) => request,
                        None => {
                            // Queues are drained. Republish the counts a
                            // mutation invalidated, then park for more work.
                            publish_library_counts_if_dirty(
                                &connection,
                                &mut counts_dirty,
                                &mut counts_published_at,
                            );
                            run_idle_optimize_if_due(&connection, &mut optimized_at);
                            match worker_queue.pop() {
                                Some(request) => request,
                                None => break,
                            }
                        }
                    };
                    counts_dirty |= request.changes_library_counts();
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
        self.count_request(scope, search, None)
    }

    pub(crate) fn count_coalesced(
        &self,
        owner: u64,
        generation: u64,
        scope: LibraryScope,
        search: &str,
    ) -> LibraryResponse<usize> {
        self.count_request(scope, search, Some(SearchGeneration { owner, generation }))
    }

    fn count_request(
        &self,
        scope: LibraryScope,
        search: &str,
        query_generation: Option<SearchGeneration>,
    ) -> LibraryResponse<usize> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::Count {
                scope,
                search: search.to_lowercase(),
                query_generation,
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
        self.page_request(scope, search, page, None, PagePriority::Visible)
    }

    pub(crate) fn page_coalesced(
        &self,
        owner: u64,
        generation: u64,
        scope: LibraryScope,
        search: &str,
        page: usize,
    ) -> LibraryResponse<SoundPage> {
        self.page_request(
            scope,
            search,
            page,
            Some(SearchGeneration { owner, generation }),
            PagePriority::Visible,
        )
    }

    pub(crate) fn prefetch_page_coalesced(
        &self,
        owner: u64,
        generation: u64,
        scope: LibraryScope,
        search: &str,
        page: usize,
    ) -> LibraryResponse<SoundPage> {
        self.page_request(
            scope,
            search,
            page,
            Some(SearchGeneration { owner, generation }),
            PagePriority::Prefetch,
        )
    }

    fn page_request(
        &self,
        scope: LibraryScope,
        search: &str,
        page: usize,
        query_generation: Option<SearchGeneration>,
        priority: PagePriority,
    ) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::Page {
                scope,
                search: search.to_lowercase(),
                page,
                query_generation,
                priority,
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

    pub fn hotkey_binding(&self, binding_id: &str) -> LibraryResponse<Option<HotkeyBindingRecord>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyBinding {
                binding_id: binding_id.to_string(),
                reply,
            },
            response,
        )
    }

    /// Every sound bound to the chord `binding_id` carries, ordered as the
    /// library lists them. One entry for an ordinary binding, several when the
    /// chord is shared.
    pub fn hotkey_group(&self, binding_id: &str) -> LibraryResponse<Vec<HotkeyGroupMember>> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::HotkeyGroup {
                binding_id: binding_id.to_string(),
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

    pub fn loudness_stats(&self) -> LibraryResponse<LoudnessStats> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::LoudnessStats { reply }, response)
    }

    pub fn stats(&self) -> LibraryResponse<LibraryStats> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::LibraryStats { reply }, response)
    }

    pub fn loudness_backfill_after(&self, after: Option<&str>) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::LoudnessBackfillAfter {
                after: after.map(str::to_string),
                reply,
            },
            response,
        )
    }

    pub fn loudness_refinement_candidates(
        &self,
        force: bool,
        after: Option<&str>,
        limit: usize,
    ) -> LibraryResponse<SoundPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(
            Request::LoudnessRefinementCandidates {
                force,
                after: after.map(str::to_string),
                limit: limit.min(MAX_BATCH_ROWS),
                reply,
            },
            response,
        )
    }

    pub fn apply_loudness_updates(&self, updates: Vec<LoudnessUpdate>) -> LibraryResponse<usize> {
        if updates.len() > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "loudness update exceeds {MAX_BATCH_ROWS} rows"
            ))));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::ApplyLoudnessUpdates { updates, reply }, response)
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

    pub fn clear_folder_overrides(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        sound_public_ids: Vec<String>,
    ) -> LibraryResponse<bool> {
        if sound_public_ids.len() > MAX_BATCH_ROWS {
            return LibraryResponse::ready(Err(LibraryError::InvalidData(format!(
                "folder override batches are limited to {MAX_BATCH_ROWS} rows"
            ))));
        }
        self.edit(LibraryEdit::ClearFolderOverrides {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            sound_public_ids,
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

    pub fn set_folder_expanded(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        expanded: bool,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderExpanded {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            expanded,
        })
    }

    /// Hides or restores a folder. Hiding takes the whole subtree and the
    /// sounds that live only inside it out of the library; nothing on disk is
    /// touched, and a rescan will not bring it back.
    pub fn set_folder_hidden(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        hidden: bool,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderHidden {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            hidden,
        })
    }

    pub fn hidden_folders(&self, page: usize) -> LibraryResponse<HiddenFolderPage> {
        let (reply, response) = mpsc::sync_channel(1);
        self.enqueue(Request::HiddenFolders { page, reply }, response)
    }

    pub fn set_folder_display_name(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        display_name: Option<&str>,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::SetFolderDisplayName {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            display_name: display_name.map(str::to_string),
        })
    }

    pub fn move_folder(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        direction: i32,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::MoveFolder {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            direction,
        })
    }

    /// Places a folder at `target_index` among its siblings. Unlike
    /// `set_folder_preferences` this writes only the ordering, so a drag cannot
    /// disturb a folder's display name or its expanded state.
    pub fn reorder_folder(
        &self,
        root_path: &str,
        folder_relative_path: &str,
        target_index: usize,
    ) -> LibraryResponse<bool> {
        self.edit(LibraryEdit::ReorderFolder {
            root_path: root_path.to_string(),
            folder_relative_path: folder_relative_path.to_string(),
            target_index,
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

/// How often the worker re-runs `PRAGMA optimize` while idle. SQLite advises
/// running it on open and periodically thereafter for a long-lived connection;
/// `open_connection` covers the open, this covers the rest of the session.
const OPTIMIZE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// Runs `PRAGMA optimize` if the interval has passed. Called from the store
/// worker once its queues drain, so it never runs while requests are waiting
/// and never runs on the GTK thread. Returns whether it ran.
fn run_idle_optimize_if_due(
    connection: &Connection,
    optimized_at: &mut Option<std::time::Instant>,
) -> bool {
    if optimized_at.is_some_and(|at| at.elapsed() < OPTIMIZE_INTERVAL) {
        return false;
    }
    *optimized_at = Some(std::time::Instant::now());
    if let Err(error) = connection.execute_batch("PRAGMA optimize;") {
        log::warn!("Idle PRAGMA optimize failed: {error}");
    }
    true
}

/// Shortest gap between two diagnostics recounts on the store worker.
const COUNTS_REPUBLISH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

impl Request {
    /// Whether handling this request can change the counts published to
    /// diagnostics: live sounds, roots, manual tabs, or active hotkey bindings.
    ///
    /// Bulk row writes are deliberately excluded, because a caller that waits on
    /// each batch drains the queue every time and would recount the whole table
    /// once per batch — measured at over 30 s for a 156k import that otherwise
    /// takes about 3 s.
    ///
    /// `RootScanBatch` rows are staged under a new generation and are not live
    /// until `FinishRootScan` flips it, so a batch cannot change a count anyway.
    /// `ApplyBatch` only builds a database offline — legacy migration and
    /// seeding — and startup publishes the counts once that database is opened.
    fn changes_library_counts(&self) -> bool {
        match self {
            Request::FinishRootScan { .. }
            | Request::CancelRootScan { .. }
            | Request::RemoveRoot { .. }
            | Request::Edit { .. }
            | Request::UpdateSound { .. }
            | Request::DeleteSound { .. }
            | Request::SetHotkeyBinding { .. }
            | Request::DeleteHotkeyBinding { .. } => true,
            Request::Count { .. }
            | Request::Page { .. }
            | Request::SoundById { .. }
            | Request::SoundByPath { .. }
            | Request::SoundForBinding { .. }
            | Request::Adjacent { .. }
            | Request::HotkeyPage { .. }
            | Request::HotkeyBindingsAfter { .. }
            | Request::HotkeyBinding { .. }
            | Request::HotkeyGroup { .. }
            | Request::HotkeyConflict { .. }
            | Request::LoudnessStats { .. }
            | Request::LibraryStats { .. }
            | Request::LoudnessBackfillAfter { .. }
            | Request::LoudnessRefinementCandidates { .. }
            | Request::ApplyLoudnessUpdates { .. }
            | Request::BeginRootScan { .. }
            | Request::RootScanBatch { .. }
            | Request::ApplyBatch { .. }
            | Request::Roots { .. }
            | Request::FolderChildren { .. }
            | Request::HiddenFolders { .. }
            | Request::ManualTabs { .. } => false,
        }
    }
}

/// Recomputes and publishes the library counts diagnostics reports, but only
/// when a mutation has landed since the last publication. Called from the store
/// worker once its queues drain, so a busy import pays nothing and a burst of
/// edits collapses into a single recount.
fn publish_library_counts_if_dirty(
    connection: &Connection,
    dirty: &mut bool,
    published_at: &mut Option<std::time::Instant>,
) -> Option<LibraryStats> {
    if !*dirty {
        return None;
    }
    // A bulk caller that waits on each write drains the queue between every
    // batch. Recounting each time made a 156k import take over 30 s instead of
    // about 3 s, so republishing is rate limited. The flag stays set, and the
    // next drain after the interval publishes the settled numbers.
    if published_at.is_some_and(|at| at.elapsed() < COUNTS_REPUBLISH_INTERVAL) {
        return None;
    }
    *dirty = false;
    *published_at = Some(std::time::Instant::now());
    match load_library_stats(connection) {
        Ok(stats) => {
            crate::diagnostics::set_library_counts(
                stats.sounds,
                stats.manual_tabs,
                stats.roots,
                stats.active_hotkeys,
            );
            Some(stats)
        }
        Err(error) => {
            log::warn!("Failed to refresh library diagnostics counts: {error}");
            None
        }
    }
}

fn handle_request(connection: &mut Connection, request: Request) {
    match request {
        Request::Count {
            scope,
            search,
            query_generation: _,
            reply,
        } => {
            let _ = reply.send(count_sounds(connection, &scope, &search));
        }
        Request::Page {
            scope,
            search,
            page,
            query_generation: _,
            priority: _,
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
        Request::HotkeyBinding { binding_id, reply } => {
            let _ = reply.send(load_hotkey_binding(connection, &binding_id));
        }
        Request::HotkeyGroup { binding_id, reply } => {
            let _ = reply.send(load_hotkey_group(connection, &binding_id));
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
        Request::LoudnessStats { reply } => {
            let _ = reply.send(load_loudness_stats(connection));
        }
        Request::LibraryStats { reply } => {
            let _ = reply.send(load_library_stats(connection));
        }
        Request::LoudnessBackfillAfter { after, reply } => {
            let _ = reply.send(load_loudness_backfill_after(connection, after.as_deref()));
        }
        Request::LoudnessRefinementCandidates {
            force,
            after,
            limit,
            reply,
        } => {
            let _ = reply.send(load_loudness_refinement_candidates(
                connection,
                force,
                after.as_deref(),
                limit,
            ));
        }
        Request::ApplyLoudnessUpdates { updates, reply } => {
            let _ = reply.send(apply_loudness_updates(connection, updates));
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
        Request::HiddenFolders { page, reply } => {
            let _ = reply.send(load_hidden_folders(connection, page));
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
    } else if schema_version == 2 {
        migrate_schema_2_to_3(&connection)?;
        migrate_schema_3_to_4(&connection)?;
        migrate_schema_4_to_5(&connection)?;
    } else if schema_version == 3 {
        migrate_schema_3_to_4(&connection)?;
        migrate_schema_4_to_5(&connection)?;
    } else if schema_version == 4 {
        migrate_schema_4_to_5(&connection)?;
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

/// Created verbatim by both `create_schema` and `migrate_schema_4_to_5`, so a
/// migrated library and a fresh one are the same database.
///
/// A binding owns exactly one of: a sound, a control action, or a tab it
/// activates. `tab_scope` names the tab a binding is live in, and NULL means
/// every tab — which is how every binding written before tab scoping is
/// stored, and why enabling the toggle changes nothing on its own. Scope keys
/// are `general`, `tab:<public id>`, or `folder:<root>\u{1f}<relative path>`;
/// the unit separator cannot occur in a path, so the two halves stay
/// unambiguous.
///
/// There is deliberately no unique index on `normalized`: several sounds may
/// share one chord. Rejecting a duplicate is a policy decision that depends on
/// the Settings toggles, so it lives in the command layer, which is also where
/// the message the user reads comes from.
const HOTKEY_BINDINGS_SCHEMA: &str = "\
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
         INSERT INTO meta(key, value) VALUES('schema_version', '5');
         INSERT INTO meta(key, value) VALUES('schema_flavor', 'bounded-generation-v5');
         PRAGMA user_version = 5;
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
    // The table is rebuilt rather than altered: SQLite cannot change a CHECK
    // constraint in place, and the old one allowed only a sound or a control
    // action. Renaming the old table first means the surviving table is created
    // from HOTKEY_BINDINGS_SCHEMA verbatim, so it is indistinguishable from a
    // freshly created one. Dropping the old table takes its indexes with it.
    connection.execute_batch(&format!(
        "BEGIN IMMEDIATE;
         ALTER TABLE hotkey_bindings RENAME TO hotkey_bindings_v4;
         {HOTKEY_BINDINGS_SCHEMA}
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
    ))?;
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
    let (root_id, old_generation): (i64, i64) = transaction.query_row(
        "SELECT id, active_generation FROM roots WHERE path = ?1",
        [root_path],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    convert_disappeared_custom_folders(&transaction, root_id, old_generation, generation)?;
    let changed = transaction.execute(
        "UPDATE roots SET active_generation = ?2 WHERE path = ?1",
        params![root_path, generation],
    )?;
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
        "DELETE FROM folders
         WHERE root_id = ?1
           AND NOT EXISTS (
               SELECT 1 FROM folder_presence
               WHERE folder_id = folders.id AND generation = ?2
           )",
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

fn convert_disappeared_custom_folders(
    transaction: &Transaction<'_>,
    root_id: i64,
    old_generation: i64,
    new_generation: i64,
) -> Result<(), LibraryError> {
    let next_position: i64 = transaction.query_row(
        "SELECT COALESCE(MAX(position) + 1, 0) FROM manual_tabs",
        [],
        |row| row.get(0),
    )?;
    transaction.execute(
        "WITH customized AS (
             SELECT folder.id, COALESCE(pref.display_name, folder.name) AS name,
                    ROW_NUMBER() OVER (ORDER BY folder.id) - 1 AS position_offset
             FROM folders AS folder
             JOIN folder_presence AS old_presence ON old_presence.folder_id = folder.id
                 AND old_presence.generation = ?2
             LEFT JOIN folder_presence AS new_presence ON new_presence.folder_id = folder.id
                 AND new_presence.generation = ?3
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE folder.root_id = ?1
               AND new_presence.folder_id IS NULL
               AND (
                   pref.folder_id IS NOT NULL
                   OR EXISTS (
                       SELECT 1 FROM folder_overrides
                       WHERE folder_id = folder.id
                   )
               )
         )
         INSERT INTO manual_tabs(public_id, name, position)
         SELECT 'converted-folder-' || id || '-' || ?3,
                name, ?4 + position_offset
         FROM customized",
        params![root_id, old_generation, new_generation, next_position],
    )?;
    transaction.execute(
        "WITH customized AS (
             SELECT folder.id
             FROM folders AS folder
             JOIN folder_presence AS old_presence ON old_presence.folder_id = folder.id
                 AND old_presence.generation = ?2
             LEFT JOIN folder_presence AS new_presence ON new_presence.folder_id = folder.id
                 AND new_presence.generation = ?3
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE folder.root_id = ?1
               AND new_presence.folder_id IS NULL
               AND (
                   pref.folder_id IS NOT NULL
                   OR EXISTS (
                       SELECT 1 FROM folder_overrides
                       WHERE folder_id = folder.id
                   )
               )
         ), effective(folder_id, sound_id) AS (
             SELECT customized.id, location.sound_id
             FROM customized
             JOIN folder_closure AS closure ON closure.ancestor_id = customized.id
             JOIN sound_locations AS location ON location.folder_id = closure.descendant_id
                 AND location.root_id = ?1 AND location.generation = ?2
             UNION
             SELECT customized.id, override.sound_id
             FROM customized
             JOIN folder_overrides AS override ON override.folder_id = customized.id
             WHERE override.action = 'include'
             EXCEPT
             SELECT customized.id, override.sound_id
             FROM customized
             JOIN folder_overrides AS override ON override.folder_id = customized.id
             WHERE override.action = 'exclude'
         ), ranked AS (
             SELECT effective.folder_id, effective.sound_id,
                    ROW_NUMBER() OVER (
                        PARTITION BY effective.folder_id
                        ORDER BY sound.general_position, sound.public_id
                    ) - 1 AS position
             FROM effective
             JOIN sounds AS sound ON sound.rowid = effective.sound_id
         )
         INSERT INTO manual_memberships(tab_id, sound_id, position)
         SELECT tab.id, ranked.sound_id, ranked.position
         FROM ranked
         JOIN manual_tabs AS tab
           ON tab.public_id = 'converted-folder-' || ranked.folder_id || '-' || ?3",
        params![root_id, old_generation, new_generation],
    )?;
    Ok(())
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
        LibraryEdit::ClearFolderOverrides {
            root_path,
            folder_relative_path,
            sound_public_ids,
        } => {
            let transaction = connection.transaction()?;
            let mut statement = transaction.prepare(
                "DELETE FROM folder_overrides
                 WHERE folder_id = (
                     SELECT folder.id FROM folders AS folder
                     JOIN roots AS root ON root.id = folder.root_id
                     WHERE root.path = ?1 AND folder.relative_path = ?2
                 ) AND sound_id = (SELECT rowid FROM sounds WHERE public_id = ?3)",
            )?;
            let mut changed = 0_usize;
            for sound_public_id in sound_public_ids {
                changed = changed.saturating_add(statement.execute(params![
                    root_path,
                    folder_relative_path,
                    sound_public_id
                ])?);
            }
            drop(statement);
            transaction.commit()?;
            changed
        }
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
        LibraryEdit::SetFolderExpanded {
            root_path,
            folder_relative_path,
            expanded,
        } => connection.execute(
            "INSERT INTO folder_prefs(folder_id, expanded)
             SELECT folder.id, ?3
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.relative_path = ?2
             ON CONFLICT(folder_id) DO UPDATE SET expanded = excluded.expanded",
            params![root_path, folder_relative_path, i64::from(expanded)],
        )?,
        LibraryEdit::SetFolderHidden {
            root_path,
            folder_relative_path,
            hidden,
        } => connection.execute(
            "INSERT INTO folder_prefs(folder_id, hidden)
             SELECT folder.id, ?3
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.relative_path = ?2
             ON CONFLICT(folder_id) DO UPDATE SET hidden = excluded.hidden",
            params![root_path, folder_relative_path, i64::from(hidden)],
        )?,
        LibraryEdit::SetFolderDisplayName {
            root_path,
            folder_relative_path,
            display_name,
        } => connection.execute(
            "INSERT INTO folder_prefs(folder_id, display_name)
             SELECT folder.id, ?3
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE root.path = ?1 AND folder.relative_path = ?2
             ON CONFLICT(folder_id) DO UPDATE SET display_name = excluded.display_name",
            params![root_path, folder_relative_path, display_name],
        )?,
        LibraryEdit::ReorderFolder {
            root_path,
            folder_relative_path,
            target_index,
        } => return reorder_folder(connection, &root_path, &folder_relative_path, target_index),
        LibraryEdit::MoveFolder {
            root_path,
            folder_relative_path,
            direction,
        } => return move_folder(connection, &root_path, &folder_relative_path, direction),
    };
    Ok(changed != 0)
}

/// Places a folder at `target_index` among its siblings, renumbering the whole
/// sibling run so the result is a dense 0..n ordering.
///
/// Only `sibling_position` is written. A drag must not disturb a folder's
/// display name or its expanded flag, which is why this does not go through
/// `set_folder_preferences`.
fn reorder_folder(
    connection: &mut Connection,
    root_path: &str,
    folder_relative_path: &str,
    target_index: usize,
) -> Result<bool, LibraryError> {
    let transaction = connection.transaction()?;
    let target_id = transaction
        .query_row(
            "SELECT folder.id
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             WHERE root.path = ?1 AND folder.relative_path = ?2",
            params![root_path, folder_relative_path],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(target_id) = target_id else {
        return Ok(false);
    };

    let mut statement = transaction.prepare(
        "WITH target AS (
             SELECT folder.id, folder.root_id, folder.parent_id, root.active_generation
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE folder.id = ?1
         )
         SELECT folder.id
         FROM folders AS folder
         JOIN target ON target.root_id = folder.root_id
             AND folder.parent_id IS target.parent_id
         JOIN folder_presence AS presence ON presence.folder_id = folder.id
             AND presence.generation = target.active_generation
         LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
         ORDER BY COALESCE(pref.sibling_position, folder.position), folder.id",
    )?;
    let mut siblings = Vec::new();
    for row in statement.query_map(params![target_id], |row| row.get::<_, i64>(0))? {
        siblings.push(row?);
    }
    drop(statement);

    let Some(current_index) = siblings.iter().position(|id| *id == target_id) else {
        return Ok(false);
    };
    let target_index = target_index.min(siblings.len().saturating_sub(1));
    if current_index == target_index {
        return Ok(false);
    }
    let moved = siblings.remove(current_index);
    siblings.insert(target_index, moved);

    {
        let mut upsert = transaction.prepare(
            "INSERT INTO folder_prefs(folder_id, sibling_position)
             VALUES(?1, ?2)
             ON CONFLICT(folder_id) DO UPDATE SET sibling_position = excluded.sibling_position",
        )?;
        for (position, id) in siblings.iter().enumerate() {
            upsert.execute(params![id, i64::try_from(position).unwrap_or(i64::MAX)])?;
        }
    }
    transaction.commit()?;
    Ok(true)
}

fn move_folder(
    connection: &mut Connection,
    root_path: &str,
    folder_relative_path: &str,
    direction: i32,
) -> Result<bool, LibraryError> {
    if !matches!(direction, -1 | 1) {
        return Err(LibraryError::InvalidData(
            "folder move direction must be -1 or 1".to_string(),
        ));
    }
    let transaction = connection.transaction()?;
    let target_id = transaction
        .query_row(
            "SELECT folder.id
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             WHERE root.path = ?1 AND folder.relative_path = ?2",
            params![root_path, folder_relative_path],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(target_id) = target_id else {
        return Ok(false);
    };
    let mut statement = transaction.prepare(
        "WITH target AS (
             SELECT folder.id, folder.root_id, folder.parent_id, root.active_generation
             FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             WHERE folder.id = ?1
         ), ordered AS (
             SELECT folder.id,
                    ROW_NUMBER() OVER (
                        ORDER BY COALESCE(pref.sibling_position, folder.position), folder.id
                    ) - 1 AS position
             FROM folders AS folder
             JOIN target ON target.root_id = folder.root_id
                 AND folder.parent_id IS target.parent_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = target.active_generation
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
         ), target_position AS (
             SELECT position FROM ordered WHERE id = ?1
         )
         SELECT id, position FROM ordered
         WHERE position = (SELECT position FROM target_position)
            OR position = (SELECT position FROM target_position) + ?2",
    )?;
    let rows = statement.query_map(params![target_id, direction], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut pair = Vec::with_capacity(2);
    for row in rows {
        pair.push(row?);
    }
    drop(statement);
    if pair.len() != 2 {
        return Ok(false);
    }
    let target_position = pair
        .iter()
        .find_map(|(id, position)| (*id == target_id).then_some(*position))
        .ok_or_else(|| LibraryError::InvalidData("folder move target disappeared".to_string()))?;
    let (adjacent_id, adjacent_position) = pair
        .iter()
        .find(|(id, _)| *id != target_id)
        .copied()
        .ok_or_else(|| LibraryError::InvalidData("folder move sibling disappeared".to_string()))?;
    transaction.execute(
        "INSERT INTO folder_prefs(folder_id, sibling_position)
         VALUES(?1, ?2), (?3, ?4)
         ON CONFLICT(folder_id) DO UPDATE SET sibling_position = excluded.sibling_position",
        params![target_id, adjacent_position, adjacent_id, target_position],
    )?;
    transaction.commit()?;
    Ok(true)
}

fn insert_roots(transaction: &Transaction<'_>, rows: Vec<RootRecord>) -> Result<(), LibraryError> {
    let mut statement = transaction.prepare(
        "INSERT INTO roots(path, position) VALUES(?1, ?2)
         ON CONFLICT(path) DO NOTHING",
    )?;
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
                  AND live_location.generation = live_root.active_generation
                  AND NOT EXISTS(SELECT 1 FROM folder_closure AS hidden_closure
                                 JOIN folder_prefs AS hidden_pref
                                     ON hidden_pref.folder_id = hidden_closure.ancestor_id
                                 WHERE hidden_closure.descendant_id = live_location.folder_id
                                   AND hidden_pref.hidden = 1)))";

/// True when a folder is hidden, or sits under a folder that is. Written
/// against `folder.id`, so a query must expose the folder it is testing under
/// that alias.
const HIDDEN_FOLDER_FILTER: &str = "EXISTS(
          SELECT 1 FROM folder_closure AS hidden_closure
          JOIN folder_prefs AS hidden_pref ON hidden_pref.folder_id = hidden_closure.ancestor_id
          WHERE hidden_closure.descendant_id = folder.id AND hidden_pref.hidden = 1)";

const SOUND_FIELDS: &str = "sound.public_id, sound.name, sound.path, sound.source_path,
    (SELECT binding.accelerator FROM hotkey_bindings AS binding
     WHERE binding.sound_id = sound.rowid AND binding.state = 'active'),
    sound.duration_ms, sound.volume, sound.enabled, sound.loudness_lufs,
    sound.loudness_state, sound.loudness_confidence, sound.loudness_fingerprint,
    sound.loudness_true_peak_dbtp";

fn load_loudness_stats(connection: &Connection) -> Result<LoudnessStats, LibraryError> {
    let sql = format!(
        "SELECT COUNT(*),
                COALESCE(SUM(sound.loudness_state = 'pending'), 0),
                COALESCE(SUM(sound.loudness_state = 'estimated'), 0),
                COALESCE(SUM(sound.loudness_state = 'refined'), 0),
                COALESCE(SUM(sound.loudness_state = 'unavailable'), 0),
                COALESCE(SUM(sound.loudness_lufs IS NULL
                    AND sound.loudness_state <> 'unavailable'), 0)
         FROM sounds AS sound WHERE {LIVE_SOUND_FILTER}"
    );
    let values = connection.query_row(&sql, [], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })?;
    let convert = |value: i64| {
        usize::try_from(value)
            .map_err(|_| LibraryError::InvalidData("negative loudness count".to_string()))
    };
    Ok(LoudnessStats {
        total: convert(values.0)?,
        pending: convert(values.1)?,
        estimated: convert(values.2)?,
        refined: convert(values.3)?,
        unavailable: convert(values.4)?,
        missing: convert(values.5)?,
    })
}

fn load_library_stats(connection: &Connection) -> Result<LibraryStats, LibraryError> {
    let sound_sql = format!("SELECT COUNT(*) FROM sounds AS sound WHERE {LIVE_SOUND_FILTER}");
    let sounds: i64 = connection.query_row(&sound_sql, [], |row| row.get(0))?;
    let roots: i64 = connection.query_row("SELECT COUNT(*) FROM roots", [], |row| row.get(0))?;
    let manual_tabs: i64 =
        connection.query_row("SELECT COUNT(*) FROM manual_tabs", [], |row| row.get(0))?;
    let hotkey_sql = format!(
        "SELECT COUNT(*) FROM hotkey_bindings AS binding
         LEFT JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active'
           AND (binding.control_action IS NOT NULL OR {LIVE_SOUND_FILTER})"
    );
    let active_hotkeys: i64 = connection.query_row(&hotkey_sql, [], |row| row.get(0))?;
    let convert = |value: i64| {
        usize::try_from(value)
            .map_err(|_| LibraryError::InvalidData("negative library count".to_string()))
    };
    Ok(LibraryStats {
        sounds: convert(sounds)?,
        roots: convert(roots)?,
        manual_tabs: convert(manual_tabs)?,
        active_hotkeys: convert(active_hotkeys)?,
    })
}

fn load_loudness_backfill_after(
    connection: &Connection,
    after: Option<&str>,
) -> Result<SoundPage, LibraryError> {
    let sql = format!(
        "SELECT {SOUND_FIELDS} FROM sounds AS sound
         WHERE sound.loudness_lufs IS NULL
           AND sound.loudness_state <> 'unavailable'
           AND {LIVE_SOUND_FILTER}
           AND (?1 IS NULL OR sound.public_id > ?1)
         ORDER BY sound.public_id LIMIT ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(params![after, usize_to_i64(PAGE_SIZE)?], sound_from_row)?;
    let mut sounds = Vec::with_capacity(PAGE_SIZE);
    for sound in rows {
        sounds.push(sound?);
    }
    Ok(SoundPage { sounds })
}

fn load_loudness_refinement_candidates(
    connection: &Connection,
    force: bool,
    after: Option<&str>,
    limit: usize,
) -> Result<SoundPage, LibraryError> {
    let (sql, threshold) = if force {
        (
            format!(
                "SELECT {SOUND_FIELDS} FROM sounds AS sound
                 WHERE sound.loudness_state = 'estimated'
                   AND sound.loudness_lufs IS NOT NULL
                   AND {LIVE_SOUND_FILTER}
                   AND (?1 IS NULL OR sound.public_id > ?1)
                 ORDER BY sound.public_id LIMIT ?3"
            ),
            0.0,
        )
    } else {
        (
            format!(
                "SELECT {SOUND_FIELDS} FROM sounds AS sound
                 WHERE sound.loudness_state = 'estimated'
                   AND sound.loudness_lufs IS NOT NULL
                   AND {LIVE_SOUND_FILTER}
                   AND COALESCE(sound.loudness_confidence, 0.0) <= ?2
                 ORDER BY COALESCE(sound.loudness_confidence, 0.0),
                          COALESCE(sound.duration_ms, 0) DESC, sound.public_id
                 LIMIT ?3"
            ),
            FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD_FOR_STORE,
        )
    };
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map(
        params![after, threshold, usize_to_i64(limit)?],
        sound_from_row,
    )?;
    let mut sounds = Vec::with_capacity(limit);
    for sound in rows {
        sounds.push(sound?);
    }
    Ok(SoundPage { sounds })
}

const FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD_FOR_STORE: f32 = 0.80;

fn apply_loudness_updates(
    connection: &mut Connection,
    updates: Vec<LoudnessUpdate>,
) -> Result<usize, LibraryError> {
    let transaction = connection.transaction()?;
    let mut statement = transaction.prepare(
        "UPDATE sounds SET loudness_lufs = ?2, loudness_state = ?3,
             loudness_confidence = ?4, loudness_true_peak_dbtp = ?5
         WHERE public_id = ?1",
    )?;
    let mut changed = 0_usize;
    for update in updates {
        changed = changed.saturating_add(statement.execute(params![
            update.sound_id,
            update.lufs,
            update.state.as_str(),
            update.confidence,
            update.true_peak_dbtp,
        ])?);
    }
    drop(statement);
    transaction.commit()?;
    Ok(changed)
}

fn count_sounds(
    connection: &Connection,
    scope: &LibraryScope,
    search: &str,
) -> Result<usize, LibraryError> {
    let fts = search_query(search);
    let count: i64 = match scope {
        LibraryScope::General => connection.query_row(
            &format!(
                "SELECT COUNT(*) FROM sounds AS sound
                 WHERE {LIVE_SOUND_FILTER} AND {SEARCH_FILTER}"
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
                "SELECT {fields} FROM sounds AS sound
                 WHERE {LIVE_SOUND_FILTER} AND {SEARCH_FILTER}
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
        "SELECT public_id, name,
                (SELECT COUNT(*) FROM manual_memberships AS membership
                 WHERE membership.tab_id = manual_tabs.id), position
         FROM manual_tabs
         ORDER BY position, id LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        |row| {
            let sound_count = usize::try_from(row.get::<_, i64>(2)?).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?;
            Ok(ManualTabItem {
                public_id: row.get(0)?,
                name: row.get(1)?,
                sound_count,
                position: usize::try_from(row.get::<_, i64>(3)?).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Integer,
                        Box::new(error),
                    )
                })?,
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
    // `child.root_id = root.id` is redundant for correctness (children always
    // share their parent's root) but required for performance: it lets the
    // EXISTS use `folders_parent_order`, whose leading column is `root_id`.
    // Without it SQLite scans every folder in the active generation once per
    // output row.
    let fields = "folder.id, folder.relative_path,
                  COALESCE(pref.display_name, folder.name), COALESCE(pref.expanded, 0),
                  EXISTS(SELECT 1 FROM folders AS child
                         JOIN folder_presence AS child_presence
                           ON child_presence.folder_id = child.id
                          AND child_presence.generation = root.active_generation
                         WHERE child.root_id = root.id AND child.parent_id = folder.id
                           AND NOT EXISTS(
                               SELECT 1 FROM folder_closure AS hidden_closure
                               JOIN folder_prefs AS hidden_pref
                                   ON hidden_pref.folder_id = hidden_closure.ancestor_id
                               WHERE hidden_closure.descendant_id = child.id
                                 AND hidden_pref.hidden = 1))";
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
             ) AND NOT EXISTS(
                 SELECT 1 FROM folder_closure AS hidden_closure
                 JOIN folder_prefs AS hidden_pref ON hidden_pref.folder_id = hidden_closure.ancestor_id
                 WHERE hidden_closure.descendant_id = folder.id AND hidden_pref.hidden = 1
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
             ) AND NOT {HIDDEN_FOLDER_FILTER}
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
             WHERE root.path = ?1 AND folder.parent_id IS NULL
                 AND NOT EXISTS(
                     SELECT 1 FROM folder_closure AS hidden_closure
                     JOIN folder_prefs AS hidden_pref
                         ON hidden_pref.folder_id = hidden_closure.ancestor_id
                     WHERE hidden_closure.descendant_id = folder.id AND hidden_pref.hidden = 1
                 )",
            [root_path],
            |row| row.get(0),
        )?;
        let sql = format!(
            "SELECT {fields} FROM folders AS folder
             JOIN roots AS root ON root.id = folder.root_id
             JOIN folder_presence AS presence ON presence.folder_id = folder.id
                 AND presence.generation = root.active_generation
             LEFT JOIN folder_prefs AS pref ON pref.folder_id = folder.id
             WHERE root.path = ?1 AND folder.parent_id IS NULL AND NOT {HIDDEN_FOLDER_FILTER}
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

/// Folders the user removed, newest-shallowest first so a hidden parent is
/// listed before anything nested under it.
fn load_hidden_folders(
    connection: &Connection,
    page: usize,
) -> Result<HiddenFolderPage, LibraryError> {
    let offset = page
        .checked_mul(PAGE_SIZE)
        .ok_or_else(|| LibraryError::InvalidData("page offset overflow".to_string()))?;
    let total: i64 = connection.query_row(
        "SELECT COUNT(*) FROM folder_prefs AS pref
         JOIN folders AS folder ON folder.id = pref.folder_id
         WHERE pref.hidden = 1",
        [],
        |row| row.get(0),
    )?;
    let mut statement = connection.prepare(
        "SELECT root.path, folder.relative_path, COALESCE(pref.display_name, folder.name)
         FROM folder_prefs AS pref
         JOIN folders AS folder ON folder.id = pref.folder_id
         JOIN roots AS root ON root.id = folder.root_id
         WHERE pref.hidden = 1
         ORDER BY root.path, folder.relative_path
         LIMIT ?1 OFFSET ?2",
    )?;
    let rows = statement.query_map(
        params![usize_to_i64(PAGE_SIZE)?, usize_to_i64(offset)?],
        |row| {
            Ok(HiddenFolderItem {
                root_path: row.get(0)?,
                relative_path: row.get(1)?,
                name: row.get(2)?,
            })
        },
    )?;
    let mut folders = Vec::new();
    for folder in rows {
        folders.push(folder?);
    }
    Ok(HiddenFolderPage {
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

/// One row per chord: a chord several bindings share is projected under one of
/// them, because the backends can only be told about it once. Control actions
/// and tab hotkeys take that slot ahead of sounds — a press carrying a sound's
/// id can still be resolved back to its chord, while a control action reached
/// under another binding's id would simply never run.
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
           AND binding.binding_id = (
               SELECT representative.binding_id
               FROM hotkey_bindings AS representative
               LEFT JOIN sounds AS sound ON sound.rowid = representative.sound_id
               WHERE representative.state = 'active'
                 AND representative.normalized = binding.normalized
                 AND (representative.control_action IS NOT NULL
                      OR representative.target_tab IS NOT NULL
                      OR (sound.rowid IS NOT NULL AND {LIVE_SOUND_FILTER}))
               ORDER BY CASE WHEN representative.control_action IS NOT NULL THEN 0
                             WHEN representative.target_tab IS NOT NULL THEN 1
                             ELSE 2 END,
                        representative.binding_id
               LIMIT 1)
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

fn load_hotkey_group(
    connection: &Connection,
    binding_id: &str,
) -> Result<Vec<HotkeyGroupMember>, LibraryError> {
    // Keyed by the chord rather than by the binding, so every member of a
    // shared chord resolves to the same group no matter which binding the
    // backend reported. An unknown binding, or one still needing attention,
    // has a NULL accelerator here and matches nothing.
    let sql = format!(
        "SELECT binding.binding_id, sound.public_id, binding.tab_scope
         FROM hotkey_bindings AS binding
         JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.state = 'active'
           AND binding.normalized = (SELECT pressed.normalized
                                     FROM hotkey_bindings AS pressed
                                     WHERE pressed.binding_id = ?1
                                       AND pressed.state = 'active')
           AND {LIVE_SOUND_FILTER}
         ORDER BY sound.general_position, sound.public_id"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([binding_id], |row| {
        Ok(HotkeyGroupMember {
            binding_id: row.get(0)?,
            sound_id: row.get(1)?,
            tab_scope: row.get(2)?,
        })
    })?;
    let mut members = Vec::new();
    for member in rows {
        members.push(member?);
    }
    Ok(members)
}

fn load_hotkey_binding(
    connection: &Connection,
    binding_id: &str,
) -> Result<Option<HotkeyBindingRecord>, LibraryError> {
    let sql = format!(
        "SELECT binding.binding_id, sound.public_id, binding.control_action,
                binding.accelerator, binding.normalized, binding.issue
         FROM hotkey_bindings AS binding
         LEFT JOIN sounds AS sound ON sound.rowid = binding.sound_id
         WHERE binding.binding_id = ?1
           AND (binding.control_action IS NOT NULL OR {LIVE_SOUND_FILTER})"
    );
    connection
        .query_row(&sql, [binding_id], |row| {
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
        })
        .optional()
        .map_err(Into::into)
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
                    "SELECT {fields} FROM sounds AS sound
                     WHERE {LIVE_SOUND_FILTER} AND {SEARCH_FILTER}
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
                query_generation: None,
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
    fn prefetched_page_waits_behind_visible_work() {
        let queue = RequestQueue::default();
        let (prefetch_reply, _) = mpsc::sync_channel(1);
        let (visible_reply, _) = mpsc::sync_channel(1);
        queue
            .push(Request::Page {
                scope: LibraryScope::General,
                search: String::new(),
                page: 1,
                query_generation: None,
                priority: PagePriority::Prefetch,
                reply: prefetch_reply,
            })
            .expect("queue prefetch");
        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: String::new(),
                query_generation: None,
                reply: visible_reply,
            })
            .expect("queue visible");

        assert!(matches!(queue.pop(), Some(Request::Count { .. })));
        assert!(matches!(
            queue.pop(),
            Some(Request::Page {
                priority: PagePriority::Prefetch,
                ..
            })
        ));
    }

    #[test]
    fn request_queue_coalesces_older_search_generations() {
        let queue = RequestQueue::default();
        let (old_count_reply, old_count_response) = mpsc::sync_channel(1);
        let (old_page_reply, old_page_response) = mpsc::sync_channel(1);
        let (latest_reply, _) = mpsc::sync_channel(1);
        let old = SearchGeneration {
            owner: 7,
            generation: 1,
        };
        let latest = SearchGeneration {
            owner: 7,
            generation: 2,
        };

        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: "old".to_string(),
                query_generation: Some(old),
                reply: old_count_reply,
            })
            .expect("queue old count");
        queue
            .push(Request::Page {
                scope: LibraryScope::General,
                search: "old".to_string(),
                page: 0,
                query_generation: Some(old),
                priority: PagePriority::Prefetch,
                reply: old_page_reply,
            })
            .expect("queue old page");
        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: "latest".to_string(),
                query_generation: Some(latest),
                reply: latest_reply,
            })
            .expect("queue latest count");

        let state = queue.state.lock().expect("queue state");
        assert_eq!(state.visible.len(), 1);
        assert!(matches!(
            state.visible.front(),
            Some(Request::Count {
                query_generation: Some(value),
                ..
            }) if *value == latest
        ));
        drop(state);
        assert!(matches!(
            old_count_response.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
        assert!(matches!(
            old_page_response.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));

        assert!(matches!(queue.pop(), Some(Request::Count { .. })));
        let (late_old_reply, late_old_response) = mpsc::sync_channel(1);
        queue
            .push(Request::Count {
                scope: LibraryScope::General,
                search: "late-old".to_string(),
                query_generation: Some(old),
                reply: late_old_reply,
            })
            .expect("drop late old count");
        assert!(queue.state.lock().expect("queue state").visible.is_empty());
        assert!(matches!(
            late_old_response.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
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

#[cfg(test)]
mod idle_count_publication_tests {
    use super::*;

    fn reply<T>() -> mpsc::SyncSender<Result<T, LibraryError>> {
        mpsc::sync_channel(1).0
    }

    #[test]
    fn staged_scan_batches_do_not_dirty_the_counts() {
        // Scanned rows are staged under a new generation and are not live until
        // the scan finishes, so a batch cannot change any published count.
        let batch = Request::RootScanBatch {
            root_path: "/music".to_string(),
            generation: 1,
            folders: Vec::new(),
            sounds: Vec::new(),
            reply: reply(),
        };
        assert!(!batch.changes_library_counts());

        let finish = Request::FinishRootScan {
            root_path: "/music".to_string(),
            generation: 1,
            reply: reply(),
        };
        assert!(finish.changes_library_counts());
    }

    #[test]
    fn reads_and_loudness_writes_do_not_dirty_the_counts() {
        let page = Request::Page {
            scope: LibraryScope::General,
            search: String::new(),
            page: 0,
            query_generation: None,
            priority: PagePriority::Visible,
            reply: reply(),
        };
        assert!(!page.changes_library_counts());

        // Loudness values change, but not the number of sounds, tabs, roots or
        // active bindings, so a backfill must not trigger a recount per batch.
        let loudness = Request::ApplyLoudnessUpdates {
            updates: Vec::new(),
            reply: reply(),
        };
        assert!(!loudness.changes_library_counts());
    }

    #[test]
    fn deleting_a_sound_dirties_the_counts() {
        let delete = Request::DeleteSound {
            id: "sound-1".to_string(),
            reply: reply(),
        };
        assert!(delete.changes_library_counts());
    }

    #[test]
    fn bulk_row_writes_do_not_dirty_the_counts() {
        // A caller that waits on each batch drains the queue every time. Marking
        // these dirty recounts the whole table once per batch, which took a 156k
        // import from about 3 s to over 30 s.
        let batch = Request::ApplyBatch {
            batch: LibraryBatch::Roots(Vec::new()),
            reply: reply(),
        };
        assert!(!batch.changes_library_counts());
    }

    #[test]
    fn idle_optimize_runs_once_per_interval() {
        let dir = std::env::temp_dir().join(format!("lsb-idle-optimize-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let connection = open_connection(&dir.join("library.sqlite3")).expect("open connection");

        // open_connection already optimized on open, so the worker starts with a
        // timestamp and must not immediately run it again.
        let mut optimized_at = Some(std::time::Instant::now());
        assert!(
            !run_idle_optimize_if_due(&connection, &mut optimized_at),
            "optimize must not run again right after the connection opened"
        );

        optimized_at = std::time::Instant::now().checked_sub(OPTIMIZE_INTERVAL);
        assert!(
            run_idle_optimize_if_due(&connection, &mut optimized_at),
            "optimize must run once the interval has passed"
        );
        assert!(
            !run_idle_optimize_if_due(&connection, &mut optimized_at),
            "running it must restart the interval"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_burst_of_mutations_does_not_recount_per_drain() {
        let dir = std::env::temp_dir().join(format!("lsb-idle-burst-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let connection = open_connection(&dir.join("library.sqlite3")).expect("open connection");

        let mut dirty = true;
        let mut published_at = None;
        assert!(
            publish_library_counts_if_dirty(&connection, &mut dirty, &mut published_at).is_some()
        );

        // Another mutation lands immediately: the recount must wait out the
        // interval rather than run again on the very next drain.
        dirty = true;
        assert!(
            publish_library_counts_if_dirty(&connection, &mut dirty, &mut published_at).is_none(),
            "recount must be rate limited"
        );
        assert!(
            dirty,
            "a rate-limited recount must stay pending, not be dropped"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn counts_are_published_once_per_idle_period_and_only_when_dirty() {
        let dir = std::env::temp_dir().join(format!("lsb-idle-counts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create test dir");
        let connection = open_connection(&dir.join("library.sqlite3")).expect("open connection");

        let mut dirty = true;
        let mut published_at = None;
        let published = publish_library_counts_if_dirty(&connection, &mut dirty, &mut published_at)
            .expect("counts published");
        assert_eq!(published.sounds, 0);
        assert!(!dirty, "publishing must clear the dirty flag");
        assert!(published_at.is_some());

        assert!(
            publish_library_counts_if_dirty(&connection, &mut dirty, &mut published_at).is_none(),
            "a clean idle period must not re-query the database"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
