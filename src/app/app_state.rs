use crate::audio::pipewire_detection::PipeWireStatus;
use crate::audio::AudioPlayer;
use crate::commands::LoudnessCoordinators;
use crate::config::Config;
use crate::hotkeys::{HotkeyManager, HotkeyProjectionCoordinator};
use crate::library_store::LibraryStore;
use parking_lot::Mutex;
use std::sync::{atomic::AtomicBool, Arc};

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<Config>>,
    pub library: LibraryStore,
    pub player: Arc<AudioPlayer>,
    pub hotkeys: Arc<Mutex<HotkeyManager>>,
    pub hotkey_projection: HotkeyProjectionCoordinator,
    pub manual_tabs: Arc<Mutex<Vec<crate::library_store::ManualTabItem>>>,
    pub pipewire_status: Arc<Mutex<PipeWireStatus>>,

    pub loudness_coordinators: LoudnessCoordinators,
    pub hotkey_group_cursor: Arc<Mutex<std::collections::HashMap<String, String>>>,

    pub first_playback_recorded: Arc<AtomicBool>,
}
