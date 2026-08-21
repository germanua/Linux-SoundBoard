use parking_lot::Mutex;
use std::str::FromStr;
use std::sync::Arc;

use crate::audio::pipewire_detection::{check_pipewire, PipeWireStatus};
use crate::audio::AudioPlayer;
use crate::config::{Config, DefaultSourceMode, ListStyle, MicLatencyProfile, Theme};

use super::shared::{
    dispatch_async_result, with_config_mut, with_saved_config, with_saved_config_result,
};
use super::CommandError;

fn parse_theme(s: &str) -> Result<Theme, CommandError> {
    match s.to_lowercase().as_str() {
        "dark" => Ok(Theme::Dark),
        "light" => Ok(Theme::Light),
        _ => Err(CommandError::Invalid(format!(
            "Invalid theme '{}'. Use 'dark' or 'light'.",
            s
        ))),
    }
}

pub fn set_local_volume(
    volume: u8,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), CommandError> {
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

pub fn set_local_volume_live(
    volume: u8,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), CommandError> {
    let (clamped_volume, local_muted) = with_config_mut(&config, |cfg| {
        let clamped = volume.min(100);
        cfg.settings.local_volume = clamped;
        (clamped, cfg.settings.local_mute)
    })?;

    if !local_muted {
        player.set_local_volume(clamped_volume as f32 / 100.0);
    }
    Ok(())
}

pub fn save_local_volume(volume: u8, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    with_saved_config(&config, |cfg| {
        cfg.settings.local_volume = volume.min(100);
    })
}

pub fn toggle_local_mute(
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<bool, CommandError> {
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
) -> Result<(), CommandError> {
    let clamped = with_saved_config_result(&config, |cfg| {
        let clamped = volume.min(100);
        cfg.settings.mic_volume = clamped;
        Ok(clamped)
    })?;

    player.set_mic_volume(clamped as f32 / 100.0);
    Ok(())
}

pub fn set_mic_volume_live(
    volume: u8,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), CommandError> {
    let clamped = with_config_mut(&config, |cfg| {
        let clamped = volume.min(100);
        cfg.settings.mic_volume = clamped;
        clamped
    })?;

    player.set_mic_volume(clamped as f32 / 100.0);
    Ok(())
}

pub fn save_mic_volume(volume: u8, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    with_saved_config(&config, |cfg| {
        cfg.settings.mic_volume = volume.min(100);
    })
}

pub fn get_config(config: Arc<Mutex<Config>>) -> Config {
    (*config.lock()).clone()
}

pub fn save_config(config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    with_config_mut(&config, |cfg| cfg.save().map_err(CommandError::config_save))?
}

pub fn set_theme(theme: String, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    let theme = parse_theme(&theme)?;
    with_saved_config(&config, |cfg| {
        cfg.settings.theme = theme;
    })
}

pub fn set_list_style(style: String, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    let style = ListStyle::from_str(&style).map_err(|_| {
        CommandError::Invalid("Invalid list style. Use 'compact' or 'card'.".to_string())
    })?;
    with_saved_config(&config, |cfg| {
        cfg.settings.list_style = style;
    })
}

pub fn set_mic_passthrough_enabled(
    enabled: bool,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<bool, CommandError> {
    player
        .set_mic_passthrough(enabled)
        .map_err(|e| CommandError::Engine(e.to_string()))?;

    let _ = with_config_mut(&config, |cfg| {
        cfg.settings.mic_passthrough = enabled;
        cfg.save().map_err(CommandError::config_save)
    })?;

    Ok(enabled)
}

pub fn toggle_mic_passthrough(
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<bool, CommandError> {
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

pub fn active_capture_target(player: Arc<AudioPlayer>) -> Option<String> {
    player.active_capture_target()
}

pub fn set_mic_source(
    source: Option<String>,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), CommandError> {
    player
        .set_mic_source(source.clone())
        .map_err(|e| CommandError::Engine(e.to_string()))?;
    with_config_mut(&config, |cfg| {
        cfg.settings.mic_source = source;
        cfg.save().map_err(CommandError::config_save)
    })??;
    Ok(())
}

pub fn set_default_source_mode(
    mode: DefaultSourceMode,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), CommandError> {
    player
        .set_default_source_mode(mode)
        .map_err(|e| CommandError::Engine(e.to_string()))?;
    with_config_mut(&config, |cfg| {
        cfg.settings.default_source_mode = mode;
        cfg.save().map_err(CommandError::config_save)
    })??;
    Ok(())
}

pub fn set_mic_latency_profile(
    profile: MicLatencyProfile,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
) -> Result<(), CommandError> {
    player
        .set_mic_latency_profile(profile)
        .map_err(|e| CommandError::Engine(e.to_string()))?;
    with_config_mut(&config, |cfg| {
        cfg.settings.mic_latency_profile = profile;
        cfg.save().map_err(CommandError::config_save)
    })??;
    Ok(())
}

pub fn set_mic_passthrough_enabled_async<F>(
    enabled: bool,
    config: Arc<Mutex<Config>>,
    player: Arc<AudioPlayer>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<bool, CommandError>) + 'static,
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
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
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
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
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
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
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

/// Show or hide the tray icon. Takes effect immediately: the icon is exported
/// or withdrawn without restarting.
pub fn set_tray_enabled(enabled: bool, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    with_saved_config(&config, |cfg| {
        cfg.settings.tray_enabled = enabled;
    })
}

/// Let the close button hide the window instead of quitting.
pub fn set_close_to_tray(enabled: bool, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    with_saved_config(&config, |cfg| {
        cfg.settings.close_to_tray = enabled;
    })
}
