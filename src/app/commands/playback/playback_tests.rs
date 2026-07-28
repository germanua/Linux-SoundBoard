use super::{
    estimated_loudness_refinement_trigger, fast_loudness_preview_budget_ms,
    missing_loudness_analysis_trigger, EstimatedLoudnessRefinementTrigger,
    MissingLoudnessAnalysisTrigger, FAST_LUFS_PREVIEW_TOTAL_MS_LONG,
    FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM,
};

#[test]
fn fast_loudness_preview_budget_uses_medium_default_without_duration_hint() {
    assert_eq!(
        fast_loudness_preview_budget_ms(None),
        FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM
    );
}

#[test]
fn fast_loudness_preview_budget_uses_medium_budget_for_medium_tracks() {
    assert_eq!(
        fast_loudness_preview_budget_ms(Some(60_000)),
        FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM
    );
}

#[test]
fn fast_loudness_preview_budget_uses_long_budget_for_long_tracks() {
    assert_eq!(
        fast_loudness_preview_budget_ms(Some(120_000)),
        FAST_LUFS_PREVIEW_TOTAL_MS_LONG
    );
}

#[test]
fn estimated_refinement_trigger_skips_without_candidates() {
    assert_eq!(
        estimated_loudness_refinement_trigger(true, false, false, false),
        EstimatedLoudnessRefinementTrigger::SkippedNoCandidates
    );
}

#[test]
fn estimated_refinement_trigger_skips_when_auto_gain_disabled() {
    assert_eq!(
        estimated_loudness_refinement_trigger(false, true, false, false),
        EstimatedLoudnessRefinementTrigger::SkippedAutoGainDisabled
    );
}

#[test]
fn estimated_refinement_trigger_starts_with_force_even_if_auto_gain_disabled() {
    assert_eq!(
        estimated_loudness_refinement_trigger(false, true, true, false),
        EstimatedLoudnessRefinementTrigger::Started
    );
}

#[test]
fn missing_loudness_analysis_skips_when_auto_gain_disabled() {
    assert_eq!(
        missing_loudness_analysis_trigger(false, true, false, false),
        MissingLoudnessAnalysisTrigger::SkippedAutoGainDisabled
    );
}

#[test]
fn missing_loudness_analysis_skips_when_no_sounds_need_backfill() {
    assert_eq!(
        missing_loudness_analysis_trigger(true, false, false, false),
        MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds
    );
}

#[test]
fn missing_loudness_analysis_skips_when_job_already_running() {
    assert_eq!(
        missing_loudness_analysis_trigger(true, true, false, true),
        MissingLoudnessAnalysisTrigger::SkippedAlreadyRunning
    );
}

#[test]
fn missing_loudness_analysis_force_mode_bypasses_auto_gain_setting() {
    assert_eq!(
        missing_loudness_analysis_trigger(false, true, true, false),
        MissingLoudnessAnalysisTrigger::Started
    );
}

/// Dispatch coverage moved in from the integration crate when the Config-based
/// `play_sound` was removed. `play_resolved_sound` is the live path and keeps
/// the same guards: disabled sounds and missing sources never reach the engine,
/// and everything stops before a new clip starts.
mod dispatch {
    use std::sync::Arc;

    use crate::audio::PlaybackEngine;
    use crate::commands::CommandError;
    use crate::config::Sound;
    use crate::test_support::audio_mock::FakeAudioPlayer;

    struct ClipOnDisk(std::path::PathBuf);

    impl Drop for ClipOnDisk {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sound_on_disk(name: &str) -> (Sound, ClipOnDisk, String) {
        let dir = std::env::temp_dir().join(format!("lsb-dispatch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("clip.wav");
        std::fs::write(&path, b"placeholder - only needs to exist on disk").expect("write clip");
        let path = path.to_string_lossy().to_string();
        (
            Sound::new(name.to_string(), path.clone()),
            ClipOnDisk(dir),
            path,
        )
    }

    #[test]
    fn dispatches_play_with_resolved_volume_after_stopping_everything() {
        let (mut sound, _dir, path) = sound_on_disk("Airhorn");
        sound.volume = 50;
        let sound_id = sound.id.clone();
        let fake = Arc::new(FakeAudioPlayer::new());

        let play_id =
            super::super::play_resolved_sound(sound, fake.clone() as Arc<dyn PlaybackEngine>)
                .expect("an enabled sound that exists should play");

        fake.assert_played(&sound_id);
        let calls = fake.play_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].path, path);
        // 50% sound volume must reach the engine as a 0.5 base gain.
        assert_eq!(calls[0].base_volume, 0.5);
        assert!(play_id.starts_with("fake-play-"));
        assert_eq!(fake.stop_all_calls(), 1);
    }

    #[test]
    fn disabled_sound_does_not_reach_the_engine() {
        let (mut sound, _dir, _path) = sound_on_disk("Disabled");
        sound.enabled = false;
        let fake = Arc::new(FakeAudioPlayer::new());

        let result =
            super::super::play_resolved_sound(sound, fake.clone() as Arc<dyn PlaybackEngine>);

        assert!(matches!(result, Err(CommandError::SoundDisabled)));
        fake.assert_no_plays();
        assert_eq!(fake.stop_all_calls(), 0);
    }

    #[test]
    fn missing_source_does_not_reach_the_engine() {
        let mut sound = Sound::new("Gone".to_string(), "/nonexistent/clip.wav".to_string());
        sound.enabled = true;
        let fake = Arc::new(FakeAudioPlayer::new());

        let result =
            super::super::play_resolved_sound(sound, fake.clone() as Arc<dyn PlaybackEngine>);

        assert!(matches!(result, Err(CommandError::SourceUnavailable(_))));
        fake.assert_no_plays();
        assert_eq!(fake.stop_all_calls(), 0);
    }
}
