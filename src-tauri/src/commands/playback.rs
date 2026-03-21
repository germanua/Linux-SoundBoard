use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;

use crate::audio::loudness;
use crate::audio::player::{AudioPlayer, PlaybackPosition};
use crate::config::{Config, Sound};

use super::shared::{
    bounded_audio_analysis_threads, parse_auto_gain_apply_to, parse_auto_gain_mode,
    validate_play_mode, with_saved_config,
};

#[allow(dead_code)]
pub fn list_sounds(config: Arc<Mutex<Config>>) -> Vec<Sound> {
    config.lock().unwrap().sounds.clone()
}

pub fn play_sound(
    id: String,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<String, String> {
    let config = config.lock().unwrap();
    let player = player.lock().unwrap();

    let sound = config
        .get_sound(&id)
        .ok_or_else(|| "Sound not found".to_string())?;

    if !sound.enabled {
        return Err("Sound is disabled".to_string());
    }

    if !config.settings.allow_multiple_playbacks {
        player.stop_all();
    }

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
    }
    result
}

pub fn set_allow_multiple_playbacks(allow: bool, config: Arc<Mutex<Config>>) -> Result<(), String> {
    with_saved_config(&config, |cfg| {
        cfg.settings.allow_multiple_playbacks = allow;
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
    player.lock().unwrap().set_auto_gain_enabled(enabled);
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
    player.lock().unwrap().set_auto_gain_target(clamped);
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
        .unwrap()
        .set_auto_gain_mode(mode.player_value());
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
        .unwrap()
        .set_auto_gain_apply_to(scope.player_value());
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
        .unwrap()
        .set_auto_gain_dynamic_settings(lookahead_ms, attack_ms, release_ms);
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
    player.lock().unwrap().set_looping(mode.should_loop());
    Ok(())
}

pub fn analyze_all_loudness(config: Arc<Mutex<Config>>) -> Result<u32, String> {
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:start");
    let sounds_to_analyze: Vec<(String, String)> = {
        let cfg = config.lock().unwrap();
        cfg.sounds
            .iter()
            .filter(|s| s.loudness_lufs.is_none())
            .map(|s| (s.id.clone(), s.path.clone()))
            .collect()
    };

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
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:before_pool");
    let results: Vec<(String, f64)> = match rayon::ThreadPoolBuilder::new()
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
    };
    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:after_pool");

    let analyzed_count = results.len() as u32;
    if analyzed_count > 0 {
        let mut cfg = config.lock().unwrap();
        for (id, lufs) in results {
            if let Some(sound) = cfg.sounds.iter_mut().find(|s| s.id == id) {
                sound.loudness_lufs = Some(lufs);
            }
        }
        cfg.save().map_err(|e| e.to_string())?;
    }

    crate::diagnostics::memory::log_memory_snapshot("analyze_all_loudness:end");
    Ok(analyzed_count)
}

#[allow(dead_code)]
pub fn analyze_sound_loudness(
    id: String,
    config: Arc<Mutex<Config>>,
) -> Result<Option<f64>, String> {
    let path = {
        let cfg = config.lock().unwrap();
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

    let mut cfg = config.lock().unwrap();
    if let Some(sound) = cfg.sounds.iter_mut().find(|s| s.id == id) {
        sound.loudness_lufs = lufs;
    }
    cfg.save().map_err(|e| e.to_string())?;

    Ok(lufs)
}

#[allow(dead_code)]
pub fn stop_sound(id: String, player: Arc<Mutex<AudioPlayer>>) -> Result<(), String> {
    player.lock().unwrap().stop_sound(&id)
}

pub fn stop_all(player: Arc<Mutex<AudioPlayer>>) {
    player.lock().unwrap().stop_all();
}

pub fn seek_sound(
    id: String,
    position_ms: u64,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    if position_ms > 24 * 60 * 60 * 1000 {
        return Err("Seek position too large (max 24 hours)".to_string());
    }
    player.lock().unwrap().seek(&id, position_ms);
    Ok(())
}

pub fn pause_sound(id: String, player: Arc<Mutex<AudioPlayer>>) {
    player.lock().unwrap().pause(&id);
}

pub fn resume_sound(id: String, player: Arc<Mutex<AudioPlayer>>) {
    player.lock().unwrap().resume(&id);
}

#[allow(dead_code)]
pub fn get_audio_status(player: Arc<Mutex<AudioPlayer>>) -> AudioStatus {
    let player = player.lock().unwrap();
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
    player.lock().unwrap().get_playback_positions()
}

#[derive(serde::Serialize)]
#[allow(dead_code)]
pub struct AudioStatus {
    pub playing: Vec<String>,
    pub positions: HashMap<String, u64>,
}
