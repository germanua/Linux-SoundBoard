use super::*;

const INPUT_STREAM_MEDIA_CLASS: &str = "Stream/Input/Audio";
const DEFAULT_METADATA_NAME: &str = "default";
const METADATA_NAME_KEY: &str = "metadata.name";
const TARGET_NODE_KEY: &str = "target.node";
const TARGET_OBJECT_KEY: &str = "target.object";
const NODE_DONT_MOVE_KEY: &str = "node.dont-move";
const STREAM_CAPTURE_SINK_KEY: &str = "stream.capture.sink";
const SPA_ID_TYPE: &str = "Spa:Id";
const SPA_STRING_TYPE: &str = "Spa:String";
const MAX_AUTOROUTE_RETRIES: u8 = 2;
const DEFAULT_AUDIO_SOURCE_KEY: &str = "default.audio.source";
const PW_ID_CORE: u32 = 0;

pub(super) struct DefaultMetadataHandle {
    pub(super) id: u32,
    metadata: pw::metadata::Metadata,
    _metadata_listener: pw::metadata::MetadataListener,
}

impl DefaultMetadataHandle {
    fn set_property(&self, subject: u32, key: &str, type_: Option<&str>, value: Option<&str>) {
        self.metadata.set_property(subject, key, type_, value);
    }
}

pub(super) struct InputStreamNodeHandle {
    _node: pw::node::Node,
    _listener: pw::node::NodeListener,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AutoroutedInputStream {
    target_node_id: u32,
    target_object: String,
    retries: u8,
}

#[derive(Clone, Copy)]
struct VirtualMicTarget {
    node_id: u32,
    serial: Option<u64>,
}

impl VirtualMicTarget {
    fn target_object_value(self) -> (Option<&'static str>, String) {
        self.serial
            .map(|serial| (Some(SPA_ID_TYPE), serial.to_string()))
            .unwrap_or_else(|| (Some(SPA_STRING_TYPE), VIRTUAL_SOURCE_NAME.to_string()))
    }
}

pub(super) fn bind_default_metadata_from_global(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
    weak_state: Weak<RefCell<LoopState>>,
) -> Option<DefaultMetadataHandle> {
    if global.type_ != pw::types::ObjectType::Metadata {
        return None;
    }
    let props = global.props?;
    if props.get(METADATA_NAME_KEY)? != DEFAULT_METADATA_NAME {
        return None;
    }

    match registry.bind::<pw::metadata::Metadata, _>(global) {
        Ok(metadata) => {
            let metadata_id = global.id;
            let metadata_listener = metadata
                .add_listener_local()
                .property(move |subject, key, _type_, value| {
                    if key == Some(TARGET_OBJECT_KEY) {
                        if let Some(state) = weak_state.upgrade() {
                            let mut state = state.borrow_mut();
                            handle_input_stream_target_metadata(
                                &mut state,
                                subject,
                                value.map(str::to_string),
                            );
                        }
                    } else if key == Some(DEFAULT_AUDIO_SOURCE_KEY) && subject == PW_ID_CORE {
                        if let Some(state) = weak_state.upgrade() {
                            let mut state = state.borrow_mut();
                            state.default_audio_source_name = value
                                .map(str::to_string)
                                .filter(|v| !v.is_empty());
                        }
                    }
                    0
                })
                .register();
            info!("Registered PipeWire default metadata for input stream routing");
            Some(DefaultMetadataHandle {
                id: metadata_id,
                metadata,
                _metadata_listener: metadata_listener,
            })
        }
        Err(err) => {
            warn!("Failed to bind PipeWire default metadata: {err}");
            None
        }
    }
}

pub(super) fn bind_input_stream_node_from_global(
    registry: &pw::registry::RegistryRc,
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
    weak_state: Weak<RefCell<LoopState>>,
) -> Option<(InputStreamDescriptor, InputStreamNodeHandle)> {
    let descriptor = input_stream_from_global(global)?;
    let stream_id = descriptor.id;
    let node = match registry.bind::<pw::node::Node, _>(global) {
        Ok(node) => node,
        Err(err) => {
            warn!(
                "Failed to bind PipeWire input stream node '{}': {err}",
                descriptor.node_name
            );
            return None;
        }
    };

    let listener = node
        .add_listener_local()
        .info(move |info| {
            let Some(state) = weak_state.upgrade() else {
                return;
            };
            let mut state = state.borrow_mut();
            if let Some(props) = info.props() {
                if let Some(mut next) = input_stream_descriptor_from_props(stream_id, props) {
                    if let Some(target_object) =
                        state.input_stream_metadata_targets.get(&stream_id).cloned()
                    {
                        next.target_object = Some(target_object);
                    }
                    let target_changed = state
                        .input_streams
                        .get(&stream_id)
                        .map(|current| current.target_object != next.target_object)
                        .unwrap_or(false);
                    state.input_streams.insert(stream_id, next);
                    if target_changed {
                        let target_object = state
                            .input_streams
                            .get(&stream_id)
                            .and_then(|stream| stream.target_object.clone());
                        handle_input_stream_target_metadata(&mut state, stream_id, target_object);
                    } else {
                        maybe_autoroute_input_streams(&mut state);
                    }
                } else {
                    state.input_streams.remove(&stream_id);
                    state.input_stream_metadata_targets.remove(&stream_id);
                    state.autorouted_input_streams.remove(&stream_id);
                    state.autoroute_blocked_input_streams.remove(&stream_id);
                }
            }
        })
        .register();

    Some((
        descriptor,
        InputStreamNodeHandle {
            _node: node,
            _listener: listener,
        },
    ))
}

pub(super) fn input_stream_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<InputStreamDescriptor> {
    if global.type_ != pw::types::ObjectType::Node {
        return None;
    }

    input_stream_descriptor_from_props(global.id, global.props?)
}

pub(super) fn maybe_autoroute_input_streams(state: &mut LoopState) {
    if !autoroute_enabled(state.runtime.default_source_mode) {
        clear_autorouted_input_streams(state);
        state.autoroute_blocked_input_streams.clear();
        return;
    }

    let Some(target) = virtual_mic_target(state) else {
        clear_autorouted_input_streams(state);
        return;
    };

    let stream_ids = state.input_streams.keys().copied().collect::<Vec<_>>();
    for stream_id in stream_ids {
        maybe_autoroute_input_stream(state, stream_id, target);
    }
}

pub(super) fn clear_autorouted_input_streams(state: &mut LoopState) {
    if state.autorouted_input_streams.is_empty() {
        return;
    }

    let routes = std::mem::take(&mut state.autorouted_input_streams);
    let Some(metadata) = state.default_metadata.as_ref() else {
        state.autoroute_blocked_input_streams.clear();
        return;
    };

    for (stream_id, route) in routes {
        let should_clear = state
            .input_streams
            .get(&stream_id)
            .map(|stream| stream_target_matches_route(stream.target_object.as_deref(), &route))
            .unwrap_or(true);
        if should_clear {
            state.input_stream_metadata_targets.remove(&stream_id);
            if let Some(stream) = state.input_streams.get_mut(&stream_id) {
                stream.target_object = None;
            }
            metadata.set_property(stream_id, TARGET_NODE_KEY, None, None);
            metadata.set_property(stream_id, TARGET_OBJECT_KEY, None, None);
        }
    }
    state.autoroute_blocked_input_streams.clear();
}

pub(super) fn autoroute_enabled(mode: DefaultSourceMode) -> bool {
    matches!(
        mode,
        DefaultSourceMode::AutoRouteWhileRunning | DefaultSourceMode::TemporaryDefaultWhileRunning
    )
}

fn input_stream_descriptor_from_props(
    id: u32,
    props: &spa::utils::dict::DictRef,
) -> Option<InputStreamDescriptor> {
    if props.get(*pw::keys::MEDIA_CLASS)? != INPUT_STREAM_MEDIA_CLASS {
        return None;
    }

    let node_name = props.get(*pw::keys::NODE_NAME)?.to_string();
    Some(InputStreamDescriptor {
        id,
        node_name,
        app_name: props.get(*pw::keys::APP_NAME).map(str::to_string),
        app_id: props.get(*pw::keys::APP_ID).map(str::to_string),
        app_process_binary: props.get(*pw::keys::APP_PROCESS_BINARY).map(str::to_string),
        media_name: props.get(*pw::keys::MEDIA_NAME).map(str::to_string),
        media_role: props.get(*pw::keys::MEDIA_ROLE).map(str::to_string),
        target_object: props.get(TARGET_OBJECT_KEY).map(str::to_string),
        dont_move: prop_is_true(props.get(NODE_DONT_MOVE_KEY)),
        stream_capture_sink: prop_is_true(props.get(STREAM_CAPTURE_SINK_KEY)),
    })
}

fn maybe_autoroute_input_stream(state: &mut LoopState, stream_id: u32, target: VirtualMicTarget) {
    if state.autoroute_blocked_input_streams.contains(&stream_id) {
        return;
    }

    let Some(stream) = state.input_streams.get(&stream_id).cloned() else {
        return;
    };
    let upstream_source = resolve_upstream_capture_source(state);
    if !input_stream_should_autoroute(&stream, target, upstream_source.as_ref()) {
        let reason = input_stream_skip_reason(&stream, target, upstream_source.as_ref());
        debug!(
            "Skipping routing '{}': {}",
            stream_label(&stream),
            reason.unwrap_or("unknown")
        );

        // If the stream was previously auto-routed but its OWN properties now
        // disqualify it as a microphone stream (e.g. stream.capture.sink arriving
        // late in node.info), clear the routing. Only do this for hard categorical
        // disqualifiers — NOT for "already targeting virtual mic" (routing correct)
        // or "explicit non-upstream target" (ambiguous user preference).
        let hard_disqualified = stream.stream_capture_sink || stream.dont_move;
        if hard_disqualified && state.autorouted_input_streams.remove(&stream_id).is_some() {
            state.input_stream_metadata_targets.remove(&stream_id);
            if let Some(s) = state.input_streams.get_mut(&stream_id) {
                s.target_object = None;
            }
            if let Some(metadata) = state.default_metadata.as_ref() {
                metadata.set_property(stream_id, TARGET_NODE_KEY, None, None);
                metadata.set_property(stream_id, TARGET_OBJECT_KEY, None, None);
            }
            info!(
                "Un-routed '{}' after property update ({})",
                stream_label(&stream),
                reason.unwrap_or("filter changed")
            );
        }
        return;
    }

    let (target_object_type, target_object) = target.target_object_value();
    if state
        .autorouted_input_streams
        .get(&stream_id)
        .is_some_and(|route| {
            route.target_node_id == target.node_id
                && route.target_object == target_object
                && stream_target_matches_route(stream.target_object.as_deref(), route)
        })
    {
        return;
    }

    let retries = state
        .autorouted_input_streams
        .get(&stream_id)
        .map(|route| route.retries)
        .unwrap_or(0);
    if retries >= MAX_AUTOROUTE_RETRIES {
        state.autoroute_blocked_input_streams.insert(stream_id);
        state.autorouted_input_streams.remove(&stream_id);
        warn!(
            "Stopped auto-routing input stream '{}' after repeated external target changes",
            stream_label(&stream)
        );
        return;
    }

    if state.default_metadata.is_none() {
        return;
    }

    state.autorouted_input_streams.insert(
        stream_id,
        AutoroutedInputStream {
            target_node_id: target.node_id,
            target_object: target_object.clone(),
            retries,
        },
    );
    if let Some(stream) = state.input_streams.get_mut(&stream_id) {
        stream.target_object = Some(target_object.clone());
    }
    state
        .input_stream_metadata_targets
        .insert(stream_id, target_object.clone());

    let Some(metadata) = state.default_metadata.as_ref() else {
        return;
    };

    let target_node = target.node_id.to_string();
    metadata.set_property(
        stream_id,
        TARGET_NODE_KEY,
        Some(SPA_ID_TYPE),
        Some(&target_node),
    );
    metadata.set_property(
        stream_id,
        TARGET_OBJECT_KEY,
        target_object_type,
        Some(&target_object),
    );

    let label = stream_label(&stream);
    info!("Routed input stream '{}' to {}", label, VIRTUAL_SOURCE_NAME);
}

fn handle_input_stream_target_metadata(
    state: &mut LoopState,
    stream_id: u32,
    target_object: Option<String>,
) {
    match target_object.as_ref() {
        Some(target_object) => {
            state
                .input_stream_metadata_targets
                .insert(stream_id, target_object.clone());
        }
        None => {
            state.input_stream_metadata_targets.remove(&stream_id);
        }
    }

    let Some(stream) = state.input_streams.get_mut(&stream_id) else {
        return;
    };
    stream.target_object = target_object.clone();

    let Some(route) = state.autorouted_input_streams.get_mut(&stream_id) else {
        maybe_autoroute_input_streams(state);
        return;
    };

    if stream_target_matches_route(target_object.as_deref(), route) {
        return;
    }

    if route.retries < MAX_AUTOROUTE_RETRIES {
        route.retries = route.retries.saturating_add(1);
        maybe_autoroute_input_streams(state);
    } else {
        state.autorouted_input_streams.remove(&stream_id);
        state.autoroute_blocked_input_streams.insert(stream_id);
        if let Some(stream) = state.input_streams.get(&stream_id) {
            warn!(
                "Stopped auto-routing input stream '{}' after another manager changed its target",
                stream_label(stream)
            );
        }
    }
}

fn virtual_mic_target(state: &LoopState) -> Option<VirtualMicTarget> {
    state
        .sources
        .values()
        .find(|source| source.node_name == VIRTUAL_SOURCE_NAME)
        .map(|source| VirtualMicTarget {
            node_id: source.id,
            serial: source.serial,
        })
}

fn resolve_upstream_capture_source(state: &LoopState) -> Option<SourceDescriptor> {
    resolve_capture_target(state).and_then(|target| {
        state
            .sources
            .values()
            .find(|source| source.node_name == target)
            .cloned()
    })
}

fn input_stream_should_autoroute(
    stream: &InputStreamDescriptor,
    target: VirtualMicTarget,
    upstream_source: Option<&SourceDescriptor>,
) -> bool {
    input_stream_skip_reason(stream, target, upstream_source).is_none()
}

fn input_stream_skip_reason(
    stream: &InputStreamDescriptor,
    target: VirtualMicTarget,
    upstream_source: Option<&SourceDescriptor>,
) -> Option<&'static str> {
    if stream.dont_move {
        return Some("node.dont-move=true");
    }
    if stream.stream_capture_sink {
        return Some("stream.capture.sink=true");
    }
    if is_own_stream(&stream.node_name) {
        return Some("own stream");
    }
    if is_processor_internal_stream(stream) {
        return Some("processor internal (easyeffects/noisetorch)");
    }

    let Some(target_object) = stream.target_object.as_deref() else {
        return None;
    };
    if target_object_is_default(target_object) {
        return None;
    }
    if target_object_matches_virtual_mic(target_object, target) {
        return Some("already targeting virtual mic");
    }
    if upstream_source.is_some_and(|source| target_object_matches_source(target_object, source)) {
        return None;
    }

    Some("explicit non-upstream target")
}

fn stream_target_matches_route(target_object: Option<&str>, route: &AutoroutedInputStream) -> bool {
    target_object
        .is_some_and(|target| target == route.target_object || target == VIRTUAL_SOURCE_NAME)
}

fn target_object_matches_virtual_mic(target_object: &str, target: VirtualMicTarget) -> bool {
    if target_object == VIRTUAL_SOURCE_NAME {
        return true;
    }
    target
        .serial
        .is_some_and(|serial| target_object.parse::<u64>().ok() == Some(serial))
}

fn target_object_matches_source(target_object: &str, source: &SourceDescriptor) -> bool {
    target_object == source.node_name
        || source
            .serial
            .is_some_and(|serial| target_object.parse::<u64>().ok() == Some(serial))
}

fn target_object_is_default(target_object: &str) -> bool {
    matches!(
        target_object.trim().to_ascii_lowercase().as_str(),
        "default" | "@default_source@" | "@default_audio_source@"
    )
}

fn is_own_stream(node_name: &str) -> bool {
    matches!(
        node_name,
        MIC_CAPTURE_NODE_NAME
            | VIRTUAL_FEEDER_NODE_NAME
            | LOCAL_PLAYBACK_NODE_NAME
            | VIRTUAL_SOURCE_NAME
    ) || node_name.starts_with("linuxsoundboard.")
}

fn is_processor_internal_stream(stream: &InputStreamDescriptor) -> bool {
    if stream.node_name.starts_with("easyeffects.")
        || stream.node_name.starts_with("easyeffects_")
        || stream.node_name.starts_with("ee_")
        || stream.node_name.contains("output_level")
        || stream.node_name.contains("spectrum")
    {
        return true;
    }
    if stream
        .media_role
        .as_deref()
        .is_some_and(|role| role.eq_ignore_ascii_case("DSP"))
    {
        return true;
    }

    stream_identity_values(stream).any(|value| {
        let value = value.to_ascii_lowercase();
        [
            "easyeffects",
            "easy effects",
            "noisetorch",
            "noise_torch",
            "rnnoise",
        ]
        .iter()
        .any(|needle| value.contains(needle))
    })
}

fn stream_identity_values(stream: &InputStreamDescriptor) -> impl Iterator<Item = &str> {
    [
        Some(stream.node_name.as_str()),
        stream.app_name.as_deref(),
        stream.app_id.as_deref(),
        stream.app_process_binary.as_deref(),
        stream.media_name.as_deref(),
        stream.media_role.as_deref(),
    ]
    .into_iter()
    .flatten()
}

fn stream_label(stream: &InputStreamDescriptor) -> &str {
    stream
        .app_name
        .as_deref()
        .filter(|name| !name.is_empty())
        .unwrap_or(stream.node_name.as_str())
}

fn prop_is_true(value: Option<&str>) -> bool {
    matches!(value, Some("true" | "1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> VirtualMicTarget {
        VirtualMicTarget {
            node_id: 100,
            serial: Some(200),
        }
    }

    fn source(node_name: &str, serial: Option<u64>) -> SourceDescriptor {
        SourceDescriptor {
            id: 7,
            serial,
            node_name: node_name.to_string(),
            display_name: node_name.to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: node_name == VIRTUAL_SOURCE_NAME,
        }
    }

    fn stream(node_name: &str) -> InputStreamDescriptor {
        InputStreamDescriptor {
            id: 42,
            node_name: node_name.to_string(),
            app_name: None,
            app_id: None,
            app_process_binary: None,
            media_name: None,
            media_role: None,
            target_object: None,
            dont_move: false,
            stream_capture_sink: false,
        }
    }

    #[test]
    fn autoroute_skips_own_streams() {
        assert!(!input_stream_should_autoroute(
            &stream(MIC_CAPTURE_NODE_NAME),
            target(),
            None
        ));
        assert!(!input_stream_should_autoroute(
            &stream("linuxsoundboard.anything"),
            target(),
            None
        ));
    }

    #[test]
    fn autoroute_skips_streams_marked_dont_move() {
        let mut stream = stream("WEBRTC VoiceEngine");
        stream.dont_move = true;

        assert!(!input_stream_should_autoroute(&stream, target(), None));
    }

    #[test]
    fn autoroute_skips_capture_sink_streams() {
        let mut stream = stream("OBS Monitor");
        stream.stream_capture_sink = true;

        assert!(!input_stream_should_autoroute(&stream, target(), None));
    }

    #[test]
    fn autoroute_skips_easyeffects_internal_streams() {
        let mut descriptor = stream("ee_input_level");
        assert!(!input_stream_should_autoroute(&descriptor, target(), None));

        descriptor = stream("input");
        descriptor.app_name = Some("EasyEffects".to_string());
        assert!(!input_stream_should_autoroute(&descriptor, target(), None));
    }

    #[test]
    fn autoroute_skips_noisetorch_internal_streams() {
        let mut stream = stream("input");
        stream.app_process_binary = Some("noisetorch".to_string());

        assert!(!input_stream_should_autoroute(&stream, target(), None));
    }

    #[test]
    fn autoroute_accepts_external_input_streams_without_target() {
        assert!(input_stream_should_autoroute(
            &stream("WEBRTC VoiceEngine"),
            target(),
            None
        ));
    }

    #[test]
    fn autoroute_accepts_external_input_streams_targeting_default() {
        let mut stream = stream("WEBRTC VoiceEngine");
        stream.target_object = Some("@DEFAULT_SOURCE@".to_string());

        assert!(input_stream_should_autoroute(&stream, target(), None));
    }

    #[test]
    fn autoroute_accepts_streams_targeting_upstream_capture_source() {
        let mut stream = stream("WEBRTC VoiceEngine");
        stream.target_object = Some("easyeffects_source".to_string());

        assert!(input_stream_should_autoroute(
            &stream,
            target(),
            Some(&source("easyeffects_source", Some(300)))
        ));

        stream.target_object = Some("300".to_string());
        assert!(input_stream_should_autoroute(
            &stream,
            target(),
            Some(&source("easyeffects_source", Some(300)))
        ));
    }

    #[test]
    fn autoroute_skips_streams_already_targeting_virtual_mic() {
        let mut stream = stream("WEBRTC VoiceEngine");
        stream.target_object = Some(VIRTUAL_SOURCE_NAME.to_string());

        assert!(!input_stream_should_autoroute(&stream, target(), None));

        stream.target_object = Some("200".to_string());
        assert!(!input_stream_should_autoroute(&stream, target(), None));
    }

    #[test]
    fn autoroute_skips_unrelated_explicit_targets() {
        let mut stream = stream("WEBRTC VoiceEngine");
        stream.target_object = Some("alsa_input.other".to_string());

        assert!(!input_stream_should_autoroute(
            &stream,
            target(),
            Some(&source("easyeffects_source", None))
        ));
    }

    #[test]
    fn autoroute_skips_easyeffects_underscore_node_names() {
        assert!(!input_stream_should_autoroute(
            &stream("easyeffects_input"),
            target(),
            None
        ));
        assert!(!input_stream_should_autoroute(
            &stream("easyeffects_output"),
            target(),
            None
        ));
        assert!(!input_stream_should_autoroute(
            &stream("easyeffects_source"),
            target(),
            None
        ));
    }

    #[test]
    fn skip_reason_is_reported_for_filtered_streams() {
        let mut s = stream("WEBRTC VoiceEngine");
        s.dont_move = true;
        assert_eq!(
            input_stream_skip_reason(&s, target(), None),
            Some("node.dont-move=true")
        );

        let s2 = stream("easyeffects_input");
        assert_eq!(
            input_stream_skip_reason(&s2, target(), None),
            Some("processor internal (easyeffects/noisetorch)")
        );

        let routable = stream("Discord");
        assert_eq!(input_stream_skip_reason(&routable, target(), None), None);
    }
}
