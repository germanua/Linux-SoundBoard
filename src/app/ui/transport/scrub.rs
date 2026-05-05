use std::sync::Arc;

use gtk4::prelude::*;

use crate::commands;

use super::helpers::{
    begin_scrub_interaction_state, clear_scrub_interaction_state, displayed_scrub_position_ms,
    format_duration, pending_seek_deadline_ms_from_now, record_scrub_preview_state,
    resolve_scrub_duration_ms, scrub_progress_value, settle_pending_seek_state,
    should_continue_playback, should_sync_scrub_from_playback, take_scrub_commit_position,
};
use super::playback::update_play_pause_button;
use super::{ScrubInput, TransportInner};

impl TransportInner {
    pub(super) fn begin_scrub_interaction(&self, input: ScrubInput) {
        begin_scrub_interaction_state(&mut self.scrub_interaction.borrow_mut(), input);
    }

    pub(super) fn record_scrub_preview(&self, value: f64) -> Option<u64> {
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

    pub(super) fn commit_scrub_seek_on_release(&self) {
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

    pub(super) fn cancel_scrub_interaction(&self) {
        clear_scrub_interaction_state(&mut self.scrub_interaction.borrow_mut());
    }

    pub(super) fn update_scrub(&self) {
        let positions = commands::get_playback_positions(Arc::clone(&self.state.player));
        let now_ms = glib::monotonic_time() as u64 / 1_000;

        if positions.is_empty() {
            self.cancel_scrub_interaction();
            let play_mode = {
                self.state
                    .config
                    .lock()
                    .expect("config lock poisoned")
                    .settings
                    .play_mode
            };
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

        let has_provider = self
            .sound_list_provider
            .lock()
            .expect("sound_list_provider lock poisoned")
            .is_some();
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
                let cfg = self.state.config.lock().expect("config lock poisoned");
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
                *self.active_track.borrow_mut() = Some(super::ActiveTrack {
                    sound_id: sound_id.clone(),
                    sound_duration_ms: Some(duration_ms),
                    play_id: position.play_id.clone(),
                });
                *self.last_track_sound_id.borrow_mut() = Some(sound_id);
            } else {
                self.track_name_label.set_visible(false);
                *self.active_track.borrow_mut() = Some(super::ActiveTrack {
                    sound_id: position.sound_id.clone(),
                    sound_duration_ms: Some(duration_ms),
                    play_id: position.play_id.clone(),
                });
                *self.last_track_sound_id.borrow_mut() = Some(position.sound_id.clone());
            }
        } else if positions.iter().all(|position| position.finished) {
            let play_mode = {
                self.state
                    .config
                    .lock()
                    .expect("config lock poisoned")
                    .settings
                    .play_mode
            };
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
}
