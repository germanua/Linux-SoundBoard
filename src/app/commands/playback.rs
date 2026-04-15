use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, LazyLock, Mutex,
};

use rayon::prelude::*;

use crate::audio::loudness;
use crate::audio::player::{AudioPlayer, PlaybackPosition};
use crate::config::{Config, LoudnessAnalysisState, Sound};

use super::shared::{
    adaptive_audio_analysis_plan, dispatch_async_result, parse_auto_gain_apply_to,
    parse_auto_gain_mode, validate_play_mode, with_saved_config,
};

static MISSING_LOUDNESS_ANALYSIS_COORDINATOR:
    LazyLock<crate::audio::analysis_worker::MissingLoudnessAnalysisCoordinator> =
    LazyLock::new(crate::audio::analysis_worker::MissingLoudnessAnalysisCoordinator::new);
static ESTIMATED_LOUDNESS_REFINEMENT_COORDINATOR:
    LazyLock<crate::audio::analysis_worker::MissingLoudnessAnalysisCoordinator> =
    LazyLock::new(crate::audio::analysis_worker::MissingLoudnessAnalysisCoordinator::new);
pub const SOURCE_UNAVAILABLE_ERROR_PREFIX: &str = "Source file unavailable:";

const FAST_LUFS_FULL_SCAN_THRESHOLD_MS: u64 = 12_000;
const FAST_LUFS_MEDIUM_TRACK_THRESHOLD_MS: u64 = 90_000;
const FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM: u32 = 8_000;
const FAST_LUFS_PREVIEW_TOTAL_MS_LONG: u32 = 12_000;
const FAST_LUFS_ESTIMATED_CONFIDENCE: f32 = 0.75;
const FAST_LUFS_REFINED_CONFIDENCE: f32 = 1.0;
const FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD: f32 = 0.80;
const FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN: usize = 10;

pub type LoudnessAnalysisCompletion = Box<dyn FnOnce(Result<u32, String>) + Send + 'static>;

static FIRST_PLAYBACK_PHASE_RECORDED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingLoudnessAnalysisTrigger {
    Started,
    SkippedAutoGainDisabled,
    SkippedNoMissingSounds,
    SkippedAlreadyRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimatedLoudnessRefinementTrigger {
    Started,
    SkippedAutoGainDisabled,
    SkippedNoCandidates,
    SkippedAlreadyRunning,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LoudnessStatusSummary {
    pub total_sounds: usize,
    pub pending_count: usize,
    pub estimated_count: usize,
    pub refined_count: usize,
    pub unavailable_count: usize,
    pub missing_loudness_count: usize,
    pub in_flight_backfill: bool,
    pub in_flight_refinement: bool,
}

fn missing_loudness_analysis_trigger(
    auto_gain_enabled: bool,
    has_missing_loudness: bool,
    force: bool,
    in_flight: bool,
) -> MissingLoudnessAnalysisTrigger {
    if !force && !auto_gain_enabled {
        return MissingLoudnessAnalysisTrigger::SkippedAutoGainDisabled;
    }
    if !has_missing_loudness {
        return MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds;
    }
    if in_flight {
        return MissingLoudnessAnalysisTrigger::SkippedAlreadyRunning;
    }
    MissingLoudnessAnalysisTrigger::Started
}

fn estimated_loudness_refinement_trigger(
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

fn maybe_trigger_estimated_loudness_refinement(
    config: Arc<Mutex<Config>>,
    force: bool,
) -> Result<EstimatedLoudnessRefinementTrigger, String> {
    let trigger = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        estimated_loudness_refinement_trigger(
            cfg.settings.auto_gain,
            cfg.sounds
                .iter()
                .any(|sound| sound_needs_loudness_refinement(sound, force)),
            force,
            ESTIMATED_LOUDNESS_REFINEMENT_COORDINATOR.is_in_flight(),
        )
    };

    if trigger != EstimatedLoudnessRefinementTrigger::Started {
        return Ok(trigger);
    }

    let started = ESTIMATED_LOUDNESS_REFINEMENT_COORDINATOR.try_start(
        "loudness-refinement",
        move || refine_estimated_loudness(config, force),
        None,
    )?;

    if !started {
        return Ok(EstimatedLoudnessRefinementTrigger::SkippedAlreadyRunning);
    }

    Ok(EstimatedLoudnessRefinementTrigger::Started)
}

pub fn trigger_estimated_loudness_refinement(
    config: Arc<Mutex<Config>>,
    force: bool,
) -> Result<EstimatedLoudnessRefinementTrigger, String> {
    maybe_trigger_estimated_loudness_refinement(config, force)
}

pub fn get_loudness_status_summary(config: Arc<Mutex<Config>>) -> Result<LoudnessStatusSummary, String> {
    let cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;

    let mut summary = LoudnessStatusSummary {
        total_sounds: cfg.sounds.len(),
        pending_count: 0,
        estimated_count: 0,
        refined_count: 0,
        unavailable_count: 0,
        missing_loudness_count: 0,
        in_flight_backfill: MISSING_LOUDNESS_ANALYSIS_COORDINATOR.is_in_flight(),
        in_flight_refinement: ESTIMATED_LOUDNESS_REFINEMENT_COORDINATOR.is_in_flight(),
    };

    for sound in &cfg.sounds {
        if sound.loudness_lufs.is_none() {
            summary.missing_loudness_count += 1;
        }
        match sound.loudness_analysis_state {
            LoudnessAnalysisState::Pending => summary.pending_count += 1,
            LoudnessAnalysisState::Estimated => summary.estimated_count += 1,
            LoudnessAnalysisState::Refined => summary.refined_count += 1,
            LoudnessAnalysisState::Unavailable => summary.unavailable_count += 1,
        }
    }

    Ok(summary)
}

pub fn trigger_missing_loudness_analysis(
    config: Arc<Mutex<Config>>,
    force: bool,
    on_complete: Option<LoudnessAnalysisCompletion>,
) -> Result<MissingLoudnessAnalysisTrigger, String> {
    let trigger = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        missing_loudness_analysis_trigger(
            cfg.settings.auto_gain,
            cfg.sounds.iter().any(sound_needs_loudness_backfill),
            force,
            MISSING_LOUDNESS_ANALYSIS_COORDINATOR.is_in_flight(),
        )
    };

    if trigger != MissingLoudnessAnalysisTrigger::Started {
        if trigger == MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds {
            if let Err(err) = maybe_trigger_estimated_loudness_refinement(Arc::clone(&config), force)
            {
                log::warn!("Failed to schedule estimated loudness refinement: {}", err);
            }
        }
        return Ok(trigger);
    }

    let started = MISSING_LOUDNESS_ANALYSIS_COORDINATOR.try_start(
        "loudness-backfill",
        move || analyze_all_loudness(config),
        on_complete,
    )?;

    if !started {
        return Ok(MissingLoudnessAnalysisTrigger::SkippedAlreadyRunning);
    }

    Ok(MissingLoudnessAnalysisTrigger::Started)
}

#[cfg(test)]
pub(super) fn reset_missing_loudness_analysis_test_state() {
    MISSING_LOUDNESS_ANALYSIS_COORDINATOR.reset_test_state();
}

#[cfg(test)]
pub(super) fn missing_loudness_analysis_start_count() -> usize {
    MISSING_LOUDNESS_ANALYSIS_COORDINATOR.start_count()
}

#[cfg(test)]
pub(super) fn wait_for_missing_loudness_analysis_to_finish(timeout: std::time::Duration) -> bool {
    MISSING_LOUDNESS_ANALYSIS_COORDINATOR.wait_for_idle(timeout)
}

#[cfg(test)]
pub(super) fn reset_estimated_loudness_refinement_test_state() {
    ESTIMATED_LOUDNESS_REFINEMENT_COORDINATOR.reset_test_state();
}

#[cfg(test)]
pub(super) fn estimated_loudness_refinement_start_count() -> usize {
    ESTIMATED_LOUDNESS_REFINEMENT_COORDINATOR.start_count()
}

#[cfg(test)]
pub(super) fn wait_for_estimated_loudness_refinement_to_finish(
    timeout: std::time::Duration,
) -> bool {
    ESTIMATED_LOUDNESS_REFINEMENT_COORDINATOR.wait_for_idle(timeout)
}

pub fn list_sounds(config: Arc<Mutex<Config>>) -> Vec<Sound> {
    config
        .lock()
        .map(|cfg| cfg.sounds.clone())
        .unwrap_or_else(|_e| {
            log::warn!("Config lock poisoned in list_sounds, returning empty");
            Vec::new()
        })
}

#[allow(clippy::unnecessary_mut_passed)]
pub fn play_sound(
    id: String,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<String, String> {
    let sound = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        cfg.get_sound(&id)
            .cloned()
            .ok_or_else(|| "Sound not found".to_string())?
    };

    if !sound.enabled {
        return Err("Sound is disabled".to_string());
    }

    let source_path = sound.source_path.as_deref().unwrap_or(&sound.path);
    if !crate::audio::file_link::check_file_exists(source_path) {
        return Err(format!(
            "{} {}",
            SOURCE_UNAVAILABLE_ERROR_PREFIX, source_path
        ));
    }

    player.stop_all();

    let base_volume = sound.volume as f32 / 100.0;
    let sound_lufs = sound.loudness_lufs;
    let result = player.play(&id, source_path, base_volume, sound_lufs);
    if let Err(err) = &result {
        log::error!(
            "play_sound failed: id='{}' path='{}' base_volume={:.3} err={}",
            id,
            source_path,
            base_volume,
            err
        );
        crate::diagnostics::memory::log_memory_snapshot("audio_cmd:play:command_error");
    } else if FIRST_PLAYBACK_PHASE_RECORDED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        crate::diagnostics::set_playback_registry_count(player.get_playback_positions().len());
        if let Ok(cfg) = config.lock() {
            crate::diagnostics::record_phase_with_config("playback:first_play_start", &cfg);
        } else {
            crate::diagnostics::record_phase("playback:first_play_start", None);
        }
    } else {
        crate::diagnostics::set_playback_registry_count(player.get_playback_positions().len());
    }
    result
}

pub fn play_sound_async<F>(
    id: String,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
    on_complete: F,
) -> Result<(), String>
where
    F: FnOnce(Result<String, String>) + 'static,
{
    dispatch_async_result(
        "play_sound",
        move || play_sound(id, config, player),
        on_complete,
    )
}

pub fn set_allow_multiple_playbacks(allow: bool, config: Arc<Mutex<Config>>) -> Result<(), String> {
    if allow {
        log::info!("Ignoring request to enable multiple simultaneous playbacks");
    }
    with_saved_config(&config, |cfg| {
        cfg.settings.allow_multiple_playbacks = false;
    })
}

pub fn set_skip_delete_confirm(skip: bool, config: Arc<Mutex<Config>>) -> Result<(), String> {
    with_saved_config(&config, |cfg| {
        cfg.settings.skip_delete_confirm = skip;
    })
}

fn save_config_and_notify_player<F, G>(
    config: &Arc<Mutex<Config>>,
    player: &Arc<AudioPlayer>,
    save_update: F,
    notify_player: G,
) -> Result<(), String>
where
    F: FnOnce(&mut Config),
    G: FnOnce(&AudioPlayer),
{
    with_saved_config(config, save_update)?;
    notify_player(player.as_ref());
    Ok(())
}

pub fn set_auto_gain(
    enabled: bool,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    save_config_and_notify_player(
        &config,
        &player,
        |cfg| {
            cfg.settings.auto_gain = enabled;
        },
        |player| player.set_auto_gain_enabled(enabled),
    )?;
    if enabled {
        trigger_missing_loudness_analysis(Arc::clone(&config), false, None).map(|_| ())?;
    }
    Ok(())
}

pub fn set_auto_gain_target(
    target_lufs: f64,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    let clamped = target_lufs.clamp(-24.0, 0.0);
    save_config_and_notify_player(
        &config,
        &player,
        |cfg| {
            cfg.settings.auto_gain_target_lufs = clamped;
        },
        |player| player.set_auto_gain_target(clamped),
    )
}

pub fn set_auto_gain_mode(
    mode: String,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    let mode = parse_auto_gain_mode(&mode)?;
    let player_mode = mode.player_value();
    save_config_and_notify_player(
        &config,
        &player,
        |cfg| {
            cfg.settings.auto_gain_mode = mode;
        },
        |player| player.set_auto_gain_mode(player_mode),
    )
}

pub fn set_auto_gain_apply_to(
    scope: String,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    let scope = parse_auto_gain_apply_to(&scope)?;
    let player_scope = scope.player_value();
    save_config_and_notify_player(
        &config,
        &player,
        |cfg| {
            cfg.settings.auto_gain_apply_to = scope;
        },
        |player| player.set_auto_gain_apply_to(player_scope),
    )
}

pub fn set_auto_gain_dynamic_settings(
    lookahead_ms: u32,
    attack_ms: u32,
    release_ms: u32,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    let lookahead_ms = lookahead_ms.clamp(5, 200);
    let attack_ms = attack_ms.clamp(1, 50);
    let release_ms = release_ms.clamp(50, 1000);

    save_config_and_notify_player(
        &config,
        &player,
        |cfg| {
            cfg.settings.auto_gain_lookahead_ms = lookahead_ms;
            cfg.settings.auto_gain_attack_ms = attack_ms;
            cfg.settings.auto_gain_release_ms = release_ms;
        },
        |player| player.set_auto_gain_dynamic_settings(lookahead_ms, attack_ms, release_ms),
    )
}

pub fn set_play_mode(
    mode: String,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    let mode = validate_play_mode(&mode)?;
    let should_loop = mode.should_loop();
    save_config_and_notify_player(
        &config,
        &player,
        |cfg| {
            cfg.settings.play_mode = mode;
        },
        |player| player.set_looping(should_loop),
    )
}

fn fast_loudness_preview_budget_ms(duration_hint_ms: Option<u64>) -> u32 {
    match duration_hint_ms {
        Some(duration_ms) if duration_ms > FAST_LUFS_MEDIUM_TRACK_THRESHOLD_MS => {
            FAST_LUFS_PREVIEW_TOTAL_MS_LONG
        }
        _ => FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM,
    }
}

fn sound_needs_loudness_backfill(sound: &Sound) -> bool {
    sound.loudness_lufs.is_none() && sound.loudness_analysis_state != LoudnessAnalysisState::Unavailable
}

fn sound_needs_loudness_refinement(sound: &Sound, force: bool) -> bool {
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

fn should_mark_unavailable_loudness_error(err: &str) -> bool {
    err.contains("No audio frames decoded")
        || err.contains("No audio tracks found")
        || err.contains("Failed to create decoder")
}

fn analyze_loudness_for_backfill(
    path: &Path,
    duration_hint_ms: Option<u64>,
) -> Result<(f64, LoudnessAnalysisState, Option<f32>), String> {
    if duration_hint_ms.is_some_and(|duration_ms| duration_ms <= FAST_LUFS_FULL_SCAN_THRESHOLD_MS) {
        return loudness::analyze_loudness_path(path).map(|lufs| {
            (
                lufs,
                LoudnessAnalysisState::Refined,
                Some(FAST_LUFS_REFINED_CONFIDENCE),
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
            ))
        }
        Err(err) => {
            log::debug!(
                "Fast loudness preview failed for '{}' ({}); falling back to full analysis",
                path.display(),
                err
            );
            loudness::analyze_loudness_path(path).map(|lufs| {
                (
                    lufs,
                    LoudnessAnalysisState::Refined,
                    Some(FAST_LUFS_REFINED_CONFIDENCE),
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
    },
    Unavailable {
        id: String,
    },
}

enum RefinementOutcome {
    Refined {
        id: String,
        lufs: f64,
    },
    Deferred {
        id: String,
        backoff_confidence: f32,
    },
}

#[derive(Debug, Clone)]
struct RefinementCandidate {
    id: String,
    path: String,
    confidence: f32,
    duration_ms: u64,
}

fn normalized_loudness_confidence(confidence: Option<f32>) -> f32 {
    match confidence {
        Some(value) if value.is_finite() => value.clamp(0.0, 1.0),
        _ => 0.0,
    }
}

fn collect_refinement_candidates(config: &Config, force: bool) -> Vec<RefinementCandidate> {
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

fn refine_estimated_loudness(config: Arc<Mutex<Config>>, force: bool) -> Result<u32, String> {
    crate::diagnostics::memory::log_memory_snapshot("refine_estimated_loudness:start");

    let candidates = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        collect_refinement_candidates(&cfg, force)
    };

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

        if !Path::new(&path).exists() {
            outcomes.push(RefinementOutcome::Deferred {
                id,
                backoff_confidence: FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD + 0.05,
            });
            continue;
        }

        match loudness::analyze_loudness_path(Path::new(&path)) {
            Ok(lufs) if lufs.is_finite() => {
                outcomes.push(RefinementOutcome::Refined { id, lufs });
            }
            Ok(lufs) => {
                log::warn!(
                    "Deferring refinement after non-finite result for '{}': {}",
                    path,
                    lufs
                );
                outcomes.push(RefinementOutcome::Deferred {
                    id,
                    backoff_confidence: FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD + 0.05,
                });
            }
            Err(err) => {
                if should_mark_unavailable_loudness_error(&err) {
                    log::warn!(
                        "Deferring refinement after terminal decode error for '{}': {}",
                        path,
                        err
                    );
                    outcomes.push(RefinementOutcome::Deferred {
                        id,
                        backoff_confidence: FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD + 0.05,
                    });
                } else {
                    log::warn!("Failed to refine loudness for '{}': {}", path, err);
                    outcomes.push(RefinementOutcome::Deferred {
                        id,
                        backoff_confidence: FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD + 0.05,
                    });
                }
            }
        }
    }

    let refined_count = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, RefinementOutcome::Refined { .. }))
        .count() as u32;
    if !outcomes.is_empty() {
        let mut cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        for outcome in outcomes {
            match outcome {
                RefinementOutcome::Refined { id, lufs } => {
                    if let Some(sound) = cfg.sounds.iter_mut().find(|sound| sound.id == id) {
                        sound.loudness_lufs = Some(lufs);
                        sound.loudness_analysis_state = LoudnessAnalysisState::Refined;
                        sound.loudness_confidence = Some(FAST_LUFS_REFINED_CONFIDENCE);
                    }
                }
                RefinementOutcome::Deferred {
                    id,
                    backoff_confidence,
                } => {
                    if let Some(sound) = cfg.sounds.iter_mut().find(|sound| sound.id == id) {
                        let current_confidence = sound.loudness_confidence.unwrap_or(0.0);
                        sound.loudness_confidence = Some(
                            current_confidence
                                .max(backoff_confidence)
                                .clamp(0.0, 1.0),
                        );
                    }
                }
            }
        }
        cfg.save().map_err(|e| e.to_string())?;
    }

    crate::diagnostics::memory::log_memory_snapshot("refine_estimated_loudness:end");
    Ok(refined_count)
}

pub fn analyze_all_loudness(config: Arc<Mutex<Config>>) -> Result<u32, String> {
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:start");
    let sounds_to_analyze: Vec<(String, String, Option<u64>)> = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        cfg.sounds
            .iter()
            .filter(|s| sound_needs_loudness_backfill(s))
            .map(|s| (s.id.clone(), s.path.clone(), s.duration_ms))
            .collect()
    };
    if let Ok(cfg) = config.lock() {
        crate::diagnostics::record_phase_with_config("analyze_all_loudness:start", &cfg);
    } else {
        crate::diagnostics::record_phase("analyze_all_loudness:start", None);
    }

    log::info!("Analyzing loudness for {} sounds", sounds_to_analyze.len());

    loudness::reset_loudness_analysis_cancelled();

    let analyze_entry =
        |(id, path, duration_hint_ms): &(String, String, Option<u64>)| -> Option<BackfillOutcome> {
        if loudness::is_loudness_analysis_cancelled() {
            return None;
        }
        if !Path::new(path).exists() {
            return None;
        }
        match analyze_loudness_for_backfill(Path::new(path), *duration_hint_ms) {
            Ok((lufs, state, confidence)) if lufs.is_finite() => Some(BackfillOutcome::Analyzed {
                id: id.clone(),
                lufs,
                state,
                confidence,
            }),
            Ok((lufs, _, _)) => {
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
    if let Ok(cfg) = config.lock() {
        crate::diagnostics::record_phase_with_config("analyze_all_loudness:before_pool", &cfg);
    } else {
        crate::diagnostics::record_phase("analyze_all_loudness:before_pool", None);
    }
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
                    .collect::<Vec<_>>()
            }
        }
    };
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:after_pool");
    if let Ok(cfg) = config.lock() {
        crate::diagnostics::record_phase_with_config("analyze_all_loudness:after_pool", &cfg);
    } else {
        crate::diagnostics::record_phase("analyze_all_loudness:after_pool", None);
    }

    let has_updates = !results.is_empty();
    let analyzed_count = results
        .iter()
        .filter(|result| matches!(result, BackfillOutcome::Analyzed { .. }))
        .count() as u32;
    if has_updates {
        let mut cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        for result in results {
            match result {
                BackfillOutcome::Analyzed {
                    id,
                    lufs,
                    state,
                    confidence,
                } => {
                    if let Some(sound) = cfg.sounds.iter_mut().find(|s| s.id == id) {
                        sound.loudness_lufs = Some(lufs);
                        sound.loudness_analysis_state = state;
                        sound.loudness_confidence = confidence;
                    }
                }
                BackfillOutcome::Unavailable { id } => {
                    if let Some(sound) = cfg.sounds.iter_mut().find(|s| s.id == id) {
                        sound.loudness_lufs = None;
                        sound.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
                        sound.loudness_confidence = None;
                    }
                }
            }
        }
        cfg.save().map_err(|e| e.to_string())?;
        crate::diagnostics::record_phase_with_config("playback:loudness_analysis_complete", &cfg);
    } else if let Ok(cfg) = config.lock() {
        crate::diagnostics::record_phase_with_config("playback:loudness_analysis_complete", &cfg);
    } else {
        crate::diagnostics::record_phase("playback:loudness_analysis_complete", None);
    }

    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:end");
    crate::diagnostics::clear_work_runtime();
    if let Err(err) = maybe_trigger_estimated_loudness_refinement(Arc::clone(&config), false) {
        log::warn!(
            "Failed to schedule estimated loudness refinement after fast backfill: {}",
            err
        );
    }
    Ok(analyzed_count)
}

/// Cancel loudness backfill.
pub fn cancel_loudness_analysis() {
    loudness::cancel_loudness_analysis();
}

pub fn analyze_sound_loudness(
    id: String,
    config: Arc<Mutex<Config>>,
) -> Result<Option<f64>, String> {
    let path = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        let sound = cfg
            .sounds
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| "Sound not found".to_string())?;
        sound.path.clone()
    };
    let lufs = loudness::analyze_loudness_path(Path::new(&path))?;
    let (lufs, state, confidence) = if lufs.is_finite() {
        (
            Some(lufs),
            LoudnessAnalysisState::Refined,
            Some(FAST_LUFS_REFINED_CONFIDENCE),
        )
    } else {
        log::warn!(
            "Marking sound as unavailable due to non-finite loudness result for '{}': {}",
            path,
            lufs
        );
        (None, LoudnessAnalysisState::Unavailable, None)
    };
    let mut cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    if let Some(sound) = cfg.sounds.iter_mut().find(|s| s.id == id) {
        sound.loudness_lufs = lufs;
        sound.loudness_analysis_state = state;
        sound.loudness_confidence = confidence;
    }
    cfg.save().map_err(|e| e.to_string())?;
    Ok(lufs)
}

pub fn analyze_sound_loudness_async<F>(
    id: String,
    config: Arc<Mutex<Config>>,
    on_complete: F,
) -> Result<(), String>
where
    F: FnOnce(Result<Option<f64>, String>) + 'static,
{
    dispatch_async_result(
        "analyze_sound_loudness",
        move || analyze_sound_loudness(id, config),
        on_complete,
    )
}

pub fn stop_sound(id: String, player: Arc<AudioPlayer>) -> Result<(), String> {
    player.stop_sound(&id)
}

pub fn stop_all(player: Arc<AudioPlayer>) {
    player.stop_all();
    crate::diagnostics::set_playback_registry_count(0);
    crate::diagnostics::record_phase("playback:stop_all_idle", None);
}

pub fn seek_playback(
    play_id: String,
    position_ms: u64,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    if position_ms > 24 * 60 * 60 * 1000 {
        return Err("Seek position too large (max 24 hours)".to_string());
    }
    log::debug!(
        "Dispatching seek request: play_id={}, position_ms={}",
        play_id,
        position_ms
    );
    player.seek_playback(&play_id, position_ms);
    Ok(())
}

pub fn seek_sound(id: String, position_ms: u64, player: Arc<AudioPlayer>) -> Result<(), String> {
    if position_ms > 24 * 60 * 60 * 1000 {
        return Err("Seek position too large (max 24 hours)".to_string());
    }
    let play_id = player
        .get_playback_positions()
        .into_iter()
        .find(|position| !position.finished && position.sound_id == id)
        .map(|position| position.play_id);
    if let Some(play_id) = play_id {
        log::debug!(
            "Dispatching legacy seek request: sound_id={}, play_id={}, position_ms={}",
            id,
            play_id,
            position_ms
        );
        player.seek_playback(&play_id, position_ms);
    } else {
        log::warn!(
            "Ignoring legacy seek request for inactive sound_id={}, position_ms={}",
            id,
            position_ms
        );
    }
    Ok(())
}

pub fn pause_sound(id: String, player: Arc<AudioPlayer>) {
    player.pause(&id);
}

pub fn resume_sound(id: String, player: Arc<AudioPlayer>) {
    player.resume(&id);
}

pub fn get_audio_status(player: Arc<AudioPlayer>) -> AudioStatus {
    let playing = player.get_playing();
    let mut positions: HashMap<String, u64> = HashMap::new();
    for p in player.get_playback_positions() {
        if !p.finished {
            positions.entry(p.sound_id.clone()).or_insert(p.position_ms);
        }
    }
    AudioStatus { playing, positions }
}

pub fn get_playback_positions(player: Arc<AudioPlayer>) -> Vec<PlaybackPosition> {
    player.get_playback_positions()
}

#[derive(serde::Serialize)]
pub struct AudioStatus {
    pub playing: Vec<String>,
    pub positions: HashMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::{
        collect_refinement_candidates,
        estimated_loudness_refinement_trigger, fast_loudness_preview_budget_ms,
        get_loudness_status_summary,
        missing_loudness_analysis_trigger, sound_needs_loudness_backfill,
        sound_needs_loudness_refinement, EstimatedLoudnessRefinementTrigger,
        FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN,
        FAST_LUFS_PREVIEW_TOTAL_MS_LONG, FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM,
        FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD, MissingLoudnessAnalysisTrigger,
    };
    use crate::config::{Config, LoudnessAnalysisState, Sound};
    use std::sync::{Arc, Mutex};

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
        let mut cfg = Config::default();

        cfg.sounds = (0..(FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN + 4))
            .map(|idx| {
                let mut sound =
                    Sound::new(format!("sound-{idx}"), format!("/tmp/sound-{idx}.wav"));
                sound.id = format!("sound-{idx:03}");
                sound.loudness_lufs = Some(-14.0);
                sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
                sound.loudness_confidence = Some((idx as f32 / 100.0).clamp(0.0, 1.0));
                sound.duration_ms = Some((idx as u64 + 1) * 1_000);
                sound
            })
            .collect();

        let candidates = collect_refinement_candidates(&cfg, false);

        assert_eq!(candidates.len(), FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN);
        assert_eq!(candidates[0].id, "sound-000");
    }

    #[test]
    fn collect_refinement_candidates_force_mode_bypasses_run_budget() {
        let mut cfg = Config::default();

        cfg.sounds = (0..(FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN + 4))
            .map(|idx| {
                let mut sound =
                    Sound::new(format!("sound-{idx}"), format!("/tmp/sound-{idx}.wav"));
                sound.id = format!("sound-{idx:03}");
                sound.loudness_lufs = Some(-14.0);
                sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
                sound.loudness_confidence = Some(0.1);
                sound
            })
            .collect();

        let candidates = collect_refinement_candidates(&cfg, true);

        assert_eq!(candidates.len(), FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN + 4);
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

        let mut unavailable =
            Sound::new("unavailable".to_string(), "/tmp/unavailable.wav".to_string());
        unavailable.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
        unavailable.loudness_lufs = None;

        cfg.sounds = vec![pending, estimated, refined, unavailable];

        let summary =
            get_loudness_status_summary(Arc::new(Mutex::new(cfg))).expect("status summary");

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
}
