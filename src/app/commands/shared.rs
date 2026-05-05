use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::UNIX_EPOCH,
};

use crate::config::{Config, PlayMode, Sound};
use crate::hotkeys::HotkeyManager;

const ASYNC_COMMAND_POLL_INTERVAL_MS: u64 = 10;
const SLOW_GTK_CALLBACK_THRESHOLD_MS: u128 = 16;
const ANALYSIS_RSS_ELEVATED_KB: u64 = 650 * 1024;
const ANALYSIS_RSS_HIGH_KB: u64 = 800 * 1024;
const ANALYSIS_RSS_CRITICAL_KB: u64 = 1_000 * 1024;
const ANALYSIS_THREAD_ELEVATED: u64 = 28;
const ANALYSIS_THREAD_HIGH: u64 = 36;
const ANALYSIS_THREAD_CRITICAL: u64 = 48;

pub const ERR_FILE_DOES_NOT_EXIST: &str = "File does not exist";
pub const ERR_UNSUPPORTED_AUDIO_FILE: &str = "Not a supported audio file";
pub const ERR_SOUND_ALREADY_EXISTS: &str = "Sound already exists";
pub const ERR_SOUND_NOT_FOUND: &str = "Sound not found";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdaptiveAnalysisPlan {
    pub threads: usize,
    pub base_threads: usize,
    pub rss_kb: Option<u64>,
    pub process_threads: Option<u64>,
    pub throttled: bool,
}

/// Lock config immutably and surface poison errors.
pub fn with_config<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&Config) -> R,
{
    config
        .lock()
        .map(|guard| f(&guard))
        .map_err(|e| format!("Config lock poisoned: {}", e))
}

/// Lock config mutably and surface poison errors.
pub fn with_config_mut<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut Config) -> R,
{
    config
        .lock()
        .map(|mut guard| f(&mut guard))
        .map_err(|e| format!("Config lock poisoned: {}", e))
}

/// Run a config mutation and save on success.
pub fn with_saved_config<F>(config: &Arc<Mutex<Config>>, f: F) -> Result<(), String>
where
    F: FnOnce(&mut Config),
{
    with_config_mut(config, |cfg| {
        f(cfg);
        cfg.save().map_err(|e| e.to_string())
    })?
}

/// Run a config mutation that returns a value and save on success.
pub fn with_saved_config_result<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut Config) -> Result<R, String>,
{
    with_config_mut(config, |cfg| {
        let result = f(cfg)?;
        cfg.save().map_err(|e| e.to_string())?;
        Ok(result)
    })?
}

/// Run a config mutation that returns `()` and save on success.
pub fn with_saved_config_checked<F>(config: &Arc<Mutex<Config>>, f: F) -> Result<(), String>
where
    F: FnOnce(&mut Config) -> Result<(), String>,
{
    with_saved_config_result(config, f)
}

/// Parse a theme string.
#[cfg(test)]
pub(crate) fn parse_theme(s: &str) -> Result<crate::config::Theme, String> {
    match s.to_lowercase().as_str() {
        "dark" => Ok(crate::config::Theme::Dark),
        "light" => Ok(crate::config::Theme::Light),
        _ => Err(format!("Invalid theme '{}'. Use 'dark' or 'light'.", s)),
    }
}

/// Parse auto-gain mode.
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

/// Parse auto-gain scope.
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

/// Parse play mode.
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

/// Cap metadata analysis threads.
pub fn bounded_audio_analysis_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).clamp(1, 4))
        .unwrap_or(1)
}

pub fn adaptive_audio_analysis_plan(item_count: usize) -> AdaptiveAnalysisPlan {
    let base_threads = bounded_audio_analysis_threads();
    let snapshot = crate::diagnostics::read_memory_snapshot();
    adaptive_audio_analysis_plan_with_snapshot(item_count, base_threads, snapshot)
}

fn adaptive_audio_analysis_plan_with_snapshot(
    item_count: usize,
    base_threads: usize,
    snapshot: Option<crate::diagnostics::MemorySnapshot>,
) -> AdaptiveAnalysisPlan {
    let mut threads = base_threads.min(item_count.max(1));
    threads = match item_count {
        0 | 1 => 1,
        2..=4 => threads.min(2),
        5..=8 => threads.min(3),
        _ => threads,
    };

    let rss_kb = snapshot.as_ref().and_then(|s| s.vm_rss_kb);
    let process_threads = snapshot.as_ref().and_then(|s| s.threads);

    let memory_limit = match (rss_kb.unwrap_or(0), process_threads.unwrap_or(0)) {
        (rss, thread_count)
            if rss >= ANALYSIS_RSS_CRITICAL_KB || thread_count >= ANALYSIS_THREAD_CRITICAL =>
        {
            Some(1)
        }
        (rss, thread_count)
            if rss >= ANALYSIS_RSS_HIGH_KB || thread_count >= ANALYSIS_THREAD_HIGH =>
        {
            Some(2)
        }
        (rss, thread_count)
            if rss >= ANALYSIS_RSS_ELEVATED_KB || thread_count >= ANALYSIS_THREAD_ELEVATED =>
        {
            Some(3)
        }
        _ => None,
    };

    let mut throttled = false;
    if let Some(limit) = memory_limit {
        if threads > limit {
            threads = limit;
            throttled = true;
        }
    }

    AdaptiveAnalysisPlan {
        threads: threads.max(1),
        base_threads,
        rss_kb,
        process_threads,
        throttled,
    }
}

pub(crate) fn compute_sound_source_fingerprint(
    path: &str,
    duration_ms: Option<u64>,
) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;

    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    metadata.len().hash(&mut hasher);
    modified.as_secs().hash(&mut hasher);
    modified.subsec_nanos().hash(&mut hasher);
    duration_ms.unwrap_or(0).hash(&mut hasher);

    Some(format!("v1:{:016x}", hasher.finish()))
}

pub(crate) fn build_sound_with_metadata(name: String, path: String) -> Sound {
    let mut sound = Sound::new(name, path);
    sound.duration_ms = probe_duration_ms(&sound.path);
    sound.loudness_source_fingerprint =
        compute_sound_source_fingerprint(&sound.path, sound.duration_ms);
    sound
}

pub(crate) fn probe_duration_ms(path: &str) -> Option<u64> {
    crate::audio::metadata::probe_duration_ms(path)
}

/// Resolve the default import directory.
pub fn default_sound_import_dir(
    audio_dir: Option<std::path::PathBuf>,
    home_dir: Option<std::path::PathBuf>,
) -> std::path::PathBuf {
    audio_dir
        .or_else(|| home_dir.map(|h| h.join("Music")))
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("soundboard-imports")
}

/// Best-effort hotkey unregister for sound removals.
pub fn unregister_hotkeys_best_effort(
    hotkeys: &Arc<Mutex<HotkeyManager>>,
    sound_ids: &[String],
    context: &'static str,
) {
    if sound_ids.is_empty() {
        return;
    }

    if let Ok(mut manager) = hotkeys.lock() {
        let _ = manager.unregister_hotkeys_blocking(sound_ids);
    } else {
        log::warn!(
            "Hotkeys lock poisoned during {}; skipping unregister for {} sounds",
            context,
            sound_ids.len()
        );
    }
}

pub fn dispatch_async_result<T, F, C>(
    task_name: &'static str,
    task: F,
    on_complete: C,
) -> Result<(), String>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    C: FnOnce(T) + 'static,
{
    let (result_tx, result_rx) = mpsc::channel::<(T, Duration)>();
    log::debug!("Async UI command started: name={}", task_name);

    std::thread::Builder::new()
        .name(format!("ui-{task_name}"))
        .spawn(move || {
            let started_at = Instant::now();
            let result = task();
            let _ = result_tx.send((result, started_at.elapsed()));
        })
        .map_err(|e| format!("Failed to spawn async UI command '{task_name}': {e}"))?;

    let mut on_complete = Some(on_complete);
    glib::timeout_add_local(
        Duration::from_millis(ASYNC_COMMAND_POLL_INTERVAL_MS),
        move || match result_rx.try_recv() {
            Ok((result, elapsed)) => {
                log::debug!(
                    "Async UI command finished: name={} elapsed_ms={}",
                    task_name,
                    elapsed.as_millis()
                );
                if let Some(on_complete) = on_complete.take() {
                    let callback_started_at = Instant::now();
                    on_complete(result);
                    let callback_elapsed = callback_started_at.elapsed().as_millis();
                    if callback_elapsed >= SLOW_GTK_CALLBACK_THRESHOLD_MS {
                        log::debug!(
                            "GTK callback latency exceeded threshold: name={} elapsed_ms={}",
                            task_name,
                            callback_elapsed
                        );
                    }
                }
                glib::ControlFlow::Break
            }
            Err(TryRecvError::Empty) => glib::ControlFlow::Continue,
            Err(TryRecvError::Disconnected) => glib::ControlFlow::Break,
        },
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        adaptive_audio_analysis_plan_with_snapshot, bounded_audio_analysis_threads,
        ANALYSIS_RSS_CRITICAL_KB, ANALYSIS_RSS_ELEVATED_KB, ANALYSIS_RSS_HIGH_KB,
        ANALYSIS_THREAD_CRITICAL, ANALYSIS_THREAD_ELEVATED, ANALYSIS_THREAD_HIGH,
    };

    #[test]
    fn test_bounded_audio_analysis_threads_never_exceeds_cap() {
        assert!((1..=4).contains(&bounded_audio_analysis_threads()));
    }

    #[test]
    fn test_adaptive_analysis_plan_scales_with_small_workloads() {
        let plan = adaptive_audio_analysis_plan_with_snapshot(2, 4, None);
        assert_eq!(plan.threads, 2);
        assert!(!plan.throttled);

        let plan = adaptive_audio_analysis_plan_with_snapshot(0, 4, None);
        assert_eq!(plan.threads, 1);
    }

    #[test]
    fn test_adaptive_analysis_plan_throttles_under_elevated_pressure() {
        let snapshot = crate::diagnostics::MemorySnapshot {
            vm_rss_kb: Some(ANALYSIS_RSS_ELEVATED_KB),
            threads: Some(ANALYSIS_THREAD_ELEVATED),
            ..Default::default()
        };
        let plan = adaptive_audio_analysis_plan_with_snapshot(64, 4, Some(snapshot));

        assert_eq!(plan.threads, 3);
        assert!(plan.throttled);
    }

    #[test]
    fn test_adaptive_analysis_plan_throttles_under_high_pressure() {
        let snapshot = crate::diagnostics::MemorySnapshot {
            vm_rss_kb: Some(ANALYSIS_RSS_HIGH_KB),
            threads: Some(ANALYSIS_THREAD_HIGH),
            ..Default::default()
        };
        let plan = adaptive_audio_analysis_plan_with_snapshot(64, 4, Some(snapshot));

        assert_eq!(plan.threads, 2);
        assert!(plan.throttled);
    }

    #[test]
    fn test_adaptive_analysis_plan_throttles_to_single_thread_under_critical_pressure() {
        let snapshot = crate::diagnostics::MemorySnapshot {
            vm_rss_kb: Some(ANALYSIS_RSS_CRITICAL_KB),
            threads: Some(ANALYSIS_THREAD_CRITICAL),
            ..Default::default()
        };
        let plan = adaptive_audio_analysis_plan_with_snapshot(64, 4, Some(snapshot));

        assert_eq!(plan.threads, 1);
        assert!(plan.throttled);
    }
}
