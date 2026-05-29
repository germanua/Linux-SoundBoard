//! PipeWire registry event parsers and graph-state helpers.
//!
//! These functions are called from the `global` and `global_remove` callbacks
//! registered in `create_pipewire_backend`. Each parser takes a raw
//! `GlobalObject` and returns a typed value (or `None`); the caller then
//! decides which `LoopState` maps to update.

use super::*;

/// Parse a PipeWire `Audio/Source` or `Audio/Source/Virtual` node into a
/// `SourceDescriptor`, or return `None` if the global is not a source.
pub(super) fn source_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<SourceDescriptor> {
    if global.type_ != pw::types::ObjectType::Node {
        return None;
    }

    let props = global.props?;
    let media_class = props.get(*pw::keys::MEDIA_CLASS)?;
    // Modern PipeWire-native virtual sources (EasyEffects, NoiseTorch, our own
    // Linux Soundboard Mic) advertise Audio/Source/Virtual; physical
    // mics use plain Audio/Source. Accept both so users can route through
    // EasyEffects to get processed mic + soundboard mixed into one feed.
    if media_class != "Audio/Source" && media_class != "Audio/Source/Virtual" {
        return None;
    }

    let node_name = props.get(*pw::keys::NODE_NAME)?.to_string();
    let display_name = props
        .get(*pw::keys::NODE_DESCRIPTION)
        .or_else(|| props.get("device.description"))
        .unwrap_or(node_name.as_str())
        .to_string();
    let priority_session = props
        .get("priority.session")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(0);
    let serial = props
        .get("object.serial")
        .and_then(|value| value.parse::<u64>().ok());
    let is_virtual = media_class == "Audio/Source/Virtual";
    // Explicit allow-list of hardware APIs rather than "any device.api present" —
    // guards against exotic software nodes that might set device.api to a custom value.
    let is_hardware_backed = matches!(
        props.get(*pw::keys::DEVICE_API),
        Some("alsa") | Some("bluez5") | Some("v4l2") | Some("oss")
    );

    Some(SourceDescriptor {
        id: global.id,
        serial,
        is_monitor: node_name.ends_with(".monitor"),
        is_our_virtual_mic: node_name == VIRTUAL_SOURCE_NAME,
        is_virtual,
        is_hardware_backed,
        priority_session,
        node_name,
        display_name,
    })
}

/// Recognise our feeder Node or the virtual-mic Node by name. Returns
/// `(role, id)` where role is "feeder" or "vmic" — caller stores the id
/// in the right `LoopState` slot.
pub(super) fn explicit_link_node_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<(ExplicitLinkRole, u32)> {
    if global.type_ != pw::types::ObjectType::Node {
        return None;
    }
    let name = global.props?.get(*pw::keys::NODE_NAME)?;
    let role = if name == crate::app_meta::VIRTUAL_MIC_FEEDER_NODE_NAME {
        ExplicitLinkRole::Feeder
    } else if name == VIRTUAL_SOURCE_NAME {
        ExplicitLinkRole::VirtualMic
    } else {
        return None;
    };
    Some((role, global.id))
}

/// Recognise a Port belonging to a tracked Node (feeder or virtual mic).
/// Returns `(node_id, channel, direction, port_id)` so the caller can route
/// it into the right `LoopState` map.
pub(super) fn explicit_link_port_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<(u32, AudioChannel, PortDirection, u32)> {
    if global.type_ != pw::types::ObjectType::Port {
        return None;
    }
    let props = global.props?;
    let node_id = props.get("node.id")?.parse::<u32>().ok()?;
    let channel = AudioChannel::from_prop(props.get("audio.channel")?)?;
    let direction = match props.get("port.direction")? {
        "in" => PortDirection::In,
        "out" => PortDirection::Out,
        _ => return None,
    };
    Some((node_id, channel, direction, global.id))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExplicitLinkRole {
    Feeder,
    VirtualMic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PortDirection {
    In,
    Out,
}

pub(super) fn sink_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<SinkDescriptor> {
    if global.type_ != pw::types::ObjectType::Node {
        return None;
    }
    let props = global.props?;
    let media_class = props.get(*pw::keys::MEDIA_CLASS)?;
    if media_class != "Audio/Sink" && media_class != "Audio/Sink/Virtual" {
        return None;
    }
    let node_name = props.get(*pw::keys::NODE_NAME)?.to_string();
    let serial = props
        .get("object.serial")
        .and_then(|value| value.parse::<u64>().ok());
    Some(SinkDescriptor {
        id: global.id,
        serial,
        node_name,
        monitor_source_node_name: None,
    })
}

// PipeWire sinks expose their monitor as a separate Audio/Source node named
// "<sink_name>.monitor". The two registry events can arrive in either order, so
// both code paths back-fill the cross-link when the second half lands.
pub(super) fn link_sink_to_known_monitor_source(state: &mut LoopState, sink_id: u32) {
    let monitor_name = match state.sinks.get(&sink_id) {
        Some(sink) => format!("{}.monitor", sink.node_name),
        None => return,
    };
    let exists = state
        .sources
        .values()
        .any(|src| src.is_monitor && src.node_name == monitor_name);
    if exists {
        if let Some(sink) = state.sinks.get_mut(&sink_id) {
            sink.monitor_source_node_name = Some(monitor_name);
        }
    }
}

pub(super) fn link_monitor_source_to_known_sink(state: &mut LoopState, source_id: u32) {
    let Some(source) = state.sources.get(&source_id) else {
        return;
    };
    if !source.is_monitor {
        return;
    }
    let Some(sink_name) = source.node_name.strip_suffix(".monitor") else {
        return;
    };
    let sink_name = sink_name.to_string();
    let monitor_name = source.node_name.clone();
    if let Some(sink) = state.sinks.values_mut().find(|s| s.node_name == sink_name) {
        sink.monitor_source_node_name = Some(monitor_name);
    }
}

pub(super) fn spawn_virtual_mic_state_reset(source_id: u32) {
    let _ = thread::Builder::new()
        .name("lsb-virtual-mic-state-reset".to_string())
        .spawn(move || {
            let source_id = source_id.to_string();
            let volume = Command::new("wpctl")
                .args(["set-volume", &source_id, "1.0"])
                .status();
            if !matches!(volume, Ok(status) if status.success()) {
                warn!("Failed to reset Linux Soundboard virtual mic volume with wpctl");
            }
            let mute = Command::new("wpctl")
                .args(["set-mute", &source_id, "0"])
                .status();
            if !matches!(mute, Ok(status) if status.success()) {
                warn!("Failed to unmute Linux Soundboard virtual mic with wpctl");
            }
        });
}

pub(super) fn capture_node_id_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<u32> {
    if global.type_ != pw::types::ObjectType::Node {
        return None;
    }
    let props = global.props?;
    (props.get(*pw::keys::NODE_NAME)? == MIC_CAPTURE_NODE_NAME).then_some(global.id)
}

pub(super) fn link_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<LinkDescriptor> {
    if global.type_ != pw::types::ObjectType::Link {
        return None;
    }

    let props = global.props?;
    Some(LinkDescriptor {
        id: global.id,
        output_node_id: props.get("link.output.node")?.parse().ok()?,
        input_node_id: props.get("link.input.node")?.parse().ok()?,
        output_port_id: props
            .get("link.output.port")
            .and_then(|value| value.parse().ok()),
        input_port_id: props
            .get("link.input.port")
            .and_then(|value| value.parse().ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DefaultSourceMode;

    fn make_state() -> LoopState {
        LoopState::new(
            super::super::test_runtime_config_with_mode(DefaultSourceMode::Manual),
            super::super::test_player_snapshot_store(),
        )
    }

    fn make_sink(id: u32, node_name: &str) -> SinkDescriptor {
        SinkDescriptor {
            id,
            serial: None,
            node_name: node_name.to_string(),
            monitor_source_node_name: None,
        }
    }

    fn make_source(id: u32, node_name: &str, is_monitor: bool) -> SourceDescriptor {
        SourceDescriptor {
            id,
            serial: None,
            node_name: node_name.to_string(),
            display_name: node_name.to_string(),
            priority_session: 0,
            is_monitor,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: !is_monitor,
        }
    }

    #[test]
    fn link_sink_backfills_monitor_when_source_present() {
        let mut state = make_state();
        state.sinks.insert(1, make_sink(1, "alsa_output.speaker"));
        state
            .sources
            .insert(2, make_source(2, "alsa_output.speaker.monitor", true));
        link_sink_to_known_monitor_source(&mut state, 1);
        assert_eq!(
            state.sinks[&1].monitor_source_node_name.as_deref(),
            Some("alsa_output.speaker.monitor")
        );
    }

    #[test]
    fn link_sink_no_op_when_monitor_absent() {
        let mut state = make_state();
        state.sinks.insert(1, make_sink(1, "alsa_output.speaker"));
        link_sink_to_known_monitor_source(&mut state, 1);
        assert!(state.sinks[&1].monitor_source_node_name.is_none());
    }

    #[test]
    fn link_monitor_source_backfills_sink() {
        let mut state = make_state();
        state.sinks.insert(1, make_sink(1, "alsa_output.speaker"));
        state
            .sources
            .insert(2, make_source(2, "alsa_output.speaker.monitor", true));
        link_monitor_source_to_known_sink(&mut state, 2);
        assert_eq!(
            state.sinks[&1].monitor_source_node_name.as_deref(),
            Some("alsa_output.speaker.monitor")
        );
    }

    #[test]
    fn link_monitor_source_ignores_non_monitor() {
        let mut state = make_state();
        state.sinks.insert(1, make_sink(1, "alsa_output.speaker"));
        state
            .sources
            .insert(2, make_source(2, "alsa_input.mic", false));
        link_monitor_source_to_known_sink(&mut state, 2);
        assert!(state.sinks[&1].monitor_source_node_name.is_none());
    }

    #[test]
    fn link_monitor_source_no_op_when_sink_absent() {
        let mut state = make_state();
        state
            .sources
            .insert(1, make_source(1, "alsa_output.hdmi.monitor", true));
        link_monitor_source_to_known_sink(&mut state, 1);
    }
}
