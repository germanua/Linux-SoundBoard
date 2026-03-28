use std::sync::{Arc, Mutex};

use crate::config::{Config, PlayMode, Sound, Theme};

/// Helper to acquire config lock with poison handling - returns Result
#[allow(dead_code)]
pub fn with_config<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&Config) -> R,
{
    config
        .lock()
        .map(|guard| f(&guard))
        .map_err(|e| format!("Config lock poisoned: {}", e))
}

/// Helper to acquire config mut lock with poison handling - returns Result
pub fn with_config_mut<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut Config) -> R,
{
    config
        .lock()
        .map(|mut guard| f(&mut guard))
        .map_err(|e| format!("Config lock poisoned: {}", e))
}

/// Execute config operation with automatic save on success
pub fn with_saved_config<F>(config: &Arc<Mutex<Config>>, f: F) -> Result<(), String>
where
    F: FnOnce(&mut Config),
{
    with_config_mut(config, |cfg| {
        f(cfg);
        cfg.save().map_err(|e| e.to_string())
    })?
}

/// Execute config operation that returns a value, with automatic save on success
pub fn with_saved_config_result<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut Config) -> Result<R, String>,
{
    with_config_mut(config, f)?
}

/// Execute config operation that returns (), with automatic save on success
pub fn with_saved_config_checked<F>(config: &Arc<Mutex<Config>>, f: F) -> Result<(), String>
where
    F: FnOnce(&mut Config) -> Result<(), String>,
{
    with_config_mut(config, |cfg| f(cfg))?
}

/// Parse theme string to Theme enum (kept for potential future use)
#[allow(dead_code)]
pub fn parse_theme(s: &str) -> Result<Theme, String> {
    match s.to_lowercase().as_str() {
        "dark" => Ok(Theme::Dark),
        "light" => Ok(Theme::Light),
        _ => Err(format!("Invalid theme '{}'. Use 'dark' or 'light'.", s)),
    }
}

/// Parse auto gain mode string
pub fn parse_auto_gain_mode(s: &str) -> Result<crate::config::AutoGainMode, String> {
    match s.to_lowercase().as_str() {
        "dynamic" => Ok(crate::config::AutoGainMode::Dynamic),
        "static" => Ok(crate::config::AutoGainMode::Static),
        _ => Err(format!(
            "Invalid auto gain mode '{}'. Use 'dynamic' or 'static'.",
            s
        )),
    }
}

/// Parse auto gain apply to string
pub fn parse_auto_gain_apply_to(s: &str) -> Result<crate::config::AutoGainApplyTo, String> {
    match s.to_lowercase().as_str() {
        "both" => Ok(crate::config::AutoGainApplyTo::Both),
        "mic_only" => Ok(crate::config::AutoGainApplyTo::MicOnly),
        _ => Err(format!(
            "Invalid auto gain apply-to '{}'. Use 'both' or 'mic_only'.",
            s
        )),
    }
}

/// Validate play mode string
pub fn validate_play_mode(s: &str) -> Result<PlayMode, String> {
    match s.to_lowercase().as_str() {
        "default" => Ok(PlayMode::Default),
        "loop" => Ok(PlayMode::Loop),
        "continue" => Ok(PlayMode::Continue),
        _ => Err(format!(
            "Invalid play mode '{}'. Use 'default', 'loop', or 'continue'.",
            s
        )),
    }
}

/// Number of threads for audio metadata analysis (duration/probe)
pub fn bounded_audio_analysis_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).clamp(1, 4))
        .unwrap_or(1)
}

pub(crate) fn build_sound_with_metadata(name: String, path: String) -> Sound {
    let mut sound = Sound::new(name, path);
    sound.duration_ms = probe_duration_ms(&sound.path);
    sound
}

pub(crate) fn probe_duration_ms(path: &str) -> Option<u64> {
    crate::audio::metadata::probe_duration_ms(path)
}

/// Get default sound import directory
pub fn default_sound_import_dir(
    audio_dir: Option<std::path::PathBuf>,
    home_dir: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    audio_dir
        .or_else(|| home_dir.map(|h| h.join("Music")))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("soundboard-imports")
}

#[cfg(test)]
mod tests {
    use super::bounded_audio_analysis_threads;

    #[test]
    fn test_bounded_audio_analysis_threads_never_exceeds_cap() {
        assert!((1..=4).contains(&bounded_audio_analysis_threads()));
    }
}
