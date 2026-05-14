pub fn init_player(config: &crate::config::Config) -> crate::audio::player::AudioPlayer {
    use crate::audio::player::AudioBackendKind;

    cleanup_legacy_audio_routing_artifacts();
    ensure_wireplumber_stream_moving_enabled();
    let backend = if crate::pipewire::detection::check_pipewire().available {
        AudioBackendKind::PipeWire
    } else {
        AudioBackendKind::PulseAudio
    };
    // No pre-start wait: the player's PipeWire event loop handles capture-source
    // readiness reactively. `recreate_capture_stream` is called immediately on
    // startup; if the desired source (e.g. EasyEffects) isn't registered yet,
    // the registry listener and periodic watchdog re-try automatically when it
    // appears — including hot-plug and the no-mic-at-boot case.
    crate::audio::player::AudioPlayer::new_with_config_and_audio_backend(config, backend)
}

fn ensure_wireplumber_stream_moving_enabled() {
    let Ok(output) = std::process::Command::new("wpctl")
        .args(["settings", "linking.allow-moving-streams"])
        .output()
    else {
        return;
    };

    if !output.status.success() {
        // Older WirePlumber that doesn't have `wpctl settings` — assume default (enabled).
        return;
    }

    let value = String::from_utf8_lossy(&output.stdout);
    let value = value.trim();

    // Empty output = setting at default (enabled). "true"/"1" = explicitly enabled. Fine.
    if !matches!(value, "false" | "0") {
        return;
    }

    log::warn!(
        "WirePlumber 'linking.allow-moving-streams' is disabled — Linux Soundboard cannot \
         auto-route apps to the virtual microphone. Attempting to enable it..."
    );

    match std::process::Command::new("wpctl")
        .args(["settings", "--save", "linking.allow-moving-streams", "true"])
        .output()
    {
        Ok(out) if out.status.success() => {
            log::info!(
                "Enabled WirePlumber 'linking.allow-moving-streams'; \
                 apps will now be routed to Linux Soundboard Mic automatically"
            );
        }
        Ok(out) => {
            let detail = String::from_utf8_lossy(&out.stderr);
            log::warn!(
                "Failed to enable 'linking.allow-moving-streams': {}. \
                 Fix manually with: wpctl settings --save linking.allow-moving-streams true",
                detail.trim()
            );
        }
        Err(err) => {
            log::warn!(
                "Failed to run wpctl: {err}. Fix manually with: \
                 wpctl settings --save linking.allow-moving-streams true"
            );
        }
    }
}

const LEGACY_MANAGED_MARKER: &str = "managed-by: linux-soundboard";
const LEGACY_MANAGED_END_MARKER: &str = "end-managed-by: linux-soundboard";
const LEGACY_PIPEWIRE_CONF_FILE: &str = "99-linuxsoundboard.conf";
const LEGACY_AUTOROUTE_FILE: &str = "99-linuxsoundboard-autoroute.lua";
const LEGACY_AUTOROUTE_TARGET: &str = r#"["target.object"] = "output.LinuxSoundboard_Mic""#;
const LEGACY_AUTOROUTE_LOG: &str =
    "Linux SoundBoard: Auto-routing to output.LinuxSoundboard_Mic enabled";

fn cleanup_legacy_audio_routing_artifacts() {
    cleanup_legacy_pipewire_config();
    cleanup_legacy_pulse_block();
    cleanup_legacy_wireplumber_autoroute();
    cleanup_legacy_wireplumber_force_routing();
    warn_if_system_pipewire_config_exists();
}

fn config_home() -> Option<std::path::PathBuf> {
    let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
    else {
        return None;
    };
    Some(config_home)
}

fn data_home() -> Option<std::path::PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
}

fn cleanup_legacy_pipewire_config() {
    let Some(config_home) = config_home() else {
        return;
    };
    let path = config_home
        .join("pipewire")
        .join("pipewire.conf.d")
        .join(LEGACY_PIPEWIRE_CONF_FILE);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    if !is_legacy_linuxsoundboard_managed_config(&content) {
        return;
    }

    disable_legacy_file(
        &path,
        "obsolete Linux Soundboard PipeWire virtual mic config",
    );
}

fn cleanup_legacy_pulse_block() {
    let Some(config_home) = config_home() else {
        return;
    };
    let path = config_home.join("pulse").join("default.pa");
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    if !is_legacy_linuxsoundboard_managed_config(&content) {
        return;
    }

    let stripped = strip_managed_block(&content);
    match std::fs::write(&path, stripped) {
        Ok(()) => log::warn!(
            "Removed obsolete Linux Soundboard PulseAudio virtual mic block from {}",
            path.display()
        ),
        Err(err) => log::warn!(
            "Failed to remove obsolete Linux Soundboard PulseAudio block from {}: {err}",
            path.display()
        ),
    }
}

fn cleanup_legacy_wireplumber_autoroute() {
    let Some(config_home) = config_home() else {
        return;
    };
    let path = config_home
        .join("wireplumber")
        .join("main.lua.d")
        .join(LEGACY_AUTOROUTE_FILE);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return;
    };
    if !is_legacy_linuxsoundboard_autoroute(&content) {
        return;
    }

    disable_legacy_file(
        &path,
        "obsolete Linux Soundboard WirePlumber autoroute rule",
    );
}

fn cleanup_legacy_wireplumber_force_routing() {
    let mut paths = Vec::new();
    if let Some(config_home) = config_home() {
        paths.push(
            config_home
                .join("wireplumber")
                .join("wireplumber.conf.d")
                .join("50-linuxsoundboard-capture.conf"),
        );
        paths.push(
            config_home
                .join("wireplumber")
                .join("wireplumber.conf.d")
                .join("51-linuxsoundboard-force-capture.conf"),
        );
    }
    if let Some(data_home) = data_home() {
        paths.push(
            data_home
                .join("wireplumber")
                .join("scripts")
                .join("50-linuxsoundboard-force-capture.lua"),
        );
    }

    for path in paths {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if is_legacy_linuxsoundboard_force_route(&content) {
            disable_legacy_file(
                &path,
                "obsolete Linux Soundboard WirePlumber force-routing file",
            );
        }
    }
}

fn warn_if_system_pipewire_config_exists() {
    let path = std::path::Path::new("/usr/share/pipewire/pipewire.conf.d/99-linuxsoundboard.conf");
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    if is_legacy_linuxsoundboard_managed_config(&content) {
        log::warn!(
            "Obsolete system PipeWire config still exists at {}; remove it with the package manager or sudo rm so the runtime mic cannot be created twice",
            path.display()
        );
    }
}

fn is_legacy_linuxsoundboard_autoroute(content: &str) -> bool {
    content.contains(LEGACY_AUTOROUTE_TARGET)
        && content.contains(LEGACY_AUTOROUTE_LOG)
        && content
            .contains("Auto-routes all audio input streams to Linux SoundBoard virtual microphone")
}

fn is_legacy_linuxsoundboard_managed_config(content: &str) -> bool {
    content.contains(LEGACY_MANAGED_MARKER)
        && content.contains(crate::app_meta::VIRTUAL_SOURCE_NAME)
}

fn is_legacy_linuxsoundboard_force_route(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    lower.contains("linuxsoundboard")
        && (lower.contains("target.object")
            || lower.contains("linuxsoundboard.virtual_mic")
            || lower.contains("linuxsoundboard_mic"))
}

fn strip_managed_block(content: &str) -> String {
    let mut stripped = String::new();
    let mut skipping = false;
    for line in content.lines() {
        if line.contains(LEGACY_MANAGED_MARKER) {
            skipping = true;
            continue;
        }
        if skipping && line.contains(LEGACY_MANAGED_END_MARKER) {
            skipping = false;
            continue;
        }
        if !skipping {
            stripped.push_str(line);
            stripped.push('\n');
        }
    }
    stripped
}

fn disable_legacy_file(path: &std::path::Path, label: &str) {
    let disabled_path = next_disabled_path(path);
    match std::fs::rename(path, &disabled_path) {
        Ok(()) => log::warn!("Disabled {label} at {}", disabled_path.display()),
        Err(err) => log::warn!("Failed to disable {label} {}: {err}", path.display()),
    }
}

fn next_disabled_path(path: &std::path::Path) -> std::path::PathBuf {
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return path.with_extension("disabled");
    };
    let base = path.with_file_name(format!("{file_name}.disabled"));
    if !base.exists() {
        return base;
    }

    for index in 1..100 {
        let candidate = path.with_file_name(format!("{file_name}.disabled.{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    path.with_file_name(format!("{file_name}.disabled.{}", std::process::id()))
}

#[derive(Debug, Clone)]
pub struct VolumeConfig {
    pub local_volume: f32,
    pub mic_volume: f32,
    pub local_muted: bool,
}

impl From<&crate::config::Config> for VolumeConfig {
    fn from(config: &crate::config::Config) -> Self {
        let volume = config.settings.volume_domain();
        Self {
            local_volume: volume.local_volume as f32 / 100.0,
            mic_volume: volume.mic_volume as f32 / 100.0,
            local_muted: volume.local_mute,
        }
    }
}
