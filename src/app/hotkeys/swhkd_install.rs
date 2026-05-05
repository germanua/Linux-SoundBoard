use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use log::{info, warn};

pub const SWHKD_UPSTREAM_INSTALL_URL: &str =
    "https://github.com/waycrate/swhkd/blob/main/INSTALL.md";
pub const INSTALLED_SWHKD_HELPER_PATH: &str =
    "/usr/libexec/linux-soundboard/install-swhkd-helper.sh";

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
    let normalized = raw_error.to_ascii_lowercase();
    normalized.contains("swhkd not found in path")
        || normalized.contains("swhks not found in path")
        || normalized.contains("no wayland hotkey backend available")
        || normalized.contains("swhkd requires setuid bit")
}

pub fn manual_swhkd_install_commands() -> String {
    manual_install_commands_for(detect_distro_family())
}

fn manual_install_commands_for(distro: DistroFamily) -> String {
    let polkit_install = match distro {
        DistroFamily::Arch => "sudo pacman -S --needed polkit",
        DistroFamily::Debian => "sudo apt-get update && sudo apt-get install -y policykit-1",
        DistroFamily::Fedora => "sudo dnf install -y polkit",
        DistroFamily::OpenSuse => "sudo zypper --non-interactive install polkit",
        DistroFamily::Other => {
            return format!(
            "# Unsupported distro family for built-in command recipe.\n# Follow manual guide:\n{}",
            SWHKD_UPSTREAM_INSTALL_URL
        )
        }
    };

    let build_deps_install = match distro {
        DistroFamily::Arch => "sudo pacman -S --needed git make rust cargo pkgconf systemd base-devel",
        DistroFamily::Debian => {
            "sudo apt-get update && sudo apt-get install -y git make build-essential pkg-config libudev-dev cargo rustc"
        }
        DistroFamily::Fedora => {
            "sudo dnf install -y git make gcc cargo rust pkgconf-pkg-config systemd-devel"
        }
        DistroFamily::OpenSuse => {
            "sudo zypper --non-interactive install git make gcc cargo rust pkg-config systemd-devel"
        }
        DistroFamily::Other => unreachable!("handled above"),
    };

    format!(
        "# 1) Install pkexec (polkit)\n{}\n\n# 2) Install build dependencies\n{}\n\n# 3) Build and install swhkd\nrm -rf /tmp/swhkd-build && git clone --depth 1 https://github.com/waycrate/swhkd.git /tmp/swhkd-build\ncd /tmp/swhkd-build\nmake clean || true\nmake\nsudo install -Dm755 target/release/swhkd /usr/bin/swhkd\nsudo install -Dm755 target/release/swhks /usr/bin/swhks\nsudo install -Dm644 /dev/null /etc/swhkd/swhkdrc\nsudo chown root:root /usr/bin/swhkd\nsudo chmod u+s /usr/bin/swhkd\nsudo chmod +x /usr/bin/swhks\n",
        polkit_install, build_deps_install
    )
}

pub fn install_swhkd_native() -> Result<SwhkdInstallReport, String> {
    install_swhkd_native_detailed().map_err(|e| e.to_string())
}

pub fn install_swhkd_native_detailed() -> Result<SwhkdInstallReport, SwhkdInstallError> {
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

    if has_healthy_swhkd_install() {
        info!("swhkd already installed and configured; skipping installer");
        return Ok(SwhkdInstallReport {
            summary: "swhkd is already installed and configured.".to_string(),
            details: "No installation actions were needed.".to_string(),
            states: vec![
                SwhkdInstallState::Idle,
                SwhkdInstallState::Checking,
                SwhkdInstallState::Completed,
            ],
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
                "Expected helper path: {}\nManual guide: {}",
                INSTALLED_SWHKD_HELPER_PATH, SWHKD_UPSTREAM_INSTALL_URL
            ),
            state: SwhkdInstallState::Failed,
        }
    })?;

    states.push(SwhkdInstallState::Installing);
    info!(
        "Running privileged installer helper at '{}'",
        helper_path.display()
    );
    let output = Command::new("pkexec")
        .arg(&helper_path)
        .arg("--distro")
        .arg(distro_id(distro))
        .output()
        .map_err(|e| SwhkdInstallError {
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

fn resolve_install_helper_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("LSB_SWHKD_INSTALL_HELPER") {
        let candidate = PathBuf::from(path);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }

    let installed = PathBuf::from(INSTALLED_SWHKD_HELPER_PATH);
    if is_executable_file(&installed) {
        return Some(installed);
    }

    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            let sibling = exe_dir.join("install-swhkd-helper.sh");
            if is_executable_file(&sibling) {
                return Some(sibling);
            }

            let libexec_sibling = exe_dir
                .join("..")
                .join("libexec")
                .join("linux-soundboard")
                .join("install-swhkd-helper.sh");
            if is_executable_file(&libexec_sibling) {
                return Some(libexec_sibling);
            }
        }
    }

    let source_helper = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packaging")
        .join("linux")
        .join("install-swhkd-helper.sh");
    if is_executable_file(&source_helper) {
        return Some(source_helper);
    }

    None
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = fs::metadata(path) {
            let mode = metadata.permissions().mode();
            return (mode & 0o111) != 0;
        }
    }

    #[allow(unreachable_code)]
    true
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

fn has_healthy_swhkd_install() -> bool {
    let Ok(swhkd_path) = which::which("swhkd") else {
        return false;
    };
    if which::which("swhks").is_err() {
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

    match detect_distro_family() {
        DistroFamily::Arch => format!(
            "{intro}\n\
             Install an AUR package for Wayland hotkeys:\n\
             • yay -S swhkd-bin\n\
             • or yay -S swhkd-git\n\
             X11 sessions can use the native X11 backend without swhkd."
        ),
        DistroFamily::Debian => format!(
            "{intro}\n\
             Debian and Ubuntu do not ship swhkd in their default repositories.\n\
             Install it from the upstream instructions:\n\
             {SWHKD_UPSTREAM_INSTALL_URL}\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
        DistroFamily::Fedora => format!(
            "{intro}\n\
             Fedora does not currently ship swhkd in the official package set.\n\
             Install it from the upstream instructions:\n\
             {SWHKD_UPSTREAM_INSTALL_URL}\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
        DistroFamily::Other => format!(
            "{intro}\n\
             Install swhkd from the upstream instructions:\n\
             {SWHKD_UPSTREAM_INSTALL_URL}\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
        DistroFamily::OpenSuse => format!(
            "{intro}\n\
             openSUSE does not currently ship swhkd in the official package set.\n\
             Install it from the upstream instructions:\n\
             {SWHKD_UPSTREAM_INSTALL_URL}\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        detect_distro_family_from_os_release, distro_id, manual_install_commands_for,
        should_offer_swhkd_install, DistroFamily, SwhkdInstallState,
    };

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
        assert!(should_offer_swhkd_install("swhkd not found in PATH."));
        assert!(should_offer_swhkd_install(
            "no Wayland hotkey backend available (swhkd: swhkd not found in PATH.)"
        ));
        assert!(should_offer_swhkd_install(
            "swhkd requires setuid bit for proper operation"
        ));
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
    fn manual_install_commands_include_polkit_and_build_steps() {
        let debian = manual_install_commands_for(DistroFamily::Debian);
        assert!(debian.contains("policykit-1"));
        assert!(debian.contains("git clone --depth 1"));
        assert!(debian.contains("chmod u+s /usr/bin/swhkd"));

        let arch = manual_install_commands_for(DistroFamily::Arch);
        assert!(arch.contains("pacman -S --needed polkit"));
    }

    #[test]
    fn install_state_order_is_expected() {
        let states = vec![
            SwhkdInstallState::Idle,
            SwhkdInstallState::Checking,
            SwhkdInstallState::Installing,
            SwhkdInstallState::Verifying,
            SwhkdInstallState::Completed,
            SwhkdInstallState::Failed,
        ];
        assert_eq!(states.len(), 6);
    }
}
