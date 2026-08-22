//! `LoopState` — everything the PipeWire engine loop thread owns.
//!
//! The fields split into five groups and the split is load-bearing: each group
//! has exactly one writer. A new field that fits none of them probably wants a
//! sub-struct of its own.
//!
//! - config (`runtime`): written once at init, read by everyone
//! - registry mirror (`sources` .. `default_source_command_in_flight`): written
//!   only by the `global`/`global_remove` callbacks, read by command handlers
//!   and the capture watchdog
//! - playback (`active_playback` .. `next_playback_order`): mix tick and
//!   command handlers write, snapshot publish reads
//! - RT shared (`queues` .. `mic_scratch_buffer`): mix tick writes on the main
//!   loop, the process callback reads from the RT side through `try_lock`
//! - UI publish (`snapshot` .. `last_had_active`): `publish_snapshot` writes,
//!   the UI thread reads the `Arc<RwLock>`

use super::*;

pub(super) struct LoopState {
    // Config: read-only after `new()`, safe to clone into closures.
    pub(super) runtime: RuntimeConfig,

    // Registry mirror: written only from the PipeWire `global`/`global_remove`
    // callbacks, never from the RT process callback or the UI thread.
    pub(super) available: bool,
    pub(super) default_metadata: Option<DefaultMetadataHandle>,
    pub(super) backend: Option<BackendState>,
    pub(super) sources: HashMap<u32, SourceDescriptor>,
    pub(super) sinks: HashMap<u32, SinkDescriptor>,
    // Explicit-link state, filled by the registry callback as Node and Port
    // globals turn up. Once all four ids are known — feeder node plus its two
    // output ports, virtual mic plus its two input ports —
    // `try_link_feeder_to_virtual_mic` builds the FL/FR links.
    pub(super) feeder_node_id: Option<u32>,
    pub(super) feeder_output_ports: HashMap<AudioChannel, u32>,
    pub(super) virtual_mic_node_id: Option<u32>,
    pub(super) virtual_mic_input_ports: HashMap<AudioChannel, u32>,
    pub(super) feeder_links: HashMap<AudioChannel, FeederLink>,
    pub(super) default_audio_source_name: Option<String>,
    pub(super) virtual_mic_state_reset_ids: HashSet<u32>,
    pub(super) virtual_mic_missing_since: Option<Instant>,
    pub(super) last_virtual_mic_repair_attempt: Option<Instant>,
    pub(super) links: HashMap<u32, LinkDescriptor>,
    pub(super) capture_node_id: Option<u32>,
    pub(super) active_capture_target: Option<String>,
    pub(super) capture_health_miss_ticks: u8,
    pub(super) previous_default_source_name: Option<String>,
    pub(super) claimed_default: bool,
    pub(super) default_source_command_in_flight: std::sync::Arc<AtomicBool>,

    // Playback tracking: written by the mix tick (advance/finish) and the
    // command handlers (Play/StopAll/Seek), read by `publish_snapshot`.
    pub(super) active_playback: Option<ActivePlayback>,
    pub(super) finished_playbacks: HashMap<String, PlaybackSnapshot>,
    pub(super) next_playback_order: u64,

    // RT shared. `queues` goes to the PipeWire process callback, which only
    // ever `try_lock`s it, never blocks. The three buffers are main-loop
    // scratch for the mix tick, pre-allocated so the tick never reaches the
    // allocator while holding the queues lock.
    pub(super) queues: RtSharedQueues,
    pub(super) stream_runtime: std::sync::Arc<StreamRuntimeShared>,
    pub(super) ultra_starvation_ticks: u32,
    pub(super) local_mix_buffer: Vec<f32>,
    pub(super) virtual_mix_buffer: Vec<f32>,
    pub(super) mic_scratch_buffer: Vec<f32>,

    // UI publish. `snapshot` is shared with the UI thread; the main loop writes
    // it in `publish_snapshot`. `last_ui_send`/`last_had_active` throttle how
    // often we invoke the GTK main context, so the UI isn't flooded.
    pub(super) snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
    pub(super) last_ui_send: Option<Instant>,
    pub(super) last_had_active: bool,
}

impl LoopState {
    pub(super) fn new(
        runtime: RuntimeConfig,
        snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
    ) -> Self {
        let stream_runtime = std::sync::Arc::new(StreamRuntimeShared::new(&runtime));
        Self {
            runtime,
            available: false,
            default_metadata: None,
            backend: None,
            sources: HashMap::new(),
            sinks: HashMap::new(),
            feeder_node_id: None,
            feeder_output_ports: HashMap::new(),
            virtual_mic_node_id: None,
            virtual_mic_input_ports: HashMap::new(),
            feeder_links: HashMap::new(),
            default_audio_source_name: None,
            virtual_mic_state_reset_ids: HashSet::new(),
            virtual_mic_missing_since: None,
            last_virtual_mic_repair_attempt: None,
            links: HashMap::new(),
            capture_node_id: None,
            active_capture_target: None,
            capture_health_miss_ticks: 0,
            previous_default_source_name: None,
            claimed_default: false,
            default_source_command_in_flight: std::sync::Arc::new(AtomicBool::new(false)),
            active_playback: None,
            finished_playbacks: HashMap::new(),
            next_playback_order: 0,
            queues: RtSharedQueues::new(ProcessQueues::new(
                OUTPUT_QUEUE_CAPACITY_SAMPLES,
                OUTPUT_QUEUE_CAPACITY_SAMPLES,
                MIC_QUEUE_CAPACITY_SAMPLES,
            )),
            stream_runtime,
            ultra_starvation_ticks: 0,
            snapshot,
            last_ui_send: None,
            last_had_active: false,
            local_mix_buffer: Vec::new(),
            virtual_mix_buffer: Vec::new(),
            mic_scratch_buffer: Vec::new(),
        }
    }

    pub(super) fn snapshot_positions(&self) -> Vec<PlaybackPosition> {
        let mut registry = self.finished_playbacks.clone();
        if let Some(active) = &self.active_playback {
            registry.insert(
                active.play_id.clone(),
                PlaybackSnapshot {
                    sound_id: active.sound_id.clone(),
                    playback_order: active.playback_order,
                    position_ms: active.position_ms,
                    paused: active.paused,
                    duration_ms: active.duration_ms,
                    finished: active.finished,
                },
            );
        }
        build_playback_positions(&registry)
    }

    pub(super) fn list_audio_sources(&self) -> Vec<AudioSourceInfo> {
        let mut sources = self
            .sources
            .values()
            .filter(|source| !source.is_monitor && !source.is_our_virtual_mic)
            .cloned()
            .collect::<Vec<_>>();
        sources.sort_by(|left, right| left.display_name.cmp(&right.display_name));
        sources
            .into_iter()
            .map(|source| AudioSourceInfo {
                node_name: source.node_name,
                display_name: source.display_name,
                is_virtual: source.is_virtual,
                is_hardware_backed: source.is_hardware_backed,
            })
            .collect()
    }

    pub(super) fn playing_ids(&self) -> Vec<String> {
        self.active_playback
            .as_ref()
            .filter(|playback| !playback.finished)
            .map(|playback| vec![playback.sound_id.clone()])
            .unwrap_or_default()
    }

    pub(super) fn trim_finished_playbacks(&mut self, max_entries: usize) {
        while self.finished_playbacks.len() > max_entries {
            let oldest_play_id = self
                .finished_playbacks
                .iter()
                .min_by_key(|(_, snapshot)| snapshot.playback_order)
                .map(|(play_id, _)| play_id.clone());
            if let Some(play_id) = oldest_play_id {
                self.finished_playbacks.remove(&play_id);
            } else {
                break;
            }
        }
    }

    pub(super) fn publish_snapshot(&mut self) {
        let new_snapshot = PlayerSnapshot {
            available: self.available,
            playback_positions: self.snapshot_positions(),
            playing_ids: self.playing_ids(),
            audio_sources: self.list_audio_sources(),
            active_capture_target: self.active_capture_target.clone(),
        };

        *self.snapshot.write() = new_snapshot.clone();

        let has_active = new_snapshot.playback_positions.iter().any(|p| !p.finished);
        let is_state_change = has_active != self.last_had_active;
        let throttle_ok = self
            .last_ui_send
            .map(|t| t.elapsed() >= Duration::from_millis(UI_SNAPSHOT_PROGRESS_INTERVAL_MS))
            .unwrap_or(true);

        if is_state_change || (has_active && throttle_ok) {
            self.last_ui_send = Some(Instant::now());
            self.last_had_active = has_active;
            glib::MainContext::default().invoke(move || {
                crate::ui_event_bridge::dispatch_snapshot(new_snapshot);
            });
        }
    }

    pub(super) fn backend_playback_available(&self) -> bool {
        self.backend
            .as_ref()
            .is_some_and(BackendState::playback_stream_active)
    }
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

    fn make_finished_snapshot(order: u64) -> PlaybackSnapshot {
        PlaybackSnapshot {
            sound_id: format!("snd-{order}"),
            playback_order: order,
            position_ms: 500,
            paused: false,
            duration_ms: Some(1000),
            finished: true,
        }
    }

    fn make_source(id: u32, node_name: &str, is_monitor: bool, is_ours: bool) -> SourceDescriptor {
        SourceDescriptor {
            id,
            serial: None,
            node_name: node_name.to_string(),
            display_name: node_name.to_string(),
            priority_session: 0,
            is_monitor,
            is_our_virtual_mic: is_ours,
            is_virtual: is_ours,
            is_hardware_backed: !is_monitor && !is_ours,
        }
    }

    #[test]
    fn trim_under_limit_leaves_all_entries() {
        let mut state = make_state();
        for i in 0..3u64 {
            state
                .finished_playbacks
                .insert(format!("p{i}"), make_finished_snapshot(i));
        }
        state.trim_finished_playbacks(5);
        assert_eq!(state.finished_playbacks.len(), 3);
    }

    #[test]
    fn trim_removes_oldest_when_over_limit() {
        let mut state = make_state();
        for i in 0..5u64 {
            state
                .finished_playbacks
                .insert(format!("p{i}"), make_finished_snapshot(i));
        }
        state.trim_finished_playbacks(3);
        assert_eq!(state.finished_playbacks.len(), 3);
        assert!(
            !state.finished_playbacks.contains_key("p0"),
            "order-0 should be evicted"
        );
        assert!(
            !state.finished_playbacks.contains_key("p1"),
            "order-1 should be evicted"
        );
        assert!(state.finished_playbacks.contains_key("p4"));
    }

    #[test]
    fn list_sources_excludes_monitors_and_virtual_mic() {
        let mut state = make_state();
        state
            .sources
            .insert(1, make_source(1, "alsa_input.mic", false, false));
        state
            .sources
            .insert(2, make_source(2, "alsa_output.hdmi.monitor", true, false));
        state.sources.insert(
            3,
            make_source(3, crate::app_meta::VIRTUAL_SOURCE_NAME, false, true),
        );
        let result = state.list_audio_sources();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].node_name, "alsa_input.mic");
    }

    #[test]
    fn list_sources_sorted_by_display_name() {
        let mut state = make_state();
        for (id, name) in [(1u32, "Zebra"), (2, "Apple"), (3, "Mango")] {
            state.sources.insert(
                id,
                SourceDescriptor {
                    id,
                    serial: None,
                    node_name: format!("node.{}", name.to_lowercase()),
                    display_name: name.to_string(),
                    priority_session: 0,
                    is_monitor: false,
                    is_our_virtual_mic: false,
                    is_virtual: false,
                    is_hardware_backed: true,
                },
            );
        }
        let result = state.list_audio_sources();
        assert_eq!(result[0].display_name, "Apple");
        assert_eq!(result[1].display_name, "Mango");
        assert_eq!(result[2].display_name, "Zebra");
    }

    #[test]
    fn playing_ids_empty_with_no_active_playback() {
        let state = make_state();
        assert!(state.playing_ids().is_empty());
    }
}
