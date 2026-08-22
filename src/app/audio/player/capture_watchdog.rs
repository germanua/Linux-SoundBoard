//! Capture-stream watchdog: detects missing or unhealthy mic-passthrough
//! streams and re-establishes them without user intervention.

use super::*;
use source_routing::{recreate_capture_stream, resolve_capture_target, resolve_source_id_by_name};

pub(super) fn capture_stream_missing(state: &LoopState) -> bool {
    match state.backend.as_ref() {
        Some(BackendState::PipeWire(backend)) => backend.capture_stream.is_none(),
        Some(BackendState::PulseAudio(backend)) => !backend.capture_stream_active(),
        None => false,
    }
}

/// Watchdog for the mic-passthrough capture stream. Covers two races we hit
/// for real:
///   1. We start before the registry lists any physical mic, so the first
///      `recreate_capture_stream` finds nothing. Users used to have to toggle
///      passthrough off and back on to recover.
///   2. The preferred source (EasyEffects and friends) comes up after us, and
///      now gets wired in when it appears.
pub(super) fn ensure_capture_stream_present(state: &mut LoopState) {
    if !state.runtime.mic_passthrough {
        state.capture_health_miss_ticks = 0;
        return;
    }

    if matches!(state.backend, Some(BackendState::PulseAudio(_))) {
        if capture_stream_missing(state) {
            if let Err(err) = recreate_capture_stream(state) {
                warn!("Capture-stream watchdog failed to (re)create: {err}");
            }
        }
        return;
    }

    let expected_target =
        resolve_capture_target(state).or_else(|| active_capture_target_if_available(state));
    if capture_stream_missing(state) {
        if expected_target.is_some() {
            if let Err(err) = recreate_capture_stream(state) {
                warn!("Capture-stream watchdog failed to (re)create: {err}");
            }
        }
        return;
    }

    if pipewire_capture_stream_healthy(state, expected_target.as_deref()) {
        state.capture_health_miss_ticks = 0;
        return;
    }

    if state.active_capture_target.as_deref() != expected_target.as_deref()
        || pipewire_capture_stream_failed(state)
    {
        state.capture_health_miss_ticks = state.capture_health_miss_ticks.saturating_add(1);
        if state.capture_health_miss_ticks < CAPTURE_RECREATE_MISS_THRESHOLD {
            return;
        }

        if let Err(err) = recreate_capture_stream(state) {
            warn!("Capture-stream watchdog failed to repair unhealthy stream: {err}");
        }
    }

    // Link health lies while WirePlumber rewires nodes. Keep the stream and
    // just drop its passthrough contribution until a good link shows up, rather
    // than tearing down and flushing the playback queues.
}

pub(super) fn pipewire_capture_stream_healthy(
    state: &LoopState,
    expected_target: Option<&str>,
) -> bool {
    let Some(expected_target) = expected_target else {
        return false;
    };
    if state.active_capture_target.as_deref() != Some(expected_target) {
        return false;
    }
    pipewire_capture_stream_linked_to_active_target(state)
}

pub(super) fn active_capture_target_if_available(state: &LoopState) -> Option<String> {
    let target = state.active_capture_target.as_deref()?;
    resolve_source_id_by_name(&state.sources, target).map(|_| target.to_string())
}

pub(super) fn pipewire_capture_stream_linked_to_active_target(state: &LoopState) -> bool {
    let Some(BackendState::PipeWire(backend)) = state.backend.as_ref() else {
        return false;
    };
    let Some(capture_stream) = backend.capture_stream.as_ref() else {
        return false;
    };
    pipewire_capture_link_healthy(
        state.active_capture_target.as_deref(),
        state
            .capture_node_id
            .or_else(|| Some(capture_stream.node_id())),
        capture_stream.current_state(),
        &state.sources,
        &state.links,
    )
}

pub(super) fn pipewire_capture_stream_failed(state: &LoopState) -> bool {
    let Some(BackendState::PipeWire(backend)) = state.backend.as_ref() else {
        return false;
    };
    let Some(capture_stream) = backend.capture_stream.as_ref() else {
        return false;
    };
    matches!(
        capture_stream.current_state(),
        ManagedStreamState::Error | ManagedStreamState::Unconnected
    )
}

pub(super) fn pipewire_capture_link_healthy(
    active_target: Option<&str>,
    capture_node_id: Option<u32>,
    capture_state: ManagedStreamState,
    sources: &HashMap<u32, SourceDescriptor>,
    links: &HashMap<u32, LinkDescriptor>,
) -> bool {
    if matches!(
        capture_state,
        ManagedStreamState::Error | ManagedStreamState::Unconnected
    ) {
        return false;
    }
    let Some(target_name) = active_target else {
        return false;
    };
    let Some(capture_node_id) = capture_node_id else {
        return false;
    };
    let Some(target_source) = sources
        .values()
        .find(|source| source.node_name == target_name)
    else {
        return false;
    };
    if target_source.is_monitor || target_source.is_our_virtual_mic {
        return false;
    }

    links.values().any(|link| {
        link.output_node_id == target_source.id && link.input_node_id == capture_node_id
    })
}

impl LoopState {
    pub(super) fn capture_stream_active(&self) -> bool {
        match self.backend.as_ref() {
            Some(BackendState::PipeWire(_)) => {
                pipewire_capture_stream_linked_to_active_target(self)
            }
            Some(BackendState::PulseAudio(backend)) => backend.capture_stream_active(),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source(id: u32, node_name: &str) -> SourceDescriptor {
        SourceDescriptor {
            id,
            serial: None,
            node_name: node_name.to_string(),
            display_name: node_name.to_string(),
            priority_session: 100,
            is_monitor: node_name.ends_with(".monitor"),
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        }
    }

    fn make_link(id: u32, output_node: u32, input_node: u32) -> LinkDescriptor {
        LinkDescriptor {
            id,
            output_node_id: output_node,
            input_node_id: input_node,
            output_port_id: None,
            input_port_id: None,
        }
    }

    #[test]
    fn capture_link_healthy_returns_true_when_linked() {
        let source = make_source(10, "alsa_input.mic");
        let mut sources = HashMap::new();
        sources.insert(source.id, source);
        let mut links = HashMap::new();
        links.insert(1, make_link(1, 10, 20));
        assert!(pipewire_capture_link_healthy(
            Some("alsa_input.mic"),
            Some(20),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));
    }

    #[test]
    fn capture_link_healthy_returns_false_on_error_state() {
        let source = make_source(10, "alsa_input.mic");
        let mut sources = HashMap::new();
        sources.insert(source.id, source);
        let mut links = HashMap::new();
        links.insert(1, make_link(1, 10, 20));
        assert!(!pipewire_capture_link_healthy(
            Some("alsa_input.mic"),
            Some(20),
            ManagedStreamState::Error,
            &sources,
            &links,
        ));
    }

    #[test]
    fn capture_link_healthy_returns_false_on_unconnected() {
        let source = make_source(10, "alsa_input.mic");
        let mut sources = HashMap::new();
        sources.insert(source.id, source);
        let mut links = HashMap::new();
        links.insert(1, make_link(1, 10, 20));
        assert!(!pipewire_capture_link_healthy(
            Some("alsa_input.mic"),
            Some(20),
            ManagedStreamState::Unconnected,
            &sources,
            &links,
        ));
    }

    #[test]
    fn capture_link_healthy_returns_false_without_active_target() {
        let sources = HashMap::new();
        let links = HashMap::new();
        assert!(!pipewire_capture_link_healthy(
            None,
            Some(20),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));
    }

    #[test]
    fn capture_link_healthy_returns_false_without_capture_node() {
        let source = make_source(10, "alsa_input.mic");
        let mut sources = HashMap::new();
        sources.insert(source.id, source);
        let links = HashMap::new();
        assert!(!pipewire_capture_link_healthy(
            Some("alsa_input.mic"),
            None,
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));
    }

    #[test]
    fn capture_link_healthy_returns_false_when_source_missing() {
        let sources = HashMap::new();
        let links = HashMap::new();
        assert!(!pipewire_capture_link_healthy(
            Some("alsa_input.missing"),
            Some(20),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));
    }

    #[test]
    fn capture_link_healthy_returns_false_without_matching_link() {
        let source = make_source(10, "alsa_input.mic");
        let mut sources = HashMap::new();
        sources.insert(source.id, source);
        let links = HashMap::new();
        assert!(!pipewire_capture_link_healthy(
            Some("alsa_input.mic"),
            Some(20),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));
    }

    #[test]
    fn capture_link_healthy_rejects_monitor_source() {
        let source = make_source(10, "alsa_output.speaker.monitor");
        let mut sources = HashMap::new();
        sources.insert(source.id, source);
        let mut links = HashMap::new();
        links.insert(1, make_link(1, 10, 20));
        assert!(!pipewire_capture_link_healthy(
            Some("alsa_output.speaker.monitor"),
            Some(20),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));
    }

    #[test]
    fn capture_link_healthy_rejects_our_virtual_mic() {
        let mut source = make_source(10, "linux_soundboard_source");
        source.is_our_virtual_mic = true;
        let mut sources = HashMap::new();
        sources.insert(source.id, source);
        let mut links = HashMap::new();
        links.insert(1, make_link(1, 10, 20));
        assert!(!pipewire_capture_link_healthy(
            Some("linux_soundboard_source"),
            Some(20),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));
    }
}
