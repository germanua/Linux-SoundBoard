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

    Tab(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyBindingRecord {
    pub binding_id: String,
    pub owner: HotkeyBindingOwner,
    pub accelerator: String,
    pub normalized: Option<String>,
    pub issue: Option<String>,

    pub tab_scope: Option<String>,
}

pub fn scope_key(scope: &LibraryScope) -> String {
    match scope {
        LibraryScope::General => crate::app_meta::GENERAL_TAB_ID.to_string(),
        LibraryScope::ManualTab(public_id) => format!("tab:{public_id}"),
        LibraryScope::Folder {
            root_path,
            relative_path,
        } => format!("folder:{root_path}\u{1f}{relative_path}"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeyGroupMember {
    pub binding_id: String,
    pub sound_id: String,

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
    PositionForSound {
        scope: LibraryScope,
        search: String,
        sound_id: String,
        reply: mpsc::SyncSender<Result<Option<usize>, LibraryError>>,
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
    HotkeyBindingsForSound {
        sound_id: String,
        reply: mpsc::SyncSender<Result<Vec<HotkeyBindingRecord>, LibraryError>>,
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
        sounds_may_share: bool,
        tab_scope: Option<String>,

        excluded_sound_ids: Vec<String>,
        reply: mpsc::SyncSender<Result<Option<String>, LibraryError>>,
    },
    SetSoundHotkeys {
        sound_ids: Vec<String>,
        accelerator: Option<String>,
        tab_scope: Option<String>,
        reply: mpsc::SyncSender<Result<usize, LibraryError>>,
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
            | Request::PositionForSound { .. }
            | Request::HotkeyPage { .. }
            | Request::HotkeyBindingsAfter { .. }
            | Request::HotkeyBinding { .. }
            | Request::HotkeyGroup { .. }
            | Request::HotkeyBindingsForSound { .. }
            | Request::SetHotkeyBinding { .. }
            | Request::DeleteHotkeyBinding { .. }
            | Request::SetSoundHotkeys { .. } => (&mut state.control, CONTROL_QUEUE_CAPACITY),
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
            tab_scope: None,
        },
        Err(error) => HotkeyBindingRecord {
            binding_id,
            owner,
            accelerator: raw.to_string(),
            normalized: None,
            issue: Some(format!("invalid legacy binding: {error}")),
            tab_scope: None,
        },
    }
}
