use super::*;
use pw::properties::properties;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum AudioChannel {
    FrontLeft,
    FrontRight,
}

impl AudioChannel {
    fn as_str(self) -> &'static str {
        match self {
            Self::FrontLeft => "FL",
            Self::FrontRight => "FR",
        }
    }

    fn all() -> [Self; 2] {
        [Self::FrontLeft, Self::FrontRight]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PortDirection {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TrackedPort {
    pub(super) id: u32,
    pub(super) node_id: u32,
    pub(super) direction: PortDirection,
    pub(super) channel: AudioChannel,
}

pub(super) fn parse_audio_channel(value: &str) -> Option<AudioChannel> {
    match value {
        "FL" | "front-left" | "Front Left" => Some(AudioChannel::FrontLeft),
        "FR" | "front-right" | "Front Right" => Some(AudioChannel::FrontRight),
        _ => None,
    }
}

pub(super) fn track_node_global(
    state: &mut LoopState,
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> bool {
    if global.type_ != pw::types::ObjectType::Node {
        return false;
    }
    let Some(props) = global.props else {
        return false;
    };
    let Some(node_name) = props.get(*pw::keys::NODE_NAME) else {
        return false;
    };
    match node_name {
        VIRTUAL_SOURCE_NAME => {
            state.virtual_mic_node_id = Some(global.id);
            refresh_tracked_link_ports(state);
            try_link_feeder_to_virtual_mic(state);
            true
        }
        VIRTUAL_FEEDER_NODE_NAME => {
            state.feeder_node_id = Some(global.id);
            refresh_tracked_link_ports(state);
            try_link_feeder_to_virtual_mic(state);
            true
        }
        _ => false,
    }
}

pub(super) fn track_port_global(
    state: &mut LoopState,
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> bool {
    let Some(port) = port_from_global(global) else {
        return false;
    };
    state.tracked_ports.insert(port.id, port);
    refresh_tracked_link_ports(state);
    try_link_feeder_to_virtual_mic(state);
    true
}

pub(super) fn handle_link_global_remove(state: &mut LoopState, id: u32) -> bool {
    let mut changed = false;
    if state.virtual_mic_node_id == Some(id) {
        state.virtual_mic_node_id = None;
        state.virtual_mic_input_ports.clear();
        drop_feeder_links(state);
        changed = true;
    }
    if state.feeder_node_id == Some(id) {
        state.feeder_node_id = None;
        state.feeder_output_ports.clear();
        drop_feeder_links(state);
        changed = true;
    }
    if state.tracked_ports.remove(&id).is_some() {
        refresh_tracked_link_ports(state);
        drop_feeder_links(state);
        try_link_feeder_to_virtual_mic(state);
        changed = true;
    }
    changed
}

fn port_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<TrackedPort> {
    if global.type_ != pw::types::ObjectType::Port {
        return None;
    }
    let props = global.props?;
    let node_id = props.get("node.id")?.parse::<u32>().ok()?;
    let direction = match props.get("port.direction")? {
        "in" | "input" => PortDirection::Input,
        "out" | "output" => PortDirection::Output,
        _ => return None,
    };
    let channel = props.get("audio.channel").and_then(parse_audio_channel)?;
    Some(TrackedPort {
        id: global.id,
        node_id,
        direction,
        channel,
    })
}

fn refresh_tracked_link_ports(state: &mut LoopState) {
    state.virtual_mic_input_ports.clear();
    state.feeder_output_ports.clear();

    for port in state.tracked_ports.values().copied() {
        if Some(port.node_id) == state.virtual_mic_node_id && port.direction == PortDirection::Input
        {
            state.virtual_mic_input_ports.insert(port.channel, port.id);
        } else if Some(port.node_id) == state.feeder_node_id
            && port.direction == PortDirection::Output
        {
            state.feeder_output_ports.insert(port.channel, port.id);
        }
    }
}

pub(super) fn try_link_feeder_to_virtual_mic(state: &mut LoopState) {
    let Some(core) = state.backend.as_ref().and_then(BackendState::pipewire_core) else {
        return;
    };
    let Some(feeder_node_id) = state.feeder_node_id else {
        return;
    };
    let Some(virtual_mic_node_id) = state.virtual_mic_node_id else {
        return;
    };

    for channel in AudioChannel::all() {
        if state.feeder_links.contains_key(&channel) {
            continue;
        }
        let Some(output_port_id) = state.feeder_output_ports.get(&channel).copied() else {
            continue;
        };
        let Some(input_port_id) = state.virtual_mic_input_ports.get(&channel).copied() else {
            continue;
        };
        if state.links.values().any(|link| {
            link.output_node_id == feeder_node_id
                && link.input_node_id == virtual_mic_node_id
                && link.output_port_id == Some(output_port_id)
                && link.input_port_id == Some(input_port_id)
        }) {
            state.available = state.backend_playback_available();
            continue;
        }

        let feeder_node = feeder_node_id.to_string();
        let virtual_node = virtual_mic_node_id.to_string();
        let output_port = output_port_id.to_string();
        let input_port = input_port_id.to_string();
        match core.create_object::<pw::link::Link>(
            "link-factory",
            &properties! {
                "link.output.node" => feeder_node.as_str(),
                "link.output.port" => output_port.as_str(),
                "link.input.node" => virtual_node.as_str(),
                "link.input.port" => input_port.as_str(),
                "object.linger" => "false",
            },
        ) {
            Ok(link) => {
                debug!(
                    "Linked virtual mic feeder channel {}: {}:{} -> {}:{}",
                    channel.as_str(),
                    feeder_node_id,
                    output_port_id,
                    virtual_mic_node_id,
                    input_port_id
                );
                state.feeder_links.insert(channel, link);
                state.available = true;
            }
            Err(err) => {
                warn!(
                    "Failed to create virtual mic feeder link for channel {}: {err}",
                    channel.as_str()
                );
            }
        }
    }
}

pub(super) fn drop_feeder_links(state: &mut LoopState) {
    if !state.feeder_links.is_empty() {
        debug!("Dropping virtual mic feeder links");
    }
    state.feeder_links.clear();
    if state.runtime.persistent_virtual_mic {
        state.available = state.backend_playback_available();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pipewire_audio_channels() {
        assert_eq!(parse_audio_channel("FL"), Some(AudioChannel::FrontLeft));
        assert_eq!(parse_audio_channel("FR"), Some(AudioChannel::FrontRight));
        assert_eq!(parse_audio_channel("FC"), None);
    }
}
