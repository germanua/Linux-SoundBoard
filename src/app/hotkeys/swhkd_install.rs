use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use log::{info, warn};

pub const SWHKD_UPSTREAM_INSTALL_URL: &str =
    "https://github.com/waycrate/swhkd/blob/main/INSTALL.md";
pub const INSTALLED_SWHKD_HELPER_PATH: &str =
    "/usr/libexec/linux-soundboard/install-swhkd-helper.sh";
pub const MANAGED_SWHKD_BINARY: &str = "/usr/local/libexec/linux-soundboard/swhkd";
pub const MANAGED_SWHKS_BINARY: &str = "/usr/local/libexec/linux-soundboard/swhks";
const SWHKD_RFKILL_MARKERS: [&[u8]; 2] = [b"/dev/rfkill", b"SW_RFKILL_ALL"];

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SwhkdInstallState {
    Idle,
    Checking,
    Installing,
    Verifying,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum SwhkdInstallErrorKind {
    UnsupportedDistro,
    MissingPkexec,
    MissingHelper,
    PrivilegeDenied,
    CommandFailed,
    VerificationFailed,
}

#[derive(Debug, Clone)]
pub struct SwhkdInstallError {
    pub kind: SwhkdInstallErrorKind,
    pub summary: String,
    pub details: String,
    pub state: SwhkdInstallState,
}

impl std::fmt::Display for SwhkdInstallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.details.is_empty() {
            write!(f, "{}", self.summary)
        } else {
            write!(f, "{}\n\n{}", self.summary, self.details)
        }
    }
}

#[derive(Debug, Clone)]
pub struct SwhkdInstallReport {
    pub summary: String,
    pub details: String,
    pub states: Vec<SwhkdInstallState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DistroFamily {
    Arch,
    Debian,
    Fedora,
    OpenSuse,
    Other,
}

fn detect_distro_family_from_os_release(os_release: &str) -> DistroFamily {
    let mut ids = Vec::new();
    for line in os_release.lines() {
        if let Some(value) = line.strip_prefix("ID=") {
            ids.push(value.trim_matches('"').to_ascii_lowercase());
        } else if let Some(value) = line.strip_prefix("ID_LIKE=") {
            ids.extend(
                value
                    .trim_matches('"')
                    .split_whitespace()
                    .map(|entry| entry.to_ascii_lowercase()),
            );
        }
    }

    if ids
        .iter()
        .any(|id| matches!(id.as_str(), "arch" | "manjaro" | "endeavouros"))
    {
        DistroFamily::Arch
    } else if ids.iter().any(|id| {
        matches!(
            id.as_str(),
            "debian" | "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin"
        )
    }) {
        DistroFamily::Debian
    } else if ids.iter().any(|id| {
        matches!(
            id.as_str(),
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux"
        )
    }) {
        DistroFamily::Fedora
    } else if ids
        .iter()
        .any(|id| matches!(id.as_str(), "opensuse" | "sles" | "suse"))
    {
        DistroFamily::OpenSuse
    } else {
        DistroFamily::Other
    }
}

fn detect_distro_family() -> DistroFamily {
    let Ok(os_release) = fs::read_to_string("/etc/os-release") else {
        return DistroFamily::Other;
    };

    detect_distro_family_from_os_release(&os_release)
}

pub fn should_offer_swhkd_install(raw_error: &str) -> bool {
    if !crate::app_meta::ALLOW_PRIVILEGED_HELPER {
        return false;
    }
    let normalized = raw_error.to_ascii_lowercase();
    normalized.contains("swhkd not found in path")
        || normalized.contains("swhks not found in path")
        || normalized.contains("no wayland hotkey backend available")
        || normalized.contains("swhkd requires setuid bit")
        || normalized.contains("swhkd exited immediately")
        || normalized.contains("unsafe rfkill support")
        || normalized.contains("could not verify installed swhkd")
        || normalized.contains("working setuid swhkd binary")
}

pub(super) fn binary_has_rfkill_support(binary: &[u8]) -> bool {
    SWHKD_RFKILL_MARKERS
        .iter()
        .any(|marker| binary.windows(marker.len()).any(|window| window == *marker))
}

pub(super) fn ensure_swhkd_binary_is_safe(path: &Path) -> Result<(), String> {
    let binary = fs::read(path).map_err(|error| {
        format!(
            "Could not verify installed swhkd binary at {}: {error}. Linux Soundboard will not start it. Use Install swhkd to rebuild and reinstall it safely.",
            path.display()
        )
    })?;
    if binary_has_rfkill_support(&binary) {
        return Err(
            "Installed swhkd has unsafe rfkill support enabled. Linux Soundboard will not start it because it can disable Wi-Fi or Bluetooth. Use Install swhkd to rebuild and reinstall it safely."
                .to_string(),
        );
    }
    Ok(())
}

pub fn manual_swhkd_install_commands() -> String {
    if !crate::app_meta::ALLOW_PRIVILEGED_HELPER {
        return "# This build does not install or replace the machine-global swhkd helper.
# Use the official Linux Soundboard installer to manage swhkd, or reuse an already-safe system installation."
            .to_string();
    }
    manual_install_commands_for(detect_distro_family())
}

fn manual_install_commands_for(_distro: DistroFamily) -> String {
    format!(
        "Repair the authenticated stable installation and its managed Wayland hotkey helper with:\n\
curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh | bash -s -- fix\n\n\
Upstream swhkd installation reference:\n{}",
        SWHKD_UPSTREAM_INSTALL_URL
    )
}

pub fn install_swhkd_native_detailed(
    enable_uinput: bool,
) -> Result<SwhkdInstallReport, SwhkdInstallError> {
    if !crate::app_meta::ALLOW_PRIVILEGED_HELPER {
        return Err(SwhkdInstallError {
            kind: SwhkdInstallErrorKind::MissingHelper,
            summary: "This build does not modify the machine-global swhkd installation."
                .to_string(),
            details: "Use the official Linux Soundboard installer to manage swhkd, or reuse an existing safe installation."
                .to_string(),
            state: SwhkdInstallState::Failed,
        });
    }
    let distro = detect_distro_family();
    let mut states = vec![SwhkdInstallState::Idle, SwhkdInstallState::Checking];

    info!(
        "Starting one-click swhkd install flow for distro family: {}",
        distro_display_name(distro)
    );

    if distro == DistroFamily::Other {
        states.push(SwhkdInstallState::Failed);
        warn!("One-click swhkd install unavailable on unsupported distro family");
        return Err(SwhkdInstallError {
            kind: SwhkdInstallErrorKind::UnsupportedDistro,
            summary: "One-click install is unavailable on this Linux distribution.".to_string(),
            details: format!(
                "Use manual installation steps from:\n{}",
                SWHKD_UPSTREAM_INSTALL_URL
            ),
            state: SwhkdInstallState::Failed,
        });
    }

    if which::which("pkexec").is_err() {
        states.push(SwhkdInstallState::Failed);
        warn!("Cannot run one-click swhkd installer because pkexec is unavailable");
        return Err(SwhkdInstallError {
            kind: SwhkdInstallErrorKind::MissingPkexec,
            summary: "Cannot start one-click install because pkexec is unavailable.".to_string(),
            details: format!(
                "Install polkit (policykit) and try again, or follow manual instructions:\n{}",
                SWHKD_UPSTREAM_INSTALL_URL
            ),
            state: SwhkdInstallState::Failed,
        });
    }

    let helper_path = resolve_install_helper_path().ok_or_else(|| {
        states.push(SwhkdInstallState::Failed);
        warn!("One-click swhkd installer helper path could not be resolved");
        SwhkdInstallError {
            kind: SwhkdInstallErrorKind::MissingHelper,
            summary: "Installer helper is missing from this build.".to_string(),
            details: format!(
                "Automatic installation needs a root-owned helper at {}; none is installed, so install swhkd manually:\n{}",
                INSTALLED_SWHKD_HELPER_PATH,
                manual_swhkd_install_commands()
            ),
            state: SwhkdInstallState::Failed,
        }
    })?;

    states.push(SwhkdInstallState::Installing);
    info!(
        "Running privileged installer helper at '{}'",
        helper_path.display()
    );
    let mut command = Command::new("pkexec");
    command
        .arg(&helper_path)
        .arg("--distro")
        .arg(distro_id(distro));
    if enable_uinput {
        command.arg("--enable-uinput");
    }
    let output = command.output().map_err(|e| SwhkdInstallError {
        kind: SwhkdInstallErrorKind::CommandFailed,
        summary: "Failed to launch privileged installer.".to_string(),
        details: format!("{}", e),
        state: SwhkdInstallState::Installing,
    })?;

    if !output.status.success() {
        states.push(SwhkdInstallState::Failed);
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let stderr_lower = stderr.to_ascii_lowercase();

        let kind = if output.status.code() == Some(126)
            || stderr_lower.contains("not authorized")
            || stderr_lower.contains("authentication")
            || stderr_lower.contains("permission denied")
        {
            SwhkdInstallErrorKind::PrivilegeDenied
        } else {
            SwhkdInstallErrorKind::CommandFailed
        };

        let summary = match kind {
            SwhkdInstallErrorKind::PrivilegeDenied => {
                "Installation was cancelled or denied by authentication.".to_string()
            }
            _ => "swhkd installation failed.".to_string(),
        };

        warn!(
            "swhkd one-click install failed: kind={:?} status={:?}",
            kind,
            output.status.code()
        );

        return Err(SwhkdInstallError {
            kind,
            summary,
            details: format!("stdout:\n{}\n\nstderr:\n{}", stdout, stderr),
            state: SwhkdInstallState::Failed,
        });
    }

    states.push(SwhkdInstallState::Verifying);
    info!("Verifying swhkd installation and permissions");
    if !has_healthy_swhkd_install() {
        states.push(SwhkdInstallState::Failed);
        warn!("swhkd verification failed after installer completed");
        return Err(SwhkdInstallError {
            kind: SwhkdInstallErrorKind::VerificationFailed,
            summary: "Installer finished but verification failed.".to_string(),
            details: "swhkd still appears unavailable or unconfigured after installation."
                .to_string(),
            state: SwhkdInstallState::Failed,
        });
    }

    states.push(SwhkdInstallState::Completed);
    info!("swhkd installation completed successfully");

    Ok(SwhkdInstallReport {
        summary: "Wayland hotkey support installed successfully.".to_string(),
        details: format!(
            "Installed and configured swhkd for {} using helper: {}.",
            distro_display_name(distro),
            helper_path.display()
        ),
        states,
    })
}

fn file_is_root_owned_regular_with_owner(path: &Path, owner_uid: u32) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != owner_uid || (metadata.mode() & 0o022) != 0 {
            return false;
        }
        return (metadata.mode() & 0o111) != 0;
    }

    #[allow(unreachable_code)]
    true
}

fn parent_is_root_owned_nonwritable_with_owner(path: &Path, owner_uid: u32) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(metadata) = fs::symlink_metadata(parent) else {
        return false;
    };
    if !metadata.file_type().is_dir() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != owner_uid || (metadata.mode() & 0o022) != 0 {
            return false;
        }
    }

    true
}

pub(super) fn helper_is_privilege_safe_with_owner(path: &Path, owner_uid: u32) -> bool {
    path.is_absolute()
        && file_is_root_owned_regular_with_owner(path, owner_uid)
        && parent_is_root_owned_nonwritable_with_owner(path, owner_uid)
}

pub(super) fn helper_is_privilege_safe(path: &Path) -> bool {
    helper_is_privilege_safe_with_owner(path, 0)
}

pub(super) fn first_trusted_helper(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .find(|candidate| helper_is_privilege_safe(candidate))
        .cloned()
}

fn install_helper_candidates() -> Vec<PathBuf> {
    vec![PathBuf::from(INSTALLED_SWHKD_HELPER_PATH)]
}

fn resolve_install_helper_path() -> Option<PathBuf> {
    first_trusted_helper(&install_helper_candidates())
}

fn binary_is_safe_to_launch_with_owner(path: &Path, owner_uid: u32) -> bool {
    file_is_root_owned_regular_with_owner(path, owner_uid)
        && parent_is_root_owned_nonwritable_with_owner(path, owner_uid)
}

fn binary_is_safe_to_launch(path: &Path) -> bool {
    binary_is_safe_to_launch_with_owner(path, 0)
}

pub(super) fn resolve_swhkd_binary() -> Option<PathBuf> {
    let managed = PathBuf::from(MANAGED_SWHKD_BINARY);
    if binary_is_safe_to_launch(&managed) {
        return Some(managed);
    }
    let found = which::which("swhkd").ok()?;
    binary_is_safe_to_launch(&found).then_some(found)
}

pub(super) fn resolve_swhks_binary() -> Option<PathBuf> {
    let managed = PathBuf::from(MANAGED_SWHKS_BINARY);
    if binary_is_safe_to_launch(&managed) {
        return Some(managed);
    }
    let found = which::which("swhks").ok()?;
    binary_is_safe_to_launch(&found).then_some(found)
}

fn distro_id(distro: DistroFamily) -> &'static str {
    match distro {
        DistroFamily::Arch => "arch",
        DistroFamily::Debian => "debian",
        DistroFamily::Fedora => "fedora",
        DistroFamily::OpenSuse => "opensuse",
        DistroFamily::Other => "other",
    }
}

pub fn uinput_unavailable() -> bool {
    if std::path::Path::new("/sys/module/uinput").exists() {
        return false;
    }
    match fs::OpenOptions::new().write(true).open("/dev/uinput") {
        Ok(_) => false,
        Err(error) => error.raw_os_error() == Some(19),
    }
}

fn has_healthy_swhkd_install() -> bool {
    let Some(swhkd_path) = resolve_swhkd_binary() else {
        return false;
    };
    if ensure_swhkd_binary_is_safe(&swhkd_path).is_err() {
        return false;
    }
    if resolve_swhks_binary().is_none() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(metadata) = fs::metadata(swhkd_path) else {
            return false;
        };
        let mode = metadata.permissions().mode();
        return (mode & 0o4000) != 0;
    }

    #[allow(unreachable_code)]
    false
}

fn distro_display_name(distro: DistroFamily) -> &'static str {
    match distro {
        DistroFamily::Arch => "Arch-based Linux",
        DistroFamily::Debian => "Debian/Ubuntu Linux",
        DistroFamily::Fedora => "Fedora/RHEL Linux",
        DistroFamily::OpenSuse => "openSUSE/SUSE Linux",
        DistroFamily::Other => "an unsupported Linux distribution",
    }
}

pub(super) fn missing_swhkd_message(binary_name: &str) -> String {
    let intro = format!("{binary_name} not found in PATH.");
    if !crate::app_meta::ALLOW_PRIVILEGED_HELPER {
        return format!(
            "{intro}
This build does not install or replace machine-global swhkd. Use the official Linux Soundboard installer to manage it."
        );
    }

    match detect_distro_family() {
        DistroFamily::Arch
        | DistroFamily::Debian
        | DistroFamily::Fedora
        | DistroFamily::OpenSuse => format!(
            "{intro}\n\
             Use Linux Soundboard's Install action; it only runs the pinned root-owned helper.\n\
             If the helper is unavailable, use the manual commands shown by the installer.\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
        DistroFamily::Other => format!(
            "{intro}\n\
             This distribution is not supported by the managed installer.\n\
             See the upstream installation notes:\n\
             {SWHKD_UPSTREAM_INSTALL_URL}\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        binary_has_rfkill_support, binary_is_safe_to_launch, detect_distro_family_from_os_release,
        distro_id, ensure_swhkd_binary_is_safe, first_trusted_helper, helper_is_privilege_safe,
        helper_is_privilege_safe_with_owner, manual_install_commands_for,
        should_offer_swhkd_install, DistroFamily, SwhkdInstallState, INSTALLED_SWHKD_HELPER_PATH,
        MANAGED_SWHKD_BINARY, MANAGED_SWHKS_BINARY, SWHKD_UPSTREAM_INSTALL_URL,
    };
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lsb-swhkd-trust-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_executable(path: &std::path::Path, mode: u32) {
        fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn current_uid() -> u32 {
        nix::unistd::getuid().as_raw()
    }

    #[test]
    fn detects_arch_family() {
        let os_release = "ID=manjaro\nID_LIKE=\"arch linux\"";
        assert_eq!(
            detect_distro_family_from_os_release(os_release),
            DistroFamily::Arch
        );
    }

    #[test]
    fn detects_debian_family() {
        let os_release = "ID=ubuntu\nID_LIKE=debian";
        assert_eq!(
            detect_distro_family_from_os_release(os_release),
            DistroFamily::Debian
        );
    }

    #[test]
    fn detects_fedora_family() {
        let os_release = "ID=fedora\nID_LIKE=\"fedora rhel\"";
        assert_eq!(
            detect_distro_family_from_os_release(os_release),
            DistroFamily::Fedora
        );
    }

    #[test]
    fn detects_opensuse_family() {
        let os_release = "ID=opensuse-tumbleweed\nID_LIKE=\"suse opensuse\"";
        assert_eq!(
            detect_distro_family_from_os_release(os_release),
            DistroFamily::OpenSuse
        );
    }

    #[test]
    fn detects_installable_missing_swhkd_errors() {
        assert_eq!(
            should_offer_swhkd_install("swhkd not found in PATH."),
            crate::app_meta::ALLOW_PRIVILEGED_HELPER
        );
        assert_eq!(
            should_offer_swhkd_install(
                "no Wayland hotkey backend available (swhkd: swhkd not found in PATH.)"
            ),
            crate::app_meta::ALLOW_PRIVILEGED_HELPER
        );
        assert_eq!(
            should_offer_swhkd_install("swhkd requires setuid bit for proper operation"),
            crate::app_meta::ALLOW_PRIVILEGED_HELPER
        );
        assert_eq!(
            should_offer_swhkd_install("swhkd exited immediately after startup"),
            crate::app_meta::ALLOW_PRIVILEGED_HELPER
        );
        assert_eq!(
            should_offer_swhkd_install("Installed swhkd has unsafe rfkill support enabled"),
            crate::app_meta::ALLOW_PRIVILEGED_HELPER
        );
    }

    #[test]
    fn ignores_non_installable_errors() {
        assert!(!should_offer_swhkd_install(
            "UNSUPPORTED_KEY_FOR_BACKEND:swhkd:Ctrl+NumpadDivide cannot be represented by swhkd."
        ));
    }

    #[test]
    fn maps_distro_ids() {
        assert_eq!(distro_id(DistroFamily::Arch), "arch");
        assert_eq!(distro_id(DistroFamily::Debian), "debian");
        assert_eq!(distro_id(DistroFamily::Fedora), "fedora");
        assert_eq!(distro_id(DistroFamily::OpenSuse), "opensuse");
        assert_eq!(distro_id(DistroFamily::Other), "other");
    }

    #[test]
    fn manual_install_commands_use_the_authenticated_stable_repair_path() {
        let commands = manual_install_commands_for(DistroFamily::Debian);
        assert!(commands.contains("bootstrap-install.sh | bash -s -- fix"));
        assert!(commands.contains(SWHKD_UPSTREAM_INSTALL_URL));
        assert!(!commands.contains("build-swhkd-locked.sh"));
        assert!(!commands.contains("chmod u+s"));
    }

    #[test]
    fn only_the_fixed_system_helper_path_is_a_candidate() {
        assert_eq!(
            super::install_helper_candidates(),
            vec![std::path::PathBuf::from(INSTALLED_SWHKD_HELPER_PATH)]
        );
    }

    #[test]
    fn symlinked_daemon_binary_is_not_launched() {
        let dir = temp_dir("daemon-link");
        let real = dir.join("swhkd");
        write_executable(&real, 0o4755);
        let link = dir.join("swhkd-link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(!binary_is_safe_to_launch(&link));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn detects_rfkill_markers_in_swhkd_binary() {
        assert!(binary_has_rfkill_support(b"prefix/dev/rfkill suffix"));
        assert!(binary_has_rfkill_support(b"prefix SW_RFKILL_ALL suffix"));
        assert!(!binary_has_rfkill_support(b"safe swhkd binary"));
    }

    #[test]
    fn refuses_unsafe_or_unreadable_swhkd_binaries() {
        let directory =
            std::env::temp_dir().join(format!("lsb-swhkd-rfkill-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();

        let unsafe_binary = directory.join("unsafe-swhkd");
        fs::write(&unsafe_binary, b"binary with /dev/rfkill support").unwrap();
        let error = ensure_swhkd_binary_is_safe(&unsafe_binary).unwrap_err();
        assert!(error.contains("unsafe rfkill support"));

        let safe_binary = directory.join("safe-swhkd");
        fs::write(&safe_binary, b"safe swhkd binary").unwrap();
        assert!(ensure_swhkd_binary_is_safe(&safe_binary).is_ok());

        let error = ensure_swhkd_binary_is_safe(&directory.join("missing-swhkd")).unwrap_err();
        assert!(error.contains("Could not verify installed swhkd"));

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn install_state_order_is_expected() {
        let states = [
            SwhkdInstallState::Idle,
            SwhkdInstallState::Checking,
            SwhkdInstallState::Installing,
            SwhkdInstallState::Verifying,
            SwhkdInstallState::Completed,
            SwhkdInstallState::Failed,
        ];
        assert_eq!(states.len(), 6);
    }

    #[test]
    fn user_owned_helper_is_never_a_privileged_target() {
        let dir = temp_dir("user-owned");
        let helper = dir.join("install-swhkd-helper.sh");
        write_executable(&helper, 0o755);

        assert!(helper_is_privilege_safe_with_owner(&helper, current_uid()));
        assert!(!helper_is_privilege_safe(&helper));
        assert_eq!(first_trusted_helper(&[helper]), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn symlinked_helper_is_rejected() {
        let dir = temp_dir("symlink");
        let real = dir.join("real-helper.sh");
        write_executable(&real, 0o755);
        let link = dir.join("install-swhkd-helper.sh");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(!helper_is_privilege_safe_with_owner(&link, current_uid()));
        assert!(!helper_is_privilege_safe(&link));
        assert_eq!(first_trusted_helper(&[link]), None);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn group_or_world_writable_helper_is_rejected() {
        let dir = temp_dir("writable-file");
        let helper = dir.join("install-swhkd-helper.sh");
        write_executable(&helper, 0o777);
        assert!(!helper_is_privilege_safe_with_owner(&helper, current_uid()));
        fs::remove_dir_all(dir).unwrap();

        let dir = temp_dir("writable-dir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        let helper = dir.join("install-swhkd-helper.sh");
        write_executable(&helper, 0o755);
        assert!(!helper_is_privilege_safe_with_owner(&helper, current_uid()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn home_installed_helper_is_never_a_privileged_target() {
        let home = temp_dir("home");
        let tree = home.join(".local").join("opt").join("linux-soundboard");
        fs::create_dir_all(&tree).unwrap();
        let helper = tree.join("install-swhkd-helper.sh");
        write_executable(&helper, 0o755);

        assert_eq!(first_trusted_helper(&[helper]), None);
        fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn environment_override_cannot_select_a_pkexec_helper() {
        let dir = temp_dir("env-override");
        let helper = dir.join("attacker-helper.sh");
        write_executable(&helper, 0o755);

        assert_eq!(first_trusted_helper(&[helper]), None);
        let removed_env_var = concat!("LSB_SWHKD", "_INSTALL_HELPER");
        assert!(!include_str!("swhkd_install.rs").contains(removed_env_var));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn trusted_helper_is_selected_over_an_untrusted_candidate() {
        let dir = temp_dir("candidates");
        let untrusted = dir.join("a-untrusted.sh");
        write_executable(&untrusted, 0o777);
        let trusted = dir.join("b-trusted.sh");
        write_executable(&trusted, 0o755);

        let uid = current_uid();
        let picked = [untrusted.as_path(), trusted.as_path()]
            .into_iter()
            .find(|candidate| helper_is_privilege_safe_with_owner(candidate, uid));
        assert_eq!(picked.map(std::path::Path::to_path_buf), Some(trusted));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn managed_daemon_paths_are_fixed_absolute_locations() {
        for path in [MANAGED_SWHKD_BINARY, MANAGED_SWHKS_BINARY] {
            let path = std::path::Path::new(path);
            assert!(path.is_absolute());
            assert_eq!(
                path.parent().unwrap(),
                std::path::Path::new("/usr/local/libexec/linux-soundboard")
            );
        }
    }

    #[test]
    fn daemon_in_writable_directory_is_not_launched() {
        let dir = temp_dir("daemon-writable-dir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();
        let binary = dir.join("swhkd");
        write_executable(&binary, 0o4755);

        assert!(!super::binary_is_safe_to_launch_with_owner(
            &binary,
            current_uid()
        ));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn user_owned_daemon_binary_is_not_launched() {
        let dir = temp_dir("daemon");
        let binary = dir.join("swhkd");
        write_executable(&binary, 0o4755);

        assert!(!binary_is_safe_to_launch(&binary));
        fs::remove_dir_all(dir).unwrap();
    }
}
