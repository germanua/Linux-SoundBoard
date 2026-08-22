use parking_lot::Mutex;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread;
use std::time::Duration;

use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::*;
use gtk4::{Application, Window};
use libadwaita as adw;
use libadwaita::prelude::*;
use log::{info, warn};
use nix::fcntl::{Flock, FlockArg};

use crate::app_meta::{
    APP_BINARY, APP_ICON_NAME, APP_ID, APP_TITLE, APP_VERSION, BACKEND_ENV_VAR, FALLBACK_RENDERER,
    FORCE_X11_ENV_VAR, RENDERER_ENV_VAR, WAYLAND_BACKEND, X11_BACKEND,
};
use crate::app_state::AppState;
use crate::config::Config;
use crate::timer_registry::TimerRegistry;

const ENGINE_SERVICE_UNIT: &str = "linux-soundboard-engine.service";
const ENGINE_TARGET_UNIT: &str = "linux-soundboard-engine.target";
static STORAGE_LOCK: OnceLock<Mutex<Option<Flock<File>>>> = OnceLock::new();

fn acquire_storage_lock_at(path: &Path) -> Result<Flock<File>, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|error| error.to_string())?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| {
        format!(
            "Another Linux Soundboard process is using '{}': {error}",
            path.display()
        )
    })
}

fn ensure_storage_lock() -> Result<(), String> {
    let mut lock = STORAGE_LOCK.get_or_init(|| Mutex::new(None)).lock();
    if lock.is_none() {
        let path = Config::config_path().with_file_name("storage.lock");
        *lock = Some(acquire_storage_lock_at(&path)?);
    }
    Ok(())
}

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

    let backend = std::env::var(BACKEND_ENV_VAR).ok();
    let vmware = running_in_vmware_guest();
    if !should_use_fallback_renderer(backend.as_deref(), vmware) {
        return;
    }

    let reason = if backend.as_deref() == Some(X11_BACKEND) {
        "X11/XWayland session"
    } else {
        "VMware guest"
    };
    info!("{reason} detected; using lower-memory GTK renderer via {RENDERER_ENV_VAR}={FALLBACK_RENDERER}");
    std::env::set_var(RENDERER_ENV_VAR, FALLBACK_RENDERER);
}

fn should_use_fallback_renderer(backend: Option<&str>, vmware: bool) -> bool {
    backend == Some(X11_BACKEND) || vmware
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
        let compatible_engine = compatible_engine_running();
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
const ENGINE_UNAVAILABLE_HELP_URL: &str = "https://github.com/germanua/Linux-SoundBoard/blob/main/docs/TROUBLESHOOTING.md#persistent-audio-engine-unavailable";

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

fn show_engine_unavailable_error(
    app: &Application,
    parent: &gtk4::ApplicationWindow,
    message: &str,
) {
    let dialog = adw::AlertDialog::new(Some("Persistent audio engine unavailable"), Some(message));
    dialog.add_responses(&[
        ("exit", "Exit"),
        ("help", "Open troubleshooting"),
        ("temporary", "Run temporarily"),
    ]);
    dialog.set_close_response("exit");
    dialog.set_default_response(Some("temporary"));
    dialog.set_response_appearance("temporary", adw::ResponseAppearance::Suggested);

    let callback_app = app.clone();
    let callback_parent = parent.clone();
    let callback_message = message.to_string();
    dialog.choose(
        parent,
        None::<&gio::Cancellable>,
        move |response| match response.as_str() {
            "temporary" => {
                callback_parent.close();
                start_application(&callback_app, StartupMode::Transient, None);
            }
            "help" => {
                gtk4::UriLauncher::new(ENGINE_UNAVAILABLE_HELP_URL).launch(
                    Some(&callback_parent),
                    None::<&gio::Cancellable>,
                    |result| {
                        if let Err(err) = result {
                            log::warn!("Could not open engine troubleshooting: {err}");
                        }
                    },
                );
                // Re-presented after this dialog finishes closing, so the choice stays open.
                glib::idle_add_local_once(move || {
                    show_engine_unavailable_error(
                        &callback_app,
                        &callback_parent,
                        &callback_message,
                    );
                });
            }
            _ => {
                callback_parent.close();
                callback_app.quit();
            }
        },
    );
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

fn show_storage_recovery_error(
    app: &Application,
    parent: &gtk4::ApplicationWindow,
    message: &str,
    startup_mode: StartupMode,
    previous_engine_version: Option<String>,
) {
    let config_path = Config::config_path();
    let backup_path = config_path.with_file_name("config.json.pre-v8-backup");
    if !backup_path.is_file() {
        show_startup_error(app, parent, "Sound library could not be opened", message);
        return;
    }

    let dialog = adw::AlertDialog::new(
        Some("Sound library could not be opened"),
        Some(&format!(
            "{message}\n\nYou can exit without changes or restore the preserved pre-v8 settings. Current settings and database files will be archived, not deleted."
        )),
    );
    dialog.add_responses(&[("exit", "Exit"), ("restore", "Restore pre-v8 backup")]);
    dialog.set_close_response("exit");
    dialog.set_default_response(Some("restore"));
    dialog.set_response_appearance("restore", adw::ResponseAppearance::Suggested);
    let callback_app = app.clone();
    let callback_parent = parent.clone();
    dialog.choose(parent, None::<&gio::Cancellable>, move |response| {
        if response != "restore" {
            callback_app.quit();
            return;
        }
        let library_path = config_path.with_file_name("library.sqlite3");
        let app_done = callback_app.clone();
        let parent_done = callback_parent.clone();
        if let Err(error) = crate::commands::dispatch_async_result(
            "restore_legacy_library",
            move || crate::legacy_migration::restore_legacy_backup(&config_path, &library_path),
            move |result| match result {
                Ok(report) => {
                    log::info!(
                        "Restored pre-v8 settings; archived config={:?}, database={:?}",
                        report.archived_config,
                        report.archived_database
                    );
                    parent_done.close();
                    start_application(&app_done, startup_mode, previous_engine_version);
                }
                Err(error) => show_startup_error(
                    &app_done,
                    &parent_done,
                    "Sound library restore failed",
                    &format!("{error}\n\nNo current file was deleted."),
                ),
            },
        ) {
            show_startup_error(
                &callback_app,
                &callback_parent,
                "Sound library restore could not start",
                &error.to_string(),
            );
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StoragePreparation {
    Ready,
    NeedsLegacyMigration,
    NeedsEmptyLibraryRecovery { library_id: String },
}

fn prepare_startup_storage() -> Result<StoragePreparation, String> {
    let config_path = Config::config_path();
    let library_path = config_path.with_file_name("library.sqlite3");
    prepare_startup_storage_at(&config_path, &library_path)
}

fn prepare_startup_storage_at(
    config_path: &Path,
    library_path: &Path,
) -> Result<StoragePreparation, String> {
    if !config_path.exists() {
        if library_path.exists() {
            let identity = crate::legacy_migration::database_identity(library_path)
                .map_err(|error| error.to_string())?;
            if identity.source_sha256.is_some() {
                return Err(format!(
                    "A migrated library exists but config.json is missing. Restore '{}' and restart; no files were changed.",
                    config_path.with_file_name("config.json.pre-v8-backup").display()
                ));
            }
            let mut config = Config::default();
            config
                .save_to_path(config_path)
                .map_err(|error| error.to_string())?;
            return Ok(StoragePreparation::Ready);
        }
        let mut config = Config::default();
        let library_id = uuid::Uuid::new_v4().to_string();
        crate::legacy_migration::initialize_empty_library(library_path, &library_id)
            .map_err(|error| error.to_string())?;
        config
            .save_to_path(config_path)
            .map_err(|error| error.to_string())?;
        return Ok(StoragePreparation::Ready);
    }

    let version = crate::legacy_migration::config_schema_version(config_path)
        .map_err(|error| error.to_string())?;
    if version <= crate::config::LAST_LEGACY_SCHEMA_VERSION {
        if library_path.exists() {
            crate::legacy_migration::complete_legacy_settings_cutover(config_path, library_path)
            .map_err(|error| {
                format!(
                    "Legacy settings and the existing library database cannot be matched safely: {error}. No files were replaced."
                )
            })?;
            return Ok(StoragePreparation::Ready);
        }
        return Ok(StoragePreparation::NeedsLegacyMigration);
    }
    if version != crate::config::CURRENT_SCHEMA_VERSION {
        return Err(format!(
            "Configuration schema {version} is newer than supported schema {}.",
            crate::config::CURRENT_SCHEMA_VERSION
        ));
    }

    Config::load_from_path(config_path).map_err(|error| error.to_string())?;
    if !library_path.exists() {
        return Ok(StoragePreparation::NeedsEmptyLibraryRecovery {
            library_id: uuid::Uuid::new_v4().to_string(),
        });
    }
    crate::legacy_migration::database_identity(library_path).map_err(|error| error.to_string())?;
    Ok(StoragePreparation::Ready)
}

fn start_application(
    app: &Application,
    startup_mode: StartupMode,
    previous_engine_version: Option<String>,
) {
    let parent = gtk4::ApplicationWindow::builder()
        .application(app)
        .title(APP_TITLE)
        .default_width(440)
        .default_height(160)
        .build();
    let content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .build();
    content.append(&gtk4::Spinner::builder().spinning(true).build());
    content.append(&gtk4::Label::new(Some("Preparing sound library…")));
    parent.set_child(Some(&content));
    parent.present();
    if let Err(error) = ensure_storage_lock() {
        show_startup_error(app, &parent, "Sound library is already in use", &error);
        return;
    }

    let callback_app = app.clone();
    let callback_parent = parent.clone();
    if let Err(error) = crate::commands::dispatch_async_result(
        "prepare_startup_storage",
        prepare_startup_storage,
        move |result| match result {
            Ok(StoragePreparation::Ready) => {
                start_application_ready(
                    &callback_app,
                    &callback_parent,
                    startup_mode,
                    previous_engine_version.clone(),
                );
            }
            Ok(StoragePreparation::NeedsLegacyMigration) => prompt_legacy_migration(
                &callback_app,
                &callback_parent,
                startup_mode,
                previous_engine_version,
            ),
            Ok(StoragePreparation::NeedsEmptyLibraryRecovery { library_id }) => {
                let message = format!(
                    "The sound library database '{}' is missing.",
                    Config::config_path()
                        .with_file_name("library.sqlite3")
                        .display()
                );
                if Config::config_path()
                    .with_file_name("config.json.pre-v8-backup")
                    .is_file()
                {
                    show_storage_recovery_error(
                        &callback_app,
                        &callback_parent,
                        &message,
                        startup_mode,
                        previous_engine_version,
                    );
                } else {
                    prompt_empty_library_creation(
                        &callback_app,
                        &callback_parent,
                        library_id,
                        startup_mode,
                        previous_engine_version,
                    );
                }
            }
            Err(error) => show_storage_recovery_error(
                &callback_app,
                &callback_parent,
                &error,
                startup_mode,
                previous_engine_version,
            ),
        },
    ) {
        show_startup_error(
            app,
            &parent,
            "Sound library could not be prepared",
            &error.to_string(),
        );
    }
}

fn prompt_empty_library_creation(
    app: &Application,
    parent: &gtk4::ApplicationWindow,
    library_id: String,
    startup_mode: StartupMode,
    previous_engine_version: Option<String>,
) {
    let dialog = adw::AlertDialog::new(
        Some("Sound library is missing"),
        Some(
            "No recoverable library backup was found. You can exit without changes or create an empty library while keeping your current settings.",
        ),
    );
    dialog.add_responses(&[("exit", "Exit"), ("create", "Create Empty Library")]);
    dialog.set_close_response("exit");
    dialog.set_default_response(Some("exit"));
    let callback_app = app.clone();
    let callback_parent = parent.clone();
    dialog.choose(parent, None::<&gio::Cancellable>, move |response| {
        if response != "create" {
            callback_app.quit();
            return;
        }
        let library_path = Config::config_path().with_file_name("library.sqlite3");
        let app_done = callback_app.clone();
        let parent_done = callback_parent.clone();
        if let Err(error) = crate::commands::dispatch_async_result(
            "create_empty_library",
            move || crate::legacy_migration::initialize_empty_library(&library_path, &library_id),
            move |result| match result {
                Ok(()) => {
                    parent_done.close();
                    start_application(&app_done, startup_mode, previous_engine_version);
                }
                Err(error) => show_startup_error(
                    &app_done,
                    &parent_done,
                    "Empty library could not be created",
                    &error.to_string(),
                ),
            },
        ) {
            show_startup_error(
                &callback_app,
                &callback_parent,
                "Empty library creation could not start",
                &error.to_string(),
            );
        }
    });
}

fn prompt_legacy_migration(
    app: &Application,
    parent: &gtk4::ApplicationWindow,
    startup_mode: StartupMode,
    previous_engine_version: Option<String>,
) {
    let dialog = adw::AlertDialog::new(
        Some("Upgrade your sound library"),
        Some(
            "Linux Soundboard will move sounds, folders, tabs, and hotkeys into its low-memory database. Your original config is kept as config.json.pre-v8-backup. Cancel makes no changes.",
        ),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("migrate", "Upgrade")]);
    dialog.set_close_response("cancel");
    dialog.set_default_response(Some("migrate"));
    dialog.set_response_appearance("migrate", adw::ResponseAppearance::Suggested);
    let callback_app = app.clone();
    let callback_parent = parent.clone();
    dialog.choose(parent, None::<&gio::Cancellable>, move |response| {
        if response != "migrate" {
            callback_parent.close();
            callback_app.quit();
            return;
        }
        let config_path = Config::config_path();
        let library_path = config_path.with_file_name("library.sqlite3");
        let progress_label = gtk4::Label::new(Some("Preparing library upgrade…"));
        let spinner = gtk4::Spinner::builder().spinning(true).build();
        let cancel_button = gtk4::Button::with_label("Cancel");
        let progress_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(12)
            .halign(gtk4::Align::Center)
            .valign(gtk4::Align::Center)
            .build();
        progress_box.append(&spinner);
        progress_box.append(&progress_label);
        progress_box.append(&cancel_button);
        callback_parent.set_child(Some(&progress_box));

        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let cancelled = Arc::clone(&cancelled);
            let progress_label = progress_label.clone();
            cancel_button.connect_clicked(move |button| {
                cancelled.store(true, AtomicOrdering::Relaxed);
                button.set_sensitive(false);
                progress_label.set_label("Cancelling safely…");
            });
        }
        let (progress_sender, progress_receiver) = mpsc::channel();
        let progress_source = Rc::new(RefCell::new(Some(glib::timeout_add_local(
            Duration::from_millis(50),
            {
                let progress_label = progress_label.clone();
                let cancel_button = cancel_button.clone();
                move || {
                    while let Ok(progress) = progress_receiver.try_recv() {
                        progress_label.set_label(migration_progress_message(progress));
                        cancel_button.set_sensitive(migration_progress_can_cancel(progress));
                    }
                    glib::ControlFlow::Continue
                }
            },
        ))));
        let app_done = callback_app.clone();
        let parent_done = callback_parent.clone();
        let progress_source_done = Rc::clone(&progress_source);
        let cancelled_worker = Arc::clone(&cancelled);
        if let Err(error) = crate::commands::dispatch_async_result(
            "migrate_legacy_library",
            move || {
                crate::legacy_migration::migrate_legacy_config_controlled(
                    &config_path,
                    &library_path,
                    cancelled_worker,
                    Arc::new(move |progress| {
                        let _ = progress_sender.send(progress);
                    }),
                )
            },
            move |result| {
                if let Some(source) = progress_source_done.borrow_mut().take() {
                    source.remove();
                }
                match result {
                    Ok(report) => {
                        log::info!(
                            "Migrated {} sounds, {} roots, {} tabs, and {} hotkeys",
                            report.sounds,
                            report.roots,
                            report.manual_tabs,
                            report.hotkeys
                        );
                        start_application_ready(
                            &app_done,
                            &parent_done,
                            startup_mode,
                            previous_engine_version,
                        );
                    }
                    Err(crate::legacy_migration::LegacyMigrationError::Cancelled) => {
                        parent_done.close();
                        app_done.quit();
                    }
                    Err(error) => show_startup_error(
                        &app_done,
                        &parent_done,
                        "Sound library upgrade failed",
                        &format!("{error}\n\nThe original config and backup were preserved."),
                    ),
                }
            },
        ) {
            if let Some(source) = progress_source.borrow_mut().take() {
                source.remove();
            }
            show_startup_error(
                &callback_app,
                &callback_parent,
                "Sound library upgrade could not start",
                &error.to_string(),
            );
        }
    });
}

fn migration_progress_message(
    progress: crate::legacy_migration::LegacyMigrationProgress,
) -> &'static str {
    use crate::legacy_migration::LegacyMigrationProgress;
    match progress {
        LegacyMigrationProgress::BackingUp => "Backing up the existing library…",
        LegacyMigrationProgress::Importing => "Importing sounds, folders, tabs, and hotkeys…",
        LegacyMigrationProgress::Verifying => "Verifying the upgraded library…",
        LegacyMigrationProgress::PublishingDatabase => "Saving the upgraded library…",
        LegacyMigrationProgress::PublishingSettings => "Saving settings…",
        LegacyMigrationProgress::Complete => "Upgrade complete",
    }
}

fn migration_progress_can_cancel(
    progress: crate::legacy_migration::LegacyMigrationProgress,
) -> bool {
    use crate::legacy_migration::LegacyMigrationProgress;
    matches!(
        progress,
        LegacyMigrationProgress::BackingUp
            | LegacyMigrationProgress::Importing
            | LegacyMigrationProgress::Verifying
    )
}

struct PreparedApplication {
    config: Config,
    library: crate::library_store::LibraryStore,
    initial_sound_count: usize,
    initial_sound_page: crate::library_store::SoundPage,
    player: crate::audio::AudioPlayer,
    pipewire_status: crate::audio::pipewire_detection::PipeWireStatus,
    engine_update_notice: Option<EngineUpdateNotice>,
}

struct StartupFailure {
    title: &'static str,
    message: String,
    /// The in-process engine can serve this session instead of failing outright.
    transient_fallback: bool,
}

impl StartupFailure {
    fn new(title: &'static str, message: String) -> Self {
        Self {
            title,
            message,
            transient_fallback: false,
        }
    }

    fn with_transient_fallback(title: &'static str, message: String) -> Self {
        Self {
            title,
            message,
            transient_fallback: true,
        }
    }
}

fn start_application_ready(
    app: &Application,
    parent: &gtk4::ApplicationWindow,
    startup_mode: StartupMode,
    previous_engine_version: Option<String>,
) {
    let callback_app = app.clone();
    let callback_parent = parent.clone();
    if let Err(error) = crate::commands::dispatch_async_result(
        "prepare_application_runtime",
        move || prepare_application(startup_mode, previous_engine_version),
        move |result| match result {
            Ok(prepared) => {
                callback_parent.close();
                finish_application_ready(&callback_app, prepared);
            }
            Err(failure) if failure.transient_fallback => {
                show_engine_unavailable_error(&callback_app, &callback_parent, &failure.message)
            }
            Err(failure) => show_startup_error(
                &callback_app,
                &callback_parent,
                failure.title,
                &failure.message,
            ),
        },
    ) {
        show_startup_error(
            app,
            parent,
            "Application startup could not continue",
            &error.to_string(),
        );
    }
}

fn prepare_application(
    startup_mode: StartupMode,
    previous_engine_version: Option<String>,
) -> Result<PreparedApplication, StartupFailure> {
    let config = match load_config() {
        Ok(config) => config,
        Err(err) => {
            let path = Config::config_path();
            return Err(StartupFailure::new(
                "Configuration could not be loaded",
                format!(
                    "{err}\n\nNo audio engine was started and '{}' was not replaced. Fix the file, or stop all Linux Soundboard processes and restore '{}.pre-v6-backup'.",
                    path.display(),
                    path.display()
                ),
            ));
        }
    };
    crate::diagnostics::memory::log_memory_snapshot("startup:config_loaded");
    crate::diagnostics::record_phase_with_config("startup:config_loaded", &config);

    let library_path = Config::config_path().with_file_name("library.sqlite3");
    let identity = match crate::legacy_migration::database_identity(&library_path) {
        Ok(identity) => identity,
        Err(error) => {
            return Err(StartupFailure::new(
                "Sound library could not be opened",
                error.to_string(),
            ));
        }
    };
    let library = match crate::library_store::LibraryStore::open_authoritative(
        library_path,
        &identity.library_id,
    ) {
        Ok(library) => library,
        Err(error) => {
            return Err(StartupFailure::new(
                "Sound library could not be opened",
                error.to_string(),
            ));
        }
    };
    let initial_sound_count = library.count(crate::library_store::LibraryScope::General, "");
    let initial_sound_page = library.page(crate::library_store::LibraryScope::General, "", 0);

    let previous_engine_version = match startup_mode {
        StartupMode::Persistent => previous_engine_version.or_else(incompatible_engine_version),
        StartupMode::Transient => None,
    };
    let (player, connected_remotely) = match initialize_player(&config, startup_mode) {
        Ok(player) => player,
        Err(error) => {
            return Err(StartupFailure::with_transient_fallback(
                "Persistent audio engine unavailable",
                error,
            ));
        }
    };
    let engine_update_notice = engine_update_notice(previous_engine_version, connected_remotely);
    crate::diagnostics::set_playback_registry_count(0);
    crate::diagnostics::memory::log_memory_snapshot("startup:player_initialized");
    crate::diagnostics::record_phase_with_config("startup:player_initialized", &config);

    let pipewire_status = crate::audio::pipewire_detection::check_pipewire();
    let initial_sound_count = initial_sound_count.recv().map_err(|error| {
        StartupFailure::new("Sound library could not be opened", error.to_string())
    })?;
    let initial_sound_page = initial_sound_page.recv().map_err(|error| {
        StartupFailure::new("Sound library could not be opened", error.to_string())
    })?;
    Ok(PreparedApplication {
        config,
        library,
        initial_sound_count,
        initial_sound_page,
        player,
        pipewire_status,
        engine_update_notice,
    })
}

fn finish_application_ready(app: &Application, prepared: PreparedApplication) {
    if let Some(display) = gtk4::gdk::Display::default() {
        info!("GTK display backend: {:?}", display.backend());
    } else {
        warn!("GTK display backend is unavailable during activation");
    }

    let PreparedApplication {
        config,
        library,
        initial_sound_count,
        initial_sound_page,
        player,
        pipewire_status,
        engine_update_notice,
    } = prepared;
    let (hotkey_sender, hotkey_receiver) = mpsc::sync_channel::<String>(64);
    let hotkey_manager = crate::hotkeys::HotkeyManager::new_deferred(hotkey_sender);
    crate::diagnostics::set_hotkey_status(&hotkey_manager.status_message());

    let hotkeys = Arc::new(Mutex::new(hotkey_manager));
    let hotkey_projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    let state = Arc::new(AppState {
        hotkey_group_cursor: Arc::new(Mutex::new(std::collections::HashMap::new())),
        config: Arc::new(Mutex::new(config)),
        library,
        player: Arc::new(player),
        hotkeys,
        hotkey_projection,
        manual_tabs: Arc::new(Mutex::new(Vec::new())),
        pipewire_status: Arc::new(Mutex::new(pipewire_status)),
        play_dispatch_debounce: Arc::new(Mutex::new(None)),
        loudness_coordinators: crate::commands::LoudnessCoordinators::new(),
        first_playback_recorded: Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });

    let timer_registry = TimerRegistry::new();

    let window = crate::ui::app_window::build_window(
        app,
        Arc::clone(&state),
        &timer_registry,
        initial_sound_count,
        initial_sound_page,
    );
    crate::diagnostics::set_validation_runtime(0, "deferred", 0);
    crate::diagnostics::memory::log_memory_snapshot("startup:window_built");
    record_state_phase("startup:window_built", &state);

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

    let tray = install_tray(app, &state);
    let mpris = install_mpris(app);
    install_now_playing(&tray, &mpris, &state);
    if let Some(mpris) = mpris.as_ref() {
        mpris.set_enabled(state.config.lock().settings.mpris_enabled);
    }

    let state_close = Arc::clone(&state);
    let timers_close = timer_registry.clone();
    window.connect_close_request(move |_| {
        // Only reached when the window is really closing: the window's own
        // handler runs first and stops the emission when it hides to the tray.
        if let Some(tray) = tray.borrow().as_ref() {
            tray.shutdown();
        }
        if let Some(mpris) = mpris.as_ref() {
            mpris.shutdown();
        }
        shutdown_application(&state_close, &timers_close);
        glib::Propagation::Proceed
    });

    window.present();
    if let Some(notice) = engine_update_notice {
        show_engine_update_notice(&window, notice);
    }
    record_state_phase("startup:window_presented", &state);

    schedule_startup_hotkey_projection(Arc::clone(&state));
    schedule_startup_loudness_backfill(Arc::clone(&state), &timer_registry);
    schedule_library_diagnostics(Arc::clone(&state));

    {
        let state_idle = Arc::clone(&state);
        glib::timeout_add_local_once(Duration::from_secs(5), move || {
            record_state_phase("idle:5s", &state_idle);
        });
    }
}

fn schedule_library_diagnostics(state: Arc<AppState>) {
    let response = state.library.stats();
    if let Err(error) = crate::commands::dispatch_async_result(
        "load_library_diagnostics",
        move || response.recv(),
        move |result| match result {
            Ok(stats) => {
                crate::diagnostics::set_library_counts(
                    stats.sounds,
                    stats.manual_tabs,
                    stats.roots,
                    stats.active_hotkeys,
                );
                record_state_phase("startup:library_ready", &state);
            }
            Err(error) => log::warn!("Failed to load library diagnostics: {error}"),
        },
    ) {
        log::warn!("Failed to dispatch library diagnostics: {error}");
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

/// Tear the app down. Reached from the close button when the window really is
/// closing, and from the tray's Quit row.
///
/// Guarded: both routes can end at the same window close, and the engine IPC
/// shouldn't have to cope with being shut down twice.
fn shutdown_application(state: &Arc<AppState>, timers: &TimerRegistry) {
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, AtomicOrdering::SeqCst) {
        return;
    }
    timers.remove_all();
    crate::diagnostics::set_timer_count(0);
    crate::diagnostics::set_playback_registry_count(0);
    record_state_phase("shutdown:close_request", state);
    state.player.stop_all();
    state.player.shutdown();
    state.hotkeys.lock().shutdown();
    if let Err(e) = crate::diagnostics::write_memory_report() {
        log::warn!("Failed to write memory report: {}", e);
    }
}

/// Where the tray icon lives while it is showing. Empty when the setting is
/// off, when the session has no bus, or when exporting failed.
type TraySlot = Rc<RefCell<Option<Rc<crate::tray::TrayService>>>>;

/// Put an icon in the panel and keep it in step with the settings.
///
/// A session with no watcher is not a failure: the item stays exported and
/// appears if a panel or extension turns up later. The close button asks
/// [`crate::tray::TrayService::is_live`] rather than assuming, so the window is
/// never hidden to a tray nobody can see.
fn install_tray(app: &Application, state: &Arc<AppState>) -> TraySlot {
    let slot: TraySlot = Rc::new(RefCell::new(None));
    let Some(connection) = app.dbus_connection() else {
        warn!("No session bus is available, so there will be no tray icon");
        return slot;
    };

    {
        let slot = Rc::clone(&slot);
        let state = Arc::clone(state);
        crate::ui_event_bridge::set_close_to_tray_policy(move || {
            state.config.lock().settings.close_to_tray
                && slot.borrow().as_ref().is_some_and(|tray| tray.is_live())
        });
    }

    {
        let slot = Rc::clone(&slot);
        crate::ui_event_bridge::set_tray_menu_handler(move |items| {
            if let Some(tray) = slot.borrow().as_ref() {
                tray.set_menu(items);
            }
        });
    }

    {
        let slot = Rc::clone(&slot);
        let state = Arc::clone(state);
        let connection = connection.clone();
        crate::ui_event_bridge::set_tray_enabled_handler(move |enabled| {
            let existing = slot.borrow_mut().take();
            match (enabled, existing) {
                (true, None) => *slot.borrow_mut() = start_tray_service(&connection, &state),
                (false, Some(tray)) => tray.shutdown(),
                (_, unchanged) => *slot.borrow_mut() = unchanged,
            }
        });
    }

    if state.config.lock().settings.tray_enabled {
        *slot.borrow_mut() = start_tray_service(&connection, state);
    }
    slot
}

/// Publish the playing sound to the desktop's media controls.
///
/// Exported for the whole session but only visible while the setting is on, so
/// the app doesn't sit in the panel's media controls holding the media keys
/// when the user never asked for it.
fn install_mpris(app: &Application) -> Option<Rc<crate::mpris::MprisService>> {
    let connection = app.dbus_connection()?;
    let service = match crate::mpris::MprisService::start(
        &connection,
        crate::ui_event_bridge::post_mpris_command,
    ) {
        Ok(service) => Rc::new(service),
        Err(error) => {
            warn!("Could not export media controls: {error}");
            return None;
        }
    };

    Some(service)
}

/// Route the playing sound to both places that show it.
///
/// The tray tooltip is not part of the media-controls feature and is not gated
/// on its setting: hovering the icon is the first thing anyone tries, and it
/// works on every desktop that can show a tray icon at all.
fn install_now_playing(
    tray: &TraySlot,
    mpris: &Option<Rc<crate::mpris::MprisService>>,
    state: &Arc<AppState>,
) {
    let tray = Rc::clone(tray);
    let mpris = mpris.clone();
    let state = Arc::clone(state);
    crate::ui_event_bridge::set_now_playing_handler(move |now| {
        if let Some(tray) = tray.borrow().as_ref() {
            tray.set_tooltip(&match now.as_ref() {
                Some(now) if now.paused => format!("Paused: {}", now.title),
                Some(now) => format!("Playing: {}", now.title),
                None => String::new(),
            });
        }
        if let Some(mpris) = mpris.as_ref() {
            // Read here rather than at the call site so the setting can be
            // turned on or off without restarting.
            let enabled = state.config.lock().settings.mpris_enabled;
            mpris.set_enabled(enabled);
            mpris.set_now_playing(if enabled { now } else { None });
        }
    });
}

fn start_tray_service(
    connection: &gio::DBusConnection,
    state: &Arc<AppState>,
) -> Option<Rc<crate::tray::TrayService>> {
    let real_mic_muted = !state.config.lock().settings.mic_passthrough;
    match crate::tray::TrayService::start(
        connection,
        crate::tray::menu::build(true, real_mic_muted),
        crate::ui_event_bridge::post_tray_action,
    ) {
        Ok(tray) => Some(Rc::new(tray)),
        Err(error) => {
            warn!("Could not export a tray icon: {error}");
            None
        }
    }
}

fn schedule_startup_loudness_backfill(state: Arc<AppState>, _timer_registry: &TimerRegistry) {
    glib::idle_add_local_once(move || {
        crate::diagnostics::memory::log_memory_snapshot("startup:loudness_bg:check");
        let config = Arc::clone(&state.config);
        let library = state.library.clone();
        let coords = state.loudness_coordinators.clone();
        if let Err(error) = crate::commands::dispatch_async_result(
            "startup_loudness_backfill",
            move || {
                crate::commands::trigger_missing_loudness_analysis_with_store(
                    config, library, false, None, &coords,
                )
            },
            |result| {
                if let Err(error) = result {
                    log::warn!("Failed to schedule startup loudness analysis: {error}");
                }
            },
        ) {
            log::warn!("Failed to dispatch startup loudness check: {error}");
        }
    });
}

fn schedule_startup_hotkey_projection(state: Arc<AppState>) {
    let projection = state.hotkey_projection.clone();
    let completion_state = Arc::clone(&state);
    if let Err(error) = crate::commands::dispatch_async_result(
        "project_startup_hotkeys",
        move || projection.reconcile_blocking(),
        move |result| {
            let status = match result {
                Ok(()) => completion_state.hotkeys.lock().status_message(),
                Err(error) => {
                    log::error!("Could not project persisted hotkeys: {error}");
                    "Hotkeys: Error (see logs)".to_string()
                }
            };
            crate::diagnostics::set_hotkey_status(&status);
            crate::diagnostics::memory::log_memory_snapshot("startup:hotkeys_ready");
            record_state_phase("startup:hotkeys_ready", &completion_state);
        },
    ) {
        log::error!("Could not schedule persisted hotkey projection: {error}");
        crate::diagnostics::set_hotkey_status("Hotkeys: Error (see logs)");
    }
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

fn compatible_engine_running() -> bool {
    let Ok(info) = crate::audio::engine_ipc::engine_info() else {
        return false;
    };
    crate::audio::engine_ipc::engine_info_compatible(&info)
}

fn initialize_player(
    config: &Config,
    startup_mode: StartupMode,
) -> Result<(crate::audio::AudioPlayer, bool), String> {
    use crate::audio::AudioBackendKind;

    // Debug aid: with route audit on, skip the systemd-spawned engine entirely.
    // The unit inherits none of the user's environment, so that engine would
    // never see `LSB_ROUTE_AUDIT` — and since routing writes happen in the
    // engine, the log would stay empty. In-process keeps it all here, where
    // init_from_env() already opened the file.
    let force_in_process = crate::diagnostics::audit::is_enabled();
    if force_in_process {
        log::warn!(
            "LSB_ROUTE_AUDIT is enabled — running audio engine in-process to capture writes \
             (the systemd-spawned engine would not inherit the env var)"
        );
        stop_audio_engine_service_and_process();
    } else {
        if startup_mode == StartupMode::Persistent {
            // Reload and start the target even when a compatible engine is already
            // connected so an upgraded RefuseManualStop policy takes effect.
            let _ = manage_audio_engine_service(ServiceAction::Start);
        }
        let remote = match startup_mode {
            StartupMode::Persistent => connect_or_start_audio_engine(
                crate::audio::AudioPlayer::connect_to_engine,
                crate::audio::engine_ipc::engine_running,
                crate::audio::engine_ipc::shutdown_incompatible_engine_if_running,
                manage_audio_engine_service,
                || {},
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
            return Ok((player, true));
        }
        if startup_mode == StartupMode::Persistent {
            return Err(
                "Linux Soundboard could not connect to its persistent audio engine, so no virtual microphone was created. Run temporarily to start an engine inside this window for the session, or exit and repair the user service."
                    .to_string(),
            );
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
    Ok((
        crate::audio::AudioPlayer::new_with_config_and_audio_backend(config, backend),
        false,
    ))
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
        .args(["--user", "stop", ENGINE_TARGET_UNIT])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    crate::audio::engine_ipc::shutdown_engine_if_running();
}

fn manage_audio_engine_service(action: ServiceAction) -> bool {
    ensure_user_audio_engine_units();
    let reload = std::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    if !matches!(reload, Ok(status) if status.success()) {
        return false;
    }

    let _ = std::process::Command::new("systemctl")
        .args(["--user", "disable", ENGINE_SERVICE_UNIT])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    // Clears the start-rate limit left by an earlier broken engine.
    let _ = std::process::Command::new("systemctl")
        .args(["--user", "reset-failed", ENGINE_SERVICE_UNIT])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    let args: &[&str] = match action {
        ServiceAction::Start => &["--user", "enable", "--now", ENGINE_TARGET_UNIT],
        ServiceAction::Restart => &["--user", "restart", ENGINE_TARGET_UNIT],
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

fn ensure_user_audio_engine_units() {
    let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
    else {
        return;
    };
    let unit_dir = config_home.join("systemd").join("user");
    let service_path = unit_dir.join(ENGINE_SERVICE_UNIT);
    let target_path = unit_dir.join(ENGINE_TARGET_UNIT);
    if packaged_audio_engine_unit_exists(ENGINE_SERVICE_UNIT)
        && packaged_audio_engine_unit_exists(ENGINE_TARGET_UNIT)
    {
        return;
    }

    // Prefer the install: the AppImage this runs from can be moved or deleted.
    let executable = dirs::home_dir()
        .map(|home| stable_user_binary_path(&home))
        .filter(|path| path.is_file())
        .or_else(|| std::env::var_os("APPIMAGE").map(PathBuf::from))
        .or_else(|| std::env::current_exe().ok());
    let Some(executable) = executable else {
        return;
    };
    if service_path.exists() {
        let Ok(existing) = std::fs::read_to_string(&service_path) else {
            return;
        };
        let managed = existing.contains("X-LinuxSoundBoard-Managed=true")
            || existing.contains("# managed-by: linux-soundboard")
            || existing == render_legacy_audio_engine_service(&executable);
        if !managed {
            return;
        }
    } else if systemd_user_unit_exists(ENGINE_SERVICE_UNIT)
        || packaged_audio_engine_unit_exists(ENGINE_SERVICE_UNIT)
    {
        return;
    }

    if target_path.exists() {
        let Ok(existing) = std::fs::read_to_string(&target_path) else {
            return;
        };
        if !existing.contains("X-LinuxSoundBoard-Managed=true")
            && !existing.contains("# managed-by: linux-soundboard")
        {
            return;
        }
    }

    if std::fs::create_dir_all(&unit_dir).is_err() {
        return;
    }

    let body = render_audio_engine_service(&executable);
    if std::fs::write(&service_path, body).is_err() {
        return;
    }
    if std::fs::write(&target_path, render_audio_engine_target()).is_err() {
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

fn packaged_audio_engine_unit_exists(service: &str) -> bool {
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

/// A type-2 AppImage is an ELF with the magic bytes `AI\x02` at offset 8.
fn is_appimage(path: &std::path::Path) -> bool {
    use std::io::Read;

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut header = [0u8; 11];
    if file.read_exact(&mut header).is_err() {
        return false;
    }
    header[0..4] == [0x7f, b'E', b'L', b'F'] && header[8..11] == [b'A', b'I', 0x02]
}

fn render_audio_engine_service(executable: &std::path::Path) -> String {
    // All three imply NoNewPrivileges — the seccomp ones implicitly — which blocks
    // the setuid fusermount an AppImage needs to mount itself.
    let hardening = if is_appimage(executable) {
        ""
    } else {
        "NoNewPrivileges=yes\nRestrictSUIDSGID=yes\nLockPersonality=yes\n"
    };
    format!(
        "[Unit]\n\
Description=Linux Soundboard audio engine\n\
After=pipewire.service pipewire-pulse.service wireplumber.service pulseaudio.service\n\
PartOf=linux-soundboard-engine.target\n\
RefuseManualStop=yes\n\
StartLimitIntervalSec=60\n\
StartLimitBurst=5\n\
X-LinuxSoundBoard-Managed=true\n\
\n\
[Service]\n\
Type=exec\n\
ExecStart={} --audio-engine\n\
Restart=on-failure\n\
RestartSec=2\n\
RestartPreventExitStatus=2\n\
{hardening}",
        systemd_quote(executable)
    )
}

fn render_audio_engine_target() -> &'static str {
    "[Unit]\n\
Description=Linux Soundboard persistent audio engine\n\
Wants=linux-soundboard-engine.service\n\
X-LinuxSoundBoard-Managed=true\n\
\n\
[Install]\n\
WantedBy=default.target\n"
}

fn render_legacy_audio_engine_service(executable: &std::path::Path) -> String {
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

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("lsb-bootstrap-{label}-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }

        fn config_path(&self) -> PathBuf {
            self.0.join("config.json")
        }

        fn library_path(&self) -> PathBuf {
            self.0.join("library.sqlite3")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn prepared_application_can_cross_the_startup_worker_boundary() {
        fn assert_send<T: Send>() {}

        assert_send::<PreparedApplication>();
    }

    #[test]
    fn migration_progress_disables_cancellation_before_publication() {
        use crate::legacy_migration::LegacyMigrationProgress;

        assert!(migration_progress_can_cancel(
            LegacyMigrationProgress::Importing
        ));
        assert!(!migration_progress_can_cancel(
            LegacyMigrationProgress::PublishingDatabase
        ));
        assert_eq!(
            migration_progress_message(LegacyMigrationProgress::Verifying),
            "Verifying the upgraded library…"
        );
    }

    fn write_legacy_config(path: &Path) -> Vec<u8> {
        let mut config = crate::test_support::legacy_config::LegacyConfigFixture::default();
        config.sound_folders.push("/music".to_string());
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        let bytes = serde_json::to_vec(&config).expect("serialize legacy config");
        fs::write(path, &bytes).expect("write legacy config");
        bytes
    }

    #[test]
    fn startup_preflight_creates_one_matching_empty_library() {
        let temp = TestDir::new("new-storage");

        assert_eq!(
            prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
                .expect("prepare new storage"),
            StoragePreparation::Ready
        );

        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.config_path()).expect("read settings"))
                .expect("parse settings");
        let identity = crate::legacy_migration::database_identity(&temp.library_path())
            .expect("load database identity");
        assert!(persisted.get("library_id").is_none());
        assert!(!identity.library_id.is_empty());
    }

    #[test]
    fn startup_preflight_leaves_unconfirmed_legacy_config_untouched() {
        let temp = TestDir::new("legacy-prompt");
        let original = write_legacy_config(&temp.config_path());

        assert_eq!(
            prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
                .expect("inspect legacy storage"),
            StoragePreparation::NeedsLegacyMigration
        );
        assert_eq!(fs::read(temp.config_path()).unwrap(), original);
        assert!(!temp.library_path().exists());
    }

    #[test]
    fn startup_preflight_resumes_after_database_publication() {
        let temp = TestDir::new("resume-cutover");
        write_legacy_config(&temp.config_path());
        let report = crate::legacy_migration::migrate_legacy_database(
            &temp.config_path(),
            &temp.library_path(),
        )
        .expect("publish database");

        assert_eq!(
            prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
                .expect("resume cutover"),
            StoragePreparation::Ready
        );
        let config = Config::load_from_path(&temp.config_path()).expect("load schema-8 settings");
        assert_eq!(config.schema_version, crate::config::CURRENT_SCHEMA_VERSION);
        assert!(!report.library_id.is_empty());
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(temp.config_path()).expect("read settings"))
                .expect("parse settings");
        assert!(persisted.get("library_id").is_none());
    }

    #[test]
    fn startup_preflight_offers_explicit_recovery_for_schema_8_without_its_database() {
        let temp = TestDir::new("missing-database");
        let mut config = Config::default();
        config
            .save_to_path(&temp.config_path())
            .expect("write schema-8 settings");

        let preparation = prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
            .expect("missing database should offer an explicit recovery choice");

        assert!(matches!(
            preparation,
            StoragePreparation::NeedsEmptyLibraryRecovery { ref library_id }
                if !library_id.is_empty()
        ));
        assert!(!temp.library_path().exists());
    }

    #[test]
    fn startup_preflight_uses_ready_database_when_obsolete_json_identity_differs() {
        let temp = TestDir::new("mismatched-database");
        let mut config = Config::default();
        config
            .save_to_path(&temp.config_path())
            .expect("write schema-8 settings");
        let mut persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(temp.config_path()).expect("read settings"))
                .expect("parse settings");
        persisted["library_id"] = serde_json::Value::String("obsolete-library".to_string());
        std::fs::write(
            temp.config_path(),
            serde_json::to_vec_pretty(&persisted).expect("serialize settings"),
        )
        .expect("write obsolete identity");
        crate::legacy_migration::initialize_empty_library(
            &temp.library_path(),
            "different-library",
        )
        .expect("create mismatched database");

        let preparation = prepare_startup_storage_at(&temp.config_path(), &temp.library_path())
            .expect("ready canonical database must remain authoritative");

        assert!(matches!(preparation, StoragePreparation::Ready));
    }

    #[test]
    fn storage_lock_excludes_a_second_writer_until_drop() {
        let temp = TestDir::new("storage-lock");
        let lock_path = temp.0.join("storage.lock");

        let first = acquire_storage_lock_at(&lock_path).expect("acquire first lock");
        assert!(acquire_storage_lock_at(&lock_path).is_err());
        drop(first);
        assert!(acquire_storage_lock_at(&lock_path).is_ok());
    }

    #[test]
    fn x11_and_vmware_choose_the_bounded_fallback_renderer() {
        assert!(should_use_fallback_renderer(Some(X11_BACKEND), false));
        assert!(should_use_fallback_renderer(Some(WAYLAND_BACKEND), true));
        assert!(!should_use_fallback_renderer(Some(WAYLAND_BACKEND), false));
    }

    #[test]
    fn audio_engine_service_renders_quoted_exec() {
        let service = render_audio_engine_service(Path::new("/tmp/Linux Soundboard.AppImage"));
        assert!(service.contains("ExecStart=\"/tmp/Linux Soundboard.AppImage\" --audio-engine"));
        assert!(service.contains("StartLimitBurst=5"));
        assert!(service.contains(
            "After=pipewire.service pipewire-pulse.service wireplumber.service pulseaudio.service"
        ));
        assert!(service.contains("X-LinuxSoundBoard-Managed=true"));
        assert!(service.contains("PartOf=linux-soundboard-engine.target"));
        assert!(service.contains("RefuseManualStop=yes"));
        assert!(!service.contains("WantedBy=default.target"));
        assert!(service.contains("RestartPreventExitStatus=2"));
    }

    #[test]
    fn appimage_engine_service_drops_no_new_privileges() {
        let temp = TestDir::new("engine-unit");
        let native = temp.0.join("linux-soundboard");
        let appimage = temp.0.join("soundboard.AppImage");
        let mut header = [0u8; 16];
        header[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        fs::write(&native, header).expect("write native binary");
        header[8..11].copy_from_slice(b"AI\x02");
        fs::write(&appimage, header).expect("write appimage");

        let native_service = render_audio_engine_service(&native);
        assert!(native_service.contains("NoNewPrivileges=yes"));
        assert!(native_service.contains("RestrictSUIDSGID=yes"));
        assert!(native_service.contains("LockPersonality=yes"));

        // Each of these implies NoNewPrivileges, which blocks the AppImage's mount.
        let service = render_audio_engine_service(&appimage);
        assert!(!service.contains("NoNewPrivileges"));
        assert!(!service.contains("RestrictSUIDSGID"));
        assert!(!service.contains("LockPersonality"));
        assert!(service.contains("--audio-engine"));
    }

    #[test]
    fn packaged_engine_target_owns_the_protected_service() {
        let target = include_str!("../../packaging/linux/linux-soundboard-engine.target");
        assert!(target.contains("Wants=linux-soundboard-engine.service"));
        assert!(target.contains("WantedBy=default.target"));
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
