use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use super::release::UpdateMetadata;
use super::verifier::{parse_manifest, verify_bytes, verify_file, verify_manifest};
use super::{state, UpdateError};

const APPLY_FLAG: &str = "--apply-staged-update";
const HEALTH_ENV: &str = "LSB_UPDATE_HEALTH_TOKEN";
const HEALTH_TIMEOUT: Duration = Duration::from_secs(120);
const OLD_PROCESS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Serialize, Deserialize)]
struct RollbackRecord {
    token: String,
    previous_version: Option<String>,
    install_root_present: bool,
    config_present: bool,
    library_present: bool,
    service_present: bool,
    target_present: bool,
}

struct VerifiedStaging {
    appimage: PathBuf,
    expected_sha256: String,
    metadata: UpdateMetadata,
}

pub fn parse_apply_args() -> Option<Result<(u32, u64, PathBuf), UpdateError>> {
    let args: Vec<_> = std::env::args_os().collect();
    let position = args.iter().position(|arg| arg == APPLY_FLAG)?;
    let pid = args
        .get(position + 1)
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<u32>().ok());
    let ticks = args
        .get(position + 2)
        .and_then(|value| value.to_str())
        .and_then(|value| value.parse::<u64>().ok());
    let previous_appimage = args.get(position + 3).map(PathBuf::from);
    Some(match (pid, ticks, previous_appimage) {
        (Some(pid), Some(ticks), Some(previous_appimage)) if pid > 1 => {
            Ok((pid, ticks, previous_appimage))
        }
        _ => Err(UpdateError::State(
            "invalid staged-update handoff arguments".into(),
        )),
    })
}

pub fn launch(staged_appimage: &Path) -> Result<(), UpdateError> {
    validate_user_executable(staged_appimage)?;
    let previous_appimage = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .ok_or_else(|| {
            UpdateError::State("APPIMAGE is unavailable before update handoff".into())
        })?;
    validate_user_executable(&previous_appimage)?;
    let pid = std::process::id();
    let ticks = process_start_ticks(pid)?;
    Command::new(staged_appimage)
        .arg(APPLY_FLAG)
        .arg(pid.to_string())
        .arg(ticks.to_string())
        .arg(&previous_appimage)
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("OWD")
        .spawn()
        .map_err(|error| UpdateError::State(format!("could not start staged updater: {error}")))?;
    Ok(())
}

pub fn run(old_pid: u32, old_ticks: u64, previous_appimage: PathBuf) -> Result<(), UpdateError> {
    validate_user_executable(&previous_appimage)?;
    let verified = match verify_staging() {
        Ok(verified) => verified,
        Err(error) => {
            recover_preinstall(old_pid, old_ticks, &previous_appimage);
            return Err(error);
        }
    };
    wait_for_process_exit(old_pid, old_ticks, OLD_PROCESS_TIMEOUT)?;
    let _ = crate::audio::engine_ipc::shutdown_incompatible_engine_if_running();
    let rollback = match create_rollback_snapshot() {
        Ok(rollback) => rollback,
        Err(error) => {
            record_restore_notice();
            let _ = launch_previous_install(&previous_appimage);
            return Err(error);
        }
    };
    let result =
        install_verified_update(&verified).and_then(|_| launch_installed_and_monitor(&rollback));
    if let Err(error) = result {
        let _ = crate::audio::engine_ipc::shutdown_engine_if_running();
        let restore_result = restore_rollback_snapshot(&rollback);
        if restore_result.is_ok() {
            record_restore_notice();
            let _ = relaunch_after_rollback(&rollback, &previous_appimage);
        }
        return match restore_result {
            Ok(()) => Err(error),
            Err(restore_error) => Err(UpdateError::State(format!(
                "{error}; rollback also failed: {restore_error}"
            ))),
        };
    }
    Ok(())
}

fn record_restore_notice() {
    if let Ok(mut persisted) = state::load() {
        persisted.staged_version = None;
        persisted.pending_notice = Some(state::PendingNotice::RestoredAfterFailure);
        let _ = state::save(&persisted);
    }
}

fn recover_preinstall(old_pid: u32, old_ticks: u64, previous_appimage: &Path) {
    if wait_for_process_exit(old_pid, old_ticks, OLD_PROCESS_TIMEOUT).is_ok() {
        record_restore_notice();
        let _ = launch_previous_install(previous_appimage);
    }
}

fn validate_user_executable(path: &Path) -> Result<(), UpdateError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| UpdateError::State(error.to_string()))?;
    if !metadata.file_type().is_file()
        || metadata.uid() != nix::unistd::getuid().as_raw()
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err(UpdateError::State("unsafe previous AppImage path".into()));
    }
    Ok(())
}

fn launch_previous_install(path: &Path) -> Result<Child, UpdateError> {
    validate_user_executable(path)?;
    let mut command = Command::new(path);
    command
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("OWD")
        .env_remove(HEALTH_ENV);
    command.spawn().map_err(|error| {
        UpdateError::State(format!(
            "could not relaunch previous Linux Soundboard: {error}"
        ))
    })
}

fn relaunch_after_rollback(
    record: &RollbackRecord,
    previous_appimage: &Path,
) -> Result<Child, UpdateError> {
    if record.install_root_present {
        launch_stable_install(None)
    } else {
        launch_previous_install(previous_appimage)
    }
}

pub fn mark_healthy_from_env() {
    let Ok(token) = std::env::var(HEALTH_ENV) else {
        return;
    };
    std::env::remove_var(HEALTH_ENV);
    if uuid::Uuid::parse_str(&token).is_err() {
        return;
    }
    let Ok(path) = health_path(&token) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if !parent.is_dir() {
        return;
    }
    let temporary = parent.join(format!(".healthy-{}", uuid::Uuid::new_v4()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(&temporary);
    let marker_written = if let Ok(mut file) = file {
        if file
            .write_all(crate::app_meta::APP_VERSION.as_bytes())
            .and_then(|_| file.sync_all())
            .is_ok()
        {
            fs::rename(&temporary, &path).is_ok()
        } else {
            let _ = fs::remove_file(&temporary);
            false
        }
    } else {
        false
    };
    if !marker_written {
        return;
    }
    let Ok(version) = Version::parse(crate::app_meta::APP_VERSION) else {
        return;
    };
    let Ok(mut persisted) = state::load() else {
        return;
    };
    persisted.staged_version = None;
    persisted.pending_notice = Some(state::PendingNotice::Updated { version });
    let _ = state::save(&persisted);
}

fn verify_staging() -> Result<VerifiedStaging, UpdateError> {
    let appimage = std::env::var_os("APPIMAGE")
        .map(PathBuf::from)
        .ok_or_else(|| UpdateError::State("APPIMAGE is unavailable in updater mode".into()))?;
    let metadata =
        fs::symlink_metadata(&appimage).map_err(|error| UpdateError::State(error.to_string()))?;
    if !metadata.file_type().is_file() || metadata.uid() != nix::unistd::getuid().as_raw() {
        return Err(UpdateError::State("unsafe updater AppImage path".into()));
    }
    let dir = appimage
        .parent()
        .ok_or_else(|| UpdateError::State("staged AppImage has no parent".into()))?;
    let expected_dir = state::root()?
        .join("updates")
        .join(crate::app_meta::APP_VERSION);
    let canonical_dir =
        fs::canonicalize(dir).map_err(|error| UpdateError::State(error.to_string()))?;
    let canonical_expected =
        fs::canonicalize(&expected_dir).map_err(|error| UpdateError::State(error.to_string()))?;
    if canonical_dir != canonical_expected {
        return Err(UpdateError::State(
            "updater AppImage is outside the private staging directory".into(),
        ));
    }

    let manifest = fs::read(dir.join("SHA256SUMS.txt"))
        .map_err(|error| UpdateError::State(error.to_string()))?;
    let signature = fs::read_to_string(dir.join("SHA256SUMS.txt.minisig"))
        .map_err(|error| UpdateError::State(error.to_string()))?;
    let metadata_json =
        fs::read(dir.join("update.json")).map_err(|error| UpdateError::State(error.to_string()))?;
    let tag = format!("v{}", crate::app_meta::APP_VERSION);
    verify_manifest(&manifest, &signature, &tag)?;
    let entries = parse_manifest(&manifest)?;
    verify_bytes("update.json", &metadata_json, &entries)?;
    let update: UpdateMetadata = serde_json::from_slice(&metadata_json)
        .map_err(|error| UpdateError::Metadata(error.to_string()))?;
    if update.schema != 1
        || update.channel != crate::app_meta::UPDATE_CHANNEL
        || update.version
            != Version::parse(crate::app_meta::APP_VERSION)
                .map_err(|error| UpdateError::Metadata(error.to_string()))?
        || update.tag != tag
    {
        return Err(UpdateError::Verification(
            "staged update metadata does not match the running updater".into(),
        ));
    }
    let name = appimage
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| UpdateError::State("invalid staged AppImage filename".into()))?;
    if name != update.appimage.name || metadata.len() != update.appimage.size {
        return Err(UpdateError::Verification(
            "staged AppImage does not match signed metadata".into(),
        ));
    }
    verify_file(name, &appimage, &entries)?;
    validate_type2_appimage(&appimage)?;
    let expected_sha256 = entries.get(name).cloned().ok_or_else(|| {
        UpdateError::Verification("staged AppImage is missing from the signed manifest".into())
    })?;
    Ok(VerifiedStaging {
        appimage,
        expected_sha256,
        metadata: update,
    })
}

fn validate_type2_appimage(path: &Path) -> Result<(), UpdateError> {
    let mut file = fs::File::open(path).map_err(|error| UpdateError::State(error.to_string()))?;
    let mut header = [0u8; 11];
    file.read_exact(&mut header)
        .map_err(|error| UpdateError::Verification(error.to_string()))?;
    if header[0..4] == [0x7f, b'E', b'L', b'F'] && header[8..11] == [b'A', b'I', 0x02] {
        Ok(())
    } else {
        Err(UpdateError::Verification(
            "staged updater is not a type-2 AppImage".into(),
        ))
    }
}

fn process_start_ticks(pid: u32) -> Result<u64, UpdateError> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))
        .map_err(|error| UpdateError::State(error.to_string()))?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| UpdateError::State("invalid /proc stat format".into()))?;
    stat[close + 1..]
        .split_whitespace()
        .nth(19)
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| UpdateError::State("could not read process start time".into()))
}

fn process_matches(pid: u32, ticks: u64) -> bool {
    process_start_ticks(pid).is_ok_and(|current| current == ticks)
}

fn wait_for_process_exit(pid: u32, ticks: u64, timeout: Duration) -> Result<(), UpdateError> {
    let started = Instant::now();
    while process_matches(pid, ticks) {
        if started.elapsed() >= timeout {
            return Err(UpdateError::State(
                "the previous Linux Soundboard process did not exit".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn stable_install_root() -> Result<PathBuf, UpdateError> {
    dirs::home_dir()
        .map(|home| home.join(".local/opt").join(crate::app_meta::APP_BINARY))
        .ok_or_else(|| UpdateError::State("could not resolve home directory".into()))
}

fn stable_binary() -> Result<PathBuf, UpdateError> {
    Ok(stable_install_root()?.join(crate::app_meta::APP_BINARY))
}

fn config_home() -> Result<PathBuf, UpdateError> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| UpdateError::State("could not resolve config directory".into()))
}

fn copy_file_private(source: &Path, destination: &Path, mode: u32) -> Result<(), UpdateError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| UpdateError::State(error.to_string()))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| UpdateError::State(error.to_string()))?;
    }
    fs::copy(source, destination).map_err(|error| UpdateError::State(error.to_string()))?;
    fs::set_permissions(destination, fs::Permissions::from_mode(mode))
        .map_err(|error| UpdateError::State(error.to_string()))
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), UpdateError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|error| UpdateError::State(error.to_string()))?;
    if !metadata.file_type().is_dir() {
        return Err(UpdateError::State(
            "rollback source is not a directory".into(),
        ));
    }
    fs::create_dir_all(destination).map_err(|error| UpdateError::State(error.to_string()))?;
    fs::set_permissions(
        destination,
        fs::Permissions::from_mode(metadata.permissions().mode() & 0o777),
    )
    .map_err(|error| UpdateError::State(error.to_string()))?;
    for entry in fs::read_dir(source).map_err(|error| UpdateError::State(error.to_string()))? {
        let entry = entry.map_err(|error| UpdateError::State(error.to_string()))?;
        let metadata = entry
            .metadata()
            .map_err(|error| UpdateError::State(error.to_string()))?;
        let file_type = entry
            .file_type()
            .map_err(|error| UpdateError::State(error.to_string()))?;
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if file_type.is_file() {
            copy_file_private(
                &entry.path(),
                &target,
                metadata.permissions().mode() & 0o777,
            )?;
        } else {
            return Err(UpdateError::State(
                "rollback snapshot refuses symbolic links and special files".into(),
            ));
        }
    }
    Ok(())
}

fn create_rollback_snapshot() -> Result<RollbackRecord, UpdateError> {
    let token = uuid::Uuid::new_v4().to_string();
    let root = rollback_dir(&token)?;
    fs::create_dir_all(&root).map_err(|error| UpdateError::State(error.to_string()))?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
        .map_err(|error| UpdateError::State(error.to_string()))?;

    let install_root = stable_install_root()?;
    let config = crate::config::Config::config_path();
    let library = config.with_file_name("library.sqlite3");
    let units = config_home()?.join("systemd/user");
    let service = units.join(crate::app_meta::ENGINE_SERVICE_NAME);
    let target = units.join(crate::app_meta::ENGINE_TARGET_NAME);
    let version = install_root.join(".installed-version");
    let record = RollbackRecord {
        token: token.clone(),
        previous_version: fs::read_to_string(&version)
            .ok()
            .map(|value| value.trim().to_string()),
        install_root_present: install_root.is_dir(),
        config_present: config.is_file(),
        library_present: library.is_file(),
        service_present: service.is_file(),
        target_present: target.is_file(),
    };
    if record.install_root_present {
        copy_tree(&install_root, &root.join("install-root"))?;
    }
    if record.config_present {
        copy_file_private(&config, &root.join("config.json"), 0o600)?;
    }
    if record.library_present {
        backup_database(&library, &root.join("library.sqlite3"))?;
    }
    if record.service_present {
        copy_file_private(&service, &root.join("engine.service"), 0o600)?;
    }
    if record.target_present {
        copy_file_private(&target, &root.join("engine.target"), 0o600)?;
    }
    let record_bytes = serde_json::to_vec_pretty(&record)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    write_private_file(&root.join("rollback.json"), &record_bytes)?;
    Ok(record)
}

fn backup_database(source: &Path, destination: &Path) -> Result<(), UpdateError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| UpdateError::State(error.to_string()))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
            .map_err(|error| UpdateError::State(error.to_string()))?;
    }
    let source = rusqlite::Connection::open(source)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    let mut destination_connection = rusqlite::Connection::open(destination)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    let backup = rusqlite::backup::Backup::new(&source, &mut destination_connection)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    backup
        .run_to_completion(128, Duration::from_millis(10), None)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    drop(backup);
    drop(destination_connection);
    fs::set_permissions(destination, fs::Permissions::from_mode(0o600))
        .map_err(|error| UpdateError::State(error.to_string()))
}

fn rollback_dir(token: &str) -> Result<PathBuf, UpdateError> {
    Ok(state::root()?.join("updates/backups").join(token))
}

fn health_path(token: &str) -> Result<PathBuf, UpdateError> {
    Ok(rollback_dir(token)?.join("healthy"))
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW)
        .open(path)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    file.write_all(bytes)
        .map_err(|error| UpdateError::State(error.to_string()))?;
    file.sync_all()
        .map_err(|error| UpdateError::State(error.to_string()))
}

fn install_verified_update(verified: &VerifiedStaging) -> Result<(), UpdateError> {
    let appdir = std::env::var_os("APPDIR")
        .map(PathBuf::from)
        .ok_or_else(|| UpdateError::State("APPDIR is unavailable in updater mode".into()))?;
    let installer = appdir
        .join("usr/libexec")
        .join(crate::app_meta::APP_BINARY)
        .join("installer")
        .join("install-user.sh");
    if !installer.is_file() {
        return Err(UpdateError::State(
            "bundled lifecycle installer is missing".into(),
        ));
    }
    let output = Command::new(&installer)
        .arg("install")
        .arg(&verified.appimage)
        .env("LSB_INSTALL_VERSION", verified.metadata.version.to_string())
        .env("LSB_INSTALL_EXPECTED_SHA256", &verified.expected_sha256)
        .output()
        .map_err(|error| {
            UpdateError::State(format!("could not run lifecycle installer: {error}"))
        })?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(UpdateError::State(if stderr.is_empty() {
            format!("lifecycle installer exited with {}", output.status)
        } else {
            stderr
        }))
    }
}

fn launch_installed_and_monitor(record: &RollbackRecord) -> Result<(), UpdateError> {
    let mut child = launch_stable_install(Some(&record.token))?;
    match monitor_health(&mut child, record) {
        Ok(()) => {
            prune_old_rollbacks(&record.token)?;
            Ok(())
        }
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(error)
        }
    }
}

fn prune_old_rollbacks(keep: &str) -> Result<(), UpdateError> {
    let root = state::root()?.join("updates/backups");
    if !root.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&root).map_err(|error| UpdateError::State(error.to_string()))? {
        let entry = entry.map_err(|error| UpdateError::State(error.to_string()))?;
        if entry.file_name() == keep {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())
            .map_err(|error| UpdateError::State(error.to_string()))?;
        if metadata.file_type().is_dir() && metadata.uid() == nix::unistd::getuid().as_raw() {
            fs::remove_dir_all(entry.path())
                .map_err(|error| UpdateError::State(error.to_string()))?;
        }
    }
    Ok(())
}

fn launch_stable_install(token: Option<&str>) -> Result<Child, UpdateError> {
    let binary = stable_binary()?;
    if !binary.is_file() {
        return Err(UpdateError::State(
            "managed Linux Soundboard executable is missing".into(),
        ));
    }
    let mut command = Command::new(binary);
    command
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("OWD");
    if let Some(token) = token {
        command.env(HEALTH_ENV, token);
    }
    command.spawn().map_err(|error| {
        UpdateError::State(format!("could not relaunch Linux Soundboard: {error}"))
    })
}

fn health_confirmed(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_file() || metadata.uid() != nix::unistd::getuid().as_raw() {
        return false;
    }
    fs::read_to_string(path)
        .map(|value| value == crate::app_meta::APP_VERSION)
        .unwrap_or(false)
}

fn monitor_health(child: &mut Child, record: &RollbackRecord) -> Result<(), UpdateError> {
    let health = health_path(&record.token)?;
    let started = Instant::now();
    loop {
        if health_confirmed(&health) {
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| UpdateError::State(error.to_string()))?
        {
            return Err(UpdateError::State(format!(
                "updated Linux Soundboard exited before startup completed: {status}"
            )));
        }
        if started.elapsed() >= HEALTH_TIMEOUT {
            return Err(UpdateError::State(
                "updated Linux Soundboard did not confirm a healthy startup".into(),
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn restore_rollback_snapshot(record: &RollbackRecord) -> Result<(), UpdateError> {
    let root = rollback_dir(&record.token)?;
    let install_root = stable_install_root()?;
    if install_root.exists() {
        fs::remove_dir_all(&install_root).map_err(|error| UpdateError::State(error.to_string()))?;
    }
    if record.install_root_present {
        copy_tree(&root.join("install-root"), &install_root)?;
    }
    let config = crate::config::Config::config_path();
    let library = config.with_file_name("library.sqlite3");
    restore_optional_file(
        &root.join("config.json"),
        &config,
        record.config_present,
        0o600,
    )?;
    restore_optional_file(
        &root.join("library.sqlite3"),
        &library,
        record.library_present,
        0o600,
    )?;
    let units = config_home()?.join("systemd/user");
    restore_optional_file(
        &root.join("engine.service"),
        &units.join(crate::app_meta::ENGINE_SERVICE_NAME),
        record.service_present,
        0o644,
    )?;
    restore_optional_file(
        &root.join("engine.target"),
        &units.join(crate::app_meta::ENGINE_TARGET_NAME),
        record.target_present,
        0o644,
    )?;
    let _ = crate::audio::command_runner::run_command("systemctl", &["--user", "daemon-reload"]);
    let _ = crate::audio::command_runner::run_command(
        "systemctl",
        &["--user", "restart", crate::app_meta::ENGINE_TARGET_NAME],
    );
    Ok(())
}

fn restore_optional_file(
    backup: &Path,
    destination: &Path,
    should_exist: bool,
    mode: u32,
) -> Result<(), UpdateError> {
    if should_exist {
        copy_file_private(backup, destination, mode)
    } else {
        match fs::remove_file(destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(UpdateError::State(error.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_identity_can_be_read() {
        let pid = std::process::id();
        let ticks = process_start_ticks(pid).unwrap();
        assert!(process_matches(pid, ticks));
        assert!(!process_matches(pid, ticks.saturating_add(1)));
    }

    #[test]
    fn health_confirmation_requires_exact_regular_file() {
        use std::os::unix::fs::symlink;

        let root = std::env::temp_dir().join(format!("lsb-health-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let health = root.join("healthy");
        fs::write(&health, crate::app_meta::APP_VERSION).unwrap();
        assert!(health_confirmed(&health));
        fs::write(&health, "wrong-version").unwrap();
        assert!(!health_confirmed(&health));
        fs::remove_file(&health).unwrap();
        let target = root.join("target");
        fs::write(&target, crate::app_meta::APP_VERSION).unwrap();
        symlink(&target, &health).unwrap();
        assert!(!health_confirmed(&health));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sqlite_backup_produces_readable_snapshot() {
        let root = std::env::temp_dir().join(format!("lsb-db-backup-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let source = root.join("source.sqlite3");
        let destination = root.join("backup.sqlite3");
        let connection = rusqlite::Connection::open(&source).unwrap();
        connection
            .execute_batch("CREATE TABLE value(v INTEGER); INSERT INTO value VALUES (42);")
            .unwrap();
        drop(connection);
        backup_database(&source, &destination).unwrap();
        let backup = rusqlite::Connection::open(&destination).unwrap();
        let value: i64 = backup
            .query_row("SELECT v FROM value", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, 42);
        drop(backup);
        fs::remove_dir_all(root).unwrap();
    }
}
