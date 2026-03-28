use std::str::FromStr;
use std::sync::{Arc, Mutex};

use crate::audio::player::AudioPlayer;
use crate::config::{Config, ListStyle, Theme};
use crate::pipewire::detection::{check_pipewire, PipeWireStatus};

use super::shared::{with_config_mut, with_saved_config};

/// Helper to execute a closure and save config on success
fn with_saved_config_result<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut Config) -> R,
{
    with_config_mut(config, |cfg| {
        let result = f(cfg);
        cfg.save().map_err(|e| e.to_string())?;
        Ok(result)
    })?
}

fn parse_theme(s: &str) -> Result<Theme, String> {
    match s.to_lowercase().as_str() {
        "dark" => Ok(Theme::Dark),
        "light" => Ok(Theme::Light),
        _ => Err(format!("Invalid theme '{}'. Use 'dark' or 'light'.", s)),
    }
}

pub fn set_local_volume(
    volume: u8,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    let (clamped_volume, local_muted) = with_saved_config_result(&config, |cfg| {
        let clamped = volume.min(100);
        cfg.settings.local_volume = clamped;
        (clamped, cfg.settings.local_mute)
    })?;

    // Handle player lock poison gracefully
    if let Ok(player) = player.lock() {
        if !local_muted {
            player.set_local_volume(clamped_volume as f32 / 100.0);
        }
    } else {
        log::warn!("Player lock poisoned, skipping volume change");
    }
    Ok(())
}

pub fn toggle_local_mute(
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<bool, String> {
    let (local_mute, local_volume) = with_saved_config_result(&config, |cfg| {
        cfg.settings.local_mute = !cfg.settings.local_mute;
        (cfg.settings.local_mute, cfg.settings.local_volume)
    })?;

    // Handle player lock poison gracefully
    if let Ok(player) = player.lock() {
        if local_mute {
            player.set_local_volume(0.0);
        } else {
            player.set_local_volume(local_volume as f32 / 100.0);
        }
    } else {
        log::warn!("Player lock poisoned, skipping mute toggle");
    }
    Ok(local_mute)
}

pub fn set_mic_volume(
    volume: u8,
    config: Arc<Mutex<Config>>,
    player: Arc<Mutex<AudioPlayer>>,
) -> Result<(), String> {
    let clamped = with_saved_config_result(&config, |cfg| {
        let clamped = volume.min(100);
        cfg.settings.mic_volume = clamped;
        clamped
    })?;

    // Handle player lock poison gracefully
    player
        .lock()
        .map(|player| player.set_mic_volume(clamped as f32 / 100.0))
        .map_err(|e| format!("Player lock poisoned: {}", e))?;
    Ok(())
}

#[allow(dead_code)]
pub fn get_config(config: Arc<Mutex<Config>>) -> Config {
    config.lock().map(|cfg| cfg.clone()).unwrap_or_else(|_e| {
        log::warn!("Config lock poisoned in get_config, returning default");
        Config::default()
    })
}

#[allow(dead_code)]
pub fn save_config(config: Arc<Mutex<Config>>) -> Result<(), String> {
    with_config_mut(&config, |cfg| cfg.save().map_err(|e| e.to_string()))?
}

pub fn set_theme(theme: String, config: Arc<Mutex<Config>>) -> Result<(), String> {
    let theme = parse_theme(&theme)?;
    with_saved_config(&config, |cfg| {
        cfg.settings.theme = theme;
    })
}

pub fn set_list_style(style: String, config: Arc<Mutex<Config>>) -> Result<(), String> {
    let style = ListStyle::from_str(&style)
        .map_err(|_| "Invalid list style. Use 'compact' or 'card'.".to_string())?;
    with_saved_config(&config, |cfg| {
        cfg.settings.list_style = style;
    })
}

pub fn toggle_mic_passthrough(config: Arc<Mutex<Config>>) -> Result<bool, String> {
    use crate::pipewire::virtual_mic;

    // First read the current state
    let (current_state, mic_source) = with_config_mut(&config, |cfg| {
        let state = cfg.settings.mic_passthrough;
        let source = cfg.settings.mic_source.clone();
        (state, source)
    })?;

    // Toggle the state
    let new_state = !current_state;

    if new_state {
        if let Err(e) = virtual_mic::enable_mic_passthrough_with_source(mic_source) {
            log::warn!("Failed to enable mic passthrough: {}", e);
        }
    } else {
        let _ = virtual_mic::disable_mic_passthrough();
    }

    // Save the new state
    let _ = with_config_mut(&config, |cfg| {
        cfg.settings.mic_passthrough = new_state;
        cfg.save().map_err(|e| e.to_string())
    })?;

    Ok(new_state)
}

pub fn list_audio_sources() -> Vec<AudioSource> {
    crate::pipewire::virtual_mic::list_sources()
        .into_iter()
        .map(|name| AudioSource { name })
        .collect()
}

pub fn set_mic_source(source: Option<String>, config: Arc<Mutex<Config>>) -> Result<(), String> {
    use crate::pipewire::virtual_mic;

    // First save the source setting
    with_config_mut(&config, |cfg| {
        cfg.settings.mic_source = source.clone();
        cfg.save().map_err(|e| e.to_string())
    })??;

    // Then handle mic passthrough
    let current_state = with_config_mut(&config, |cfg| cfg.settings.mic_passthrough)?;

    if current_state {
        let _ = virtual_mic::disable_mic_passthrough();
        if let Err(e) = virtual_mic::enable_mic_passthrough_with_source(source) {
            log::warn!("Failed to restart mic passthrough: {}", e);
            return Err(e);
        }
    }
    Ok(())
}

#[derive(serde::Serialize)]
pub struct AudioSource {
    pub name: String,
}

#[allow(dead_code)]
pub fn check_pipewire_status() -> PipeWireStatus {
    check_pipewire()
}
