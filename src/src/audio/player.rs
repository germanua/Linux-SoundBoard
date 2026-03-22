//! Audio playback engine with dual output support
//!
//! Seeking implementation follows the pattern from uamp/raplay:
//! - Uses SeekMode::Coarse for reliable MP3/VBR seeking
//! - Tracks actual timestamp from decoder, not calculated from samples
//! - Properly handles decoder reset on seek

use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use log::{debug, error, info, warn};
use rodio::source::{SeekError as RodioSeekError, UniformSourceIterator};
use rodio::Source;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

use crate::app_meta::VIRTUAL_SINK_NAME;
const TARGET_OUTPUT_SAMPLE_RATE: u32 = 48_000;
const FALLBACK_OUTPUT_SAMPLE_RATE: u32 = 44_100;
const TARGET_OUTPUT_CHANNELS: u8 = 2;

/// Timestamp with current position and total duration (following raplay pattern)
#[derive(Clone, Copy, Debug)]
pub struct Timestamp {
    #[allow(dead_code)]
    pub current: Duration,
    pub total: Duration,
}

impl Timestamp {
    pub fn new(current: Duration, total: Duration) -> Self {
        Self { current, total }
    }

    #[allow(dead_code)]
    pub fn total_ms(&self) -> u64 {
        self.total.as_millis() as u64
    }
}

/// Custom audio source that uses symphonia directly with proper error handling
/// Following the uamp/raplay Symph pattern for reliable seeking
struct SymphoniaSource {
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    format: Box<dyn symphonia::core::formats::FormatReader>,
    track_id: u32,
    time_base: Option<TimeBase>,
    n_frames: Option<u64>,
    buffer: SampleBuffer<i16>,
    spec: SignalSpec,
    current_frame_offset: usize,
    /// Last decoded timestamp - tracks actual decoder position (like uamp's last_ts)
    last_ts: u64,
    /// Flag to force buffer refresh after seek
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
            .and_then(|e| e.to_str())
        {
            hint.with_extension(ext);
        }
        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let metadata_opts = MetadataOptions::default();
        let decoder_opts = DecoderOptions::default();

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| format!("Failed to probe: {}", e))?;

        let format = probed.format;

        let track = format
            .default_track()
            .or_else(|| {
                format
                    .tracks()
                    .iter()
                    .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            })
            .ok_or("No audio tracks found")?;

        let track_id = track.id;
        let codec_params = &track.codec_params;
        let rate = codec_params.sample_rate.filter(|&r| r > 0).unwrap_or(44100);
        let time_base = codec_params.time_base;
        let n_frames = codec_params.n_frames;

        let default_channels = symphonia::core::audio::Channels::FRONT_LEFT
            | symphonia::core::audio::Channels::FRONT_RIGHT;
        let mut channels = codec_params.channels.unwrap_or(default_channels);
        if channels.count() == 0 {
            channels = default_channels;
        }

        let spec = SignalSpec { rate, channels };

        let decoder = symphonia::default::get_codecs()
            .make(codec_params, &decoder_opts)
            .map_err(|e| format!("Failed to create decoder: {}", e))?;

        let buffer = SampleBuffer::new(4096, spec);

        debug!(
            "SymphoniaSource created: rate={}, channels={}, time_base={:?}, n_frames={:?}",
            rate,
            channels.count(),
            time_base,
            n_frames
        );

        Ok(Self {
            decoder,
            format,
            track_id,
            time_base,
            n_frames,
            buffer,
            spec,
            current_frame_offset: 0,
            last_ts: 0,
            needs_decode: true,
        })
    }

    /// Seek to position in milliseconds
    /// Following uamp/raplay pattern: use Coarse mode, track actual_ts
    fn seek(&mut self, pos_ms: u64) -> Result<Timestamp, String> {
        let time = Time::new(
            pos_ms / 1000,                   // seconds
            (pos_ms % 1000) as f64 / 1000.0, // fractional seconds
        );

        // Build seek target - prefer TimeStamp when we have time_base and n_frames (like uamp)
        let seek_to = if let (Some(tb), Some(max_frames)) = (self.time_base, self.n_frames) {
            let ts = tb.calc_timestamp(time);
            // Clamp to valid range (max - 1 to avoid seeking past end)
            let clamped_ts = ts.min(max_frames.saturating_sub(1));
            debug!(
                "Seeking with TimeStamp: requested_ts={}, clamped_ts={}, max_frames={}",
                ts, clamped_ts, max_frames
            );
            SeekTo::TimeStamp {
                ts: clamped_ts,
                track_id: self.track_id,
            }
        } else {
            // Fallback to time-based seek
            debug!("Seeking with Time (fallback): {:?}", time);
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            }
        };

        // Use Coarse seek mode - more reliable for VBR MP3 (uamp uses Coarse)
        let seek_result = self.format.seek(SeekMode::Coarse, seek_to);

        match seek_result {
            Ok(seeked_to) => {
                // Update last_ts from ACTUAL seek position (critical for accurate tracking)
                self.last_ts = seeked_to.actual_ts;

                // Clear buffer and force decode on next read
                self.needs_decode = true;
                self.current_frame_offset = 0;

                // Reset decoder state (uamp only resets on ResetRequired, but we do it always for safety)
                self.decoder.reset();

                info!(
                    "Seek successful: requested={}ms, actual_ts={}",
                    pos_ms, seeked_to.actual_ts
                );

                self.get_time()
                    .ok_or_else(|| "Cannot determine timestamp after seek".to_string())
            }
            Err(e) => {
                warn!("Seek failed: {}", e);
                Err(format!("Seek failed: {}", e))
            }
        }
    }

    /// Get current position and total duration (following raplay pattern)
    fn get_time(&self) -> Option<Timestamp> {
        let time_base = self.time_base?;

        let current_time = time_base.calc_time(self.last_ts);
        let current =
            Duration::from_secs(current_time.seconds) + Duration::from_secs_f64(current_time.frac);

        let total = if let Some(n_frames) = self.n_frames {
            let total_time = time_base.calc_time(n_frames);
            Duration::from_secs(total_time.seconds) + Duration::from_secs_f64(total_time.frac)
        } else {
            current // Unknown total, use current as fallback
        };

        Some(Timestamp::new(current, total))
    }

    /// Decode next packet with proper error handling (following uamp pattern)
    fn decode_next_packet(&mut self) -> Option<()> {
        let mut recoverable_packet_errors: usize = 0;
        const MAX_RECOVERABLE_PACKET_ERRORS: usize = 2048;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return None; // End of stream
                }
                Err(SymphoniaError::ResetRequired) => {
                    // Handle reset request (can happen after seek in some formats)
                    debug!("Format reader requested reset");
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::DecodeError(msg)) => {
                    // Recoverable demuxer errors (junk bytes, malformed frames) are common in
                    // real-world MP3 files. Keep scanning packets instead of treating as EOF.
                    recoverable_packet_errors = recoverable_packet_errors.saturating_add(1);
                    if recoverable_packet_errors > MAX_RECOVERABLE_PACKET_ERRORS {
                        warn!(
                            "Too many recoverable demux errors ({}), stopping decode",
                            recoverable_packet_errors
                        );
                        return None;
                    }
                    debug!("Recoverable packet decode error (skipping): {}", msg);
                    continue;
                }
                Err(e) => {
                    recoverable_packet_errors = recoverable_packet_errors.saturating_add(1);
                    if recoverable_packet_errors > MAX_RECOVERABLE_PACKET_ERRORS {
                        warn!(
                            "Too many packet read errors ({}), stopping decode: {}",
                            recoverable_packet_errors, e
                        );
                        return None;
                    }
                    debug!("Packet read error (attempting recovery): {}", e);
                    continue;
                }
            };

            if recoverable_packet_errors > 0 {
                debug!(
                    "Recovered after {} packet read errors",
                    recoverable_packet_errors
                );
                recoverable_packet_errors = 0;
            }

            // Skip packets for other tracks
            if packet.track_id() != self.track_id {
                continue;
            }

            // Update timestamp from packet (tracks actual decoder position)
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
                    // Decoder needs reset (uamp handles this)
                    debug!("Decoder requested reset");
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::DecodeError(msg)) => {
                    // Recoverable decode error - skip this packet (like uamp)
                    debug!("Decode error (skipping packet): {}", msg);
                    continue;
                }
                Err(e) => {
                    debug!("Unrecoverable decode error: {}", e);
                    return None;
                }
            };
        }
    }
}

impl Source for SymphoniaSource {
    fn current_frame_len(&self) -> Option<usize> {
        // IMPORTANT: rodio::UniformSourceIterator snapshots this value to build its internal
        // Take<> chunk. Returning Some(0) before first decode causes a permanent empty stream.
        // Use None for streaming/unknown frame length.
        None
    }
    fn channels(&self) -> u16 {
        self.spec.channels.count() as u16
    }
    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.get_time().map(|t| t.total)
    }
    fn try_seek(&mut self, pos: Duration) -> Result<(), RodioSeekError> {
        self.seek(pos.as_millis() as u64)
            .map(|_| ())
            .map_err(|e| RodioSeekError::Other(Box::new(std::io::Error::other(e))))
    }
}

impl Iterator for SymphoniaSource {
    type Item = i16;
    fn next(&mut self) -> Option<Self::Item> {
        // Force decode if buffer is exhausted or we just seeked
        if self.needs_decode || self.current_frame_offset >= self.buffer.samples().len() {
            self.decode_next_packet()?;
        }
        if self.current_frame_offset < self.buffer.samples().len() {
            let sample = self.buffer.samples()[self.current_frame_offset];
            self.current_frame_offset += 1;
            Some(sample)
        } else {
            None
        }
    }
}

// --- Types & Commands ---

#[derive(Clone, Serialize, Debug)]
pub struct PlaybackPosition {
    pub play_id: String,
    pub sound_id: String,
    pub position_ms: u64,
    pub paused: bool,
    pub finished: bool,
    pub duration_ms: Option<u64>,
}

struct PlaybackSnapshot {
    sound_id: String,
    position_ms: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    duration_ms: Option<u64>,
    is_finished_flag: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmptyBufferOutcome {
    KeepPlaying,
    ErrorNoFrames,
    Finish,
    RestartLoop,
}

fn eof_empty_buffer_outcome(
    source_exhausted: bool,
    tail_flushed: bool,
    fallback_samples_written: u64,
    looping: bool,
) -> EmptyBufferOutcome {
    if fallback_samples_written == 0 {
        return EmptyBufferOutcome::ErrorNoFrames;
    }
    if !source_exhausted || !tail_flushed {
        return EmptyBufferOutcome::KeepPlaying;
    }
    if looping {
        EmptyBufferOutcome::RestartLoop
    } else {
        EmptyBufferOutcome::Finish
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct EofTracker {
    source_exhausted: bool,
    tail_flushed: bool,
}

impl EofTracker {
    fn mark_source_exhausted(&mut self) -> bool {
        let first = !self.source_exhausted;
        self.source_exhausted = true;
        first
    }

    fn should_flush_tail(&self) -> bool {
        self.source_exhausted && !self.tail_flushed
    }

    fn mark_tail_flushed(&mut self) {
        if self.source_exhausted {
            self.tail_flushed = true;
        }
    }

    fn reset(&mut self) {
        self.source_exhausted = false;
        self.tail_flushed = false;
    }

    fn empty_buffer_outcome(
        &self,
        fallback_samples_written: u64,
        looping: bool,
    ) -> EmptyBufferOutcome {
        eof_empty_buffer_outcome(
            self.source_exhausted,
            self.tail_flushed,
            fallback_samples_written,
            looping,
        )
    }
}

fn f32_to_bits(f: f32) -> u32 {
    f.to_bits()
}
fn bits_to_f32(bits: u32) -> f32 {
    f32::from_bits(bits)
}

enum AudioCommand {
    Play {
        sound_id: String,
        path: String,
        base_volume: f32,
        sound_lufs: Option<f64>,
        response: Sender<Result<String, String>>,
    },
    #[allow(dead_code)]
    StopSound {
        sound_id: String,
    },
    StopAll,
    Seek {
        sound_id: String,
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
    #[allow(dead_code)]
    GetPlaying {
        response: Sender<Vec<String>>,
    },
    #[allow(dead_code)]
    IsAvailable {
        response: Sender<bool>,
    },
    GetPlaybackPositions {
        response: Sender<Vec<PlaybackPosition>>,
    },
}

pub struct AudioPlayer {
    command_tx: Sender<AudioCommand>,
}

impl AudioPlayer {
    pub fn new_with_initial_volumes(local_volume: f32, mic_volume: f32) -> Self {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            audio_thread_main(rx, local_volume.clamp(0.0, 1.0), mic_volume.clamp(0.0, 1.0));
        });
        Self { command_tx: tx }
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
    pub fn play(
        &self,
        sound_id: &str,
        path: &str,
        base_volume: f32,
        sound_lufs: Option<f64>,
    ) -> Result<String, String> {
        let (response_tx, response_rx) = mpsc::channel();
        self.command_tx
            .send(AudioCommand::Play {
                sound_id: sound_id.to_string(),
                path: path.to_string(),
                base_volume,
                sound_lufs,
                response: response_tx,
            })
            .map_err(|e| e.to_string())?;
        response_rx.recv().map_err(|e| e.to_string())?
    }
    #[allow(dead_code)]
    pub fn stop_sound(&self, sound_id: &str) -> Result<(), String> {
        self.command_tx
            .send(AudioCommand::StopSound {
                sound_id: sound_id.to_string(),
            })
            .map_err(|e| e.to_string())
    }
    pub fn stop_all(&self) {
        let _ = self.command_tx.send(AudioCommand::StopAll);
    }
    pub fn seek(&self, sound_id: &str, position_ms: u64) {
        let _ = self.command_tx.send(AudioCommand::Seek {
            sound_id: sound_id.to_string(),
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

    #[allow(dead_code)]
    pub fn get_playing(&self) -> Vec<String> {
        let (response_tx, response_rx) = mpsc::channel();
        if self
            .command_tx
            .send(AudioCommand::GetPlaying {
                response: response_tx,
            })
            .is_err()
        {
            return vec![];
        }
        response_rx.recv().unwrap_or_default()
    }

    #[allow(dead_code)]
    pub fn is_available(&self) -> bool {
        let (response_tx, response_rx) = mpsc::channel();
        if self
            .command_tx
            .send(AudioCommand::IsAvailable {
                response: response_tx,
            })
            .is_err()
        {
            return false;
        }
        response_rx.recv().unwrap_or(false)
    }

    pub fn get_playback_positions(&self) -> Vec<PlaybackPosition> {
        let (response_tx, response_rx) = mpsc::channel();
        if self
            .command_tx
            .send(AudioCommand::GetPlaybackPositions {
                response: response_tx,
            })
            .is_err()
        {
            return vec![];
        }
        response_rx.recv().unwrap_or_default()
    }
}

trait PulseVolume {
    fn get(&self) -> f32;
}
struct SharedVolume {
    volume_bits: AtomicU32,
}
impl SharedVolume {
    fn new(volume: f32) -> Self {
        Self {
            volume_bits: AtomicU32::new(f32_to_bits(volume)),
        }
    }
    fn get(&self) -> f32 {
        bits_to_f32(self.volume_bits.load(Ordering::Relaxed))
    }
    fn set(&self, volume: f32) {
        self.volume_bits
            .store(f32_to_bits(volume), Ordering::Relaxed);
    }
}
impl PulseVolume for SharedVolume {
    fn get(&self) -> f32 {
        SharedVolume::get(self)
    }
}
impl PulseVolume for Arc<SharedVolume> {
    fn get(&self) -> f32 {
        self.as_ref().get()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoGainMode {
    Static = 0,
    DynamicLookAhead = 1,
}

impl AutoGainMode {
    fn from_u32(value: u32) -> Self {
        match value {
            1 => AutoGainMode::DynamicLookAhead,
            _ => AutoGainMode::Static,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AutoGainApplyTo {
    Both = 0,
    MicOnly = 1,
}

impl AutoGainApplyTo {
    fn from_u32(value: u32) -> Self {
        match value {
            1 => AutoGainApplyTo::MicOnly,
            _ => AutoGainApplyTo::Both,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AutoGainDynamicParams {
    lookahead_ms: u32,
    attack_ms: u32,
    release_ms: u32,
}

/// Shared auto-gain state readable by all playback threads in real-time.
/// Uses atomics so UI changes are immediately reflected in currently-playing sounds.
pub struct SharedAutoGain {
    enabled: AtomicBool,
    mode_bits: AtomicU32,
    apply_to_bits: AtomicU32,
    /// Target LUFS stored as f64 bits in an AtomicU64
    target_lufs_bits: AtomicU64,
    lookahead_ms: AtomicU32,
    attack_ms: AtomicU32,
    release_ms: AtomicU32,
}

impl SharedAutoGain {
    fn new(enabled: bool, target_lufs: f64) -> Self {
        Self {
            enabled: AtomicBool::new(enabled),
            mode_bits: AtomicU32::new(AutoGainMode::Static as u32),
            apply_to_bits: AtomicU32::new(AutoGainApplyTo::Both as u32),
            target_lufs_bits: AtomicU64::new(target_lufs.to_bits()),
            lookahead_ms: AtomicU32::new(30),
            attack_ms: AtomicU32::new(6),
            release_ms: AtomicU32::new(150),
        }
    }
    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
    fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }
    fn mode(&self) -> AutoGainMode {
        AutoGainMode::from_u32(self.mode_bits.load(Ordering::Relaxed))
    }
    fn set_mode(&self, mode: AutoGainMode) {
        self.mode_bits.store(mode as u32, Ordering::Relaxed);
    }
    fn apply_to(&self) -> AutoGainApplyTo {
        AutoGainApplyTo::from_u32(self.apply_to_bits.load(Ordering::Relaxed))
    }
    fn set_apply_to(&self, apply_to: AutoGainApplyTo) {
        self.apply_to_bits.store(apply_to as u32, Ordering::Relaxed);
    }
    fn applies_to_output(&self, is_virtual_output: bool) -> bool {
        match self.apply_to() {
            AutoGainApplyTo::Both => true,
            AutoGainApplyTo::MicOnly => is_virtual_output,
        }
    }
    fn target_lufs(&self) -> f64 {
        f64::from_bits(self.target_lufs_bits.load(Ordering::Relaxed))
    }
    fn set_target_lufs(&self, target: f64) {
        self.target_lufs_bits
            .store(target.to_bits(), Ordering::Relaxed);
    }
    fn set_dynamic_params(&self, lookahead_ms: u32, attack_ms: u32, release_ms: u32) {
        self.lookahead_ms.store(lookahead_ms, Ordering::Relaxed);
        self.attack_ms.store(attack_ms, Ordering::Relaxed);
        self.release_ms.store(release_ms, Ordering::Relaxed);
    }
    fn dynamic_params(&self) -> AutoGainDynamicParams {
        AutoGainDynamicParams {
            lookahead_ms: self.lookahead_ms.load(Ordering::Relaxed),
            attack_ms: self.attack_ms.load(Ordering::Relaxed),
            release_ms: self.release_ms.load(Ordering::Relaxed),
        }
    }
    /// Compute the gain factor for a sound with the given LUFS measurement.
    /// Returns 1.0 if auto-gain should not apply on this output.
    fn gain_for(&self, sound_lufs: Option<f64>, is_virtual_output: bool) -> f32 {
        if !self.is_enabled() {
            return 1.0;
        }
        if !self.applies_to_output(is_virtual_output) {
            return 1.0;
        }
        match sound_lufs {
            Some(lufs) => crate::audio::loudness::compute_gain_factor(lufs, self.target_lufs()),
            None => 1.0,
        }
    }
}

struct LookAheadLimiter {
    buffer: Vec<f32>,
    write_idx: usize,
    filled: usize,
    lookahead_samples: usize,
    current_gain: f32,
    target_gain: f32,
    attack_samples: usize,
    release_samples: usize,
    update_interval: usize,
    update_counter: usize,
    target_peak: f32,
}

impl LookAheadLimiter {
    fn new(sample_rate: u32, channels: u16, params: AutoGainDynamicParams) -> Self {
        let samples_per_ms = (sample_rate as f32 * channels as f32) / 1000.0;
        let lookahead_samples = (params.lookahead_ms as f32 * samples_per_ms)
            .round()
            .max(1.0) as usize;
        let attack_samples = (params.attack_ms as f32 * samples_per_ms).round().max(1.0) as usize;
        let release_samples = (params.release_ms as f32 * samples_per_ms).round().max(1.0) as usize;
        let buffer = vec![0.0_f32; lookahead_samples + 1];
        Self {
            buffer,
            write_idx: 0,
            filled: 0,
            lookahead_samples,
            current_gain: 1.0,
            target_gain: 1.0,
            attack_samples,
            release_samples,
            update_interval: 256,
            update_counter: 0,
            target_peak: 0.98 * 32767.0,
        }
    }

    fn process(&mut self, sample: f32) -> Option<f32> {
        self.buffer[self.write_idx] = sample;
        self.write_idx = (self.write_idx + 1) % self.buffer.len();
        if self.filled < self.buffer.len() {
            self.filled += 1;
        }

        self.update_counter += 1;
        if self.update_counter >= self.update_interval {
            self.update_counter = 0;
            let mut peak = 0.0_f32;
            let scan_len = self.filled.min(self.buffer.len());
            for idx in 0..scan_len {
                let value = self.buffer[idx].abs();
                if value > peak {
                    peak = value;
                }
            }
            if peak > 0.0 {
                self.target_gain = (self.target_peak / peak).min(1.0);
            } else {
                self.target_gain = 1.0;
            }
        }

        if self.target_gain < self.current_gain {
            let step = (self.current_gain - self.target_gain) / self.attack_samples as f32;
            self.current_gain = (self.current_gain - step).max(self.target_gain);
        } else if self.target_gain > self.current_gain {
            let step = (self.target_gain - self.current_gain) / self.release_samples as f32;
            self.current_gain = (self.current_gain + step).min(self.target_gain);
        }

        if self.filled <= self.lookahead_samples {
            return None;
        }

        let read_idx = if self.write_idx >= self.lookahead_samples {
            self.write_idx - self.lookahead_samples
        } else {
            self.buffer.len() + self.write_idx - self.lookahead_samples
        };
        let delayed = self.buffer[read_idx];
        Some(delayed * self.current_gain)
    }

    fn flush(&mut self) -> Vec<f32> {
        let mut out = Vec::new();
        for _ in 0..self.lookahead_samples {
            if let Some(sample) = self.process(0.0) {
                out.push(sample);
            }
        }
        out
    }
}

enum PlaybackMsg {
    Stop,
    Seek(u64),
    Pause,
    Resume,
}

struct PlayingSound {
    local_tx: Option<Sender<PlaybackMsg>>,
    virtual_tx: Option<Sender<PlaybackMsg>>,
    sound_id: String,
    position_ms: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
    _local_thread: Option<thread::JoinHandle<()>>,
    _virtual_thread: Option<thread::JoinHandle<()>>,
    is_finished_flag: Arc<AtomicBool>,
}

struct PlaybackSharedState {
    local_volume: Arc<SharedVolume>,
    mic_volume: Arc<SharedVolume>,
    auto_gain: Arc<SharedAutoGain>,
    looping: Arc<AtomicBool>,
    registry: Arc<Mutex<HashMap<String, PlaybackSnapshot>>>,
}

struct PlayRequest<'a> {
    local_available: bool,
    virtual_available: bool,
    sound_id: &'a str,
    path: &'a str,
    base_volume: f32,
    sound_lufs: Option<f64>,
    shared: PlaybackSharedState,
}

struct PlaybackThreadContext {
    file_path: Arc<str>,
    local_enabled: bool,
    virtual_enabled: bool,
    base_volume: f32,
    sound_lufs: Option<f64>,
    shared_local_volume: Arc<SharedVolume>,
    shared_mic_volume: Arc<SharedVolume>,
    shared_auto_gain: Arc<SharedAutoGain>,
    shared_loop: Arc<AtomicBool>,
    control_rx: Receiver<PlaybackMsg>,
    position_ms: Arc<AtomicU64>,
    paused: Arc<AtomicBool>,
}

impl PlayingSound {
    fn is_finished(&self) -> bool {
        self.is_finished_flag.load(Ordering::Relaxed)
    }
    fn stop(&self) {
        if let Some(tx) = &self.local_tx {
            let _ = tx.send(PlaybackMsg::Stop);
        }
        if let Some(tx) = &self.virtual_tx {
            let _ = tx.send(PlaybackMsg::Stop);
        }
    }
    fn seek(&self, pos_ms: u64) {
        if let Some(tx) = &self.local_tx {
            let _ = tx.send(PlaybackMsg::Seek(pos_ms));
        }
        if let Some(tx) = &self.virtual_tx {
            let _ = tx.send(PlaybackMsg::Seek(pos_ms));
        }
        self.position_ms.store(pos_ms, Ordering::Relaxed);
    }
    fn pause(&self) {
        if let Some(tx) = &self.local_tx {
            let _ = tx.send(PlaybackMsg::Pause);
        }
        if let Some(tx) = &self.virtual_tx {
            let _ = tx.send(PlaybackMsg::Pause);
        }
        self.paused.store(true, Ordering::Relaxed);
    }
    fn resume(&self) {
        if let Some(tx) = &self.local_tx {
            let _ = tx.send(PlaybackMsg::Resume);
        }
        if let Some(tx) = &self.virtual_tx {
            let _ = tx.send(PlaybackMsg::Resume);
        }
        self.paused.store(false, Ordering::Relaxed);
    }
}

fn audio_thread_main(
    rx: Receiver<AudioCommand>,
    initial_local_volume: f32,
    initial_mic_volume: f32,
) {
    let local_available = test_default_sink_available();
    let virtual_available = test_virtual_sink_available();

    let mut playing: HashMap<String, PlayingSound> = HashMap::new();
    let positions_registry: Arc<Mutex<HashMap<String, PlaybackSnapshot>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let shared_mic_volume = Arc::new(SharedVolume::new(initial_mic_volume));
    let shared_local_volume = Arc::new(SharedVolume::new(initial_local_volume));
    let shared_auto_gain = Arc::new(SharedAutoGain::new(false, -14.0));
    let shared_loop = Arc::new(AtomicBool::new(false));

    info!("Audio thread started");
    crate::diagnostics::memory::log_memory_snapshot("audio_thread:start");

    while let Ok(cmd) = rx.recv() {
        playing.retain(|_, ps| !ps.is_finished());
        // Clean registry
        {
            let mut reg = positions_registry.lock().unwrap();
            reg.retain(|_, snap| !snap.is_finished_flag.load(Ordering::Relaxed));
        }

        match cmd {
            AudioCommand::Play {
                sound_id,
                path,
                base_volume,
                sound_lufs,
                response,
            } => {
                crate::diagnostics::memory::log_memory_snapshot("audio_cmd:play:before");
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    play_dual_output(PlayRequest {
                        local_available,
                        virtual_available,
                        sound_id: &sound_id,
                        path: &path,
                        base_volume,
                        sound_lufs,
                        shared: PlaybackSharedState {
                            local_volume: Arc::clone(&shared_local_volume),
                            mic_volume: Arc::clone(&shared_mic_volume),
                            auto_gain: Arc::clone(&shared_auto_gain),
                            looping: Arc::clone(&shared_loop),
                            registry: Arc::clone(&positions_registry),
                        },
                    })
                }));
                match result {
                    Ok(Ok((play_id, playing_sound))) => {
                        playing.insert(play_id.clone(), playing_sound);
                        crate::diagnostics::memory::log_memory_snapshot(
                            "audio_cmd:play:after_success",
                        );
                        let _ = response.send(Ok(play_id));
                    }
                    Ok(Err(e)) => {
                        crate::diagnostics::memory::log_memory_snapshot(
                            "audio_cmd:play:after_error",
                        );
                        let _ = response.send(Err(e));
                    }
                    Err(_) => {
                        crate::diagnostics::memory::log_memory_snapshot("audio_cmd:play:panic");
                        let _ = response.send(Err("Audio playback internal error".to_string()));
                    }
                }
            }
            AudioCommand::StopSound { sound_id } => {
                for id in playing
                    .iter()
                    .filter(|(_, ps)| ps.sound_id == sound_id)
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>()
                {
                    if let Some(ps) = playing.remove(&id) {
                        ps.stop();
                    }
                }
            }
            AudioCommand::StopAll => {
                for (_, ps) in playing.drain() {
                    ps.stop();
                }
                crate::diagnostics::memory::log_memory_snapshot("audio_cmd:stop_all");
            }
            AudioCommand::Seek {
                sound_id,
                position_ms,
            } => {
                let mut found = false;
                for ps in playing.values() {
                    if ps.sound_id == sound_id {
                        info!(
                            "Sending seek message to sound_id={}, position_ms={}",
                            sound_id, position_ms
                        );
                        ps.seek(position_ms);
                        found = true;
                    }
                }
                if !found {
                    warn!(
                        "Seek: No playing sound found with sound_id={}. Playing sounds: {:?}",
                        sound_id,
                        playing.values().map(|ps| &ps.sound_id).collect::<Vec<_>>()
                    );
                }
            }
            AudioCommand::Pause { sound_id } => {
                for ps in playing.values() {
                    if ps.sound_id == sound_id {
                        ps.pause();
                    }
                }
            }
            AudioCommand::Resume { sound_id } => {
                for ps in playing.values() {
                    if ps.sound_id == sound_id {
                        ps.resume();
                    }
                }
            }

            AudioCommand::SetLocalVolume { volume } => shared_local_volume.set(volume),
            AudioCommand::SetMicVolume { volume } => shared_mic_volume.set(volume),
            AudioCommand::SetAutoGainEnabled { enabled } => shared_auto_gain.set_enabled(enabled),
            AudioCommand::SetAutoGainTarget { target_lufs } => {
                shared_auto_gain.set_target_lufs(target_lufs)
            }
            AudioCommand::SetAutoGainMode { mode } => {
                shared_auto_gain.set_mode(AutoGainMode::from_u32(mode))
            }
            AudioCommand::SetAutoGainApplyTo { apply_to } => {
                shared_auto_gain.set_apply_to(AutoGainApplyTo::from_u32(apply_to))
            }
            AudioCommand::SetAutoGainDynamicSettings {
                lookahead_ms,
                attack_ms,
                release_ms,
            } => shared_auto_gain.set_dynamic_params(lookahead_ms, attack_ms, release_ms),
            AudioCommand::SetLooping { enabled } => shared_loop.store(enabled, Ordering::Relaxed),
            AudioCommand::GetPlaying { response } => {
                let ids: Vec<String> = playing.values().map(|ps| ps.sound_id.clone()).collect();
                let _ = response.send(ids);
            }
            AudioCommand::IsAvailable { response } => {
                let _ = response.send(local_available || virtual_available);
            }

            AudioCommand::GetPlaybackPositions { response } => {
                let mut res = Vec::new();
                if let Ok(reg) = positions_registry.lock() {
                    for (play_id, snap) in reg.iter() {
                        res.push(PlaybackPosition {
                            play_id: play_id.clone(),
                            sound_id: snap.sound_id.clone(),
                            position_ms: snap.position_ms.load(Ordering::Relaxed),
                            paused: snap.paused.load(Ordering::Relaxed),
                            finished: snap.is_finished_flag.load(Ordering::Relaxed),
                            duration_ms: snap.duration_ms,
                        });
                    }
                }
                let _ = response.send(res);
            }
        }
    }
}

fn test_default_sink_available() -> bool {
    test_sink_available(None)
}
fn test_virtual_sink_available() -> bool {
    test_sink_available(Some(VIRTUAL_SINK_NAME))
}

fn test_sink_available(device_name: Option<&str>) -> bool {
    for rate in [TARGET_OUTPUT_SAMPLE_RATE, FALLBACK_OUTPUT_SAMPLE_RATE] {
        let spec = Spec {
            format: Format::S16le,
            channels: TARGET_OUTPUT_CHANNELS,
            rate,
        };
        if Simple::new(
            None,
            "LinuxSoundboard",
            Direction::Playback,
            device_name,
            "test",
            &spec,
            None,
            None,
        )
        .is_ok()
        {
            return true;
        }
    }
    false
}

fn open_pulse_outputs_for_rate(
    rate: u32,
    channels: u8,
    local_enabled: bool,
    virtual_enabled: bool,
) -> (Option<Simple>, Option<Simple>) {
    let spec = Spec {
        format: Format::S16le,
        channels,
        rate,
    };

    let local_pulse = if local_enabled {
        match Simple::new(
            None,
            "LinuxSoundboard",
            Direction::Playback,
            None,
            "Soundboard Audio (local)",
            &spec,
            None,
            None,
        ) {
            Ok(p) => Some(p),
            Err(e) => {
                warn!(
                    "Local sink connect failed at {} Hz / {} ch: {:?}",
                    rate, channels, e
                );
                None
            }
        }
    } else {
        None
    };

    let virtual_pulse = if virtual_enabled {
        match Simple::new(
            None,
            "LinuxSoundboard",
            Direction::Playback,
            Some(VIRTUAL_SINK_NAME),
            "Soundboard Audio (virtual)",
            &spec,
            None,
            None,
        ) {
            Ok(p) => Some(p),
            Err(e) => {
                warn!(
                    "Virtual sink connect failed at {} Hz / {} ch: {:?}",
                    rate, channels, e
                );
                None
            }
        }
    } else {
        None
    };

    (local_pulse, virtual_pulse)
}

fn open_pulse_outputs_with_fallback(
    local_enabled: bool,
    virtual_enabled: bool,
) -> Result<(u32, u8, Option<Simple>, Option<Simple>), String> {
    for rate in [TARGET_OUTPUT_SAMPLE_RATE, FALLBACK_OUTPUT_SAMPLE_RATE] {
        let (local_pulse, virtual_pulse) = open_pulse_outputs_for_rate(
            rate,
            TARGET_OUTPUT_CHANNELS,
            local_enabled,
            virtual_enabled,
        );
        if local_pulse.is_some() || virtual_pulse.is_some() {
            return Ok((rate, TARGET_OUTPUT_CHANNELS, local_pulse, virtual_pulse));
        }
    }

    Err(format!(
        "Pulse connect failed for all outputs at {} Hz and {} Hz",
        TARGET_OUTPUT_SAMPLE_RATE, FALLBACK_OUTPUT_SAMPLE_RATE
    ))
}

fn play_dual_output(request: PlayRequest<'_>) -> Result<(String, PlayingSound), String> {
    crate::diagnostics::memory::log_memory_snapshot("play_dual_output:start");
    let _ = std::fs::File::open(request.path).map_err(|e| format!("Failed to open file: {e}"))?;
    let file_path: Arc<str> = Arc::from(request.path);

    let finished_flag = Arc::new(AtomicBool::new(false));
    let position_ms = Arc::new(AtomicU64::new(0));
    let paused = Arc::new(AtomicBool::new(false));

    let duration_ms = probe_duration_ms(request.path);

    let (tx, rx) = mpsc::channel();
    let path = Arc::clone(&file_path);
    let flag = Arc::clone(&finished_flag);
    let thread_context = PlaybackThreadContext {
        file_path: path,
        local_enabled: true,
        virtual_enabled: true,
        base_volume: request.base_volume,
        sound_lufs: request.sound_lufs,
        shared_local_volume: Arc::clone(&request.shared.local_volume),
        shared_mic_volume: Arc::clone(&request.shared.mic_volume),
        shared_auto_gain: Arc::clone(&request.shared.auto_gain),
        shared_loop: Arc::clone(&request.shared.looping),
        control_rx: rx,
        position_ms: Arc::clone(&position_ms),
        paused: Arc::clone(&paused),
    };
    if !request.local_available && !request.virtual_available {
        warn!(
            "Startup sink probes reported unavailable; attempting real playback stream opens anyway"
        );
    }
    let handle = thread::spawn(move || {
        crate::diagnostics::memory::log_memory_snapshot("play_thread:start");
        if let Err(e) = play_to_pulse_sinks_dynamic(thread_context) {
            error!("Playback error: {}", e);
        }
        crate::diagnostics::memory::log_memory_snapshot("play_thread:end");
        flag.store(true, Ordering::SeqCst);
    });

    let play_id = uuid::Uuid::new_v4().to_string();

    {
        let mut reg = request.shared.registry.lock().unwrap();
        reg.insert(
            play_id.clone(),
            PlaybackSnapshot {
                sound_id: request.sound_id.to_string(),
                position_ms: Arc::clone(&position_ms),
                paused: Arc::clone(&paused),
                duration_ms,
                is_finished_flag: Arc::clone(&finished_flag),
            },
        );
    }

    Ok((
        play_id,
        PlayingSound {
            local_tx: Some(tx),
            virtual_tx: None,
            sound_id: request.sound_id.to_string(),
            position_ms,
            paused,
            _local_thread: Some(handle),
            _virtual_thread: None,
            is_finished_flag: finished_flag,
        },
    ))
}

fn play_to_pulse_sinks_dynamic(context: PlaybackThreadContext) -> Result<(), String> {
    crate::diagnostics::memory::log_memory_snapshot("playback_loop:start");
    let (output_rate, output_channels, mut local_pulse, mut virtual_pulse) =
        open_pulse_outputs_with_fallback(context.local_enabled, context.virtual_enabled)?;
    info!(
        "Playback stream ready: rate={}Hz channels={} local={} virtual={}",
        output_rate,
        output_channels,
        local_pulse.is_some(),
        virtual_pulse.is_some()
    );

    let source = SymphoniaSource::from_path(&context.file_path)?;
    let mut source = UniformSourceIterator::<SymphoniaSource, i16>::new(
        source,
        output_channels as u16,
        output_rate,
    );

    let mut local_buffer: Vec<i16> = Vec::with_capacity(4096);
    let mut virtual_buffer: Vec<i16> = Vec::with_capacity(4096);
    let mut is_paused = false;

    // Position tracking using decoder's actual timestamp (like uamp)
    // We track output samples for position updates.
    let channels = source.channels() as u64;
    let rate = source.sample_rate() as u64;
    if channels == 0 || rate == 0 {
        return Err("Invalid output stream parameters".to_string());
    }
    let mut fallback_samples_written: u64 = 0;
    let mut eof_tracker = EofTracker::default();
    let mut local_limiter: Option<LookAheadLimiter> = None;
    let mut virtual_limiter: Option<LookAheadLimiter> = None;
    let mut last_dynamic_mode = context.shared_auto_gain.mode();
    let mut last_dynamic_params = context.shared_auto_gain.dynamic_params();
    if context.shared_auto_gain.is_enabled() && last_dynamic_mode == AutoGainMode::DynamicLookAhead
    {
        if local_pulse.is_some() && context.shared_auto_gain.applies_to_output(false) {
            local_limiter = Some(LookAheadLimiter::new(
                rate as u32,
                channels as u16,
                last_dynamic_params,
            ));
        }
        if virtual_pulse.is_some() && context.shared_auto_gain.applies_to_output(true) {
            virtual_limiter = Some(LookAheadLimiter::new(
                rate as u32,
                channels as u16,
                last_dynamic_params,
            ));
        }
    }

    'playback: loop {
        while let Ok(msg) = context.control_rx.try_recv() {
            match msg {
                PlaybackMsg::Stop => break 'playback,
                PlaybackMsg::Seek(ms) => {
                    // Reduced logging for better performance during rapid seeking
                    if let Some(pulse) = local_pulse.as_ref() {
                        let _ = pulse.flush();
                    }
                    if let Some(pulse) = virtual_pulse.as_ref() {
                        let _ = pulse.flush();
                    }

                    match source.try_seek(Duration::from_millis(ms)) {
                        Ok(_) => {
                            local_buffer.clear();
                            virtual_buffer.clear();

                            // UniformSourceIterator seek uses requested timeline.
                            context.position_ms.store(ms, Ordering::Relaxed);

                            // Sync fallback counter to decoder position
                            fallback_samples_written = (ms * rate * channels) / 1000;
                            eof_tracker.reset();
                        }
                        Err(e) => {
                            warn!("Seek failed: {}", e);
                            // Don't update position if seek failed
                        }
                    }
                }
                PlaybackMsg::Pause => {
                    is_paused = true;
                    context.paused.store(true, Ordering::Relaxed);
                }
                PlaybackMsg::Resume => {
                    is_paused = false;
                    context.paused.store(false, Ordering::Relaxed);
                }
            }
        }

        if is_paused {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if local_pulse.is_none() && virtual_pulse.is_none() {
            return Err("All outputs became unavailable during playback".to_string());
        }

        let mut current_local_volume = context.shared_local_volume.get();
        let mut current_mic_volume = context.shared_mic_volume.get();
        let mut local_auto_gain_factor =
            context.shared_auto_gain.gain_for(context.sound_lufs, false);
        let mut virtual_auto_gain_factor =
            context.shared_auto_gain.gain_for(context.sound_lufs, true);
        let mut frames_processed = 0;
        let mut samples_decoded_this_cycle: u64 = 0;
        let mut reached_end = false;

        while (local_pulse.is_some() && local_buffer.len() < 4096)
            || (virtual_pulse.is_some() && virtual_buffer.len() < 4096)
        {
            if let Some(sample) = source.next() {
                samples_decoded_this_cycle = samples_decoded_this_cycle.saturating_add(1);
                frames_processed += 1;
                if frames_processed > 1000 {
                    current_local_volume = context.shared_local_volume.get();
                    current_mic_volume = context.shared_mic_volume.get();
                    local_auto_gain_factor =
                        context.shared_auto_gain.gain_for(context.sound_lufs, false);
                    virtual_auto_gain_factor =
                        context.shared_auto_gain.gain_for(context.sound_lufs, true);
                    let new_mode = context.shared_auto_gain.mode();
                    let new_params = context.shared_auto_gain.dynamic_params();
                    let local_dynamic_enabled = local_pulse.is_some()
                        && context.shared_auto_gain.is_enabled()
                        && context.shared_auto_gain.applies_to_output(false)
                        && new_mode == AutoGainMode::DynamicLookAhead;
                    if local_dynamic_enabled {
                        if local_limiter.is_none()
                            || new_mode != last_dynamic_mode
                            || new_params != last_dynamic_params
                        {
                            local_limiter = Some(LookAheadLimiter::new(
                                rate as u32,
                                channels as u16,
                                new_params,
                            ));
                        }
                    } else {
                        local_limiter = None;
                    }
                    let virtual_dynamic_enabled = virtual_pulse.is_some()
                        && context.shared_auto_gain.is_enabled()
                        && context.shared_auto_gain.applies_to_output(true)
                        && new_mode == AutoGainMode::DynamicLookAhead;
                    if virtual_dynamic_enabled {
                        if virtual_limiter.is_none()
                            || new_mode != last_dynamic_mode
                            || new_params != last_dynamic_params
                        {
                            virtual_limiter = Some(LookAheadLimiter::new(
                                rate as u32,
                                channels as u16,
                                new_params,
                            ));
                        }
                    } else {
                        virtual_limiter = None;
                    }
                    last_dynamic_mode = new_mode;
                    last_dynamic_params = new_params;
                    frames_processed = 0;
                }
                if local_pulse.is_some() {
                    let local_scaled = sample as f32
                        * context.base_volume
                        * current_local_volume
                        * local_auto_gain_factor;
                    if let Some(limiter) = local_limiter.as_mut() {
                        if let Some(limited) = limiter.process(local_scaled) {
                            let clamped = limited.clamp(-32768.0, 32767.0);
                            local_buffer.push(clamped as i16);
                        }
                    } else {
                        let clamped = local_scaled.clamp(-32768.0, 32767.0);
                        local_buffer.push(clamped as i16);
                    }
                }
                if virtual_pulse.is_some() {
                    let virtual_scaled = sample as f32
                        * context.base_volume
                        * current_mic_volume
                        * virtual_auto_gain_factor;
                    if let Some(limiter) = virtual_limiter.as_mut() {
                        if let Some(limited) = limiter.process(virtual_scaled) {
                            let clamped = limited.clamp(-32768.0, 32767.0);
                            virtual_buffer.push(clamped as i16);
                        }
                    } else {
                        let clamped = virtual_scaled.clamp(-32768.0, 32767.0);
                        virtual_buffer.push(clamped as i16);
                    }
                }
            } else {
                if eof_tracker.mark_source_exhausted() {
                    debug!(
                        "Playback source exhausted for '{}'; flushing limiter tail once",
                        context.file_path
                    );
                }
                reached_end = true;
                break;
            }
        }

        if reached_end && eof_tracker.should_flush_tail() {
            let mut local_tail_samples = 0usize;
            if let Some(limiter) = local_limiter.as_mut() {
                for sample in limiter.flush() {
                    let clamped = sample.clamp(-32768.0, 32767.0);
                    local_buffer.push(clamped as i16);
                    local_tail_samples = local_tail_samples.saturating_add(1);
                }
            }

            let mut virtual_tail_samples = 0usize;
            if let Some(limiter) = virtual_limiter.as_mut() {
                for sample in limiter.flush() {
                    let clamped = sample.clamp(-32768.0, 32767.0);
                    virtual_buffer.push(clamped as i16);
                    virtual_tail_samples = virtual_tail_samples.saturating_add(1);
                }
            }

            eof_tracker.mark_tail_flushed();
            debug!(
                "EOF tail flush complete for '{}': local={} virtual={}",
                context.file_path, local_tail_samples, virtual_tail_samples
            );
        }

        if local_buffer.is_empty() && virtual_buffer.is_empty() {
            let looping = context.shared_loop.load(Ordering::Relaxed);
            match eof_tracker.empty_buffer_outcome(fallback_samples_written, looping) {
                EmptyBufferOutcome::ErrorNoFrames => {
                    return Err(format!(
                        "No decodable audio frames found in '{}'",
                        context.file_path
                    ));
                }
                EmptyBufferOutcome::KeepPlaying => continue 'playback,
                EmptyBufferOutcome::Finish => {
                    debug!(
                        "Playback reached EOF and finished for '{}'",
                        context.file_path
                    );
                    break;
                }
                EmptyBufferOutcome::RestartLoop => {
                    debug!(
                        "Playback reached EOF in loop mode for '{}'; seeking to start",
                        context.file_path
                    );
                    if let Some(pulse) = local_pulse.as_ref() {
                        let _ = pulse.drain();
                    }
                    if let Some(pulse) = virtual_pulse.as_ref() {
                        let _ = pulse.drain();
                    }

                    match source.try_seek(Duration::from_millis(0)) {
                        Ok(_) => {
                            fallback_samples_written = 0;
                            context.position_ms.store(0, Ordering::Relaxed);
                            eof_tracker.reset();

                            let restart_mode = context.shared_auto_gain.mode();
                            let restart_params = context.shared_auto_gain.dynamic_params();
                            last_dynamic_mode = restart_mode;
                            last_dynamic_params = restart_params;
                            local_limiter = if local_pulse.is_some()
                                && context.shared_auto_gain.is_enabled()
                                && context.shared_auto_gain.applies_to_output(false)
                                && restart_mode == AutoGainMode::DynamicLookAhead
                            {
                                Some(LookAheadLimiter::new(
                                    rate as u32,
                                    channels as u16,
                                    restart_params,
                                ))
                            } else {
                                None
                            };
                            virtual_limiter = if virtual_pulse.is_some()
                                && context.shared_auto_gain.is_enabled()
                                && context.shared_auto_gain.applies_to_output(true)
                                && restart_mode == AutoGainMode::DynamicLookAhead
                            {
                                Some(LookAheadLimiter::new(
                                    rate as u32,
                                    channels as u16,
                                    restart_params,
                                ))
                            } else {
                                None
                            };

                            debug!("Loop: restarting from beginning");
                            continue 'playback;
                        }
                        Err(e) => {
                            warn!("Loop seek-to-0 failed: {}", e);
                            break;
                        }
                    }
                }
            }
        }

        let local_write_err = if !local_buffer.is_empty() {
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    local_buffer.as_ptr() as *const u8,
                    local_buffer.len() * 2,
                )
            };
            local_pulse
                .as_ref()
                .and_then(|pulse| pulse.write(bytes).err())
        } else {
            None
        };
        if let Some(e) = local_write_err {
            warn!(
                "Local output write failed: {:?}. Disabling local output for this playback.",
                e
            );
            local_pulse = None;
            local_limiter = None;
        }

        let virtual_write_err = if !virtual_buffer.is_empty() {
            let bytes: &[u8] = unsafe {
                std::slice::from_raw_parts(
                    virtual_buffer.as_ptr() as *const u8,
                    virtual_buffer.len() * 2,
                )
            };
            virtual_pulse
                .as_ref()
                .and_then(|pulse| pulse.write(bytes).err())
        } else {
            None
        };
        if let Some(e) = virtual_write_err {
            warn!(
                "Virtual output write failed: {:?}. Disabling virtual output for this playback.",
                e
            );
            virtual_pulse = None;
            virtual_limiter = None;
        }

        if local_pulse.is_none() && virtual_pulse.is_none() {
            return Err("All outputs became unavailable during playback".to_string());
        }

        fallback_samples_written =
            fallback_samples_written.saturating_add(samples_decoded_this_cycle);

        // Sample-based position against fixed output rate.
        let frames = fallback_samples_written / channels;
        let ms = frames * 1000 / rate;
        context.position_ms.store(ms, Ordering::Relaxed);

        local_buffer.clear();
        virtual_buffer.clear();
    }
    if let Some(pulse) = local_pulse.as_ref() {
        let _ = pulse.drain();
    }
    if let Some(pulse) = virtual_pulse.as_ref() {
        let _ = pulse.drain();
    }
    crate::diagnostics::memory::log_memory_snapshot("playback_loop:end");
    Ok(())
}

fn probe_duration_ms(path: &str) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()?;
    let track = probed.format.default_track()?;
    let params = &track.codec_params;
    if let (Some(tb), Some(n_frames)) = (params.time_base, params.n_frames) {
        let time = tb.calc_time(n_frames);
        let ms = time.seconds.saturating_mul(1000);
        if ms > 0 {
            return Some(ms);
        }
    }
    if let (Some(n_frames), Some(sr)) = (params.n_frames, params.sample_rate) {
        if sr > 0 {
            return Some(((n_frames as u128) * 1000 / (sr as u128)) as u64);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eof_tracker_flushes_tail_only_once_per_eof() {
        let mut tracker = EofTracker::default();

        assert!(!tracker.should_flush_tail());
        assert!(tracker.mark_source_exhausted());
        assert!(tracker.should_flush_tail());

        tracker.mark_tail_flushed();
        assert!(!tracker.should_flush_tail());

        // Repeated EOF notifications must not re-arm tail flushing.
        assert!(!tracker.mark_source_exhausted());
        assert!(!tracker.should_flush_tail());
    }

    #[test]
    fn eof_empty_buffers_finish_when_not_looping() {
        let mut tracker = EofTracker::default();
        tracker.mark_source_exhausted();
        tracker.mark_tail_flushed();

        assert_eq!(
            tracker.empty_buffer_outcome(42, false),
            EmptyBufferOutcome::Finish
        );
    }

    #[test]
    fn eof_empty_buffers_restart_when_looping() {
        let mut tracker = EofTracker::default();
        tracker.mark_source_exhausted();
        tracker.mark_tail_flushed();

        assert_eq!(
            tracker.empty_buffer_outcome(42, true),
            EmptyBufferOutcome::RestartLoop
        );
    }

    #[test]
    fn eof_empty_buffers_wait_until_tail_was_flushed() {
        let mut tracker = EofTracker::default();
        tracker.mark_source_exhausted();

        assert_eq!(
            tracker.empty_buffer_outcome(42, false),
            EmptyBufferOutcome::KeepPlaying
        );
    }
}
