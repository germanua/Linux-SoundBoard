//! Explicit graph-link manager for the virtual-mic feeder.
//!
//! Background: our feeder is a `Stream/Output/Audio` node. The virtual mic
//! is a `module-null-sink` with `media.class = Audio/Source/Virtual`.
//! WirePlumber's session-manager policy refuses to link those two via
//! AUTOCONNECT — it sees a Playback stream targeting a Source class and
//! drops the routing decision silently. The plain
//! `pw::stream::StreamFlags::AUTOCONNECT` path therefore leaves the feeder
//! dangling and the null-sink dry.
//!
//! Fix: wire the graph ourselves with `core.create_object::<Link>("link-factory", …)`.
//! That call is the API behind `pw-link`'s CLI — same mechanism, no policy
//! interference. WirePlumber sees the links exist and lets them be.
//!
//! Lifecycle:
//! - Track the feeder node id + its `output_FL` / `output_FR` port ids.
//! - Track the virtual-mic node id + its `input_FL` / `input_FR` port ids.
//! - When all four are visible, create two Links (FL→FL, FR→FR).
//! - On any of the four going away (PipeWire restart, null-sink reload),
//!   drop the held Link proxies and recreate when ports reappear.

use log::{info, warn};
use pipewire as pw;
use pw::properties::properties;

use super::EngineError;
use super::LoopState;

/// Audio channel discriminator. We only handle stereo FL/FR for now — same
/// channel layout as EasyEffects and pavucontrol's virtual sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum AudioChannel {
    FrontLeft,
    FrontRight,
}

impl AudioChannel {
    /// Parse PipeWire's `audio.channel` port property. PipeWire spells front
    /// channels with hyphenated SPA names — `FL`, `FR` are the short form
    /// (used in port names and `audio.channel`).
    pub(super) fn from_prop(value: &str) -> Option<Self> {
        match value.trim() {
            "FL" => Some(Self::FrontLeft),
            "FR" => Some(Self::FrontRight),
            _ => None,
        }
    }
}

/// Owned `Link` proxy. Dropping it destroys the link on the PipeWire daemon.
pub(super) struct FeederLink {
    _link: pw::link::Link,
}

/// Idempotent reconciler. Called from registry callbacks any time a Port or
/// Node global appears or disappears, and when the feeder/null-sink state
/// changes. Cheap when there's nothing to do.
pub(super) fn try_link_feeder_to_virtual_mic(state: &mut LoopState) {
    let core_opt = state.backend.as_ref().and_then(|b| b.pipewire_core());
    let Some(core) = core_opt else {
        return;
    };
    let feeder_node = state.feeder_node_id;
    let virtual_node = state.virtual_mic_node_id;
    let (Some(feeder_node), Some(virtual_node)) = (feeder_node, virtual_node) else {
        return;
    };

    for &channel in &[AudioChannel::FrontLeft, AudioChannel::FrontRight] {
        if state.feeder_links.contains_key(&channel) {
            continue;
        }
        let Some(&out_port) = state.feeder_output_ports.get(&channel) else {
            continue;
        };
        let Some(&in_port) = state.virtual_mic_input_ports.get(&channel) else {
            continue;
        };

        match create_link(&core, feeder_node, out_port, virtual_node, in_port) {
            Ok(link) => {
                info!(
                    "Linked feeder→virtual_mic on {:?} (feeder.node={} feeder.port={} → vmic.node={} vmic.port={})",
                    channel, feeder_node, out_port, virtual_node, in_port
                );
                state
                    .feeder_links
                    .insert(channel, FeederLink { _link: link });
            }
            Err(err) => {
                warn!(
                    "Failed to create feeder→virtual_mic link on {:?}: {err}",
                    channel
                );
            }
        }
    }
}

/// Drop ALL active links. Called when either side disappears so we don't keep
/// dangling proxies that would refuse to relink on reappearance.
pub(super) fn drop_feeder_links(state: &mut LoopState) {
    if state.feeder_links.is_empty() {
        return;
    }
    state.feeder_links.clear();
    info!("Dropped feeder→virtual_mic links; will rebuild when both sides return");
}

fn create_link(
    core: &pw::core::CoreRc,
    output_node: u32,
    output_port: u32,
    input_node: u32,
    input_port: u32,
) -> Result<pw::link::Link, EngineError> {
    let props = properties! {
        "link.output.node" => output_node.to_string(),
        "link.output.port" => output_port.to_string(),
        "link.input.node" => input_node.to_string(),
        "link.input.port" => input_port.to_string(),
        // We own the link explicitly; it must NOT linger after we drop the
        // proxy. Without this WirePlumber's session-manager may treat the
        // link as a persistent user-configured route and keep recreating it.
        "object.linger" => "false",
    };
    core.create_object::<pw::link::Link>("link-factory", &props)
        .map_err(|e| EngineError::Routing(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_channel_parses_pipewire_short_names() {
        assert_eq!(AudioChannel::from_prop("FL"), Some(AudioChannel::FrontLeft));
        assert_eq!(
            AudioChannel::from_prop("FR"),
            Some(AudioChannel::FrontRight)
        );
        assert_eq!(
            AudioChannel::from_prop("  FL  "),
            Some(AudioChannel::FrontLeft)
        );
        assert_eq!(AudioChannel::from_prop("UNKNOWN"), None);
        assert_eq!(AudioChannel::from_prop("MONO"), None);
    }
}
