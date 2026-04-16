use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glib::timeout_add_local;
use gtk4::prelude::*;
use gtk4::{
    gdk, Adjustment, Align, Box as GtkBox, Button, Entry, EventControllerFocus, EventControllerKey,
    Label, Orientation, Scale, SearchEntry, ToggleButton, Widget,
};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app_state::AppState;
use crate::commands;
use crate::config::PlayMode;
use crate::timer_registry::remove_source_id_safe;

use super::icons;
use super::sound_list::NavigationSound;

type SoundListProvider = Box<dyn Fn() -> Vec<NavigationSound> + Send + Sync>;
type LibraryChangedCallback = Rc<dyn Fn() + 'static>;
type ListStyleChangedCallback = Rc<dyn Fn(String) + 'static>;
const TRANSPORT_BUTTON_SIZE: i32 = 31;
const SLOW_GTK_CALLBACK_THRESHOLD_MS: u128 = 16;

fn weak_library_changed_callback(
    callback: &RefCell<Option<LibraryChangedCallback>>,
) -> Option<Rc<dyn Fn() + 'static>> {
    let weak = Rc::downgrade(callback.borrow().as_ref()?);
    Some(Rc::new(move || {
        if let Some(callback) = weak.upgrade() {
            callback();
        }
    }))
}

fn weak_list_style_changed_callback(
    callback: &RefCell<Option<ListStyleChangedCallback>>,
) -> Option<Rc<dyn Fn(String) + 'static>> {
    let weak = Rc::downgrade(callback.borrow().as_ref()?);
    Some(Rc::new(move |style| {
        if let Some(callback) = weak.upgrade() {
            callback(style);
        }
    }))
}

fn apply_transport_button_size(button: &impl IsA<Widget>) {
    button.set_size_request(TRANSPORT_BUTTON_SIZE, TRANSPORT_BUTTON_SIZE);
    button.set_valign(Align::Center);
}

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
    pub fn new(state: Arc<AppState>) -> Self {
        let hbox = GtkBox::new(Orientation::Horizontal, 5);
        hbox.add_css_class("transport-bar");
        hbox.set_margin_start(0);
        hbox.set_margin_end(0);
        hbox.set_margin_top(0);
        hbox.set_margin_bottom(0);

        let playback_group = GtkBox::new(Orientation::Horizontal, 4);
        playback_group.add_css_class("transport-cluster");
        playback_group.add_css_class("transport-playback-cluster");

        let play_btn = icons::toggle_button(icons::PLAY, "Play / Pause");
        play_btn.set_sensitive(false);
        play_btn.add_css_class("transport-btn");
        play_btn.add_css_class("transport-playback-btn");
        apply_transport_button_size(&play_btn);
        update_play_pause_button(&play_btn, false);

        let stop_btn = icons::button(icons::STOP, "Stop All");
        stop_btn.set_sensitive(false);
        stop_btn.add_css_class("transport-btn");
        stop_btn.add_css_class("transport-playback-btn");
        apply_transport_button_size(&stop_btn);

        let prev_btn = icons::button(icons::PREVIOUS, "Previous Sound");
        prev_btn.set_sensitive(false);
        prev_btn.add_css_class("transport-btn");
        prev_btn.add_css_class("transport-playback-btn");
        apply_transport_button_size(&prev_btn);

        let next_btn = icons::button(icons::NEXT, "Next Sound");
        next_btn.set_sensitive(false);
        next_btn.add_css_class("transport-btn");
        next_btn.add_css_class("transport-playback-btn");
        apply_transport_button_size(&next_btn);

        playback_group.append(&play_btn);
        playback_group.append(&stop_btn);
        playback_group.append(&prev_btn);
        playback_group.append(&next_btn);
        hbox.append(&playback_group);

        let timeline_group = GtkBox::new(Orientation::Horizontal, 5);
        timeline_group.add_css_class("transport-cluster");
        timeline_group.add_css_class("transport-timeline");

        let scrub_adj = Adjustment::new(0.0, 0.0, 1.0, 0.01, 0.1, 0.0);
        let scrub = Scale::builder()
            .adjustment(&scrub_adj)
            .orientation(Orientation::Horizontal)
            .hexpand(true)
            .draw_value(false)
            .sensitive(false)
            .build();
        scrub.set_range(0.0, 1.0);
        scrub.set_height_request(18);
        scrub.set_valign(Align::Center);
        timeline_group.set_valign(Align::Center);

        let time_label = Label::builder()
            .label("0:00")
            .css_classes(vec!["monospace", "dim-label"])
            .width_chars(5)
            .build();

        let dur_label = Label::builder()
            .label("0:00")
            .css_classes(vec!["monospace", "dim-label"])
            .width_chars(5)
            .build();

        let track_name_label = Label::builder()
            .label("")
            .css_classes(vec!["dim-label", "transport-track-name"])
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(16)
            .visible(false)
            .build();

        timeline_group.append(&time_label);
        timeline_group.append(&scrub);
        timeline_group.append(&dur_label);
        timeline_group.append(&track_name_label);
        hbox.append(&timeline_group);

        let (local_vol_value, mic_vol_value, local_mute, mic_passthrough, play_mode) = {
            let cfg = state.config.lock().unwrap();
            (
                cfg.settings.local_volume as f64,
                cfg.settings.mic_volume as f64,
                cfg.settings.local_mute,
                cfg.settings.mic_passthrough,
                cfg.settings.play_mode,
            )
        };

        let audio_group = GtkBox::new(Orientation::Horizontal, 5);
        audio_group.add_css_class("transport-cluster");
        audio_group.add_css_class("volume-group");

        let local_vol_icon = icons::image(icons::HEADPHONES);
        local_vol_icon.add_css_class("transport-volume-icon");
        local_vol_icon.set_tooltip_text(Some("Headphones Volume"));
        let local_vol_adj = Adjustment::new(local_vol_value, 0.0, 100.0, 1.0, 0.0, 0.0);
        let local_vol = Scale::builder()
            .adjustment(&local_vol_adj)
            .orientation(Orientation::Horizontal)
            .draw_value(false)
            .width_request(100)
            .build();
        local_vol.add_css_class("volume-slider");

        let local_vol_label = Label::builder()
            .label(format!("{}", local_vol_value as u8))
            .css_classes(vec!["volume-readout", "monospace"])
            .width_chars(3)
            .build();
        let local_vol_entry = Entry::builder()
            .text(format!("{}", local_vol_value as u8))
            .width_chars(3)
            .max_width_chars(3)
            .hexpand(false)
            .visible(false)
            .build();
        local_vol_entry.set_size_request(46, -1);
        local_vol_entry.set_max_length(3);
        local_vol_entry.set_input_purpose(gtk4::InputPurpose::Digits);
        local_vol_entry.set_tooltip_text(Some("Headphones Volume"));
        local_vol_entry.add_css_class("volume-input");
        let local_vol_readout = GtkBox::new(Orientation::Horizontal, 0);
        local_vol_readout.add_css_class("volume-readout-wrap");
        local_vol_readout.append(&local_vol_label);
        local_vol_readout.append(&local_vol_entry);
        {
            let label = local_vol_label.clone();
            let entry = local_vol_entry.clone();
            let click = gtk4::GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                begin_volume_edit(&label, &entry);
            });
            local_vol_readout.add_controller(click);
        }

        let mic_vol_icon = icons::image(icons::MICROPHONE);
        mic_vol_icon.add_css_class("transport-volume-icon");
        mic_vol_icon.set_tooltip_text(Some("Microphone Volume"));
        let mic_vol_adj = Adjustment::new(mic_vol_value, 0.0, 100.0, 1.0, 0.0, 0.0);
        let mic_vol = Scale::builder()
            .adjustment(&mic_vol_adj)
            .orientation(Orientation::Horizontal)
            .draw_value(false)
            .width_request(100)
            .build();
        mic_vol.add_css_class("volume-slider");

        let mic_vol_label = Label::builder()
            .label(format!("{}", mic_vol_value as u8))
            .css_classes(vec!["volume-readout", "monospace"])
            .width_chars(3)
            .build();
        let mic_vol_entry = Entry::builder()
            .text(format!("{}", mic_vol_value as u8))
            .width_chars(3)
            .max_width_chars(3)
            .hexpand(false)
            .visible(false)
            .build();
        mic_vol_entry.set_size_request(46, -1);
        mic_vol_entry.set_max_length(3);
        mic_vol_entry.set_input_purpose(gtk4::InputPurpose::Digits);
        mic_vol_entry.set_tooltip_text(Some("Microphone Volume"));
        mic_vol_entry.add_css_class("volume-input");
        let mic_vol_readout = GtkBox::new(Orientation::Horizontal, 0);
        mic_vol_readout.add_css_class("volume-readout-wrap");
        mic_vol_readout.append(&mic_vol_label);
        mic_vol_readout.append(&mic_vol_entry);
        {
            let label = mic_vol_label.clone();
            let entry = mic_vol_entry.clone();
            let click = gtk4::GestureClick::new();
            click.connect_released(move |_, _, _, _| {
                begin_volume_edit(&label, &entry);
            });
            mic_vol_readout.add_controller(click);
        }

        audio_group.append(&local_vol_icon);
        audio_group.append(&local_vol);
        audio_group.append(&local_vol_readout);
        audio_group.append(&mic_vol_icon);
        audio_group.append(&mic_vol);
        audio_group.append(&mic_vol_readout);

        let headphones_btn = icons::toggle_button(
            if local_mute {
                icons::HEADPHONES_MUTED
            } else {
                icons::HEADPHONES
            },
            "Toggle Headphone Output",
        );
        headphones_btn.set_active(!local_mute);
        headphones_btn.add_css_class("transport-btn");
        headphones_btn.add_css_class("transport-icon-btn");
        apply_transport_button_size(&headphones_btn);
        if !local_mute {
            headphones_btn.add_css_class("btn-active");
        }
        audio_group.append(&headphones_btn);

        let mic_btn = icons::toggle_button(
            if mic_passthrough {
                icons::MICROPHONE
            } else {
                icons::MICROPHONE_DISABLED
            },
            "Toggle Mic Passthrough",
        );
        mic_btn.set_active(mic_passthrough);
        mic_btn.add_css_class("transport-btn");
        mic_btn.add_css_class("transport-icon-btn");
        apply_transport_button_size(&mic_btn);
        if mic_passthrough {
            mic_btn.add_css_class("btn-active");
        }
        audio_group.append(&mic_btn);
        hbox.append(&audio_group);

        let utility_group = GtkBox::new(Orientation::Horizontal, 4);
        utility_group.add_css_class("transport-cluster");

        let playmode_btn = icons::button(play_mode_icon(play_mode), play_mode_tooltip(play_mode));
        playmode_btn.add_css_class("transport-btn");
        playmode_btn.add_css_class("transport-icon-btn");
        playmode_btn.add_css_class("transport-playmode-btn");
        apply_transport_button_size(&playmode_btn);
        update_play_mode_button(&playmode_btn, play_mode);
        utility_group.append(&playmode_btn);

        let refresh_btn = icons::button(icons::REFRESH, "Refresh Sounds");
        refresh_btn.add_css_class("transport-btn");
        refresh_btn.add_css_class("transport-icon-btn");
        refresh_btn.add_css_class("transport-refresh-btn");
        apply_transport_button_size(&refresh_btn);
        utility_group.append(&refresh_btn);

        let search_entry = SearchEntry::builder()
            .placeholder_text("Search sounds…")
            .width_request(112)
            .build();
        search_entry.set_size_request(112, TRANSPORT_BUTTON_SIZE);
        search_entry.set_valign(Align::Center);
        search_entry.add_css_class("transport-search");
        utility_group.append(&search_entry);

        let settings_btn = icons::button(icons::SETTINGS, "Settings");
        settings_btn.add_css_class("transport-btn");
        settings_btn.add_css_class("transport-icon-btn");
        apply_transport_button_size(&settings_btn);
        utility_group.append(&settings_btn);

        hbox.append(&utility_group);

        let inner = Rc::new(TransportInner {
            widget: hbox,
            play_btn: play_btn.clone(),
            stop_btn: stop_btn.clone(),
            prev_btn: prev_btn.clone(),
            next_btn: next_btn.clone(),
            scrub: scrub.clone(),
            time_label: time_label.clone(),
            dur_label: dur_label.clone(),
            track_name_label: track_name_label.clone(),
            local_vol: local_vol.clone(),
            local_vol_label: local_vol_label.clone(),
            local_vol_entry: local_vol_entry.clone(),
            mic_vol: mic_vol.clone(),
            mic_vol_label: mic_vol_label.clone(),
            mic_vol_entry: mic_vol_entry.clone(),
            headphones_btn: headphones_btn.clone(),
            mic_btn: mic_btn.clone(),
            playmode_btn: playmode_btn.clone(),
            refresh_btn: refresh_btn.clone(),
            search_entry: search_entry.clone(),
            settings_btn: settings_btn.clone(),
            active_track: RefCell::new(None),
            scrub_interaction: RefCell::new(ScrubInteraction::default()),
            scrub_commit_timeout: RefCell::new(None),
            scrub_timer_id: RefCell::new(None),
            suppress_headphones_toggle: Cell::new(false),
            suppress_mic_toggle: Cell::new(false),
            continue_suppressed_play_id: RefCell::new(None),
            last_track_sound_id: RefCell::new(None),
            state,
            sound_list_provider: Mutex::new(None),
            toast_sender: Mutex::new(None),
            on_library_changed: RefCell::new(None),
            on_list_style_changed: RefCell::new(None),
            settings_dialog: RefCell::new(None),
        });

        let tb = Self { inner };
        tb.connect_signals();

        {
            let inner_weak = Rc::downgrade(&tb.inner);
            let timer_id = timeout_add_local(Duration::from_millis(150), move || {
                let Some(inner_poll) = inner_weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                let started_at = Instant::now();
                inner_poll.update_scrub();
                log_slow_ui_callback("transport.update_scrub", started_at);
                glib::ControlFlow::Continue
            });
            *tb.inner.scrub_timer_id.borrow_mut() = Some(timer_id);
        }

        tb
    }

    pub fn widget(&self) -> &Widget {
        self.inner.widget.upcast_ref()
    }

    pub fn connect_search_changed<F: Fn(String) + 'static>(&self, f: F) {
        self.inner
            .search_entry
            .connect_search_changed(move |entry| {
                f(entry.text().to_string());
            });
    }

    pub fn set_sound_list_provider<F: Fn() -> Vec<NavigationSound> + Send + Sync + 'static>(
        &self,
        f: F,
    ) {
        *self.inner.sound_list_provider.lock().unwrap() = Some(Box::new(f));
    }

    pub fn toggle_play_pause(&self) {
        let track = self.inner.active_track.borrow();
        if let Some(track) = track.as_ref() {
            let positions = commands::get_playback_positions(Arc::clone(&self.inner.state.player));
            let is_paused = positions
                .iter()
                .any(|position| position.sound_id == track.sound_id && position.paused);
            if is_paused {
                commands::resume_sound(
                    track.sound_id.clone(),
                    Arc::clone(&self.inner.state.player),
                );
                update_play_pause_button(&self.inner.play_btn, true);
            } else {
                commands::pause_sound(track.sound_id.clone(), Arc::clone(&self.inner.state.player));
                update_play_pause_button(&self.inner.play_btn, false);
            }
        }
    }

    pub fn play_previous(&self) {
        self.inner.play_adjacent_sound(-1);
    }

    pub fn play_next(&self) {
        self.inner.play_adjacent_sound(1);
    }

    pub fn stop_all(&self) {
        self.inner.stop_all_playback();
    }

    pub fn toggle_headphones_mute(&self) {
        match commands::toggle_local_mute(
            Arc::clone(&self.inner.state.config),
            Arc::clone(&self.inner.state.player),
        ) {
            Ok(local_mute) => self.apply_headphones_state(local_mute),
            Err(e) => {
                log::warn!("Toggle local mute failed via hotkey: {e}");
                self.refresh_controls_from_state();
            }
        }
    }

    pub fn toggle_mic_mute(&self) {
        match commands::toggle_mic_passthrough(
            Arc::clone(&self.inner.state.config),
            Arc::clone(&self.inner.state.player),
        ) {
            Ok(enabled) => self.apply_mic_state(enabled),
            Err(e) => {
                log::warn!("Toggle mic passthrough failed via hotkey: {e}");
                self.refresh_controls_from_state();
            }
        }
    }

    pub fn cycle_play_mode(&self) {
        let new_mode = self
            .inner
            .state
            .config
            .lock()
            .unwrap()
            .settings
            .play_mode
            .next();
        if let Err(e) = commands::set_play_mode(
            new_mode.as_str().to_string(),
            Arc::clone(&self.inner.state.config),
            Arc::clone(&self.inner.state.player),
        ) {
            log::warn!("Set play mode failed via hotkey: {e}");
            self.refresh_controls_from_state();
            return;
        }
        update_play_mode_button(&self.inner.playmode_btn, new_mode);
    }

    pub fn refresh_controls_from_state(&self) {
        let (local_mute, mic_passthrough, play_mode) = self
            .inner
            .state
            .config
            .lock()
            .map(|cfg| {
                (
                    cfg.settings.local_mute,
                    cfg.settings.mic_passthrough,
                    cfg.settings.play_mode,
                )
            })
            .unwrap_or((false, true, PlayMode::Default));
        self.apply_headphones_state(local_mute);
        self.apply_mic_state(mic_passthrough);
        update_play_mode_button(&self.inner.playmode_btn, play_mode);
    }

    pub fn set_toast_sender(&self, sender: std::sync::mpsc::Sender<String>) {
        *self.inner.toast_sender.lock().unwrap() = Some(sender);
    }

    pub fn connect_library_changed<F: Fn() + 'static>(&self, f: F) {
        *self.inner.on_library_changed.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_list_style_changed<F: Fn(String) + 'static>(&self, f: F) {
        *self.inner.on_list_style_changed.borrow_mut() = Some(Rc::new(f));
    }

    pub fn cleanup(&self) {
        if let Some(timeout_id) = self.inner.scrub_commit_timeout.borrow_mut().take() {
            let _ = remove_source_id_safe(timeout_id);
        }
        if let Some(timer_id) = self.inner.scrub_timer_id.borrow_mut().take() {
            let _ = remove_source_id_safe(timer_id);
        }
        *self.inner.sound_list_provider.lock().unwrap() = None;
        *self.inner.toast_sender.lock().unwrap() = None;
        *self.inner.on_library_changed.borrow_mut() = None;
        *self.inner.on_list_style_changed.borrow_mut() = None;
    }

    fn apply_headphones_state(&self, local_mute: bool) {
        self.inner.suppress_headphones_toggle.set(true);
        self.inner.headphones_btn.set_active(!local_mute);
        self.inner.suppress_headphones_toggle.set(false);
        update_headphones_button(&self.inner.headphones_btn, !local_mute);
    }

    fn apply_mic_state(&self, enabled: bool) {
        self.inner.suppress_mic_toggle.set(true);
        self.inner.mic_btn.set_active(enabled);
        self.inner.suppress_mic_toggle.set(false);
        update_mic_button(&self.inner.mic_btn, enabled);
    }

    fn connect_signals(&self) {
        let inner_weak = Rc::downgrade(&self.inner);

        {
            let inner_weak = inner_weak.clone();
            self.inner.stop_btn.connect_clicked(move |_| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                inner.stop_all_playback();
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.play_btn.connect_clicked(move |btn| {
                let Some(inner_toggle) = inner_weak.upgrade() else {
                    return;
                };
                let should_resume = btn.is_active();
                update_play_pause_button(btn, should_resume);
                let active_track = inner_toggle.active_track.borrow().clone();
                if let Some(track) = active_track.as_ref() {
                    if should_resume {
                        commands::resume_sound(
                            track.sound_id.clone(),
                            Arc::clone(&inner_toggle.state.player),
                        );
                    } else {
                        commands::pause_sound(
                            track.sound_id.clone(),
                            Arc::clone(&inner_toggle.state.player),
                        );
                    }
                }
            });
        }

        {
            let inner_weak = inner_weak.clone();
            let local_adj = self.inner.local_vol.adjustment();
            let local_label = self.inner.local_vol_label.clone();
            let local_entry = self.inner.local_vol_entry.clone();
            local_adj.connect_value_changed(move |adj| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let volume = adj.value().round().clamp(0.0, 100.0) as u8;
                local_label.set_label(&format!("{volume}"));
                if !gtk4::prelude::WidgetExt::is_visible(&local_entry) {
                    local_entry.set_text(&format!("{volume}"));
                }
                let _ = commands::set_local_volume(
                    volume,
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                );
            });
        }

        {
            let inner_weak = inner_weak.clone();
            let mic_adj = self.inner.mic_vol.adjustment();
            let mic_label = self.inner.mic_vol_label.clone();
            let mic_entry = self.inner.mic_vol_entry.clone();
            mic_adj.connect_value_changed(move |adj| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let volume = adj.value().round().clamp(0.0, 100.0) as u8;
                mic_label.set_label(&format!("{volume}"));
                if !gtk4::prelude::WidgetExt::is_visible(&mic_entry) {
                    mic_entry.set_text(&format!("{volume}"));
                }
                let _ = commands::set_mic_volume(
                    volume,
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                );
            });
        }

        install_volume_editor(
            &self.inner.local_vol.adjustment(),
            &self.inner.local_vol_label,
            &self.inner.local_vol_entry,
        );
        install_volume_editor(
            &self.inner.mic_vol.adjustment(),
            &self.inner.mic_vol_label,
            &self.inner.mic_vol_entry,
        );

        {
            let inner_weak = inner_weak.clone();
            self.inner.headphones_btn.connect_toggled(move |btn| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                if inner.suppress_headphones_toggle.get() {
                    return;
                }
                let requested_enabled = btn.is_active();
                match commands::toggle_local_mute(
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                ) {
                    Ok(muted) => {
                        update_headphones_button(btn, !muted);
                    }
                    Err(e) => {
                        log::warn!("Toggle local mute failed: {e}");
                        inner.suppress_headphones_toggle.set(true);
                        btn.set_active(!requested_enabled);
                        inner.suppress_headphones_toggle.set(false);
                        update_headphones_button(btn, !requested_enabled);
                    }
                }
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.mic_btn.connect_toggled(move |btn| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                if inner.suppress_mic_toggle.get() {
                    return;
                }
                let requested_enabled = btn.is_active();
                btn.set_sensitive(false);
                let btn_weak = btn.downgrade();
                let inner_done_weak = Rc::downgrade(&inner);
                if let Err(e) = commands::set_mic_passthrough_enabled_async(
                    requested_enabled,
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                    move |result| {
                        let Some(btn) = btn_weak.upgrade() else {
                            return;
                        };
                        match result {
                            Ok(enabled) => {
                                if let Some(inner_done) = inner_done_weak.upgrade() {
                                    inner_done.suppress_mic_toggle.set(true);
                                    btn.set_active(enabled);
                                    inner_done.suppress_mic_toggle.set(false);
                                }
                                update_mic_button(&btn, enabled);
                            }
                            Err(err) => {
                                log::warn!("Toggle mic passthrough failed: {err}");
                                let fallback_enabled = !requested_enabled;
                                if let Some(inner_done) = inner_done_weak.upgrade() {
                                    inner_done.suppress_mic_toggle.set(true);
                                    btn.set_active(fallback_enabled);
                                    inner_done.suppress_mic_toggle.set(false);
                                }
                                update_mic_button(&btn, fallback_enabled);
                            }
                        }
                        btn.set_sensitive(true);
                    },
                ) {
                    log::warn!("Failed to dispatch mic passthrough toggle: {e}");
                    inner.suppress_mic_toggle.set(true);
                    btn.set_active(!requested_enabled);
                    inner.suppress_mic_toggle.set(false);
                    update_mic_button(btn, !requested_enabled);
                    btn.set_sensitive(true);
                }
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner
                .scrub
                .connect_change_value(move |_, scroll_type, value| {
                    let Some(inner_seek) = inner_weak.upgrade() else {
                        return glib::Propagation::Proceed;
                    };
                    if scroll_type == gtk4::ScrollType::Jump {
                        inner_seek.begin_scrub_interaction(ScrubInput::Pointer);

                        if let Some(position_ms) = inner_seek.record_scrub_preview(value) {
                            inner_seek
                                .time_label
                                .set_text(&format_duration(position_ms));
                        }

                        if let Some(timeout_id) =
                            inner_seek.scrub_commit_timeout.borrow_mut().take()
                        {
                            let _ = remove_source_id_safe(timeout_id);
                        }

                        // Coalesce drag updates into one seek.
                        let inner_weak_commit = Rc::downgrade(&inner_seek);
                        let timeout_id =
                            glib::timeout_add_local_once(Duration::from_millis(100), move || {
                                let Some(inner_commit) = inner_weak_commit.upgrade() else {
                                    return;
                                };
                                inner_commit.commit_scrub_seek_on_release();
                                *inner_commit.scrub_commit_timeout.borrow_mut() = None;
                            });
                        *inner_seek.scrub_commit_timeout.borrow_mut() = Some(timeout_id);
                    }
                    glib::Propagation::Proceed
                });
        }

        {
            let inner_weak_pressed = inner_weak.clone();
            let key = EventControllerKey::new();
            key.connect_key_pressed(move |_, keyval, _, _| {
                let Some(inner_key) = inner_weak_pressed.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if keyval.name().as_deref() == Some("Escape") {
                    inner_key.cancel_scrub_interaction();
                    return glib::Propagation::Stop;
                }

                if is_seek_key(keyval) {
                    inner_key.begin_scrub_interaction(ScrubInput::Keyboard);
                }

                glib::Propagation::Proceed
            });

            let inner_weak_release = inner_weak.clone();
            key.connect_key_released(move |_, keyval, _, _| {
                let Some(inner_key_release) = inner_weak_release.upgrade() else {
                    return;
                };
                if is_seek_key(keyval) {
                    inner_key_release.commit_scrub_seek_on_release();
                }
            });

            self.inner.scrub.add_controller(key);
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.refresh_btn.connect_clicked(move |btn| {
                let Some(inner_refresh) = inner_weak.upgrade() else {
                    return;
                };
                btn.add_css_class("spinning");
                let state_refresh = Arc::clone(&inner_refresh.state);
                let inner_weak_done = Rc::downgrade(&inner_refresh);
                let btn_done = btn.clone();
                glib::MainContext::default().spawn_local(async move {
                    match commands::refresh_sounds(
                        Arc::clone(&state_refresh.config),
                        Arc::clone(&state_refresh.hotkeys),
                    ) {
                        Ok(_) => {
                            if let Some(inner_refresh_done) = inner_weak_done.upgrade() {
                                if let Some(tx) = &*inner_refresh_done.toast_sender.lock().unwrap()
                                {
                                    let _ = tx.send("Sounds refreshed".to_string());
                                }
                                if let Some(cb) =
                                    inner_refresh_done.on_library_changed.borrow().as_ref()
                                {
                                    cb();
                                }
                            }
                        }
                        Err(e) => log::warn!("Refresh failed: {e}"),
                    }
                    btn_done.remove_css_class("spinning");
                });
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.settings_btn.connect_clicked(move |btn| {
                let Some(inner_settings) = inner_weak.upgrade() else {
                    return;
                };
                if let Some(win) = btn
                    .root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok())
                {
                    if let Some(existing_dialog) = inner_settings.settings_dialog.borrow().as_ref()
                    {
                        existing_dialog.present(Some(&win));
                        crate::diagnostics::memory::log_memory_snapshot("ui:settings:reused");
                        crate::diagnostics::record_phase("ui:settings_reused", None);
                        return;
                    }

                    let on_library_changed =
                        weak_library_changed_callback(&inner_settings.on_library_changed);
                    let on_list_style_changed =
                        weak_list_style_changed_callback(&inner_settings.on_list_style_changed);
                    let prefs = crate::ui::settings::build_settings_dialog(
                        &win,
                        Arc::clone(&inner_settings.state),
                        on_library_changed,
                        on_list_style_changed,
                    );
                    *inner_settings.settings_dialog.borrow_mut() = Some(prefs.clone());
                    prefs.connect_closed(move |_| {
                        crate::diagnostics::memory::log_memory_snapshot("ui:settings:closed");
                        crate::diagnostics::record_phase("ui:settings_closed", None);
                    });
                    prefs.present(Some(&win));
                    crate::diagnostics::memory::log_memory_snapshot("ui:settings:opened");
                    crate::diagnostics::record_phase("ui:settings_opened", None);
                }
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.playmode_btn.connect_clicked(move |btn| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let current_mode = inner.state.config.lock().unwrap().settings.play_mode;
                let new_mode = current_mode.next();
                let _ = commands::set_play_mode(
                    new_mode.as_str().to_string(),
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                );
                update_play_mode_button(btn, new_mode);
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.prev_btn.connect_clicked(move |_| {
                let Some(inner_prev) = inner_weak.upgrade() else {
                    return;
                };
                inner_prev.play_adjacent_sound(-1);
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.next_btn.connect_clicked(move |_| {
                let Some(inner_next) = inner_weak.upgrade() else {
                    return;
                };
                inner_next.play_adjacent_sound(1);
            });
        }
    }
}

impl TransportInner {
    fn begin_scrub_interaction(&self, input: ScrubInput) {
        begin_scrub_interaction_state(&mut self.scrub_interaction.borrow_mut(), input);
    }

    fn record_scrub_preview(&self, value: f64) -> Option<u64> {
        let duration_ms = self
            .active_track
            .borrow()
            .as_ref()
            .and_then(|track| track.sound_duration_ms);
        record_scrub_preview_state(
            &mut self.scrub_interaction.borrow_mut(),
            duration_ms,
            value,
            Some(ScrubInput::Pointer),
        )
    }

    fn commit_scrub_seek_on_release(&self) {
        let track = self.active_track.borrow().as_ref().cloned();
        let current_value = self.scrub.value();
        let duration_ms = track.as_ref().and_then(|track| track.sound_duration_ms);
        let position_ms = take_scrub_commit_position(
            &mut self.scrub_interaction.borrow_mut(),
            duration_ms,
            current_value,
        );

        if let (Some(track), Some(position_ms)) = (track, position_ms) {
            {
                let mut interaction = self.scrub_interaction.borrow_mut();
                if interaction.last_committed_sound_id.as_deref() == Some(track.sound_id.as_str())
                    && interaction.last_committed_position_ms == Some(position_ms)
                {
                    interaction.pending_seek_sound_id = None;
                    interaction.pending_seek_position_ms = None;
                    interaction.pending_seek_deadline_ms = None;
                    return;
                }
                interaction.last_committed_sound_id = Some(track.sound_id.clone());
                interaction.last_committed_position_ms = Some(position_ms);
                interaction.pending_seek_sound_id = Some(track.sound_id.clone());
                interaction.pending_seek_position_ms = Some(position_ms);
                interaction.pending_seek_deadline_ms = Some(pending_seek_deadline_ms_from_now());
            }

            if let Err(e) =
                commands::seek_sound(track.sound_id, position_ms, Arc::clone(&self.state.player))
            {
                log::warn!("Seek dispatch failed for {}ms: {}", position_ms, e);
                let mut interaction = self.scrub_interaction.borrow_mut();
                interaction.pending_seek_position_ms = None;
                interaction.pending_seek_sound_id = None;
                interaction.pending_seek_deadline_ms = None;
            }
        }
    }

    fn cancel_scrub_interaction(&self) {
        clear_scrub_interaction_state(&mut self.scrub_interaction.borrow_mut());
    }

    fn has_navigation_sounds(&self) -> bool {
        let guard = self.sound_list_provider.lock().unwrap();
        match guard.as_ref() {
            Some(provider) => !provider().is_empty(),
            None => false,
        }
    }

    fn play_adjacent_sound(&self, offset: i32) {
        self.clear_continue_suppression();
        let sounds = match self.sound_list_provider.lock().unwrap().as_ref() {
            Some(provider) => provider(),
            None => return,
        };
        if sounds.is_empty() {
            return;
        }

        let current_id = self
            .active_track
            .borrow()
            .as_ref()
            .map(|track| track.sound_id.clone())
            .or_else(|| self.last_track_sound_id.borrow().clone());

        let current_idx = current_id
            .and_then(|id| sounds.iter().position(|sound| sound.id == id))
            .map(|idx| idx as i32)
            .unwrap_or_else(|| if offset >= 0 { -1 } else { 0 });

        let len = sounds.len() as i32;
        let next_idx = ((current_idx + offset) % len + len) % len;
        let next_sound = &sounds[next_idx as usize];

        commands::stop_all(Arc::clone(&self.state.player));
        let sound_name = next_sound.name.clone();
        if let Err(e) = commands::play_sound_async(
            next_sound.id.clone(),
            Arc::clone(&self.state.config),
            Arc::clone(&self.state.player),
            move |result| {
                if let Err(e) = result {
                    log::warn!("Play adjacent failed for '{}': {}", sound_name, e);
                }
            },
        ) {
            log::warn!(
                "Failed to dispatch adjacent playback for '{}': {}",
                next_sound.name,
                e
            );
        }
    }

    fn update_scrub(&self) {
        let positions = commands::get_playback_positions(Arc::clone(&self.state.player));
        let now_ms = glib::monotonic_time() as u64 / 1_000;

        if positions.is_empty() {
            self.cancel_scrub_interaction();
            let play_mode = { self.state.config.lock().unwrap().settings.play_mode };
            let has_navigation_sounds = self.has_navigation_sounds();
            let should_continue = should_continue_playback(
                self.last_track_sound_id.borrow().is_some(),
                play_mode,
                has_navigation_sounds,
                self.is_continue_suppressed(),
            );
            if should_continue {
                self.play_adjacent_sound(1);
                return;
            }

            self.clear_continue_suppression();
            self.reset_idle_playback_ui();
            return;
        }

        let has_provider = self.sound_list_provider.lock().unwrap().is_some();
        self.prev_btn.set_sensitive(has_provider);
        self.next_btn.set_sensitive(has_provider);

        if let Some(position) = positions.iter().find(|position| !position.finished) {
            self.clear_continue_suppression_for_playback(&position.play_id);
            self.stop_btn.set_sensitive(true);
            self.play_btn.set_sensitive(true);
            update_play_pause_button(&self.play_btn, !position.paused);
            let interaction = {
                let mut interaction = self.scrub_interaction.borrow_mut();
                settle_pending_seek_state(
                    &mut interaction,
                    &position.sound_id,
                    position.position_ms,
                    now_ms,
                );
                interaction.clone()
            };

            let sound_snapshot = {
                let cfg = self.state.config.lock().unwrap();
                cfg.get_sound(&position.sound_id)
                    .map(|sound| (sound.id.clone(), sound.name.clone(), sound.duration_ms))
            };
            let track_duration_ms = sound_snapshot
                .as_ref()
                .and_then(|(_, _, duration_ms)| *duration_ms);
            let duration_ms = resolve_scrub_duration_ms(position.duration_ms, track_duration_ms);

            self.scrub.set_sensitive(true);
            if should_sync_scrub_from_playback(&interaction) {
                self.scrub
                    .set_value(scrub_progress_value(position.position_ms, duration_ms));
            }

            if position.duration_ms.is_some() || track_duration_ms.is_some() {
                self.dur_label.set_text(&format_duration(duration_ms));
            } else {
                self.dur_label
                    .set_text(&format!("~{}", format_duration(duration_ms)));
            }
            self.time_label
                .set_text(&format_duration(displayed_scrub_position_ms(
                    &interaction,
                    position.position_ms,
                )));

            if let Some((sound_id, sound_name, _)) = sound_snapshot {
                self.track_name_label.set_label(&sound_name);
                self.track_name_label.set_visible(true);
                *self.active_track.borrow_mut() = Some(ActiveTrack {
                    sound_id: sound_id.clone(),
                    sound_duration_ms: Some(duration_ms),
                    play_id: position.play_id.clone(),
                });
                *self.last_track_sound_id.borrow_mut() = Some(sound_id);
            } else {
                self.track_name_label.set_visible(false);
                *self.active_track.borrow_mut() = Some(ActiveTrack {
                    sound_id: position.sound_id.clone(),
                    sound_duration_ms: Some(duration_ms),
                    play_id: position.play_id.clone(),
                });
                *self.last_track_sound_id.borrow_mut() = Some(position.sound_id.clone());
            }
        } else if positions.iter().all(|position| position.finished) {
            let play_mode = { self.state.config.lock().unwrap().settings.play_mode };
            let has_navigation_sounds = self.has_navigation_sounds();
            if should_continue_playback(
                self.last_track_sound_id.borrow().is_some(),
                play_mode,
                has_navigation_sounds,
                self.is_continue_suppressed(),
            ) {
                self.play_adjacent_sound(1);
            } else {
                self.clear_continue_suppression();
                self.reset_idle_playback_ui();
            }
        }
    }

    fn stop_all_playback(&self) {
        *self.continue_suppressed_play_id.borrow_mut() = self
            .active_track
            .borrow()
            .as_ref()
            .map(|track| track.play_id.clone());
        commands::stop_all(Arc::clone(&self.state.player));
    }

    fn is_continue_suppressed(&self) -> bool {
        self.continue_suppressed_play_id.borrow().is_some()
    }

    fn clear_continue_suppression(&self) {
        *self.continue_suppressed_play_id.borrow_mut() = None;
    }

    fn clear_continue_suppression_for_playback(&self, play_id: &str) {
        let should_clear = should_clear_continue_suppression(
            self.continue_suppressed_play_id.borrow().as_deref(),
            play_id,
        );
        if should_clear {
            self.clear_continue_suppression();
        }
    }

    fn reset_idle_playback_ui(&self) {
        self.play_btn.set_sensitive(false);
        update_play_pause_button(&self.play_btn, false);
        self.stop_btn.set_sensitive(false);
        self.prev_btn.set_sensitive(false);
        self.next_btn.set_sensitive(false);
        self.scrub.set_sensitive(false);
        self.track_name_label.set_visible(false);
        *self.active_track.borrow_mut() = None;
        *self.last_track_sound_id.borrow_mut() = None;
        self.time_label.set_text("0:00");
        self.dur_label.set_text("0:00");
    }
}

fn scrub_position_ms(value: f64, duration_ms: u64) -> u64 {
    value.clamp(0.0, 1.0).mul_add(duration_ms as f64, 0.0) as u64
}

fn resolve_scrub_duration_ms(
    playback_duration_ms: Option<u64>,
    track_duration_ms: Option<u64>,
) -> u64 {
    playback_duration_ms
        .or(track_duration_ms)
        .filter(|duration_ms| *duration_ms > 0)
        .unwrap_or(DEFAULT_SCRUB_DURATION_MS)
}

fn scrub_progress_value(position_ms: u64, duration_ms: u64) -> f64 {
    if duration_ms == 0 {
        0.0
    } else {
        (position_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
    }
}

fn begin_scrub_interaction_state(interaction: &mut ScrubInteraction, input: ScrubInput) {
    if !interaction.active {
        interaction.active = true;
    }
    interaction.input = Some(input);
    interaction.pending_seek_position_ms = None;
    interaction.pending_seek_sound_id = None;
    interaction.pending_seek_deadline_ms = None;
}

fn record_scrub_preview_state(
    interaction: &mut ScrubInteraction,
    duration_ms: Option<u64>,
    value: f64,
    default_input: Option<ScrubInput>,
) -> Option<u64> {
    if !interaction.active {
        begin_scrub_interaction_state(interaction, default_input?);
    }

    let effective_duration = resolve_scrub_duration_ms(duration_ms, None);
    let position_ms = scrub_position_ms(value, effective_duration);
    interaction.preview_position_ms = Some(position_ms);
    Some(position_ms)
}

fn take_scrub_commit_position(
    interaction: &mut ScrubInteraction,
    duration_ms: Option<u64>,
    current_value: f64,
) -> Option<u64> {
    if !interaction.active {
        return None;
    }

    let position_ms = interaction
        .preview_position_ms
        .or_else(|| duration_ms.map(|duration_ms| scrub_position_ms(current_value, duration_ms)));

    interaction.active = false;
    interaction.input = None;
    interaction.preview_position_ms = None;

    position_ms
}

fn clear_scrub_interaction_state(interaction: &mut ScrubInteraction) {
    interaction.active = false;
    interaction.input = None;
    interaction.preview_position_ms = None;
    interaction.pending_seek_position_ms = None;
    interaction.pending_seek_sound_id = None;
    interaction.pending_seek_deadline_ms = None;
    interaction.last_committed_position_ms = None;
    interaction.last_committed_sound_id = None;
}

fn pending_seek_deadline_ms_from_now() -> u64 {
    (glib::monotonic_time() as u64 / 1_000) + PENDING_SEEK_TIMEOUT_MS
}

fn settle_pending_seek_state(
    interaction: &mut ScrubInteraction,
    playback_sound_id: &str,
    playback_position_ms: u64,
    now_ms: u64,
) {
    let Some(pending_seek_ms) = interaction.pending_seek_position_ms else {
        return;
    };
    let pending_sound_matches =
        interaction.pending_seek_sound_id.as_deref() == Some(playback_sound_id);

    if !pending_sound_matches {
        interaction.pending_seek_position_ms = None;
        interaction.pending_seek_sound_id = None;
        interaction.pending_seek_deadline_ms = None;
        return;
    }

    if playback_position_ms.abs_diff(pending_seek_ms) <= SEEK_SETTLE_TOLERANCE_MS {
        interaction.pending_seek_position_ms = None;
        interaction.pending_seek_sound_id = None;
        interaction.pending_seek_deadline_ms = None;
        return;
    }

    if let Some(deadline_ms) = interaction.pending_seek_deadline_ms {
        if now_ms >= deadline_ms {
            log::debug!(
                "Pending seek timeout reached: target={}ms current={}ms, clearing pending state",
                pending_seek_ms,
                playback_position_ms
            );
            interaction.pending_seek_position_ms = None;
            interaction.pending_seek_sound_id = None;
            interaction.pending_seek_deadline_ms = None;
        }
    }
}

fn displayed_scrub_position_ms(interaction: &ScrubInteraction, playback_position_ms: u64) -> u64 {
    if interaction.active {
        interaction
            .preview_position_ms
            .unwrap_or(playback_position_ms)
    } else if let Some(pending_seek_ms) = interaction.pending_seek_position_ms {
        pending_seek_ms
    } else {
        playback_position_ms
    }
}

fn should_sync_scrub_from_playback(interaction: &ScrubInteraction) -> bool {
    !interaction.active && interaction.pending_seek_position_ms.is_none()
}

fn should_continue_playback(
    has_last_track: bool,
    play_mode: PlayMode,
    has_navigation_sounds: bool,
    continue_suppressed: bool,
) -> bool {
    has_last_track
        && play_mode == PlayMode::Continue
        && has_navigation_sounds
        && !continue_suppressed
}

fn should_clear_continue_suppression(
    suppressed_play_id: Option<&str>,
    active_play_id: &str,
) -> bool {
    matches!(suppressed_play_id, Some(suppressed_play_id) if suppressed_play_id != active_play_id)
}

fn is_seek_key(keyval: gdk::Key) -> bool {
    matches!(
        keyval,
        gdk::Key::Left
            | gdk::Key::Right
            | gdk::Key::KP_Left
            | gdk::Key::KP_Right
            | gdk::Key::Page_Up
            | gdk::Key::Page_Down
            | gdk::Key::Home
            | gdk::Key::End
            | gdk::Key::KP_Page_Up
            | gdk::Key::KP_Page_Down
            | gdk::Key::KP_Home
            | gdk::Key::KP_End
    )
}

fn update_play_pause_button(button: &ToggleButton, is_playing: bool) {
    button.set_active(is_playing);
    if is_playing {
        button.add_css_class("btn-active");
        icons::apply_button_icon(button, icons::PAUSE);
        button.set_tooltip_text(Some("Pause"));
    } else {
        button.remove_css_class("btn-active");
        icons::apply_button_icon(button, icons::PLAY);
        button.set_tooltip_text(Some("Play"));
    }
}

fn update_mic_button(button: &ToggleButton, enabled: bool) {
    if enabled {
        button.add_css_class("btn-active");
        icons::apply_button_icon(button, icons::MICROPHONE);
    } else {
        button.remove_css_class("btn-active");
        icons::apply_button_icon(button, icons::MICROPHONE_DISABLED);
    }
}

fn update_headphones_button(button: &ToggleButton, enabled: bool) {
    if enabled {
        button.add_css_class("btn-active");
        icons::apply_button_icon(button, icons::HEADPHONES);
    } else {
        button.remove_css_class("btn-active");
        icons::apply_button_icon(button, icons::HEADPHONES_MUTED);
    }
}

fn play_mode_icon(mode: PlayMode) -> icons::IconPair {
    match mode {
        PlayMode::Loop => icons::PLAYMODE_LOOP,
        PlayMode::Continue => icons::PLAYMODE_CONTINUE,
        PlayMode::Default => icons::PLAYMODE_DEFAULT,
    }
}

fn update_play_mode_button(button: &impl IsA<gtk4::Button>, mode: PlayMode) {
    icons::apply_button_icon(button, play_mode_icon(mode));
    button
        .as_ref()
        .set_tooltip_text(Some(play_mode_tooltip(mode)));
    if mode == PlayMode::Default {
        button.as_ref().remove_css_class("btn-active");
    } else {
        button.as_ref().add_css_class("btn-active");
    }
}

fn play_mode_tooltip(mode: PlayMode) -> &'static str {
    match mode {
        PlayMode::Loop => "Play Mode: Loop",
        PlayMode::Continue => "Play Mode: Continue",
        PlayMode::Default => "Play Mode: Default",
    }
}

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn log_slow_ui_callback(name: &str, started_at: Instant) {
    let elapsed_ms = started_at.elapsed().as_millis();
    if elapsed_ms >= SLOW_GTK_CALLBACK_THRESHOLD_MS {
        log::debug!(
            "GTK callback latency exceeded threshold: name={} elapsed_ms={}",
            name,
            elapsed_ms
        );
    }
}

fn begin_volume_edit(label: &Label, entry: &Entry) {
    entry.set_text(label.text().as_str());
    label.set_visible(false);
    entry.set_visible(true);
    entry.grab_focus();
    entry.select_region(0, -1);
}

fn finish_volume_edit(label: &Label, entry: &Entry, adjustment: &Adjustment, commit: bool) {
    if commit {
        if let Ok(value) = entry.text().parse::<i32>() {
            let clamped = value.clamp(0, 100) as f64;
            adjustment.set_value(clamped);
        }
    }
    entry.set_visible(false);
    label.set_visible(true);
    let current = adjustment.value().round().clamp(0.0, 100.0) as u8;
    label.set_label(&format!("{current}"));
    entry.set_text(&format!("{current}"));
}

fn install_volume_editor(adjustment: &Adjustment, label: &Label, entry: &Entry) {
    {
        let label = label.clone();
        let entry = entry.clone();
        let adjustment = adjustment.clone();
        let entry_for_cb = entry.clone();
        entry.connect_activate(move |_| {
            finish_volume_edit(&label, &entry_for_cb, &adjustment, true);
        });
    }

    {
        let label = label.clone();
        let entry = entry.clone();
        let adjustment = adjustment.clone();
        let key = EventControllerKey::new();
        let entry_for_cb = entry.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval.name().as_deref() == Some("Escape") {
                finish_volume_edit(&label, &entry_for_cb, &adjustment, false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        entry.add_controller(key);
    }

    {
        let label = label.clone();
        let entry = entry.clone();
        let adjustment = adjustment.clone();
        let focus = EventControllerFocus::new();
        let entry_for_cb = entry.clone();
        focus.connect_leave(move |_| {
            finish_volume_edit(&label, &entry_for_cb, &adjustment, true);
        });
        entry.add_controller(focus);
    }
}

impl Drop for TransportBar {
    fn drop(&mut self) {
        self.cleanup();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrub_position_uses_live_duration() {
        assert_eq!(scrub_position_ms(0.5, 12_000), 6_000);
    }

    #[test]
    fn resolve_scrub_duration_prefers_playback_then_track_then_default() {
        assert_eq!(
            resolve_scrub_duration_ms(Some(12_000), Some(10_000)),
            12_000
        );
        assert_eq!(resolve_scrub_duration_ms(None, Some(10_000)), 10_000);
        assert_eq!(
            resolve_scrub_duration_ms(None, None),
            DEFAULT_SCRUB_DURATION_MS
        );
    }

    #[test]
    fn scrub_progress_value_clamps_ratio() {
        assert_eq!(scrub_progress_value(3_000, 12_000), 0.25);
        assert_eq!(scrub_progress_value(15_000, 12_000), 1.0);
        assert_eq!(scrub_progress_value(3_000, 0), 0.0);
    }

    #[test]
    fn begin_scrub_interaction_marks_pointer_active() {
        let mut interaction = ScrubInteraction::default();

        begin_scrub_interaction_state(&mut interaction, ScrubInput::Pointer);

        assert_eq!(
            interaction,
            ScrubInteraction {
                active: true,
                input: Some(ScrubInput::Pointer),
                preview_position_ms: None,
                pending_seek_position_ms: None,
                pending_seek_sound_id: None,
                pending_seek_deadline_ms: None,
                last_committed_position_ms: None,
                last_committed_sound_id: None,
            }
        );
    }

    #[test]
    fn record_scrub_preview_stores_preview_position() {
        let mut interaction = ScrubInteraction::default();
        begin_scrub_interaction_state(&mut interaction, ScrubInput::Pointer);

        assert_eq!(
            record_scrub_preview_state(
                &mut interaction,
                Some(12_000),
                0.75,
                Some(ScrubInput::Pointer)
            ),
            Some(9_000)
        );
        assert_eq!(interaction.preview_position_ms, Some(9_000));
    }

    #[test]
    fn record_scrub_preview_starts_pointer_interaction_when_inactive() {
        let mut interaction = ScrubInteraction::default();

        assert_eq!(
            record_scrub_preview_state(
                &mut interaction,
                Some(12_000),
                0.75,
                Some(ScrubInput::Pointer)
            ),
            Some(9_000)
        );
        assert_eq!(
            interaction,
            ScrubInteraction {
                active: true,
                input: Some(ScrubInput::Pointer),
                preview_position_ms: Some(9_000),
                pending_seek_position_ms: None,
                pending_seek_sound_id: None,
                pending_seek_deadline_ms: None,
                last_committed_position_ms: None,
                last_committed_sound_id: None,
            }
        );
    }

    #[test]
    fn record_scrub_preview_requires_input_when_inactive() {
        let mut interaction = ScrubInteraction::default();

        assert_eq!(
            record_scrub_preview_state(&mut interaction, Some(12_000), 0.75, None),
            None
        );
        assert_eq!(interaction.preview_position_ms, None);
    }

    #[test]
    fn commit_scrub_seek_returns_last_preview_and_clears_state() {
        let mut interaction = ScrubInteraction::default();
        begin_scrub_interaction_state(&mut interaction, ScrubInput::Pointer);
        record_scrub_preview_state(
            &mut interaction,
            Some(12_000),
            0.75,
            Some(ScrubInput::Pointer),
        );

        assert_eq!(
            take_scrub_commit_position(&mut interaction, Some(12_000), 0.25),
            Some(9_000)
        );
        assert_eq!(interaction, ScrubInteraction::default());
    }

    #[test]
    fn commit_scrub_seek_falls_back_to_current_value() {
        let mut interaction = ScrubInteraction::default();
        begin_scrub_interaction_state(&mut interaction, ScrubInput::Keyboard);

        assert_eq!(
            take_scrub_commit_position(&mut interaction, Some(12_000), 0.25),
            Some(3_000)
        );
        assert_eq!(interaction, ScrubInteraction::default());
    }

    #[test]
    fn second_commit_after_clear_returns_none() {
        let mut interaction = ScrubInteraction::default();
        begin_scrub_interaction_state(&mut interaction, ScrubInput::Pointer);
        record_scrub_preview_state(
            &mut interaction,
            Some(10_000),
            0.6,
            Some(ScrubInput::Pointer),
        );

        assert_eq!(
            take_scrub_commit_position(&mut interaction, Some(10_000), 0.1),
            Some(6_000)
        );
        assert_eq!(
            take_scrub_commit_position(&mut interaction, Some(10_000), 0.1),
            None
        );
    }

    #[test]
    fn cancel_scrub_interaction_clears_state() {
        let mut interaction = ScrubInteraction {
            active: true,
            input: Some(ScrubInput::Keyboard),
            preview_position_ms: Some(4_000),
            pending_seek_position_ms: Some(4_000),
            pending_seek_sound_id: Some("sound-1".to_string()),
            pending_seek_deadline_ms: Some(4_800),
            last_committed_position_ms: Some(4_000),
            last_committed_sound_id: Some("sound-1".to_string()),
        };

        clear_scrub_interaction_state(&mut interaction);

        assert_eq!(interaction, ScrubInteraction::default());
    }

    #[test]
    fn active_interaction_uses_preview_position_and_blocks_backend_sync() {
        let interaction = ScrubInteraction {
            active: true,
            input: Some(ScrubInput::Pointer),
            preview_position_ms: Some(4_000),
            pending_seek_position_ms: None,
            pending_seek_sound_id: None,
            pending_seek_deadline_ms: None,
            last_committed_position_ms: None,
            last_committed_sound_id: None,
        };

        assert_eq!(displayed_scrub_position_ms(&interaction, 2_000), 4_000);
        assert!(!should_sync_scrub_from_playback(&interaction));
    }

    #[test]
    fn inactive_interaction_uses_backend_position_and_allows_sync() {
        let interaction = ScrubInteraction::default();

        assert_eq!(displayed_scrub_position_ms(&interaction, 2_000), 2_000);
        assert!(should_sync_scrub_from_playback(&interaction));
    }

    #[test]
    fn pending_seek_blocks_sync_and_keeps_pending_display_position() {
        let interaction = ScrubInteraction {
            active: false,
            input: None,
            preview_position_ms: None,
            pending_seek_position_ms: Some(8_000),
            pending_seek_sound_id: Some("sound-1".to_string()),
            pending_seek_deadline_ms: Some(8_800),
            last_committed_position_ms: Some(8_000),
            last_committed_sound_id: Some("sound-1".to_string()),
        };

        assert_eq!(displayed_scrub_position_ms(&interaction, 1_000), 8_000);
        assert!(!should_sync_scrub_from_playback(&interaction));
    }

    #[test]
    fn settle_pending_seek_clears_after_backend_catches_up() {
        let mut interaction = ScrubInteraction {
            active: false,
            input: None,
            preview_position_ms: None,
            pending_seek_position_ms: Some(10_000),
            pending_seek_sound_id: Some("sound-1".to_string()),
            pending_seek_deadline_ms: Some(10_800),
            last_committed_position_ms: Some(10_000),
            last_committed_sound_id: Some("sound-1".to_string()),
        };

        settle_pending_seek_state(&mut interaction, "sound-1", 10_400, 10_700);
        assert_eq!(interaction.pending_seek_position_ms, Some(10_000));

        settle_pending_seek_state(
            &mut interaction,
            "sound-1",
            10_000 + SEEK_SETTLE_TOLERANCE_MS,
            10_750,
        );
        assert_eq!(interaction.pending_seek_position_ms, None);
        assert_eq!(interaction.pending_seek_sound_id, None);
        assert_eq!(interaction.pending_seek_deadline_ms, None);
    }

    #[test]
    fn settle_pending_seek_clears_when_sound_changes() {
        let mut interaction = ScrubInteraction {
            active: false,
            input: None,
            preview_position_ms: None,
            pending_seek_position_ms: Some(10_000),
            pending_seek_sound_id: Some("sound-1".to_string()),
            pending_seek_deadline_ms: Some(10_800),
            last_committed_position_ms: Some(10_000),
            last_committed_sound_id: Some("sound-1".to_string()),
        };

        settle_pending_seek_state(&mut interaction, "sound-2", 1_000, 10_700);
        assert_eq!(interaction.pending_seek_position_ms, None);
        assert_eq!(interaction.pending_seek_sound_id, None);
        assert_eq!(interaction.pending_seek_deadline_ms, None);
    }

    #[test]
    fn settle_pending_seek_clears_when_deadline_expires() {
        let mut interaction = ScrubInteraction {
            active: false,
            input: None,
            preview_position_ms: None,
            pending_seek_position_ms: Some(10_000),
            pending_seek_sound_id: Some("sound-1".to_string()),
            pending_seek_deadline_ms: Some(10_800),
            last_committed_position_ms: Some(10_000),
            last_committed_sound_id: Some("sound-1".to_string()),
        };

        settle_pending_seek_state(&mut interaction, "sound-1", 6_000, 10_750);
        assert_eq!(interaction.pending_seek_position_ms, Some(10_000));

        settle_pending_seek_state(&mut interaction, "sound-1", 6_050, 10_800);
        assert_eq!(interaction.pending_seek_position_ms, None);
        assert_eq!(interaction.pending_seek_sound_id, None);
        assert_eq!(interaction.pending_seek_deadline_ms, None);
    }

    #[test]
    fn duplicate_commit_should_not_create_pending_seek() {
        let mut interaction = ScrubInteraction {
            active: false,
            input: None,
            preview_position_ms: None,
            pending_seek_position_ms: Some(5_000),
            pending_seek_sound_id: Some("sound-1".to_string()),
            pending_seek_deadline_ms: Some(5_800),
            last_committed_position_ms: Some(5_000),
            last_committed_sound_id: Some("sound-1".to_string()),
        };

        // Clear pending state when a duplicate commit does not dispatch a seek.
        if interaction.last_committed_sound_id.as_deref() == Some("sound-1")
            && interaction.last_committed_position_ms == Some(5_000)
        {
            interaction.pending_seek_sound_id = None;
            interaction.pending_seek_position_ms = None;
            interaction.pending_seek_deadline_ms = None;
        }

        assert_eq!(interaction.pending_seek_position_ms, None);
        assert_eq!(interaction.pending_seek_sound_id, None);
        assert_eq!(interaction.pending_seek_deadline_ms, None);
    }

    #[test]
    fn is_seek_key_accepts_configured_navigation_keys() {
        assert!(is_seek_key(gdk::Key::Left));
        assert!(is_seek_key(gdk::Key::Right));
        assert!(is_seek_key(gdk::Key::KP_Left));
        assert!(is_seek_key(gdk::Key::KP_Right));
        assert!(is_seek_key(gdk::Key::Page_Up));
        assert!(is_seek_key(gdk::Key::Page_Down));
        assert!(is_seek_key(gdk::Key::Home));
        assert!(is_seek_key(gdk::Key::End));
        assert!(!is_seek_key(gdk::Key::Escape));
        assert!(!is_seek_key(gdk::Key::space));
    }

    #[test]
    fn format_duration_formats_minutes_and_seconds() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(61_000), "1:01");
    }

    #[test]
    fn resolve_scrub_duration_prefers_live_playback_duration() {
        assert_eq!(resolve_scrub_duration_ms(Some(12_000), Some(8_000)), 12_000);
    }

    #[test]
    fn resolve_scrub_duration_falls_back_to_track_duration() {
        assert_eq!(resolve_scrub_duration_ms(None, Some(8_000)), 8_000);
    }

    #[test]
    fn resolve_scrub_duration_uses_default_when_unknown() {
        assert_eq!(
            resolve_scrub_duration_ms(None, None),
            DEFAULT_SCRUB_DURATION_MS
        );
    }

    #[test]
    fn scrub_progress_value_advances_with_default_duration() {
        let duration_ms = resolve_scrub_duration_ms(None, None);

        assert!(
            scrub_progress_value(2_000, duration_ms) > scrub_progress_value(1_000, duration_ms)
        );
    }

    #[test]
    fn continue_mode_advances_when_state_allows_it() {
        assert!(should_continue_playback(
            true,
            PlayMode::Continue,
            true,
            false,
        ));
    }

    #[test]
    fn continue_mode_does_not_advance_after_manual_stop() {
        assert!(!should_continue_playback(
            true,
            PlayMode::Continue,
            true,
            true,
        ));
    }

    #[test]
    fn continue_suppression_persists_for_same_playback() {
        assert!(!should_clear_continue_suppression(Some("play-1"), "play-1",));
    }

    #[test]
    fn continue_suppression_clears_when_new_playback_starts() {
        assert!(should_clear_continue_suppression(Some("play-1"), "play-2",));
    }
}
