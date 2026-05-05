//! PipeWire-backed audio playback with a persistent virtual microphone.

use log::{debug, error, trace, warn};
use pipewire as pw;
use pw::channel as pw_channel;
use pw::properties::properties;
use pw::spa;
use rodio::source::SeekError as RodioSeekError;
use rodio::source::UniformSourceIterator;
use rodio::Source;
use serde::Serialize;
use std::cell::RefCell;
use std::collections::HashMap;
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
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

use crate::app_meta::{
    LOCAL_PLAYBACK_NODE_NAME, MIC_CAPTURE_NODE_NAME, VIRTUAL_MIC_DESCRIPTION,
    VIRTUAL_OUTPUT_DESCRIPTION, VIRTUAL_SOURCE_NAME,
};
use crate::config::{DefaultSourceMode, MicLatencyProfile};

mod command_handlers;
mod limiter;
mod mixing;
mod playback;
mod queues;
mod source_routing;
mod streams;

use command_handlers::{audio_command_kind, handle_audio_command};
use limiter::LookAheadLimiter;
use mixing::{clear_all_queues, clear_output_queues, clear_virtual_mic_queues, mix_tick};
#[cfg(test)]
use mixing::{enqueue_passthrough_chunk, fill_output_queues};
use playback::ActivePlayback;
use queues::ProcessQueues;
use source_routing::{
    apply_default_source_mode, maybe_claim_default_source, recreate_capture_stream,
    restore_default_source,
};
#[cfg(test)]
use source_routing::{parse_wpctl_node_name, resolve_source_id_by_name};
use streams::{create_capture_stream, create_local_output_stream, create_virtual_source_stream};

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

thread_local! {
    static OUTPUT_CALLBACK_SCRATCH: RefCell<Vec<f32>> = RefCell::new(Vec::new());
    static CAPTURE_CALLBACK_SCRATCH: RefCell<Vec<f32>> = RefCell::new(Vec::new());
}

#[derive(Clone, Serialize, Debug)]
pub struct PlaybackPosition {
    pub play_id: String,
    pub sound_id: String,
    pub position_ms: u64,
    pub paused: bool,
    pub finished: bool,
    pub duration_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AudioSourceInfo {
    pub node_name: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Default)]
pub struct PlayerSnapshot {
    pub available: bool,
    pub playback_positions: Vec<PlaybackPosition>,
    pub playing_ids: Vec<String>,
    pub audio_sources: Vec<AudioSourceInfo>,
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
    node_name: String,
    display_name: String,
    is_monitor: bool,
    is_virtual: bool,
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

pub struct AudioPlayer {
    command_tx: pw_channel::Sender<AudioCommand>,
    join_handle: Mutex<Option<thread::JoinHandle<()>>>,
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
}

impl AudioPlayer {
    pub fn new_with_config(config: &crate::config::Config) -> Self {
        let (command_tx, command_rx) = pw_channel::channel();
        let runtime = RuntimeConfig::from_config(config);
        let snapshot = std::sync::Arc::new(RwLock::new(PlayerSnapshot::default()));
        let thread_snapshot = snapshot.clone();
        let handle =
            thread::spawn(move || pipewire_thread_main(command_rx, runtime, thread_snapshot));

        Self {
            command_tx,
            join_handle: Mutex::new(Some(handle)),
            snapshot,
        }
    }

    pub fn snapshot(&self) -> PlayerSnapshot {
        self.snapshot
            .read()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    }

    pub fn set_local_volume(&self, volume: f32) {
        let _ = self.command_tx.send(AudioCommand::SetLocalVolume {
            volume: volume.clamp(0.0, 1.0),
        });
    }

    pub fn set_mic_volume(&self, volume: f32) {
        let _ = self.command_tx.send(AudioCommand::SetMicVolume {
            volume: volume.clamp(0.0, 1.0),
        });
    }

    pub fn set_auto_gain_enabled(&self, enabled: bool) {
        let _ = self
            .command_tx
            .send(AudioCommand::SetAutoGainEnabled { enabled });
    }

    pub fn set_auto_gain_target(&self, target_lufs: f64) {
        let _ = self
            .command_tx
            .send(AudioCommand::SetAutoGainTarget { target_lufs });
    }

    pub fn set_auto_gain_mode(&self, mode: u32) {
        let _ = self.command_tx.send(AudioCommand::SetAutoGainMode { mode });
    }

    pub fn set_auto_gain_apply_to(&self, apply_to: u32) {
        let _ = self
            .command_tx
            .send(AudioCommand::SetAutoGainApplyTo { apply_to });
    }

    pub fn set_auto_gain_dynamic_settings(
        &self,
        lookahead_ms: u32,
        attack_ms: u32,
        release_ms: u32,
    ) {
        let _ = self
            .command_tx
            .send(AudioCommand::SetAutoGainDynamicSettings {
                lookahead_ms,
                attack_ms,
                release_ms,
            });
    }

    pub fn set_looping(&self, enabled: bool) {
        let _ = self.command_tx.send(AudioCommand::SetLooping { enabled });
    }

    pub fn set_mic_passthrough(&self, enabled: bool) -> Result<(), String> {
        let (tx, rx) = mpsc::channel();
        self.command_tx
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
        let (tx, rx) = mpsc::channel();
        self.command_tx
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
        let (tx, rx) = mpsc::channel();
        self.command_tx
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
        let (tx, rx) = mpsc::channel();
        self.command_tx
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

    pub fn play(
        &self,
        sound_id: &str,
        path: &str,
        base_volume: f32,
        sound_lufs: Option<f64>,
    ) -> Result<String, String> {
        let (response_tx, response_rx) = mpsc::channel();
        debug!(
            "Submitting Play command: sound_id={} path={}",
            sound_id, path
        );
        let enqueue_started_at = Instant::now();
        self.command_tx
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
        self.command_tx
            .send(AudioCommand::StopSound {
                sound_id: sound_id.to_string(),
            })
            .map_err(|_| "Audio backend thread is not running".to_string())
    }

    pub fn stop_all(&self) {
        let _ = self.command_tx.send(AudioCommand::StopAll);
    }

    pub fn seek_playback(&self, play_id: &str, position_ms: u64) {
        let _ = self.command_tx.send(AudioCommand::Seek {
            play_id: play_id.to_string(),
            position_ms,
        });
    }

    pub fn pause(&self, sound_id: &str) {
        let _ = self.command_tx.send(AudioCommand::Pause {
            sound_id: sound_id.to_string(),
        });
    }

    pub fn resume(&self, sound_id: &str) {
        let _ = self.command_tx.send(AudioCommand::Resume {
            sound_id: sound_id.to_string(),
        });
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
        let _ = self.command_tx.send(AudioCommand::Shutdown);
        if let Ok(mut handle) = self.join_handle.lock() {
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
}

impl Drop for AudioPlayer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct StreamHandle {
    _stream: pw::stream::StreamRc,
    _listener: pw::stream::StreamListener<()>,
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        if let Err(err) = self._stream.disconnect() {
            debug!("PipeWire stream disconnect during drop failed: {err}");
        }
    }
}

struct BackendState {
    _context: pw::context::ContextRc,
    core: pw::core::CoreRc,
    _registry: pw::registry::RegistryRc,
    _registry_listener: pw::registry::Listener,
    _local_stream: Option<StreamHandle>,
    virtual_stream: Option<StreamHandle>,
    capture_stream: Option<StreamHandle>,
}

struct LoopState {
    runtime: RuntimeConfig,
    available: bool,
    backend: Option<BackendState>,
    sources: HashMap<u32, SourceDescriptor>,
    previous_default_source_name: Option<String>,
    claimed_default: bool,
    active_playback: Option<ActivePlayback>,
    finished_playbacks: HashMap<String, PlaybackSnapshot>,
    next_playback_order: u64,
    queues: std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    stream_runtime: std::sync::Arc<StreamRuntimeShared>,
    ultra_starvation_ticks: u32,
    snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>,
}

impl LoopState {
    fn new(runtime: RuntimeConfig, snapshot: std::sync::Arc<RwLock<PlayerSnapshot>>) -> Self {
        let stream_runtime = std::sync::Arc::new(StreamRuntimeShared::new(&runtime));
        Self {
            runtime,
            available: false,
            backend: None,
            sources: HashMap::new(),
            previous_default_source_name: None,
            claimed_default: false,
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
            .filter(|source| !source.is_monitor && !source.is_virtual)
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

    fn publish_snapshot(&self) {
        self.stream_runtime
            .set_playback_active(self.active_playback.is_some());

        if let Ok(mut snapshot) = self.snapshot.write() {
            *snapshot = PlayerSnapshot {
                available: self.available,
                playback_positions: self.snapshot_positions(),
                playing_ids: self.playing_ids(),
                audio_sources: self.list_audio_sources(),
            };
        }
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
                    state_ref.available = backend.virtual_stream.is_some();
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

        let _keep_alive = (attached_receiver, mix_timer);
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
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|e| e.to_string())?;
    let core = context.connect_rc(None).map_err(|e| e.to_string())?;
    let registry = core.get_registry_rc().map_err(|e| e.to_string())?;

    let registry_listener = registry
        .add_listener_local()
        .global({
            let weak_state = weak_state.clone();
            move |global| {
                if let Some(source) = source_from_global(global) {
                    if let Some(state) = weak_state.upgrade() {
                        let mut state = state.borrow_mut();
                        state.sources.insert(source.id, source);
                        maybe_claim_default_source(&mut state);
                        state.publish_snapshot();
                    }
                }
            }
        })
        .global_remove({
            let weak_state = weak_state.clone();
            move |id| {
                if let Some(state) = weak_state.upgrade() {
                    let mut state = state.borrow_mut();
                    state.sources.remove(&id);
                    state.publish_snapshot();
                }
            }
        })
        .register();

    let local_stream =
        create_local_output_stream(core.clone(), queues.clone(), stream_runtime.clone()).ok();
    let virtual_stream = create_virtual_source_stream(
        core.clone(),
        queues.clone(),
        stream_runtime,
        runtime.pipewire_latency_hint(),
    )
    .ok();

    Ok(BackendState {
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
    if props.get(*pw::keys::MEDIA_CLASS) != Some("Audio/Source") {
        return None;
    }

    let node_name = props.get(*pw::keys::NODE_NAME)?.to_string();
    let display_name = props
        .get(*pw::keys::NODE_DESCRIPTION)
        .or_else(|| props.get("device.description"))
        .unwrap_or(node_name.as_str())
        .to_string();

    Some(SourceDescriptor {
        id: global.id,
        is_monitor: node_name.ends_with(".monitor"),
        is_virtual: node_name == VIRTUAL_SOURCE_NAME,
        node_name,
        display_name,
    })
}

fn clamp_seek_position_ms(position_ms: u64, duration_ms: Option<u64>) -> u64 {
    match duration_ms {
        Some(duration_ms) => position_ms.min(duration_ms),
        None => position_ms,
    }
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

        let track = format
            .default_track()
            .or_else(|| {
                format
                    .tracks()
                    .iter()
                    .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            })
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
        }
    }

    fn test_player_snapshot_store() -> Arc<RwLock<PlayerSnapshot>> {
        Arc::new(RwLock::new(PlayerSnapshot::default()))
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
                node_name: "alsa_input.pci-0000_12_00.6.analog-stereo".to_string(),
                display_name: "Mic".to_string(),
                is_monitor: false,
                is_virtual: false,
            },
        )]);

        assert_eq!(
            resolve_source_id_by_name(&sources, "alsa_input.pci-0000_12_00.6.analog-stereo"),
            Some(7)
        );
        assert_eq!(resolve_source_id_by_name(&sources, "missing"), None);
    }

    #[test]
    fn loop_state_filters_virtual_and_monitor_sources() {
        let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
        state.sources.insert(
            1,
            SourceDescriptor {
                id: 1,
                node_name: "alsa_input.real".to_string(),
                display_name: "Real Mic".to_string(),
                is_monitor: false,
                is_virtual: false,
            },
        );
        state.sources.insert(
            2,
            SourceDescriptor {
                id: 2,
                node_name: "alsa_output.monitor".to_string(),
                display_name: "Monitor".to_string(),
                is_monitor: true,
                is_virtual: false,
            },
        );
        state.sources.insert(
            3,
            SourceDescriptor {
                id: 3,
                node_name: VIRTUAL_SOURCE_NAME.to_string(),
                display_name: VIRTUAL_MIC_DESCRIPTION.to_string(),
                is_monitor: false,
                is_virtual: true,
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
    fn recreate_capture_stream_clears_virtual_mic_queues_even_without_backend() {
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
        assert_eq!(queues.virtual_out.len(), 0);
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
                node_name: "alsa_input.real".to_string(),
                display_name: "Real Mic".to_string(),
                is_monitor: false,
                is_virtual: false,
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

        let (local, virtual_out) = playback.render(512, &runtime);

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
        let _ = playback.render(128, &runtime);

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
