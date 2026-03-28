use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use rayon::prelude::*;

use crate::audio::loudness;
use crate::audio::player::{AudioPlayer, PlaybackPosition};
use crate::config::{Config, Sound};

use super::shared::{
    bounded_audio_analysis_threads, parse_auto_gain_apply_to, parse_auto_gain_mode,
    validate_play_mode, with_saved_config,
};

static MISSING_LOUDNESS_ANALYSIS_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

pub type LoudnessAnalysisCompletion = Box<dyn FnOnce(Result<u32, String>) + Send + 'static>;

#[cfg(test)]
static MISSING_LOUDNESS_ANALYSIS_START_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static FIRST_PLAYBACK_PHASE_RECORDED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingLoudnessAnalysisTrigger {
    Started,
    SkippedAutoGainDisabled,
    SkippedNoMissingSounds,
    SkippedAlreadyRunning,
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
            cfg.sounds.iter().any(|sound| sound.loudness_lufs.is_none()),
            force,
            MISSING_LOUDNESS_ANALYSIS_IN_FLIGHT.load(Ordering::Acquire),
        )
    };

    if trigger != MissingLoudnessAnalysisTrigger::Started {
        return Ok(trigger);
    }

    if MISSING_LOUDNESS_ANALYSIS_IN_FLIGHT
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Ok(MissingLoudnessAnalysisTrigger::SkippedAlreadyRunning);
    }

    let spawn_result = std::thread::Builder::new()
        .name("loudness-backfill".to_string())
        .spawn(move || {
            let result = analyze_all_loudness(config);
            MISSING_LOUDNESS_ANALYSIS_IN_FLIGHT.store(false, Ordering::Release);
            if let Some(on_complete) = on_complete {
                on_complete(result);
            }
        });

    if let Err(e) = spawn_result {
        MISSING_LOUDNESS_ANALYSIS_IN_FLIGHT.store(false, Ordering::Release);
        return Err(format!("Failed to spawn loudness analysis thread: {e}"));
    }

    #[cfg(test)]
    MISSING_LOUDNESS_ANALYSIS_START_COUNT.fetch_add(1, Ordering::AcqRel);

    Ok(MissingLoudnessAnalysisTrigger::Started)
}

#[cfg(test)]
pub(super) fn reset_missing_loudness_analysis_test_state() {
    MISSING_LOUDNESS_ANALYSIS_START_COUNT.store(0, Ordering::Release);
}

#[cfg(test)]
pub(super) fn missing_loudness_analysis_start_count() -> usize {
    MISSING_LOUDNESS_ANALYSIS_START_COUNT.load(Ordering::Acquire)
}

#[cfg(test)]
pub(super) fn wait_for_missing_loudness_analysis_to_finish(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if !MISSING_LOUDNESS_ANALYSIS_IN_FLIGHT.load(Ordering::Acquire) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    false
}

#[allow(dead_code)]
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
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<String, String> {
    // Get sound data first (brief lock).
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

    // Get player lock
    let player = player
        .lock()
        .map_err(|e| format!("Player lock poisoned: {}", e))?;

    // Playback is serialized in the backend so a new play request replaces
    // any currently active playback threads.
    player.stop_all();

    let base_volume = sound.volume as f32 / 100.0;
    let sound_lufs = sound.loudness_lufs;
    let result = player.play(&id, &sound.path, base_volume, sound_lufs);
    if let Err(err) = &result {
        log::error!(
            "play_sound failed: id='{}' path='{}' base_volume={:.3} err={}",
            id,
            sound.path,
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

pub fn set_auto_gain(
    enabled: bool,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    with_saved_config(&config, |cfg| {
        cfg.settings.auto_gain = enabled;
    })?;
    player
        .lock()
        .map(|p| p.set_auto_gain_enabled(enabled))
        .map_err(|e| format!("Player lock poisoned: {}", e))?;
    if enabled {
        trigger_missing_loudness_analysis(Arc::clone(&config), false, None).map(|_| ())?;
    }
    Ok(())
}

pub fn set_auto_gain_target(
    target_lufs: f64,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    let clamped = target_lufs.clamp(-24.0, 0.0);
    with_saved_config(&config, |cfg| {
        cfg.settings.auto_gain_target_lufs = clamped;
    })?;
    player
        .lock()
        .map(|p| p.set_auto_gain_target(clamped))
        .map_err(|e| format!("Player lock poisoned: {}", e))?;
    Ok(())
}

pub fn set_auto_gain_mode(
    mode: String,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    let mode = parse_auto_gain_mode(&mode)?;
    with_saved_config(&config, |cfg| {
        cfg.settings.auto_gain_mode = mode;
    })?;
    player
        .lock()
        .map(|p| p.set_auto_gain_mode(mode.player_value()))
        .map_err(|e| format!("Player lock poisoned: {}", e))?;
    Ok(())
}

pub fn set_auto_gain_apply_to(
    scope: String,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    let scope = parse_auto_gain_apply_to(&scope)?;
    with_saved_config(&config, |cfg| {
        cfg.settings.auto_gain_apply_to = scope;
    })?;
    player
        .lock()
        .map(|p| p.set_auto_gain_apply_to(scope.player_value()))
        .map_err(|e| format!("Player lock poisoned: {}", e))?;
    Ok(())
}

pub fn set_auto_gain_dynamic_settings(
    lookahead_ms: u32,
    attack_ms: u32,
    release_ms: u32,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    let lookahead_ms = lookahead_ms.clamp(5, 200);
    let attack_ms = attack_ms.clamp(1, 50);
    let release_ms = release_ms.clamp(50, 1000);

    with_saved_config(&config, |cfg| {
        cfg.settings.auto_gain_lookahead_ms = lookahead_ms;
        cfg.settings.auto_gain_attack_ms = attack_ms;
        cfg.settings.auto_gain_release_ms = release_ms;
    })?;

    player
        .lock()
        .map(|p| p.set_auto_gain_dynamic_settings(lookahead_ms, attack_ms, release_ms))
        .map_err(|e| format!("Player lock poisoned: {}", e))?;
    Ok(())
}

pub fn set_play_mode(
    mode: String,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    let mode = validate_play_mode(&mode)?;
    with_saved_config(&config, |cfg| {
        cfg.settings.play_mode = mode;
    })?;
    player
        .lock()
        .map(|p| p.set_looping(mode.should_loop()))
        .map_err(|e| format!("Player lock poisoned: {}", e))?;
    Ok(())
}

pub fn analyze_all_loudness(config: Arc<Mutex<Config>>) -> Result<u32, String> {
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:start");
    let sounds_to_analyze: Vec<(String, String)> = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        cfg.sounds
            .iter()
            .filter(|s| s.loudness_lufs.is_none())
            .map(|s| (s.id.clone(), s.path.clone()))
            .collect()
    };
    if let Ok(cfg) = config.lock() {
        crate::diagnostics::record_phase_with_config("analyze_all_loudness:start", &cfg);
    } else {
        crate::diagnostics::record_phase("analyze_all_loudness:start", None);
    }

    log::info!("Analyzing loudness for {} sounds", sounds_to_analyze.len());

    let analyze_entry = |(id, path): &(String, String)| -> Option<(String, f64)> {
        if !Path::new(path).exists() {
            return None;
        }
        match loudness::analyze_loudness_path(Path::new(path)) {
            Ok(lufs) if lufs.is_finite() => Some((id.clone(), lufs)),
            Ok(lufs) => {
                log::warn!(
                    "Ignoring non-finite loudness result for '{}': {}",
                    path,
                    lufs
                );
                None
            }
            Err(e) => {
                log::warn!("Failed to analyze loudness for '{}': {}", path, e);
                None
            }
        }
    };

    let analysis_threads = bounded_audio_analysis_threads();
    let pool_threads = if sounds_to_analyze.is_empty() {
        1
    } else {
        analysis_threads
    };
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
    let results: Vec<(String, f64)> = if sounds_to_analyze.is_empty() {
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
                    analysis_threads,
                    e
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

    let analyzed_count = results.len() as u32;
    if analyzed_count > 0 {
        let mut cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        for (id, lufs) in results {
            if let Some(sound) = cfg.sounds.iter_mut().find(|s| s.id == id) {
                sound.loudness_lufs = Some(lufs);
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
    Ok(analyzed_count)
}

#[allow(dead_code)]
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
    let lufs = if lufs.is_finite() {
        Some(lufs)
    } else {
        log::warn!(
            "Ignoring non-finite loudness result for '{}': {}",
            path,
            lufs
        );
        None
    };

    let mut cfg = config
        .lock()
        .map_err(|e| format!("Config lock poisoned: {}", e))?;
    if let Some(sound) = cfg.sounds.iter_mut().find(|s| s.id == id) {
        sound.loudness_lufs = lufs;
    }
    cfg.save().map_err(|e| e.to_string())?;

    Ok(lufs)
}

#[allow(dead_code)]
pub fn stop_sound(id: String, player: Arc<Mutex<AudioPlayer>>) -> Result<(), String> {
    player
        .lock()
        .map(|p| p.stop_sound(&id))
        .map_err(|e| format!("Player lock poisoned: {}", e))?
}

pub fn stop_all(player: Arc<Mutex<AudioPlayer>>) {
    player.lock().map(|p| p.stop_all()).unwrap_or_else(|e| {
        log::warn!("Player lock poisoned in stop_all: {}", e);
    });
    crate::diagnostics::set_playback_registry_count(0);
    crate::diagnostics::record_phase("playback:stop_all_idle", None);
}

pub fn seek_playback(
    play_id: String,
    position_ms: u64,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    if position_ms > 24 * 60 * 60 * 1000 {
        return Err("Seek position too large (max 24 hours)".to_string());
    }

    log::debug!(
        "Dispatching seek request: play_id={}, position_ms={}",
        play_id,
        position_ms
    );

    player
        .lock()
        .map(|p| p.seek_playback(&play_id, position_ms))
        .map_err(|e| format!("Player lock poisoned: {}", e))?;

    Ok(())
}

pub fn seek_sound(
    id: String,
    position_ms: u64,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    if position_ms > 24 * 60 * 60 * 1000 {
        return Err("Seek position too large (max 24 hours)".to_string());
    }

    let player = player
        .lock()
        .map_err(|e| format!("Player lock poisoned: {}", e))?;

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

pub fn pause_sound(id: String, player: Arc<Mutex<AudioPlayer>>) {
    let _ = player.lock().map(|p| p.pause(&id));
}

pub fn resume_sound(id: String, player: Arc<Mutex<AudioPlayer>>) {
    let _ = player.lock().map(|p| p.resume(&id));
}

#[allow(dead_code)]
pub fn get_audio_status(player: Arc<Mutex<AudioPlayer>>) -> AudioStatus {
    let player = match player.lock() {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Player lock poisoned in get_audio_status: {}", e);
            return AudioStatus {
                playing: Vec::new(),
                positions: HashMap::new(),
            };
        }
    };
    let playing = player.get_playing();

    let mut positions: HashMap<String, u64> = HashMap::new();
    for p in player.get_playback_positions() {
        if !p.finished {
            positions.entry(p.sound_id.clone()).or_insert(p.position_ms);
        }
    }

    AudioStatus { playing, positions }
}

pub fn get_playback_positions(player: Arc<Mutex<AudioPlayer>>) -> Vec<PlaybackPosition> {
    player
        .lock()
        .map(|p| p.get_playback_positions())
        .unwrap_or_else(|e| {
            log::warn!("Player lock poisoned in get_playback_positions: {}", e);
            Vec::new()
        })
}

#[derive(serde::Serialize)]
#[allow(dead_code)]
pub struct AudioStatus {
    pub playing: Vec<String>,
    pub positions: HashMap<String, u64>,
}

#[cfg(test)]
mod tests {
    use super::{missing_loudness_analysis_trigger, MissingLoudnessAnalysisTrigger};

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
