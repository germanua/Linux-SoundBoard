use crate::audio::pipewire_detection::PipeWireStatus;
use crate::audio::AudioPlayer;
use crate::commands::LoudnessCoordinators;
use crate::config::Config;
use crate::hotkeys::{HotkeyManager, HotkeyProjectionCoordinator};
use crate::library_store::LibraryStore;
use parking_lot::Mutex;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

/// Shared application state handed out as `Arc<AppState>` to UI, hotkey, and
/// IPC paths.
///
/// ## Lock ordering
///
/// When multiple locks must be acquired in the same call chain, always take
/// them in this order to avoid deadlocks:
///
/// 1. `config`
/// 2. `hotkeys`
/// 3. `pipewire_status`
///
/// Never acquire a lower-numbered lock while holding a higher-numbered one.
/// `player` owns no `Mutex` fields visible here — it uses an internal channel
/// to the PipeWire engine loop and is always safe to call while holding any of
/// the above locks.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<Config>>,
    pub library: LibraryStore,
    pub player: Arc<AudioPlayer>,
    pub hotkeys: Arc<Mutex<HotkeyManager>>,
    pub hotkey_projection: HotkeyProjectionCoordinator,
    pub manual_tabs: Arc<Mutex<Vec<crate::library_store::ManualTabItem>>>,
    pub pipewire_status: Arc<Mutex<PipeWireStatus>>,
    /// Debounce state for `commands::play_sound_async`. Prevents hotkey
    /// auto-repeat from dispatching the same sound twice within 30 ms.
    /// Owned here rather than as a process-global so that tests can give each
    /// test case an independent instance.
    pub play_dispatch_debounce: Arc<Mutex<Option<(Instant, String)>>>,
    /// Per-instance loudness analysis coordinators. Tracks in-flight backfill
    /// and refinement jobs without process-global state.
    pub loudness_coordinators: LoudnessCoordinators,
    /// Which sound each shared hotkey played last, keyed by the binding a
    /// press arrives as. Owned here rather than as a process-global so tests
    /// can give each case an independent cursor.
    pub hotkey_group_cursor: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Set once on the first successful play. Used by `play_sound_async` to
    /// record a one-shot diagnostic phase without process-global state.
    pub first_playback_recorded: Arc<AtomicBool>,
}
