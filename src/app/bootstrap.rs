use parking_lot::Mutex;
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::*;
use gtk4::{Application, Window};
use libadwaita as adw;
use libadwaita::prelude::*;
use log::{info, warn};

use crate::app_meta::{
    APP_BINARY, APP_ICON_NAME, APP_ID, APP_TITLE, APP_VERSION, BACKEND_ENV_VAR, FALLBACK_RENDERER,
    FORCE_X11_ENV_VAR, RENDERER_ENV_VAR, WAYLAND_BACKEND, X11_BACKEND,
};
use crate::app_state::AppState;
use crate::config::{Config, ControlHotkeyAction};
use crate::timer_registry::TimerRegistry;

pub fn run() {
    init_logging();
    handoff_to_newer_user_install_if_needed();
    crate::diagnostics::audit::init_from_env();
    if std::env::args().any(|arg| arg == "--audio-engine") {
        std::process::exit(crate::audio::engine_server::run());
    }
    if std::env::args().any(|arg| arg == "--diagnose") {
        std::process::exit(crate::diagnostics::routing::run());
    }
    if let Some(snapshot_path) = parse_graph_snapshot_arg() {
        std::process::exit(crate::diagnostics::routing::run_graph_snapshot(
            &snapshot_path,
        ));
    }

    configure_preferred_backend();
    configure_preferred_renderer();
    glib::set_prgname(Some(APP_BINARY));
    glib::set_application_name(APP_TITLE);

    info!("Starting Linux Soundboard (GTK4)");

    gtk4::init().expect("Failed to initialize GTK4");
    adw::init().expect("Failed to initialize libadwaita");
    Window::set_default_icon_name(APP_ICON_NAME);

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_activate_handler());
    app.run();
}

// Accepts `--diagnose-graph-snapshot <path>` and `--diagnose-graph-snapshot=<path>`.
// Returns None if the flag isn't present; exits with a usage message if the
// flag is present but no path follows it.
#[allow(clippy::print_stderr)]
fn parse_graph_snapshot_arg() -> Option<PathBuf> {
    const FLAG: &str = "--diagnose-graph-snapshot";
    let mut args = std::env::args();
    while let Some(arg) = args.next() {
        if arg == FLAG {
            let Some(value) = args.next() else {
                eprintln!("error: {FLAG} requires a path argument");
                std::process::exit(2);
            };
            return Some(PathBuf::from(value));
        }
        if let Some(value) = arg.strip_prefix(&format!("{FLAG}=")) {
            return Some(PathBuf::from(value));
        }
    }
    None
}

fn init_logging() {
    let env = env_logger::Env::default().default_filter_or(
        "warn,\
linux_soundboard::audio::engine_server=info,\
linux_soundboard::init::audio=info,\
linux_soundboard::audio::player=info,\
linux_soundboard::audio::player::source_routing=info",
    );
    env_logger::Builder::from_env(env).init();
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
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }

        let kind = installation_kind();
        let compatible_engine = compatible_stable_engine_running();
        let home = dirs::home_dir().unwrap_or_default();
        let stable_binary = stable_user_binary_path(&home);
        let installed_version = installed_user_version(&home);
        match appimage_startup_action(
            kind,
            compatible_engine,
            stable_binary.is_file(),
            installed_version.as_deref(),
            APP_VERSION,
        ) {
            AppImageStartupAction::Prompt => prompt_appimage_startup(app),
            AppImageStartupAction::AutoUpdate => update_appimage_and_start(app),
            AppImageStartupAction::StartPersistent => {
                start_application(app, StartupMode::Persistent, None)
            }
            AppImageStartupAction::StartTransient => {
                start_application(app, StartupMode::Transient, None)
            }
            AppImageStartupAction::LaunchInstalled => {
                // This is handled before GTK owns the application ID.
                log::error!("Could not hand off to the newer installed Linux Soundboard");
                app.quit();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupMode {
    Persistent,
    Transient,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum EngineUpdateNotice {
    Updated { previous_version: String },
    Failed { previous_version: String },
}

const ENGINE_UPDATE_HELP_URL: &str = "https://github.com/germanua/Linux-SoundBoard/blob/main/docs/TROUBLESHOOTING.md#engine-update-failed";

fn incompatible_engine_version() -> Option<String> {
    crate::audio::engine_ipc::engine_info()
        .ok()
        .and_then(|info| {
            (!crate::audio::engine_ipc::engine_info_compatible(&info)).then_some(info.app_version)
        })
}

fn engine_update_notice(
    previous_version: Option<String>,
    connected_remotely: bool,
) -> Option<EngineUpdateNotice> {
    previous_version.map(|previous_version| {
        if connected_remotely {
            EngineUpdateNotice::Updated { previous_version }
        } else {
            EngineUpdateNotice::Failed { previous_version }
        }
    })
}

fn show_engine_update_notice(parent: &gtk4::ApplicationWindow, notice: EngineUpdateNotice) {
    let (title, message) = match &notice {
        EngineUpdateNotice::Updated { previous_version } => (
            "Audio engine updated",
            format!(
                "The running audio engine ({previous_version}) did not match this app and needed to be updated. Linux Soundboard restarted it as {APP_VERSION} and reconnected successfully."
            ),
        ),
        EngineUpdateNotice::Failed { previous_version } => (
            "Audio engine update failed",
            format!(
                "The running audio engine ({previous_version}) did not match this app, but Linux Soundboard could not start engine {APP_VERSION}. A temporary engine is running for this session."
            ),
        ),
    };
    let dialog = adw::AlertDialog::new(Some(title), Some(&message));

    match notice {
        EngineUpdateNotice::Updated { .. } => {
            dialog.add_response("ok", "OK");
            dialog.set_close_response("ok");
            dialog.set_default_response(Some("ok"));
            dialog.choose(parent, None::<&gio::Cancellable>, |_| {});
        }
        EngineUpdateNotice::Failed { .. } => {
            dialog.add_responses(&[
                ("continue", "Continue temporarily"),
                ("help", "Open troubleshooting"),
            ]);
            dialog.set_close_response("continue");
            dialog.set_default_response(Some("help"));
            dialog.set_response_appearance("help", adw::ResponseAppearance::Suggested);
            let launcher_parent = parent.clone();
            dialog.choose(parent, None::<&gio::Cancellable>, move |response| {
                if response == "help" {
                    gtk4::UriLauncher::new(ENGINE_UPDATE_HELP_URL).launch(
                        Some(&launcher_parent),
                        None::<&gio::Cancellable>,
                        |result| {
                            if let Err(err) = result {
                                log::warn!("Could not open engine-update troubleshooting: {err}");
                            }
                        },
                    );
                }
            });
        }
    }
}

fn prompt_appimage_startup(app: &Application) {
    let parent = gtk4::ApplicationWindow::builder()
        .application(app)
        .title(APP_TITLE)
        .default_width(440)
        .default_height(160)
        .child(
            &gtk4::Label::builder()
                .label("Choose how Linux Soundboard should run.")
                .margin_top(32)
                .margin_bottom(32)
                .margin_start(24)
                .margin_end(24)
                .build(),
        )
        .build();
    parent.present();

    let dialog = adw::AlertDialog::new(
        Some("Set up the virtual microphone"),
        Some(
            "Install for a persistent virtual microphone, or run temporarily and restore your previous microphone when this window closes.",
        ),
    );
    dialog.add_responses(&[
        ("exit", "Exit"),
        ("temporary", "Run temporarily"),
        ("install", "Install for persistent virtual mic"),
    ]);
    dialog.set_close_response("exit");
    dialog.set_default_response(Some("install"));
    dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);

    let app = app.clone();
    let callback_parent = parent.clone();
    dialog.choose(
        &parent,
        None::<&gio::Cancellable>,
        move |response| match response.as_str() {
            "install" => install_appimage_and_start(
                &app,
                &callback_parent,
                "Installing Linux Soundboard…",
                "Installation failed",
                None,
            ),
            "temporary" => {
                callback_parent.close();
                start_application(&app, StartupMode::Transient, None);
            }
            _ => {
                callback_parent.close();
                app.quit();
            }
        },
    );
}

fn update_appimage_and_start(app: &Application) {
    let previous_engine_version = incompatible_engine_version();
    let parent = gtk4::ApplicationWindow::builder()
        .application(app)
        .title(APP_TITLE)
        .default_width(440)
        .default_height(160)
        .build();
    parent.present();
    install_appimage_and_start(
        app,
        &parent,
        "Updating Linux Soundboard…",
        "Update failed",
        previous_engine_version,
    );
}

fn install_appimage_and_start(
    app: &Application,
    parent: &gtk4::ApplicationWindow,
    progress_message: &str,
    failure_title: &'static str,
    previous_engine_version: Option<String>,
) {
    parent.set_child(Some(
        &gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(12)
            .halign(gtk4::Align::Center)
            .valign(gtk4::Align::Center)
            .build(),
    ));
    if let Some(container) = parent.child().and_downcast::<gtk4::Box>() {
        let spinner = gtk4::Spinner::builder().spinning(true).build();
        container.append(&spinner);
        container.append(&gtk4::Label::new(Some(progress_message)));
    }

    let callback_app = app.clone();
    let callback_parent = parent.clone();
    if let Err(err) = crate::commands::dispatch_async_result(
        "install_appimage",
        run_bundled_appimage_installer,
        move |result| match result {
            Ok(()) => {
                callback_parent.close();
                start_application(
                    &callback_app,
                    StartupMode::Persistent,
                    previous_engine_version,
                );
            }
            Err(err) => show_startup_error(
                &callback_app,
                &callback_parent,
                failure_title,
                &format!("{err}\n\nNo audio engine was started. Exit and try again."),
            ),
        },
    ) {
        show_startup_error(
            app,
            parent,
            failure_title,
            &format!("Could not start the installer: {err}"),
        );
    }
}

fn run_bundled_appimage_installer() -> Result<(), String> {
    let appdir = std::env::var_os("APPDIR")
        .map(PathBuf::from)
        .ok_or_else(|| {
            "APPDIR is unavailable; this AppImage has no installer payload".to_string()
        })?;
    let appimage = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .ok_or_else(|| "APPIMAGE is unavailable; cannot install the portable image".to_string())?;
    let installer = appdir
        .join("usr/libexec/linux-soundboard/installer")
        .join("install-user.sh");
    if !installer.is_file() {
        return Err(format!(
            "Bundled installer is missing at '{}'",
            installer.display()
        ));
    }

    let output = std::process::Command::new(&installer)
        .args(["install", appimage.to_string_lossy().as_ref()])
        .env("LSB_INSTALL_VERSION", APP_VERSION)
        .output()
        .map_err(|err| format!("Failed to run '{}': {err}", installer.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if detail.is_empty() {
            format!("Installer exited with status {}", output.status)
        } else {
            detail
        })
    }
}

fn show_startup_error(
    app: &Application,
    parent: &gtk4::ApplicationWindow,
    title: &str,
    message: &str,
) {
    let dialog = adw::AlertDialog::new(Some(title), Some(message));
    dialog.add_response("exit", "Exit");
    dialog.set_close_response("exit");
    dialog.set_default_response(Some("exit"));
    let app = app.clone();
    dialog.choose(parent, None::<&gio::Cancellable>, move |_| app.quit());
}

fn show_config_error(app: &Application, error: &dyn std::error::Error) {
    let parent = gtk4::ApplicationWindow::builder()
        .application(app)
        .title(APP_TITLE)
        .default_width(480)
        .default_height(160)
        .build();
    parent.present();
    let path = Config::config_path();
    show_startup_error(
        app,
        &parent,
        "Configuration could not be loaded",
        &format!(
            "{}\n\nNo audio engine was started and '{}' was not replaced. Fix the file, or stop all Linux Soundboard processes and restore '{}.pre-v6-backup'.",
            error,
            path.display(),
            path.display()
        ),
    );
}

fn start_application(
    app: &Application,
    startup_mode: StartupMode,
    previous_engine_version: Option<String>,
) {
    if let Some(display) = gtk4::gdk::Display::default() {
        info!("GTK display backend: {:?}", display.backend());
    } else {
        warn!("GTK display backend is unavailable during activation");
    }

    let mut config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            log::error!(
                "Refusing to start with unreadable config '{}': {err}",
                Config::config_path().display()
            );
            show_config_error(app, err.as_ref());
            return;
        }
    };
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

    let previous_engine_version = match startup_mode {
        StartupMode::Persistent => previous_engine_version.or_else(incompatible_engine_version),
        StartupMode::Transient => None,
    };
    let (player, connected_remotely) = initialize_player(&config, startup_mode);
    let engine_update_notice = engine_update_notice(previous_engine_version, connected_remotely);
    crate::diagnostics::set_playback_registry_count(0);
    crate::diagnostics::memory::log_memory_snapshot("startup:player_initialized");
    crate::diagnostics::record_phase_with_config("startup:player_initialized", &config);

    let pipewire_status = crate::audio::pipewire_detection::check_pipewire();

    let state = Arc::new(AppState {
        config: Arc::new(Mutex::new(config)),
        player: Arc::new(player),
        hotkeys: Arc::new(Mutex::new(hotkey_manager)),
        pipewire_status: Arc::new(Mutex::new(pipewire_status)),
        play_dispatch_debounce: Arc::new(Mutex::new(None)),
        loudness_coordinators: crate::commands::LoudnessCoordinators::new(),
        first_playback_recorded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        state_close.hotkeys.lock().shutdown();
        if let Err(e) = crate::diagnostics::write_memory_report() {
            log::warn!("Failed to write memory report: {}", e);
        }
        glib::Propagation::Proceed
    });

    window.present();
    if let Some(notice) = engine_update_notice {
        show_engine_update_notice(&window, notice);
    }
    record_state_phase("startup:window_presented", &state);

    schedule_startup_loudness_backfill(Arc::clone(&state), &timer_registry);

    {
        let state_idle = Arc::clone(&state);
        glib::timeout_add_local_once(Duration::from_secs(5), move || {
            record_state_phase("idle:5s", &state_idle);
        });
    }
}

fn load_config() -> Result<Config, Box<dyn std::error::Error>> {
    Config::load()
}

fn record_config_phase(name: &str, config: &Arc<Mutex<Config>>) {
    crate::diagnostics::record_phase_with_config(name, &config.lock());
}

fn record_state_phase(name: &str, state: &Arc<AppState>) {
    record_config_phase(name, &state.config);
}

fn schedule_startup_loudness_backfill(state: Arc<AppState>, _timer_registry: &TimerRegistry) {
    glib::idle_add_local_once(move || {
        crate::diagnostics::memory::log_memory_snapshot("startup:loudness_bg:check");
        let (auto_gain_enabled, missing_count) = {
            let cfg = state.config.lock();
            let missing_count = cfg
                .sounds
                .iter()
                .filter(|sound| sound.loudness_lufs.is_none())
                .count();
            crate::diagnostics::record_phase_with_config("startup:loudness_check", &cfg);
            (cfg.settings.auto_gain, missing_count)
        };

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallationKind {
    Stable,
    DirectAppImage,
    PortableOrDevelopment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppImageStartupAction {
    Prompt,
    AutoUpdate,
    StartPersistent,
    StartTransient,
    LaunchInstalled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceAction {
    Start,
    Restart,
}

fn installation_kind_for(
    path: &std::path::Path,
    home: &std::path::Path,
    appimage: bool,
) -> InstallationKind {
    let stable_user_binary = stable_user_binary_path(home);
    if path == std::path::Path::new("/usr/bin/linux-soundboard") || path == stable_user_binary {
        InstallationKind::Stable
    } else if appimage {
        InstallationKind::DirectAppImage
    } else {
        InstallationKind::PortableOrDevelopment
    }
}

fn installation_kind() -> InstallationKind {
    let appimage_path = std::env::var_os("APPIMAGE").map(PathBuf::from);
    let executable = appimage_path
        .clone()
        .or_else(|| std::env::current_exe().ok())
        .unwrap_or_default();
    let home = dirs::home_dir().unwrap_or_default();
    installation_kind_for(&executable, &home, appimage_path.is_some())
}

fn stable_user_install_root(home: &Path) -> PathBuf {
    home.join(".local/opt").join(APP_BINARY)
}

fn stable_user_binary_path(home: &Path) -> PathBuf {
    stable_user_install_root(home).join(APP_BINARY)
}

fn installed_user_version(home: &Path) -> Option<String> {
    std::fs::read_to_string(stable_user_install_root(home).join(".installed-version"))
        .ok()
        .map(|version| version.trim().to_string())
        .filter(|version| !version.is_empty())
}

fn appimage_startup_action(
    kind: InstallationKind,
    compatible_engine: bool,
    stable_user_binary_exists: bool,
    installed_version: Option<&str>,
    current_version: &str,
) -> AppImageStartupAction {
    if kind != InstallationKind::DirectAppImage {
        return if kind == InstallationKind::Stable || compatible_engine {
            AppImageStartupAction::StartPersistent
        } else {
            AppImageStartupAction::StartTransient
        };
    }

    if stable_user_binary_exists {
        return match installed_version
            .and_then(|installed| compare_release_versions(current_version, installed))
        {
            Some(Ordering::Less) => AppImageStartupAction::LaunchInstalled,
            Some(Ordering::Equal) => AppImageStartupAction::StartPersistent,
            Some(Ordering::Greater) | None => AppImageStartupAction::AutoUpdate,
        };
    }

    if compatible_engine {
        AppImageStartupAction::StartPersistent
    } else {
        AppImageStartupAction::Prompt
    }
}

fn compare_release_versions(left: &str, right: &str) -> Option<Ordering> {
    fn parse(version: &str) -> Option<[u64; 3]> {
        let mut parts = version.split('.');
        let parsed = [
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        ];
        parts.next().is_none().then_some(parsed)
    }

    Some(parse(left)?.cmp(&parse(right)?))
}

fn handoff_to_newer_user_install_if_needed() {
    let kind = installation_kind();
    if kind != InstallationKind::DirectAppImage {
        return;
    }
    let home = dirs::home_dir().unwrap_or_default();
    let stable_binary = stable_user_binary_path(&home);
    if appimage_startup_action(
        kind,
        false,
        stable_binary.is_file(),
        installed_user_version(&home).as_deref(),
        APP_VERSION,
    ) != AppImageStartupAction::LaunchInstalled
    {
        return;
    }

    use std::os::unix::process::CommandExt;
    let error = std::process::Command::new(&stable_binary)
        .args(std::env::args_os().skip(1))
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("OWD")
        .exec();
    log::error!(
        "Failed to launch newer installed Linux Soundboard '{}': {error}",
        stable_binary.display()
    );
    std::process::exit(1);
}

fn compatible_stable_engine_running() -> bool {
    let Ok(info) = crate::audio::engine_ipc::engine_info() else {
        return false;
    };
    let home = dirs::home_dir().unwrap_or_default();
    crate::audio::engine_ipc::engine_info_compatible(&info)
        && installation_kind_for(
            std::path::Path::new(&info.binary_path),
            &home,
            info.binary_path.ends_with(".AppImage"),
        ) == InstallationKind::Stable
}

fn initialize_player(
    config: &Config,
    startup_mode: StartupMode,
) -> (crate::audio::AudioPlayer, bool) {
    use crate::audio::AudioBackendKind;

    // Debug aid: when route audit is requested, bypass the systemd-spawned
    // engine entirely. The systemd unit doesn't inherit the user's environment,
    // so an engine started via `systemctl --user` would never see
    // `LSB_ROUTE_AUDIT`; routing writes happen in the engine, so the audit log
    // would stay empty. Forcing the in-process backend keeps everything inside
    // this process where init_from_env() already opened the audit file.
    let force_in_process = crate::diagnostics::audit::is_enabled();
    if force_in_process {
        log::warn!(
            "LSB_ROUTE_AUDIT is enabled — running audio engine in-process to capture writes \
             (the systemd-spawned engine would not inherit the env var)"
        );
        stop_audio_engine_service_and_process();
    } else {
        let remote = match startup_mode {
            StartupMode::Persistent => connect_or_start_audio_engine(
                crate::audio::AudioPlayer::connect_to_engine,
                crate::audio::engine_ipc::engine_running,
                crate::audio::engine_ipc::shutdown_incompatible_engine_if_running,
                manage_audio_engine_service,
                stop_audio_engine_service_and_process,
                60,
                || std::thread::sleep(Duration::from_millis(50)),
            ),
            StartupMode::Transient => {
                stop_audio_engine_service_and_process();
                None
            }
        };
        if let Some(player) = remote {
            log::info!("Connected UI to Linux Soundboard audio engine");
            return (player, true);
        }
    }

    let binary = std::env::current_exe()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    log::warn!(
        "Using transient in-process audio engine from {binary}; the systemd service was stopped to prevent duplicate virtual-mic ownership"
    );
    let backend = if crate::audio::pipewire_detection::check_pipewire().available {
        AudioBackendKind::PipeWire
    } else {
        AudioBackendKind::PulseAudio
    };
    (
        crate::audio::AudioPlayer::new_with_config_and_audio_backend(config, backend),
        false,
    )
}

fn connect_or_start_audio_engine<T>(
    mut connect: impl FnMut() -> Option<T>,
    engine_running: impl FnOnce() -> bool,
    shutdown_incompatible: impl FnOnce() -> bool,
    mut service_action: impl FnMut(ServiceAction) -> bool,
    cleanup_before_local: impl FnOnce(),
    max_connect_attempts: usize,
    mut wait_between_attempts: impl FnMut(),
) -> Option<T> {
    if let Some(engine) = connect() {
        return Some(engine);
    }

    let action = if engine_running() {
        if !shutdown_incompatible() {
            cleanup_before_local();
            return None;
        }
        ServiceAction::Restart
    } else {
        ServiceAction::Start
    };

    if !service_action(action) {
        cleanup_before_local();
        return None;
    }

    for attempt in 0..max_connect_attempts.max(1) {
        if let Some(engine) = connect() {
            return Some(engine);
        }
        if attempt + 1 < max_connect_attempts {
            wait_between_attempts();
        }
    }

    cleanup_before_local();
    None
}

fn stop_audio_engine_service_and_process() {
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "stop", "linux-soundboard-engine.service"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    crate::audio::engine_ipc::shutdown_engine_if_running();
}

fn manage_audio_engine_service(action: ServiceAction) -> bool {
    let service = "linux-soundboard-engine.service";
    ensure_user_audio_engine_service_file(service);
    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !matches!(reload, Ok(status) if status.success()) {
        return false;
    }

    let args: &[&str] = match action {
        ServiceAction::Start => &["--user", "enable", "--now", service],
        ServiceAction::Restart => &["--user", "restart", service],
    };
    matches!(
        std::process::Command::new("systemctl")
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
        Ok(status) if status.success()
    )
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

/// Standard search paths for systemd user unit files, in priority order.
/// Mirrors the lookup order used by `systemctl --user` on most distributions.
const SYSTEMD_USER_UNIT_DIRS: &[&str] = &[
    "/etc/systemd/user",
    "/usr/local/share/systemd/user",
    "/usr/share/systemd/user",
    "/usr/local/lib/systemd/user",
    "/usr/lib/systemd/user",
    "/usr/lib64/systemd/user",
    "/lib/systemd/user",
];

fn packaged_audio_engine_service_exists(service: &str) -> bool {
    SYSTEMD_USER_UNIT_DIRS
        .iter()
        .any(|dir| PathBuf::from(dir).join(service).exists())
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
RestartPreventExitStatus=2\n\
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
    use std::cell::{Cell, RefCell};
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
        assert!(service.contains("RestartPreventExitStatus=2"));
    }

    #[test]
    fn installation_kind_uses_only_system_and_stable_user_paths() {
        let home = Path::new("/home/test");
        assert_eq!(
            installation_kind_for(Path::new("/usr/bin/linux-soundboard"), home, false),
            InstallationKind::Stable
        );
        assert_eq!(
            installation_kind_for(
                Path::new("/home/test/.local/opt/linux-soundboard/linux-soundboard"),
                home,
                true
            ),
            InstallationKind::Stable
        );
        assert_eq!(
            installation_kind_for(
                Path::new("/home/test/Downloads/Soundboard.AppImage"),
                home,
                true
            ),
            InstallationKind::DirectAppImage
        );
        assert_eq!(
            installation_kind_for(Path::new("/tmp/target/debug/linux-soundboard"), home, false),
            InstallationKind::PortableOrDevelopment
        );
    }

    #[test]
    fn direct_appimage_auto_updates_an_existing_user_install() {
        assert_eq!(
            appimage_startup_action(
                InstallationKind::DirectAppImage,
                false,
                true,
                Some("2.1.0"),
                "2.1.1",
            ),
            AppImageStartupAction::AutoUpdate
        );
        assert_eq!(
            appimage_startup_action(InstallationKind::DirectAppImage, false, true, None, "2.1.1",),
            AppImageStartupAction::AutoUpdate
        );
    }

    #[test]
    fn direct_appimage_only_prompts_for_a_first_user_install() {
        assert_eq!(
            appimage_startup_action(
                InstallationKind::DirectAppImage,
                false,
                false,
                None,
                "2.1.1",
            ),
            AppImageStartupAction::Prompt
        );
        assert_eq!(
            appimage_startup_action(InstallationKind::DirectAppImage, true, false, None, "2.1.1",),
            AppImageStartupAction::StartPersistent
        );
    }

    #[test]
    fn direct_appimage_does_not_downgrade_a_newer_user_install() {
        assert_eq!(
            appimage_startup_action(
                InstallationKind::DirectAppImage,
                false,
                true,
                Some("2.2.0"),
                "2.1.1",
            ),
            AppImageStartupAction::LaunchInstalled
        );
        assert_eq!(
            appimage_startup_action(
                InstallationKind::DirectAppImage,
                false,
                true,
                Some("2.1.1"),
                "2.1.1",
            ),
            AppImageStartupAction::StartPersistent
        );
    }

    #[test]
    fn release_version_comparison_is_numeric() {
        assert_eq!(
            compare_release_versions("2.10.0", "2.9.9"),
            Some(std::cmp::Ordering::Greater)
        );
        assert_eq!(
            compare_release_versions("3.0.0", "3.0.0"),
            Some(std::cmp::Ordering::Equal)
        );
        assert_eq!(compare_release_versions("not-a-version", "3.0.0"), None);
    }

    #[test]
    fn matching_engine_connects_without_service_changes() {
        let events = RefCell::new(Vec::new());

        let engine = connect_or_start_audio_engine(
            || {
                events.borrow_mut().push("connect");
                Some(7)
            },
            || panic!("matching engine must not be reprobed"),
            || panic!("matching engine must not be stopped"),
            |_| panic!("matching engine must not change the service"),
            || panic!("matching engine must not run local cleanup"),
            1,
            || {},
        );

        assert_eq!(engine, Some(7));
        assert_eq!(*events.borrow(), ["connect"]);
    }

    #[test]
    fn absent_engine_starts_service_and_connects() {
        let attempts = Cell::new(0);
        let events = RefCell::new(Vec::new());

        let engine = connect_or_start_audio_engine(
            || {
                events.borrow_mut().push("connect");
                attempts.set(attempts.get() + 1);
                (attempts.get() == 2).then_some(7)
            },
            || {
                events.borrow_mut().push("engine-running");
                false
            },
            || panic!("absent engine must not be stopped"),
            |action| {
                events.borrow_mut().push(match action {
                    ServiceAction::Start => "start-service",
                    ServiceAction::Restart => "restart-service",
                });
                true
            },
            || panic!("compatible service must not run local cleanup"),
            1,
            || {},
        );

        assert_eq!(engine, Some(7));
        assert_eq!(
            *events.borrow(),
            ["connect", "engine-running", "start-service", "connect"]
        );
    }

    #[test]
    fn incompatible_engine_is_stopped_restarted_and_reconnected() {
        let attempts = Cell::new(0);
        let events = RefCell::new(Vec::new());

        let engine = connect_or_start_audio_engine(
            || {
                events.borrow_mut().push("connect");
                attempts.set(attempts.get() + 1);
                (attempts.get() == 2).then_some(7)
            },
            || {
                events.borrow_mut().push("engine-running");
                true
            },
            || {
                events.borrow_mut().push("stop-incompatible");
                true
            },
            |action| {
                assert_eq!(action, ServiceAction::Restart);
                events.borrow_mut().push("restart-service");
                true
            },
            || panic!("compatible restarted service must not fall back locally"),
            1,
            || {},
        );

        assert_eq!(engine, Some(7));
        assert_eq!(
            *events.borrow(),
            [
                "connect",
                "engine-running",
                "stop-incompatible",
                "restart-service",
                "connect"
            ]
        );
    }

    #[test]
    fn restarted_incompatible_engine_is_stopped_once_before_local_fallback() {
        let events = RefCell::new(Vec::new());
        let cleanup_count = Cell::new(0);

        let engine = connect_or_start_audio_engine(
            || {
                events.borrow_mut().push("connect");
                None::<u8>
            },
            || {
                events.borrow_mut().push("engine-running");
                true
            },
            || {
                events.borrow_mut().push("stop-incompatible");
                true
            },
            |action| {
                assert_eq!(action, ServiceAction::Restart);
                events.borrow_mut().push("restart-service");
                true
            },
            || {
                cleanup_count.set(cleanup_count.get() + 1);
                events.borrow_mut().push("cleanup-before-local");
            },
            1,
            || {},
        );

        assert_eq!(engine, None);
        assert_eq!(cleanup_count.get(), 1);
        assert_eq!(
            *events.borrow(),
            [
                "connect",
                "engine-running",
                "stop-incompatible",
                "restart-service",
                "connect",
                "cleanup-before-local"
            ]
        );
    }

    #[test]
    fn unavailable_systemd_falls_back_to_one_local_owner() {
        let events = RefCell::new(Vec::new());
        let cleanup_count = Cell::new(0);

        let engine = connect_or_start_audio_engine(
            || None::<u8>,
            || false,
            || panic!("absent engine must not be stopped"),
            |action| {
                assert_eq!(action, ServiceAction::Start);
                events.borrow_mut().push("systemd-unavailable");
                false
            },
            || cleanup_count.set(cleanup_count.get() + 1),
            1,
            || {},
        );

        assert_eq!(engine, None);
        assert_eq!(*events.borrow(), ["systemd-unavailable"]);
        assert_eq!(cleanup_count.get(), 1);
    }

    #[test]
    fn successful_stale_engine_restart_reports_update() {
        assert_eq!(
            engine_update_notice(Some("2.0.0".to_string()), true),
            Some(EngineUpdateNotice::Updated {
                previous_version: "2.0.0".to_string(),
            })
        );
    }

    #[test]
    fn failed_stale_engine_restart_reports_temporary_fallback() {
        assert_eq!(
            engine_update_notice(Some("2.0.0".to_string()), false),
            Some(EngineUpdateNotice::Failed {
                previous_version: "2.0.0".to_string(),
            })
        );
    }

    #[test]
    fn matching_or_absent_engine_reports_no_update() {
        assert_eq!(engine_update_notice(None, true), None);
        assert_eq!(engine_update_notice(None, false), None);
    }
}
