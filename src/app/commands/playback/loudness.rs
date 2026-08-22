use rayon::prelude::*;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::audio::loudness;
use crate::commands::shared::adaptive_audio_analysis_plan;
use crate::config::{LoudnessAnalysisState, Sound};
use crate::library_store::{LibraryStore, LoudnessUpdate};

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

pub(super) fn fast_loudness_preview_budget_ms(duration_hint_ms: Option<u64>) -> u32 {
    match duration_hint_ms {
        Some(duration_ms) if duration_ms > FAST_LUFS_MEDIUM_TRACK_THRESHOLD_MS => {
            FAST_LUFS_PREVIEW_TOTAL_MS_LONG
        }
        _ => FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM,
    }
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
    cancel: &AtomicBool,
) -> Result<(f64, LoudnessAnalysisState, Option<f32>, Option<f32>), crate::audio::LoudnessError> {
    if duration_hint_ms.is_some_and(|duration_ms| duration_ms <= FAST_LUFS_FULL_SCAN_THRESHOLD_MS) {
        return loudness::analyze_loudness_path_full(path, cancel).map(|(lufs, tp)| {
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
        cancel,
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
            loudness::analyze_loudness_path_full(path, cancel).map(|(lufs, tp)| {
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

fn backfill_update(outcome: BackfillOutcome) -> LoudnessUpdate {
    match outcome {
        BackfillOutcome::Analyzed {
            id,
            lufs,
            state,
            confidence,
            true_peak_dbtp,
        } => LoudnessUpdate {
            sound_id: id,
            lufs: Some(lufs),
            state,
            confidence,
            true_peak_dbtp,
        },
        BackfillOutcome::Unavailable { id } => LoudnessUpdate {
            sound_id: id,
            lufs: None,
            state: LoudnessAnalysisState::Unavailable,
            confidence: None,
            true_peak_dbtp: None,
        },
    }
}

fn refinement_update(outcome: RefinementOutcome, sound: &Sound) -> LoudnessUpdate {
    match outcome {
        RefinementOutcome::Refined {
            id,
            lufs,
            true_peak_dbtp,
        } => LoudnessUpdate {
            sound_id: id,
            lufs: Some(lufs),
            state: LoudnessAnalysisState::Refined,
            confidence: Some(FAST_LUFS_REFINED_CONFIDENCE),
            true_peak_dbtp,
        },
        RefinementOutcome::Deferred {
            id,
            backoff_confidence,
        } => LoudnessUpdate {
            sound_id: id,
            lufs: sound.loudness_lufs,
            state: LoudnessAnalysisState::Estimated,
            confidence: Some(
                sound
                    .loudness_confidence
                    .unwrap_or(0.0)
                    .max(backoff_confidence)
                    .clamp(0.0, 1.0),
            ),
            true_peak_dbtp: sound.loudness_true_peak_dbtp,
        },
        RefinementOutcome::Unavailable { id } => LoudnessUpdate {
            sound_id: id,
            lufs: None,
            state: LoudnessAnalysisState::Unavailable,
            confidence: None,
            true_peak_dbtp: None,
        },
    }
}

pub(super) fn maybe_trigger_estimated_loudness_refinement_with_store(
    auto_gain_enabled: bool,
    library: LibraryStore,
    force: bool,
    coords: &LoudnessCoordinators,
) -> Result<EstimatedLoudnessRefinementTrigger, CommandError> {
    let has_candidates = !library
        .loudness_refinement_candidates(force, None, 1)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?
        .sounds
        .is_empty();
    let trigger = estimated_loudness_refinement_trigger(
        auto_gain_enabled,
        has_candidates,
        force,
        coords.refinement.is_in_flight(),
    );
    if trigger != EstimatedLoudnessRefinementTrigger::Started {
        return Ok(trigger);
    }
    let cancel = coords.refinement.cancel_token();
    let started = coords
        .refinement
        .try_start(
            "loudness-refinement",
            move || refine_estimated_loudness_with_store(library, force, &cancel),
            Some(Box::new(|_| {
                crate::ui_event_bridge::post_loudness_status_refresh();
            })),
        )
        .map_err(|error| CommandError::Analysis(error.to_string()))?;
    Ok(if started {
        trigger
    } else {
        EstimatedLoudnessRefinementTrigger::SkippedAlreadyRunning
    })
}

const LOUDNESS_PROGRESS_FLUSH_ROWS: usize = 8;

fn should_flush_loudness_progress(pending: usize, page_finished: bool) -> bool {
    pending > 0 && (page_finished || pending >= LOUDNESS_PROGRESS_FLUSH_ROWS)
}

fn refine_estimated_loudness_with_store(
    library: LibraryStore,
    force: bool,
    cancel: &AtomicBool,
) -> Result<u32, crate::audio::LoudnessError> {
    let limit = if force {
        crate::library_store::MAX_BATCH_ROWS
    } else {
        FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN
    };
    let mut refined = 0_u32;
    let mut after = None::<String>;
    loop {
        let sounds = library
            .loudness_refinement_candidates(force, after.as_deref(), limit)
            .recv()
            .map_err(|error| crate::audio::LoudnessError::Io(error.to_string()))?
            .sounds;
        if sounds.is_empty() {
            break;
        }
        after = sounds.last().map(|sound| sound.id.clone());
        let mut updates = Vec::with_capacity(sounds.len());
        for sound in sounds {
            if cancel.load(Ordering::SeqCst) {
                break;
            }
            let id = sound.id.clone();
            let path = sound.path.clone();
            let outcome = if !Path::new(&path).exists() {
                RefinementOutcome::Unavailable { id }
            } else {
                match loudness::analyze_loudness_path_full(Path::new(&path), cancel) {
                    Ok((lufs, true_peak_dbtp)) if lufs.is_finite() => {
                        refined = refined.saturating_add(1);
                        RefinementOutcome::Refined {
                            id,
                            lufs,
                            true_peak_dbtp,
                        }
                    }
                    Ok(_) => RefinementOutcome::Unavailable { id },
                    Err(error) if should_mark_unavailable_loudness_error(&error) => {
                        RefinementOutcome::Unavailable { id }
                    }
                    Err(error) => {
                        log::warn!("Failed to refine loudness for '{path}': {error}");
                        RefinementOutcome::Deferred {
                            id,
                            backoff_confidence: FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD + 0.05,
                        }
                    }
                }
            };
            updates.push(refinement_update(outcome, &sound));
            // Push partial progress so the settings counts move mid-run.
            if should_flush_loudness_progress(updates.len(), false) {
                library
                    .apply_loudness_updates(std::mem::take(&mut updates))
                    .recv()
                    .map_err(|error| crate::audio::LoudnessError::Io(error.to_string()))?;
                crate::ui_event_bridge::post_loudness_status_refresh();
            }
        }
        if should_flush_loudness_progress(updates.len(), true) {
            library
                .apply_loudness_updates(std::mem::take(&mut updates))
                .recv()
                .map_err(|error| crate::audio::LoudnessError::Io(error.to_string()))?;
            crate::ui_event_bridge::post_loudness_status_refresh();
        }
        if !force || cancel.load(Ordering::SeqCst) {
            break;
        }
    }
    Ok(refined)
}

pub fn analyze_all_loudness_with_store(
    library: LibraryStore,
    auto_gain_enabled: bool,
    coords: LoudnessCoordinators,
) -> Result<u32, crate::audio::LoudnessError> {
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:start");
    let cancel = coords.backfill.cancel_token();
    let mut after = None::<String>;
    let mut analyzed = 0_u32;
    loop {
        let sounds = library
            .loudness_backfill_after(after.as_deref())
            .recv()
            .map_err(|error| crate::audio::LoudnessError::Io(error.to_string()))?
            .sounds;
        if sounds.is_empty() {
            break;
        }
        after = sounds.last().map(|sound| sound.id.clone());
        let analysis_plan = adaptive_audio_analysis_plan(sounds.len());
        let analyze = |sound: &Sound| -> Option<BackfillOutcome> {
            if cancel.load(Ordering::SeqCst) {
                return None;
            }
            if !Path::new(&sound.path).exists() {
                return Some(BackfillOutcome::Unavailable {
                    id: sound.id.clone(),
                });
            }
            match analyze_loudness_for_backfill(Path::new(&sound.path), sound.duration_ms, &cancel)
            {
                Ok((lufs, state, confidence, true_peak_dbtp)) if lufs.is_finite() => {
                    Some(BackfillOutcome::Analyzed {
                        id: sound.id.clone(),
                        lufs,
                        state,
                        confidence,
                        true_peak_dbtp,
                    })
                }
                Ok(_) => Some(BackfillOutcome::Unavailable {
                    id: sound.id.clone(),
                }),
                Err(error) if should_mark_unavailable_loudness_error(&error) => {
                    Some(BackfillOutcome::Unavailable {
                        id: sound.id.clone(),
                    })
                }
                Err(error) => {
                    log::warn!("Failed to analyze loudness for '{}': {error}", sound.path);
                    None
                }
            }
        };
        let outcomes = match rayon::ThreadPoolBuilder::new()
            .num_threads(analysis_plan.threads)
            .build()
        {
            Ok(pool) => pool.install(|| sounds.par_iter().filter_map(analyze).collect::<Vec<_>>()),
            Err(_) => sounds.iter().filter_map(analyze).collect(),
        };
        analyzed = analyzed.saturating_add(
            outcomes
                .iter()
                .filter(|outcome| matches!(outcome, BackfillOutcome::Analyzed { .. }))
                .count() as u32,
        );
        let updates = outcomes
            .into_iter()
            .map(backfill_update)
            .collect::<Vec<_>>();
        if !updates.is_empty() {
            library
                .apply_loudness_updates(updates)
                .recv()
                .map_err(|error| crate::audio::LoudnessError::Io(error.to_string()))?;
            crate::ui_event_bridge::post_loudness_status_refresh();
        }
        if cancel.load(Ordering::SeqCst) {
            break;
        }
    }
    crate::diagnostics::clear_work_runtime();
    if let Err(error) = maybe_trigger_estimated_loudness_refinement_with_store(
        auto_gain_enabled,
        library,
        false,
        &coords,
    ) {
        log::warn!("Failed to schedule loudness refinement: {error}");
    }
    Ok(analyzed)
}

#[cfg(test)]
mod progress_tests {
    #[test]
    fn flushes_loudness_progress_once_the_pending_batch_fills() {
        assert!(should_flush_loudness_progress(
            LOUDNESS_PROGRESS_FLUSH_ROWS,
            false
        ));
    }

    #[test]
    fn holds_loudness_progress_while_the_batch_is_small() {
        assert!(!should_flush_loudness_progress(1, false));
    }

    #[test]
    fn always_flushes_what_is_left_when_a_page_finishes() {
        assert!(should_flush_loudness_progress(1, true));
    }

    #[test]
    fn never_flushes_an_empty_batch() {
        assert!(!should_flush_loudness_progress(0, true));
    }

    use super::*;
}
