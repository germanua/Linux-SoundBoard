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
    let is_system_stable = crate::app_meta::BUILD_PROFILE == "stable"
        && path == std::path::Path::new("/usr/bin/linux-soundboard");
    if is_system_stable || path == stable_user_binary {
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
        let mut parts = version.strip_prefix('v').unwrap_or(version).split('.');
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

    let force_in_process = crate::diagnostics::audit::is_enabled();
    if force_in_process {
        log::warn!(
            "LSB_ROUTE_AUDIT is enabled — running audio engine in-process to capture writes \
             (the systemd-spawned engine would not inherit the env var)"
        );
        stop_audio_engine_service_and_process();
    } else {
        if startup_mode == StartupMode::Persistent {
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
    let _ = crate::audio::command_runner::run_command(
        "systemctl",
        &["--user", "stop", ENGINE_TARGET_UNIT],
    );
    crate::audio::engine_ipc::shutdown_engine_if_running();
}

fn manage_audio_engine_service(action: ServiceAction) -> bool {
    ensure_user_audio_engine_units();
    let reload =
        crate::audio::command_runner::run_command("systemctl", &["--user", "daemon-reload"]);
    if !matches!(reload, Ok(output) if output.success) {
        return false;
    }

    let _ = crate::audio::command_runner::run_command(
        "systemctl",
        &["--user", "disable", ENGINE_SERVICE_UNIT],
    );

    let _ = crate::audio::command_runner::run_command(
        "systemctl",
        &["--user", "reset-failed", ENGINE_SERVICE_UNIT],
    );

    let args: &[&str] = match action {
        ServiceAction::Start => &["--user", "enable", "--now", ENGINE_TARGET_UNIT],
        ServiceAction::Restart => &["--user", "restart", ENGINE_TARGET_UNIT],
    };
    matches!(
        crate::audio::command_runner::run_command("systemctl", args),
        Ok(output) if output.success
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
    let _ = crate::audio::command_runner::run_command("systemctl", &["--user", "daemon-reload"]);
}

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
    crate::audio::command_runner::run_command("systemctl", &["--user", "cat", service])
        .map(|output| output.success)
        .unwrap_or(false)
}

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
    let hardening = if is_appimage(executable) {
        ""
    } else {
        "NoNewPrivileges=yes\nRestrictSUIDSGID=yes\nLockPersonality=yes\n"
    };
    format!(
        "[Unit]\n\
Description=Linux Soundboard audio engine\n\
After=pipewire.service pipewire-pulse.service wireplumber.service pulseaudio.service\n\
PartOf={ENGINE_TARGET_UNIT}\n\
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

fn render_audio_engine_target() -> String {
    format!(
        "[Unit]\n\
Description=Linux Soundboard persistent audio engine\n\
Wants={ENGINE_SERVICE_UNIT}\n\
X-LinuxSoundBoard-Managed=true\n\
\n\
[Install]\n\
WantedBy=default.target\n"
    )
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
