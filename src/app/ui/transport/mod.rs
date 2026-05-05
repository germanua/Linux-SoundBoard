use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Entry, Label, Scale, SearchEntry, ToggleButton, Widget};
use libadwaita as adw;

use crate::app_state::AppState;

use super::sound_list::NavigationSound;

mod build;
mod helpers;
mod playback;
mod scrub;
mod signals;

type SoundListProvider = Box<dyn Fn() -> Vec<NavigationSound> + Send + Sync>;
type LibraryChangedCallback = Rc<dyn Fn() + 'static>;
type ListStyleChangedCallback = Rc<dyn Fn(String) + 'static>;
const TRANSPORT_BUTTON_SIZE: i32 = 31;
const SLOW_GTK_CALLBACK_THRESHOLD_MS: u128 = 16;

#[derive(Clone)]
struct ActiveTrack {
    sound_id: String,
    sound_duration_ms: Option<u64>,
    play_id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrubInput {
    Pointer,
    Keyboard,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct ScrubInteraction {
    active: bool,
    input: Option<ScrubInput>,
    preview_position_ms: Option<u64>,
    pending_seek_position_ms: Option<u64>,
    pending_seek_sound_id: Option<String>,
    pending_seek_deadline_ms: Option<u64>,
    last_committed_position_ms: Option<u64>,
    last_committed_sound_id: Option<String>,
}

const SEEK_SETTLE_TOLERANCE_MS: u64 = 100;
const PENDING_SEEK_TIMEOUT_MS: u64 = 800;
const DEFAULT_SCRUB_DURATION_MS: u64 = 30_000;

#[derive(Clone)]
pub struct TransportBar {
    inner: Rc<TransportInner>,
}

struct TransportInner {
    widget: GtkBox,
    play_btn: ToggleButton,
    stop_btn: Button,
    prev_btn: Button,
    next_btn: Button,
    scrub: Scale,
    time_label: Label,
    dur_label: Label,
    track_name_label: Label,
    local_vol: Scale,
    local_vol_label: Label,
    local_vol_entry: Entry,
    mic_vol: Scale,
    mic_vol_label: Label,
    mic_vol_entry: Entry,
    headphones_btn: ToggleButton,
    mic_btn: ToggleButton,
    playmode_btn: Button,
    refresh_btn: Button,
    search_entry: SearchEntry,
    settings_btn: Button,
    active_track: RefCell<Option<ActiveTrack>>,
    scrub_interaction: RefCell<ScrubInteraction>,
    scrub_commit_timeout: RefCell<Option<glib::SourceId>>,
    scrub_timer_id: RefCell<Option<glib::SourceId>>,
    suppress_headphones_toggle: Cell<bool>,
    suppress_mic_toggle: Cell<bool>,
    continue_suppressed_play_id: RefCell<Option<String>>,
    last_track_sound_id: RefCell<Option<String>>,
    state: Arc<AppState>,
    sound_list_provider: Mutex<Option<SoundListProvider>>,
    toast_sender: Mutex<Option<std::sync::mpsc::Sender<String>>>,
    on_library_changed: RefCell<Option<LibraryChangedCallback>>,
    on_list_style_changed: RefCell<Option<ListStyleChangedCallback>>,
    settings_dialog: RefCell<Option<adw::PreferencesDialog>>,
}

impl TransportBar {
    pub fn widget(&self) -> &Widget {
        self.inner.widget.upcast_ref()
    }
}

impl Drop for TransportBar {
    fn drop(&mut self) {
        self.cleanup();
    }
}
