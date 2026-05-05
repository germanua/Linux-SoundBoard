use std::sync::Arc;

use gtk4::prelude::*;
use gtk4::ToggleButton;

use crate::commands;
use crate::config::PlayMode;

use super::helpers::should_clear_continue_suppression;
use super::{TransportBar, TransportInner};
use crate::ui::icons;

pub(super) fn update_play_pause_button(button: &ToggleButton, is_playing: bool) {
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

pub(super) fn update_mic_button(button: &ToggleButton, enabled: bool) {
    if enabled {
        button.add_css_class("btn-active");
        icons::apply_button_icon(button, icons::MICROPHONE);
    } else {
        button.remove_css_class("btn-active");
        icons::apply_button_icon(button, icons::MICROPHONE_DISABLED);
    }
}

pub(super) fn update_headphones_button(button: &ToggleButton, enabled: bool) {
    if enabled {
        button.add_css_class("btn-active");
        icons::apply_button_icon(button, icons::HEADPHONES);
    } else {
        button.remove_css_class("btn-active");
        icons::apply_button_icon(button, icons::HEADPHONES_MUTED);
    }
}

pub(super) fn play_mode_icon(mode: PlayMode) -> icons::IconPair {
    match mode {
        PlayMode::Loop => icons::PLAYMODE_LOOP,
        PlayMode::Continue => icons::PLAYMODE_CONTINUE,
        PlayMode::Default => icons::PLAYMODE_DEFAULT,
    }
}

pub(super) fn update_play_mode_button(button: &impl IsA<gtk4::Button>, mode: PlayMode) {
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

pub(super) fn play_mode_tooltip(mode: PlayMode) -> &'static str {
    match mode {
        PlayMode::Loop => "Play Mode: Loop",
        PlayMode::Continue => "Play Mode: Continue",
        PlayMode::Default => "Play Mode: Default",
    }
}

impl TransportBar {
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

    pub(super) fn apply_headphones_state(&self, local_mute: bool) {
        self.inner.suppress_headphones_toggle.set(true);
        self.inner.headphones_btn.set_active(!local_mute);
        self.inner.suppress_headphones_toggle.set(false);
        update_headphones_button(&self.inner.headphones_btn, !local_mute);
    }

    pub(super) fn apply_mic_state(&self, enabled: bool) {
        self.inner.suppress_mic_toggle.set(true);
        self.inner.mic_btn.set_active(enabled);
        self.inner.suppress_mic_toggle.set(false);
        update_mic_button(&self.inner.mic_btn, enabled);
    }
}

impl TransportInner {
    pub(super) fn has_navigation_sounds(&self) -> bool {
        let guard = self
            .sound_list_provider
            .lock()
            .expect("sound_list_provider lock poisoned");
        match guard.as_ref() {
            Some(provider) => !provider().is_empty(),
            None => false,
        }
    }

    pub(super) fn play_adjacent_sound(&self, offset: i32) {
        self.clear_continue_suppression();
        let sounds = match self
            .sound_list_provider
            .lock()
            .expect("sound_list_provider lock poisoned")
            .as_ref()
        {
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

    pub(super) fn stop_all_playback(&self) {
        *self.continue_suppressed_play_id.borrow_mut() = self
            .active_track
            .borrow()
            .as_ref()
            .map(|track| track.play_id.clone());
        commands::stop_all(Arc::clone(&self.state.player));
    }

    pub(super) fn is_continue_suppressed(&self) -> bool {
        self.continue_suppressed_play_id.borrow().is_some()
    }

    pub(super) fn clear_continue_suppression(&self) {
        *self.continue_suppressed_play_id.borrow_mut() = None;
    }

    pub(super) fn clear_continue_suppression_for_playback(&self, play_id: &str) {
        let should_clear = should_clear_continue_suppression(
            self.continue_suppressed_play_id.borrow().as_deref(),
            play_id,
        );
        if should_clear {
            self.clear_continue_suppression();
        }
    }

    pub(super) fn reset_idle_playback_ui(&self) {
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
