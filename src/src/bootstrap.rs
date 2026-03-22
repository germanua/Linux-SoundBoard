//! Application bootstrap and startup wiring.

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::*;
use gtk4::{Application, Window};
use libadwaita as adw;
use log::{info, warn};

use crate::app_meta::{
    APP_BINARY, APP_ID, APP_TITLE, BACKEND_ENV_VAR, HOTKEY_POLL_INTERVAL_MS,
    STARTUP_VIRTUAL_MIC_DELAY_MS, X11_BACKEND,
};
use crate::app_state::AppState;
use crate::config::{Config, ControlHotkeyAction};

pub fn run() {
    env_logger::init();
    configure_preferred_backend();
    glib::set_prgname(Some(APP_BINARY));
    glib::set_application_name(APP_TITLE);

    info!("Starting Linux Soundboard (GTK4)");

    adw::init().expect("Failed to initialize libadwaita");
    Window::set_default_icon_name(APP_ID);

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_activate_handler());
    app.run();
}

fn configure_preferred_backend() {
    if std::env::var("WAYLAND_DISPLAY").is_ok() && std::env::var("DISPLAY").is_ok() {
        let keep_wayland = matches!(
            std::env::var("LSB_PREFER_WAYLAND_GTK").ok().as_deref(),
            Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
        );
        if keep_wayland {
            info!(
                "Keeping GTK backend unchanged because LSB_PREFER_WAYLAND_GTK is enabled"
            );
            return;
        }

        let previous = std::env::var(BACKEND_ENV_VAR).ok();
        if previous.as_deref() != Some(X11_BACKEND) {
            info!(
                "Wayland and X11 displays detected; forcing GTK onto X11 via {}={} (previous: {:?})",
                BACKEND_ENV_VAR,
                X11_BACKEND,
                previous
            );
            std::env::set_var(BACKEND_ENV_VAR, X11_BACKEND);
        }
    }
}

fn build_activate_handler() -> impl Fn(&Application) + 'static {
    move |app| {
        if let Some(display) = gtk4::gdk::Display::default() {
            info!("GTK display backend: {:?}", display.backend());
        } else {
            warn!("GTK display backend is unavailable during activation");
        }

        let config = load_config();
        crate::diagnostics::memory::log_memory_snapshot("startup:config_loaded");

        let mic_status = setup_virtual_microphone(&config);

        std::thread::sleep(Duration::from_millis(STARTUP_VIRTUAL_MIC_DELAY_MS));

        let prebound_hotkeys = prebound_hotkeys(&config);
        let (hotkey_sender, hotkey_receiver) = mpsc::channel::<String>();
        let hotkey_receiver = Arc::new(Mutex::new(hotkey_receiver));

        let hotkey_manager =
            crate::hotkeys::HotkeyManager::new_blocking(hotkey_sender, &prebound_hotkeys);

        let player = initialize_player(&config);
        crate::diagnostics::memory::log_memory_snapshot("startup:player_initialized");

        let pipewire_status = crate::pipewire::detection::check_pipewire();

        let state = Arc::new(AppState {
            config: Arc::new(Mutex::new(config)),
            player: Arc::new(Mutex::new(player)),
            hotkeys: Arc::new(Mutex::new(hotkey_manager)),
            mic_status: Arc::new(Mutex::new(mic_status)),
            pipewire_status: Arc::new(Mutex::new(pipewire_status)),
        });

        let (window, transport) = crate::ui::app_window::build_window(app, Arc::clone(&state));
        crate::diagnostics::memory::log_memory_snapshot("startup:window_built");

        let state_hk = Arc::clone(&state);
        let window_hk = window.clone();
        glib::timeout_add_local(Duration::from_millis(HOTKEY_POLL_INTERVAL_MS), move || {
            while let Ok(sound_id) = hotkey_receiver.lock().unwrap().try_recv() {
                crate::ui::app_window::handle_hotkey(&window_hk, &state_hk, &transport, &sound_id);
            }
            glib::ControlFlow::Continue
        });

        window.present();
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
