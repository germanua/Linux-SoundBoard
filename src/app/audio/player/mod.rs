//! PipeWire-backed audio playback with a runtime virtual microphone.

use crate::app_meta::{
    LOCAL_PLAYBACK_NODE_NAME, MIC_CAPTURE_NODE_NAME, VIRTUAL_OUTPUT_DESCRIPTION,
    VIRTUAL_SOURCE_NAME,
};
use crate::config::{DefaultSourceMode, MicLatencyProfile};
use glib;
use log::{debug, error, info, trace, warn};
use parking_lot::Mutex;
use parking_lot::RwLock;
use pipewire as pw;
use pw::channel as pw_channel;
use pw::properties::properties;
use pw::spa;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::mem;
use std::process::Command;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

mod capture_watchdog;
mod command_handlers;
mod command_protocol;
mod decode;
mod default_source;
mod error;
mod explicit_links;
mod limiter;
mod loop_state;
mod mixing;
mod playback;
mod pulse_backend;
mod pw_backend;
mod queues;
mod registry_handlers;
mod runtime_config;
mod source_routing;
mod streams;
mod virtual_mic_module;

use capture_watchdog::*;
use command_handlers::{audio_command_kind, handle_audio_command};
use command_protocol::*;
use decode::*;
pub(crate) use decode::{
    is_strict_audio_container, select_audio_track, AudioSource as DecodedAudioSource,
    PlaybackSource as DecodedPlaybackSource,
};
use default_source::{
    apply_default_source_mode, bind_default_metadata_from_global, claim_default_source_if_enabled,
    forget_default_source_belief, DefaultMetadataHandle,
};
pub use error::EngineError;
use explicit_links::{drop_feeder_links, try_link_feeder_to_virtual_mic, AudioChannel, FeederLink};
use limiter::LookAheadLimiter;
use loop_state::*;
use mixing::clear_mic_input_queue;
#[cfg(test)]
use mixing::clear_virtual_mic_queues;
use mixing::{clear_all_queues, fade_output_queues, mix_tick};
#[cfg(test)]
use mixing::{enqueue_passthrough_chunk, fill_output_queues};
use playback::ActivePlayback;
use pulse_backend::PulseAudioBackend;
use pw_backend::{
    create_backend, remote_ok, remote_play, BackendState, ManagedStreamState, StreamHandle,
};
use queues::{ProcessQueues, RtSharedQueues, SampleQueue};
use registry_handlers::*;
use runtime_config::*;
use source_routing::recreate_capture_stream;
#[cfg(test)]
use source_routing::{
    best_upstream_mic_source_name, parse_wpctl_node_name, resolve_capture_target_from_default,
    resolve_source_id_by_name,
};
use streams::{
    create_capture_stream, create_local_output_stream, create_runtime_virtual_source_stream,
};

const TARGET_OUTPUT_SAMPLE_RATE: u32 = 48_000;
const TARGET_OUTPUT_CHANNELS: u32 = 2;
// Mix tick runs every 2 ms. Ultra uses a 256-frame quantum (~5.3 ms), and a
// tick faster than the quantum keeps the producer from starving under load.
const MIX_INTERVAL_MS: u64 = 2;
const MIX_CHUNK_FRAMES: usize = 512;
const LOCAL_OUTPUT_QUEUE_TARGET_FRAMES: usize = 3_072;
const BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES: usize = 2_048;
const LOW_VIRTUAL_QUEUE_TARGET_FRAMES: usize = 1_024;
const ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES: usize = 512;
const OUTPUT_QUEUE_CAPACITY_SAMPLES: usize = TARGET_OUTPUT_SAMPLE_RATE as usize * 2;
const MIC_QUEUE_CAPACITY_SAMPLES: usize = TARGET_OUTPUT_SAMPLE_RATE as usize * 4;
const MAX_LOCAL_OUTPUT_CALLBACK_SAMPLES: usize =
    LOCAL_OUTPUT_QUEUE_TARGET_FRAMES * TARGET_OUTPUT_CHANNELS as usize;
const ULTRA_STARVATION_TICK_FALLBACK_THRESHOLD: u32 = 12;
const AUDIO_COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const PLAY_COMMAND_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_FINISHED_PLAYBACK_SNAPSHOTS: usize = 128;
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const SHUTDOWN_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const UI_SNAPSHOT_PROGRESS_INTERVAL_MS: u64 = 100;
const CAPTURE_RECREATE_MISS_THRESHOLD: u8 = 2;

thread_local! {
    static OUTPUT_CALLBACK_SCRATCH: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
    static CAPTURE_CALLBACK_SCRATCH: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
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
    #[serde(default)]
    pub is_virtual: bool,
    #[serde(default)]
    pub is_hardware_backed: bool,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShutdownPolicy {
    Persistent,
    Transient,
}

impl ShutdownPolicy {
    const fn restores_default_source(self) -> bool {
        matches!(self, Self::Transient)
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
    is_virtual: bool,
    is_hardware_backed: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SinkDescriptor {
    pub(super) id: u32,
    pub(super) serial: Option<u64>,
    pub(super) node_name: String,
    pub(super) monitor_source_node_name: Option<String>,
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

enum AudioPlayerBackend {
    Local(LocalAudioPlayer),
    Remote(RemoteAudioPlayer),
    #[cfg(test)]
    Noop(NoopAudioPlayer),
}

struct LocalAudioPlayer {
    command_tx: pw_channel::Sender<AudioCommand>,
    join_handle: Mutex<Option<thread::JoinHandle<()>>>,
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
    shutdown_policy: ShutdownPolicy,
}

struct RemoteAudioPlayer {
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
    stop_poll: std::sync::Arc<AtomicBool>,
    poll_handle: Mutex<Option<thread::JoinHandle<()>>>,
}

#[cfg(test)]
struct NoopAudioPlayer {
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
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
                "Refusing to use incompatible Linux Soundboard audio engine: version={} protocol={} schema={} binary={}",
                info.app_version,
                info.engine_protocol_version,
                info.config_schema_version,
                info.binary_path
            );
            return None;
        }

        let snapshot = std::sync::Arc::new(RwLock::new(PlayerSnapshot::default()));
        if let Ok(crate::audio::engine_ipc::EngineResponse::Snapshot { snapshot: initial }) =
            crate::audio::engine_ipc::send_request(
                crate::audio::engine_ipc::EngineRequest::Snapshot,
            )
        {
            *snapshot.write() = initial;
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
                        *poll_snapshot.write() = snapshot.clone();
                        glib::MainContext::default().invoke(move || {
                            crate::ui_event_bridge::dispatch_snapshot(snapshot);
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

    #[cfg(test)]
    pub(crate) fn new_test_noop() -> Self {
        Self {
            backend: AudioPlayerBackend::Noop(NoopAudioPlayer {
                snapshot: std::sync::Arc::new(RwLock::new(PlayerSnapshot::default())),
            }),
        }
    }

    pub fn new_with_config_and_audio_backend(
        config: &crate::config::Config,
        audio_backend: AudioBackendKind,
    ) -> Self {
        Self::new_with_config_audio_backend_and_shutdown_policy(
            config,
            audio_backend,
            ShutdownPolicy::Transient,
        )
    }

    pub(crate) fn new_with_config_audio_backend_and_shutdown_policy(
        config: &crate::config::Config,
        audio_backend: AudioBackendKind,
        shutdown_policy: ShutdownPolicy,
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
                shutdown_policy,
            }),
        }
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        match &self.backend {
            AudioPlayerBackend::Local(local) => local.snapshot.read().clone(),
            AudioPlayerBackend::Remote(remote) => remote.snapshot.read().clone(),
            #[cfg(test)]
            AudioPlayerBackend::Noop(noop) => noop.snapshot.read().clone(),
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
        }
    }

    pub fn set_mic_passthrough(&self, enabled: bool) -> Result<(), EngineError> {
        let local = match &self.backend {
            AudioPlayerBackend::Local(local) => local,
            AudioPlayerBackend::Remote(_) => {
                return remote_ok(crate::audio::engine_ipc::EngineRequest::SetMicPassthrough {
                    enabled,
                });
            }
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => return Ok(()),
        };
        let (tx, rx) = mpsc::channel();
        local
            .command_tx
            .send(AudioCommand::SetMicPassthrough {
                enabled,
                response: tx,
            })
            .map_err(|_| EngineError::Setup("Audio backend thread is not running".to_string()))?;
        match rx.recv_timeout(AUDIO_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(EngineError::Setup(format!(
                "Audio backend timed out while handling SetMicPassthrough after {} ms",
                AUDIO_COMMAND_RESPONSE_TIMEOUT.as_millis()
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(EngineError::Setup(
                "Audio backend response channel closed".to_string(),
            )),
        }
    }

    pub fn set_mic_source(&self, source: Option<String>) -> Result<(), EngineError> {
        let local = match &self.backend {
            AudioPlayerBackend::Local(local) => local,
            AudioPlayerBackend::Remote(_) => {
                return remote_ok(crate::audio::engine_ipc::EngineRequest::SetMicSource { source });
            }
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => return Ok(()),
        };
        let (tx, rx) = mpsc::channel();
        local
            .command_tx
            .send(AudioCommand::SetMicSource {
                source,
                response: tx,
            })
            .map_err(|_| EngineError::Setup("Audio backend thread is not running".to_string()))?;
        match rx.recv_timeout(AUDIO_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(EngineError::Setup(format!(
                "Audio backend timed out while handling SetMicSource after {} ms",
                AUDIO_COMMAND_RESPONSE_TIMEOUT.as_millis()
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(EngineError::Setup(
                "Audio backend response channel closed".to_string(),
            )),
        }
    }

    pub fn set_default_source_mode(&self, mode: DefaultSourceMode) -> Result<(), EngineError> {
        let local = match &self.backend {
            AudioPlayerBackend::Local(local) => local,
            AudioPlayerBackend::Remote(_) => {
                return remote_ok(
                    crate::audio::engine_ipc::EngineRequest::SetDefaultSourceMode { mode },
                );
            }
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => return Ok(()),
        };
        let (tx, rx) = mpsc::channel();
        local
            .command_tx
            .send(AudioCommand::SetDefaultSourceMode { mode, response: tx })
            .map_err(|_| EngineError::Setup("Audio backend thread is not running".to_string()))?;
        match rx.recv_timeout(AUDIO_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(EngineError::Setup(format!(
                "Audio backend timed out while handling SetDefaultSourceMode after {} ms",
                AUDIO_COMMAND_RESPONSE_TIMEOUT.as_millis()
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(EngineError::Setup(
                "Audio backend response channel closed".to_string(),
            )),
        }
    }

    pub fn set_mic_latency_profile(&self, profile: MicLatencyProfile) -> Result<(), EngineError> {
        let local = match &self.backend {
            AudioPlayerBackend::Local(local) => local,
            AudioPlayerBackend::Remote(_) => {
                return remote_ok(
                    crate::audio::engine_ipc::EngineRequest::SetMicLatencyProfile { profile },
                );
            }
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => return Ok(()),
        };
        let (tx, rx) = mpsc::channel();
        local
            .command_tx
            .send(AudioCommand::SetMicLatencyProfile {
                profile,
                response: tx,
            })
            .map_err(|_| EngineError::Setup("Audio backend thread is not running".to_string()))?;
        match rx.recv_timeout(AUDIO_COMMAND_RESPONSE_TIMEOUT) {
            Ok(result) => result,
            Err(RecvTimeoutError::Timeout) => Err(EngineError::Setup(format!(
                "Audio backend timed out while handling SetMicLatencyProfile after {} ms",
                AUDIO_COMMAND_RESPONSE_TIMEOUT.as_millis()
            ))),
            Err(RecvTimeoutError::Disconnected) => Err(EngineError::Setup(
                "Audio backend response channel closed".to_string(),
            )),
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
        sound_true_peak_dbtp: Option<f32>,
    ) -> Result<String, EngineError> {
        #[cfg(test)]
        if matches!(self.backend, AudioPlayerBackend::Noop(_)) {
            return Ok(format!("noop-play-{sound_id}"));
        }

        if matches!(self.backend, AudioPlayerBackend::Remote(_)) {
            return remote_play(
                crate::audio::engine_ipc::EngineRequest::Play {
                    sound_id: sound_id.to_string(),
                    path: path.to_string(),
                    base_volume,
                    sound_lufs,
                    sound_true_peak_dbtp,
                },
                "Play",
            );
        }
        let AudioPlayerBackend::Local(local) = &self.backend else {
            return Err(EngineError::Setup(
                "Remote audio player unavailable".to_string(),
            ));
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
                sound_true_peak_dbtp,
                response: response_tx,
            })
            .map_err(|_| EngineError::Setup("Audio backend thread is not running".to_string()))?;
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
                Err(EngineError::Setup(format!(
                    "Audio backend timed out while handling Play after {} ms",
                    PLAY_COMMAND_RESPONSE_TIMEOUT.as_millis()
                )))
            }
            Err(RecvTimeoutError::Disconnected) => Err(EngineError::Setup(
                "Audio backend response channel closed".to_string(),
            )),
        }
    }

    pub fn stop_sound(&self, sound_id: &str) -> Result<(), EngineError> {
        match &self.backend {
            AudioPlayerBackend::Local(local) => local
                .command_tx
                .send(AudioCommand::StopSound {
                    sound_id: sound_id.to_string(),
                })
                .map_err(|_| EngineError::Setup("Audio backend thread is not running".to_string())),
            AudioPlayerBackend::Remote(_) => {
                remote_ok(crate::audio::engine_ipc::EngineRequest::StopSound {
                    sound_id: sound_id.to_string(),
                })
            }
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => Ok(()),
        }
    }

    pub fn play_replace(
        &self,
        sound_id: &str,
        path: &str,
        base_volume: f32,
        sound_lufs: Option<f64>,
        sound_true_peak_dbtp: Option<f32>,
    ) -> Result<String, EngineError> {
        #[cfg(test)]
        if matches!(self.backend, AudioPlayerBackend::Noop(_)) {
            return Ok(format!("noop-play-{sound_id}"));
        }

        if matches!(self.backend, AudioPlayerBackend::Remote(_)) {
            return remote_play(
                crate::audio::engine_ipc::EngineRequest::PlayReplace {
                    sound_id: sound_id.to_string(),
                    path: path.to_string(),
                    base_volume,
                    sound_lufs,
                    sound_true_peak_dbtp,
                },
                "PlayReplace",
            );
        }
        // Local backend: stop_all + play via the command channel (in-process, no IPC race).
        self.stop_all();
        self.play(
            sound_id,
            path,
            base_volume,
            sound_lufs,
            sound_true_peak_dbtp,
        )
    }

    pub fn stop_all(&self) {
        match &self.backend {
            AudioPlayerBackend::Local(local) => {
                let _ = local.command_tx.send(AudioCommand::StopAll);
            }
            AudioPlayerBackend::Remote(_) => {
                let _ = remote_ok(crate::audio::engine_ipc::EngineRequest::StopAll);
            }
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
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
                let (done_tx, done_rx) = mpsc::channel();
                let _ = local.command_tx.send(AudioCommand::Shutdown {
                    policy: local.shutdown_policy,
                    response: done_tx,
                });
                if done_rx.recv_timeout(SHUTDOWN_COMMAND_TIMEOUT).is_err() {
                    warn!(
                        "Audio backend did not complete {:?} shutdown within {} ms",
                        local.shutdown_policy,
                        SHUTDOWN_COMMAND_TIMEOUT.as_millis()
                    );
                }
                let mut handle = local.join_handle.lock();
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
            AudioPlayerBackend::Remote(remote) => {
                remote.stop_poll.store(true, Ordering::Relaxed);
                let mut handle = remote.poll_handle.lock();
                if let Some(handle) = handle.take() {
                    let _ = handle.join();
                }
            }
            #[cfg(test)]
            AudioPlayerBackend::Noop(_) => {}
        }
    }
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.shutdown();
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
                    virtual_mic_module::ensure_virtual_mic_present(&mut state);
                    try_link_feeder_to_virtual_mic(&mut state);
                    ensure_capture_stream_present(&mut state);
                    state.publish_snapshot();
                }
            }
        });
        let _ = graph_watchdog_timer.update_timer(
            Some(Duration::from_millis(200)),
            Some(Duration::from_millis(200)),
        );

        let underrun_timer = mainloop.loop_().add_timer({
            let weak = weak.clone();
            move |_| {
                if let Some(state_rc) = weak.upgrade() {
                    let state = state_rc.borrow();
                    let (local, virt, contention) = state.stream_runtime.snapshot_counters();
                    if local > 0 || virt > 0 || contention > 0 {
                        debug!(
                            "Audio path counters: local_underruns={} virtual_underruns={} mix_lock_contention={}",
                            local, virt, contention
                        );
                    }
                }
            }
        });
        let _ = underrun_timer
            .update_timer(Some(Duration::from_secs(10)), Some(Duration::from_secs(10)));

        let _keep_alive = (
            attached_receiver,
            mix_timer,
            graph_watchdog_timer,
            underrun_timer,
        );
        mainloop.run();
    }
}

#[cfg(test)]
fn test_runtime_config_with_mode(mode: DefaultSourceMode) -> RuntimeConfig {
    RuntimeConfig {
        local_volume: 1.0,
        mic_volume: 1.0,
        mic_passthrough: false,
        mic_source: None,
        default_source_mode: mode,
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

#[cfg(test)]
fn test_player_snapshot_store() -> std::sync::Arc<RwLock<PlayerSnapshot>> {
    std::sync::Arc::new(RwLock::new(PlayerSnapshot::default()))
}

#[cfg(test)]
#[path = "player_tests.rs"]
mod tests;
