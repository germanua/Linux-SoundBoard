use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    time::UNIX_EPOCH,
};

use crate::config::{Config, PlayMode, Sound};
use crate::hotkeys::HotkeyManager;

const SLOW_GTK_CALLBACK_THRESHOLD_MS: u128 = 16;
const UI_WORKER_THREADS: usize = 4;
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

type PendingCallback = Box<dyn FnOnce(Box<dyn Any + Send>) + 'static>;

thread_local! {
    static PENDING_UI_CALLBACKS: RefCell<HashMap<u64, PendingCallback>> =
        RefCell::new(HashMap::new());
}

static NEXT_DISPATCH_ID: AtomicU64 = AtomicU64::new(0);

fn ui_worker_pool() -> &'static rayon::ThreadPool {
    static POOL: OnceLock<rayon::ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(UI_WORKER_THREADS)
            .thread_name(|i| format!("ui-worker-{i}"))
            .build()
            .expect("failed to build UI worker thread pool")
    })
}

/// Dispatch a Send task on a worker thread and run `on_complete` on the GTK main thread
/// when it finishes. Completion is event-driven via `MainContext::invoke` — there is no
/// per-call polling timer and no per-call OS thread.
///
/// Must be called from the GTK main thread (the thread that owns the default `MainContext`),
/// because `on_complete` is `!Send` and is stashed in a thread-local until the result lands.
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
    let dispatch_id = NEXT_DISPATCH_ID.fetch_add(1, Ordering::Relaxed);

    PENDING_UI_CALLBACKS.with(|m| {
        m.borrow_mut().insert(
            dispatch_id,
            Box::new(move |any: Box<dyn Any + Send>| match any.downcast::<T>() {
                Ok(value) => on_complete(*value),
                Err(_) => log::error!(
                    "dispatch_async_result type mismatch: name={} id={}",
                    task_name,
                    dispatch_id
                ),
            }),
        );
    });

    log::debug!("Async UI command started: name={}", task_name);

    ui_worker_pool().spawn(move || {
        let started_at = Instant::now();
        let result = task();
        let elapsed = started_at.elapsed();
        let result_any: Box<dyn Any + Send> = Box::new(result);
        glib::MainContext::default().invoke(move || {
            log::debug!(
                "Async UI command finished: name={} elapsed_ms={}",
                task_name,
                elapsed.as_millis()
            );
            let cb_started_at = Instant::now();
            PENDING_UI_CALLBACKS.with(|m| {
                if let Some(cb) = m.borrow_mut().remove(&dispatch_id) {
                    cb(result_any);
                }
            });
            let cb_elapsed = cb_started_at.elapsed().as_millis();
            if cb_elapsed >= SLOW_GTK_CALLBACK_THRESHOLD_MS {
                log::debug!(
                    "GTK callback latency exceeded threshold: name={} elapsed_ms={}",
                    task_name,
                    cb_elapsed
                );
            }
        });
    });

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
