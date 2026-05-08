//! Persistent virtual microphone installer.
//!
//! Writes a PipeWire or PulseAudio config so `Linux Soundboard Mic` exists
//! at session start — games that enumerate audio devices once at boot can find
//! it without the UI needing to run first.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use log::{info, warn};

use super::command_runner::{CommandOutput, CommandRunner, SystemCommandRunner};
use crate::app_meta::{
    PERSISTENT_VIRTUAL_MIC_CONF_BODY, PERSISTENT_VIRTUAL_MIC_CONF_NAME, PULSE_DEFAULT_PA_SNIPPET,
    VIRTUAL_SOURCE_NAME,
};

const MANAGED_MARKER: &str = "# managed-by: linux-soundboard";
const END_MANAGED_MARKER: &str = "# end-managed-by: linux-soundboard";
const VERIFY_TIMEOUT: Duration = Duration::from_secs(3);
const VERIFY_POLL_INTERVAL: Duration = Duration::from_millis(150);
const PULSE_DEFAULT_PA_INCLUDE: &str = ".include /etc/pulse/default.pa";
const SYSTEM_PIPEWIRE_CONF_PATH: &str =
    "/usr/share/pipewire/pipewire.conf.d/99-linuxsoundboard.conf";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioServer {
    PipeWire,
    PulseAudio,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupOutcome {
    /// Persistent node is live and visible; app should connect to it.
    Ready(AudioServer),
    /// Conf was written but the server could not be restarted or loaded live.
    /// Node will appear after next session restart.
    PendingRestart(AudioServer),
    /// No supported audio server is available.
    Unsupported(String),
    /// Setup attempted but failed; legacy in-process path required.
    Failed(String),
}

impl SetupOutcome {
    pub fn node_available(&self) -> bool {
        matches!(self, SetupOutcome::Ready(_))
    }

    pub fn audio_server(&self) -> Option<AudioServer> {
        match self {
            SetupOutcome::Ready(server) | SetupOutcome::PendingRestart(server) => Some(*server),
            SetupOutcome::Unsupported(_) | SetupOutcome::Failed(_) => None,
        }
    }
}

pub fn ensure_persistent_virtual_mic() -> SetupOutcome {
    ensure_persistent_virtual_mic_with(&SystemCommandRunner, &SystemFs)
}

pub fn ensure_persistent_virtual_mic_with<R: CommandRunner, F: Filesystem>(
    runner: &R,
    fs: &F,
) -> SetupOutcome {
    match detect_audio_server(runner, fs) {
        AudioServer::PipeWire => ensure_pipewire_virtual_mic(runner, fs),
        AudioServer::PulseAudio => ensure_pulseaudio_virtual_mic(runner, fs),
        AudioServer::Unsupported => {
            let msg = "No supported audio server found; virtual mic unavailable".to_string();
            info!("{msg}");
            SetupOutcome::Unsupported(msg)
        }
    }
}

fn ensure_pipewire_virtual_mic<R: CommandRunner, F: Filesystem>(
    runner: &R,
    fs: &F,
) -> SetupOutcome {
    let conf_path = match user_conf_path(fs) {
        Some(path) => path,
        None => {
            let msg = "Could not determine user config directory".to_string();
            warn!("{msg}");
            return SetupOutcome::Failed(msg);
        }
    };

    let write_result = match write_conf_if_needed(fs, &conf_path) {
        Ok(result) => result,
        Err(err) => {
            let msg = format!(
                "Failed to write persistent virtual mic config at {}: {err}",
                conf_path.display()
            );
            warn!("{msg}");
            return SetupOutcome::Failed(msg);
        }
    };

    match write_result {
        WriteResult::WroteNew => {
            info!(
                "Wrote persistent virtual mic config to {}",
                conf_path.display()
            );
        }
        WriteResult::AlreadyManaged => {
            info!(
                "Persistent virtual mic config already present at {}",
                conf_path.display()
            );
        }
        WriteResult::ForeignFile => {
            let msg = format!(
                "Refusing to overwrite non-managed file at {}",
                conf_path.display()
            );
            warn!("{msg}");
            return SetupOutcome::Failed(msg);
        }
    }

    if write_result == WriteResult::AlreadyManaged && pipewire_virtual_source_present(runner) {
        info!("Persistent virtual mic already registered with PipeWire");
        return SetupOutcome::Ready(AudioServer::PipeWire);
    }

    if !user_systemd_available(fs) {
        info!("User systemd unavailable; persistent mic will appear after next session start");
        return SetupOutcome::PendingRestart(AudioServer::PipeWire);
    }

    let units = active_pipewire_units(runner);
    if units.is_empty() {
        info!("No active PipeWire user units detected; skipping restart");
        return SetupOutcome::PendingRestart(AudioServer::PipeWire);
    }

    if let Err(err) = restart_user_units(runner, &units) {
        warn!("Failed to restart PipeWire user units: {err}");
        return SetupOutcome::Failed(err);
    }

    if wait_for_pipewire_virtual_source(runner, VERIFY_TIMEOUT) {
        info!("Persistent virtual mic registered and verified with PipeWire");
        SetupOutcome::Ready(AudioServer::PipeWire)
    } else {
        let msg = format!(
            "Persistent virtual mic did not appear within {} ms after restart",
            VERIFY_TIMEOUT.as_millis()
        );
        warn!("{msg}");
        SetupOutcome::Failed(msg)
    }
}

fn ensure_pulseaudio_virtual_mic<R: CommandRunner, F: Filesystem>(
    runner: &R,
    fs: &F,
) -> SetupOutcome {
    let default_pa_path = match user_pulse_default_pa_path(fs) {
        Some(path) => path,
        None => {
            let msg = "Could not determine PulseAudio user config path".to_string();
            warn!("{msg}");
            return SetupOutcome::Failed(msg);
        }
    };

    let write_result = match write_pulse_default_pa_if_needed(fs, &default_pa_path) {
        Ok(result) => result,
        Err(err) => {
            let msg = format!(
                "Failed to update PulseAudio config at {}: {err}",
                default_pa_path.display()
            );
            warn!("{msg}");
            return SetupOutcome::Failed(msg);
        }
    };

    match write_result {
        WriteResult::WroteNew => {
            info!(
                "Wrote persistent PulseAudio virtual mic block to {}",
                default_pa_path.display()
            );
        }
        WriteResult::AlreadyManaged => {
            info!(
                "PulseAudio virtual mic block already present at {}",
                default_pa_path.display()
            );
        }
        WriteResult::ForeignFile => unreachable!("PulseAudio block updates append safely"),
    }

    if pulseaudio_virtual_source_present(runner) {
        info!("Persistent virtual mic already registered with PulseAudio");
        return SetupOutcome::Ready(AudioServer::PulseAudio);
    }

    if let Err(err) = load_pulseaudio_virtual_mic_live(runner) {
        warn!("Failed to load PulseAudio virtual mic live: {err}");
        return SetupOutcome::PendingRestart(AudioServer::PulseAudio);
    }

    if wait_for_pulseaudio_virtual_source(runner, VERIFY_TIMEOUT) {
        info!("Persistent virtual mic registered and verified with PulseAudio");
        SetupOutcome::Ready(AudioServer::PulseAudio)
    } else {
        let msg = format!(
            "PulseAudio virtual mic did not appear within {} ms after module load",
            VERIFY_TIMEOUT.as_millis()
        );
        warn!("{msg}");
        SetupOutcome::Failed(msg)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum WriteResult {
    WroteNew,
    AlreadyManaged,
    ForeignFile,
}

fn write_conf_if_needed<F: Filesystem>(fs: &F, path: &Path) -> io::Result<WriteResult> {
    let system_conf_matches = fs
        .read_to_string_optional(Path::new(SYSTEM_PIPEWIRE_CONF_PATH))?
        .as_deref()
        .map(conf_matches_persistent_body)
        .unwrap_or(false);

    if let Some(existing) = fs.read_to_string_optional(path)? {
        if existing.contains(MANAGED_MARKER) {
            if system_conf_matches {
                fs.remove_file(path)?;
                return Ok(WriteResult::WroteNew);
            }
            if existing == PERSISTENT_VIRTUAL_MIC_CONF_BODY {
                return Ok(WriteResult::AlreadyManaged);
            }
            fs.write(path, PERSISTENT_VIRTUAL_MIC_CONF_BODY)?;
            return Ok(WriteResult::WroteNew);
        }
        return Ok(WriteResult::ForeignFile);
    }

    if system_conf_matches {
        return Ok(WriteResult::AlreadyManaged);
    }

    if let Some(parent) = path.parent() {
        fs.create_dir_all(parent)?;
    }
    fs.write(path, PERSISTENT_VIRTUAL_MIC_CONF_BODY)?;
    Ok(WriteResult::WroteNew)
}

fn conf_matches_persistent_body(contents: &str) -> bool {
    contents.trim() == PERSISTENT_VIRTUAL_MIC_CONF_BODY.trim()
}

fn write_pulse_default_pa_if_needed<F: Filesystem>(fs: &F, path: &Path) -> io::Result<WriteResult> {
    let existing = fs.read_to_string_optional(path)?;
    let base = existing.unwrap_or_else(|| format!("{PULSE_DEFAULT_PA_INCLUDE}\n"));
    let updated = upsert_managed_block(&base, PULSE_DEFAULT_PA_SNIPPET);
    if updated == base {
        return Ok(WriteResult::AlreadyManaged);
    }
    if let Some(parent) = path.parent() {
        fs.create_dir_all(parent)?;
    }
    fs.write(path, &updated)?;
    Ok(WriteResult::WroteNew)
}

fn upsert_managed_block(existing: &str, block: &str) -> String {
    let block = block.trim_end();
    if let Some(start) = existing.find(MANAGED_MARKER) {
        let end = existing[start..]
            .find(END_MANAGED_MARKER)
            .map(|idx| start + idx + END_MANAGED_MARKER.len())
            .unwrap_or(existing.len());
        let mut updated = String::new();
        updated.push_str(existing[..start].trim_end());
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(block);
        let tail = existing[end..].trim_start_matches(['\r', '\n']);
        if !tail.is_empty() {
            updated.push_str("\n\n");
            updated.push_str(tail.trim_end());
        }
        updated.push('\n');
        updated
    } else {
        let mut updated = existing.trim_end().to_string();
        if !updated.is_empty() {
            updated.push_str("\n\n");
        }
        updated.push_str(block);
        updated.push('\n');
        updated
    }
}

fn user_conf_path<F: Filesystem>(fs: &F) -> Option<PathBuf> {
    let base = fs
        .env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            fs.env("HOME")
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(
        base.join("pipewire")
            .join("pipewire.conf.d")
            .join(PERSISTENT_VIRTUAL_MIC_CONF_NAME),
    )
}

fn user_pulse_default_pa_path<F: Filesystem>(fs: &F) -> Option<PathBuf> {
    let base = fs
        .env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            fs.env("HOME")
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("pulse").join("default.pa"))
}

fn detect_audio_server<R: CommandRunner, F: Filesystem>(runner: &R, fs: &F) -> AudioServer {
    let pipewire_socket = pipewire_socket_present(fs);
    if pipewire_socket {
        return AudioServer::PipeWire;
    }

    if pulseaudio_socket_present(fs) {
        return AudioServer::PulseAudio;
    }

    if let Ok(output) = runner.run("pactl", &["info"]) {
        if output.success && parse_pactl_server_name(&output.stdout).is_some() {
            return AudioServer::PulseAudio;
        }
    }

    AudioServer::Unsupported
}

fn pipewire_socket_present<F: Filesystem>(fs: &F) -> bool {
    let runtime_dir = fs
        .env("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let uid = fs.uid();
            format!("/run/user/{}", uid)
        });
    let socket = PathBuf::from(runtime_dir).join("pipewire-0");
    fs.path_exists(&socket)
}

fn pulseaudio_socket_present<F: Filesystem>(fs: &F) -> bool {
    let runtime_dir = fs
        .env("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            let uid = fs.uid();
            format!("/run/user/{}", uid)
        });
    fs.path_exists(&PathBuf::from(runtime_dir).join("pulse").join("native"))
}

fn parse_pactl_server_name(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "Server Name")
            .then(|| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn user_systemd_available<F: Filesystem>(fs: &F) -> bool {
    let runtime_dir = match fs.env("XDG_RUNTIME_DIR") {
        Some(dir) if !dir.is_empty() => dir,
        _ => format!("/run/user/{}", fs.uid()),
    };
    fs.path_exists(&PathBuf::from(runtime_dir).join("systemd").join("private"))
}

fn active_pipewire_units<R: CommandRunner>(runner: &R) -> Vec<&'static str> {
    const CANDIDATES: &[&str] = &[
        "wireplumber.service",
        "pipewire-media-session.service",
        "pipewire-pulse.service",
        "pipewire.service",
    ];

    CANDIDATES
        .iter()
        .copied()
        .filter(|unit| unit_is_active(runner, unit))
        .collect()
}

fn unit_is_active<R: CommandRunner>(runner: &R, unit: &str) -> bool {
    matches!(
        runner.run("systemctl", &["--user", "is-active", "--quiet", unit]),
        Ok(CommandOutput { success: true, .. })
    )
}

fn restart_user_units<R: CommandRunner>(runner: &R, units: &[&str]) -> Result<(), String> {
    let mut args = vec!["--user", "restart"];
    args.extend_from_slice(units);
    let output = runner
        .run("systemctl", &args)
        .map_err(|e| format!("systemctl --user restart failed to spawn: {e}"))?;
    if output.success {
        return Ok(());
    }
    let stderr = output.stderr.trim();
    let stdout = output.stdout.trim();
    let detail = if !stderr.is_empty() { stderr } else { stdout };
    Err(format!("systemctl --user restart failed: {detail}"))
}

fn wait_for_pipewire_virtual_source<R: CommandRunner>(runner: &R, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pipewire_virtual_source_present(runner) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(VERIFY_POLL_INTERVAL);
    }
}

fn wait_for_pulseaudio_virtual_source<R: CommandRunner>(runner: &R, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if pulseaudio_virtual_source_present(runner) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(VERIFY_POLL_INTERVAL);
    }
}

fn pipewire_virtual_source_present<R: CommandRunner>(runner: &R) -> bool {
    node_present(runner, VIRTUAL_SOURCE_NAME)
}

fn pulseaudio_virtual_source_present<R: CommandRunner>(runner: &R) -> bool {
    let Ok(output) = runner.run("pactl", &["list", "short", "sources"]) else {
        return false;
    };
    output.success && pactl_short_list_contains_name(&output.stdout, VIRTUAL_SOURCE_NAME)
}

fn pactl_short_list_contains_name(stdout: &str, name: &str) -> bool {
    stdout.lines().any(|line| {
        let mut fields = line.split_whitespace();
        let _id = fields.next();
        fields.next() == Some(name)
    })
}

fn load_pulseaudio_virtual_mic_live<R: CommandRunner>(runner: &R) -> Result<(), String> {
    if pulseaudio_virtual_source_present(runner) {
        return Ok(());
    }

    let output = runner
        .run(
            "pactl",
            &[
                "load-module",
                "module-null-sink",
                "media.class=Audio/Source/Virtual",
                "sink_name=linuxsoundboard.virtual_mic",
                "sink_properties=device.description=Linux\\ Soundboard\\ Mic",
            ],
        )
        .map_err(|e| format!("pactl load-module failed to spawn: {e}"))?;
    if output.success {
        Ok(())
    } else {
        let detail = if output.stderr.trim().is_empty() {
            output.stdout.trim()
        } else {
            output.stderr.trim()
        };
        Err(format!("pactl load-module failed: {detail}"))
    }
}

/// Demotes our virtual mic from the WirePlumber configured default and restores
/// a physical source in its place.
///
/// **Not called in production.** Kept for manual rescue use (e.g. via a
/// diagnostic CLI flag) in case a user's system is left with our virtual mic
/// as the permanent default. The Soundpad-style design intentionally keeps the
/// virtual mic as the WirePlumber default across reboots; this function exists
/// only as an escape hatch if the user wants to undo that.
///
/// Returns `Some(restored_node_name)` if a fix was applied, `None` if nothing
/// needed doing or no replacement source was available.
#[allow(dead_code)]
pub fn cleanup_stale_default_source() -> Option<String> {
    cleanup_stale_default_source_with(&SystemCommandRunner)
}

#[allow(dead_code)]
pub fn cleanup_stale_default_source_for(server: AudioServer) -> Option<String> {
    match server {
        AudioServer::PipeWire => cleanup_stale_default_source_with(&SystemCommandRunner),
        AudioServer::PulseAudio => {
            cleanup_stale_pulseaudio_default_source_with(&SystemCommandRunner)
        }
        AudioServer::Unsupported => None,
    }
}

pub fn cleanup_stale_default_source_with<R: CommandRunner>(runner: &R) -> Option<String> {
    let current_is_ours =
        inspect_default_source_node_name(runner).as_deref() == Some(VIRTUAL_SOURCE_NAME);
    let configured_is_ours =
        configured_default_source_node_name(runner).as_deref() == Some(VIRTUAL_SOURCE_NAME);
    if !current_is_ours && !configured_is_ours {
        return None;
    }

    let Some((replacement_id, replacement_name)) = pick_replacement_default_source(runner) else {
        warn!(
            "{} is the system default source but no replacement is available",
            VIRTUAL_SOURCE_NAME
        );
        return None;
    };

    match wpctl_set_default(runner, replacement_id) {
        Ok(()) => {
            info!(
                "Demoted {} from system default; restored {} (id {})",
                VIRTUAL_SOURCE_NAME, replacement_name, replacement_id
            );
            Some(replacement_name)
        }
        Err(err) => {
            warn!("Failed to restore default source: {err}");
            None
        }
    }
}

fn cleanup_stale_pulseaudio_default_source_with<R: CommandRunner>(runner: &R) -> Option<String> {
    let current = pulseaudio_default_source_name(runner)?;
    if current != VIRTUAL_SOURCE_NAME {
        return None;
    }

    let replacement = pick_pulseaudio_replacement_source(runner)?;
    let output = runner
        .run("pactl", &["set-default-source", replacement.as_str()])
        .ok()?;
    if output.success {
        info!(
            "Demoted {} from PulseAudio default source; restored {}",
            VIRTUAL_SOURCE_NAME, replacement
        );
        Some(replacement)
    } else {
        warn!("Failed to restore PulseAudio default source");
        None
    }
}

fn pulseaudio_default_source_name<R: CommandRunner>(runner: &R) -> Option<String> {
    let output = runner.run("pactl", &["get-default-source"]).ok()?;
    output
        .success
        .then(|| output.stdout.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn pick_pulseaudio_replacement_source<R: CommandRunner>(runner: &R) -> Option<String> {
    let output = runner.run("pactl", &["list", "short", "sources"]).ok()?;
    if !output.success {
        return None;
    }
    output.stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let _id = fields.next()?;
        let name = fields.next()?;
        (name != VIRTUAL_SOURCE_NAME && !name.ends_with(".monitor")).then(|| name.to_string())
    })
}

fn inspect_default_source_node_name<R: CommandRunner>(runner: &R) -> Option<String> {
    let output = runner.run("wpctl", &["inspect", "@DEFAULT_SOURCE@"]).ok()?;
    if !output.success {
        return None;
    }
    parse_node_name_from_inspect(&output.stdout)
}

fn configured_default_source_node_name<R: CommandRunner>(runner: &R) -> Option<String> {
    let output = runner.run("wpctl", &["status", "-n"]).ok()?;
    if !output.success {
        return None;
    }
    parse_configured_default_source_name(&output.stdout)
}

fn parse_node_name_from_inspect(stdout: &str) -> Option<String> {
    for line in stdout.lines() {
        let line = line.trim();
        // Accept either `node.name = "..."` or `* node.name = "..."`.
        let stripped = line.strip_prefix('*').map(str::trim_start).unwrap_or(line);
        if let Some(rest) = stripped.strip_prefix("node.name") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                let value = value.trim();
                let value = value.strip_prefix('"').unwrap_or(value);
                let value = value.strip_suffix('"').unwrap_or(value);
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

fn parse_configured_default_source_name(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let index = fields.next()?;
        if !index.ends_with('.') {
            return None;
        }
        (fields.next()? == "Audio/Source").then(|| fields.next().map(str::to_string))?
    })
}

/// Picks an ID/name to install as the replacement default source. Prefers
/// physical mics (Audio/Source class, no virtualness), falls back to any
/// non-soundboard source if no physical source exists. Returns None if the
/// only sources we can see are ours.
fn pick_replacement_default_source<R: CommandRunner>(runner: &R) -> Option<(u32, String)> {
    let output = runner.run("pw-cli", &["list-objects", "Node"]).ok()?;
    if !output.success {
        return None;
    }
    let candidates = parse_source_candidates(&output.stdout);
    candidates
        .iter()
        .find(|c| c.media_class == "Audio/Source" && !c.is_ours())
        .or_else(|| candidates.iter().find(|c| !c.is_ours()))
        .map(|c| (c.id, c.node_name.clone()))
}

#[derive(Debug, Default)]
struct SourceCandidate {
    id: u32,
    node_name: String,
    media_class: String,
}

impl SourceCandidate {
    fn is_ours(&self) -> bool {
        self.node_name == VIRTUAL_SOURCE_NAME
    }
}

/// Parses `pw-cli list-objects Node` output into source candidates. The output
/// format is one block per object, separated by blank lines, with `id 12, type ...`
/// header followed by indented `key = value` lines.
fn parse_source_candidates(stdout: &str) -> Vec<SourceCandidate> {
    let mut out = Vec::new();
    let mut current = SourceCandidate::default();
    let mut have_header = false;

    let push_if_source = |c: SourceCandidate, out: &mut Vec<SourceCandidate>| {
        if c.id != 0
            && (c.media_class == "Audio/Source" || c.media_class == "Audio/Source/Virtual")
            && !c.node_name.is_empty()
            && !c.node_name.ends_with(".monitor")
        {
            out.push(c);
        }
    };

    for line in stdout.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("id ") {
            // New object header — flush previous.
            if have_header {
                push_if_source(std::mem::take(&mut current), &mut out);
            }
            have_header = true;
            if let Some(comma_idx) = rest.find(',') {
                if let Ok(id) = rest[..comma_idx].trim().parse::<u32>() {
                    current.id = id;
                }
            }
            continue;
        }
        if !have_header {
            continue;
        }
        let line_for_kv = trimmed
            .strip_prefix('*')
            .map(str::trim_start)
            .unwrap_or(trimmed);
        if let Some(rest) = line_for_kv.strip_prefix("media.class") {
            if let Some(value) = parse_quoted_value(rest) {
                current.media_class = value;
            }
        } else if let Some(rest) = line_for_kv.strip_prefix("node.name") {
            if let Some(value) = parse_quoted_value(rest) {
                current.node_name = value;
            }
        }
    }
    if have_header {
        push_if_source(current, &mut out);
    }
    out
}

fn parse_quoted_value(after_key: &str) -> Option<String> {
    let trimmed = after_key.trim_start();
    let value = trimmed.strip_prefix('=')?.trim();
    let value = value.strip_prefix('"').unwrap_or(value);
    let value = value.strip_suffix('"').unwrap_or(value);
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn wpctl_set_default<R: CommandRunner>(runner: &R, source_id: u32) -> Result<(), String> {
    let id_str = source_id.to_string();
    let output = runner
        .run("wpctl", &["set-default", id_str.as_str()])
        .map_err(|e| e.to_string())?;
    if output.success {
        Ok(())
    } else {
        let detail = if !output.stderr.trim().is_empty() {
            output.stderr
        } else {
            output.stdout
        };
        Err(detail.trim().to_string())
    }
}

fn node_present<R: CommandRunner>(runner: &R, node_name: &str) -> bool {
    if let Ok(output) = runner.run("pw-cli", &["list-objects", "Node"]) {
        if output.success && output.stdout.contains(node_name) {
            return true;
        }
    }
    if let Ok(output) = runner.run("wpctl", &["status", "-n"]) {
        if output.success && output.stdout.contains(node_name) {
            return true;
        }
    }
    false
}

pub trait Filesystem {
    fn read_to_string_optional(&self, path: &Path) -> io::Result<Option<String>>;
    fn write(&self, path: &Path, contents: &str) -> io::Result<()>;
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    fn create_dir_all(&self, path: &Path) -> io::Result<()>;
    fn path_exists(&self, path: &Path) -> bool;
    fn env(&self, key: &str) -> Option<String>;
    fn uid(&self) -> u32;
}

pub struct SystemFs;

impl Filesystem for SystemFs {
    fn read_to_string_optional(&self, path: &Path) -> io::Result<Option<String>> {
        match std::fs::read_to_string(path) {
            Ok(value) => Ok(Some(value)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
        std::fs::write(path, contents)
    }

    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir_all(path)
    }

    fn path_exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn env(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn uid(&self) -> u32 {
        nix::unistd::getuid().as_raw()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct FakeFs {
        files: RefCell<HashMap<PathBuf, String>>,
        dirs: RefCell<Vec<PathBuf>>,
        env: HashMap<String, String>,
        uid: u32,
    }

    impl Filesystem for FakeFs {
        fn read_to_string_optional(&self, path: &Path) -> io::Result<Option<String>> {
            Ok(self.files.borrow().get(path).cloned())
        }

        fn write(&self, path: &Path, contents: &str) -> io::Result<()> {
            self.files
                .borrow_mut()
                .insert(path.to_path_buf(), contents.to_string());
            Ok(())
        }

        fn remove_file(&self, path: &Path) -> io::Result<()> {
            self.files.borrow_mut().remove(path);
            Ok(())
        }

        fn create_dir_all(&self, path: &Path) -> io::Result<()> {
            self.dirs.borrow_mut().push(path.to_path_buf());
            Ok(())
        }

        fn path_exists(&self, path: &Path) -> bool {
            self.files.borrow().contains_key(path)
                || self.dirs.borrow().iter().any(|dir| dir == path)
        }

        fn env(&self, key: &str) -> Option<String> {
            self.env.get(key).cloned()
        }

        fn uid(&self) -> u32 {
            self.uid
        }
    }

    struct ScriptedRunner {
        responses: HashMap<(String, Vec<String>), CommandOutput>,
    }

    impl CommandRunner for ScriptedRunner {
        fn run(&self, program: &str, args: &[&str]) -> io::Result<CommandOutput> {
            self.responses
                .get(&(
                    program.to_string(),
                    args.iter().map(|arg| (*arg).to_string()).collect(),
                ))
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no scripted response"))
        }
    }

    fn ok(stdout: &str) -> CommandOutput {
        CommandOutput {
            success: true,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    fn key(program: &str, args: &[&str]) -> (String, Vec<String>) {
        (
            program.to_string(),
            args.iter().map(|arg| (*arg).to_string()).collect(),
        )
    }

    #[test]
    fn skips_setup_when_pipewire_socket_missing() {
        let fs = FakeFs {
            uid: 1000,
            ..Default::default()
        };
        let runner = ScriptedRunner {
            responses: HashMap::new(),
        };

        let outcome = ensure_persistent_virtual_mic_with(&runner, &fs);
        assert!(matches!(outcome, SetupOutcome::Unsupported(_)));
    }

    #[test]
    fn returns_ready_when_node_already_registered() {
        let mut fs = FakeFs {
            uid: 1000,
            ..Default::default()
        };
        fs.env
            .insert("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string());
        fs.env
            .insert("HOME".to_string(), "/home/tester".to_string());
        fs.dirs.borrow_mut().push(PathBuf::from("/run/user/1000"));
        fs.files
            .borrow_mut()
            .insert(PathBuf::from("/run/user/1000/pipewire-0"), String::new());
        fs.files.borrow_mut().insert(
            PathBuf::from("/home/tester/.config/pipewire/pipewire.conf.d")
                .join(PERSISTENT_VIRTUAL_MIC_CONF_NAME),
            PERSISTENT_VIRTUAL_MIC_CONF_BODY.to_string(),
        );

        let mut responses = HashMap::new();
        responses.insert(
            key("pw-cli", &["list-objects", "Node"]),
            ok(&format!("node.name = \"{}\"", VIRTUAL_SOURCE_NAME)),
        );
        let runner = ScriptedRunner { responses };

        assert_eq!(
            ensure_persistent_virtual_mic_with(&runner, &fs),
            SetupOutcome::Ready(AudioServer::PipeWire)
        );
    }

    #[test]
    fn write_conf_refuses_foreign_file() {
        let fs = FakeFs::default();
        let path = PathBuf::from("/tmp/99-linuxsoundboard.conf");
        fs.files
            .borrow_mut()
            .insert(path.clone(), "user-edited content".to_string());
        assert_eq!(
            write_conf_if_needed(&fs, &path).unwrap(),
            WriteResult::ForeignFile
        );
    }

    #[test]
    fn write_conf_writes_when_missing() {
        let fs = FakeFs::default();
        let path = PathBuf::from("/tmp/conf.d/99-linuxsoundboard.conf");
        assert_eq!(
            write_conf_if_needed(&fs, &path).unwrap(),
            WriteResult::WroteNew
        );
        assert_eq!(
            fs.files.borrow().get(&path).cloned().unwrap(),
            PERSISTENT_VIRTUAL_MIC_CONF_BODY.to_string()
        );
    }

    #[test]
    fn write_conf_uses_packaged_system_conf_when_present() {
        let fs = FakeFs::default();
        let user_path = PathBuf::from("/home/tester/.config/pipewire/pipewire.conf.d")
            .join(PERSISTENT_VIRTUAL_MIC_CONF_NAME);
        fs.files.borrow_mut().insert(
            PathBuf::from(SYSTEM_PIPEWIRE_CONF_PATH),
            PERSISTENT_VIRTUAL_MIC_CONF_BODY.to_string(),
        );

        assert_eq!(
            write_conf_if_needed(&fs, &user_path).unwrap(),
            WriteResult::AlreadyManaged
        );
        assert!(!fs.files.borrow().contains_key(&user_path));
    }

    #[test]
    fn write_conf_removes_managed_user_duplicate_when_system_conf_present() {
        let fs = FakeFs::default();
        let user_path = PathBuf::from("/home/tester/.config/pipewire/pipewire.conf.d")
            .join(PERSISTENT_VIRTUAL_MIC_CONF_NAME);
        fs.files.borrow_mut().insert(
            PathBuf::from(SYSTEM_PIPEWIRE_CONF_PATH),
            PERSISTENT_VIRTUAL_MIC_CONF_BODY.to_string(),
        );
        fs.files.borrow_mut().insert(
            user_path.clone(),
            PERSISTENT_VIRTUAL_MIC_CONF_BODY.to_string(),
        );

        assert_eq!(
            write_conf_if_needed(&fs, &user_path).unwrap(),
            WriteResult::WroteNew
        );
        assert!(!fs.files.borrow().contains_key(&user_path));
    }

    #[test]
    fn write_conf_idempotent_for_managed_match() {
        let fs = FakeFs::default();
        let path = PathBuf::from("/tmp/99-linuxsoundboard.conf");
        fs.files
            .borrow_mut()
            .insert(path.clone(), PERSISTENT_VIRTUAL_MIC_CONF_BODY.to_string());
        assert_eq!(
            write_conf_if_needed(&fs, &path).unwrap(),
            WriteResult::AlreadyManaged
        );
    }

    #[test]
    fn write_conf_migrates_managed_loopback_config() {
        let fs = FakeFs::default();
        let path = PathBuf::from("/tmp/99-linuxsoundboard.conf");
        fs.files.borrow_mut().insert(
            path.clone(),
            "# managed-by: linux-soundboard\ncontext.modules = [ { name = libpipewire-module-loopback } ]\n".to_string(),
        );

        assert_eq!(
            write_conf_if_needed(&fs, &path).unwrap(),
            WriteResult::WroteNew
        );
        let written = fs.files.borrow().get(&path).cloned().unwrap();
        assert!(written.contains("factory.name     = support.null-audio-sink"));
        assert!(!written.contains("libpipewire-module-loopback"));
    }

    #[test]
    fn ensure_pipewire_migrates_config_before_accepting_live_old_node() {
        let mut fs = FakeFs {
            uid: 1000,
            ..Default::default()
        };
        fs.env
            .insert("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string());
        fs.env
            .insert("HOME".to_string(), "/home/tester".to_string());
        fs.files
            .borrow_mut()
            .insert(PathBuf::from("/run/user/1000/pipewire-0"), String::new());
        let conf_path = PathBuf::from("/home/tester/.config/pipewire/pipewire.conf.d")
            .join(PERSISTENT_VIRTUAL_MIC_CONF_NAME);
        fs.files.borrow_mut().insert(
            conf_path.clone(),
            "# managed-by: linux-soundboard\ncontext.modules = [ { name = libpipewire-module-loopback } ]\n".to_string(),
        );

        let mut responses = HashMap::new();
        responses.insert(
            key("pw-cli", &["list-objects", "Node"]),
            ok(&format!("node.name = \"{}\"", VIRTUAL_SOURCE_NAME)),
        );
        let runner = ScriptedRunner { responses };

        assert_eq!(
            ensure_persistent_virtual_mic_with(&runner, &fs),
            SetupOutcome::PendingRestart(AudioServer::PipeWire)
        );
        let written = fs.files.borrow().get(&conf_path).cloned().unwrap();
        assert!(written.contains("factory.name     = support.null-audio-sink"));
        assert!(!written.contains("libpipewire-module-loopback"));
    }

    #[test]
    fn detects_audio_server_from_sockets() {
        let mut fs = FakeFs {
            uid: 1000,
            ..Default::default()
        };
        fs.env
            .insert("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string());
        fs.files
            .borrow_mut()
            .insert(PathBuf::from("/run/user/1000/pulse/native"), String::new());
        let runner = ScriptedRunner {
            responses: HashMap::new(),
        };
        assert_eq!(detect_audio_server(&runner, &fs), AudioServer::PulseAudio);

        fs.files
            .borrow_mut()
            .insert(PathBuf::from("/run/user/1000/pipewire-0"), String::new());
        assert_eq!(detect_audio_server(&runner, &fs), AudioServer::PipeWire);
    }

    #[test]
    fn detects_pulseaudio_from_pactl_info() {
        let fs = FakeFs {
            uid: 1000,
            ..Default::default()
        };
        let mut responses = HashMap::new();
        responses.insert(
            key("pactl", &["info"]),
            ok("Server Name: pulseaudio\nDefault Source: alsa_input.physical\n"),
        );
        let runner = ScriptedRunner { responses };
        assert_eq!(detect_audio_server(&runner, &fs), AudioServer::PulseAudio);
    }

    #[test]
    fn ensure_pulse_writes_default_pa_before_accepting_live_source() {
        let mut fs = FakeFs {
            uid: 1000,
            ..Default::default()
        };
        fs.env
            .insert("XDG_RUNTIME_DIR".to_string(), "/run/user/1000".to_string());
        fs.env
            .insert("HOME".to_string(), "/home/tester".to_string());
        fs.files
            .borrow_mut()
            .insert(PathBuf::from("/run/user/1000/pulse/native"), String::new());

        let mut responses = HashMap::new();
        responses.insert(
            key("pactl", &["list", "short", "sources"]),
            ok(&format!(
                "1\t{}\tPipeWire\tfloat32le 2ch 48000Hz\n",
                VIRTUAL_SOURCE_NAME
            )),
        );
        let runner = ScriptedRunner { responses };

        assert_eq!(
            ensure_persistent_virtual_mic_with(&runner, &fs),
            SetupOutcome::Ready(AudioServer::PulseAudio)
        );
        let path = PathBuf::from("/home/tester/.config/pulse/default.pa");
        let written = fs.files.borrow().get(&path).cloned().unwrap();
        assert!(written.contains(PULSE_DEFAULT_PA_INCLUDE));
        assert!(written.contains("sink_name=linuxsoundboard.virtual_mic"));
    }

    #[test]
    fn pulse_default_pa_block_appends_and_updates() {
        let original = "load-module module-native-protocol-unix\n";
        let updated = upsert_managed_block(original, PULSE_DEFAULT_PA_SNIPPET);
        assert!(updated.contains("load-module module-native-protocol-unix"));
        assert!(updated.contains("sink_name=linuxsoundboard.virtual_mic"));

        let replaced = upsert_managed_block(
            &updated.replace(
                "sink_name=linuxsoundboard.virtual_mic",
                "sink_name=old_name",
            ),
            PULSE_DEFAULT_PA_SNIPPET,
        );
        assert!(replaced.contains("sink_name=linuxsoundboard.virtual_mic"));
        assert!(!replaced.contains("sink_name=old_name"));
        assert_eq!(replaced.matches(MANAGED_MARKER).count(), 1);
    }

    #[test]
    fn pactl_short_list_checks_exact_source_name() {
        let stdout = "1\tlinuxsoundboard.virtual_mic\tmodule-null-sink.c\tfloat32le 2ch 48000Hz\n";
        assert!(pactl_short_list_contains_name(stdout, VIRTUAL_SOURCE_NAME));
        assert!(!pactl_short_list_contains_name(
            "1\tlinuxsoundboard.virtual_mic.monitor\tmodule-null-sink.c\n",
            VIRTUAL_SOURCE_NAME
        ));
    }

    #[test]
    fn parse_node_name_from_inspect_handles_quoted_value() {
        let stdout = "id 12, type PipeWire:Interface:Node\n  * node.name = \"linuxsoundboard.virtual_mic\"\n";
        assert_eq!(
            parse_node_name_from_inspect(stdout).as_deref(),
            Some("linuxsoundboard.virtual_mic")
        );
    }

    #[test]
    fn parse_configured_default_source_name_reads_wpctl_status() {
        let stdout = r#"Settings
 └─ Default Configured Devices:
         0. Audio/Sink    alsa_output.pci-0000_12_00.6.analog-stereo
         1. Audio/Source  linuxsoundboard.virtual_mic
"#;
        assert_eq!(
            parse_configured_default_source_name(stdout).as_deref(),
            Some(VIRTUAL_SOURCE_NAME)
        );
    }

    #[test]
    fn parse_source_candidates_extracts_audio_sources() {
        let stdout = r#"id 100, type PipeWire:Interface:Node
            *           media.class = "Audio/Source"
            *             node.name = "alsa_input.pci-0000_12_00.6.analog-stereo"
        id 101, type PipeWire:Interface:Node
            *           media.class = "Audio/Source/Virtual"
            *             node.name = "easyeffects_source"
        id 102, type PipeWire:Interface:Node
            *           media.class = "Audio/Sink"
            *             node.name = "linuxsoundboard.virtual_mic_sink"
        id 103, type PipeWire:Interface:Node
            *           media.class = "Audio/Source/Virtual"
            *             node.name = "linuxsoundboard.virtual_mic"
        "#;
        let candidates = parse_source_candidates(stdout);
        let names: Vec<_> = candidates.iter().map(|c| c.node_name.as_str()).collect();
        assert!(names.contains(&"alsa_input.pci-0000_12_00.6.analog-stereo"));
        assert!(names.contains(&"easyeffects_source"));
        assert!(names.contains(&"linuxsoundboard.virtual_mic"));
        assert!(!names.contains(&"linuxsoundboard.virtual_mic_sink"));
    }

    #[test]
    fn cleanup_no_op_when_default_is_not_ours() {
        let mut responses = HashMap::new();
        responses.insert(
            key("wpctl", &["inspect", "@DEFAULT_SOURCE@"]),
            ok("id 89, type PipeWire:Interface:Node\n  * node.name = \"alsa_input.pci-0000_12_00.6.analog-stereo\"\n"),
        );
        let runner = ScriptedRunner { responses };
        assert_eq!(cleanup_stale_default_source_with(&runner), None);
    }

    #[test]
    fn cleanup_demotes_when_configured_default_is_ours_but_current_is_physical() {
        let mut responses = HashMap::new();
        responses.insert(
            key("wpctl", &["inspect", "@DEFAULT_SOURCE@"]),
            ok("id 89, type PipeWire:Interface:Node\n  * node.name = \"alsa_input.physical-mic\"\n"),
        );
        responses.insert(
            key("wpctl", &["status", "-n"]),
            ok(&format!(
                r#"Settings
 └─ Default Configured Devices:
         0. Audio/Sink    alsa_output.pci-0000_12_00.6.analog-stereo
         1. Audio/Source  {}
"#,
                VIRTUAL_SOURCE_NAME
            )),
        );
        responses.insert(
            key("pw-cli", &["list-objects", "Node"]),
            ok(&format!(
                r#"id 100, type PipeWire:Interface:Node
                    *           media.class = "Audio/Source"
                    *             node.name = "alsa_input.physical-mic"
                id 50, type PipeWire:Interface:Node
                    *           media.class = "Audio/Source/Virtual"
                    *             node.name = "{}"
                "#,
                VIRTUAL_SOURCE_NAME
            )),
        );
        responses.insert(key("wpctl", &["set-default", "100"]), ok(""));
        let runner = ScriptedRunner { responses };

        assert_eq!(
            cleanup_stale_default_source_with(&runner),
            Some("alsa_input.physical-mic".to_string())
        );
    }

    #[test]
    fn cleanup_demotes_when_we_are_default_and_picks_physical() {
        let mut responses = HashMap::new();
        responses.insert(
            key("wpctl", &["inspect", "@DEFAULT_SOURCE@"]),
            ok(&format!(
                "id 50, type PipeWire:Interface:Node\n  * node.name = \"{}\"\n",
                VIRTUAL_SOURCE_NAME
            )),
        );
        responses.insert(
            key("pw-cli", &["list-objects", "Node"]),
            ok(&format!(
                r#"id 100, type PipeWire:Interface:Node
                    *           media.class = "Audio/Source"
                    *             node.name = "alsa_input.physical-mic"
                id 101, type PipeWire:Interface:Node
                    *           media.class = "Audio/Source/Virtual"
                    *             node.name = "easyeffects_source"
                id 50, type PipeWire:Interface:Node
                    *           media.class = "Audio/Source/Virtual"
                    *             node.name = "{}"
                "#,
                VIRTUAL_SOURCE_NAME
            )),
        );
        responses.insert(key("wpctl", &["set-default", "100"]), ok(""));
        let runner = ScriptedRunner { responses };

        assert_eq!(
            cleanup_stale_default_source_with(&runner),
            Some("alsa_input.physical-mic".to_string())
        );
    }

    #[test]
    fn cleanup_falls_back_to_virtual_source_when_no_physical_available() {
        let mut responses = HashMap::new();
        responses.insert(
            key("wpctl", &["inspect", "@DEFAULT_SOURCE@"]),
            ok(&format!(
                "id 50, type PipeWire:Interface:Node\n  * node.name = \"{}\"\n",
                VIRTUAL_SOURCE_NAME
            )),
        );
        responses.insert(
            key("pw-cli", &["list-objects", "Node"]),
            ok(&format!(
                r#"id 101, type PipeWire:Interface:Node
                    *           media.class = "Audio/Source/Virtual"
                    *             node.name = "easyeffects_source"
                id 50, type PipeWire:Interface:Node
                    *           media.class = "Audio/Source/Virtual"
                    *             node.name = "{}"
                "#,
                VIRTUAL_SOURCE_NAME
            )),
        );
        responses.insert(key("wpctl", &["set-default", "101"]), ok(""));
        let runner = ScriptedRunner { responses };

        assert_eq!(
            cleanup_stale_default_source_with(&runner),
            Some("easyeffects_source".to_string())
        );
    }

    #[test]
    fn cleanup_returns_none_when_only_our_sources_visible() {
        let mut responses = HashMap::new();
        responses.insert(
            key("wpctl", &["inspect", "@DEFAULT_SOURCE@"]),
            ok(&format!(
                "id 50, type PipeWire:Interface:Node\n  * node.name = \"{}\"\n",
                VIRTUAL_SOURCE_NAME
            )),
        );
        responses.insert(
            key("pw-cli", &["list-objects", "Node"]),
            ok(&format!(
                r#"id 50, type PipeWire:Interface:Node
                    *           media.class = "Audio/Source/Virtual"
                    *             node.name = "{}"
                "#,
                VIRTUAL_SOURCE_NAME
            )),
        );
        let runner = ScriptedRunner { responses };
        assert_eq!(cleanup_stale_default_source_with(&runner), None);
    }
}
