use super::{
    collect_refinement_candidates, estimated_loudness_refinement_trigger,
    fast_loudness_preview_budget_ms, get_loudness_status_summary,
    missing_loudness_analysis_trigger, sound_needs_loudness_backfill,
    sound_needs_loudness_refinement, EstimatedLoudnessRefinementTrigger, LoudnessCoordinators,
    MissingLoudnessAnalysisTrigger, FAST_LUFS_PREVIEW_TOTAL_MS_LONG,
    FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM, FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD,
    FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN,
};
use crate::config::{Config, LoudnessAnalysisState, Sound};
use parking_lot::Mutex;
use std::sync::Arc;

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
fn sound_needs_loudness_backfill_skips_unavailable() {
    let mut sound = Sound::new("test".to_string(), "/tmp/test.wav".to_string());
    assert!(sound_needs_loudness_backfill(&sound));

    sound.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
    assert!(!sound_needs_loudness_backfill(&sound));
}

#[test]
fn sound_needs_loudness_refinement_requires_estimated_state() {
    let mut sound = Sound::new("test".to_string(), "/tmp/test.wav".to_string());
    sound.loudness_lufs = Some(-15.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Refined;
    sound.loudness_confidence = Some(0.5);
    assert!(!sound_needs_loudness_refinement(&sound, false));
}

#[test]
fn sound_needs_loudness_refinement_uses_confidence_threshold() {
    let mut sound = Sound::new("test".to_string(), "/tmp/test.wav".to_string());
    sound.loudness_lufs = Some(-15.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD + 0.05);
    assert!(!sound_needs_loudness_refinement(&sound, false));

    sound.loudness_confidence = Some(FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD - 0.05);
    assert!(sound_needs_loudness_refinement(&sound, false));
}

#[test]
fn estimated_refinement_force_mode_ignores_confidence() {
    let mut sound = Sound::new("test".to_string(), "/tmp/test.wav".to_string());
    sound.loudness_lufs = Some(-15.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(1.0);
    assert!(sound_needs_loudness_refinement(&sound, true));
}

#[test]
fn collect_refinement_candidates_prioritizes_low_confidence_then_long_duration() {
    let mut cfg = Config::default();

    let mut high_conf = Sound::new("high".to_string(), "/tmp/high.wav".to_string());
    high_conf.id = "high".to_string();
    high_conf.loudness_lufs = Some(-14.0);
    high_conf.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    high_conf.loudness_confidence = Some(0.95);
    high_conf.duration_ms = Some(40_000);

    let mut low_short = Sound::new("low-short".to_string(), "/tmp/low-short.wav".to_string());
    low_short.id = "low-short".to_string();
    low_short.loudness_lufs = Some(-14.0);
    low_short.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    low_short.loudness_confidence = Some(0.20);
    low_short.duration_ms = Some(20_000);

    let mut low_long = Sound::new("low-long".to_string(), "/tmp/low-long.wav".to_string());
    low_long.id = "low-long".to_string();
    low_long.loudness_lufs = Some(-14.0);
    low_long.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    low_long.loudness_confidence = Some(0.20);
    low_long.duration_ms = Some(120_000);

    let mut refined = Sound::new("refined".to_string(), "/tmp/refined.wav".to_string());
    refined.id = "refined".to_string();
    refined.loudness_lufs = Some(-14.0);
    refined.loudness_analysis_state = LoudnessAnalysisState::Refined;
    refined.loudness_confidence = Some(1.0);
    refined.duration_ms = Some(120_000);

    cfg.sounds = vec![high_conf, low_short, low_long, refined];

    let candidates = collect_refinement_candidates(&cfg, false);
    let ordered_ids = candidates
        .iter()
        .map(|candidate| candidate.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ordered_ids, vec!["low-long", "low-short"]);
}

#[test]
fn collect_refinement_candidates_respects_run_budget() {
    let cfg = Config {
        sounds: (0..(FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN + 4))
            .map(|idx| {
                let mut sound = Sound::new(format!("sound-{idx}"), format!("/tmp/sound-{idx}.wav"));
                sound.id = format!("sound-{idx:03}");
                sound.loudness_lufs = Some(-14.0);
                sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
                sound.loudness_confidence = Some((idx as f32 / 100.0).clamp(0.0, 1.0));
                sound.duration_ms = Some((idx as u64 + 1) * 1_000);
                sound
            })
            .collect(),
        ..Default::default()
    };

    let candidates = collect_refinement_candidates(&cfg, false);

    assert_eq!(candidates.len(), FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN);
    assert_eq!(candidates[0].id, "sound-000");
}

#[test]
fn collect_refinement_candidates_force_mode_bypasses_run_budget() {
    let cfg = Config {
        sounds: (0..(FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN + 4))
            .map(|idx| {
                let mut sound = Sound::new(format!("sound-{idx}"), format!("/tmp/sound-{idx}.wav"));
                sound.id = format!("sound-{idx:03}");
                sound.loudness_lufs = Some(-14.0);
                sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
                sound.loudness_confidence = Some(0.1);
                sound
            })
            .collect(),
        ..Default::default()
    };

    let candidates = collect_refinement_candidates(&cfg, true);

    assert_eq!(
        candidates.len(),
        FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN + 4
    );
}

#[test]
fn collect_refinement_candidates_force_mode_bypasses_confidence_threshold() {
    let mut cfg = Config::default();

    let mut sound = Sound::new("estimated".to_string(), "/tmp/estimated.wav".to_string());
    sound.id = "estimated".to_string();
    sound.loudness_lufs = Some(-14.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(0.99);
    cfg.sounds = vec![sound];

    assert!(collect_refinement_candidates(&cfg, false).is_empty());
    assert_eq!(collect_refinement_candidates(&cfg, true).len(), 1);
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
fn loudness_status_summary_counts_states() {
    let mut cfg = Config::default();

    let mut pending = Sound::new("pending".to_string(), "/tmp/pending.wav".to_string());
    pending.loudness_analysis_state = LoudnessAnalysisState::Pending;
    pending.loudness_lufs = None;

    let mut estimated = Sound::new("estimated".to_string(), "/tmp/estimated.wav".to_string());
    estimated.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    estimated.loudness_lufs = Some(-15.0);
    estimated.loudness_confidence = Some(0.66);

    let mut refined = Sound::new("refined".to_string(), "/tmp/refined.wav".to_string());
    refined.loudness_analysis_state = LoudnessAnalysisState::Refined;
    refined.loudness_lufs = Some(-14.0);
    refined.loudness_confidence = Some(1.0);

    let mut unavailable = Sound::new(
        "unavailable".to_string(),
        "/tmp/unavailable.wav".to_string(),
    );
    unavailable.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
    unavailable.loudness_lufs = None;

    cfg.sounds = vec![pending, estimated, refined, unavailable];

    let coords = LoudnessCoordinators::new();
    let summary =
        get_loudness_status_summary(Arc::new(Mutex::new(cfg)), &coords).expect("status summary");

    assert_eq!(summary.total_sounds, 4);
    assert_eq!(summary.pending_count, 1);
    assert_eq!(summary.estimated_count, 1);
    assert_eq!(summary.refined_count, 1);
    assert_eq!(summary.unavailable_count, 1);
    assert_eq!(summary.missing_loudness_count, 2);
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
