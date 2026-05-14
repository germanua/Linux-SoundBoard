//! PipeWire-backed audio playback with a runtime virtual microphone.

use glib;
use log::{debug, error, info, trace, warn};
use ogg::PacketReader;
use opus::{Channels as OpusChannels, Decoder as OpusDecoder};
use pipewire as pw;
use pw::channel as pw_channel;
use pw::properties::properties;
use pw::spa;
use rodio::source::SeekError as RodioSeekError;
use rodio::source::UniformSourceIterator;
use rodio::Source;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io::BufReader as IoBufReader;
use std::mem;
use std::process::Command;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Mutex, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

use crate::app_meta::{
    LOCAL_PLAYBACK_NODE_NAME, MIC_CAPTURE_NODE_NAME, VIRTUAL_FEEDER_NODE_NAME,
    VIRTUAL_MIC_DESCRIPTION, VIRTUAL_OUTPUT_DESCRIPTION, VIRTUAL_SOURCE_NAME,
};
use crate::config::{DefaultSourceMode, MicLatencyProfile};

mod command_handlers;
mod explicit_links;
mod limiter;
mod mixing;
mod playback;
mod pulse_backend;
mod queues;
mod source_autoroute;
mod source_routing;
mod streams;

use command_handlers::{audio_command_kind, handle_audio_command};
use explicit_links::{
    handle_link_global_remove, track_node_global, track_port_global, AudioChannel, TrackedPort,
};
use limiter::LookAheadLimiter;
use mixing::clear_mic_input_queue;
#[cfg(test)]
use mixing::clear_virtual_mic_queues;
use mixing::{clear_all_queues, clear_output_queues, mix_tick};
#[cfg(test)]
use mixing::{enqueue_passthrough_chunk, fill_output_queues};
use playback::ActivePlayback;
use pulse_backend::PulseAudioBackend;
use queues::ProcessQueues;
use source_autoroute::{
    bind_default_metadata_from_global, bind_input_stream_node_from_global,
    clear_autorouted_input_streams, maybe_autoroute_input_streams, AutoroutedInputStream,
    DefaultMetadataHandle, InputStreamNodeHandle,
};
use source_routing::{
    apply_default_source_mode, maybe_claim_default_source, recreate_capture_stream,
    resolve_capture_target, resolve_source_id_by_name, restore_default_source,
};
#[cfg(test)]
use source_routing::{
    best_fallback_source_name, parse_wpctl_node_name, resolve_capture_target_from_default,
};
use streams::{
    create_capture_stream, create_local_output_stream, create_runtime_virtual_source_stream,
};

const TARGET_OUTPUT_SAMPLE_RATE: u32 = 48_000;
const TARGET_OUTPUT_CHANNELS: u32 = 2;
const MIX_INTERVAL_MS: u64 = 10;
const MIX_CHUNK_FRAMES: usize = 480;
const LOCAL_OUTPUT_QUEUE_TARGET_FRAMES: usize = 1_920;
const BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES: usize = 1_920;
const LOW_VIRTUAL_QUEUE_TARGET_FRAMES: usize = 960;
const ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES: usize = 480;
const OUTPUT_QUEUE_CAPACITY_SAMPLES: usize = TARGET_OUTPUT_SAMPLE_RATE as usize * 2;
const MIC_QUEUE_CAPACITY_SAMPLES: usize = TARGET_OUTPUT_SAMPLE_RATE as usize * 4;
const MAX_LOCAL_OUTPUT_CALLBACK_SAMPLES: usize =
    LOCAL_OUTPUT_QUEUE_TARGET_FRAMES * TARGET_OUTPUT_CHANNELS as usize;
const ULTRA_STARVATION_TICK_FALLBACK_THRESHOLD: u32 = 12;
const AUDIO_COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const PLAY_COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FINISHED_PLAYBACK_SNAPSHOTS: usize = 128;
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const UI_SNAPSHOT_PROGRESS_INTERVAL_MS: u64 = 100;
const CAPTURE_RECREATE_MISS_THRESHOLD: u8 = 2;

thread_local! {
    static OUTPUT_CALLBACK_SCRATCH: RefCell<Vec<f32>> = RefCell::new(Vec::new());
    static CAPTURE_CALLBACK_SCRATCH: RefCell<Vec<f32>> = RefCell::new(Vec::new());
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct PlaybackPosition {
    pub play_id: String,
    pub sound_id: String,
    pub position_ms: u64,
    pub paused: bool,
    pub finished: bool,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AudioSourceInfo {
    pub node_name: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PlayerSnapshot {
    pub available: bool,
    pub playback_positions: Vec<PlaybackPosition>,
    pub playing_ids: Vec<String>,
    pub audio_sources: Vec<AudioSourceInfo>,
    /// The node name of the microphone source currently captured for passthrough,
    /// or `None` if passthrough is off or no suitable source was found yet.
    pub active_capture_target: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioBackendKind {
    PipeWire,
    PulseAudio,
}

#[derive(Debug, Clone)]
struct RuntimeConfig {
    local_volume: f32,
    mic_volume: f32,
    mic_passthrough: bool,
    mic_source: Option<String>,
    default_source_mode: DefaultSourceMode,
    mic_latency_profile: MicLatencyProfile,
    auto_gain: AutoGainState,
    looping: bool,
    audio_backend: AudioBackendKind,
}

impl RuntimeConfig {
    fn from_config(config: &crate::config::Config) -> Self {
        let volume = config.settings.volume_domain();
        let routing = config.settings.mic_routing_domain();
        let playback = config.settings.playback_domain();
        Self {
            local_volume: if volume.local_mute {
                0.0
            } else {
                volume.local_volume as f32 / 100.0
            },
            mic_volume: volume.mic_volume as f32 / 100.0,
            mic_passthrough: routing.mic_passthrough,
            mic_source: routing.mic_source,
            default_source_mode: routing.default_source_mode,
            mic_latency_profile: routing.mic_latency_profile,
            auto_gain: AutoGainState::from_config(config),
            looping: playback.play_mode.should_loop(),
            audio_backend: AudioBackendKind::PipeWire,
        }
    }

    fn latency_tuning(&self) -> LatencyTuning {
        match self.mic_latency_profile {
            MicLatencyProfile::Balanced => LatencyTuning {
                virtual_target_frames: BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES,
                max_virtual_backlog_frames: BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                max_mic_backlog_frames: BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                callback_cap_frames: BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES,
                capture_batch_frames: 192,
                pipewire_latency_hint: "960/48000",
            },
            MicLatencyProfile::Low => LatencyTuning {
                virtual_target_frames: LOW_VIRTUAL_QUEUE_TARGET_FRAMES,
                max_virtual_backlog_frames: LOW_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                max_mic_backlog_frames: LOW_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                callback_cap_frames: LOW_VIRTUAL_QUEUE_TARGET_FRAMES,
                capture_batch_frames: 96,
                pipewire_latency_hint: "480/48000",
            },
            MicLatencyProfile::Ultra => LatencyTuning {
                virtual_target_frames: ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES,
                max_virtual_backlog_frames: ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                max_mic_backlog_frames: ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                callback_cap_frames: ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES,
                capture_batch_frames: 48,
                pipewire_latency_hint: "240/48000",
            },
        }
    }

    fn local_output_target_samples(&self) -> usize {
        LOCAL_OUTPUT_QUEUE_TARGET_FRAMES * TARGET_OUTPUT_CHANNELS as usize
    }

    fn virtual_output_target_samples(&self) -> usize {
        self.latency_tuning().virtual_target_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    fn max_fill_batches_per_tick(
        &self,
        wants_local_output: bool,
        wants_virtual_output: bool,
    ) -> usize {
        let mut target_frames = 0usize;
        if wants_local_output {
            target_frames = target_frames.max(LOCAL_OUTPUT_QUEUE_TARGET_FRAMES);
        }
        if wants_virtual_output {
            target_frames = target_frames.max(self.latency_tuning().virtual_target_frames);
        }

        (target_frames / MIX_CHUNK_FRAMES).max(1)
    }

    fn max_virtual_callback_samples(&self) -> usize {
        self.latency_tuning().callback_cap_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    fn max_virtual_backlog_samples(&self) -> usize {
        self.latency_tuning().max_virtual_backlog_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    fn max_mic_backlog_samples(&self) -> usize {
        self.latency_tuning().max_mic_backlog_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    fn capture_batch_samples(&self) -> usize {
        self.latency_tuning().capture_batch_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    fn pipewire_latency_hint(&self) -> &'static str {
        self.latency_tuning().pipewire_latency_hint
    }
}

#[derive(Debug, Clone, Copy)]
struct LatencyTuning {
    virtual_target_frames: usize,
    max_virtual_backlog_frames: usize,
    max_mic_backlog_frames: usize,
    callback_cap_frames: usize,
    capture_batch_frames: usize,
    pipewire_latency_hint: &'static str,
}

#[derive(Debug)]
struct StreamRuntimeShared {
    max_virtual_callback_samples: AtomicUsize,
    max_virtual_backlog_samples: AtomicUsize,
    max_mic_backlog_samples: AtomicUsize,
    capture_batch_samples: AtomicUsize,
    mic_passthrough: AtomicBool,
    playback_active: AtomicBool,
}

impl StreamRuntimeShared {
    fn new(runtime: &RuntimeConfig) -> Self {
        let tuning = runtime.latency_tuning();
        Self {
            max_virtual_callback_samples: AtomicUsize::new(
                tuning.callback_cap_frames * TARGET_OUTPUT_CHANNELS as usize,
            ),
            max_virtual_backlog_samples: AtomicUsize::new(
                tuning.max_virtual_backlog_frames * TARGET_OUTPUT_CHANNELS as usize,
            ),
            max_mic_backlog_samples: AtomicUsize::new(
                tuning.max_mic_backlog_frames * TARGET_OUTPUT_CHANNELS as usize,
            ),
            capture_batch_samples: AtomicUsize::new(
                tuning.capture_batch_frames * TARGET_OUTPUT_CHANNELS as usize,
            ),
            mic_passthrough: AtomicBool::new(runtime.mic_passthrough),
            playback_active: AtomicBool::new(false),
        }
    }

    fn apply_runtime(&self, runtime: &RuntimeConfig) {
        self.max_virtual_callback_samples
            .store(runtime.max_virtual_callback_samples(), Ordering::Relaxed);
        self.max_virtual_backlog_samples
            .store(runtime.max_virtual_backlog_samples(), Ordering::Relaxed);
        self.max_mic_backlog_samples
            .store(runtime.max_mic_backlog_samples(), Ordering::Relaxed);
        self.capture_batch_samples
            .store(runtime.capture_batch_samples(), Ordering::Relaxed);
        self.mic_passthrough
            .store(runtime.mic_passthrough, Ordering::Relaxed);
    }

    fn set_playback_active(&self, active: bool) {
        self.playback_active.store(active, Ordering::Relaxed);
    }

    fn max_virtual_callback_samples(&self) -> usize {
        self.max_virtual_callback_samples
            .load(Ordering::Relaxed)
            .max(TARGET_OUTPUT_CHANNELS as usize)
    }

    fn max_virtual_backlog_samples(&self) -> usize {
        self.max_virtual_backlog_samples
            .load(Ordering::Relaxed)
            .max(TARGET_OUTPUT_CHANNELS as usize)
    }

    fn max_mic_backlog_samples(&self) -> usize {
        self.max_mic_backlog_samples
            .load(Ordering::Relaxed)
            .max(TARGET_OUTPUT_CHANNELS as usize)
    }

    fn capture_batch_samples(&self) -> usize {
        self.capture_batch_samples
            .load(Ordering::Relaxed)
            .max(TARGET_OUTPUT_CHANNELS as usize)
    }

    fn fast_lane_passthrough_enabled(&self) -> bool {
        self.mic_passthrough.load(Ordering::Relaxed)
            && !self.playback_active.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoGainMode {
    Static,
    DynamicLookAhead,
}

impl AutoGainMode {
    fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::DynamicLookAhead,
            _ => Self::Static,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoGainApplyTo {
    Both,
    MicOnly,
}

impl AutoGainApplyTo {
    fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::MicOnly,
            _ => Self::Both,
        }
    }

    fn applies_to_output(self, is_virtual_output: bool) -> bool {
        match self {
            Self::Both => true,
            Self::MicOnly => is_virtual_output,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AutoGainDynamicParams {
    lookahead_ms: u32,
    attack_ms: u32,
    release_ms: u32,
}

#[derive(Debug, Clone)]
struct AutoGainState {
    enabled: bool,
    mode: AutoGainMode,
    apply_to: AutoGainApplyTo,
    target_lufs: f64,
    dynamic: AutoGainDynamicParams,
}

impl AutoGainState {
    fn from_config(config: &crate::config::Config) -> Self {
        let auto_gain = config.settings.auto_gain_domain();
        Self {
            enabled: auto_gain.enabled,
            mode: AutoGainMode::from_u32(auto_gain.mode.player_value()),
            apply_to: AutoGainApplyTo::from_u32(auto_gain.apply_to.player_value()),
            target_lufs: auto_gain.target_lufs,
            dynamic: AutoGainDynamicParams {
                lookahead_ms: auto_gain.lookahead_ms,
                attack_ms: auto_gain.attack_ms,
                release_ms: auto_gain.release_ms,
            },
        }
    }

    fn gain_for(&self, sound_lufs: Option<f64>, is_virtual_output: bool) -> f32 {
        if !self.enabled || !self.apply_to.applies_to_output(is_virtual_output) {
            return 1.0;
        }
        match sound_lufs {
            Some(lufs) => crate::audio::loudness::compute_gain_factor(lufs, self.target_lufs),
            None => 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceDescriptor {
    id: u32,
    serial: Option<u64>,
    node_name: String,
    display_name: String,
    priority_session: i32,
    is_monitor: bool,
    is_our_virtual_mic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct InputStreamDescriptor {
    id: u32,
    node_name: String,
    app_name: Option<String>,
    app_id: Option<String>,
    app_process_binary: Option<String>,
    media_name: Option<String>,
    media_role: Option<String>,
    target_object: Option<String>,
    dont_move: bool,
    stream_capture_sink: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkDescriptor {
    id: u32,
    output_node_id: u32,
    input_node_id: u32,
    output_port_id: Option<u32>,
    input_port_id: Option<u32>,
}

#[derive(Clone, Debug)]
struct PlaybackSnapshot {
    sound_id: String,
    playback_order: u64,
    position_ms: u64,
    paused: bool,
    duration_ms: Option<u64>,
    finished: bool,
}

fn build_playback_positions(registry: &HashMap<String, PlaybackSnapshot>) -> Vec<PlaybackPosition> {
    let mut ordered = registry
        .iter()
        .map(|(play_id, snap)| {
            (
                snap.playback_order,
                PlaybackPosition {
                    play_id: play_id.clone(),
                    sound_id: snap.sound_id.clone(),
                    position_ms: snap.position_ms,
                    paused: snap.paused,
                    finished: snap.finished,
                    duration_ms: snap.duration_ms,
                },
            )
        })
        .collect::<Vec<_>>();

    ordered.sort_by(|(left_order, left), (right_order, right)| {
        left.finished
            .cmp(&right.finished)
            .then_with(|| right_order.cmp(left_order))
    });

    ordered.into_iter().map(|(_, position)| position).collect()
}

enum AudioCommand {
    Play {
        sound_id: String,
        path: String,
        base_volume: f32,
        sound_lufs: Option<f64>,
        response: Sender<Result<String, String>>,
    },
    StopSound {
        sound_id: String,
    },
    StopAll,
    Seek {
        play_id: String,
        position_ms: u64,
    },
    Pause {
        sound_id: String,
    },
    Resume {
        sound_id: String,
    },
    SetLocalVolume {
        volume: f32,
    },
    SetMicVolume {
        volume: f32,
    },
    SetAutoGainEnabled {
        enabled: bool,
    },
    SetAutoGainTarget {
        target_lufs: f64,
    },
    SetAutoGainMode {
        mode: u32,
    },
    SetAutoGainApplyTo {
        apply_to: u32,
    },
    SetAutoGainDynamicSettings {
        lookahead_ms: u32,
        attack_ms: u32,
        release_ms: u32,
    },
    SetLooping {
        enabled: bool,
    },
    SetMicPassthrough {
        enabled: bool,
        response: Sender<Result<(), String>>,
    },
    SetMicSource {
        source: Option<String>,
        response: Sender<Result<(), String>>,
    },
    SetDefaultSourceMode {
        mode: DefaultSourceMode,
        response: Sender<Result<(), String>>,
    },
    SetMicLatencyProfile {
        profile: MicLatencyProfile,
        response: Sender<Result<(), String>>,
    },
    Shutdown,
}

enum AudioPlayerBackend {
    Local(LocalAudioPlayer),
    Remote(RemoteAudioPlayer),
}

struct LocalAudioPlayer {
    command_tx: pw_channel::Sender<AudioCommand>,
    join_handle: Mutex<Option<thread::JoinHandle<()>>>,
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
}

struct RemoteAudioPlayer {
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
    stop_poll: std::sync::Arc<AtomicBool>,
    poll_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

pub struct AudioPlayer {
    backend: AudioPlayerBackend,
}

impl AudioPlayer {
    pub fn connect_to_engine() -> Option<Self> {
        let info = match crate::audio::engine_ipc::engine_info() {
            Ok(info) => info,
            Err(err) => {
                if crate::audio::engine_ipc::engine_running() {
                    warn!("Refusing to use incompatible Linux Soundboard audio engine: {err}");
                }
                return None;
            }
        };
        if !crate::audio::engine_ipc::engine_info_compatible(&info) {
            warn!(
                "Refusing to use incompatible Linux Soundboard audio engine: protocol={} schema={} binary={}",
                info.engine_protocol_version, info.config_schema_version, info.binary_path
            );
            return None;
        }

        let snapshot = std::sync::Arc::new(RwLock::new(PlayerSnapshot::default()));
        if let Ok(crate::audio::engine_ipc::EngineResponse::Snapshot { snapshot: initial }) =
            crate::audio::engine_ipc::send_request(
                crate::audio::engine_ipc::EngineRequest::Snapshot,
            )
        {
            if let Ok(mut guard) = snapshot.write() {
                *guard = initial;
            }
        }

        let stop_poll = std::sync::Arc::new(AtomicBool::new(false));
        let poll_snapshot = snapshot.clone();
        let poll_stop = stop_poll.clone();
        let poll_handle = thread::Builder::new()
            .name("lsb-engine-snapshot-poll".to_string())
            .spawn(move || {
                while !poll_stop.load(Ordering::Relaxed) {
                    if let Ok(crate::audio::engine_ipc::EngineResponse::Snapshot { snapshot }) =
                        crate::audio::engine_ipc::send_request(
                            crate::audio::engine_ipc::EngineRequest::Snapshot,
                        )
                    {
                        if let Ok(mut guard) = poll_snapshot.write() {
                            *guard = snapshot.clone();
                        }
                        glib::MainContext::default().invoke(move || {
                            crate::playback_bridge::dispatch_snapshot(snapshot);
                        });
                    }
                    thread::sleep(Duration::from_millis(UI_SNAPSHOT_PROGRESS_INTERVAL_MS));
                }
            })
            .ok();

        Some(Self {
            backend: AudioPlayerBackend::Remote(RemoteAudioPlayer {
                snapshot,
                stop_poll,
                poll_handle: Mutex::new(poll_handle),
            }),
        })
    }

    pub fn new_with_config(config: &crate::config::Config) -> Self {
        Self::new_with_config_and_audio_backend(config, AudioBackendKind::PipeWire)
    }

    pub fn new_with_config_and_audio_backend(
        config: &crate::config::Config,
        audio_backend: AudioBackendKind,
    ) -> Self {
        let (command_tx, command_rx) = pw_channel::channel();
        let mut runtime = RuntimeConfig::from_config(config);
        runtime.audio_backend = audio_backend;
        let snapshot = std::sync::Arc::new(RwLock::new(PlayerSnapshot::default()));
        let thread_snapshot = snapshot.clone();
        let handle =
            thread::spawn(move || pipewire_thread_main(command_rx, runtime, thread_snapshot));

        Self {
            backend: AudioPlayerBackend::Local(LocalAudioPlayer {
                command_tx,
                join_handle: Mutex::new(Some(handle)),
                snapshot,
            }),
        }
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        match &self.backend {
            AudioPlayerBackend::Local(local) => local
                .snapshot
                .read()
                .map(|snapshot| snapshot.clone())
                .unwrap_or_default(),
            AudioPlayerBackend::Remote(remote) => remote
                .snapshot
                .read()
                .map(|snapshot| snapshot.clone())
                .unwrap_or_default(),
        }
    }

    pub fn set_local_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local
                    .command_tx
                    .send(AudioCommand::SetLocalVolume { volume });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ =
                    remote_ok(crate::audio::engine_ipc::EngineRequest::SetLocalVolume { volume });
            }
        }
    }

    pub fn set_mic_volume(&self, volume: f32) {
        let volume = volume.clamp(0.0, 1.0);
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local.command_tx.send(AudioCommand::SetMicVolume { volume });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(crate::audio::engine_ipc::EngineRequest::SetMicVolume { volume });
            }
        }
    }

    pub fn set_auto_gain_enabled(&self, enabled: bool) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local
                    .command_tx
                    .send(AudioCommand::SetAutoGainEnabled { enabled });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(
                    crate::audio::engine_ipc::EngineRequest::SetAutoGainEnabled { enabled },
                );
            }
        }
    }

    pub fn set_auto_gain_target(&self, target_lufs: f64) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local
                    .command_tx
                    .send(AudioCommand::SetAutoGainTarget { target_lufs });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(crate::audio::engine_ipc::EngineRequest::SetAutoGainTarget {
                    target_lufs,
                });
            }
        }
    }

    pub fn set_auto_gain_mode(&self, mode: u32) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local
                    .command_tx
                    .send(AudioCommand::SetAutoGainMode { mode });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ =
                    remote_ok(crate::audio::engine_ipc::EngineRequest::SetAutoGainMode { mode });
            }
        }
    }

    pub fn set_auto_gain_apply_to(&self, apply_to: u32) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local
                    .command_tx
                    .send(AudioCommand::SetAutoGainApplyTo { apply_to });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(
                    crate::audio::engine_ipc::EngineRequest::SetAutoGainApplyTo { apply_to },
                );
            }
        }
    }

    pub fn set_auto_gain_dynamic_settings(
        &self,
        lookahead_ms: u32,
        attack_ms: u32,
        release_ms: u32,
    ) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local
                    .command_tx
                    .send(AudioCommand::SetAutoGainDynamicSettings {
                        lookahead_ms,
                        attack_ms,
                        release_ms,
                    });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(
                    crate::audio::engine_ipc::EngineRequest::SetAutoGainDynamicSettings {
                        lookahead_ms,
                        attack_ms,
                        release_ms,
                    },
                );
            }
        }
    }

    pub fn set_looping(&self, enabled: bool) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local.command_tx.send(AudioCommand::SetLooping { enabled });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(crate::audio::engine_ipc::EngineRequest::SetLooping { enabled });
            }
        }
    }

    pub fn set_mic_passthrough(&self, enabled: bool) -> Result<(), String> {
        let AudioPlayerBackend::Local(local) = &self.backend else {
            return remote_ok(crate::audio::engine_ipc::EngineRequest::SetMicPassthrough {
                enabled,
            });
        };
        let (tx, rx) = mpsc::channel();
        local
            .command_tx
            .send(AudioCommand::SetMicPassthrough {
                enabled,
                response: tx,
            })
            .map_err(|_| "Audio backend thread is not running".to_string())?;
        match rx.recv_timeout(AUDIO_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "Audio backend timed out while handling SetMicPassthrough after {} ms",
                AUDIO_COMMAND_RESPONSE_TIMEOUT.as_millis()
            )),
            Err(RecvTimeoutError::Disconnected) => {
                Err("Audio backend response channel closed".to_string())
            }
        }
    }

    pub fn set_mic_source(&self, source: Option<String>) -> Result<(), String> {
        let AudioPlayerBackend::Local(local) = &self.backend else {
            return remote_ok(crate::audio::engine_ipc::EngineRequest::SetMicSource { source });
        };
        let (tx, rx) = mpsc::channel();
        local
            .command_tx
            .send(AudioCommand::SetMicSource {
                source,
                response: tx,
            })
            .map_err(|_| "Audio backend thread is not running".to_string())?;
        match rx.recv_timeout(AUDIO_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "Audio backend timed out while handling SetMicSource after {} ms",
                AUDIO_COMMAND_RESPONSE_TIMEOUT.as_millis()
            )),
            Err(RecvTimeoutError::Disconnected) => {
                Err("Audio backend response channel closed".to_string())
            }
        }
    }

    pub fn set_default_source_mode(&self, mode: DefaultSourceMode) -> Result<(), String> {
        let AudioPlayerBackend::Local(local) = &self.backend else {
            return remote_ok(
                crate::audio::engine_ipc::EngineRequest::SetDefaultSourceMode { mode },
            );
        };
        let (tx, rx) = mpsc::channel();
        local
            .command_tx
            .send(AudioCommand::SetDefaultSourceMode { mode, response: tx })
            .map_err(|_| "Audio backend thread is not running".to_string())?;
        match rx.recv_timeout(AUDIO_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "Audio backend timed out while handling SetDefaultSourceMode after {} ms",
                AUDIO_COMMAND_RESPONSE_TIMEOUT.as_millis()
            )),
            Err(RecvTimeoutError::Disconnected) => {
                Err("Audio backend response channel closed".to_string())
            }
        }
    }

    pub fn set_mic_latency_profile(&self, profile: MicLatencyProfile) -> Result<(), String> {
        let AudioPlayerBackend::Local(local) = &self.backend else {
            return remote_ok(
                crate::audio::engine_ipc::EngineRequest::SetMicLatencyProfile { profile },
            );
        };
        let (tx, rx) = mpsc::channel();
        local
            .command_tx
            .send(AudioCommand::SetMicLatencyProfile {
                profile,
                response: tx,
            })
            .map_err(|_| "Audio backend thread is not running".to_string())?;
        match rx.recv_timeout(AUDIO_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(format!(
                "Audio backend timed out while handling SetMicLatencyProfile after {} ms",
                AUDIO_COMMAND_RESPONSE_TIMEOUT.as_millis()
            )),
            Err(RecvTimeoutError::Disconnected) => {
                Err("Audio backend response channel closed".to_string())
            }
        }
    }

    pub fn list_audio_sources(&self) -> Vec<AudioSourceInfo> {
        self.snapshot().audio_sources
    }

    pub fn active_capture_target(&self) -> Option<String> {
        self.snapshot().active_capture_target
    }

    pub fn play(
        &self,
        sound_id: &str,
        path: &str,
        base_volume: f32,
        sound_lufs: Option<f64>,
    ) -> Result<String, String> {
        if matches!(self.backend, AudioPlayerBackend::Remote(_)) {
            return match crate::audio::engine_ipc::send_request(
                crate::audio::engine_ipc::EngineRequest::Play {
                    sound_id: sound_id.to_string(),
                    path: path.to_string(),
                    base_volume,
                    sound_lufs,
                },
            )? {
                crate::audio::engine_ipc::EngineResponse::PlayId { play_id } => Ok(play_id),
                crate::audio::engine_ipc::EngineResponse::Error { message } => Err(message),
                other => Err(format!("Unexpected engine response to Play: {other:?}")),
            };
        }
        let AudioPlayerBackend::Local(local) = &self.backend else {
            return Err("Remote audio player unavailable".to_string());
        };
        let (response_tx, response_rx) = mpsc::channel();
        debug!(
            "Submitting Play command: sound_id={} path={}",
            sound_id, path
        );
        let enqueue_started_at = Instant::now();
        local
            .command_tx
            .send(AudioCommand::Play {
                sound_id: sound_id.to_string(),
                path: path.to_string(),
                base_volume,
                sound_lufs,
                response: response_tx,
            })
            .map_err(|_| "Audio backend thread is not running".to_string())?;
        let enqueue_elapsed_ms = enqueue_started_at.elapsed().as_millis();
        if enqueue_elapsed_ms >= 50 {
            debug!(
                "Play command enqueue was slow: sound_id={} elapsed_ms={}",
                sound_id, enqueue_elapsed_ms
            );
        }

        let wait_started_at = Instant::now();
        match response_rx.recv_timeout(PLAY_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => {
                let wait_elapsed_ms = wait_started_at.elapsed().as_millis();
                if wait_elapsed_ms >= 100 {
                    debug!(
                        "Play command response received: sound_id={} elapsed_ms={}",
                        sound_id, wait_elapsed_ms
                    );
                }
                result
            }
            Err(RecvTimeoutError::Timeout) => {
                warn!(
                    "Play command timed out waiting for backend: sound_id={} timeout_ms={}",
                    sound_id,
                    PLAY_COMMAND_RESPONSE_TIMEOUT.as_millis()
                );
                Err(format!(
                    "Audio backend timed out while handling Play after {} ms",
                    PLAY_COMMAND_RESPONSE_TIMEOUT.as_millis()
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                Err("Audio backend response channel closed".to_string())
            }
        }
    }

    pub fn stop_sound(&self, sound_id: &str) -> Result<(), String> {
        match &self.backend {
            AudioPlayerBackend::Local(local) => local
                .command_tx
                .send(AudioCommand::StopSound {
                    sound_id: sound_id.to_string(),
                })
                .map_err(|_| "Audio backend thread is not running".to_string()),
            AudioPlayerBackend::Remote(_) => {
                remote_ok(crate::audio::engine_ipc::EngineRequest::StopSound {
                    sound_id: sound_id.to_string(),
                })
            }
        }
    }

    /// Stop all active sounds and immediately start a new one.
    /// For the remote backend this is a single atomic IPC request, so no
    /// snapshot poll can observe the transient "all stopped" state between the
    /// two operations.
    pub fn play_replace(
        &self,
        sound_id: &str,
        path: &str,
        base_volume: f32,
        sound_lufs: Option<f64>,
    ) -> Result<String, String> {
        if matches!(self.backend, AudioPlayerBackend::Remote(_)) {
            return match crate::audio::engine_ipc::send_request(
                crate::audio::engine_ipc::EngineRequest::PlayReplace {
                    sound_id: sound_id.to_string(),
                    path: path.to_string(),
                    base_volume,
                    sound_lufs,
                },
            )? {
                crate::audio::engine_ipc::EngineResponse::PlayId { play_id } => Ok(play_id),
                crate::audio::engine_ipc::EngineResponse::Error { message } => Err(message),
                other => Err(format!(
                    "Unexpected engine response to PlayReplace: {other:?}"
                )),
            };
        }
        // Local backend: stop_all + play via the command channel (in-process, no IPC race).
        self.stop_all();
        self.play(sound_id, path, base_volume, sound_lufs)
    }

    pub fn stop_all(&self) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local.command_tx.send(AudioCommand::StopAll);
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(crate::audio::engine_ipc::EngineRequest::StopAll);
            }
        }
    }

    pub fn seek_playback(&self, play_id: &str, position_ms: u64) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local.command_tx.send(AudioCommand::Seek {
                    play_id: play_id.to_string(),
                    position_ms,
                });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(crate::audio::engine_ipc::EngineRequest::Seek {
                    play_id: play_id.to_string(),
                    position_ms,
                });
            }
        }
    }

    pub fn pause(&self, sound_id: &str) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local.command_tx.send(AudioCommand::Pause {
                    sound_id: sound_id.to_string(),
                });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(crate::audio::engine_ipc::EngineRequest::Pause {
                    sound_id: sound_id.to_string(),
                });
            }
        }
    }

    pub fn resume(&self, sound_id: &str) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local.command_tx.send(AudioCommand::Resume {
                    sound_id: sound_id.to_string(),
                });
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(crate::audio::engine_ipc::EngineRequest::Resume {
                    sound_id: sound_id.to_string(),
                });
            }
        }
    }

    pub fn get_playing(&self) -> Vec<String> {
        self.snapshot().playing_ids
    }

    pub fn is_available(&self) -> bool {
        self.snapshot().available
    }

    pub fn get_playback_positions(&self) -> Vec<PlaybackPosition> {
        self.snapshot().playback_positions
    }

    pub fn shutdown(&self) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local.command_tx.send(AudioCommand::Shutdown);
                if let Ok(mut handle) = local.join_handle.lock() {
                    if let Some(handle) = handle.take() {
                        let (done_tx, done_rx) = mpsc::channel();
                        thread::spawn(move || {
                            let _ = handle.join();
                            let _ = done_tx.send(());
                        });

                        if done_rx.recv_timeout(SHUTDOWN_JOIN_TIMEOUT).is_err() {
                            warn!(
                                "Audio backend thread did not shut down within {} ms",
                                SHUTDOWN_JOIN_TIMEOUT.as_millis()
                            );
                        }
                    }
                }
            }
            AudioPlayerBackend::Remote(remote) => {
                remote.stop_poll.store(true, Ordering::Relaxed);
                if let Ok(mut handle) = remote.poll_handle.lock() {
                    if let Some(handle) = handle.take() {
                        let _ = handle.join();
                    }
                }
            }
        }
    }
}

fn remote_ok(request: crate::audio::engine_ipc::EngineRequest) -> Result<(), String> {
    match crate::audio::engine_ipc::send_request(request)? {
        crate::audio::engine_ipc::EngineResponse::Ok => Ok(()),
        crate::audio::engine_ipc::EngineResponse::Error { message } => Err(message),
        other => Err(format!("Unexpected engine response: {other:?}")),
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ManagedStreamState {
    Error,
    Unconnected,
    Connecting,
    Paused,
    Streaming,
}

impl ManagedStreamState {
    fn from_pipewire(state: pw::stream::StreamState) -> Self {
        match state {
            pw::stream::StreamState::Error(_) => Self::Error,
            pw::stream::StreamState::Unconnected => Self::Unconnected,
            pw::stream::StreamState::Connecting => Self::Connecting,
            pw::stream::StreamState::Paused => Self::Paused,
            pw::stream::StreamState::Streaming => Self::Streaming,
        }
    }
}

struct StreamHandle {
    _stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<()>,
    state: Rc<RefCell<ManagedStreamState>>,
}

impl StreamHandle {
    fn new(
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

    fn current_state(&self) -> ManagedStreamState {
        *self.state.borrow()
    }

    fn node_id(&self) -> u32 {
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

struct PipeWireBackendState {
    _context: pw::context::ContextRc,
    core: pw::core::CoreRc,
    _registry: pw::registry::RegistryRc,
    _registry_listener: pw::registry::Listener,
    _local_stream: Option<StreamHandle>,
    virtual_stream: Option<StreamHandle>,
    capture_stream: Option<StreamHandle>,
}

enum BackendState {
    PipeWire(PipeWireBackendState),
    PulseAudio(PulseAudioBackend),
}

impl BackendState {
    fn pipewire_core(&self) -> Option<pw::core::CoreRc> {
        match self {
            Self::PipeWire(backend) => Some(backend.core.clone()),
            Self::PulseAudio(_) => None,
        }
    }

    fn playback_stream_active(&self) -> bool {
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

struct LoopState {
    runtime: RuntimeConfig,
    available: bool,
    default_metadata: Option<DefaultMetadataHandle>,
    backend: Option<BackendState>,
    sources: HashMap<u32, SourceDescriptor>,
    input_streams: HashMap<u32, InputStreamDescriptor>,
    input_stream_metadata_targets: HashMap<u32, String>,
    input_stream_handles: HashMap<u32, InputStreamNodeHandle>,
    autorouted_input_streams: HashMap<u32, AutoroutedInputStream>,
    autoroute_blocked_input_streams: HashSet<u32>,
    default_audio_source_name: Option<String>,
    virtual_mic_state_reset_ids: HashSet<u32>,
    virtual_mic_node_id: Option<u32>,
    virtual_mic_input_ports: HashMap<AudioChannel, u32>,
    feeder_node_id: Option<u32>,
    feeder_output_ports: HashMap<AudioChannel, u32>,
    feeder_links: HashMap<AudioChannel, pw::link::Link>,
    tracked_ports: HashMap<u32, TrackedPort>,
    links: HashMap<u32, LinkDescriptor>,
    capture_node_id: Option<u32>,
    active_capture_target: Option<String>,
    capture_health_miss_ticks: u8,
    previous_default_source_name: Option<String>,
    claimed_default: bool,
    default_source_command_in_flight: std::sync::Arc<AtomicBool>,
    active_playback: Option<ActivePlayback>,
    finished_playbacks: HashMap<String, PlaybackSnapshot>,
    next_playback_order: u64,
    queues: std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
    ultra_starvation_ticks: u32,
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
    last_ui_send: Option<Instant>,
    last_had_active: bool,
    local_mix_buffer: Vec<f32>,
    virtual_mix_buffer: Vec<f32>,
}

impl LoopState {
    fn new(runtime: RuntimeConfig, snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>) -> Self {
        let stream_runtime = std::sync::Arc::new(StreamRuntimeShared::new(&runtime));
        Self {
            runtime,
            available: false,
            default_metadata: None,
            backend: None,
            sources: HashMap::new(),
            input_streams: HashMap::new(),
            input_stream_metadata_targets: HashMap::new(),
            input_stream_handles: HashMap::new(),
            autorouted_input_streams: HashMap::new(),
            autoroute_blocked_input_streams: HashSet::new(),
            default_audio_source_name: None,
            virtual_mic_state_reset_ids: HashSet::new(),
            virtual_mic_node_id: None,
            virtual_mic_input_ports: HashMap::new(),
            feeder_node_id: None,
            feeder_output_ports: HashMap::new(),
            feeder_links: HashMap::new(),
            tracked_ports: HashMap::new(),
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
            queues: std::sync::Arc::new(std::sync::Mutex::new(ProcessQueues::new(
                OUTPUT_QUEUE_CAPACITY_SAMPLES,
                OUTPUT_QUEUE_CAPACITY_SAMPLES,
                MIC_QUEUE_CAPACITY_SAMPLES,
            ))),
            stream_runtime,
            ultra_starvation_ticks: 0,
            snapshot,
            last_ui_send: None,
            last_had_active: false,
            local_mix_buffer: Vec::new(),
            virtual_mix_buffer: Vec::new(),
        }
    }

    fn snapshot_positions(&self) -> Vec<PlaybackPosition> {
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

    fn list_audio_sources(&self) -> Vec<AudioSourceInfo> {
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
            })
            .collect()
    }

    fn playing_ids(&self) -> Vec<String> {
        self.active_playback
            .as_ref()
            .filter(|playback| !playback.finished)
            .map(|playback| vec![playback.sound_id.clone()])
            .unwrap_or_default()
    }

    fn trim_finished_playbacks(&mut self, max_entries: usize) {
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

    fn publish_snapshot(&mut self) {
        self.stream_runtime
            .set_playback_active(self.active_playback.is_some());
        let new_snapshot = PlayerSnapshot {
            available: self.available,
            playback_positions: self.snapshot_positions(),
            playing_ids: self.playing_ids(),
            audio_sources: self.list_audio_sources(),
            active_capture_target: self.active_capture_target.clone(),
        };

        if let Ok(mut guard) = self.snapshot.write() {
            *guard = new_snapshot.clone();
        }

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
                crate::playback_bridge::dispatch_snapshot(new_snapshot);
            });
        }
    }

    fn backend_playback_available(&self) -> bool {
        self.backend
            .as_ref()
            .is_some_and(BackendState::playback_stream_active)
    }
}

fn pipewire_thread_main(
    command_rx: pw_channel::Receiver<AudioCommand>,
    runtime: RuntimeConfig,
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
) {
    pw::init();

    let Ok(mainloop) = pw::main_loop::MainLoopRc::new(None) else {
        error!("Failed to create PipeWire main loop");
        return;
    };

    let state = Rc::new(RefCell::new(LoopState::new(runtime, snapshot)));
    {
        let weak = Rc::downgrade(&state);
        if let Ok(mut state_ref) = state.try_borrow_mut() {
            match create_backend(
                weak.clone(),
                mainloop.clone(),
                state_ref.queues.clone(),
                state_ref.runtime.clone(),
                state_ref.stream_runtime.clone(),
            ) {
                Ok(backend) => {
                    state_ref.available = backend.playback_stream_active();
                    state_ref.backend = Some(backend);
                    let _ = recreate_capture_stream(&mut state_ref);
                }
                Err(err) => {
                    warn!("PipeWire backend unavailable: {}", err);
                }
            }
            state_ref.publish_snapshot();
        }

        let attached_receiver = command_rx.attach(mainloop.loop_(), {
            let mainloop = mainloop.clone();
            let weak = weak.clone();
            move |cmd| {
                if let Some(state_rc) = weak.upgrade() {
                    let command_kind = audio_command_kind(&cmd);
                    if matches!(&cmd, AudioCommand::Play { .. }) {
                        debug!("Audio command received: kind=Play");
                    }
                    let started_at = Instant::now();
                    let should_quit = handle_audio_command(&mainloop, &state_rc, cmd);
                    let elapsed_ms = started_at.elapsed().as_millis();
                    if elapsed_ms >= 100 {
                        warn!(
                            "Audio command handling was slow: kind={} elapsed_ms={}",
                            command_kind, elapsed_ms
                        );
                    }
                    if should_quit {
                        mainloop.quit();
                    }
                }
            }
        });

        let mix_timer = mainloop.loop_().add_timer({
            let weak = weak.clone();
            move |_| {
                if let Some(state_rc) = weak.upgrade() {
                    mix_tick(&state_rc);
                }
            }
        });
        let _ = mix_timer.update_timer(
            Some(Duration::from_millis(1)),
            Some(Duration::from_millis(MIX_INTERVAL_MS)),
        );

        let graph_watchdog_timer = mainloop.loop_().add_timer({
            let weak = weak.clone();
            move |_| {
                if let Some(state_rc) = weak.upgrade() {
                    let mut state = state_rc.borrow_mut();
                    ensure_capture_stream_present(&mut state);
                    state.publish_snapshot();
                }
            }
        });
        let _ = graph_watchdog_timer.update_timer(
            Some(Duration::from_millis(250)),
            Some(Duration::from_secs(1)),
        );

        let _keep_alive = (attached_receiver, mix_timer, graph_watchdog_timer);
        mainloop.run();
    }
}

fn create_backend(
    weak_state: Weak<RefCell<LoopState>>,
    mainloop: pw::main_loop::MainLoopRc,
    queues: std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    runtime: RuntimeConfig,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
) -> Result<BackendState, String> {
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
    queues: std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    runtime: RuntimeConfig,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
) -> Result<PipeWireBackendState, String> {
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context.connect_rc(None).map_err(|e| e.to_string())?;
    let registry = core.get_registry_rc().map_err(|e| e.to_string())?;
    let registry_for_global = registry.clone();

    let registry_listener = registry
        .add_listener_local()
        .global({
            let weak_state = weak_state.clone();
            move |global| {
                let is_link_node = global.type_ == pw::types::ObjectType::Node;
                let is_link_port = global.type_ == pw::types::ObjectType::Port;
                let link = link_from_global(global);
                let capture_node_id = capture_node_id_from_global(global);
                let source = source_from_global(global);
                let input_stream = bind_input_stream_node_from_global(
                    &registry_for_global,
                    global,
                    weak_state.clone(),
                );
                let metadata = bind_default_metadata_from_global(
                    &registry_for_global,
                    global,
                    weak_state.clone(),
                );
                if !is_link_node
                    && !is_link_port
                    && source.is_none()
                    && input_stream.is_none()
                    && metadata.is_none()
                    && link.is_none()
                    && capture_node_id.is_none()
                {
                    return;
                }
                if let Some(state) = weak_state.upgrade() {
                    let mut state = state.borrow_mut();
                    if let Some(metadata) = metadata {
                        state.default_metadata = Some(metadata);
                        maybe_autoroute_input_streams(&mut state);
                    }
                    if let Some(capture_node_id) = capture_node_id {
                        state.capture_node_id = Some(capture_node_id);
                    }
                    if is_link_node {
                        track_node_global(&mut state, global);
                    }
                    if is_link_port {
                        track_port_global(&mut state, global);
                    }
                    if let Some(link) = link {
                        state.links.insert(link.id, link);
                    }
                    if let Some(source) = source {
                        if source.is_our_virtual_mic
                            && state.virtual_mic_state_reset_ids.insert(source.id)
                        {
                            spawn_virtual_mic_state_reset(source.id);
                        }
                        state.sources.insert(source.id, source);
                        maybe_claim_default_source(&mut state);
                        maybe_autoroute_input_streams(&mut state);
                        // A new source appeared — if mic passthrough is on but
                        // we haven't been able to attach yet (startup race), or
                        // the user's preferred source just came online, retry.
                        ensure_capture_stream_present(&mut state);
                    }
                    if let Some((mut input_stream, input_stream_handle)) = input_stream {
                        let stream_id = input_stream.id;
                        if let Some(target_object) =
                            state.input_stream_metadata_targets.get(&stream_id).cloned()
                        {
                            input_stream.target_object = Some(target_object);
                        }
                        state.input_streams.insert(stream_id, input_stream);
                        state
                            .input_stream_handles
                            .insert(stream_id, input_stream_handle);
                        maybe_autoroute_input_streams(&mut state);
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
                    state.virtual_mic_state_reset_ids.remove(&id);
                    if state
                        .default_metadata
                        .as_ref()
                        .is_some_and(|metadata| metadata.id == id)
                    {
                        state.default_metadata = None;
                        state.autorouted_input_streams.clear();
                        state.autoroute_blocked_input_streams.clear();
                        state.input_stream_metadata_targets.clear();
                    }
                    state.input_streams.remove(&id);
                    state.input_stream_metadata_targets.remove(&id);
                    state.input_stream_handles.remove(&id);
                    state.autorouted_input_streams.remove(&id);
                    state.autoroute_blocked_input_streams.remove(&id);
                    state.links.remove(&id);
                    if state.capture_node_id == Some(id) {
                        state.capture_node_id = None;
                    }
                    if removed_source_name.as_deref() == Some(VIRTUAL_SOURCE_NAME) {
                        clear_autorouted_input_streams(&mut state);
                    }
                    handle_link_global_remove(&mut state, id);
                    if removed_source && state.runtime.mic_passthrough {
                        // The source we may have been capturing from went
                        // away (e.g. EasyEffects quit). Re-resolve so we
                        // either pick a new target or release the dangling
                        // stream cleanly.
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

    let local_stream =
        create_local_output_stream(core.clone(), queues.clone(), stream_runtime.clone()).ok();
    debug!("Creating runtime in-process virtual mic source");
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
    })
}

fn source_from_global(
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

    Some(SourceDescriptor {
        id: global.id,
        serial,
        is_monitor: node_name.ends_with(".monitor"),
        is_our_virtual_mic: node_name == VIRTUAL_SOURCE_NAME,
        priority_session,
        node_name,
        display_name,
    })
}

fn spawn_virtual_mic_state_reset(source_id: u32) {
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

fn capture_node_id_from_global(
    global: &pw::registry::GlobalObject<&spa::utils::dict::DictRef>,
) -> Option<u32> {
    if global.type_ != pw::types::ObjectType::Node {
        return None;
    }
    let props = global.props?;
    (props.get(*pw::keys::NODE_NAME)? == MIC_CAPTURE_NODE_NAME).then_some(global.id)
}

fn link_from_global(
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

/// Idempotently (re)creates the feeder stream so the soundboard keeps
/// reaching `Linux Soundboard Mic` after a PipeWire restart.
fn capture_stream_missing(state: &LoopState) -> bool {
    match state.backend.as_ref() {
        Some(BackendState::PipeWire(backend)) => backend.capture_stream.is_none(),
        Some(BackendState::PulseAudio(backend)) => !backend.capture_stream_active(),
        None => false,
    }
}

/// Capture-stream watchdog for the mic passthrough capture stream. Fixes two
/// real-world races:
///   1. Soundboard launches before the registry reports physical mics, so the
///      initial `recreate_capture_stream` finds nothing and warns "No physical
///      microphone source available for passthrough" — before this watchdog
///      the user had to toggle passthrough off/on to recover.
///   2. The user's preferred mic source (e.g. EasyEffects) isn't running yet
///      when the soundboard starts; when it appears later we now wire it up
///      automatically.
fn ensure_capture_stream_present(state: &mut LoopState) {
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
        return;
    }

    // Link health can be temporarily wrong while WirePlumber rewires nodes.
    // Leaving the stream in place avoids clearing soundboard playback queues
    // and disables passthrough contribution until a valid link appears.
}

fn pipewire_capture_stream_healthy(state: &LoopState, expected_target: Option<&str>) -> bool {
    let Some(expected_target) = expected_target else {
        return false;
    };
    if state.active_capture_target.as_deref() != Some(expected_target) {
        return false;
    }
    pipewire_capture_stream_linked_to_active_target(state)
}

fn active_capture_target_if_available(state: &LoopState) -> Option<String> {
    let target = state.active_capture_target.as_deref()?;
    resolve_source_id_by_name(&state.sources, target).map(|_| target.to_string())
}

fn pipewire_capture_stream_linked_to_active_target(state: &LoopState) -> bool {
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

fn pipewire_capture_stream_failed(state: &LoopState) -> bool {
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

fn pipewire_capture_link_healthy(
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
    fn capture_stream_active(&self) -> bool {
        match self.backend.as_ref() {
            Some(BackendState::PipeWire(_)) => {
                pipewire_capture_stream_linked_to_active_target(self)
            }
            Some(BackendState::PulseAudio(backend)) => backend.capture_stream_active(),
            None => false,
        }
    }
}

fn clamp_seek_position_ms(position_ms: u64, duration_ms: Option<u64>) -> u64 {
    match duration_ms {
        Some(duration_ms) => position_ms.min(duration_ms),
        None => position_ms,
    }
}

enum PlaybackSource {
    Symphonia(SymphoniaSource),
    OggOpus(OggOpusSource),
}

impl PlaybackSource {
    fn from_path(path: &str) -> Result<Self, String> {
        if OggOpusSource::looks_like_ogg_opus(path) {
            return OggOpusSource::from_path(path).map(Self::OggOpus);
        }

        SymphoniaSource::from_path(path).map(Self::Symphonia)
    }
}

impl Iterator for PlaybackSource {
    type Item = i16;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Symphonia(source) => source.next(),
            Self::OggOpus(source) => source.next(),
        }
    }
}

impl Source for PlaybackSource {
    fn current_frame_len(&self) -> Option<usize> {
        match self {
            Self::Symphonia(source) => source.current_frame_len(),
            Self::OggOpus(source) => source.current_frame_len(),
        }
    }

    fn channels(&self) -> u16 {
        match self {
            Self::Symphonia(source) => source.channels(),
            Self::OggOpus(source) => source.channels(),
        }
    }

    fn sample_rate(&self) -> u32 {
        match self {
            Self::Symphonia(source) => source.sample_rate(),
            Self::OggOpus(source) => source.sample_rate(),
        }
    }

    fn total_duration(&self) -> Option<Duration> {
        match self {
            Self::Symphonia(source) => source.total_duration(),
            Self::OggOpus(source) => source.total_duration(),
        }
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), RodioSeekError> {
        match self {
            Self::Symphonia(source) => source.try_seek(position),
            Self::OggOpus(source) => source.try_seek(position),
        }
    }
}

impl ResettableSource for PlaybackSource {
    fn seek_resettable(&mut self, position: Duration) -> Result<(), RodioSeekError> {
        self.try_seek(position)
    }
}

const OPUS_SAMPLE_RATE: u32 = 48_000;
const OPUS_MAX_FRAME_SAMPLES_PER_CHANNEL: usize = 5_760;

struct OggOpusHead {
    channels: u16,
    pre_skip: u16,
    stream_serial: u32,
}

struct OggOpusSource {
    path: String,
    reader: PacketReader<IoBufReader<std::fs::File>>,
    decoder: OpusDecoder,
    channels: u16,
    stream_serial: u32,
    pre_skip_remaining: u64,
    total_duration: Option<Duration>,
    buffer: Vec<i16>,
    decode_buffer: Vec<i16>,
    current_sample_offset: usize,
}

impl OggOpusSource {
    fn looks_like_ogg_opus(path: &str) -> bool {
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };
        let mut reader = PacketReader::new(IoBufReader::new(file));
        matches!(
            reader.read_packet(),
            Ok(Some(packet)) if packet.data.starts_with(b"OpusHead")
        )
    }

    fn from_path(path: &str) -> Result<Self, String> {
        let file =
            std::fs::File::open(path).map_err(|e| format!("Failed to open Ogg Opus file: {e}"))?;
        let mut reader = PacketReader::new(IoBufReader::new(file));
        let head = read_ogg_opus_headers(&mut reader)?;
        let opus_channels = match head.channels {
            1 => OpusChannels::Mono,
            2 => OpusChannels::Stereo,
            channels => {
                return Err(format!(
                    "Unsupported Ogg Opus channel count: {channels}. Only mono and stereo are supported."
                ))
            }
        };
        let decoder = OpusDecoder::new(OPUS_SAMPLE_RATE, opus_channels)
            .map_err(|e| format!("Failed to create Opus decoder: {e}"))?;
        let total_duration = scan_ogg_opus_duration(path, head.stream_serial, head.pre_skip);
        let decode_buffer_len = OPUS_MAX_FRAME_SAMPLES_PER_CHANNEL * head.channels as usize;

        Ok(Self {
            path: path.to_string(),
            reader,
            decoder,
            channels: head.channels,
            stream_serial: head.stream_serial,
            pre_skip_remaining: u64::from(head.pre_skip),
            total_duration,
            buffer: Vec::new(),
            decode_buffer: vec![0; decode_buffer_len],
            current_sample_offset: 0,
        })
    }

    fn seek(&mut self, position: Duration) -> Result<(), String> {
        let mut fresh = Self::from_path(&self.path)?;
        let target_samples = position
            .as_millis()
            .saturating_mul(u128::from(OPUS_SAMPLE_RATE))
            .saturating_mul(u128::from(fresh.channels))
            / 1_000;
        let mut remaining = target_samples.min(u128::from(u64::MAX)) as u64;
        while remaining > 0 {
            if fresh.next().is_none() {
                break;
            }
            remaining -= 1;
        }
        *self = fresh;
        Ok(())
    }

    fn decode_next_packet(&mut self) -> Option<()> {
        loop {
            let packet = match self.reader.read_packet() {
                Ok(Some(packet)) => packet,
                Ok(None) => return None,
                Err(err) => {
                    debug!("Ogg Opus packet read failed: {err}");
                    return None;
                }
            };

            if packet.stream_serial() != self.stream_serial
                || packet.data.is_empty()
                || packet.data.starts_with(b"OpusHead")
                || packet.data.starts_with(b"OpusTags")
            {
                continue;
            }

            let decoded_frames =
                match self
                    .decoder
                    .decode(&packet.data, &mut self.decode_buffer, false)
                {
                    Ok(frames) => frames,
                    Err(err) => {
                        debug!("Opus packet decode failed: {err}");
                        continue;
                    }
                };
            let channels = self.channels as usize;
            let decoded_samples = decoded_frames * channels;
            let mut start_frame = 0usize;
            if self.pre_skip_remaining > 0 {
                let skip_frames = decoded_frames.min(self.pre_skip_remaining as usize);
                self.pre_skip_remaining -= skip_frames as u64;
                start_frame = skip_frames;
            }
            let start_sample = start_frame * channels;
            self.buffer.clear();
            self.buffer
                .extend_from_slice(&self.decode_buffer[start_sample..decoded_samples]);
            self.current_sample_offset = 0;
            if !self.buffer.is_empty() {
                return Some(());
            }
        }
    }
}

impl Iterator for OggOpusSource {
    type Item = i16;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.current_sample_offset >= self.buffer.len() {
                self.decode_next_packet()?;
            }
            if self.current_sample_offset < self.buffer.len() {
                let sample = self.buffer[self.current_sample_offset];
                self.current_sample_offset += 1;
                return Some(sample);
            }
        }
    }
}

impl Source for OggOpusSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        OPUS_SAMPLE_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), RodioSeekError> {
        self.seek(position)
            .map_err(|err| RodioSeekError::Other(Box::new(std::io::Error::other(err))))
    }
}

fn read_ogg_opus_headers(
    reader: &mut PacketReader<IoBufReader<std::fs::File>>,
) -> Result<OggOpusHead, String> {
    let head_packet = reader
        .read_packet()
        .map_err(|e| format!("Failed to read Ogg Opus header: {e}"))?
        .ok_or_else(|| "Ogg Opus file is empty".to_string())?;
    let mut head = parse_ogg_opus_head(&head_packet.data)?;
    head.stream_serial = head_packet.stream_serial();

    let tags_packet = reader
        .read_packet()
        .map_err(|e| format!("Failed to read Ogg Opus tags: {e}"))?
        .ok_or_else(|| "Ogg Opus file is missing OpusTags".to_string())?;
    if tags_packet.stream_serial() != head.stream_serial
        || !tags_packet.data.starts_with(b"OpusTags")
    {
        return Err("Ogg Opus file is missing OpusTags".to_string());
    }

    Ok(head)
}

fn parse_ogg_opus_head(data: &[u8]) -> Result<OggOpusHead, String> {
    if data.len() < 19 || !data.starts_with(b"OpusHead") {
        return Err("Ogg file is not an Opus stream".to_string());
    }
    let version = data[8];
    if version & 0xf0 != 0 {
        return Err(format!("Unsupported Ogg Opus version: {version}"));
    }
    let channels = u16::from(data[9]);
    if !(1..=2).contains(&channels) {
        return Err(format!(
            "Unsupported Ogg Opus channel count: {channels}. Only mono and stereo are supported."
        ));
    }
    let pre_skip = u16::from_le_bytes([data[10], data[11]]);
    let input_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    if input_rate != OPUS_SAMPLE_RATE {
        return Err(format!(
            "Unsupported Ogg Opus input sample rate: {input_rate}. Only 48000 Hz is supported."
        ));
    }
    let channel_mapping_family = data[18];
    if channel_mapping_family != 0 {
        return Err(format!(
            "Unsupported Ogg Opus channel mapping family: {channel_mapping_family}"
        ));
    }

    Ok(OggOpusHead {
        channels,
        pre_skip,
        stream_serial: 0,
    })
}

fn scan_ogg_opus_duration(path: &str, stream_serial: u32, pre_skip: u16) -> Option<Duration> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = PacketReader::new(IoBufReader::new(file));
    let mut last_granule = None;
    while let Ok(Some(packet)) = reader.read_packet() {
        if packet.stream_serial() == stream_serial {
            last_granule = Some(packet.absgp_page());
        }
    }
    let frames = last_granule?.saturating_sub(u64::from(pre_skip));
    Some(Duration::from_secs_f64(
        frames as f64 / f64::from(OPUS_SAMPLE_RATE),
    ))
}

/// Symphonia-backed source with seek tracking.
struct SymphoniaSource {
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    format: Box<dyn symphonia::core::formats::FormatReader>,
    track_id: u32,
    time_base: Option<TimeBase>,
    n_frames: Option<u64>,
    buffer: SampleBuffer<i16>,
    spec: SignalSpec,
    current_frame_offset: usize,
    last_ts: u64,
    needs_decode: bool,
}

impl SymphoniaSource {
    fn from_path(path: &str) -> Result<Self, String> {
        let file = std::fs::File::open(path)
            .map_err(|e| format!("Failed to open file for decode: {e}"))?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
        {
            hint.with_extension(ext);
        }

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &MetadataOptions::default())
            .map_err(|e| format!("Failed to probe media: {e}"))?;
        let format = probed.format;
        let strict_audio_container = is_strict_audio_container(path);
        let track = select_audio_track(&*format, strict_audio_container)
            .ok_or_else(|| "No audio tracks found".to_string())?;

        let track_id = track.id;
        let time_base = track.codec_params.time_base;
        let n_frames = track.codec_params.n_frames;
        let rate = track
            .codec_params
            .sample_rate
            .filter(|rate| *rate > 0)
            .unwrap_or(TARGET_OUTPUT_SAMPLE_RATE);
        let channels = track.codec_params.channels.unwrap_or(
            symphonia::core::audio::Channels::FRONT_LEFT
                | symphonia::core::audio::Channels::FRONT_RIGHT,
        );
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())
            .map_err(|e| format!("Failed to create decoder: {e}"))?;

        Ok(Self {
            decoder,
            format,
            track_id,
            time_base,
            n_frames,
            buffer: SampleBuffer::new(4_096, SignalSpec { rate, channels }),
            spec: SignalSpec { rate, channels },
            current_frame_offset: 0,
            last_ts: 0,
            needs_decode: true,
        })
    }

    fn seek(&mut self, position_ms: u64) -> Result<(), String> {
        let time = Time::new(position_ms / 1000, (position_ms % 1000) as f64 / 1000.0);
        let seek_to = if let (Some(time_base), Some(max_frames)) = (self.time_base, self.n_frames) {
            SeekTo::TimeStamp {
                ts: time_base
                    .calc_timestamp(time)
                    .min(max_frames.saturating_sub(1)),
                track_id: self.track_id,
            }
        } else {
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            }
        };

        let seeked_to = self
            .format
            .seek(SeekMode::Coarse, seek_to)
            .map_err(|e| format!("Seek failed: {e}"))?;
        self.last_ts = seeked_to.actual_ts;
        self.needs_decode = true;
        self.current_frame_offset = 0;
        self.decoder.reset();
        Ok(())
    }

    fn total_duration(&self) -> Option<Duration> {
        let time_base = self.time_base?;
        let n_frames = self.n_frames?;
        let total_time = time_base.calc_time(n_frames);
        Some(Duration::from_secs(total_time.seconds) + Duration::from_secs_f64(total_time.frac))
    }

    fn decode_next_packet(&mut self) -> Option<()> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(packet) => packet,
                Err(SymphoniaError::IoError(err))
                    if err.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return None;
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::DecodeError(_)) => continue,
                Err(err) => {
                    debug!("Symphonia packet read failed: {}", err);
                    return None;
                }
            };

            if packet.track_id() != self.track_id {
                continue;
            }

            self.last_ts = packet.ts();
            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    if self.buffer.capacity() < decoded.capacity() {
                        self.buffer = SampleBuffer::new(decoded.capacity().max(1) as u64, spec);
                    }
                    self.buffer.copy_interleaved_ref(decoded);
                    self.spec = spec;
                    self.current_frame_offset = 0;
                    self.needs_decode = false;
                    return Some(());
                }
                Err(SymphoniaError::ResetRequired) => {
                    self.decoder.reset();
                }
                Err(SymphoniaError::DecodeError(_)) => {}
                Err(err) => {
                    debug!("Symphonia decode failed: {}", err);
                    return None;
                }
            }
        }
    }
}

fn is_strict_audio_container(path: &str) -> bool {
    matches!(
        std::path::Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase()),
        Some(ext) if matches!(ext.as_str(), "aac" | "m4a" | "mp4")
    )
}

fn is_audio_track(track: &Track) -> bool {
    track.codec_params.codec != CODEC_TYPE_NULL && track.codec_params.sample_rate.is_some()
}

fn select_audio_track(format: &dyn FormatReader, strict_audio_container: bool) -> Option<&Track> {
    format
        .tracks()
        .iter()
        .find(|track| is_audio_track(track))
        .or_else(|| {
            (!strict_audio_container)
                .then(|| {
                    format
                        .default_track()
                        .filter(|track| track.codec_params.codec != CODEC_TYPE_NULL)
                })
                .flatten()
        })
        .or_else(|| {
            (!strict_audio_container)
                .then(|| {
                    format
                        .tracks()
                        .iter()
                        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
                })
                .flatten()
        })
}

impl Iterator for SymphoniaSource {
    type Item = i16;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.needs_decode || self.current_frame_offset >= self.buffer.samples().len() {
                self.decode_next_packet()?;
            }
            if self.current_frame_offset < self.buffer.samples().len() {
                let sample = self.buffer.samples()[self.current_frame_offset];
                self.current_frame_offset += 1;
                return Some(sample);
            }
            return None;
        }
    }
}

impl Source for SymphoniaSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.spec.channels.count() as u16
    }

    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), RodioSeekError> {
        self.seek(position.as_millis() as u64)
            .map_err(|err| RodioSeekError::Other(Box::new(std::io::Error::other(err))))
    }
}

trait ResettableSource: Source<Item = i16> {
    fn seek_resettable(&mut self, position: Duration) -> Result<(), RodioSeekError>;
}

impl ResettableSource for SymphoniaSource {
    fn seek_resettable(&mut self, position: Duration) -> Result<(), RodioSeekError> {
        self.try_seek(position)
    }
}

struct ResettablePlaybackSource<S, F>
where
    S: ResettableSource,
    F: Fn() -> Result<S, String>,
{
    factory: F,
    converted: UniformSourceIterator<S, i16>,
    target_channels: u16,
    target_sample_rate: u32,
    total_duration: Option<Duration>,
}

impl<S, F> ResettablePlaybackSource<S, F>
where
    S: ResettableSource,
    F: Fn() -> Result<S, String>,
{
    fn new(factory: F, target_channels: u16, target_sample_rate: u32) -> Result<Self, String> {
        let input = factory()?;
        let total_duration = input.total_duration();
        Ok(Self {
            factory,
            converted: UniformSourceIterator::new(input, target_channels, target_sample_rate),
            target_channels,
            target_sample_rate,
            total_duration,
        })
    }

    fn total_duration_ms(&self) -> Option<u64> {
        self.total_duration
            .map(|duration| duration.as_millis() as u64)
    }

    fn seek_internal(&mut self, position: Duration) -> Result<(), RodioSeekError> {
        let mut input = (self.factory)().map_err(|err| {
            RodioSeekError::Other(Box::new(std::io::Error::other(format!(
                "Failed to rebuild playback source: {err}"
            ))))
        })?;
        input.seek_resettable(position)?;
        self.total_duration = input.total_duration();
        self.converted =
            UniformSourceIterator::new(input, self.target_channels, self.target_sample_rate);
        Ok(())
    }
}

impl<S, F> Iterator for ResettablePlaybackSource<S, F>
where
    S: ResettableSource,
    F: Fn() -> Result<S, String>,
{
    type Item = i16;

    fn next(&mut self) -> Option<Self::Item> {
        self.converted.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::audio_fixtures::{cleanup_test_audio_path, create_test_audio_file};
    use ogg::writing::{PacketWriteEndInfo, PacketWriter};
    use opus::{Application as OpusApplication, Encoder as OpusEncoder};
    use std::sync::Arc;

    fn test_runtime_config() -> RuntimeConfig {
        RuntimeConfig {
            local_volume: 1.0,
            mic_volume: 1.0,
            mic_passthrough: false,
            mic_source: None,
            default_source_mode: DefaultSourceMode::Manual,
            mic_latency_profile: MicLatencyProfile::Balanced,
            auto_gain: AutoGainState {
                enabled: false,
                mode: AutoGainMode::Static,
                apply_to: AutoGainApplyTo::Both,
                target_lufs: -14.0,
                dynamic: AutoGainDynamicParams {
                    lookahead_ms: 30,
                    attack_ms: 6,
                    release_ms: 150,
                },
            },
            looping: false,
            audio_backend: AudioBackendKind::PipeWire,
        }
    }

    fn test_player_snapshot_store() -> Arc<RwLock<PlayerSnapshot>> {
        Arc::new(RwLock::new(PlayerSnapshot::default()))
    }

    fn test_source(
        id: u32,
        node_name: &str,
        display_name: &str,
        priority: i32,
    ) -> SourceDescriptor {
        SourceDescriptor {
            id,
            serial: None,
            node_name: node_name.to_string(),
            display_name: display_name.to_string(),
            priority_session: priority,
            is_monitor: node_name.ends_with(".monitor"),
            is_our_virtual_mic: node_name == VIRTUAL_SOURCE_NAME,
        }
    }

    fn create_test_ogg_opus_file() -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("linux-soundboard-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&base).expect("create ogg opus temp dir");
        let path = base.join("tone.ogg");
        let serial = 0x4c53424f;
        let mut writer = PacketWriter::new(Vec::new());
        let mut head = b"OpusHead".to_vec();
        head.push(1);
        head.push(1);
        head.extend_from_slice(&0u16.to_le_bytes());
        head.extend_from_slice(&OPUS_SAMPLE_RATE.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(0);
        writer
            .write_packet(
                head.into_boxed_slice(),
                serial,
                PacketWriteEndInfo::EndPage,
                0,
            )
            .expect("write opus head");

        let vendor = b"linux-soundboard-test";
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes());
        writer
            .write_packet(
                tags.into_boxed_slice(),
                serial,
                PacketWriteEndInfo::EndPage,
                0,
            )
            .expect("write opus tags");

        let mut encoder =
            OpusEncoder::new(OPUS_SAMPLE_RATE, OpusChannels::Mono, OpusApplication::Audio)
                .expect("create opus encoder");
        let frame_samples = 960usize;
        let mut granule = 0u64;
        for packet_index in 0..2 {
            let mut pcm = vec![0.0f32; frame_samples];
            for (index, sample) in pcm.iter_mut().enumerate() {
                let absolute = packet_index * frame_samples + index;
                let phase =
                    2.0 * std::f32::consts::PI * 440.0 * absolute as f32 / OPUS_SAMPLE_RATE as f32;
                *sample = phase.sin() * 0.25;
            }
            let mut encoded = vec![0; 4_000];
            let len = encoder
                .encode_float(&pcm, &mut encoded)
                .expect("encode opus frame");
            encoded.truncate(len);
            granule += frame_samples as u64;
            let end = if packet_index == 1 {
                PacketWriteEndInfo::EndStream
            } else {
                PacketWriteEndInfo::NormalPacket
            };
            writer
                .write_packet(encoded.into_boxed_slice(), serial, end, granule)
                .expect("write opus packet");
        }

        std::fs::write(&path, writer.into_inner()).expect("write ogg opus fixture");
        path
    }

    #[test]
    fn parse_wpctl_node_name_extracts_quoted_name() {
        let output = r#"
id 72, type PipeWire:Interface:Node
  * node.name = "alsa_input.pci-0000_12_00.6.analog-stereo"
"#;
        assert_eq!(
            parse_wpctl_node_name(output).as_deref(),
            Some("alsa_input.pci-0000_12_00.6.analog-stereo")
        );
    }

    #[test]
    fn list_audio_sources_includes_virtual_third_parties_excludes_own_and_monitors() {
        let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());

        state.sources.insert(
            10,
            SourceDescriptor {
                id: 10,
                serial: None,
                node_name: "alsa_input.pci-0000_12_00.6.analog-stereo".to_string(),
                display_name: "Ryzen HD Audio".to_string(),
                priority_session: 0,
                is_monitor: false,
                is_our_virtual_mic: false,
            },
        );
        state.sources.insert(
            11,
            SourceDescriptor {
                id: 11,
                serial: None,
                node_name: "easyeffects_source".to_string(),
                display_name: "Easy Effects Source".to_string(),
                priority_session: 0,
                is_monitor: false,
                is_our_virtual_mic: false,
            },
        );
        state.sources.insert(
            12,
            SourceDescriptor {
                id: 12,
                serial: None,
                node_name: VIRTUAL_SOURCE_NAME.to_string(),
                display_name: VIRTUAL_MIC_DESCRIPTION.to_string(),
                priority_session: 0,
                is_monitor: false,
                is_our_virtual_mic: true,
            },
        );
        state.sources.insert(
            13,
            SourceDescriptor {
                id: 13,
                serial: None,
                node_name: "alsa_output.pci-0000_12_00.6.analog-stereo.monitor".to_string(),
                display_name: "Speaker Monitor".to_string(),
                priority_session: 0,
                is_monitor: true,
                is_our_virtual_mic: false,
            },
        );

        let listed = state.list_audio_sources();
        let names: Vec<_> = listed.iter().map(|s| s.node_name.as_str()).collect();
        assert!(names.contains(&"alsa_input.pci-0000_12_00.6.analog-stereo"));
        assert!(names.contains(&"easyeffects_source"));
        assert!(!names.contains(&VIRTUAL_SOURCE_NAME));
        assert!(!names.iter().any(|name| name.ends_with(".monitor")));
    }

    #[test]
    fn build_playback_positions_prefers_newest_unfinished_entries() {
        let mut registry = HashMap::new();
        registry.insert(
            "play-old".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-old".to_string(),
                playback_order: 1,
                position_ms: 1_000,
                paused: false,
                duration_ms: Some(10_000),
                finished: false,
            },
        );
        registry.insert(
            "play-new".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-new".to_string(),
                playback_order: 2,
                position_ms: 250,
                paused: false,
                duration_ms: Some(10_000),
                finished: false,
            },
        );
        registry.insert(
            "play-finished".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-finished".to_string(),
                playback_order: 3,
                position_ms: 10_000,
                paused: false,
                duration_ms: Some(10_000),
                finished: true,
            },
        );

        let positions = build_playback_positions(&registry);
        assert_eq!(positions[0].play_id, "play-new");
        assert_eq!(positions[1].play_id, "play-old");
        assert_eq!(positions[2].play_id, "play-finished");
    }

    #[test]
    fn resolve_source_id_by_name_finds_matching_source() {
        let sources = HashMap::from([(
            7,
            SourceDescriptor {
                id: 7,
                serial: None,
                node_name: "alsa_input.pci-0000_12_00.6.analog-stereo".to_string(),
                display_name: "Mic".to_string(),
                priority_session: 0,
                is_monitor: false,
                is_our_virtual_mic: false,
            },
        )]);

        assert_eq!(
            resolve_source_id_by_name(&sources, "alsa_input.pci-0000_12_00.6.analog-stereo"),
            Some(7)
        );
        assert_eq!(resolve_source_id_by_name(&sources, "missing"), None);
    }

    #[test]
    fn auto_route_mode_does_not_claim_system_default() {
        let mut runtime = test_runtime_config();
        runtime.default_source_mode = DefaultSourceMode::AutoRouteWhileRunning;
        let mut state = LoopState::new(runtime, test_player_snapshot_store());
        state
            .sources
            .insert(1, test_source(1, "alsa_input.real", "Real Mic", 100));
        state.sources.insert(
            2,
            test_source(2, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 0),
        );

        maybe_claim_default_source(&mut state);

        assert!(!state.claimed_default);
        assert!(state.previous_default_source_name.is_none());
    }

    #[test]
    fn restore_default_source_stops_claim_without_random_fallback() {
        let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
        state.claimed_default = true;
        state.previous_default_source_name = Some("missing.source".to_string());
        state.sources.insert(
            2,
            test_source(2, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 0),
        );

        restore_default_source(&mut state).unwrap();

        assert!(!state.claimed_default);
        assert_eq!(
            state.previous_default_source_name.as_deref(),
            Some("missing.source")
        );
    }

    #[test]
    fn explicit_selected_mic_waits_for_exact_source() {
        let mut runtime = test_runtime_config();
        runtime.mic_source = Some("easyeffects_source".to_string());
        let mut state = LoopState::new(runtime, test_player_snapshot_store());
        state
            .sources
            .insert(7, test_source(7, "alsa_input.real", "Real Mic", 2000));

        assert_eq!(
            resolve_capture_target_from_default(&state, Some("alsa_input.real".to_string())),
            None
        );

        state.sources.insert(
            8,
            test_source(8, "easyeffects_source", "Easy Effects", 1000),
        );
        assert_eq!(
            resolve_capture_target_from_default(&state, Some("alsa_input.real".to_string()))
                .as_deref(),
            Some("easyeffects_source")
        );
    }

    #[test]
    fn explicit_selected_mic_rejects_linux_soundboard_virtual_mic() {
        let mut runtime = test_runtime_config();
        runtime.mic_source = Some(VIRTUAL_SOURCE_NAME.to_string());
        let mut state = LoopState::new(runtime, test_player_snapshot_store());
        state.sources.insert(
            8,
            test_source(8, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 5000),
        );

        assert_eq!(
            resolve_capture_target_from_default(&state, Some(VIRTUAL_SOURCE_NAME.to_string())),
            None
        );
    }

    #[test]
    fn auto_capture_prefers_default_then_previous_then_enhancement_fallback() {
        let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
        state.sources.insert(
            6,
            test_source(6, "easyeffects_source", "Easy Effects Source", 10),
        );
        state
            .sources
            .insert(7, test_source(7, "alsa_input.low", "Low Priority", 100));
        state
            .sources
            .insert(8, test_source(8, "alsa_input.high", "High Priority", 200));
        state.sources.insert(
            9,
            test_source(9, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 5000),
        );
        state.sources.insert(
            10,
            test_source(10, "alsa_output.speakers.monitor", "Monitor", 9000),
        );

        assert_eq!(
            resolve_capture_target_from_default(&state, Some("alsa_input.low".to_string()))
                .as_deref(),
            Some("alsa_input.low")
        );

        state.previous_default_source_name = Some("alsa_input.low".to_string());
        assert_eq!(
            resolve_capture_target_from_default(&state, Some(VIRTUAL_SOURCE_NAME.to_string()))
                .as_deref(),
            Some("alsa_input.low")
        );

        state.previous_default_source_name = None;
        assert_eq!(
            best_fallback_source_name(&state.sources).as_deref(),
            Some("easyeffects_source")
        );
        assert_eq!(
            resolve_capture_target_from_default(&state, Some(VIRTUAL_SOURCE_NAME.to_string()))
                .as_deref(),
            Some("easyeffects_source")
        );
    }

    #[test]
    fn pipewire_capture_health_requires_non_error_linked_expected_target() {
        let sources = HashMap::from([(
            78,
            test_source(
                78,
                "alsa_input.pci-0000_12_00.6.analog-stereo",
                "Real Mic",
                2000,
            ),
        )]);
        let mut links = HashMap::new();

        assert!(!pipewire_capture_link_healthy(
            Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
            Some(253),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));

        links.insert(
            1,
            LinkDescriptor {
                id: 1,
                output_node_id: 78,
                input_node_id: 999,
                output_port_id: None,
                input_port_id: None,
            },
        );
        assert!(!pipewire_capture_link_healthy(
            Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
            Some(253),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));

        links.insert(
            2,
            LinkDescriptor {
                id: 2,
                output_node_id: 78,
                input_node_id: 253,
                output_port_id: None,
                input_port_id: None,
            },
        );
        assert!(pipewire_capture_link_healthy(
            Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
            Some(253),
            ManagedStreamState::Paused,
            &sources,
            &links,
        ));
        assert!(!pipewire_capture_link_healthy(
            Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
            Some(253),
            ManagedStreamState::Error,
            &sources,
            &links,
        ));
    }

    #[test]
    fn pipewire_capture_health_rejects_self_capture_from_virtual_mic() {
        let sources = HashMap::from([(
            32,
            test_source(32, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 5000),
        )]);
        let links = HashMap::from([(
            1,
            LinkDescriptor {
                id: 1,
                output_node_id: 32,
                input_node_id: 253,
                output_port_id: None,
                input_port_id: None,
            },
        )]);

        assert!(!pipewire_capture_link_healthy(
            Some(VIRTUAL_SOURCE_NAME),
            Some(253),
            ManagedStreamState::Streaming,
            &sources,
            &links,
        ));
    }

    #[test]
    fn loop_state_filters_virtual_and_monitor_sources() {
        let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
        state.sources.insert(
            1,
            SourceDescriptor {
                id: 1,
                serial: None,
                node_name: "alsa_input.real".to_string(),
                display_name: "Real Mic".to_string(),
                priority_session: 0,
                is_monitor: false,
                is_our_virtual_mic: false,
            },
        );
        state.sources.insert(
            2,
            SourceDescriptor {
                id: 2,
                serial: None,
                node_name: "alsa_output.monitor".to_string(),
                display_name: "Monitor".to_string(),
                priority_session: 0,
                is_monitor: true,
                is_our_virtual_mic: false,
            },
        );
        state.sources.insert(
            3,
            SourceDescriptor {
                id: 3,
                serial: None,
                node_name: VIRTUAL_SOURCE_NAME.to_string(),
                display_name: VIRTUAL_MIC_DESCRIPTION.to_string(),
                priority_session: 0,
                is_monitor: false,
                is_our_virtual_mic: true,
            },
        );

        let visible = state.list_audio_sources();
        assert_eq!(
            visible,
            vec![AudioSourceInfo {
                node_name: "alsa_input.real".to_string(),
                display_name: "Real Mic".to_string(),
            }]
        );
    }

    #[test]
    fn fill_output_queues_prefills_target_buffer_for_active_playback() {
        let audio_path = create_test_audio_file("wav");
        let runtime = test_runtime_config();
        let playback = ActivePlayback::new(
            "play-1".to_string(),
            "sound-1".to_string(),
            audio_path.to_string_lossy().to_string(),
            0,
            1.0,
            None,
            &runtime,
        )
        .expect("create active playback");

        let mut state = LoopState::new(runtime, test_player_snapshot_store());
        state.active_playback = Some(playback);

        fill_output_queues(&mut state);

        let queues = state.queues.lock().expect("lock queues");
        let target_samples = LOCAL_OUTPUT_QUEUE_TARGET_FRAMES * TARGET_OUTPUT_CHANNELS as usize;
        assert_eq!(queues.local.len(), target_samples);
        assert_eq!(queues.virtual_out.len(), target_samples);
        drop(queues);

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn ogg_opus_source_decodes_and_seek_discards() {
        let audio_path = create_test_ogg_opus_file();
        let mut source = OggOpusSource::from_path(&audio_path.to_string_lossy())
            .expect("create ogg opus source");

        assert_eq!(source.channels(), 1);
        assert_eq!(source.sample_rate(), OPUS_SAMPLE_RATE);
        assert!(source
            .total_duration()
            .is_some_and(|duration| duration >= Duration::from_millis(40)));

        let first_samples: Vec<_> = source.by_ref().take(960).collect();
        assert!(first_samples.iter().any(|sample| *sample != 0));

        source
            .try_seek(Duration::from_millis(20))
            .expect("seek ogg opus source");
        let seeked_samples: Vec<_> = source.take(128).collect();
        assert!(seeked_samples.iter().any(|sample| *sample != 0));

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn active_playback_routes_ogg_opus_through_common_mix_path() {
        let audio_path = create_test_ogg_opus_file();
        let runtime = test_runtime_config();
        let mut playback = ActivePlayback::new(
            "play-opus".to_string(),
            "sound-opus".to_string(),
            audio_path.to_string_lossy().to_string(),
            0,
            1.0,
            None,
            &runtime,
        )
        .expect("create active ogg opus playback");

        let mut local = vec![0.0; 512];
        let mut virtual_out = vec![0.0; 512];
        playback.render_into(&mut local, &mut virtual_out, &runtime);

        assert!(local.iter().any(|sample| sample.abs() > f32::EPSILON));
        assert!(virtual_out.iter().any(|sample| sample.abs() > f32::EPSILON));

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn fill_output_queues_respects_per_tick_batch_budget() {
        let audio_path = create_test_audio_file("wav");
        let runtime = test_runtime_config();
        let playback = ActivePlayback::new(
            "play-budget".to_string(),
            "sound-budget".to_string(),
            audio_path.to_string_lossy().to_string(),
            0,
            1.0,
            None,
            &runtime,
        )
        .expect("create active playback");

        let mut state = LoopState::new(runtime, test_player_snapshot_store());
        state.active_playback = Some(playback);

        fill_output_queues(&mut state);

        let queues = state.queues.lock().expect("lock queues");
        let max_samples_per_tick = state.runtime.max_fill_batches_per_tick(true, true)
            * MIX_CHUNK_FRAMES
            * TARGET_OUTPUT_CHANNELS as usize;
        assert!(queues.local.len() <= max_samples_per_tick);
        assert!(queues.virtual_out.len() <= max_samples_per_tick);
        drop(queues);

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn fill_output_queues_mic_passthrough_without_capture_stream_keeps_queues_idle() {
        let mut runtime = test_runtime_config();
        runtime.mic_passthrough = true;

        let mut state = LoopState::new(runtime, test_player_snapshot_store());
        fill_output_queues(&mut state);
        fill_output_queues(&mut state);

        let queues = state.queues.lock().expect("lock queues");
        assert_eq!(queues.local.len(), 0);
        assert_eq!(queues.virtual_out.len(), 0);
    }

    #[test]
    fn passthrough_chunk_pads_short_mic_input_with_silence() {
        let mut queues = ProcessQueues::new(8, 8, 8);
        queues.mic_in.push_slice(&[0.25, -0.5]);

        enqueue_passthrough_chunk(&mut queues, 6);

        let mut output = vec![1.0; 6];
        let dequeued = queues.virtual_out.pop_into(&mut output);
        assert_eq!(dequeued, 6);
        assert_eq!(output, vec![0.25, -0.5, 0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn runtime_latency_profile_low_reduces_virtual_target() {
        let mut runtime = test_runtime_config();
        runtime.mic_latency_profile = MicLatencyProfile::Low;

        assert!(runtime.virtual_output_target_samples() < runtime.local_output_target_samples());
        assert!(runtime.max_virtual_callback_samples() < MAX_LOCAL_OUTPUT_CALLBACK_SAMPLES);
    }

    #[test]
    fn runtime_latency_profile_ultra_is_smallest_virtual_target() {
        let mut low = test_runtime_config();
        low.mic_latency_profile = MicLatencyProfile::Low;
        let mut ultra = test_runtime_config();
        ultra.mic_latency_profile = MicLatencyProfile::Ultra;

        assert!(ultra.virtual_output_target_samples() < low.virtual_output_target_samples());
        assert!(ultra.max_virtual_callback_samples() < low.max_virtual_callback_samples());
    }

    #[test]
    fn clear_virtual_mic_queues_resets_mic_path_only() {
        let state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
        {
            let mut queues = state.queues.lock().expect("lock queues");
            queues.local.push_slice(&[0.1, 0.2]);
            queues.virtual_out.push_slice(&[0.3, 0.4, 0.5]);
            queues.mic_in.push_slice(&[0.6, 0.7, 0.8, 0.9]);
        }

        clear_virtual_mic_queues(&state.queues);

        let queues = state.queues.lock().expect("lock queues");
        assert_eq!(queues.local.len(), 2);
        assert_eq!(queues.virtual_out.len(), 0);
        assert_eq!(queues.mic_in.len(), 0);
    }

    #[test]
    fn clear_all_queues_resets_local_virtual_and_mic_buffers() {
        let state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
        {
            let mut queues = state.queues.lock().expect("lock queues");
            queues.local.push_slice(&[0.1, 0.2]);
            queues.virtual_out.push_slice(&[0.3, 0.4, 0.5]);
            queues.mic_in.push_slice(&[0.6, 0.7, 0.8, 0.9]);
        }

        clear_all_queues(&state.queues);

        let queues = state.queues.lock().expect("lock queues");
        assert_eq!(queues.local.len(), 0);
        assert_eq!(queues.virtual_out.len(), 0);
        assert_eq!(queues.mic_in.len(), 0);
    }

    #[test]
    fn recreate_capture_stream_clears_mic_input_without_dropping_soundboard_output() {
        let mut runtime = test_runtime_config();
        runtime.mic_passthrough = true;
        let mut state = LoopState::new(runtime, test_player_snapshot_store());
        {
            let mut queues = state.queues.lock().expect("lock queues");
            queues.local.push_slice(&[0.1, 0.2]);
            queues.virtual_out.push_slice(&[0.3, 0.4, 0.5]);
            queues.mic_in.push_slice(&[0.6, 0.7, 0.8, 0.9]);
        }

        let result = recreate_capture_stream(&mut state);
        assert!(result.is_ok());

        let queues = state.queues.lock().expect("lock queues");
        assert_eq!(queues.local.len(), 2);
        assert_eq!(queues.virtual_out.len(), 3);
        assert_eq!(queues.mic_in.len(), 0);
    }

    #[test]
    fn publish_snapshot_includes_visible_sources_and_active_playback() {
        let audio_path = create_test_audio_file("wav");
        let runtime = test_runtime_config();
        let snapshot = test_player_snapshot_store();
        let mut state = LoopState::new(runtime.clone(), snapshot.clone());
        state.available = true;
        state.sources.insert(
            1,
            SourceDescriptor {
                id: 1,
                serial: None,
                node_name: "alsa_input.real".to_string(),
                display_name: "Real Mic".to_string(),
                priority_session: 0,
                is_monitor: false,
                is_our_virtual_mic: false,
            },
        );
        state.active_playback = Some(
            ActivePlayback::new(
                "play-1".to_string(),
                "sound-1".to_string(),
                audio_path.to_string_lossy().to_string(),
                0,
                1.0,
                None,
                &runtime,
            )
            .expect("create active playback"),
        );
        state.publish_snapshot();

        let snapshot = snapshot.read().expect("read snapshot").clone();
        assert!(snapshot.available);
        assert_eq!(snapshot.playing_ids, vec!["sound-1".to_string()]);
        assert_eq!(snapshot.audio_sources.len(), 1);
        assert_eq!(snapshot.audio_sources[0].node_name, "alsa_input.real");

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn dynamic_lookahead_mode_warmup_does_not_output_initial_silence() {
        let audio_path = create_test_audio_file("wav");
        let mut runtime = test_runtime_config();
        runtime.auto_gain.enabled = true;
        runtime.auto_gain.mode = AutoGainMode::DynamicLookAhead;
        runtime.auto_gain.apply_to = AutoGainApplyTo::Both;

        let mut playback = ActivePlayback::new(
            "play-warmup".to_string(),
            "sound-warmup".to_string(),
            audio_path.to_string_lossy().to_string(),
            0,
            1.0,
            Some(-14.0),
            &runtime,
        )
        .expect("create active playback");

        let mut local = vec![0.0; 512];
        let mut virtual_out = vec![0.0; 512];
        playback.render_into(&mut local, &mut virtual_out, &runtime);

        assert!(local.iter().any(|sample| sample.abs() > f32::EPSILON));
        assert!(virtual_out.iter().any(|sample| sample.abs() > f32::EPSILON));

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn dynamic_apply_to_switch_rebuilds_live_limiter_scope() {
        let audio_path = create_test_audio_file("wav");
        let mut runtime = test_runtime_config();
        runtime.auto_gain.enabled = true;
        runtime.auto_gain.mode = AutoGainMode::DynamicLookAhead;
        runtime.auto_gain.apply_to = AutoGainApplyTo::Both;

        let mut playback = ActivePlayback::new(
            "play-scope".to_string(),
            "sound-scope".to_string(),
            audio_path.to_string_lossy().to_string(),
            0,
            1.0,
            Some(-14.0),
            &runtime,
        )
        .expect("create active playback");

        assert!(playback.local_limiter.is_some());
        assert!(playback.virtual_limiter.is_some());

        runtime.auto_gain.apply_to = AutoGainApplyTo::MicOnly;
        let mut local = vec![0.0; 128];
        let mut virtual_out = vec![0.0; 128];
        playback.render_into(&mut local, &mut virtual_out, &runtime);

        assert!(playback.local_limiter.is_none());
        assert!(playback.virtual_limiter.is_some());

        cleanup_test_audio_path(&audio_path);
    }

    #[test]
    fn loop_state_trim_finished_playbacks_discards_oldest_entries() {
        let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
        state.finished_playbacks.insert(
            "play-1".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-1".to_string(),
                playback_order: 1,
                position_ms: 100,
                paused: false,
                duration_ms: Some(1_000),
                finished: true,
            },
        );
        state.finished_playbacks.insert(
            "play-2".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-2".to_string(),
                playback_order: 2,
                position_ms: 200,
                paused: false,
                duration_ms: Some(1_000),
                finished: true,
            },
        );
        state.finished_playbacks.insert(
            "play-3".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-3".to_string(),
                playback_order: 3,
                position_ms: 300,
                paused: false,
                duration_ms: Some(1_000),
                finished: true,
            },
        );

        state.trim_finished_playbacks(2);

        assert_eq!(state.finished_playbacks.len(), 2);
        assert!(!state.finished_playbacks.contains_key("play-1"));
        assert!(state.finished_playbacks.contains_key("play-2"));
        assert!(state.finished_playbacks.contains_key("play-3"));
    }
}
