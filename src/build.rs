use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;

struct Profile {
    id: String,
    app_id: String,
    icon: String,
    binary: String,
    title: String,
    config_dir: String,
    state_dir: String,
    update_repo: String,
    update_channel: String,
    engine_service: String,
    engine_target: String,
    local_playback: String,
    mic_capture: String,
    virtual_source: String,
    virtual_feeder: String,
    output_description: String,
    mic_description: String,
    hotkey_pipe: String,
    allow_privileged_helper: String,
    allow_default_source_claim: String,
    installer_command: String,
}

fn stable_profile() -> Profile {
    Profile {
        id: "stable".into(),
        app_id: "com.linuxsoundboard.app".into(),
        icon: "linux-soundboard".into(),
        binary: "linux-soundboard".into(),
        title: "Linux Soundboard".into(),
        config_dir: "linux-soundboard".into(),
        state_dir: "linux-soundboard".into(),
        update_repo: "germanua/Linux-SoundBoard".into(),
        update_channel: "stable".into(),
        engine_service: "linux-soundboard-engine.service".into(),
        engine_target: "linux-soundboard-engine.target".into(),
        local_playback: "linuxsoundboard.local_playback".into(),
        mic_capture: "linuxsoundboard.mic_capture".into(),
        virtual_source: "linuxsoundboard.virtual_mic".into(),
        virtual_feeder: "linuxsoundboard.virtual_mic_feeder".into(),
        output_description: "Linux_Soundboard_Output".into(),
        mic_description: "Linux_Soundboard_Mic".into(),
        hotkey_pipe: "lsb_hotkey.pipe".into(),
        allow_privileged_helper: "1".into(),
        allow_default_source_claim: "1".into(),
        installer_command: "curl -fsSL https://raw.githubusercontent.com/germanua/Linux-SoundBoard/main/bootstrap-install.sh | bash".into(),
    }
}

fn parse_profile_file(path: &Path) -> HashMap<String, String> {
    let content = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let mut values = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (key, raw_value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("invalid profile line in {}: {line}", path.display()));
        let raw_value = raw_value.trim();
        let value = if raw_value.len() >= 2
            && ((raw_value.starts_with('"') && raw_value.ends_with('"'))
                || (raw_value.starts_with('\'') && raw_value.ends_with('\'')))
        {
            raw_value[1..raw_value.len() - 1].to_string()
        } else {
            raw_value.to_string()
        };
        values.insert(key.trim().to_string(), value);
    }
    values
}

fn required(values: &HashMap<String, String>, key: &str) -> String {
    values
        .get(key)
        .cloned()
        .unwrap_or_else(|| panic!("DEV profile is missing {key}"))
}

fn dev_profile() -> Profile {
    let values = parse_profile_file(Path::new("../dev/profile.env"));
    let namespace = required(&values, "PIPEWIRE_NAMESPACE");
    Profile {
        id: "dev".into(),
        app_id: required(&values, "APP_ID"),
        icon: required(&values, "APP_ICON_NAME"),
        binary: required(&values, "APP_BINARY"),
        title: required(&values, "APP_NAME"),
        config_dir: required(&values, "CONFIG_DIR_NAME"),
        state_dir: required(&values, "STATE_DIR_NAME"),
        update_repo: required(&values, "APP_REPO"),
        update_channel: required(&values, "UPDATE_CHANNEL"),
        engine_service: required(&values, "ENGINE_SERVICE_NAME"),
        engine_target: required(&values, "ENGINE_TARGET_NAME"),
        local_playback: format!("{namespace}.local_playback"),
        mic_capture: format!("{namespace}.mic_capture"),
        virtual_source: format!("{namespace}.virtual_mic"),
        virtual_feeder: format!("{namespace}.virtual_mic_feeder"),
        output_description: required(&values, "VIRTUAL_OUTPUT_DESCRIPTION"),
        mic_description: required(&values, "VIRTUAL_MIC_DESCRIPTION"),
        hotkey_pipe: required(&values, "HOTKEY_PIPE_NAME"),
        allow_privileged_helper: required(&values, "LSB_ALLOW_PRIVILEGED_HELPER"),
        allow_default_source_claim: required(&values, "LSB_ALLOW_DEFAULT_SOURCE_CLAIM"),
        installer_command: required(&values, "INSTALLER_COMMAND"),
    }
}

fn profile(name: &str) -> Profile {
    match name {
        "stable" => stable_profile(),
        "dev" => dev_profile(),
        other => panic!("unknown LSB_BUILD_PROFILE '{other}'"),
    }
}
fn emit(key: &str, value: &str) {
    println!("cargo:rustc-env={key}={value}");
}

fn main() {
    println!("cargo:rerun-if-env-changed=LSB_BUILD_PROFILE");
    println!("cargo:rerun-if-env-changed=LSB_DEV_VERSION");
    println!("cargo:rerun-if-changed=../dev/profile.env");
    println!("cargo::rustc-check-cfg=cfg(lsb_dev_profile)");
    let name = env::var("LSB_BUILD_PROFILE").unwrap_or_else(|_| "stable".into());
    let p = profile(&name);
    if p.id == "dev" {
        println!("cargo:rustc-cfg=lsb_dev_profile");
    }
    let package_version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let app_version = if p.id == "dev" {
        let value = env::var("LSB_DEV_VERSION")
            .expect("DEV builds require LSB_DEV_VERSION, e.g. 2.4.7-dev.1");
        if !value.contains("-dev.") {
            panic!("LSB_DEV_VERSION must be a dev prerelease (e.g. 2.4.7-dev.1)");
        }
        value
    } else {
        package_version
    };

    for (key, value) in [
        ("LSB_PROFILE_ID", p.id.as_str()),
        ("LSB_APP_ID", p.app_id.as_str()),
        ("LSB_APP_ICON_NAME", p.icon.as_str()),
        ("LSB_APP_BINARY", p.binary.as_str()),
        ("LSB_APP_TITLE", p.title.as_str()),
        ("LSB_APP_VERSION", app_version.as_str()),
        ("LSB_CONFIG_DIR_NAME", p.config_dir.as_str()),
        ("LSB_STATE_DIR_NAME", p.state_dir.as_str()),
        ("LSB_UPDATE_REPO", p.update_repo.as_str()),
        ("LSB_UPDATE_CHANNEL", p.update_channel.as_str()),
        ("LSB_ENGINE_SERVICE_NAME", p.engine_service.as_str()),
        ("LSB_ENGINE_TARGET_NAME", p.engine_target.as_str()),
        ("LSB_LOCAL_PLAYBACK_NODE", p.local_playback.as_str()),
        ("LSB_MIC_CAPTURE_NODE", p.mic_capture.as_str()),
        ("LSB_VIRTUAL_SOURCE", p.virtual_source.as_str()),
        ("LSB_VIRTUAL_FEEDER_NODE", p.virtual_feeder.as_str()),
        (
            "LSB_VIRTUAL_OUTPUT_DESCRIPTION",
            p.output_description.as_str(),
        ),
        ("LSB_VIRTUAL_MIC_DESCRIPTION", p.mic_description.as_str()),
        ("LSB_HOTKEY_PIPE_NAME", p.hotkey_pipe.as_str()),
        (
            "LSB_ALLOW_PRIVILEGED_HELPER",
            p.allow_privileged_helper.as_str(),
        ),
        (
            "LSB_ALLOW_DEFAULT_SOURCE_CLAIM",
            p.allow_default_source_claim.as_str(),
        ),
        ("LSB_INSTALLER_COMMAND", p.installer_command.as_str()),
    ] {
        emit(key, value);
    }

    glib_build_tools::compile_resources(
        &["resources"],
        "resources/resources.gresource.xml",
        "compiled.gresource",
    );
}
