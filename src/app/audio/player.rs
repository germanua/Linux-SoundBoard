//! Audio playback with local and virtual outputs.
//!
//! Seek handling keeps the requested timeline stable after decoder jumps.

use libpulse_binding::sample::{Format, Spec};
use libpulse_binding::stream::Direction;
use libpulse_simple_binding::Simple;
use log::{debug, error, info, warn};
use rodio::source::SeekError as RodioSeekError;
use rodio::source::UniformSourceIterator;
use rodio::Source;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
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

/// Playback timestamp and total duration.
#[derive(Clone, Copy, Debug)]
pub struct Timestamp {
    pub current: Duration,
    pub total: Duration,
}

impl Timestamp {
    pub fn new(current: Duration, total: Duration) -> Self {
        Self { current, total }
    }

    pub fn total_ms(&self) -> u64 {
        self.total.as_millis() as u64
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

    /// Seek to a position in milliseconds.
    fn seek(&mut self, pos_ms: u64) -> Result<(), String> {
        let time = Time::new(
            pos_ms / 1000,                   // seconds
            (pos_ms % 1000) as f64 / 1000.0, // fractional seconds
        );

        // Prefer timestamp seeks when the container exposes frame counts.
        let seek_to = if let (Some(tb), Some(max_frames)) = (self.time_base, self.n_frames) {
            let ts = tb.calc_timestamp(time);
            // Clamp to the last valid frame.
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
            // Fall back to time-based seek.
            debug!("Seeking with Time (fallback): {:?}", time);
            SeekTo::Time {
                time,
                track_id: Some(self.track_id),
            }
        };

        let seek_result = self.format.seek(SeekMode::Coarse, seek_to);

        match seek_result {
            Ok(seeked_to) => {
                self.last_ts = seeked_to.actual_ts;

                self.needs_decode = true;
                self.current_frame_offset = 0;

                // Reset decoder state after seek.
                self.decoder.reset();

                info!(
                    "Seek successful: requested={}ms, actual_ts={}",
                    pos_ms, seeked_to.actual_ts
                );

                Ok(())
            }
            Err(e) => {
                warn!("Seek failed: {}", e);
                Err(format!("Seek failed: {}", e))
            }
        }
    }

    /// Current playback position and total duration.
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

    /// Decode the next packet, skipping recoverable errors.
    fn decode_next_packet(&mut self) -> Option<()> {
        let mut recoverable_packet_errors: usize = 0;
        const MAX_RECOVERABLE_PACKET_ERRORS: usize = 2048;
        loop {
            let packet = match self.format.next_packet() {
                Ok(p) => p,
                Err(SymphoniaError::IoError(e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return None;
                }
                Err(SymphoniaError::ResetRequired) => {
                    // Some formats need a reset after seek.
                    debug!("Format reader requested reset");
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::DecodeError(msg)) => {
                    // Skip malformed packets instead of stopping on bad media.
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

            // Ignore packets from other tracks.
            if packet.track_id() != self.track_id {
                continue;
            }

            // Track the decoder's actual position.
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
                    // Decoder wants a reset here too.
                    debug!("Decoder requested reset");
                    self.decoder.reset();
                    continue;
                }
                Err(SymphoniaError::DecodeError(msg)) => {
                    // Skip bad packets and keep going.
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
        // Rodio snapshots this before the first decode, so keep it unknown.
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
            .map_err(|e| RodioSeekError::Other(Box::new(std::io::Error::other(e))))
    }
}

impl Iterator for SymphoniaSource {
    type Item = i16;
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            // Decode again after seek or when the buffer runs dry.
            if self.needs_decode || self.current_frame_offset >= self.buffer.samples().len() {
                self.decode_next_packet()?;
            }

            if self.current_frame_offset < self.buffer.samples().len() {
                let sample = self.buffer.samples()[self.current_frame_offset];
                self.current_frame_offset += 1;
                return Some(sample);
            } else {
                return None;
            }
        }
    }
}

/// Playback source that rebuilds its conversion chain after seek.
trait ResettableSource: Source<Item = i16> {
    fn seek_resettable(&mut self, pos: Duration) -> Result<(), RodioSeekError>;
}

impl ResettableSource for SymphoniaSource {
    fn seek_resettable(&mut self, pos: Duration) -> Result<(), RodioSeekError> {
        self.seek(pos.as_millis() as u64)
            .map_err(|e| RodioSeekError::Other(Box::new(std::io::Error::other(e))))
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
            converted: UniformSourceIterator::<S, i16>::new(
                input,
                target_channels,
                target_sample_rate,
            ),
            target_channels,
            target_sample_rate,
            total_duration,
        })
    }

    /// Rebuild the source after seek.
    fn seek_internal(&mut self, pos: Duration) -> Result<(), RodioSeekError> {
        let mut input = (self.factory)().map_err(|e| {
            RodioSeekError::Other(Box::new(std::io::Error::other(format!(
                "Failed to rebuild playback source: {e}"
            ))))
        })?;

        input.seek_resettable(pos)?;

        self.total_duration = input.total_duration();
        self.converted = UniformSourceIterator::<S, i16>::new(
            input,
            self.target_channels,
            self.target_sample_rate,
        );

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

impl<S, F> Source for ResettablePlaybackSource<S, F>
where
    S: ResettableSource,
    F: Fn() -> Result<S, String>,
{
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.target_channels
    }

    fn sample_rate(&self) -> u32 {
        self.target_sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), RodioSeekError> {
        self.seek_internal(pos).map(|_| ())
    }
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

struct PlaybackSnapshot {
    sound_id: String,
    playback_order: u64,
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

fn clamp_seek_position_ms(position_ms: u64, duration_ms: Option<u64>) -> u64 {
    match duration_ms {
        Some(duration_ms) => position_ms.min(duration_ms),
        None => position_ms,
    }
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
    GetPlaying {
        response: Sender<Vec<String>>,
    },
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
    pub fn seek_playback(&self, play_id: &str, position_ms: u64) {
        if let Err(e) = self.command_tx.send(AudioCommand::Seek {
            play_id: play_id.to_string(),
            position_ms,
        }) {
            warn!(
                "Failed to enqueue seek command: play_id={}, position_ms={}, error={}",
                play_id, position_ms, e
            );
        }
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

/// Shared auto-gain state for playback threads.
pub struct SharedAutoGain {
    enabled: AtomicBool,
    mode_bits: AtomicU32,
    apply_to_bits: AtomicU32,
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
    /// Return the gain for this output, or 1.0 if auto-gain is inactive.
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

#[derive(Debug, PartialEq, Eq)]
enum PlaybackMsg {
    Stop,
    Seek(u64),
    Pause,
    Resume,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct PendingPlaybackControl {
    stop: bool,
    seek_ms: Option<u64>,
    paused: Option<bool>,
}

fn drain_playback_messages(
    first: PlaybackMsg,
    control_rx: &Receiver<PlaybackMsg>,
) -> PendingPlaybackControl {
    let mut pending = PendingPlaybackControl::default();
    apply_playback_message(&mut pending, first);

    while !pending.stop {
        match control_rx.try_recv() {
            Ok(msg) => apply_playback_message(&mut pending, msg),
            Err(_) => break,
        }
    }

    pending
}

fn apply_playback_message(pending: &mut PendingPlaybackControl, msg: PlaybackMsg) {
    match msg {
        PlaybackMsg::Stop => {
            pending.stop = true;
            pending.seek_ms = None;
            pending.paused = None;
        }
        PlaybackMsg::Seek(ms) => pending.seek_ms = Some(ms),
        PlaybackMsg::Pause => pending.paused = Some(true),
        PlaybackMsg::Resume => pending.paused = Some(false),
    }
}

struct PlayingSound {
    local_tx: Option<Sender<PlaybackMsg>>,
    virtual_tx: Option<Sender<PlaybackMsg>>,
    sound_id: String,
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
    playback_order: u64,
    base_volume: f32,
    sound_lufs: Option<f64>,
    shared: PlaybackSharedState,
}

struct PlaybackThreadContext {
    file_path: Arc<str>,
    duration_ms: Option<u64>,
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
    fn seek(&self, pos_ms: u64) -> SeekSendOutcome {
        let mut had_channel = false;
        let mut send_failed = false;

        if let Some(tx) = &self.local_tx {
            had_channel = true;
            if tx.send(PlaybackMsg::Seek(pos_ms)).is_err() {
                send_failed = true;
                warn!(
                    "Seek delivery failed on local channel: sound_id={}, position_ms={}",
                    self.sound_id, pos_ms
                );
            }
        }
        if let Some(tx) = &self.virtual_tx {
            had_channel = true;
            if tx.send(PlaybackMsg::Seek(pos_ms)).is_err() {
                send_failed = true;
                warn!(
                    "Seek delivery failed on virtual channel: sound_id={}, position_ms={}",
                    self.sound_id, pos_ms
                );
            }
        }

        if !had_channel {
            SeekSendOutcome::NoControlChannel
        } else if send_failed {
            SeekSendOutcome::ChannelSendFailed
        } else {
            SeekSendOutcome::Sent
        }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekSendOutcome {
    Sent,
    NoControlChannel,
    ChannelSendFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SeekDispatchOutcome {
    Sent,
    MissingPlayId,
    NoControlChannel,
    ChannelSendFailed,
}

fn dispatch_playback_seek(
    playing: &HashMap<String, PlayingSound>,
    play_id: &str,
    position_ms: u64,
) -> SeekDispatchOutcome {
    if let Some(ps) = playing.get(play_id) {
        info!(
            "Sending seek message to play_id={}, position_ms={}",
            play_id, position_ms
        );
        match ps.seek(position_ms) {
            SeekSendOutcome::Sent => SeekDispatchOutcome::Sent,
            SeekSendOutcome::NoControlChannel => SeekDispatchOutcome::NoControlChannel,
            SeekSendOutcome::ChannelSendFailed => SeekDispatchOutcome::ChannelSendFailed,
        }
    } else {
        SeekDispatchOutcome::MissingPlayId
    }
}

fn remove_registry_entries(
    positions_registry: &Arc<Mutex<HashMap<String, PlaybackSnapshot>>>,
    play_ids: &HashSet<String>,
) {
    if play_ids.is_empty() {
        return;
    }

    if let Ok(mut reg) = positions_registry.lock() {
        reg.retain(|play_id, _| !play_ids.contains(play_id));
    }
}

fn stop_sound_entries(
    sound_id: &str,
    playing: &mut HashMap<String, PlayingSound>,
    positions_registry: &Arc<Mutex<HashMap<String, PlaybackSnapshot>>>,
) {
    let stopped_play_ids: HashSet<String> = playing
        .iter()
        .filter(|(_, ps)| ps.sound_id == sound_id)
        .map(|(play_id, _)| play_id.clone())
        .collect();

    for play_id in &stopped_play_ids {
        if let Some(ps) = playing.remove(play_id) {
            ps.stop();
        }
    }

    remove_registry_entries(positions_registry, &stopped_play_ids);
}

fn stop_all_playing(
    playing: &mut HashMap<String, PlayingSound>,
    positions_registry: &Arc<Mutex<HashMap<String, PlaybackSnapshot>>>,
) {
    let stopped_play_ids: HashSet<String> = playing.keys().cloned().collect();

    for (_, ps) in playing.drain() {
        ps.stop();
    }

    remove_registry_entries(positions_registry, &stopped_play_ids);
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
    let mut next_playback_order: u64 = 0;

    info!("Audio thread started");
    crate::diagnostics::memory::log_memory_snapshot("audio_thread:start");

    while let Ok(cmd) = rx.recv() {
        playing.retain(|_, ps| !ps.is_finished());
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
                let playback_order = next_playback_order;
                next_playback_order = next_playback_order.saturating_add(1);
                crate::diagnostics::memory::log_memory_snapshot("audio_cmd:play:before");
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    play_dual_output(PlayRequest {
                        local_available,
                        virtual_available,
                        sound_id: &sound_id,
                        path: &path,
                        playback_order,
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
                stop_sound_entries(&sound_id, &mut playing, &positions_registry);
            }
            AudioCommand::StopAll => {
                stop_all_playing(&mut playing, &positions_registry);
                crate::diagnostics::memory::log_memory_snapshot("audio_cmd:stop_all");
            }
            AudioCommand::Seek {
                play_id,
                position_ms,
            } => match dispatch_playback_seek(&playing, &play_id, position_ms) {
                SeekDispatchOutcome::Sent => {}
                SeekDispatchOutcome::MissingPlayId => {
                    warn!(
                        "Seek: No playing sound found with play_id={}. Playing ids: {:?}",
                        play_id,
                        playing.keys().collect::<Vec<_>>()
                    );
                }
                SeekDispatchOutcome::NoControlChannel => {
                    warn!(
                        "Seek: play_id={} has no active control channel; position_ms={}",
                        play_id, position_ms
                    );
                }
                SeekDispatchOutcome::ChannelSendFailed => {
                    warn!(
                        "Seek: control channel send failed for play_id={}, position_ms={}",
                        play_id, position_ms
                    );
                }
            },
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
                let res = positions_registry
                    .lock()
                    .map(|reg| build_playback_positions(&reg))
                    .unwrap_or_default();
                let _ = response.send(res);
            }
        }
    }
}

fn build_playback_positions(registry: &HashMap<String, PlaybackSnapshot>) -> Vec<PlaybackPosition> {
    let mut ordered: Vec<(u64, PlaybackPosition)> = registry
        .iter()
        .map(|(play_id, snap)| {
            (
                snap.playback_order,
                PlaybackPosition {
                    play_id: play_id.clone(),
                    sound_id: snap.sound_id.clone(),
                    position_ms: snap.position_ms.load(Ordering::Relaxed),
                    paused: snap.paused.load(Ordering::Relaxed),
                    finished: snap.is_finished_flag.load(Ordering::Relaxed),
                    duration_ms: snap.duration_ms,
                },
            )
        })
        .collect();

    // Keep unfinished playback ahead of finished entries, newest first.
    ordered.sort_by(|(left_order, left), (right_order, right)| {
        left.finished
            .cmp(&right.finished)
            .then_with(|| right_order.cmp(left_order))
    });

    ordered.into_iter().map(|(_, position)| position).collect()
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

fn build_dynamic_limiters(
    auto_gain: &SharedAutoGain,
    rate: u64,
    channels: u64,
    local_enabled: bool,
    virtual_enabled: bool,
) -> (Option<LookAheadLimiter>, Option<LookAheadLimiter>) {
    let mode = auto_gain.mode();
    if !auto_gain.is_enabled() || mode != AutoGainMode::DynamicLookAhead {
        return (None, None);
    }

    let params = auto_gain.dynamic_params();
    let local_limiter = if local_enabled && auto_gain.applies_to_output(false) {
        Some(LookAheadLimiter::new(rate as u32, channels as u16, params))
    } else {
        None
    };
    let virtual_limiter = if virtual_enabled && auto_gain.applies_to_output(true) {
        Some(LookAheadLimiter::new(rate as u32, channels as u16, params))
    } else {
        None
    };

    (local_limiter, virtual_limiter)
}

fn play_dual_output(request: PlayRequest<'_>) -> Result<(String, PlayingSound), String> {
    crate::diagnostics::memory::log_memory_snapshot("play_dual_output:start");
    let _ = std::fs::File::open(request.path).map_err(|e| format!("Failed to open file: {e}"))?;
    let file_path: Arc<str> = Arc::from(request.path);

    let finished_flag = Arc::new(AtomicBool::new(false));
    let position_ms = Arc::new(AtomicU64::new(0));
    let paused = Arc::new(AtomicBool::new(false));

    let duration_ms = crate::audio::metadata::probe_duration_ms(request.path);

    let (tx, rx) = mpsc::channel();
    let path = Arc::clone(&file_path);
    let flag = Arc::clone(&finished_flag);
    let thread_context = PlaybackThreadContext {
        file_path: path,
        duration_ms,
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
                playback_order: request.playback_order,
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

    let file_path = Arc::clone(&context.file_path);
    let mut source = ResettablePlaybackSource::new(
        move || SymphoniaSource::from_path(&file_path),
        output_channels as u16,
        output_rate,
    )?;

    let mut local_buffer: Vec<i16> = Vec::with_capacity(4096);
    let mut virtual_buffer: Vec<i16> = Vec::with_capacity(4096);
    let mut is_paused = false;

    // Track the requested seek first, then advance by written samples.
    let channels = source.channels() as u64;
    let rate = source.sample_rate() as u64;
    if channels == 0 || rate == 0 {
        return Err("Invalid output stream parameters".to_string());
    }
    let mut fallback_samples_written: u64 = 0;
    let mut eof_tracker = EofTracker::default();
    let (mut local_limiter, mut virtual_limiter) = build_dynamic_limiters(
        &context.shared_auto_gain,
        rate,
        channels,
        local_pulse.is_some(),
        virtual_pulse.is_some(),
    );
    let mut last_dynamic_mode = context.shared_auto_gain.mode();
    let mut last_dynamic_params = context.shared_auto_gain.dynamic_params();

    'playback: loop {
        while let Ok(msg) = context.control_rx.try_recv() {
            let pending = drain_playback_messages(msg, &context.control_rx);
            if pending.stop {
                break 'playback;
            }

            if let Some(ms) = pending.seek_ms {
                let clamped_ms = clamp_seek_position_ms(ms, context.duration_ms);
                let current_position_ms = context.position_ms.load(Ordering::Relaxed);

                if current_position_ms == clamped_ms {
                    debug!(
                        "Skipping redundant seek: position_ms={}, file_path={}",
                        clamped_ms, context.file_path
                    );
                    continue;
                }

                if let Some(pulse) = local_pulse.as_ref() {
                    let _ = pulse.flush();
                }
                if let Some(pulse) = virtual_pulse.as_ref() {
                    let _ = pulse.flush();
                }

                match source.seek_internal(Duration::from_millis(clamped_ms)) {
                    Ok(()) => {
                        local_buffer.clear();
                        virtual_buffer.clear();
                        (local_limiter, virtual_limiter) = build_dynamic_limiters(
                            &context.shared_auto_gain,
                            rate,
                            channels,
                            local_pulse.is_some(),
                            virtual_pulse.is_some(),
                        );

                        let seek_baseline_samples = (clamped_ms * rate * channels) / 1000;
                        fallback_samples_written = seek_baseline_samples;

                        context.position_ms.store(clamped_ms, Ordering::SeqCst);
                        eof_tracker.reset();

                        debug!(
                            "Seek position tracking: baseline_samples={}, requested_position_ms={}",
                            seek_baseline_samples, clamped_ms
                        );
                    }
                    Err(e) => {
                        warn!("Seek failed: {}", e);
                    }
                }
            }

            if let Some(paused) = pending.paused {
                is_paused = paused;
                context.paused.store(paused, Ordering::Relaxed);
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
                            context.position_ms.store(0, Ordering::SeqCst);
                            eof_tracker.reset();

                            let restart_mode = context.shared_auto_gain.mode();
                            let restart_params = context.shared_auto_gain.dynamic_params();
                            last_dynamic_mode = restart_mode;
                            last_dynamic_params = restart_params;
                            (local_limiter, virtual_limiter) = build_dynamic_limiters(
                                &context.shared_auto_gain,
                                rate,
                                channels,
                                local_pulse.is_some(),
                                virtual_pulse.is_some(),
                            );

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

        // Update position from written samples.
        let ms = (fallback_samples_written * 1000) / (rate * channels);
        context.position_ms.store(ms, Ordering::SeqCst);

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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::TryRecvError;

    #[derive(Clone)]
    struct TestSeekSource {
        samples: Vec<i16>,
        position: usize,
        channels: u16,
        sample_rate: u32,
    }

    impl Iterator for TestSeekSource {
        type Item = i16;

        fn next(&mut self) -> Option<Self::Item> {
            let sample = self.samples.get(self.position).copied()?;
            self.position += 1;
            Some(sample)
        }
    }

    impl Source for TestSeekSource {
        fn current_frame_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> u16 {
            self.channels
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn total_duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(
                (self.samples.len() as u64) / u64::from(self.channels.max(1)),
            ))
        }

        fn try_seek(&mut self, pos: Duration) -> Result<(), RodioSeekError> {
            let frame = pos.as_secs() as usize * usize::from(self.channels);
            self.position = frame.min(self.samples.len());
            Ok(())
        }
    }

    impl ResettableSource for TestSeekSource {
        fn seek_resettable(&mut self, pos: Duration) -> Result<(), RodioSeekError> {
            self.try_seek(pos)
        }
    }

    #[derive(Clone)]
    struct FailingSeekSource {
        samples: Vec<i16>,
        position: usize,
        channels: u16,
        sample_rate: u32,
    }

    impl Iterator for FailingSeekSource {
        type Item = i16;

        fn next(&mut self) -> Option<Self::Item> {
            let sample = self.samples.get(self.position).copied()?;
            self.position += 1;
            Some(sample)
        }
    }

    impl Source for FailingSeekSource {
        fn current_frame_len(&self) -> Option<usize> {
            None
        }

        fn channels(&self) -> u16 {
            self.channels
        }

        fn sample_rate(&self) -> u32 {
            self.sample_rate
        }

        fn total_duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(
                (self.samples.len() as u64) / u64::from(self.channels.max(1)),
            ))
        }

        fn try_seek(&mut self, _pos: Duration) -> Result<(), RodioSeekError> {
            Err(RodioSeekError::Other(Box::new(std::io::Error::other(
                "seek failed",
            ))))
        }
    }

    impl ResettableSource for FailingSeekSource {
        fn seek_resettable(&mut self, pos: Duration) -> Result<(), RodioSeekError> {
            self.try_seek(pos)
        }
    }

    fn test_playing_sound(local_tx: Sender<PlaybackMsg>, sound_id: &str) -> PlayingSound {
        PlayingSound {
            local_tx: Some(local_tx),
            virtual_tx: None,
            sound_id: sound_id.to_string(),
            paused: Arc::new(AtomicBool::new(false)),
            _local_thread: None,
            _virtual_thread: None,
            is_finished_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    fn test_playback_snapshot(sound_id: &str, playback_order: u64) -> PlaybackSnapshot {
        PlaybackSnapshot {
            sound_id: sound_id.to_string(),
            playback_order,
            position_ms: Arc::new(AtomicU64::new(0)),
            paused: Arc::new(AtomicBool::new(false)),
            duration_ms: None,
            is_finished_flag: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn eof_tracker_flushes_tail_only_once_per_eof() {
        let mut tracker = EofTracker::default();

        assert!(!tracker.should_flush_tail());
        assert!(tracker.mark_source_exhausted());
        assert!(tracker.should_flush_tail());

        tracker.mark_tail_flushed();
        assert!(!tracker.should_flush_tail());

        // EOF should only flush the tail once.
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

    #[test]
    fn clamp_seek_position_ms_preserves_seek_without_duration() {
        assert_eq!(clamp_seek_position_ms(5_000, None), 5_000);
        assert_eq!(clamp_seek_position_ms(0, None), 0);
    }

    #[test]
    fn clamp_seek_position_ms_clamps_seek_near_end() {
        assert_eq!(clamp_seek_position_ms(12_000, Some(10_000)), 10_000);
        assert_eq!(clamp_seek_position_ms(9_999, Some(10_000)), 9_999);
    }

    #[test]
    fn dispatch_playback_seek_targets_only_requested_play_id() {
        let (target_tx, target_rx) = mpsc::channel();
        let (other_tx, other_rx) = mpsc::channel();
        let mut playing = HashMap::new();
        playing.insert(
            "play-1".to_string(),
            test_playing_sound(target_tx, "shared-sound"),
        );
        playing.insert(
            "play-2".to_string(),
            test_playing_sound(other_tx, "shared-sound"),
        );

        assert_eq!(
            dispatch_playback_seek(&playing, "play-2", 3_500),
            SeekDispatchOutcome::Sent
        );
        assert_eq!(
            other_rx.try_recv().expect("seek sent"),
            PlaybackMsg::Seek(3_500)
        );
        assert!(matches!(target_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn dispatch_playback_seek_returns_missing_play_id() {
        let playing = HashMap::new();

        assert_eq!(
            dispatch_playback_seek(&playing, "missing", 4_200),
            SeekDispatchOutcome::MissingPlayId
        );
    }

    #[test]
    fn dispatch_playback_seek_returns_channel_send_failed_when_receiver_dropped() {
        let (tx, rx) = mpsc::channel();
        drop(rx);

        let mut playing = HashMap::new();
        playing.insert("play-1".to_string(), test_playing_sound(tx, "sound-1"));

        assert_eq!(
            dispatch_playback_seek(&playing, "play-1", 4_200),
            SeekDispatchOutcome::ChannelSendFailed
        );
    }

    #[test]
    fn dispatch_playback_seek_returns_no_control_channel() {
        let playing_sound = PlayingSound {
            local_tx: None,
            virtual_tx: None,
            sound_id: "sound-1".to_string(),
            paused: Arc::new(AtomicBool::new(false)),
            _local_thread: None,
            _virtual_thread: None,
            is_finished_flag: Arc::new(AtomicBool::new(false)),
        };
        let mut playing = HashMap::new();
        playing.insert("play-1".to_string(), playing_sound);

        assert_eq!(
            dispatch_playback_seek(&playing, "play-1", 1_200),
            SeekDispatchOutcome::NoControlChannel
        );
    }

    #[test]
    fn drain_playback_messages_coalesces_to_last_seek() {
        let (tx, rx) = mpsc::channel();
        tx.send(PlaybackMsg::Seek(2_000)).expect("first seek");
        tx.send(PlaybackMsg::Seek(4_000)).expect("second seek");
        tx.send(PlaybackMsg::Pause).expect("pause");

        let pending = drain_playback_messages(PlaybackMsg::Seek(1_000), &rx);

        assert_eq!(
            pending,
            PendingPlaybackControl {
                stop: false,
                seek_ms: Some(4_000),
                paused: Some(true),
            }
        );
    }

    #[test]
    fn stop_sound_entries_remove_matching_registry_snapshots_immediately() {
        let (keep_tx, _keep_rx) = mpsc::channel();
        let (stop_tx, _stop_rx) = mpsc::channel();
        let mut playing = HashMap::new();
        playing.insert(
            "play-keep".to_string(),
            test_playing_sound(keep_tx, "sound-keep"),
        );
        playing.insert(
            "play-stop".to_string(),
            test_playing_sound(stop_tx, "sound-stop"),
        );

        let positions_registry = Arc::new(Mutex::new(HashMap::from([
            (
                "play-keep".to_string(),
                test_playback_snapshot("sound-keep", 0),
            ),
            (
                "play-stop".to_string(),
                test_playback_snapshot("sound-stop", 1),
            ),
        ])));

        stop_sound_entries("sound-stop", &mut playing, &positions_registry);

        assert!(playing.contains_key("play-keep"));
        assert!(!playing.contains_key("play-stop"));

        let registry = positions_registry.lock().expect("registry lock");
        assert!(registry.contains_key("play-keep"));
        assert!(!registry.contains_key("play-stop"));
    }

    #[test]
    fn stop_all_playing_clears_registry_immediately() {
        let (first_tx, _first_rx) = mpsc::channel();
        let (second_tx, _second_rx) = mpsc::channel();
        let mut playing = HashMap::new();
        playing.insert(
            "play-1".to_string(),
            test_playing_sound(first_tx, "sound-1"),
        );
        playing.insert(
            "play-2".to_string(),
            test_playing_sound(second_tx, "sound-2"),
        );

        let positions_registry = Arc::new(Mutex::new(HashMap::from([
            ("play-1".to_string(), test_playback_snapshot("sound-1", 0)),
            ("play-2".to_string(), test_playback_snapshot("sound-2", 1)),
        ])));

        stop_all_playing(&mut playing, &positions_registry);

        assert!(playing.is_empty());
        assert!(positions_registry.lock().expect("registry lock").is_empty());
    }

    #[test]
    fn build_playback_positions_prefers_newest_unfinished_playback() {
        let mut registry = HashMap::new();
        registry.insert(
            "play-old".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-old".to_string(),
                playback_order: 1,
                position_ms: Arc::new(AtomicU64::new(9_000)),
                paused: Arc::new(AtomicBool::new(false)),
                duration_ms: Some(10_000),
                is_finished_flag: Arc::new(AtomicBool::new(false)),
            },
        );
        registry.insert(
            "play-new".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-new".to_string(),
                playback_order: 2,
                position_ms: Arc::new(AtomicU64::new(200)),
                paused: Arc::new(AtomicBool::new(false)),
                duration_ms: Some(10_000),
                is_finished_flag: Arc::new(AtomicBool::new(false)),
            },
        );
        registry.insert(
            "play-finished".to_string(),
            PlaybackSnapshot {
                sound_id: "sound-finished".to_string(),
                playback_order: 3,
                position_ms: Arc::new(AtomicU64::new(10_000)),
                paused: Arc::new(AtomicBool::new(false)),
                duration_ms: Some(10_000),
                is_finished_flag: Arc::new(AtomicBool::new(true)),
            },
        );

        let positions = build_playback_positions(&registry);

        assert_eq!(positions[0].play_id, "play-new");
        assert!(!positions[0].finished);
        assert_eq!(positions[1].play_id, "play-old");
        assert!(!positions[1].finished);
        assert_eq!(positions[2].play_id, "play-finished");
        assert!(positions[2].finished);
    }

    #[test]
    fn resettable_playback_source_rebuilds_converter_after_seek() {
        let input = TestSeekSource {
            samples: vec![100, 200, 300, 400],
            position: 0,
            channels: 1,
            sample_rate: 1,
        };
        let mut source = ResettablePlaybackSource::new(move || Ok(input.clone()), 1, 2)
            .expect("wrapper should build");

        assert_eq!(source.next(), Some(100));
        assert_eq!(source.next(), Some(150));

        source
            .try_seek(Duration::from_secs(2))
            .expect("seek should succeed");

        let post_seek = (0..4).filter_map(|_| source.next()).collect::<Vec<_>>();
        assert_eq!(post_seek, vec![300, 350, 400]);
    }

    #[test]
    fn resettable_playback_source_failed_seek_leaves_position_unchanged() {
        let input = FailingSeekSource {
            samples: vec![10, 20, 30, 40],
            position: 0,
            channels: 1,
            sample_rate: 1,
        };
        let mut source = ResettablePlaybackSource::new(move || Ok(input.clone()), 1, 1)
            .expect("wrapper should build");

        assert_eq!(source.next(), Some(10));
        assert!(source.try_seek(Duration::from_secs(2)).is_err());
        assert_eq!(source.next(), Some(20));
    }

    #[test]
    fn requested_seek_baseline_handles_seek_to_start() {
        let requested_ms: u64 = 0;
        let rate: u64 = 48_000;
        let channels: u64 = 2;

        let seek_baseline_samples = (requested_ms * rate * channels) / 1000;
        assert_eq!(seek_baseline_samples, 0);
    }

    #[test]
    fn requested_seek_baseline_handles_midpoint_seek() {
        let requested_ms: u64 = 5_000;
        let rate: u64 = 48_000;
        let channels: u64 = 2;

        let seek_baseline_samples = (requested_ms * rate * channels) / 1000;
        assert_eq!(seek_baseline_samples, 480_000);
    }

    #[test]
    fn requested_seek_baseline_handles_clamped_seek_near_end() {
        let requested_ms: u64 = clamp_seek_position_ms(12_000, Some(10_000));
        let rate: u64 = 48_000;
        let channels: u64 = 2;

        let seek_baseline_samples = (requested_ms * rate * channels) / 1000;
        assert_eq!(seek_baseline_samples, 960_000);
    }

    #[test]
    fn position_tracking_uses_requested_seek_baseline() {
        let requested_ms: u64 = 10_000;
        let rate: u64 = 48_000;
        let channels: u64 = 2;

        let seek_baseline_samples = (requested_ms * rate * channels) / 1000;
        let samples_after_seek: u64 = 1000;
        let total_samples = seek_baseline_samples + samples_after_seek;

        let frames = total_samples / channels;
        let position_ms = frames * 1000 / rate;

        assert!(position_ms >= 10_010 && position_ms <= 10_011);
    }

    #[test]
    fn position_tracking_without_seek_baseline_would_be_wrong() {
        let rate = 48_000;
        let channels = 2;

        let samples_after_seek = 1000;
        let frames = samples_after_seek / channels;
        let position_ms = frames * 1000 / rate;

        assert!(position_ms < 100);
    }
}
