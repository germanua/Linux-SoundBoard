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

use std::process::Command;

use log::{info, warn};

use super::EngineError;
use crate::app_meta::{VIRTUAL_MIC_DESCRIPTION, VIRTUAL_SOURCE_NAME};

/// Owning handle for a `pactl`-loaded `module-null-sink` instance.
/// Dropping this calls `pactl unload-module <id>`.
pub(super) struct NullSinkModule {
    pub(super) module_id: u32,
}

impl NullSinkModule {
    /// Load a fresh null-sink. Before loading, eagerly clean up any stale
    /// instances from prior engine crashes — keeping the graph free of
    /// duplicate null-sinks under the same `sink_name`.
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
}

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

/// Find every module-null-sink instance currently loaded for our virtual mic
/// name. Used to clean up duplicates from previous engine runs that crashed
/// before Drop could unload them.
fn find_all_existing_module_ids() -> Vec<u32> {
    let output = match Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return Vec::new(),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
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
}
