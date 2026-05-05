use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glib::timeout_add_local;
use gtk4::prelude::*;
use gtk4::{Adjustment, Align, Box as GtkBox, Entry, Label, Orientation, Scale, SearchEntry};

use crate::app_state::AppState;

use super::helpers::{begin_volume_edit, log_slow_ui_callback};
use super::playback::{
    play_mode_icon, play_mode_tooltip, update_play_mode_button, update_play_pause_button,
};
use super::{ScrubInteraction, TransportBar, TransportInner, TRANSPORT_BUTTON_SIZE};
use crate::ui::icons;

fn apply_transport_button_size(button: &impl IsA<gtk4::Widget>) {
    button.set_size_request(TRANSPORT_BUTTON_SIZE, TRANSPORT_BUTTON_SIZE);
    button.set_valign(Align::Center);
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
            let cfg = state.config.lock().expect("config lock poisoned");
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

        let local_vol_icon = icons::image(icons::LOCAL_AUDIO);
        local_vol_icon.add_css_class("transport-volume-icon");
        local_vol_icon.set_tooltip_text(Some("Local Sound Volume"));
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
        local_vol_entry.set_tooltip_text(Some("Local Sound Volume"));
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
                icons::LOCAL_AUDIO_MUTED
            } else {
                icons::LOCAL_AUDIO
            },
            "Toggle Local Sound",
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
}
