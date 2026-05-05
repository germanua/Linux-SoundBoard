use std::time::Instant;

use gtk4::gdk;
use gtk4::prelude::*;
use gtk4::{Adjustment, Entry, EventControllerFocus, EventControllerKey, Label};

use crate::config::PlayMode;

use super::{
    ScrubInput, ScrubInteraction, DEFAULT_SCRUB_DURATION_MS, PENDING_SEEK_TIMEOUT_MS,
    SEEK_SETTLE_TOLERANCE_MS, SLOW_GTK_CALLBACK_THRESHOLD_MS,
};

pub(super) fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

pub(super) fn log_slow_ui_callback(name: &str, started_at: Instant) {
    let elapsed_ms = started_at.elapsed().as_millis();
    if elapsed_ms >= SLOW_GTK_CALLBACK_THRESHOLD_MS {
        log::debug!(
            "GTK callback latency exceeded threshold: name={} elapsed_ms={}",
            name,
            elapsed_ms
        );
    }
}

pub(super) fn scrub_position_ms(value: f64, duration_ms: u64) -> u64 {
    value.clamp(0.0, 1.0).mul_add(duration_ms as f64, 0.0) as u64
}

pub(super) fn resolve_scrub_duration_ms(
    playback_duration_ms: Option<u64>,
    track_duration_ms: Option<u64>,
) -> u64 {
    playback_duration_ms
        .or(track_duration_ms)
        .filter(|duration_ms| *duration_ms > 0)
        .unwrap_or(DEFAULT_SCRUB_DURATION_MS)
}

pub(super) fn scrub_progress_value(position_ms: u64, duration_ms: u64) -> f64 {
    if duration_ms == 0 {
        0.0
    } else {
        (position_ms as f64 / duration_ms as f64).clamp(0.0, 1.0)
    }
}

pub(super) fn begin_scrub_interaction_state(interaction: &mut ScrubInteraction, input: ScrubInput) {
    if !interaction.active {
        interaction.active = true;
    }
    interaction.input = Some(input);
    interaction.pending_seek_position_ms = None;
    interaction.pending_seek_sound_id = None;
    interaction.pending_seek_deadline_ms = None;
}

pub(super) fn record_scrub_preview_state(
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

pub(super) fn take_scrub_commit_position(
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

pub(super) fn clear_scrub_interaction_state(interaction: &mut ScrubInteraction) {
    interaction.active = false;
    interaction.input = None;
    interaction.preview_position_ms = None;
    interaction.pending_seek_position_ms = None;
    interaction.pending_seek_sound_id = None;
    interaction.pending_seek_deadline_ms = None;
    interaction.last_committed_position_ms = None;
    interaction.last_committed_sound_id = None;
}

pub(super) fn pending_seek_deadline_ms_from_now() -> u64 {
    (glib::monotonic_time() as u64 / 1_000) + PENDING_SEEK_TIMEOUT_MS
}

pub(super) fn settle_pending_seek_state(
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

pub(super) fn displayed_scrub_position_ms(
    interaction: &ScrubInteraction,
    playback_position_ms: u64,
) -> u64 {
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

pub(super) fn should_sync_scrub_from_playback(interaction: &ScrubInteraction) -> bool {
    !interaction.active && interaction.pending_seek_position_ms.is_none()
}

pub(super) fn should_continue_playback(
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

pub(super) fn should_clear_continue_suppression(
    suppressed_play_id: Option<&str>,
    active_play_id: &str,
) -> bool {
    matches!(suppressed_play_id, Some(suppressed_play_id) if suppressed_play_id != active_play_id)
}

pub(super) fn is_seek_key(keyval: gdk::Key) -> bool {
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

pub(super) fn begin_volume_edit(label: &Label, entry: &Entry) {
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

pub(super) fn install_volume_editor(adjustment: &Adjustment, label: &Label, entry: &Entry) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::gdk;

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

        // Clear pending state when the commit matches the last dispatched seek.
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
