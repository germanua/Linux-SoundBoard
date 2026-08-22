use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Button, Entry, Label, Scale, SearchEntry, ToggleButton, Widget};

use crate::app_state::AppState;

use super::sound_list::NavigationContext;

mod build;
mod helpers;
mod playback;
mod scrub;
mod signals;

type SoundListProvider = Box<dyn Fn() -> NavigationContext + 'static>;
type HasSoundsChecker = Box<dyn Fn() -> bool + 'static>;
type LibraryChangedCallback = Rc<dyn Fn() + 'static>;
type ListStyleChangedCallback = Rc<dyn Fn(String) + 'static>;
type SettingsRequestedCallback = Rc<dyn Fn() + 'static>;
const TRANSPORT_BUTTON_SIZE: i32 = 31;

#[derive(Clone)]
struct ActiveTrack {
    sound_id: String,
    sound_name: Option<String>,
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
    sidebar_toggle_btn: Button,
    row1: GtkBox,
    row2: GtkBox,
    audio_group: GtkBox,
    utility_group: GtkBox,
    compact: Cell<bool>,
    active_track: RefCell<Option<ActiveTrack>>,
    scrub_interaction: RefCell<ScrubInteraction>,
    scrub_commit_timeout: RefCell<Option<glib::SourceId>>,
    local_volume_save_timeout: RefCell<Option<glib::SourceId>>,
    mic_volume_save_timeout: RefCell<Option<glib::SourceId>>,
    suppress_headphones_toggle: Cell<bool>,
    suppress_mic_toggle: Cell<bool>,
    continue_suppressed_play_id: RefCell<Option<String>>,
    last_track_sound_id: RefCell<Option<String>>,
    refresh_cancel: RefCell<Option<Arc<AtomicBool>>>,
    state: Arc<AppState>,
    has_sound_list_provider: Cell<bool>,
    sound_list_provider: RefCell<Option<SoundListProvider>>,
    has_sounds_checker: RefCell<Option<HasSoundsChecker>>,
    toast_sender: Mutex<Option<std::sync::mpsc::Sender<String>>>,
    on_library_changed: RefCell<Option<LibraryChangedCallback>>,
    on_list_style_changed: RefCell<Option<ListStyleChangedCallback>>,
    on_settings_requested: RefCell<Option<SettingsRequestedCallback>>,
}

impl TransportBar {
    pub fn widget(&self) -> &Widget {
        self.inner.widget.upcast_ref()
    }

    /// Button that reveals the collapsed sidebar; hidden unless the layout is collapsed.
    pub fn sidebar_toggle_button(&self) -> &Button {
        &self.inner.sidebar_toggle_btn
    }

    /// Swap between the wide single-row layout and the compact two-row one,
    /// where the audio and utility clusters drop to row 2 so a narrow window
    /// doesn't clip the bar.
    pub fn set_compact(&self, compact: bool) {
        if self.inner.compact.get() == compact {
            return;
        }
        self.inner.compact.set(compact);

        let row1 = &self.inner.row1;
        let row2 = &self.inner.row2;
        let audio = &self.inner.audio_group;
        let utility = &self.inner.utility_group;

        if compact {
            row1.remove(audio);
            row1.remove(utility);
            row2.append(audio);
            row2.append(utility);
            row2.set_visible(true);
        } else {
            row2.remove(audio);
            row2.remove(utility);
            row1.append(audio);
            row1.append(utility);
            row2.set_visible(false);
        }
    }
}
