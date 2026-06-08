//! Runtime null-sink for the virtual microphone.
//!
//! EasyEffects, pavucontrol's "Create Virtual Source", NoiseTorch — they all
//! create their source via `module-null-sink` with `media.class` overridden
//! to `Audio/Source/Virtual`. This produces a *real* adapter/driver node in
//! the PipeWire graph that WirePlumber will accept as the system default
//! source. A plain `pw::stream::StreamRc` proxy will NOT — WirePlumber's
//! default-source policy refuses to pin a stream-proxy as the resolved
//! `default.audio.source`, even when `default.configured.audio.source` names
//! it (verified live).
//!
//! Lifecycle: loaded at engine start via `pactl load-module`, unloaded at
//! shutdown via `pactl unload-module`. No system config files are touched —
//! the module is purely a runtime side-effect of the running engine, the
//! same way pavucontrol or NoiseTorch loads it from the GUI.

#[cfg(not(test))]
use std::process::Command;
use std::time::{Duration, Instant};

use log::{info, warn};

use super::default_source::claim_default_source_if_enabled;
use super::explicit_links::drop_feeder_links;
use super::loop_state::LoopState;
use super::pw_backend::BackendState;
use super::EngineError;
use crate::app_meta::{VIRTUAL_MIC_DESCRIPTION, VIRTUAL_SOURCE_NAME};

const VIRTUAL_MIC_REPAIR_DELAY: Duration = Duration::from_secs(2);
const VIRTUAL_MIC_REPAIR_COOLDOWN: Duration = Duration::from_secs(5);

/// Owning handle for a `pactl`-loaded `module-null-sink` instance.
/// Dropping this calls `pactl unload-module <id>`.
pub(super) struct NullSinkModule {
    pub(super) module_id: u32,
}

impl NullSinkModule {
    /// Load a fresh null-sink. Before loading, eagerly clean up any stale
    /// instances from prior engine crashes — keeping the graph free of
    /// duplicate null-sinks under the same `sink_name`.
    #[cfg(not(test))]
    pub(super) fn load_or_attach() -> Result<Self, EngineError> {
        // Unload any prior null-sink instances with our sink_name. Each
        // engine-Drop should already do this, but a SIGKILL (or a crash
        // before Drop runs) can leave duplicates behind. Idempotent on the
        // common case (no stale instances).
        let stale = find_all_existing_module_ids();
        if !stale.is_empty() {
            info!(
                "Unloading {} stale null-sink instance(s) before fresh load",
                stale.len()
            );
            for id in stale {
                let _ = Command::new("pactl")
                    .args(["unload-module", &id.to_string()])
                    .output();
            }
        }

        let args = build_load_args();
        let output = Command::new("pactl")
            .arg("load-module")
            .arg("module-null-sink")
            .args(&args)
            .output()
            .map_err(|err| {
                EngineError::Setup(format!("Failed to spawn pactl load-module: {err}"))
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(EngineError::Setup(format!(
                "pactl load-module failed: {}",
                stderr.trim()
            )));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        let id_text = stdout.trim();
        let module_id: u32 = id_text.parse().map_err(|_| {
            EngineError::Setup(format!(
                "pactl load-module returned non-numeric module id: {:?}",
                id_text
            ))
        })?;
        info!(
            "Loaded null-sink module (id={}) as {}",
            module_id, VIRTUAL_SOURCE_NAME
        );
        Ok(Self { module_id })
    }

    #[cfg(test)]
    pub(super) fn load_or_attach() -> Result<Self, EngineError> {
        Ok(Self { module_id: 0 })
    }

    #[cfg(not(test))]
    pub(super) fn is_loaded(&self) -> bool {
        find_all_existing_module_ids()
            .into_iter()
            .any(|id| id == self.module_id)
    }

    #[cfg(test)]
    pub(super) fn is_loaded(&self) -> bool {
        self.module_id == 0
    }
}

#[cfg(not(test))]
impl Drop for NullSinkModule {
    fn drop(&mut self) {
        let module_id = self.module_id;
        match Command::new("pactl")
            .args(["unload-module", &module_id.to_string()])
            .output()
        {
            Ok(out) if out.status.success() => {
                info!("Unloaded null-sink module (id={})", module_id);
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                warn!(
                    "pactl unload-module {} failed: {}",
                    module_id,
                    stderr.trim()
                );
            }
            Err(err) => {
                warn!("Failed to spawn pactl unload-module {}: {err}", module_id);
            }
        }
    }
}

#[cfg(test)]
impl Drop for NullSinkModule {
    fn drop(&mut self) {}
}

fn build_load_args() -> Vec<String> {
    vec![
        format!("media.class=Audio/Source/Virtual"),
        format!("sink_name={}", VIRTUAL_SOURCE_NAME),
        // Description is single-word (`Linux_Soundboard_Mic`) by design:
        // pipewire-pulse's `pactl load-module` strips whitespace from
        // sink_properties values regardless of quoting. See
        // `app_meta::VIRTUAL_MIC_DESCRIPTION` for the rationale.
        format!(
            "sink_properties=device.description={}",
            VIRTUAL_MIC_DESCRIPTION
        ),
        format!("channel_map=front-left,front-right"),
    ]
}

pub(super) fn ensure_virtual_mic_present(state: &mut LoopState) {
    if !matches!(state.backend.as_ref(), Some(BackendState::PipeWire(_))) {
        return;
    }

    if virtual_mic_visible_in_registry(state) {
        state.virtual_mic_missing_since = None;
        return;
    }

    let now = Instant::now();
    let missing_since = *state.virtual_mic_missing_since.get_or_insert(now);
    if now.duration_since(missing_since) < VIRTUAL_MIC_REPAIR_DELAY {
        return;
    }
    if state
        .last_virtual_mic_repair_attempt
        .is_some_and(|last| now.duration_since(last) < VIRTUAL_MIC_REPAIR_COOLDOWN)
    {
        return;
    }

    let module_loaded = state
        .backend
        .as_ref()
        .and_then(|backend| match backend {
            BackendState::PipeWire(backend) => backend._virtual_mic_module.as_ref(),
            BackendState::PulseAudio(_) => None,
        })
        .is_some_and(NullSinkModule::is_loaded);
    let source_visible_to_pulse = virtual_source_exists();
    if module_loaded && source_visible_to_pulse {
        return;
    }

    state.last_virtual_mic_repair_attempt = Some(now);
    reset_virtual_mic_graph_state(state);

    if let Some(BackendState::PipeWire(backend)) = state.backend.as_mut() {
        drop(backend._virtual_mic_module.take());
    }

    match NullSinkModule::load_or_attach() {
        Ok(module) => {
            info!(
                "Reloaded missing null-sink module for {}",
                VIRTUAL_SOURCE_NAME
            );
            if let Some(BackendState::PipeWire(backend)) = state.backend.as_mut() {
                backend._virtual_mic_module = Some(module);
            }
            state.claimed_default = false;
            claim_default_source_if_enabled(state);
        }
        Err(err) => {
            warn!(
                "Failed to reload null-sink module for {}: {}",
                VIRTUAL_SOURCE_NAME, err
            );
        }
    }
}

fn virtual_mic_visible_in_registry(state: &LoopState) -> bool {
    state.virtual_mic_node_id.is_some()
        || state
            .sources
            .values()
            .any(|source| source.is_our_virtual_mic)
}

fn reset_virtual_mic_graph_state(state: &mut LoopState) {
    state.sources.retain(|_, source| !source.is_our_virtual_mic);
    state.virtual_mic_node_id = None;
    state.virtual_mic_input_ports.clear();
    state.virtual_mic_state_reset_ids.clear();
    drop_feeder_links(state);
}

#[cfg(not(test))]
fn virtual_source_exists() -> bool {
    let output = match Command::new("pactl")
        .args(["list", "short", "sources"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return false,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    source_list_contains_virtual_mic(&stdout)
}

#[cfg(test)]
fn virtual_source_exists() -> bool {
    true
}

/// Find every module-null-sink instance currently loaded for our virtual mic
/// name. Used to clean up duplicates from previous engine runs that crashed
/// before Drop could unload them.
#[cfg(not(test))]
fn find_all_existing_module_ids() -> Vec<u32> {
    let output = match Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_existing_module_ids(&stdout)
}

fn parse_existing_module_ids(stdout: &str) -> Vec<u32> {
    let mut found = Vec::new();
    for line in stdout.lines() {
        // Format: "<id>\tmodule-null-sink\t<args>"
        let mut cols = line.split('\t');
        let Some(id) = cols.next() else { continue };
        let Some(name) = cols.next() else { continue };
        if name != "module-null-sink" {
            continue;
        }
        let args = cols.next().unwrap_or("");
        if !args.contains(&format!("sink_name={VIRTUAL_SOURCE_NAME}")) {
            continue;
        }
        if let Ok(id) = id.parse::<u32>() {
            found.push(id);
        }
    }
    found
}

fn source_list_contains_virtual_mic(stdout: &str) -> bool {
    stdout.lines().any(|line| {
        let mut cols = line.split('\t');
        let _id = cols.next();
        let name = cols.next();
        name == Some(VIRTUAL_SOURCE_NAME)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_load_args_includes_required_properties() {
        let args = build_load_args();
        let joined = args.join(" ");
        assert!(joined.contains("media.class=Audio/Source/Virtual"));
        assert!(joined.contains(&format!("sink_name={VIRTUAL_SOURCE_NAME}")));
        // Description must be space-free so pactl doesn't truncate at the
        // first whitespace. See app_meta::VIRTUAL_MIC_DESCRIPTION.
        assert!(joined.contains(VIRTUAL_MIC_DESCRIPTION));
        assert!(!VIRTUAL_MIC_DESCRIPTION.contains(' '));
    }

    #[test]
    fn parse_existing_module_ids_finds_only_our_null_sink() {
        let stdout = format!(
            "10\tmodule-null-sink\tmedia.class=Audio/Sink sink_name=other\n\
             11\tmodule-null-sink\tmedia.class=Audio/Source/Virtual sink_name={VIRTUAL_SOURCE_NAME}\n\
             12\tmodule-loopback\tsource={VIRTUAL_SOURCE_NAME}\n\
             invalid\tmodule-null-sink\tsink_name={VIRTUAL_SOURCE_NAME}\n"
        );

        assert_eq!(parse_existing_module_ids(&stdout), vec![11]);
    }

    #[test]
    fn source_list_contains_virtual_mic_matches_exact_source_name() {
        let stdout = format!(
            "74\talsa_input.pci-0000_12_00.6.analog-stereo\tPipeWire\ts32le 2ch 48000Hz\tRUNNING\n\
             91\t{VIRTUAL_SOURCE_NAME}.monitor\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n\
             92\t{VIRTUAL_SOURCE_NAME}\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n"
        );

        assert!(source_list_contains_virtual_mic(&stdout));
    }

    #[test]
    fn source_list_contains_virtual_mic_rejects_monitor_only() {
        let stdout =
            format!("91\t{VIRTUAL_SOURCE_NAME}.monitor\tPipeWire\tfloat32le 2ch 48000Hz\tIDLE\n");

        assert!(!source_list_contains_virtual_mic(&stdout));
    }
}
