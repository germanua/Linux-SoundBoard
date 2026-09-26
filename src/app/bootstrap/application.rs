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
    install_now_playing(&tray, &mpris);
    if let Some(mpris) = mpris.as_ref() {
        mpris.set_enabled(state.config.lock().settings.mpris_enabled);
    }

    let state_close = Arc::clone(&state);
    let timers_close = timer_registry.clone();
    window.connect_close_request(move |_| {
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
    crate::update::mark_update_healthy();
    show_pending_update_notice(&window);
    if let Some(notice) = engine_update_notice {
        show_engine_update_notice(&window, notice);
    }
    record_state_phase("startup:window_presented", &state);

    schedule_startup_hotkey_projection(Arc::clone(&state));
    schedule_startup_loudness_backfill(Arc::clone(&state), &timer_registry);
    schedule_library_diagnostics(Arc::clone(&state));
    schedule_automatic_update_check();

    {
        let state_idle = Arc::clone(&state);
        glib::timeout_add_local_once(Duration::from_secs(5), move || {
            record_state_phase("idle:5s", &state_idle);
        });
    }
}

fn show_pending_update_notice(parent: &gtk4::ApplicationWindow) {
    let notice = match crate::update::take_pending_notice() {
        Ok(notice) => notice,
        Err(error) => {
            log::warn!("Could not read updater result: {error}");
            return;
        }
    };
    match notice {
        Some(crate::update::state::PendingNotice::Updated { version }) => {
            crate::ui_event_bridge::post_toast(format!("Linux Soundboard updated to {version}"));
        }
        Some(crate::update::state::PendingNotice::RestoredAfterFailure) => {
            let dialog = adw::AlertDialog::new(
                Some("Update could not be completed"),
                Some("Linux Soundboard restored the previous version and your saved application state."),
            );
            dialog.add_response("ok", "OK");
            dialog.set_close_response("ok");
            dialog.set_default_response(Some("ok"));
            dialog.choose(parent, None::<&gio::Cancellable>, |_| {});
        }
        None => {}
    }
}

fn schedule_automatic_update_check() {
    glib::timeout_add_local_once(Duration::from_secs(12), move || {
        if let Err(error) = crate::commands::dispatch_async_result(
            "automatic_update_check",
            crate::update::check_automatic_if_due,
            move |result| match result {
                Ok(Some(crate::update::CheckOutcome::Available(info))) => {
                    crate::ui_event_bridge::post_update_available(*info);
                }
                Ok(Some(crate::update::CheckOutcome::UpToDate { .. })) | Ok(None) => {}
                Err(error) => log::debug!("Automatic update check failed: {error}"),
            },
        ) {
            log::debug!("Could not dispatch automatic update check: {error}");
        }
    });
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

type TraySlot = Rc<RefCell<Option<Rc<crate::tray::TrayService>>>>;

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

fn install_now_playing(tray: &TraySlot, mpris: &Option<Rc<crate::mpris::MprisService>>) {
    let tray = Rc::clone(tray);
    let now_playing = mpris.clone();
    crate::ui_event_bridge::set_now_playing_handler(move |now| {
        if let Some(tray) = tray.borrow().as_ref() {
            tray.set_tooltip(&match now.as_ref() {
                Some(now) if now.paused => format!("Paused: {}", now.title),
                Some(now) => format!("Playing: {}", now.title),
                None => String::new(),
            });
        }
        if let Some(mpris) = now_playing.as_ref() {
            mpris.set_now_playing(now);
        }
    });

    let mpris = mpris.clone();
    crate::ui_event_bridge::set_mpris_enabled_handler(move |enabled| {
        if let Some(mpris) = mpris.as_ref() {
            mpris.set_enabled(enabled);
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
