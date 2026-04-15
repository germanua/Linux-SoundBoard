use std::str::FromStr;
use std::sync::{Arc, Mutex};

use crate::audio::player::AudioPlayer;
use crate::config::{Config, DefaultSourceMode, ListStyle, MicLatencyProfile, Theme};
use crate::pipewire::detection::{check_pipewire, PipeWireStatus};

use super::shared::{
    dispatch_async_result, with_config_mut, with_saved_config, with_saved_config_result,
};

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
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    let (clamped_volume, local_muted) = with_saved_config_result(&config, |cfg| {
        let clamped = volume.min(100);
        cfg.settings.local_volume = clamped;
        Ok((clamped, cfg.settings.local_mute))
    })?;

    if !local_muted {
        player.set_local_volume(clamped_volume as f32 / 100.0);
    }
    Ok(())
}

pub fn toggle_local_mute(
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<bool, String> {
    let (local_mute, local_volume) = with_saved_config_result(&config, |cfg| {
        cfg.settings.local_mute = !cfg.settings.local_mute;
        Ok((cfg.settings.local_mute, cfg.settings.local_volume))
    })?;

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
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    let clamped = with_saved_config_result(&config, |cfg| {
        let clamped = volume.min(100);
        cfg.settings.mic_volume = clamped;
        Ok(clamped)
    })?;

    player.set_mic_volume(clamped as f32 / 100.0);
    Ok(())
}

pub fn get_config(config: Arc<Mutex<Config>>) -> Config {
    config.lock().map(|cfg| cfg.clone()).unwrap_or_else(|_e| {
        log::warn!("Config lock poisoned in get_config, returning default");
        Config::default()
    })
}

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

pub fn set_mic_passthrough_enabled(
    enabled: bool,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<bool, String> {
    player.set_mic_passthrough(enabled)?;

    let _ = with_config_mut(&config, |cfg| {
        cfg.settings.mic_passthrough = enabled;
        cfg.save().map_err(|e| e.to_string())
    })?;

    Ok(enabled)
}

pub fn toggle_mic_passthrough(
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<bool, String> {
    let current_state = with_config_mut(&config, |cfg| cfg.settings.mic_passthrough)?;
    set_mic_passthrough_enabled(!current_state, config, player)
}

pub fn list_audio_sources(player: Arc<AudioPlayer>) -> Vec<AudioSource> {
    player
        .list_audio_sources()
        .into_iter()
        .map(|source| AudioSource {
            name: source.node_name,
            display_name: source.display_name,
        })
        .collect()
}

pub fn set_mic_source(
    source: Option<String>,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    player.set_mic_source(source.clone())?;
    with_config_mut(&config, |cfg| {
        cfg.settings.mic_source = source;
        cfg.save().map_err(|e| e.to_string())
    })??;
    Ok(())
}

pub fn set_default_source_mode(
    mode: DefaultSourceMode,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    player.set_default_source_mode(mode)?;
    with_config_mut(&config, |cfg| {
        cfg.settings.default_source_mode = mode;
        cfg.save().map_err(|e| e.to_string())
    })??;
    Ok(())
}

pub fn set_mic_latency_profile(
    profile: MicLatencyProfile,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), String> {
    player.set_mic_latency_profile(profile)?;
    with_config_mut(&config, |cfg| {
        cfg.settings.mic_latency_profile = profile;
        cfg.save().map_err(|e| e.to_string())
    })??;
    Ok(())
}

pub fn set_mic_passthrough_enabled_async<F>(
    enabled: bool,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
    on_complete: F,
) -> Result<(), String>
where
    F: FnOnce(Result<bool, String>) + 'static,
{
    dispatch_async_result(
        "set_mic_passthrough_enabled",
        move || set_mic_passthrough_enabled(enabled, config, player),
        on_complete,
    )
}

pub fn set_mic_source_async<F>(
    source: Option<String>,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
    on_complete: F,
) -> Result<(), String>
where
    F: FnOnce(Result<(), String>) + 'static,
{
    dispatch_async_result(
        "set_mic_source",
        move || set_mic_source(source, config, player),
        on_complete,
    )
}

pub fn set_default_source_mode_async<F>(
    mode: DefaultSourceMode,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
    on_complete: F,
) -> Result<(), String>
where
    F: FnOnce(Result<(), String>) + 'static,
{
    dispatch_async_result(
        "set_default_source_mode",
        move || set_default_source_mode(mode, config, player),
        on_complete,
    )
}

pub fn set_mic_latency_profile_async<F>(
    profile: MicLatencyProfile,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
    on_complete: F,
) -> Result<(), String>
where
    F: FnOnce(Result<(), String>) + 'static,
{
    dispatch_async_result(
        "set_mic_latency_profile",
        move || set_mic_latency_profile(profile, config, player),
        on_complete,
    )
}

#[derive(serde::Serialize)]
pub struct AudioSource {
    pub name: String,
    pub display_name: String,
}

pub fn check_pipewire_status() -> PipeWireStatus {
    check_pipewire()
}
