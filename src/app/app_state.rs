use std::sync::{Arc, Mutex};

use crate::audio::player::AudioPlayer;
use crate::config::Config;
use crate::hotkeys::HotkeyManager;
use crate::pipewire::detection::PipeWireStatus;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Mutex<Config>>,
    pub player: Arc<AudioPlayer>,
    pub hotkeys: Arc<Mutex<HotkeyManager>>,
    pub pipewire_status: Arc<Mutex<PipeWireStatus>>,
}
