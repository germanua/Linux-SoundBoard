use std::env;

struct Profile {
    id: &'static str,
    app_id: &'static str,
    icon: &'static str,
    binary: &'static str,
    title: &'static str,
    config_dir: &'static str,
    state_dir: &'static str,
    update_repo: &'static str,
    update_channel: &'static str,
    engine_service: &'static str,
    engine_target: &'static str,
    local_playback: &'static str,
    mic_capture: &'static str,
    virtual_source: &'static str,
    virtual_feeder: &'static str,
    output_description: &'static str,
    mic_description: &'static str,
    hotkey_pipe: &'static str,
    allow_privileged_helper: &'static str,
    allow_default_source_claim: &'static str,
    installer_command: &'static str,
}

fn stable_profile() -> Profile {
    Profile {
        id: "stable",
        app_id: "com.linuxsoundboard.app",
        icon: "linux-soundboard",
        binary: "linux-soundboard",
        title: "Linux Soundboard",
        config_dir: "linux-soundboard",
        state_dir: "linux-soundboard",
        update_repo: "germanua/Linux-SoundBoard",
        update_channel: "stable",
        engine_service: "linux-soundboard-engine.service",
        engine_target: "linux-soundboard-engine.target",
        local_playback: "linuxsoundboard.local_playback",
        mic_capture: "linuxsoundboard.mic_capture",
        virtual_source: "linuxsoundboard.virtual_mic",
        virtual_feeder: "linuxsoundboard.virtual_mic_feeder",
        output_description: "Linux_Soundboard_Output",
        mic_description: "Linux_Soundboard_Mic",
        hotkey_pipe: "lsb_hotkey.pipe",
        allow_privileged_helper: "1",
        allow_default_source_claim: "1",
        installer_command: "curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh | bash",
    }
}

fn emit(key: &str, value: &str) {
    println!("cargo:rustc-env={key}={value}");
}

fn main() {
    let profile = stable_profile();
    let app_version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    for (key, value) in [
        ("LSB_PROFILE_ID", profile.id),
        ("LSB_APP_ID", profile.app_id),
        ("LSB_APP_ICON_NAME", profile.icon),
        ("LSB_APP_BINARY", profile.binary),
        ("LSB_APP_TITLE", profile.title),
        ("LSB_APP_VERSION", app_version.as_str()),
        ("LSB_CONFIG_DIR_NAME", profile.config_dir),
        ("LSB_STATE_DIR_NAME", profile.state_dir),
        ("LSB_UPDATE_REPO", profile.update_repo),
        ("LSB_UPDATE_CHANNEL", profile.update_channel),
        ("LSB_ENGINE_SERVICE_NAME", profile.engine_service),
        ("LSB_ENGINE_TARGET_NAME", profile.engine_target),
        ("LSB_LOCAL_PLAYBACK_NODE", profile.local_playback),
        ("LSB_MIC_CAPTURE_NODE", profile.mic_capture),
        ("LSB_VIRTUAL_SOURCE", profile.virtual_source),
        ("LSB_VIRTUAL_FEEDER_NODE", profile.virtual_feeder),
        ("LSB_VIRTUAL_OUTPUT_DESCRIPTION", profile.output_description),
        ("LSB_VIRTUAL_MIC_DESCRIPTION", profile.mic_description),
        ("LSB_HOTKEY_PIPE_NAME", profile.hotkey_pipe),
        (
            "LSB_ALLOW_PRIVILEGED_HELPER",
            profile.allow_privileged_helper,
        ),
        (
            "LSB_ALLOW_DEFAULT_SOURCE_CLAIM",
            profile.allow_default_source_claim,
        ),
        ("LSB_INSTALLER_COMMAND", profile.installer_command),
    ] {
        emit(key, value);
    }

    glib_build_tools::compile_resources(
        &["resources"],
        "resources/resources.gresource.xml",
        "compiled.gresource",
    );
}
