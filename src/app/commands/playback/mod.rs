use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{atomic::Ordering, Arc};
use std::time::Instant;

use crate::app_state::AppState;
use crate::audio::PlaybackEngine;
use crate::audio::{loudness as audio_loudness, AudioPlayer, PlaybackPosition};
use crate::config::{Config, LoudnessAnalysisState, Sound};

use super::shared::{
    dispatch_async_result, parse_auto_gain_apply_to, parse_auto_gain_mode, validate_play_mode,
    with_config, with_config_mut, with_saved_config,
};
use super::CommandError;

mod loudness;
pub use loudness::{analyze_all_loudness, analyze_all_loudness_with_store};

// Bring internal helpers into this module's namespace so the sibling test
// module can import them via `super::` without widening their visibility
// beyond this crate.
#[cfg(test)]
use loudness::{
    collect_refinement_candidates, estimated_loudness_refinement_trigger,
    fast_loudness_preview_budget_ms, sound_needs_loudness_backfill,
    sound_needs_loudness_refinement,
};

const FAST_LUFS_FULL_SCAN_THRESHOLD_MS: u64 = 12_000;
const FAST_LUFS_MEDIUM_TRACK_THRESHOLD_MS: u64 = 90_000;
const FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM: u32 = 8_000;
const FAST_LUFS_PREVIEW_TOTAL_MS_LONG: u32 = 12_000;
const FAST_LUFS_ESTIMATED_CONFIDENCE: f32 = 0.75;
const FAST_LUFS_REFINED_CONFIDENCE: f32 = 1.0;
const FAST_LUFS_REFINEMENT_CONFIDENCE_THRESHOLD: f32 = 0.80;
const FAST_LUFS_REFINEMENT_MAX_SOUNDS_PER_RUN: usize = 10;

pub type LoudnessAnalysisCompletion =
    Box<dyn FnOnce(Result<u32, crate::audio::LoudnessError>) + Send + 'static>;

/// Per-instance loudness analysis coordinators. Add one to `AppState` at
/// startup so that each background analysis job can be tracked, observed, and
/// cancelled without process-global state.
#[derive(Clone)]
pub struct LoudnessCoordinators {
    pub(crate) backfill: Arc<crate::audio::analysis_worker::MissingLoudnessAnalysisCoordinator>,
    pub(crate) refinement: Arc<crate::audio::analysis_worker::MissingLoudnessAnalysisCoordinator>,
}

impl LoudnessCoordinators {
    pub fn new() -> Self {
        Self {
            backfill: Arc::new(
                crate::audio::analysis_worker::MissingLoudnessAnalysisCoordinator::new(),
            ),
            refinement: Arc::new(
                crate::audio::analysis_worker::MissingLoudnessAnalysisCoordinator::new(),
            ),
        }
    }
}

impl Default for LoudnessCoordinators {
    fn default() -> Self {
        Self::new()
    }
}

/// Drop a same-sound play request if one was just dispatched within this window.
/// Hotkey auto-repeat fires at ~30–50ms; mashing a single sound at button-press rate
/// can't be heard as more than one playback anyway because each play resets the
/// previous via stop_all. Distinct sounds are unaffected.
const SAME_SOUND_DEBOUNCE_MS: u128 = 30;

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

pub fn trigger_estimated_loudness_refinement(
    config: Arc<Mutex<Config>>,
    force: bool,
    coords: &LoudnessCoordinators,
) -> Result<EstimatedLoudnessRefinementTrigger, CommandError> {
    loudness::maybe_trigger_estimated_loudness_refinement(config, force, coords)
}

pub fn trigger_estimated_loudness_refinement_with_store(
    config: Arc<Mutex<Config>>,
    library: crate::library_store::LibraryStore,
    force: bool,
    coords: &LoudnessCoordinators,
) -> Result<EstimatedLoudnessRefinementTrigger, CommandError> {
    let auto_gain = config.lock().settings.auto_gain;
    loudness::maybe_trigger_estimated_loudness_refinement_with_store(
        auto_gain, library, force, coords,
    )
}

pub fn get_loudness_status_summary(
    config: Arc<Mutex<Config>>,
    coords: &LoudnessCoordinators,
) -> Result<LoudnessStatusSummary, CommandError> {
    with_config(&config, |cfg| {
        let mut summary = LoudnessStatusSummary {
            total_sounds: cfg.sounds.len(),
            pending_count: 0,
            estimated_count: 0,
            refined_count: 0,
            unavailable_count: 0,
            missing_loudness_count: 0,
            in_flight_backfill: coords.backfill.is_in_flight(),
            in_flight_refinement: coords.refinement.is_in_flight(),
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

        summary
    })
}

pub fn get_loudness_status_summary_with_store(
    library: crate::library_store::LibraryStore,
    coords: &LoudnessCoordinators,
) -> Result<LoudnessStatusSummary, CommandError> {
    let stats = library
        .loudness_stats()
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    Ok(LoudnessStatusSummary {
        total_sounds: stats.total,
        pending_count: stats.pending,
        estimated_count: stats.estimated,
        refined_count: stats.refined,
        unavailable_count: stats.unavailable,
        missing_loudness_count: stats.missing,
        in_flight_backfill: coords.backfill.is_in_flight(),
        in_flight_refinement: coords.refinement.is_in_flight(),
    })
}

pub fn trigger_missing_loudness_analysis(
    config: Arc<Mutex<Config>>,
    force: bool,
    on_complete: Option<LoudnessAnalysisCompletion>,
    coords: &LoudnessCoordinators,
) -> Result<MissingLoudnessAnalysisTrigger, CommandError> {
    let trigger = with_config(&config, |cfg| {
        missing_loudness_analysis_trigger(
            cfg.settings.auto_gain,
            cfg.sounds
                .iter()
                .any(loudness::sound_needs_loudness_backfill),
            force,
            coords.backfill.is_in_flight(),
        )
    })?;

    if trigger != MissingLoudnessAnalysisTrigger::Started {
        if trigger == MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds {
            if let Err(err) = loudness::maybe_trigger_estimated_loudness_refinement(
                Arc::clone(&config),
                force,
                coords,
            ) {
                log::warn!("Failed to schedule estimated loudness refinement: {}", err);
            }
        }
        return Ok(trigger);
    }

    let completion: Option<LoudnessAnalysisCompletion> = Some(Box::new(move |result| {
        if let Some(on_complete) = on_complete {
            on_complete(result);
        }
        crate::ui_event_bridge::post_loudness_status_refresh();
    }));

    let coords_clone = coords.clone();
    let started = coords
        .backfill
        .try_start(
            "loudness-backfill",
            move || loudness::analyze_all_loudness(config, coords_clone),
            completion,
        )
        .map_err(|e| CommandError::Analysis(e.to_string()))?;

    if !started {
        return Ok(MissingLoudnessAnalysisTrigger::SkippedAlreadyRunning);
    }

    Ok(MissingLoudnessAnalysisTrigger::Started)
}

pub fn trigger_missing_loudness_analysis_with_store(
    config: Arc<Mutex<Config>>,
    library: crate::library_store::LibraryStore,
    force: bool,
    on_complete: Option<LoudnessAnalysisCompletion>,
    coords: &LoudnessCoordinators,
) -> Result<MissingLoudnessAnalysisTrigger, CommandError> {
    let auto_gain = config.lock().settings.auto_gain;
    let stats = library
        .loudness_stats()
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    let trigger = missing_loudness_analysis_trigger(
        auto_gain,
        stats.missing > 0,
        force,
        coords.backfill.is_in_flight(),
    );
    if trigger != MissingLoudnessAnalysisTrigger::Started {
        if trigger == MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds {
            let _ = loudness::maybe_trigger_estimated_loudness_refinement_with_store(
                auto_gain, library, force, coords,
            );
        }
        return Ok(trigger);
    }
    let completion: Option<LoudnessAnalysisCompletion> = Some(Box::new(move |result| {
        if let Some(on_complete) = on_complete {
            on_complete(result);
        }
        crate::ui_event_bridge::post_loudness_status_refresh();
    }));
    let coords_clone = coords.clone();
    let started = coords
        .backfill
        .try_start(
            "loudness-backfill",
            move || loudness::analyze_all_loudness_with_store(library, auto_gain, coords_clone),
            completion,
        )
        .map_err(|error| CommandError::Analysis(error.to_string()))?;
    Ok(if started {
        trigger
    } else {
        MissingLoudnessAnalysisTrigger::SkippedAlreadyRunning
    })
}

pub fn list_sounds(config: Arc<Mutex<Config>>) -> Vec<Sound> {
    config.lock().sounds.clone()
}

#[allow(clippy::unnecessary_mut_passed)]
pub fn play_sound(
    id: String,
    config: Arc<Mutex<Config>>,
    player: Arc<dyn PlaybackEngine>,
) -> Result<String, CommandError> {
    let sound = with_config(&config, |cfg| {
        cfg.get_sound(&id)
            .cloned()
            .ok_or(CommandError::SoundNotFound)
    })??;

    play_resolved_sound(sound, player)
}

fn play_resolved_sound(
    sound: Sound,
    player: Arc<dyn PlaybackEngine>,
) -> Result<String, CommandError> {
    if !sound.enabled {
        return Err(CommandError::SoundDisabled);
    }

    let source_path = sound.source_path.as_deref().unwrap_or(&sound.path);
    if !crate::audio::file_link::check_file_exists(source_path) {
        return Err(CommandError::SourceUnavailable(source_path.to_string()));
    }

    player.stop_all();

    let base_volume = sound.volume as f32 / 100.0;
    let sound_lufs = sound.loudness_lufs;
    let sound_true_peak_dbtp = sound.loudness_true_peak_dbtp;
    let id = sound.id;
    let result = player
        .play(
            &id,
            source_path,
            base_volume,
            sound_lufs,
            sound_true_peak_dbtp,
        )
        .map_err(|e| CommandError::Engine(e.to_string()));
    if let Err(err) = &result {
        log::error!(
            "play_sound failed: id='{}' path='{}' base_volume={:.3} err={}",
            id,
            source_path,
            base_volume,
            err
        );
        crate::diagnostics::memory::log_memory_snapshot("audio_cmd:play:command_error");
    }
    result
}

fn play_sound_from_library(
    id: &str,
    binding_lookup: bool,
    library: &crate::library_store::LibraryStore,
    player: Arc<dyn PlaybackEngine>,
) -> Result<String, CommandError> {
    let sound = if binding_lookup {
        library.sound_for_binding(id)
    } else {
        library.sound_by_id(id)
    }
    .recv()
    .map_err(|error| CommandError::Library(error.to_string()))?
    .ok_or(CommandError::SoundNotFound)?;
    play_resolved_sound(sound, player)
}

fn play_adjacent_from_library(
    scope: crate::library_store::LibraryScope,
    search: &str,
    position: Option<usize>,
    offset: i32,
    library: &crate::library_store::LibraryStore,
    player: Arc<dyn PlaybackEngine>,
) -> Result<String, CommandError> {
    let total = library
        .count(scope.clone(), search)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    if total == 0 {
        return Err(CommandError::SoundNotFound);
    }
    let mut sound = match position {
        Some(position) => library
            .adjacent(scope.clone(), search, position, offset)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?,
        None => None,
    };
    if sound.is_none() {
        let boundary = if offset < 0 { total - 1 } else { 0 };
        sound = library
            .adjacent(scope, search, boundary, 0)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
    }
    play_resolved_sound(sound.ok_or(CommandError::SoundNotFound)?, player)
}

pub fn play_sound_async<F>(
    id: String,
    state: Arc<AppState>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<String, CommandError>) + 'static,
{
    dispatch_play_sound_async(id, None, state, false, on_complete)
}

pub fn play_loaded_sound_async<F>(
    sound: Sound,
    state: Arc<AppState>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<String, CommandError>) + 'static,
{
    dispatch_play_sound_async(sound.id.clone(), Some(sound), state, false, on_complete)
}

pub fn play_hotkey_sound_async<F>(
    binding_id: String,
    state: Arc<AppState>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<String, CommandError>) + 'static,
{
    dispatch_play_sound_async(binding_id, None, state, true, on_complete)
}

pub fn play_adjacent_sound_async<F>(
    scope: crate::library_store::LibraryScope,
    search: String,
    position: Option<usize>,
    offset: i32,
    state: Arc<AppState>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<String, CommandError>) + 'static,
{
    let library = state.library.clone();
    let player = Arc::clone(&state.player);
    dispatch_async_result(
        "play_adjacent_sound",
        move || play_adjacent_from_library(scope, &search, position, offset, &library, player),
        on_complete,
    )
}

fn dispatch_play_sound_async<F>(
    id: String,
    loaded_sound: Option<Sound>,
    state: Arc<AppState>,
    binding_lookup: bool,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<String, CommandError>) + 'static,
{
    let now = Instant::now();
    let debounced = {
        let mut last = state.play_dispatch_debounce.lock();
        let drop_request = matches!(
            last.as_ref(),
            Some((prev_at, prev_id))
                if prev_id == &id
                    && now.duration_since(*prev_at).as_millis() < SAME_SOUND_DEBOUNCE_MS
        );
        if !drop_request {
            *last = Some((now, id.clone()));
        }
        drop_request
    };
    if debounced {
        log::debug!("Debounced repeated play_sound dispatch: id={}", id);
        on_complete(Ok(String::new()));
        return Ok(());
    }
    let library = state.library.clone();
    let player = Arc::clone(&state.player);
    let first_recorded = Arc::clone(&state.first_playback_recorded);
    let state_diag = Arc::clone(&state);
    dispatch_async_result(
        "play_sound",
        move || match loaded_sound {
            Some(sound) => play_resolved_sound(sound, player),
            None => play_sound_from_library(&id, binding_lookup, &library, player),
        },
        move |result: Result<String, CommandError>| {
            if result.is_ok()
                && first_recorded
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            {
                crate::diagnostics::set_playback_registry_count(
                    state_diag.player.get_playback_positions().len(),
                );
                crate::diagnostics::record_phase_with_config(
                    "playback:first_play_start",
                    &state_diag.config.lock(),
                );
            }
            on_complete(result);
        },
    )
}

pub fn set_allow_multiple_playbacks(
    allow: bool,
    config: Arc<Mutex<Config>>,
) -> Result<(), CommandError> {
    if allow {
        log::info!("Ignoring request to enable multiple simultaneous playbacks");
    }
    with_saved_config(&config, |cfg| {
        cfg.settings.allow_multiple_playbacks = false;
    })
}

pub fn set_skip_delete_confirm(skip: bool, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    with_saved_config(&config, |cfg| {
        cfg.settings.skip_delete_confirm = skip;
    })
}

fn save_config_and_notify_player<F, G>(
    config: &Arc<Mutex<Config>>,
    player: &Arc<AudioPlayer>,
    save_update: F,
    notify_player: G,
) -> Result<(), CommandError>
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
    library: crate::library_store::LibraryStore,
    player: Arc<AudioPlayer>,
    coords: &LoudnessCoordinators,
) -> Result<(), CommandError> {
    save_config_and_notify_player(
        &config,
        &player,
        |cfg| {
            cfg.settings.auto_gain = enabled;
        },
        |player| player.set_auto_gain_enabled(enabled),
    )?;
    if enabled {
        // The library store is the only sound authority under schema 8; the
        // config-backed trigger sees an empty library and never schedules.
        trigger_missing_loudness_analysis_with_store(
            Arc::clone(&config),
            library,
            false,
            None,
            coords,
        )
        .map(|_| ())?;
    }
    Ok(())
}

pub fn set_auto_gain_target(
    target_lufs: f64,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), CommandError> {
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
) -> Result<(), CommandError> {
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
) -> Result<(), CommandError> {
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
) -> Result<(), CommandError> {
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
) -> Result<(), CommandError> {
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

pub fn analyze_sound_loudness(
    id: String,
    config: Arc<Mutex<Config>>,
) -> Result<Option<f64>, CommandError> {
    let path = with_config(&config, |cfg| {
        cfg.sounds
            .iter()
            .find(|s| s.id == id)
            .map(|sound| sound.path.clone())
            .ok_or(CommandError::SoundNotFound)
    })??;
    let (raw_lufs, true_peak_dbtp) = audio_loudness::analyze_loudness_path_full(Path::new(&path))
        .map_err(|e| CommandError::Analysis(e.to_string()))?;
    let (lufs, state, confidence, stored_true_peak) = if raw_lufs.is_finite() {
        (
            Some(raw_lufs),
            LoudnessAnalysisState::Refined,
            Some(FAST_LUFS_REFINED_CONFIDENCE),
            true_peak_dbtp,
        )
    } else {
        log::warn!(
            "Marking sound as unavailable due to non-finite loudness result for '{}': {}",
            path,
            raw_lufs
        );
        (None, LoudnessAnalysisState::Unavailable, None, None)
    };
    with_config_mut(&config, |cfg| {
        if let Some(sound) = cfg.sounds.iter_mut().find(|s| s.id == id) {
            sound.loudness_lufs = lufs;
            sound.loudness_analysis_state = state;
            sound.loudness_confidence = confidence;
            sound.loudness_true_peak_dbtp = stored_true_peak;
        }
        cfg.save().map_err(CommandError::config_save)
    })??;
    Ok(lufs)
}

pub fn analyze_sound_loudness_async<F>(
    id: String,
    config: Arc<Mutex<Config>>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Option<f64>, CommandError>) + 'static,
{
    dispatch_async_result(
        "analyze_sound_loudness",
        move || analyze_sound_loudness(id, config),
        on_complete,
    )
}

pub fn analyze_sound_loudness_with_store_async<F>(
    id: String,
    library: crate::library_store::LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Option<f64>, CommandError>) + 'static,
{
    dispatch_async_result(
        "analyze_sound_loudness",
        move || {
            let sound = library
                .sound_by_id(&id)
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))?
                .ok_or(CommandError::SoundNotFound)?;
            let (raw_lufs, true_peak_dbtp) =
                audio_loudness::analyze_loudness_path_full(Path::new(&sound.path))
                    .map_err(|error| CommandError::Analysis(error.to_string()))?;
            let (lufs, state, confidence, true_peak_dbtp) = if raw_lufs.is_finite() {
                (
                    Some(raw_lufs),
                    LoudnessAnalysisState::Refined,
                    Some(FAST_LUFS_REFINED_CONFIDENCE),
                    true_peak_dbtp,
                )
            } else {
                (None, LoudnessAnalysisState::Unavailable, None, None)
            };
            library
                .apply_loudness_updates(vec![crate::library_store::LoudnessUpdate {
                    sound_id: id,
                    lufs,
                    state,
                    confidence,
                    true_peak_dbtp,
                }])
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))?;
            Ok(lufs)
        },
        on_complete,
    )
}

pub fn stop_sound(id: String, player: Arc<dyn PlaybackEngine>) -> Result<(), CommandError> {
    player
        .stop_sound(&id)
        .map_err(|e| CommandError::Engine(e.to_string()))
}

pub fn stop_all(player: Arc<dyn PlaybackEngine>) {
    player.stop_all();
    crate::diagnostics::set_playback_registry_count(0);
    crate::diagnostics::record_phase("playback:stop_all_idle", None);
}

pub fn seek_playback(
    play_id: String,
    position_ms: u64,
    player: Arc<dyn PlaybackEngine>,
) -> Result<(), CommandError> {
    if position_ms > 24 * 60 * 60 * 1000 {
        return Err(CommandError::Invalid(
            "Seek position too large (max 24 hours)".to_string(),
        ));
    }
    log::debug!(
        "Dispatching seek request: play_id={}, position_ms={}",
        play_id,
        position_ms
    );
    player.seek_playback(&play_id, position_ms);
    Ok(())
}

pub fn seek_sound(
    id: String,
    position_ms: u64,
    player: Arc<dyn PlaybackEngine>,
) -> Result<(), CommandError> {
    if position_ms > 24 * 60 * 60 * 1000 {
        return Err(CommandError::Invalid(
            "Seek position too large (max 24 hours)".to_string(),
        ));
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

pub fn pause_sound(id: String, player: Arc<dyn PlaybackEngine>) {
    player.pause(&id);
}

pub fn resume_sound(id: String, player: Arc<dyn PlaybackEngine>) {
    player.resume(&id);
}

pub fn get_audio_status(player: Arc<dyn PlaybackEngine>) -> AudioStatus {
    let playing = player.get_playing();
    let mut positions: HashMap<String, u64> = HashMap::new();
    for p in player.get_playback_positions() {
        if !p.finished {
            positions.entry(p.sound_id.clone()).or_insert(p.position_ms);
        }
    }
    AudioStatus { playing, positions }
}

pub fn get_playback_positions(player: Arc<dyn PlaybackEngine>) -> Vec<PlaybackPosition> {
    player.get_playback_positions()
}

pub fn cancel_loudness_analysis() {
    audio_loudness::cancel_loudness_analysis();
}

#[derive(serde::Serialize)]
pub struct AudioStatus {
    pub playing: Vec<String>,
    pub positions: HashMap<String, u64>,
}

#[cfg(test)]
#[path = "playback_tests.rs"]
mod tests;
