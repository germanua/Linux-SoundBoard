use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::get_probe;

use crate::audio::loudness;
use crate::config::{AutoGainApplyTo, AutoGainMode, Config, PlayMode, Sound, Theme};

pub(crate) fn with_saved_config<F>(config: &Arc<Mutex<Config>>, apply: F) -> Result<(), String>
where
    F: FnOnce(&mut Config),
{
    let mut cfg = config.lock().unwrap();
    let snapshot = cfg.clone();
    apply(&mut cfg);
    if let Err(e) = cfg.save() {
        *cfg = snapshot;
        return Err(e.to_string());
    }
    Ok(())
}

pub(crate) fn with_saved_config_result<T, F>(
    config: &Arc<Mutex<Config>>,
    apply: F,
) -> Result<T, String>
where
    F: FnOnce(&mut Config) -> T,
{
    let mut cfg = config.lock().unwrap();
    let snapshot = cfg.clone();
    let result = apply(&mut cfg);
    if let Err(e) = cfg.save() {
        *cfg = snapshot;
        return Err(e.to_string());
    }
    Ok(result)
}

pub(crate) fn with_saved_config_checked<T, F>(
    config: &Arc<Mutex<Config>>,
    apply: F,
) -> Result<T, String>
where
    F: FnOnce(&mut Config) -> Result<T, String>,
{
    let mut cfg = config.lock().unwrap();
    let snapshot = cfg.clone();
    let result = match apply(&mut cfg) {
        Ok(value) => value,
        Err(e) => {
            *cfg = snapshot;
            return Err(e);
        }
    };
    if let Err(e) = cfg.save() {
        *cfg = snapshot;
        return Err(e.to_string());
    }
    Ok(result)
}

pub(crate) fn parse_auto_gain_mode(mode: &str) -> Result<AutoGainMode, String> {
    AutoGainMode::from_str(mode)
        .map_err(|_| "Invalid auto-gain mode. Use 'static' or 'dynamic'.".to_string())
}

pub(crate) fn parse_auto_gain_apply_to(scope: &str) -> Result<AutoGainApplyTo, String> {
    AutoGainApplyTo::from_str(scope)
        .map_err(|_| "Invalid auto-gain apply scope. Use 'mic_only' or 'both'.".to_string())
}

pub(crate) fn parse_theme(theme: &str) -> Result<Theme, String> {
    Theme::from_str(theme).map_err(|_| "Invalid theme. Use 'dark' or 'light'.".to_string())
}

pub(crate) fn validate_play_mode(mode: &str) -> Result<PlayMode, String> {
    PlayMode::from_str(mode).map_err(|_| {
        format!(
            "Invalid play mode: {}. Must be one of: default, loop, continue",
            mode
        )
    })
}

pub(crate) fn bounded_audio_analysis_threads() -> usize {
    const MAX_AUDIO_ANALYSIS_THREADS: usize = 1;
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(1, MAX_AUDIO_ANALYSIS_THREADS)
}

pub(crate) fn build_sound_with_metadata(name: String, path: String) -> Sound {
    const FAST_LOUDNESS_PREVIEW_MS: u32 = 5000;

    let mut sound = Sound::new(name, path);
    crate::diagnostics::memory::log_memory_snapshot(&format!("metadata:begin:{}", sound.path));
    let duration_ms = probe_duration_ms(&sound.path);
    sound.duration_ms = duration_ms;
    match loudness::analyze_loudness_path_preview_smart(
        Path::new(&sound.path),
        FAST_LOUDNESS_PREVIEW_MS,
        duration_ms,
    ) {
        Ok(lufs) => {
            if lufs.is_finite() {
                sound.loudness_lufs = Some(lufs);
            } else {
                log::warn!(
                    "Ignoring non-finite loudness preview for '{}' [{}]",
                    sound.name,
                    sound.path
                );
                sound.loudness_lufs = None;
            }
        }
        Err(e) => log::warn!(
            "Failed to analyze loudness preview for '{}': {}",
            sound.path,
            e
        ),
    }
    crate::diagnostics::memory::log_memory_snapshot(&format!("metadata:end:{}", sound.path));
    sound
}

pub(crate) fn probe_duration_ms(path: &str) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let track = probed.format.default_track()?;
    let params = &track.codec_params;
    if let (Some(tb), Some(n_frames)) = (params.time_base, params.n_frames) {
        let time = tb.calc_time(n_frames);
        let ms = time.seconds.saturating_mul(1000);
        if ms > 0 {
            return Some(ms);
        }
    }
    if let (Some(n_frames), Some(sr)) = (params.n_frames, params.sample_rate) {
        if sr > 0 {
            return Some(((n_frames as u128) * 1000 / (sr as u128)) as u64);
        }
    }
    None
}

pub(crate) fn default_sound_import_dir(
    audio_dir: Option<PathBuf>,
    home_dir: Option<PathBuf>,
) -> PathBuf {
    audio_dir
        .or_else(|| home_dir.map(|h| h.join("Music")))
        .map(|p| p.join(crate::app_meta::DEFAULT_IMPORT_DIR_NAME))
        .unwrap_or_else(|| PathBuf::from(crate::app_meta::FALLBACK_IMPORT_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sound_import_dir_prefers_audio_dir() {
        let actual = default_sound_import_dir(
            Some(PathBuf::from("/home/test/Audio")),
            Some(PathBuf::from("/home/test")),
        );
        assert_eq!(actual, PathBuf::from("/home/test/Audio/linux-soundboard"));
    }

    #[test]
    fn default_sound_import_dir_falls_back_to_music() {
        let actual = default_sound_import_dir(None, Some(PathBuf::from("/home/test")));
        assert_eq!(actual, PathBuf::from("/home/test/Music/linux-soundboard"));
    }
}
