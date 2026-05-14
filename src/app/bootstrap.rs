use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::*;
use gtk4::{Application, Window};
use libadwaita as adw;
use log::{info, warn};

use crate::app_meta::{
    APP_BINARY, APP_ICON_NAME, APP_ID, APP_TITLE, BACKEND_ENV_VAR, FALLBACK_RENDERER,
    FORCE_X11_ENV_VAR, RENDERER_ENV_VAR, WAYLAND_BACKEND, X11_BACKEND,
};
use crate::app_state::AppState;
use crate::config::{Config, ControlHotkeyAction};
use crate::timer_registry::TimerRegistry;

pub fn run() {
    init_logging();
    if std::env::args().any(|arg| arg == "--audio-engine") {
        std::process::exit(crate::audio_engine::run());
    }
    if std::env::args().any(|arg| arg == "--diagnose") {
        std::process::exit(crate::diagnostics::routing::run());
    }

    configure_preferred_backend();
    configure_preferred_renderer();
    glib::set_prgname(Some(APP_BINARY));
    glib::set_application_name(APP_TITLE);

    info!("Starting Linux Soundboard (GTK4)");

    gtk4::init().expect("Failed to initialize GTK4");
    normalize_legacy_dark_theme_setting();
    adw::init().expect("Failed to initialize libadwaita");
    Window::set_default_icon_name(APP_ICON_NAME);

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_activate_handler());
    app.run();
}

fn init_logging() {
    let env = env_logger::Env::default().default_filter_or(
        "warn,\
linux_soundboard::audio_engine=info,\
linux_soundboard::init::audio=info,\
linux_soundboard::audio::player=info,\
linux_soundboard::audio::player::source_routing=info",
    );
    env_logger::Builder::from_env(env).init();
}

fn normalize_legacy_dark_theme_setting() {
    if let Some(settings) = gtk4::Settings::default() {
        if settings.is_gtk_application_prefer_dark_theme() {
            info!(
                "Disabling legacy GtkSettings:gtk-application-prefer-dark-theme in favor of AdwStyleManager"
            );
            settings.set_gtk_application_prefer_dark_theme(false);
        }
    }
}

fn configure_preferred_backend() {
    let previous = std::env::var(BACKEND_ENV_VAR).ok();
    if previous.is_some() {
        info!(
            "Keeping GTK backend unchanged because {} is already set: {:?}",
            BACKEND_ENV_VAR, previous
        );
        return;
    }

    let has_wayland = std::env::var("WAYLAND_DISPLAY").is_ok();
    let has_x11 = std::env::var("DISPLAY").is_ok();
    let force_x11 = std::env::var(FORCE_X11_ENV_VAR)
        .ok()
        .map(|v| {
            let normalized = v.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
        })
        .unwrap_or(false);

    if force_x11 {
        if has_x11 {
            info!(
                "{} requested; forcing GTK X11 via {}={}",
                FORCE_X11_ENV_VAR, BACKEND_ENV_VAR, X11_BACKEND
            );
            std::env::set_var(BACKEND_ENV_VAR, X11_BACKEND);
            return;
        }

        warn!(
            "{} is set but DISPLAY is unavailable; cannot force GTK X11 backend",
            FORCE_X11_ENV_VAR
        );
    }

    if has_wayland {
        info!(
            "Wayland display detected; preferring native GTK Wayland via {}={}",
            BACKEND_ENV_VAR, WAYLAND_BACKEND
        );
        std::env::set_var(BACKEND_ENV_VAR, WAYLAND_BACKEND);
    } else if has_x11 {
        info!(
            "Wayland unavailable; using GTK X11 fallback via {}={}",
            BACKEND_ENV_VAR, X11_BACKEND
        );
        std::env::set_var(BACKEND_ENV_VAR, X11_BACKEND);
    }
}

fn configure_preferred_renderer() {
    let previous = std::env::var(RENDERER_ENV_VAR).ok();
    if previous.is_some() {
        info!(
            "Keeping GTK renderer unchanged because {} is already set: {:?}",
            RENDERER_ENV_VAR, previous
        );
        return;
    }

    if !running_in_vmware_guest() {
        return;
    }

    info!(
        "VMware guest detected; forcing safer GTK renderer via {}={}",
        RENDERER_ENV_VAR, FALLBACK_RENDERER
    );
    std::env::set_var(RENDERER_ENV_VAR, FALLBACK_RENDERER);
}

fn running_in_vmware_guest() -> bool {
    const DMI_PATHS: &[&str] = &[
        "/sys/class/dmi/id/product_name",
        "/sys/class/dmi/id/product_version",
        "/sys/class/dmi/id/sys_vendor",
        "/sys/class/dmi/id/board_vendor",
    ];

    DMI_PATHS.iter().any(|path| {
        std::fs::read_to_string(path)
            .map(|value| value.to_ascii_lowercase().contains("vmware"))
            .unwrap_or(false)
    })
}

fn build_activate_handler() -> impl Fn(&Application) + 'static {
    move |app| {
        normalize_legacy_dark_theme_setting();

        if let Some(display) = gtk4::gdk::Display::default() {
            info!("GTK display backend: {:?}", display.backend());
        } else {
            warn!("GTK display backend is unavailable during activation");
        }

        let mut config = load_config();
        crate::diagnostics::memory::log_memory_snapshot("startup:config_loaded");
        crate::diagnostics::record_phase_with_config("startup:config_loaded", &config);

        let cleaned_count = cleanup_stale_tmp_sounds(&mut config);
        if cleaned_count > 0 {
            if let Err(e) = config.save() {
                log::warn!("Failed to save config after cleanup: {}", e);
            }
        }

        let prebound_hotkeys = prebound_hotkeys(&config);
        let (hotkey_sender, hotkey_receiver) = mpsc::channel::<String>();

        let hotkey_manager =
            crate::hotkeys::HotkeyManager::new_blocking(hotkey_sender, &prebound_hotkeys);
        crate::diagnostics::set_hotkey_status(&hotkey_manager.status_message());
        crate::diagnostics::memory::log_memory_snapshot("startup:hotkeys_ready");
        crate::diagnostics::record_phase_with_config("startup:hotkeys_ready", &config);

        let player = initialize_player(&config);
        crate::diagnostics::set_playback_registry_count(0);
        crate::diagnostics::memory::log_memory_snapshot("startup:player_initialized");
        crate::diagnostics::record_phase_with_config("startup:player_initialized", &config);

        let pipewire_status = crate::pipewire::detection::check_pipewire();

        let state = Arc::new(AppState {
            config: Arc::new(Mutex::new(config)),
            player: Arc::new(player),
            hotkeys: Arc::new(Mutex::new(hotkey_manager)),
            pipewire_status: Arc::new(Mutex::new(pipewire_status)),
        });

        let timer_registry = TimerRegistry::new();

        let (window, transport) =
            crate::ui::app_window::build_window(app, Arc::clone(&state), &timer_registry);
        crate::diagnostics::set_validation_runtime(0, "deferred", 0);
        crate::diagnostics::memory::log_memory_snapshot("startup:window_built");
        record_state_phase("startup:window_built", &state);

        let state_hk = Arc::clone(&state);
        let window_hk = window.clone();
        let transport_hk = transport.clone();
        crate::ui_event_bridge::set_hotkey_handler(move |sound_id| {
            crate::ui::app_window::handle_hotkey(&window_hk, &state_hk, &transport_hk, &sound_id);
        });
        if let Err(err) = thread::Builder::new()
            .name("hotkey-ui-bridge".to_string())
            .spawn(move || {
                while let Ok(sound_id) = hotkey_receiver.recv() {
                    crate::ui_event_bridge::post_hotkey(sound_id);
                }
            })
        {
            warn!("Failed to start hotkey UI bridge: {}", err);
        }

        let state_close = Arc::clone(&state);
        let timers_close = timer_registry.clone();
        window.connect_close_request(move |_| {
            timers_close.remove_all();
            crate::diagnostics::set_timer_count(0);
            crate::diagnostics::set_playback_registry_count(0);
            record_state_phase("shutdown:close_request", &state_close);
            state_close.player.stop_all();
            state_close.player.shutdown();
            if let Err(e) = crate::diagnostics::write_memory_report() {
                log::warn!("Failed to write memory report: {}", e);
            }
            glib::Propagation::Proceed
        });

        window.present();
        record_state_phase("startup:window_presented", &state);

        schedule_startup_loudness_backfill(Arc::clone(&state), &timer_registry);

        {
            let state_idle = Arc::clone(&state);
            glib::timeout_add_local_once(Duration::from_secs(5), move || {
                record_state_phase("idle:5s", &state_idle);
            });
        }
    }
}

fn load_config() -> Config {
    match Config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            log::error!(
                "Failed to load config from '{}': {}. Starting with defaults.",
                Config::config_path().display(),
                e
            );
            Config::default()
        }
    }
}

fn record_config_phase(name: &str, config: &Arc<Mutex<Config>>) {
    match config.lock() {
        Ok(cfg) => crate::diagnostics::record_phase_with_config(name, &cfg),
        Err(e) => {
            log::warn!(
                "Config lock poisoned while recording phase '{}': {}",
                name,
                e
            );
            crate::diagnostics::record_phase(name, None);
        }
    }
}

fn record_state_phase(name: &str, state: &Arc<AppState>) {
    record_config_phase(name, &state.config);
}

fn schedule_startup_loudness_backfill(state: Arc<AppState>, _timer_registry: &TimerRegistry) {
    glib::idle_add_local_once(move || {
        crate::diagnostics::memory::log_memory_snapshot("startup:loudness_bg:check");
        let (auto_gain_enabled, missing_count, phase_recorded) = state
            .config
            .lock()
            .map(|cfg| {
                let missing_count = cfg
                    .sounds
                    .iter()
                    .filter(|sound| sound.loudness_lufs.is_none())
                    .count();
                crate::diagnostics::record_phase_with_config("startup:loudness_check", &cfg);
                (cfg.settings.auto_gain, missing_count, true)
            })
            .unwrap_or((false, 0, false));
        if !phase_recorded {
            crate::diagnostics::record_phase("startup:loudness_check", None);
        }

        if !auto_gain_enabled || missing_count == 0 {
            return;
        }

        log::info!(
            "Deferring startup loudness analysis: {} sounds missing LUFS",
            missing_count
        );
        crate::diagnostics::record_phase("startup:loudness_bg:deferred", None);
    });
}

fn cleanup_stale_tmp_sounds(config: &mut Config) -> usize {
    use std::path::Path;

    let sounds_to_remove: Vec<String> = config
        .sounds
        .iter()
        .filter(|sound| {
            let effective_path = sound.source_path.as_ref().unwrap_or(&sound.path);
            let path = Path::new(effective_path);
            path.starts_with("/tmp") && !path.exists()
        })
        .map(|s| s.id.clone())
        .collect();

    let count = sounds_to_remove.len();
    if count > 0 {
        info!("Cleaning up {} stale sound(s) from /tmp", count);
        config.remove_sounds(&sounds_to_remove);
    }

    count
}

pub fn backfill_missing_sound_durations(config: &mut Config) -> bool {
    let mut changed = false;

    for sound in &mut config.sounds {
        if sound.duration_ms.is_some() {
            continue;
        }
        if let Some(duration_ms) = crate::commands::probe_duration_ms(&sound.path) {
            sound.duration_ms = Some(duration_ms);
            changed = true;
        }
    }

    changed
}

fn prebound_hotkeys(config: &Config) -> Vec<(String, String)> {
    let mut prebound: Vec<(String, String)> = config
        .sounds
        .iter()
        .filter_map(|sound| {
            sound
                .hotkey
                .as_ref()
                .map(|hotkey| (sound.id.clone(), hotkey.clone()))
        })
        .collect();

    for meta in ControlHotkeyAction::all() {
        if let Some(hotkey) = config.settings.control_hotkeys.get_cloned(meta.action) {
            prebound.push((meta.action.binding_id().to_string(), hotkey));
        }
    }

    prebound
}

fn initialize_player(config: &Config) -> crate::audio::player::AudioPlayer {
    use crate::audio::player::AudioBackendKind;

    if let Some(player) = crate::audio::player::AudioPlayer::connect_to_engine() {
        log::info!("Connected UI to existing Linux Soundboard audio engine");
        return player;
    }
    crate::audio::engine_ipc::shutdown_incompatible_engine_if_running();
    ensure_audio_engine_service_started();
    if let Some(player) = crate::audio::player::AudioPlayer::connect_to_engine() {
        log::info!("Started and connected UI to Linux Soundboard audio engine");
        return player;
    }

    let backend = if crate::pipewire::detection::check_pipewire().available {
        AudioBackendKind::PipeWire
    } else {
        AudioBackendKind::PulseAudio
    };
    crate::audio::player::AudioPlayer::new_with_config_and_audio_backend(config, backend)
}

fn ensure_audio_engine_service_started() {
    let service = "linux-soundboard-engine.service";
    let status = std::process::Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", service])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if matches!(status, Ok(status) if status.success()) {
        return;
    }

    ensure_user_audio_engine_service_file(service);

    let status = std::process::Command::new("systemctl")
        .args(["--user", "enable", "--now", service])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !matches!(status, Ok(status) if status.success()) {
        return;
    }

    let started_at = std::time::Instant::now();
    while started_at.elapsed() < Duration::from_secs(2) {
        if crate::audio::engine_ipc::compatible_engine_running() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn ensure_user_audio_engine_service_file(service: &str) {
    let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
    else {
        return;
    };
    let service_path = config_home.join("systemd").join("user").join(service);
    if service_path.exists()
        || systemd_user_unit_exists(service)
        || packaged_audio_engine_service_exists(service)
    {
        return;
    }

    let executable = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok());
    let Some(executable) = executable else {
        return;
    };
    let Some(parent) = service_path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    let body = render_audio_engine_service(&executable);
    if std::fs::write(&service_path, body).is_err() {
        return;
    }
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn packaged_audio_engine_service_exists(service: &str) -> bool {
    [
        PathBuf::from("/etc/systemd/user").join(service),
        PathBuf::from("/usr/local/share/systemd/user").join(service),
        PathBuf::from("/usr/share/systemd/user").join(service),
        PathBuf::from("/usr/local/lib/systemd/user").join(service),
        PathBuf::from("/usr/lib/systemd/user").join(service),
        PathBuf::from("/usr/lib64/systemd/user").join(service),
        PathBuf::from("/lib/systemd/user").join(service),
    ]
    .iter()
    .any(|path| path.exists())
}

fn systemd_user_unit_exists(service: &str) -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "cat", service])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn render_audio_engine_service(executable: &std::path::Path) -> String {
    format!(
        "[Unit]\n\
Description=Linux Soundboard audio engine\n\
After=pipewire.service pipewire-pulse.service wireplumber.service pulseaudio.service\n\
\n\
[Service]\n\
Type=simple\n\
ExecStart={} --audio-engine\n\
Restart=on-failure\n\
RestartSec=2\n\
\n\
[Install]\n\
WantedBy=default.target\n",
        systemd_quote(executable)
    )
}

fn systemd_quote(path: &std::path::Path) -> String {
    let raw = path.to_string_lossy();
    let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Sound;
    use std::fs;
    use std::path::{Path, PathBuf};

    fn build_test_wave_payload() -> Vec<u8> {
        let sample_rate = 44_100_u32;
        let channels = 2_u16;
        let bits_per_sample = 16_u16;
        let sample_count = sample_rate / 5;
        let bytes_per_sample = (bits_per_sample / 8) as usize;
        let block_align = channels as usize * bytes_per_sample;
        let byte_rate = sample_rate as usize * block_align;
        let mut pcm = Vec::with_capacity(sample_count as usize * block_align);

        for frame in 0..sample_count {
            let phase = 2.0_f32 * std::f32::consts::PI * 440.0 * frame as f32 / sample_rate as f32;
            let sample = (phase.sin() * 12_000.0) as i16;
            for _ in 0..channels {
                pcm.extend_from_slice(&sample.to_le_bytes());
            }
        }

        let data_len = pcm.len() as u32;
        let riff_len = 36 + data_len;

        let mut bytes = Vec::with_capacity(44 + pcm.len());
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&riff_len.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(byte_rate as u32).to_le_bytes());
        bytes.extend_from_slice(&(block_align as u16).to_le_bytes());
        bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        bytes.extend_from_slice(&pcm);
        bytes
    }

    fn create_test_audio_file(ext: &str) -> PathBuf {
        let base =
            std::env::temp_dir().join(format!("lsb-bootstrap-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).expect("create temp audio dir");
        let path = base.join(format!("tone.{}", ext));
        fs::write(&path, build_test_wave_payload()).expect("write test audio payload");
        path
    }

    fn cleanup_test_audio_path(path: &Path) {
        let _ = fs::remove_file(path);
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn startup_duration_backfill_fills_missing_and_preserves_existing() {
        let audio_path = create_test_audio_file("wav");
        let mut cfg = Config::default();

        let mut missing = Sound::new(
            "Missing".to_string(),
            audio_path.to_string_lossy().to_string(),
        );
        missing.duration_ms = None;

        let mut existing = Sound::new("Existing".to_string(), "/tmp/existing.wav".to_string());
        existing.duration_ms = Some(1234);

        let missing_file = Sound::new(
            "Missing File".to_string(),
            "/tmp/does-not-exist.wav".to_string(),
        );

        cfg.sounds.push(missing);
        cfg.sounds.push(existing);
        cfg.sounds.push(missing_file);

        let changed = backfill_missing_sound_durations(&mut cfg);

        assert!(changed);
        assert!(cfg.sounds[0].duration_ms.is_some());
        assert_eq!(cfg.sounds[1].duration_ms, Some(1234));
        assert_eq!(cfg.sounds[2].duration_ms, None);

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn audio_engine_service_renders_quoted_exec() {
        let service = render_audio_engine_service(Path::new("/tmp/Linux Soundboard.AppImage"));
        assert!(service.contains("ExecStart=\"/tmp/Linux Soundboard.AppImage\" --audio-engine"));
        assert!(service.contains(
            "After=pipewire.service pipewire-pulse.service wireplumber.service pulseaudio.service"
        ));
        assert!(service.contains("WantedBy=default.target"));
    }
}
