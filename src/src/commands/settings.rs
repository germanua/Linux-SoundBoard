use std::str::FromStr;
use std::sync::{Arc, Mutex};

use crate::audio::player::AudioPlayer;
use crate::config::{Config, ListStyle};
use crate::pipewire::detection::{check_pipewire, PipeWireStatus};

use super::shared::{parse_theme, with_saved_config, with_saved_config_result};

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

    let player = player.lock().unwrap();
    if !local_muted {
        player.set_local_volume(clamped_volume as f32 / 100.0);
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

    let player = player.lock().unwrap();
    if local_mute {
        player.set_local_volume(0.0);
    } else {
        player.set_local_volume(local_volume as f32 / 100.0);
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
    player
        .lock()
        .unwrap()
        .set_mic_volume(clamped as f32 / 100.0);
    Ok(())
}

#[allow(dead_code)]
pub fn get_config(config: Arc<Mutex<Config>>) -> Config {
    config.lock().unwrap().clone()
}

#[allow(dead_code)]
pub fn save_config(config: Arc<Mutex<Config>>) -> Result<(), String> {
    config.lock().unwrap().save().map_err(|e| e.to_string())
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
    let mut config = config.lock().unwrap();
    config.settings.mic_passthrough = !config.settings.mic_passthrough;
    if config.settings.mic_passthrough {
        if let Err(e) =
            virtual_mic::enable_mic_passthrough_with_source(config.settings.mic_source.clone())
        {
            log::warn!("Failed to enable mic passthrough: {}", e);
        }
    } else {
        let _ = virtual_mic::disable_mic_passthrough();
    }
    config.save().map_err(|e| e.to_string())?;
    Ok(config.settings.mic_passthrough)
}

pub fn list_audio_sources() -> Vec<AudioSource> {
    crate::pipewire::virtual_mic::list_sources()
        .into_iter()
        .map(|name| AudioSource { name })
        .collect()
}

pub fn set_mic_source(source: Option<String>, config: Arc<Mutex<Config>>) -> Result<(), String> {
    use crate::pipewire::virtual_mic;
    let mut config = config.lock().unwrap();
    config.settings.mic_source = source.clone();
    config.save().map_err(|e| e.to_string())?;
    if config.settings.mic_passthrough {
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
