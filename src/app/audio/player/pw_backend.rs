//! PipeWire and PulseAudio backend types and initialization.
//!
//! Owns the lifecycle of the in-process audio backend:
//! - `BackendState` wraps either the PipeWire or PulseAudio backend.
//! - `PipeWireBackendState` holds the live PipeWire connection handles.
//! - `create_backend` / `create_pipewire_backend` build a backend from `RuntimeConfig`.
//! - `remote_ok` is a helper for sending fire-and-forget requests to the
//!   out-of-process engine and mapping its `Ok`/`Error` response to `EngineError`.

use super::*;

pub(super) fn remote_ok(
    request: crate::audio::engine_ipc::EngineRequest,
) -> Result<(), EngineError> {
    match crate::audio::engine_ipc::send_request(request)
        .map_err(|e| EngineError::Setup(e.to_string()))?
    {
        crate::audio::engine_ipc::EngineResponse::Ok => Ok(()),
        crate::audio::engine_ipc::EngineResponse::Error { message } => {
            Err(EngineError::Routing(message))
        }
        other => Err(EngineError::Routing(format!(
            "Unexpected engine response: {other:?}"
        ))),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ManagedStreamState {
    Error,
    Unconnected,
    Connecting,
    Paused,
    Streaming,
}

impl ManagedStreamState {
    pub(super) fn from_pipewire(state: pw::stream::StreamState) -> Self {
        match state {
            pw::stream::StreamState::Error(_) => Self::Error,
            pw::stream::StreamState::Unconnected => Self::Unconnected,
            pw::stream::StreamState::Connecting => Self::Connecting,
            pw::stream::StreamState::Paused => Self::Paused,
            pw::stream::StreamState::Streaming => Self::Streaming,
        }
    }
}

pub(super) struct StreamHandle {
    pub(super) _stream: pw::stream::StreamRc,
    pub(super) _listener: pw::stream::StreamListener<()>,
    pub(super) state: Rc<RefCell<ManagedStreamState>>,
}

impl StreamHandle {
    pub(super) fn new(
        stream: pw::stream::StreamRc,
        listener: pw::stream::StreamListener<()>,
        state: Rc<RefCell<ManagedStreamState>>,
    ) -> Self {
        Self {
            _stream: stream,
            _listener: listener,
            state,
        }
    }

    pub(super) fn current_state(&self) -> ManagedStreamState {
        *self.state.borrow()
    }

    pub(super) fn node_id(&self) -> u32 {
        self._stream.node_id()
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        if let Err(err) = self._stream.disconnect() {
            debug!("PipeWire stream disconnect during drop failed: {err}");
        }
    }
}

pub(super) struct PipeWireBackendState {
    pub(super) _context: pw::context::ContextRc,
    pub(super) core: pw::core::CoreRc,
    pub(super) _registry: pw::registry::RegistryRc,
    pub(super) _registry_listener: pw::registry::Listener,
    pub(super) _local_stream: Option<StreamHandle>,
    pub(super) virtual_stream: Option<StreamHandle>,
    pub(super) capture_stream: Option<StreamHandle>,
    // Real null-sink that backs `linuxsoundboard.virtual_mic`. Loaded at
    // engine start via pactl; unloaded on Drop. Wrapped in Option because
    // pactl might not be installed on minimal setups — we fail loud in that
    // case instead of silently falling back to a non-default-eligible
    // stream proxy.
    pub(super) _virtual_mic_module: Option<virtual_mic_module::NullSinkModule>,
}

pub(super) enum BackendState {
    PipeWire(PipeWireBackendState),
    PulseAudio(PulseAudioBackend),
}

impl BackendState {
    pub(super) fn pipewire_core(&self) -> Option<pw::core::CoreRc> {
        match self {
            Self::PipeWire(backend) => Some(backend.core.clone()),
            Self::PulseAudio(_) => None,
        }
    }

    pub(super) fn playback_stream_active(&self) -> bool {
        match self {
            Self::PipeWire(backend) => {
                backend._local_stream.is_some() || backend.virtual_stream.is_some()
            }
            Self::PulseAudio(backend) => {
                backend.local_stream_active() || backend.virtual_stream_active()
            }
        }
    }
}

pub(super) fn create_backend(
    weak_state: Weak<RefCell<LoopState>>,
    mainloop: pw::main_loop::MainLoopRc,
    queues: RtSharedQueues,
    runtime: RuntimeConfig,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
) -> Result<BackendState, EngineError> {
    match runtime.audio_backend {
        AudioBackendKind::PipeWire => {
            create_pipewire_backend(weak_state, mainloop, queues, runtime, stream_runtime)
                .map(BackendState::PipeWire)
        }
        AudioBackendKind::PulseAudio => {
            PulseAudioBackend::new(queues, stream_runtime, &runtime).map(BackendState::PulseAudio)
        }
    }
}

fn create_pipewire_backend(
    weak_state: Weak<RefCell<LoopState>>,
    mainloop: pw::main_loop::MainLoopRc,
    queues: RtSharedQueues,
    runtime: RuntimeConfig,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
) -> Result<PipeWireBackendState, EngineError> {
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| EngineError::Setup(e.to_string()))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| EngineError::Setup(e.to_string()))?;
    let registry = core
        .get_registry_rc()
        .map_err(|e| EngineError::Setup(e.to_string()))?;
    let registry_for_global = registry.clone();

    let registry_listener = registry
        .add_listener_local()
        .global({
            let weak_state = weak_state.clone();
            move |global| {
                let link = link_from_global(global);
                let capture_node_id = capture_node_id_from_global(global);
                let source = source_from_global(global);
                let sink = sink_from_global(global);
                let feeder_or_vmic_node = explicit_link_node_from_global(global);
                let feeder_or_vmic_port = explicit_link_port_from_global(global);
                let metadata = bind_default_metadata_from_global(
                    &registry_for_global,
                    global,
                    weak_state.clone(),
                );
                if source.is_none()
                    && sink.is_none()
                    && metadata.is_none()
                    && link.is_none()
                    && capture_node_id.is_none()
                    && feeder_or_vmic_node.is_none()
                    && feeder_or_vmic_port.is_none()
                {
                    return;
                }
                if let Some(state) = weak_state.upgrade() {
                    let mut state = state.borrow_mut();
                    if let Some(metadata) = metadata {
                        state.default_metadata = Some(metadata);
                        // The default-metadata listener may have already fired
                        // with the current default name during binding — re-
                        // assert our claim if the engine wants to be default.
                        claim_default_source_if_enabled(&mut state);
                    }
                    if let Some(capture_node_id) = capture_node_id {
                        state.capture_node_id = Some(capture_node_id);
                    }
                    if let Some(link) = link {
                        state.links.insert(link.id, link);
                    }
                    if let Some((role, node_id)) = feeder_or_vmic_node {
                        match role {
                            ExplicitLinkRole::Feeder => state.feeder_node_id = Some(node_id),
                            ExplicitLinkRole::VirtualMic => {
                                state.virtual_mic_node_id = Some(node_id)
                            }
                        }
                        try_link_feeder_to_virtual_mic(&mut state);
                    }
                    if let Some((node_id, channel, direction, port_id)) = feeder_or_vmic_port {
                        if Some(node_id) == state.feeder_node_id && direction == PortDirection::Out
                        {
                            state.feeder_output_ports.insert(channel, port_id);
                            try_link_feeder_to_virtual_mic(&mut state);
                        } else if Some(node_id) == state.virtual_mic_node_id
                            && direction == PortDirection::In
                        {
                            state.virtual_mic_input_ports.insert(channel, port_id);
                            try_link_feeder_to_virtual_mic(&mut state);
                        }
                    }
                    if let Some(source) = source {
                        if source.is_our_virtual_mic
                            && state.virtual_mic_state_reset_ids.insert(source.id)
                        {
                            spawn_virtual_mic_state_reset(source.id);
                        }
                        let source_id = source.id;
                        state.sources.insert(source_id, source);
                        link_monitor_source_to_known_sink(&mut state, source_id);
                        // virtual_mic just became visible — claim default now
                        // if mode requests it. Idempotent on subsequent calls.
                        claim_default_source_if_enabled(&mut state);
                        // A new source appeared — if mic passthrough is on but
                        // we haven't been able to attach yet (startup race), or
                        // the user's preferred source just came online, retry.
                        ensure_capture_stream_present(&mut state);
                    }
                    if let Some(sink) = sink {
                        let sink_id = sink.id;
                        state.sinks.insert(sink_id, sink);
                        link_sink_to_known_monitor_source(&mut state, sink_id);
                    }
                    state.publish_snapshot();
                }
            }
        })
        .global_remove({
            let weak_state = weak_state.clone();
            move |id| {
                if let Some(state) = weak_state.upgrade() {
                    let mut state = state.borrow_mut();
                    let removed_source_name = state
                        .sources
                        .get(&id)
                        .map(|source| source.node_name.clone());
                    let removed_source = state.sources.remove(&id).is_some();
                    state.sinks.remove(&id);
                    if let Some(name) = removed_source_name.as_deref() {
                        for sink in state.sinks.values_mut() {
                            if sink.monitor_source_node_name.as_deref() == Some(name) {
                                sink.monitor_source_node_name = None;
                            }
                        }
                    }
                    state.virtual_mic_state_reset_ids.remove(&id);
                    if state
                        .default_metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata.id == id)
                    {
                        state.default_metadata = None;
                    }
                    state.links.remove(&id);
                    // If the feeder or virtual_mic node disappears, drop any
                    // held link proxies so they get recreated when the node
                    // returns (e.g. on a PipeWire restart).
                    if state.feeder_node_id == Some(id) {
                        state.feeder_node_id = None;
                        state.feeder_output_ports.clear();
                        drop_feeder_links(&mut state);
                    }
                    if state.virtual_mic_node_id == Some(id) {
                        state.virtual_mic_node_id = None;
                        state.virtual_mic_input_ports.clear();
                        drop_feeder_links(&mut state);
                    }
                    // Port-level removal: if a removed id matches a known
                    // FL/FR port, drop the matching link half so it gets
                    // recreated when the port reappears.
                    let dropped_feeder_channel: Option<AudioChannel> = state
                        .feeder_output_ports
                        .iter()
                        .find(|(_, port_id)| **port_id == id)
                        .map(|(channel, _)| *channel);
                    if let Some(channel) = dropped_feeder_channel {
                        state.feeder_output_ports.remove(&channel);
                        state.feeder_links.remove(&channel);
                    }
                    let dropped_vmic_channel: Option<AudioChannel> = state
                        .virtual_mic_input_ports
                        .iter()
                        .find(|(_, port_id)| **port_id == id)
                        .map(|(channel, _)| *channel);
                    if let Some(channel) = dropped_vmic_channel {
                        state.virtual_mic_input_ports.remove(&channel);
                        state.feeder_links.remove(&channel);
                    }
                    if state.capture_node_id == Some(id) {
                        state.capture_node_id = None;
                    }
                    // Only reconnect when the removed source is the one we are
                    // actively capturing from. Spurious reconnects on unrelated
                    // source removals tear down the capture stream unnecessarily,
                    // inserting a silence gap every time any Audio/Source leaves
                    // the graph (e.g. another app's monitor source).
                    let affects_current_capture = removed_source_name
                        .as_deref()
                        .zip(state.active_capture_target.as_deref())
                        .is_some_and(|(removed, active)| removed == active);
                    if removed_source && state.runtime.mic_passthrough && affects_current_capture {
                        if let Err(err) = recreate_capture_stream(&mut state) {
                            warn!(
                                "Failed to re-resolve capture target after source removal: {err}"
                            );
                        }
                    }
                    state.publish_snapshot();
                }
            }
        })
        .register();

    let local_stream = create_local_output_stream(
        core.clone(),
        queues.clone(),
        stream_runtime.clone(),
        runtime.pipewire_latency_hint(),
    )
    .ok();

    // Load the real null-sink that backs `linuxsoundboard.virtual_mic`. This
    // MUST happen before the feeder stream is created — the feeder targets
    // the null-sink by name, so the sink has to exist in the graph first.
    // We log a warning on failure but continue: the feeder still works as a
    // fallback in-process source for apps that aren't picky about node type,
    // even if WirePlumber won't pin it as the system default.
    let virtual_mic_module = match virtual_mic_module::NullSinkModule::load_or_attach() {
        Ok(module) => Some(module),
        Err(err) => {
            warn!(
                "Failed to load null-sink module for virtual mic: {err}. \
                 Apps may not see Linux Soundboard Mic as the system default; \
                 install pulseaudio-utils (pactl) and retry."
            );
            None
        }
    };

    debug!("Creating virtual mic feeder stream");
    let virtual_stream = create_runtime_virtual_source_stream(
        core.clone(),
        queues.clone(),
        stream_runtime,
        runtime.pipewire_latency_hint(),
    )
    .ok();

    Ok(PipeWireBackendState {
        _context: context,
        core,
        _registry: registry,
        _registry_listener: registry_listener,
        _local_stream: local_stream,
        virtual_stream,
        capture_stream: None,
        _virtual_mic_module: virtual_mic_module,
    })
}
