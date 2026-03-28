use std::fs;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DistroFamily {
    Arch,
    Debian,
    Fedora,
    Other,
}

fn detect_distro_family_from_os_release(os_release: &str) -> DistroFamily {
    let mut ids = Vec::new();
    for line in os_release.lines() {
        if let Some(value) = line.strip_prefix("ID=") {
            ids.push(value.trim_matches('"').to_ascii_lowercase());
        } else if let Some(value) = line.strip_prefix("ID_LIKE=") {
            ids.extend(
                value.trim_matches('"')
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
    } else if ids
        .iter()
        .any(|id| matches!(id.as_str(), "fedora" | "rhel" | "centos" | "rocky" | "almalinux"))
    {
        DistroFamily::Fedora
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
             https://github.com/waycrate/swhkd/blob/main/INSTALL.md\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
        DistroFamily::Fedora => format!(
            "{intro}\n\
             Fedora does not currently ship swhkd in the official package set.\n\
             Install it from the upstream instructions:\n\
             https://github.com/waycrate/swhkd/blob/main/INSTALL.md\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
        DistroFamily::Other => format!(
            "{intro}\n\
             Install swhkd from the upstream instructions:\n\
             https://github.com/waycrate/swhkd/blob/main/INSTALL.md\n\
             X11 and XWayland sessions can use the native X11 backend without swhkd."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{detect_distro_family_from_os_release, DistroFamily};

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
}
