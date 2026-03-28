//! Application bootstrap and startup wiring.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::*;
use gtk4::{Application, Window};
use libadwaita as adw;
use log::{info, warn};

use crate::app_meta::{
    APP_BINARY, APP_ID, APP_TITLE, BACKEND_ENV_VAR, HOTKEY_POLL_INTERVAL_MS, WAYLAND_BACKEND,
    X11_BACKEND,
};
use crate::app_state::AppState;
use crate::config::{Config, ControlHotkeyAction};

pub fn run() {
    env_logger::init();
    configure_preferred_backend();
    glib::set_prgname(Some(APP_BINARY));
    glib::set_application_name(APP_TITLE);

    info!("Starting Linux Soundboard (GTK4)");

    gtk4::init().expect("Failed to initialize GTK4");
    normalize_legacy_dark_theme_setting();
    adw::init().expect("Failed to initialize libadwaita");
    Window::set_default_icon_name(APP_ID);

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_activate_handler());
    app.run();
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

fn build_activate_handler() -> impl Fn(&Application) + 'static {
    move |app| {
        normalize_legacy_dark_theme_setting();

        if let Some(display) = gtk4::gdk::Display::default() {
            info!("GTK display backend: {:?}", display.backend());
        } else {
            warn!("GTK display backend is unavailable during activation");
        }

        let config = load_config();
        crate::diagnostics::memory::log_memory_snapshot("startup:config_loaded");
        crate::diagnostics::record_phase_with_config("startup:config_loaded", &config);

        let mic_status = setup_virtual_microphone(&config);
        crate::diagnostics::memory::log_memory_snapshot("startup:virtual_mic_ready");
        crate::diagnostics::record_phase_with_config("startup:virtual_mic_ready", &config);

        let prebound_hotkeys = prebound_hotkeys(&config);
        let (hotkey_sender, hotkey_receiver) = mpsc::channel::<String>();
        let hotkey_receiver = Arc::new(Mutex::new(hotkey_receiver));

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
            player: Arc::new(Mutex::new(player)),
            hotkeys: Arc::new(Mutex::new(hotkey_manager)),
            mic_status: Arc::new(Mutex::new(mic_status)),
            pipewire_status: Arc::new(Mutex::new(pipewire_status)),
        });

        let (window, transport) = crate::ui::app_window::build_window(app, Arc::clone(&state));
        crate::diagnostics::set_validation_runtime(0, "deferred", 0);
        crate::diagnostics::memory::log_memory_snapshot("startup:window_built");
        record_state_phase("startup:window_built", &state);

        let state_hk = Arc::clone(&state);
        let window_hk = window.clone();
        let hotkey_timer_id = Rc::new(RefCell::new(Some(glib::timeout_add_local(
            Duration::from_millis(HOTKEY_POLL_INTERVAL_MS),
            move || {
                // Use try_lock to avoid blocking and handle poison gracefully
                if let Ok(guard) = hotkey_receiver.try_lock() {
                    while let Ok(sound_id) = guard.try_recv() {
                        crate::ui::app_window::handle_hotkey(
                            &window_hk, &state_hk, &transport, &sound_id,
                        );
                    }
                } else {
                    // Lock is held elsewhere or poisoned - skip this poll cycle
                    warn!("Hotkey receiver lock unavailable, skipping poll");
                }
                glib::ControlFlow::Continue
            },
        ))));

        let state_close = Arc::clone(&state);
        let hotkey_timer_close = Rc::clone(&hotkey_timer_id);
        crate::diagnostics::set_timer_count(4);
        window.connect_close_request(move |_| {
            if let Some(source_id) = hotkey_timer_close.borrow_mut().take() {
                source_id.remove();
            }
            crate::diagnostics::set_timer_count(0);
            crate::diagnostics::set_playback_registry_count(0);
            record_state_phase("shutdown:close_request", &state_close);
            if let Err(e) = crate::diagnostics::write_memory_report() {
                log::warn!("Failed to write memory report: {}", e);
            }
            glib::Propagation::Proceed
        });

        window.present();
        record_state_phase("startup:window_presented", &state);

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

fn setup_virtual_microphone(config: &Config) -> crate::pipewire::virtual_mic::VirtualMicStatus {
    info!("Setting up virtual microphone...");
    let mic_status = crate::pipewire::virtual_mic::create_virtual_mic();
    if mic_status.active {
        info!("Virtual microphone created");
        if config.settings.mic_passthrough {
            if let Err(e) = crate::pipewire::virtual_mic::enable_mic_passthrough_with_source(
                config.settings.mic_source.clone(),
            ) {
                log::warn!("Failed to enable mic passthrough: {}", e);
            }
        }
    }
    mic_status
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
    let initial_local_volume = if config.settings.local_mute {
        0.0
    } else {
        config.settings.local_volume as f32 / 100.0
    };
    let initial_mic_volume = config.settings.mic_volume as f32 / 100.0;

    let player = crate::audio::player::AudioPlayer::new_with_initial_volumes(
        initial_local_volume,
        initial_mic_volume,
    );
    player.set_auto_gain_enabled(config.settings.auto_gain);
    player.set_auto_gain_target(config.settings.auto_gain_target_lufs);
    player.set_auto_gain_mode(config.settings.auto_gain_mode.player_value());
    player.set_auto_gain_apply_to(config.settings.auto_gain_apply_to.player_value());
    player.set_auto_gain_dynamic_settings(
        config.settings.auto_gain_lookahead_ms,
        config.settings.auto_gain_attack_ms,
        config.settings.auto_gain_release_ms,
    );
    player.set_looping(config.settings.play_mode.should_loop());
    player
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
        let path = base.join(format!("tone.{ext}"));
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
}
