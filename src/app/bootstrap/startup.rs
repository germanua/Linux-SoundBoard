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
    if let Some(result) = crate::update::apply_from_args() {
        if let Err(error) = result {
            log::error!("Staged update failed: {error}");
            std::process::exit(1);
        }
        std::process::exit(0);
    }
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
        .join("usr/libexec")
        .join(APP_BINARY)
        .join("installer/install-user.sh");
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
