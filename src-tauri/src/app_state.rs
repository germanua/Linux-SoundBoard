//! Shared application state for the GTK4 frontend.

use std::sync::{Arc, Mutex};

use crate::audio::player::AudioPlayer;
use crate::config::Config;
use crate::hotkeys::HotkeyManager;
use crate::pipewire::detection::PipeWireStatus;
use crate::pipewire::virtual_mic::VirtualMicStatus;

/// Central application state, cheaply clonable via Arc.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<Config>>,
    pub player: Arc<Mutex<AudioPlayer>>,
    pub hotkeys: Arc<Mutex<HotkeyManager>>,
    #[allow(dead_code)]
    pub mic_status: Arc<Mutex<VirtualMicStatus>>,
    pub pipewire_status: Arc<Mutex<PipeWireStatus>>,
}
