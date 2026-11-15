//! Cleanup of null sinks left behind by older versions.
//!
//! The virtual mic is a PipeWire stream (see `streams.rs`). Older versions created it
//! as a `pactl` null sink instead. One that survived a crashed engine would still own
//! the `linuxsoundboard.virtual_mic` node name, so it is removed at startup.

#[cfg(not(test))]
use std::process::Command;

#[cfg(not(test))]
use log::{info, warn};

use crate::app_meta::VIRTUAL_SOURCE_NAME;

/// Unloads every null sink that uses our virtual-mic name.
#[cfg(not(test))]
pub(super) fn remove_stale_null_sinks() {
    let output = match Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => return,
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stale = parse_existing_module_ids(&stdout);
    if stale.is_empty() {
        return;
    }

    info!(
        "Unloading {} stale null-sink instance(s) left by an older version",
        stale.len()
    );
    for id in stale {
        match Command::new("pactl")
            .args(["unload-module", &id.to_string()])
            .output()
        {
            Ok(out) if out.status.success() => {}
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                warn!("pactl unload-module {} failed: {}", id, stderr.trim());
            }
            Err(err) => warn!("Failed to spawn pactl unload-module {}: {err}", id),
        }
    }
}

#[cfg(test)]
pub(super) fn remove_stale_null_sinks() {}

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
