//! Transport control bar — playback controls, scrub bar, volume sliders.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glib::timeout_add_local;
use gtk4::prelude::*;
use gtk4::{
    Adjustment, Box as GtkBox, Button, Entry, EventControllerFocus, EventControllerKey, Label,
    Orientation, Scale, SearchEntry, ToggleButton, Widget,
};

use crate::app_state::AppState;
use crate::commands;
use crate::config::{PlayMode, Sound};

use super::icons;

type SoundListProvider = Box<dyn Fn() -> Vec<Sound> + Send + Sync>;
type LibraryChangedCallback = Rc<dyn Fn() + 'static>;
type ListStyleChangedCallback = Rc<dyn Fn(String) + 'static>;

/// Which sound is currently the "active track" in the transport bar.
#[derive(Clone)]
struct ActiveTrack {
    sound: Sound,
    #[allow(dead_code)]
    play_id: String,
}

/// The transport bar widget bundle.
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
    last_track_sound_id: RefCell<Option<String>>,
    state: Arc<AppState>,
    sound_list_provider: Mutex<Option<SoundListProvider>>,
    toast_sender: Mutex<Option<std::sync::mpsc::Sender<String>>>,
    on_library_changed: RefCell<Option<LibraryChangedCallback>>,
    on_list_style_changed: RefCell<Option<ListStyleChangedCallback>>,
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
        update_play_pause_button(&play_btn, false);

        let stop_btn = icons::button(icons::STOP, "Stop All");
        stop_btn.set_sensitive(false);
        stop_btn.add_css_class("transport-btn");
        stop_btn.add_css_class("transport-playback-btn");

        let prev_btn = icons::button(icons::PREVIOUS, "Previous Sound");
        prev_btn.set_sensitive(false);
        prev_btn.add_css_class("transport-btn");
        prev_btn.add_css_class("transport-playback-btn");

        let next_btn = icons::button(icons::NEXT, "Next Sound");
        next_btn.set_sensitive(false);
        next_btn.add_css_class("transport-btn");
        next_btn.add_css_class("transport-playback-btn");

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
        let local_vol_adj = Adjustment::new(local_vol_value, 0.0, 100.0, 1.0, 10.0, 0.0);
        let local_vol = Scale::builder()
            .adjustment(&local_vol_adj)
            .orientation(Orientation::Horizontal)
            .draw_value(false)
            .width_request(64)
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
        let mic_vol_adj = Adjustment::new(mic_vol_value, 0.0, 100.0, 1.0, 10.0, 0.0);
        let mic_vol = Scale::builder()
            .adjustment(&mic_vol_adj)
            .orientation(Orientation::Horizontal)
            .draw_value(false)
            .width_request(64)
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
        update_play_mode_button(&playmode_btn, play_mode);
        utility_group.append(&playmode_btn);

        let refresh_btn = icons::button(icons::REFRESH, "Refresh Sounds");
        refresh_btn.add_css_class("transport-btn");
        refresh_btn.add_css_class("transport-icon-btn");
        refresh_btn.add_css_class("transport-refresh-btn");
        utility_group.append(&refresh_btn);

        let search_entry = SearchEntry::builder()
            .placeholder_text("Search sounds…")
            .width_request(130)
            .build();
        search_entry.add_css_class("transport-search");
        utility_group.append(&search_entry);

        let settings_btn = icons::button(icons::SETTINGS, "Settings");
        settings_btn.add_css_class("transport-btn");
        settings_btn.add_css_class("transport-icon-btn");
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
            last_track_sound_id: RefCell::new(None),
            state,
            sound_list_provider: Mutex::new(None),
            toast_sender: Mutex::new(None),
            on_library_changed: RefCell::new(None),
            on_list_style_changed: RefCell::new(None),
        });

        let tb = Self { inner };
        tb.connect_signals();

        {
            let inner_poll = Rc::clone(&tb.inner);
            timeout_add_local(Duration::from_millis(150), move || {
                inner_poll.update_scrub();
                glib::ControlFlow::Continue
            });
        }

        tb
    }

    pub fn widget(&self) -> &Widget {
        self.inner.widget.upcast_ref()
    }

    /// Connect a callback to the search entry's `search-changed` signal.
    pub fn connect_search_changed<F: Fn(String) + 'static>(&self, f: F) {
        self.inner
            .search_entry
            .connect_search_changed(move |entry| {
                f(entry.text().to_string());
            });
    }

    /// Store a closure that returns the currently filtered sound list.
    pub fn set_sound_list_provider<F: Fn() -> Vec<Sound> + Send + Sync + 'static>(&self, f: F) {
        *self.inner.sound_list_provider.lock().unwrap() = Some(Box::new(f));
    }

    /// Toggle play/pause on the current active track (for hotkey dispatch).
    pub fn toggle_play_pause(&self) {
        let track = self.inner.active_track.borrow();
        if let Some(track) = track.as_ref() {
            let positions = commands::get_playback_positions(Arc::clone(&self.inner.state.player));
            let is_paused = positions
                .iter()
                .any(|position| position.sound_id == track.sound.id && position.paused);
            if is_paused {
                commands::resume_sound(
                    track.sound.id.clone(),
                    Arc::clone(&self.inner.state.player),
                );
                update_play_pause_button(&self.inner.play_btn, true);
            } else {
                commands::pause_sound(track.sound.id.clone(), Arc::clone(&self.inner.state.player));
                update_play_pause_button(&self.inner.play_btn, false);
            }
        }
    }

    /// Play the previous sound in the list (for hotkey dispatch).
    pub fn play_previous(&self) {
        self.inner.play_adjacent_sound(-1);
    }

    /// Play the next sound in the list (for hotkey dispatch).
    pub fn play_next(&self) {
        self.inner.play_adjacent_sound(1);
    }

    /// Toggle headphones mute (for hotkey dispatch).
    pub fn toggle_headphones_mute(&self) {
        let _ = commands::toggle_local_mute(
            Arc::clone(&self.inner.state.config),
            Arc::clone(&self.inner.state.player),
        );
    }

    /// Toggle mic passthrough (for hotkey dispatch).
    pub fn toggle_mic_mute(&self) {
        let _ = commands::toggle_mic_passthrough(Arc::clone(&self.inner.state.config));
    }

    /// Cycle play mode default → loop → continue (for hotkey dispatch).
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
        let _ = commands::set_play_mode(
            new_mode.as_str().to_string(),
            Arc::clone(&self.inner.state.config),
            Arc::clone(&self.inner.state.player),
        );
        update_play_mode_button(&self.inner.playmode_btn, new_mode);
    }

    /// Store a sender used to dispatch toast notifications.
    pub fn set_toast_sender(&self, sender: std::sync::mpsc::Sender<String>) {
        *self.inner.toast_sender.lock().unwrap() = Some(sender);
    }

    /// Register callback fired after library mutations like refresh/import from settings.
    pub fn connect_library_changed<F: Fn() + 'static>(&self, f: F) {
        *self.inner.on_library_changed.borrow_mut() = Some(Rc::new(f));
    }

    /// Register callback fired when list style changes in settings.
    pub fn connect_list_style_changed<F: Fn(String) + 'static>(&self, f: F) {
        *self.inner.on_list_style_changed.borrow_mut() = Some(Rc::new(f));
    }

    fn connect_signals(&self) {
        let inner = Rc::clone(&self.inner);

        {
            let state = Arc::clone(&inner.state);
            inner.stop_btn.connect_clicked(move |_| {
                commands::stop_all(Arc::clone(&state.player));
            });
        }

        {
            let state = Arc::clone(&inner.state);
            let inner_toggle = Rc::clone(&inner);
            inner.play_btn.connect_clicked(move |btn| {
                let should_resume = btn.is_active();
                update_play_pause_button(btn, should_resume);
                if let Some(track) = inner_toggle.active_track.borrow().as_ref() {
                    if should_resume {
                        commands::resume_sound(track.sound.id.clone(), Arc::clone(&state.player));
                    } else {
                        commands::pause_sound(track.sound.id.clone(), Arc::clone(&state.player));
                    }
                }
            });
        }

        {
            let state = Arc::clone(&inner.state);
            let local_adj = inner.local_vol.adjustment();
            let local_label = inner.local_vol_label.clone();
            let local_entry = inner.local_vol_entry.clone();
            local_adj.connect_value_changed(move |adj| {
                let volume = adj.value().round().clamp(0.0, 100.0) as u8;
                local_label.set_label(&format!("{volume}"));
                if !gtk4::prelude::WidgetExt::is_visible(&local_entry) {
                    local_entry.set_text(&format!("{volume}"));
                }
                let _ = commands::set_local_volume(
                    volume,
                    Arc::clone(&state.config),
                    Arc::clone(&state.player),
                );
            });
        }

        {
            let state = Arc::clone(&inner.state);
            let mic_adj = inner.mic_vol.adjustment();
            let mic_label = inner.mic_vol_label.clone();
            let mic_entry = inner.mic_vol_entry.clone();
            mic_adj.connect_value_changed(move |adj| {
                let volume = adj.value().round().clamp(0.0, 100.0) as u8;
                mic_label.set_label(&format!("{volume}"));
                if !gtk4::prelude::WidgetExt::is_visible(&mic_entry) {
                    mic_entry.set_text(&format!("{volume}"));
                }
                let _ = commands::set_mic_volume(
                    volume,
                    Arc::clone(&state.config),
                    Arc::clone(&state.player),
                );
            });
        }

        install_volume_editor(
            &inner.local_vol.adjustment(),
            &inner.local_vol_label,
            &inner.local_vol_entry,
        );
        install_volume_editor(
            &inner.mic_vol.adjustment(),
            &inner.mic_vol_label,
            &inner.mic_vol_entry,
        );

        {
            let state = Arc::clone(&inner.state);
            inner.headphones_btn.connect_toggled(move |btn| {
                match commands::toggle_local_mute(
                    Arc::clone(&state.config),
                    Arc::clone(&state.player),
                ) {
                    Ok(muted) => {
                        if muted {
                            btn.remove_css_class("btn-active");
                            icons::apply_button_icon(btn, icons::HEADPHONES_MUTED);
                        } else {
                            btn.add_css_class("btn-active");
                            icons::apply_button_icon(btn, icons::HEADPHONES);
                        }
                    }
                    Err(e) => log::warn!("Toggle local mute failed: {e}"),
                }
            });
        }

        {
            let state = Arc::clone(&inner.state);
            inner.mic_btn.connect_toggled(move |btn| {
                match commands::toggle_mic_passthrough(Arc::clone(&state.config)) {
                    Ok(enabled) => {
                        if enabled {
                            btn.add_css_class("btn-active");
                            icons::apply_button_icon(btn, icons::MICROPHONE);
                        } else {
                            btn.remove_css_class("btn-active");
                            icons::apply_button_icon(btn, icons::MICROPHONE_DISABLED);
                        }
                    }
                    Err(e) => log::warn!("Toggle mic passthrough failed: {e}"),
                }
            });
        }

        {
            let state = Arc::clone(&inner.state);
            let inner_seek = Rc::clone(&inner);
            inner.scrub.connect_change_value(move |_, _, value| {
                if let Some(track) = inner_seek.active_track.borrow().as_ref() {
                    if let Some(duration_ms) = track.sound.duration_ms {
                        let position_ms = (value * duration_ms as f64) as u64;
                        let _ = commands::seek_sound(
                            track.sound.id.clone(),
                            position_ms,
                            Arc::clone(&state.player),
                        );
                    }
                }
                glib::Propagation::Proceed
            });
        }

        {
            let state = Arc::clone(&inner.state);
            let inner_refresh = Rc::clone(&inner);
            inner.refresh_btn.connect_clicked(move |btn| {
                btn.add_css_class("spinning");
                let state_refresh = Arc::clone(&state);
                let inner_refresh_done = Rc::clone(&inner_refresh);
                let btn_done = btn.clone();
                glib::MainContext::default().spawn_local(async move {
                    match commands::refresh_sounds(
                        Arc::clone(&state_refresh.config),
                        Arc::clone(&state_refresh.hotkeys),
                    ) {
                        Ok(_) => {
                            if let Some(tx) = &*inner_refresh_done.toast_sender.lock().unwrap() {
                                let _ = tx.send("Sounds refreshed".to_string());
                            }
                            if let Some(cb) =
                                inner_refresh_done.on_library_changed.borrow().as_ref()
                            {
                                cb();
                            }
                        }
                        Err(e) => log::warn!("Refresh failed: {e}"),
                    }
                    btn_done.remove_css_class("spinning");
                });
            });
        }

        {
            let state = Arc::clone(&inner.state);
            let inner_settings = Rc::clone(&inner);
            inner.settings_btn.connect_clicked(move |btn| {
                if let Some(win) = btn
                    .root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok())
                {
                    let on_library_changed =
                        inner_settings.on_library_changed.borrow().as_ref().cloned();
                    let on_list_style_changed = inner_settings
                        .on_list_style_changed
                        .borrow()
                        .as_ref()
                        .cloned();
                    crate::ui::settings::show_settings(
                        &win,
                        Arc::clone(&state),
                        on_library_changed,
                        on_list_style_changed,
                    );
                }
            });
        }

        {
            let state = Arc::clone(&inner.state);
            inner.playmode_btn.connect_clicked(move |btn| {
                let current_mode = state.config.lock().unwrap().settings.play_mode;
                let new_mode = current_mode.next();
                let _ = commands::set_play_mode(
                    new_mode.as_str().to_string(),
                    Arc::clone(&state.config),
                    Arc::clone(&state.player),
                );
                update_play_mode_button(btn, new_mode);
            });
        }

        {
            let inner_prev = Rc::clone(&inner);
            inner.prev_btn.connect_clicked(move |_| {
                inner_prev.play_adjacent_sound(-1);
            });
        }

        {
            let inner_next = Rc::clone(&inner);
            inner.next_btn.connect_clicked(move |_| {
                inner_next.play_adjacent_sound(1);
            });
        }
    }
}

impl TransportInner {
    fn has_navigation_sounds(&self) -> bool {
        let guard = self.sound_list_provider.lock().unwrap();
        match guard.as_ref() {
            Some(provider) => !provider().is_empty(),
            None => false,
        }
    }

    /// Play the sound at `offset` positions from the current active track.
    /// Wraps around at list boundaries.
    fn play_adjacent_sound(&self, offset: i32) {
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
            .map(|track| track.sound.id.clone())
            .or_else(|| self.last_track_sound_id.borrow().clone());

        let current_idx = current_id
            .and_then(|id| sounds.iter().position(|sound| sound.id == id))
            .map(|idx| idx as i32)
            .unwrap_or_else(|| if offset >= 0 { -1 } else { 0 });

        let len = sounds.len() as i32;
        let next_idx = ((current_idx + offset) % len + len) % len;
        let next_sound = &sounds[next_idx as usize];

        commands::stop_all(Arc::clone(&self.state.player));
        if let Err(e) = commands::play_sound(
            next_sound.id.clone(),
            Arc::clone(&self.state.config),
            Arc::clone(&self.state.player),
        ) {
            log::warn!("Play adjacent failed for '{}': {}", next_sound.name, e);
        }
    }

    /// Called every 150ms to update the scrub bar from playback positions.
    fn update_scrub(&self) {
        let positions = commands::get_playback_positions(Arc::clone(&self.state.player));

        if positions.is_empty() {
            let should_continue = self.last_track_sound_id.borrow().is_some()
                && self.state.config.lock().unwrap().settings.play_mode == PlayMode::Continue
                && self.has_navigation_sounds();
            if should_continue {
                self.play_adjacent_sound(1);
                return;
            }

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
            return;
        }

        let has_provider = self.sound_list_provider.lock().unwrap().is_some();
        self.prev_btn.set_sensitive(has_provider);
        self.next_btn.set_sensitive(has_provider);

        if let Some(position) = positions.iter().find(|position| !position.finished) {
            self.stop_btn.set_sensitive(true);
            self.play_btn.set_sensitive(true);
            update_play_pause_button(&self.play_btn, !position.paused);

            if let Some(duration_ms) = position.duration_ms {
                if duration_ms > 0 {
                    self.scrub.set_sensitive(true);
                    self.scrub.set_value(
                        (position.position_ms as f64 / duration_ms as f64).clamp(0.0, 1.0),
                    );
                }
                self.dur_label.set_text(&format_duration(duration_ms));
            }
            self.time_label
                .set_text(&format_duration(position.position_ms));

            let cfg = self.state.config.lock().unwrap();
            if let Some(sound) = cfg.get_sound(&position.sound_id) {
                self.track_name_label.set_label(&sound.name);
                self.track_name_label.set_visible(true);
                *self.active_track.borrow_mut() = Some(ActiveTrack {
                    sound: sound.clone(),
                    play_id: position.play_id.clone(),
                });
                *self.last_track_sound_id.borrow_mut() = Some(sound.id.clone());
            }
        } else if positions.iter().all(|position| position.finished)
            && self.active_track.borrow().is_some()
        {
            if self.state.config.lock().unwrap().settings.play_mode == PlayMode::Continue {
                self.play_adjacent_sound(1);
            } else {
                *self.active_track.borrow_mut() = None;
            }
        }
    }
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
