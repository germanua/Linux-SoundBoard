//! Explicit graph links for the virtual-mic feeder.
//!
//! AUTOCONNECT can't link our `Stream/Output/Audio` feeder to an
//! `Audio/Source/Virtual` null-sink: WirePlumber sees a Playback stream aimed
//! at a Source class and silently drops it, leaving the feeder dangling and the
//! sink dry. So we wire it ourselves through link-factory, the same API
//! `pw-link` uses, and WirePlumber leaves existing links alone.
//!
//! Track four ids (feeder node + its two output ports, mic node + its two
//! input ports), create FL→FL and FR→FR once all four are up, and drop the
//! proxies when any of them goes (PipeWire restart, null-sink reload).

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
    /// Parse PipeWire's `audio.channel` port property. `FL`/`FR` are the short
    /// form it uses in port names and in `audio.channel`.
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

/// Idempotent reconciler. Registry callbacks run it whenever a Port or Node
/// global comes or goes, and when the feeder/null-sink state changes. Cheap
/// when there is nothing to do.
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
        // We own this link, so it must not outlive the proxy. Without the flag
        // WirePlumber can read it as a persistent user route and keep bringing
        // it back.
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
