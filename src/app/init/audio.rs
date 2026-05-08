pub fn init_player(config: &crate::config::Config) -> crate::audio::player::AudioPlayer {
    use crate::audio::player::AudioBackendKind;
    use crate::pipewire::persistent_mic::AudioServer;

    cleanup_legacy_wireplumber_autoroute();
    let outcome = crate::pipewire::persistent_mic::ensure_persistent_virtual_mic();
    let backend = match outcome.audio_server().unwrap_or(AudioServer::PipeWire) {
        AudioServer::PulseAudio => AudioBackendKind::PulseAudio,
        AudioServer::PipeWire | AudioServer::Unsupported => AudioBackendKind::PipeWire,
    };
    // No pre-start wait: the player's PipeWire event loop handles capture-source
    // readiness reactively. `recreate_capture_stream` is called immediately on
    // startup; if the desired source (e.g. EasyEffects) isn't registered yet,
    // the registry listener and periodic watchdog re-try automatically when it
    // appears — including hot-plug and the no-mic-at-boot case.
    crate::audio::player::AudioPlayer::new_with_config_and_audio_backend(
        config,
        backend,
        outcome.node_available(),
    )
}

const LEGACY_AUTOROUTE_FILE: &str = "99-linuxsoundboard-autoroute.lua";
const LEGACY_AUTOROUTE_TARGET: &str = r#"["target.object"] = "output.LinuxSoundboard_Mic""#;
const LEGACY_AUTOROUTE_LOG: &str =
    "Linux SoundBoard: Auto-routing to output.LinuxSoundboard_Mic enabled";

fn cleanup_legacy_wireplumber_autoroute() {
    let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
    else {
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

    let disabled_path = next_disabled_autoroute_path(&path);
    match std::fs::rename(&path, &disabled_path) {
        Ok(()) => log::warn!(
            "Disabled obsolete Linux Soundboard WirePlumber autoroute rule at {}",
            disabled_path.display()
        ),
        Err(err) => log::warn!(
            "Failed to disable obsolete Linux Soundboard WirePlumber autoroute rule {}: {err}",
            path.display()
        ),
    }
}

fn is_legacy_linuxsoundboard_autoroute(content: &str) -> bool {
    content.contains(LEGACY_AUTOROUTE_TARGET)
        && content.contains(LEGACY_AUTOROUTE_LOG)
        && content
            .contains("Auto-routes all audio input streams to Linux SoundBoard virtual microphone")
}

fn next_disabled_autoroute_path(path: &std::path::Path) -> std::path::PathBuf {
    let base = path.with_file_name(format!("{LEGACY_AUTOROUTE_FILE}.disabled"));
    if !base.exists() {
        return base;
    }

    for index in 1..100 {
        let candidate = path.with_file_name(format!("{LEGACY_AUTOROUTE_FILE}.disabled.{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    path.with_file_name(format!(
        "{LEGACY_AUTOROUTE_FILE}.disabled.{}",
        std::process::id()
    ))
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
