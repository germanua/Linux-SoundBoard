use crate::audio::pipewire_detection::PipeWireStatus;
use crate::audio::AudioPlayer;
use crate::commands::LoudnessCoordinators;
use crate::config::Config;
use crate::hotkeys::{HotkeyManager, HotkeyProjectionCoordinator};
use crate::library_store::LibraryStore;
use parking_lot::Mutex;
use std::sync::{atomic::AtomicBool, Arc};
use std::time::Instant;

/// Shared state, handed to the UI, hotkey and IPC paths as `Arc<AppState>`.
///
/// Lock order is `config` -> `hotkeys` -> `pipewire_status`; never take an
/// earlier one while holding a later one. `player` has no locks of its own
/// (it talks to the PipeWire loop over a channel) so it is safe under any.
///
/// The `Arc<Mutex<..>>` fields below are per-instance rather than process
/// globals so each test gets its own.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<Config>>,
    pub library: LibraryStore,
    pub player: Arc<AudioPlayer>,
    pub hotkeys: Arc<Mutex<HotkeyManager>>,
    pub hotkey_projection: HotkeyProjectionCoordinator,
    pub manual_tabs: Arc<Mutex<Vec<crate::library_store::ManualTabItem>>>,
    pub pipewire_status: Arc<Mutex<PipeWireStatus>>,
    /// Debounce for `commands::play_sound_async`: swallows hotkey auto-repeat
    /// firing the same sound twice inside 30 ms.
    pub play_dispatch_debounce: Arc<Mutex<Option<(Instant, String)>>>,
    /// In-flight loudness backfill and refinement jobs.
    pub loudness_coordinators: LoudnessCoordinators,
    /// Round-robin cursor for shared hotkeys: last sound played, keyed by the
    /// binding the press arrives as.
    pub hotkey_group_cursor: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// Latch so the first-play diagnostic is only recorded once.
    pub first_playback_recorded: Arc<AtomicBool>,
}
