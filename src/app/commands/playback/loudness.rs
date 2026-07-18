use parking_lot::Mutex;
use rayon::prelude::*;
use std::path::Path;
use std::sync::Arc;

use crate::audio::loudness;
use crate::commands::shared::{adaptive_audio_analysis_plan, with_config, with_config_mut};
use crate::config::{Config, LoudnessAnalysisState, Sound};

use super::{
    CommandError, EstimatedLoudnessRefinementTrigger, LoudnessCoordinators,
    FAST_LUFS_ESTIMATED_CONFIDENCE, FAST_LUFS_FULL_SCAN_THRESHOLD_MS,
    FAST_LUFS_MEDIUM_TRACK_THRESHOLD_MS, FAST_LUFS_PREVIEW_TOTAL_MS_LONG,
    FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM, FAST_LUFS_REFINED_CONFIDENCE,
    FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD, FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN,
};

pub(super) fn estimated_loudness_refinement_trigger(
    auto_gain_enabled: bool,
    has_candidates: bool,
    force: bool,
    in_flight: bool,
) -> EstimatedLoudnessRefinementTrigger {
    if !force && !auto_gain_enabled {
        return EstimatedLoudnessRefinementTrigger::SkippedAutoGainDisabled;
    }
    if !has_candidates {
        return EstimatedLoudnessRefinementTrigger::SkippedNoCandidates;
    }
    if in_flight {
        return EstimatedLoudnessRefinementTrigger::SkippedAlreadyRunning;
    }
    EstimatedLoudnessRefinementTrigger::Started
}

pub(super) fn maybe_trigger_estimated_loudness_refinement(
    config: Arc<Mutex<Config>>,
    force: bool,
    coords: &LoudnessCoordinators,
) -> Result<EstimatedLoudnessRefinementTrigger, CommandError> {
    let trigger = with_config(&config, |cfg| {
        estimated_loudness_refinement_trigger(
            cfg.settings.auto_gain,
            cfg.sounds
                .iter()
                .any(|sound| sound_needs_loudness_refinement(sound, force)),
            force,
            coords.refinement.is_in_flight(),
        )
    })?;

    if trigger != EstimatedLoudnessRefinementTrigger::Started {
        return Ok(trigger);
    }

    let started = coords
        .refinement
        .try_start(
            "loudness-refinement",
            move || refine_estimated_loudness(config, force),
            Some(Box::new(|_| {
                crate::ui_event_bridge::post_loudness_status_refresh();
            })),
        )
        .map_err(|e| CommandError::Analysis(e.to_string()))?;

    if !started {
        return Ok(EstimatedLoudnessRefinementTrigger::SkippedAlreadyRunning);
    }

    Ok(EstimatedLoudnessRefinementTrigger::Started)
}

pub(super) fn fast_loudness_preview_budget_ms(duration_hint_ms: Option<u64>) -> u32 {
    match duration_hint_ms {
        Some(duration_ms) if duration_ms > FAST_LUFS_MEDIUM_TRACK_THRESHOLD_MS => {
            FAST_LUFS_PREVIEW_TOTAL_MS_LONG
        }
        _ => FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM,
    }
}

pub(super) fn sound_needs_loudness_backfill(sound: &Sound) -> bool {
    sound.loudness_lufs.is_none()
        && sound.loudness_analysis_state != LoudnessAnalysisState::Unavailable
}

pub(super) fn sound_needs_loudness_refinement(sound: &Sound, force: bool) -> bool {
    if sound.loudness_analysis_state != LoudnessAnalysisState::Estimated {
        return false;
    }
    if sound.loudness_lufs.is_none() {
        return false;
    }
    if force {
        return true;
    }
    sound.loudness_confidence.unwrap_or(0.0) <= FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD
}

fn should_mark_unavailable_loudness_error(err: &crate::audio::LoudnessError) -> bool {
    matches!(
        err,
        crate::audio::LoudnessError::Decode(_) | crate::audio::LoudnessError::NoResult(_)
    )
}

fn analyze_loudness_for_backfill(
    path: &Path,
    duration_hint_ms: Option<u64>,
) -> Result<(f64, LoudnessAnalysisState, Option<f32>, Option<f32>), crate::audio::LoudnessError> {
    if duration_hint_ms.is_some_and(|duration_ms| duration_ms <= FAST_LUFS_FULL_SCAN_THRESHOLD_MS) {
        return loudness::analyze_loudness_path_full(path).map(|(lufs, tp)| {
            (
                lufs,
                LoudnessAnalysisState::Refined,
                Some(FAST_LUFS_REFINED_CONFIDENCE),
                tp,
            )
        });
    }

    let preview_budget_ms = fast_loudness_preview_budget_ms(duration_hint_ms);
    match loudness::analyze_loudness_path_preview_smart_with_metrics(
        path,
        preview_budget_ms,
        duration_hint_ms,
    ) {
        Ok(metrics) => {
            let confidence = if metrics.confidence.is_finite() {
                metrics.confidence.clamp(0.0, 1.0)
            } else {
                FAST_LUFS_ESTIMATED_CONFIDENCE
            };

            Ok((
                metrics.lufs,
                LoudnessAnalysisState::Estimated,
                Some(confidence),
                metrics.true_peak_dbtp,
            ))
        }
        Err(err) => {
            log::debug!(
                "Fast loudness preview failed for '{}' ({}); falling back to full analysis",
                path.display(),
                err
            );
            loudness::analyze_loudness_path_full(path).map(|(lufs, tp)| {
                (
                    lufs,
                    LoudnessAnalysisState::Refined,
                    Some(FAST_LUFS_REFINED_CONFIDENCE),
                    tp,
                )
            })
        }
    }
}

enum BackfillOutcome {
    Analyzed {
        id: String,
        lufs: f64,
        state: LoudnessAnalysisState,
        confidence: Option<f32>,
        true_peak_dbtp: Option<f32>,
    },
    Unavailable {
        id: String,
    },
}

enum RefinementOutcome {
    Refined {
        id: String,
        lufs: f64,
        true_peak_dbtp: Option<f32>,
    },
    Deferred {
        id: String,
        backoff_confidence: f32,
    },
    Unavailable {
        id: String,
    },
}

fn apply_backfill_outcome(config: &mut Config, outcome: &BackfillOutcome) {
    match outcome {
        BackfillOutcome::Analyzed {
            id,
            lufs,
            state,
            confidence,
            true_peak_dbtp,
        } => {
            if let Some(sound) = config.sounds.iter_mut().find(|sound| sound.id == *id) {
                sound.loudness_lufs = Some(*lufs);
                sound.loudness_analysis_state = *state;
                sound.loudness_confidence = *confidence;
                sound.loudness_true_peak_dbtp = *true_peak_dbtp;
            }
        }
        BackfillOutcome::Unavailable { id } => {
            if let Some(sound) = config.sounds.iter_mut().find(|sound| sound.id == *id) {
                sound.loudness_lufs = None;
                sound.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
                sound.loudness_confidence = None;
                sound.loudness_true_peak_dbtp = None;
            }
        }
    }
}

fn apply_refinement_outcome(config: &mut Config, outcome: &RefinementOutcome) {
    match outcome {
        RefinementOutcome::Refined {
            id,
            lufs,
            true_peak_dbtp,
        } => {
            if let Some(sound) = config.sounds.iter_mut().find(|sound| sound.id == *id) {
                sound.loudness_lufs = Some(*lufs);
                sound.loudness_analysis_state = LoudnessAnalysisState::Refined;
                sound.loudness_confidence = Some(FAST_LUFS_REFINED_CONFIDENCE);
                sound.loudness_true_peak_dbtp = *true_peak_dbtp;
            }
        }
        RefinementOutcome::Deferred {
            id,
            backoff_confidence,
        } => {
            if let Some(sound) = config.sounds.iter_mut().find(|sound| sound.id == *id) {
                let current_confidence = sound.loudness_confidence.unwrap_or(0.0);
                sound.loudness_confidence =
                    Some(current_confidence.max(*backoff_confidence).clamp(0.0, 1.0));
            }
        }
        RefinementOutcome::Unavailable { id } => {
            if let Some(sound) = config.sounds.iter_mut().find(|sound| sound.id == *id) {
                sound.loudness_lufs = None;
                sound.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
                sound.loudness_confidence = None;
                sound.loudness_true_peak_dbtp = None;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct RefinementCandidate {
    pub(super) id: String,
    pub(super) path: String,
    pub(super) confidence: f32,
    pub(super) duration_ms: u64,
}

fn normalized_loudness_confidence(confidence: Option<f32>) -> f32 {
    match confidence {
        Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
        _ => 0.0,
    }
}

pub(super) fn collect_refinement_candidates(
    config: &Config,
    force: bool,
) -> Vec<RefinementCandidate> {
    let mut candidates = config
        .sounds
        .iter()
        .filter(|sound| sound_needs_loudness_refinement(sound, force))
        .map(|sound| RefinementCandidate {
            id: sound.id.clone(),
            path: sound.path.clone(),
            confidence: normalized_loudness_confidence(sound.loudness_confidence),
            duration_ms: sound.duration_ms.unwrap_or(0),
        })
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        left.confidence
            .partial_cmp(&right.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.duration_ms.cmp(&left.duration_ms))
            .then_with(|| left.id.cmp(&right.id))
    });

    if !force && candidates.len() > FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN {
        candidates.truncate(FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN);
    }

    candidates
}

fn refine_estimated_loudness(
    config: Arc<Mutex<Config>>,
    force: bool,
) -> Result<u32, crate::audio::LoudnessError> {
    crate::diagnostics::memory::log_memory_snapshot("refine_estimated_loudness:start");
    loudness::reset_loudness_analysis_cancelled();

    let candidates = with_config(&config, |cfg| collect_refinement_candidates(cfg, force))
        .map_err(|e| crate::audio::LoudnessError::Io(e.to_string()))?;

    if candidates.is_empty() {
        return Ok(0);
    }

    log::info!(
        "Refining loudness for {} estimated sounds (budget: {})",
        candidates.len(),
        FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN
    );

    let mut outcomes = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let id = candidate.id;
        let path = candidate.path;
        if loudness::is_loudness_analysis_cancelled() {
            break;
        }

        let outcome = if !Path::new(&path).exists() {
            RefinementOutcome::Unavailable { id }
        } else {
            match loudness::analyze_loudness_path_full(Path::new(&path)) {
                Ok((lufs, true_peak_dbtp)) if lufs.is_finite() => RefinementOutcome::Refined {
                    id,
                    lufs,
                    true_peak_dbtp,
                },
                Ok((lufs, _)) => {
                    log::warn!(
                        "Deferring refinement after non-finite result for '{}': {}",
                        path,
                        lufs
                    );
                    RefinementOutcome::Unavailable { id }
                }
                Err(err) => {
                    if should_mark_unavailable_loudness_error(&err) {
                        log::warn!(
                        "Marking sound as unavailable after terminal refinement error for '{}': {}",
                        path,
                        err
                    );
                        RefinementOutcome::Unavailable { id }
                    } else {
                        log::warn!("Failed to refine loudness for '{}': {}", path, err);
                        RefinementOutcome::Deferred {
                            id,
                            backoff_confidence: FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD + 0.05,
                        }
                    }
                }
            }
        };

        with_config_mut(&config, |cfg| apply_refinement_outcome(cfg, &outcome))
            .map_err(|e| crate::audio::LoudnessError::Io(e.to_string()))?;
        crate::ui_event_bridge::post_loudness_status_refresh();
        outcomes.push(outcome);
    }

    let refined_count = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, RefinementOutcome::Refined { .. }))
        .count() as u32;
    if !outcomes.is_empty() {
        with_config_mut(&config, |cfg| {
            cfg.save()
                .map_err(|e| crate::audio::LoudnessError::Io(e.to_string()))
        })
        .map_err(|e| crate::audio::LoudnessError::Io(e.to_string()))??;
    }

    crate::diagnostics::memory::log_memory_snapshot("refine_estimated_loudness:end");
    Ok(refined_count)
}

pub fn analyze_all_loudness(
    config: Arc<Mutex<Config>>,
    coords: LoudnessCoordinators,
) -> Result<u32, crate::audio::LoudnessError> {
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:start");
    let sounds_to_analyze: Vec<(String, String, Option<u64>)> = with_config(&config, |cfg| {
        cfg.sounds
            .iter()
            .filter(|s| sound_needs_loudness_backfill(s))
            .map(|s| (s.id.clone(), s.path.clone(), s.duration_ms))
            .collect()
    })
    .map_err(|e| crate::audio::LoudnessError::Io(e.to_string()))?;
    crate::diagnostics::record_phase_with_config("analyze_all_loudness:start", &config.lock());

    log::info!("Analyzing loudness for {} sounds", sounds_to_analyze.len());

    loudness::reset_loudness_analysis_cancelled();

    let analyze_entry =
        |(id, path, duration_hint_ms): &(String, String, Option<u64>)| -> Option<BackfillOutcome> {
            if loudness::is_loudness_analysis_cancelled() {
                return None;
            }
            if !Path::new(path).exists() {
                return Some(BackfillOutcome::Unavailable { id: id.clone() });
            }
            match analyze_loudness_for_backfill(Path::new(path), *duration_hint_ms) {
                Ok((lufs, state, confidence, true_peak_dbtp)) if lufs.is_finite() => {
                    Some(BackfillOutcome::Analyzed {
                        id: id.clone(),
                        lufs,
                        state,
                        confidence,
                        true_peak_dbtp,
                    })
                }
                Ok((lufs, _, _, _)) => {
                    log::warn!(
                    "Marking sound as unavailable due to non-finite loudness result for '{}': {}",
                    path,
                    lufs
                );
                    Some(BackfillOutcome::Unavailable { id: id.clone() })
                }
                Err(e) => {
                    if should_mark_unavailable_loudness_error(&e) {
                        log::warn!(
                        "Marking sound as unavailable due to terminal loudness analysis error for '{}': {}",
                        path,
                        e
                    );
                        return Some(BackfillOutcome::Unavailable { id: id.clone() });
                    }
                    log::warn!("Failed to analyze loudness for '{}': {}", path, e);
                    None
                }
            }
        };

    let analysis_plan = adaptive_audio_analysis_plan(sounds_to_analyze.len());
    let analysis_threads = analysis_plan.threads;
    let pool_threads = if sounds_to_analyze.is_empty() {
        1
    } else {
        analysis_threads
    };
    if analysis_plan.throttled {
        log::info!(
            "Adaptive loudness analysis throttling applied: threads={} base={} rss={}kB process_threads={}",
            analysis_plan.threads,
            analysis_plan.base_threads,
            analysis_plan.rss_kb.unwrap_or(0),
            analysis_plan.process_threads.unwrap_or(0)
        );
    }
    crate::diagnostics::set_work_runtime(
        "loudness_analysis",
        sounds_to_analyze.len(),
        pool_threads,
    );
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:before_pool");
    crate::diagnostics::record_phase_with_config(
        "analyze_all_loudness:before_pool",
        &config.lock(),
    );
    let results: Vec<BackfillOutcome> = if sounds_to_analyze.is_empty() {
        Vec::new()
    } else {
        match rayon::ThreadPoolBuilder::new()
            .num_threads(analysis_threads)
            .build()
        {
            Ok(pool) => pool.install(|| {
                sounds_to_analyze
                    .par_iter()
                    .filter_map(analyze_entry)
                    .inspect(|outcome| {
                        apply_backfill_outcome(&mut config.lock(), outcome);
                        crate::ui_event_bridge::post_loudness_status_refresh();
                    })
                    .collect::<Vec<_>>()
            }),
            Err(e) => {
                log::warn!(
                    "Failed to build bounded loudness pool ({} threads): {}. Falling back to sequential analysis.",
                    analysis_threads, e
                );
                sounds_to_analyze
                    .iter()
                    .filter_map(analyze_entry)
                    .inspect(|outcome| {
                        apply_backfill_outcome(&mut config.lock(), outcome);
                        crate::ui_event_bridge::post_loudness_status_refresh();
                    })
                    .collect::<Vec<_>>()
            }
        }
    };
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:after_pool");
    crate::diagnostics::record_phase_with_config("analyze_all_loudness:after_pool", &config.lock());

    let has_updates = !results.is_empty();
    let analyzed_count = results
        .iter()
        .filter(|result| matches!(result, BackfillOutcome::Analyzed { .. }))
        .count() as u32;
    if has_updates {
        with_config_mut(&config, |cfg| {
            cfg.save()
                .map_err(|e| crate::audio::LoudnessError::Io(e.to_string()))?;
            crate::diagnostics::record_phase_with_config(
                "playback:loudness_analysis_complete",
                cfg,
            );
            Ok::<(), crate::audio::LoudnessError>(())
        })
        .map_err(|e| crate::audio::LoudnessError::Io(e.to_string()))??;
    } else {
        crate::diagnostics::record_phase_with_config(
            "playback:loudness_analysis_complete",
            &config.lock(),
        );
    }

    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:end");
    crate::diagnostics::clear_work_runtime();
    if let Err(err) =
        maybe_trigger_estimated_loudness_refinement(Arc::clone(&config), false, &coords)
    {
        log::warn!(
            "Failed to schedule estimated loudness refinement after fast backfill: {}",
            err
        );
    }
    Ok(analyzed_count)
}

#[cfg(test)]
mod progress_tests {
    use super::*;

    #[test]
    fn applying_each_backfill_outcome_advances_status_state() {
        let mut config = Config::default();
        let sound = Sound::new("Pending".to_string(), "/tmp/pending.wav".to_string());
        let id = sound.id.clone();
        config.sounds.push(sound);

        apply_backfill_outcome(
            &mut config,
            &BackfillOutcome::Analyzed {
                id,
                lufs: -14.0,
                state: LoudnessAnalysisState::Refined,
                confidence: Some(1.0),
                true_peak_dbtp: Some(-1.0),
            },
        );

        assert_eq!(
            config.sounds[0].loudness_analysis_state,
            LoudnessAnalysisState::Refined
        );
    }

    #[test]
    fn applying_each_refinement_outcome_advances_status_state() {
        let mut config = Config::default();
        let mut sound = Sound::new("Estimated".to_string(), "/tmp/estimated.wav".to_string());
        sound.loudness_lufs = Some(-16.0);
        sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
        let id = sound.id.clone();
        config.sounds.push(sound);

        apply_refinement_outcome(
            &mut config,
            &RefinementOutcome::Refined {
                id,
                lufs: -14.0,
                true_peak_dbtp: Some(-1.0),
            },
        );

        assert_eq!(
            config.sounds[0].loudness_analysis_state,
            LoudnessAnalysisState::Refined
        );
    }
}
