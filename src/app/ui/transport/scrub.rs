use gtk4::prelude::*;

use crate::audio::PlayerSnapshot;
use crate::commands;
use crate::timer_registry::remove_source_id_safe;

use super::helpers::{
    begin_scrub_interaction_state, clear_scrub_interaction_state, displayed_scrub_position_ms,
    format_duration, pending_seek_deadline_ms_from_now, record_scrub_preview_state,
    resolve_scrub_duration_ms, scrub_progress_value, settle_pending_seek_state,
    should_apply_resolved_track_name, should_continue_playback, should_sync_scrub_from_playback,
    take_scrub_commit_position,
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
                commands::seek_sound(track.sound_id, position_ms, self.state.player.clone())
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
        if let Some(id) = self.scrub_commit_timeout.borrow_mut().take() {
            let _ = remove_source_id_safe(id);
        }
    }

    pub(crate) fn handle_snapshot(self: &std::rc::Rc<Self>, snapshot: PlayerSnapshot) {
        let positions = snapshot.playback_positions;
        let now_ms = glib::monotonic_time() as u64 / 1_000;

        if positions.is_empty() {
            self.cancel_scrub_interaction();
            let play_mode = { self.state.config.lock().settings.play_mode };
            let has_navigation_sounds = self.has_navigation_sounds();
            let should_continue = should_continue_playback(
                self.last_track_sound_id.borrow().is_some(),
                play_mode,
                has_navigation_sounds,
                self.is_continue_suppressed(),
            );
            if should_continue {
                if crate::ui_event_bridge::is_explicit_play_pending() {
                    return;
                }
                self.play_adjacent_sound(1);
                return;
            }

            crate::ui_event_bridge::clear_explicit_play_pending();
            self.clear_continue_suppression();
            self.reset_idle_playback_ui();
            return;
        }

        let has_provider = self.has_sound_list_provider.get();
        self.prev_btn.set_sensitive(has_provider);
        self.next_btn.set_sensitive(has_provider);

        if let Some(position) = positions.iter().find(|position| !position.finished) {
            // New sound is active — any pending explicit play has now landed.
            crate::ui_event_bridge::clear_explicit_play_pending();
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

            let (same_play, sound_name, track_duration_ms) =
                match self.active_track.borrow().as_ref() {
                    Some(track) if track.play_id == position.play_id => {
                        (true, track.sound_name.clone(), track.sound_duration_ms)
                    }
                    _ => (false, None, None),
                };

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

            if !same_play {
                match &sound_name {
                    Some(name) => {
                        self.track_name_label.set_label(name);
                        self.track_name_label.set_visible(true);
                    }
                    None => {
                        self.track_name_label.set_visible(false);
                        self.resolve_track_name_async(&position.sound_id, &position.play_id);
                    }
                }
                *self.last_track_sound_id.borrow_mut() = Some(position.sound_id.clone());
                *self.active_track.borrow_mut() = Some(super::ActiveTrack {
                    sound_id: position.sound_id.clone(),
                    sound_name: sound_name.clone(),
                    sound_duration_ms: Some(duration_ms),
                    play_id: position.play_id.clone(),
                });
            }

            publish_now_playing(position, duration_ms, &self.active_track.borrow());
        } else if positions.iter().all(|position| position.finished) {
            let play_mode = { self.state.config.lock().settings.play_mode };
            let has_navigation_sounds = self.has_navigation_sounds();
            if should_continue_playback(
                self.last_track_sound_id.borrow().is_some(),
                play_mode,
                has_navigation_sounds,
                self.is_continue_suppressed(),
            ) {
                if crate::ui_event_bridge::is_explicit_play_pending() {
                    return;
                }
                self.play_adjacent_sound(1);
            } else {
                crate::ui_event_bridge::clear_explicit_play_pending();
                self.clear_continue_suppression();
                self.reset_idle_playback_ui();
            }
        }
    }

    fn resolve_track_name_async(self: &std::rc::Rc<Self>, sound_id: &str, play_id: &str) {
        let response = self.state.library.sound_by_id(sound_id);
        let weak = std::rc::Rc::downgrade(self);
        let play_id = play_id.to_string();
        if let Err(error) = commands::dispatch_async_result(
            "resolve_transport_track_name",
            move || response.recv(),
            move |result| {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                let Ok(Some(sound)) = result else {
                    return;
                };
                let mut active = inner.active_track.borrow_mut();
                let current = active.as_ref().map(|track| track.play_id.as_str());
                if !should_apply_resolved_track_name(current, &play_id) {
                    return;
                }
                if let Some(track) = active.as_mut() {
                    track.sound_name = Some(sound.name.clone());
                }
                drop(active);
                inner.track_name_label.set_label(&sound.name);
                inner.track_name_label.set_visible(true);
                // The first snapshot for this sound could not name it, so the
                // media controls were told nothing. Tell them now.
                let active = inner.active_track.borrow();
                if let Some(track) = active.as_ref() {
                    crate::ui_event_bridge::post_now_playing(Some(crate::mpris::NowPlaying {
                        id: track.sound_id.clone(),
                        title: sound.name.clone(),
                        duration_ms: track.sound_duration_ms,
                        paused: false,
                    }));
                }
            },
        ) {
            log::warn!("Could not resolve the playing sound's name: {error}");
        }
    }
}

fn now_playing_for(
    position: &crate::audio::PlaybackPosition,
    duration_ms: u64,
    active: &Option<super::ActiveTrack>,
) -> Option<crate::mpris::NowPlaying> {
    let title = active
        .as_ref()
        .filter(|track| track.play_id == position.play_id)
        .and_then(|track| track.sound_name.clone())?;
    Some(crate::mpris::NowPlaying {
        id: position.sound_id.clone(),
        title,
        duration_ms: Some(duration_ms),
        paused: position.paused,
    })
}

fn publish_now_playing(
    position: &crate::audio::PlaybackPosition,
    duration_ms: u64,
    active: &Option<super::ActiveTrack>,
) {
    if let Some(now) = now_playing_for(position, duration_ms, active) {
        crate::ui_event_bridge::post_now_playing(Some(now));
    }
}

#[cfg(test)]
mod now_playing_tests {
    use super::*;
    use crate::audio::PlaybackPosition;

    fn position(paused: bool) -> PlaybackPosition {
        PlaybackPosition {
            play_id: "play-1".to_string(),
            sound_id: "sound-1".to_string(),
            position_ms: 500,
            paused,
            finished: false,
            duration_ms: Some(2_500),
        }
    }

    fn track(name: Option<&str>, play_id: &str) -> Option<super::super::ActiveTrack> {
        Some(super::super::ActiveTrack {
            sound_id: "sound-1".to_string(),
            sound_name: name.map(str::to_string),
            sound_duration_ms: Some(2_500),
            play_id: play_id.to_string(),
        })
    }

    #[test]
    fn a_named_sound_is_announced_with_its_name_and_length() {
        let now = now_playing_for(
            &position(false),
            2_500,
            &track(Some("airhorn.mp3"), "play-1"),
        )
        .expect("a named sound is announced");
        assert_eq!(now.title, "airhorn.mp3");
        assert_eq!(now.id, "sound-1");
        assert_eq!(now.duration_ms, Some(2_500));
        assert!(!now.paused);
    }

    #[test]
    fn a_paused_sound_says_so() {
        let now = now_playing_for(
            &position(true),
            2_500,
            &track(Some("airhorn.mp3"), "play-1"),
        )
        .expect("a paused sound is still announced");
        assert!(now.paused);
    }

    /// The name is resolved asynchronously, so the first snapshot for a sound
    /// has none yet.
    #[test]
    fn a_sound_whose_name_is_not_known_yet_announces_nothing() {
        assert_eq!(
            now_playing_for(&position(false), 2_500, &track(None, "play-1")),
            None
        );
        assert_eq!(now_playing_for(&position(false), 2_500, &None), None);
    }

    /// Guards against announcing the previous sound's name over the new one.
    #[test]
    fn a_name_left_over_from_another_playback_is_not_used() {
        assert_eq!(
            now_playing_for(&position(false), 2_500, &track(Some("stale.mp3"), "play-0")),
            None
        );
    }
}
