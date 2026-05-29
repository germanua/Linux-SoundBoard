/// Initial `SampleBuffer` capacity in frames. Symphonia resizes the buffer on
/// the first real decode if the actual frame size differs, so this only affects
/// the pre-allocation for the first packet.
const INITIAL_DECODER_BUFFER_FRAMES: u64 = 4_096;

// ── Audio source abstraction ─────────────────────────────────────────────────

pub(super) type SeekError = Box<dyn std::error::Error + Send + Sync + 'static>;

pub(super) trait AudioSource: Iterator<Item = i16> {
    fn channels(&self) -> u16;
    fn sample_rate(&self) -> u32;
    fn total_duration(&self) -> Option<std::time::Duration>;
    fn try_seek(&mut self, pos: std::time::Duration) -> Result<(), SeekError>;
}

pub(super) trait ResettableSource: AudioSource {
    fn seek_resettable(&mut self, position: std::time::Duration) -> Result<(), SeekError>;
}

/// Converts a mono or stereo `AudioSource` to stereo at `to_rate` using linear interpolation.
pub(super) struct ChannelSampleRateConverter<S: AudioSource> {
    source: S,
    in_channels: u16,
    step: f64,
    frac: f64,
    current: [i16; 2],
    next: [i16; 2],
    out_ch: u8,
    done: bool,
}

fn read_source_frame<S: Iterator<Item = i16>>(
    source: &mut S,
    in_channels: u16,
) -> Option<[i16; 2]> {
    let l = source.next()?;
    let r = if in_channels >= 2 {
        source.next().unwrap_or(l)
    } else {
        l
    };
    Some([l, r])
}

impl<S: AudioSource> ChannelSampleRateConverter<S> {
    pub(super) fn new(mut source: S, to_rate: u32) -> Option<Self> {
        let in_channels = source.channels();
        let from_rate = source.sample_rate();
        let step = if to_rate == 0 {
            1.0
        } else {
            from_rate as f64 / to_rate as f64
        };
        let current = read_source_frame(&mut source, in_channels)?;
        let next = read_source_frame(&mut source, in_channels).unwrap_or(current);
        Some(Self {
            source,
            in_channels,
            step,
            frac: 0.0,
            current,
            next,
            out_ch: 0,
            done: false,
        })
    }
}

impl<S: AudioSource> Iterator for ChannelSampleRateConverter<S> {
    type Item = i16;

    fn next(&mut self) -> Option<i16> {
        if self.done {
            return None;
        }
        let ch = self.out_ch as usize;
        let a = self.current[ch] as f64;
        let b = self.next[ch] as f64;
        let sample = (a + (b - a) * self.frac).round() as i16;

        self.out_ch += 1;
        if self.out_ch >= 2 {
            self.out_ch = 0;
            self.frac += self.step;
            while self.frac >= 1.0 {
                self.current = self.next;
                self.frac -= 1.0;
                match read_source_frame(&mut self.source, self.in_channels) {
                    Some(frame) => self.next = frame,
                    None => {
                        self.next = self.current;
                        self.done = true;
                        break;
                    }
                }
            }
        }
        Some(sample)
    }
}

// ── Decoders ─────────────────────────────────────────────────────────────────

use log::debug;
use ogg::PacketReader;
use opus::{Channels as OpusChannels, Decoder as OpusDecoder};
use std::io::BufReader as IoBufReader;
use std::time::Duration;
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::{Time, TimeBase};

use super::EngineError;
use super::TARGET_OUTPUT_SAMPLE_RATE;

pub(super) fn clamp_seek_position_ms(position_ms: u64, duration_ms: Option<u64>) -> u64 {
    match duration_ms {
        Some(duration_ms) => position_ms.min(duration_ms),
        None => position_ms,
    }
}

pub(super) enum PlaybackSource {
    Symphonia(SymphoniaSource),
    OggOpus(OggOpusSource),
}

impl PlaybackSource {
    pub(super) fn from_path(path: &str) -> Result<Self, EngineError> {
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

impl AudioSource for PlaybackSource {
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

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        match self {
            Self::Symphonia(source) => source.try_seek(position),
            Self::OggOpus(source) => source.try_seek(position),
        }
    }
}

impl ResettableSource for PlaybackSource {
    fn seek_resettable(&mut self, position: Duration) -> Result<(), SeekError> {
        self.try_seek(position)
    }
}

pub(super) const OPUS_SAMPLE_RATE: u32 = 48_000;
const OPUS_MAX_FRAME_SAMPLES_PER_CHANNEL: usize = 5_760;

struct OggOpusHead {
    channels: u16,
    pre_skip: u16,
    stream_serial: u32,
}

pub(super) struct OggOpusSource {
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
    pub(super) fn looks_like_ogg_opus(path: &str) -> bool {
        let Ok(file) = std::fs::File::open(path) else {
            return false;
        };
        let mut reader = PacketReader::new(IoBufReader::new(file));
        matches!(
            reader.read_packet(),
            Ok(Some(packet)) if packet.data.starts_with(b"OpusHead")
        )
    }

    pub(super) fn from_path(path: &str) -> Result<Self, EngineError> {
        let file = std::fs::File::open(path)
            .map_err(|e| EngineError::Playback(format!("Failed to open Ogg Opus file: {e}")))?;
        let mut reader = PacketReader::new(IoBufReader::new(file));
        let head = read_ogg_opus_headers(&mut reader)?;
        let opus_channels = match head.channels {
            1 => OpusChannels::Mono,
            2 => OpusChannels::Stereo,
            channels => {
                return Err(EngineError::Playback(format!(
                    "Unsupported Ogg Opus channel count: {channels}. Only mono and stereo are supported."
                )))
            }
        };
        let decoder = OpusDecoder::new(OPUS_SAMPLE_RATE, opus_channels)
            .map_err(|e| EngineError::Playback(format!("Failed to create Opus decoder: {e}")))?;
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

    fn seek(&mut self, position: Duration) -> Result<(), EngineError> {
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

impl AudioSource for OggOpusSource {
    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        OPUS_SAMPLE_RATE
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.seek(position)
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as SeekError)
    }
}

fn read_ogg_opus_headers(
    reader: &mut PacketReader<IoBufReader<std::fs::File>>,
) -> Result<OggOpusHead, EngineError> {
    let head_packet = reader
        .read_packet()
        .map_err(|e| EngineError::Playback(format!("Failed to read Ogg Opus header: {e}")))?
        .ok_or_else(|| EngineError::Playback("Ogg Opus file is empty".to_string()))?;
    let mut head = parse_ogg_opus_head(&head_packet.data)?;
    head.stream_serial = head_packet.stream_serial();

    let tags_packet = reader
        .read_packet()
        .map_err(|e| EngineError::Playback(format!("Failed to read Ogg Opus tags: {e}")))?
        .ok_or_else(|| EngineError::Playback("Ogg Opus file is missing OpusTags".to_string()))?;
    if tags_packet.stream_serial() != head.stream_serial
        || !tags_packet.data.starts_with(b"OpusTags")
    {
        return Err(EngineError::Playback(
            "Ogg Opus file is missing OpusTags".to_string(),
        ));
    }

    Ok(head)
}

fn parse_ogg_opus_head(data: &[u8]) -> Result<OggOpusHead, EngineError> {
    if data.len() < 19 || !data.starts_with(b"OpusHead") {
        return Err(EngineError::Playback(
            "Ogg file is not an Opus stream".to_string(),
        ));
    }
    let version = data[8];
    if version & 0xf0 != 0 {
        return Err(EngineError::Playback(format!(
            "Unsupported Ogg Opus version: {version}"
        )));
    }
    let channels = u16::from(data[9]);
    if !(1..=2).contains(&channels) {
        return Err(EngineError::Playback(format!(
            "Unsupported Ogg Opus channel count: {channels}. Only mono and stereo are supported."
        )));
    }
    let pre_skip = u16::from_le_bytes([data[10], data[11]]);
    let input_rate = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    if input_rate != OPUS_SAMPLE_RATE {
        return Err(EngineError::Playback(format!(
            "Unsupported Ogg Opus input sample rate: {input_rate}. Only 48000 Hz is supported."
        )));
    }
    let channel_mapping_family = data[18];
    if channel_mapping_family != 0 {
        return Err(EngineError::Playback(format!(
            "Unsupported Ogg Opus channel mapping family: {channel_mapping_family}"
        )));
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

pub(super) struct SymphoniaSource {
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
    fn from_path(path: &str) -> Result<Self, EngineError> {
        let file = std::fs::File::open(path)
            .map_err(|e| EngineError::Playback(format!("Failed to open file for decode: {e}")))?;
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
            .map_err(|e| EngineError::Playback(format!("Failed to probe media: {e}")))?;
        let format = probed.format;
        let strict_audio_container = is_strict_audio_container(path);
        let track = select_audio_track(&*format, strict_audio_container)
            .ok_or_else(|| EngineError::Playback("No audio tracks found".to_string()))?;

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
            .map_err(|e| EngineError::Playback(format!("Failed to create decoder: {e}")))?;

        Ok(Self {
            decoder,
            format,
            track_id,
            time_base,
            n_frames,
            buffer: SampleBuffer::new(INITIAL_DECODER_BUFFER_FRAMES, SignalSpec { rate, channels }),
            spec: SignalSpec { rate, channels },
            current_frame_offset: 0,
            last_ts: 0,
            needs_decode: true,
        })
    }

    fn seek(&mut self, position_ms: u64) -> Result<(), EngineError> {
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
            .map_err(|e| EngineError::Playback(format!("Seek failed: {e}")))?;
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
        if self.needs_decode || self.current_frame_offset >= self.buffer.samples().len() {
            self.decode_next_packet()?;
        }
        if self.current_frame_offset < self.buffer.samples().len() {
            let sample = self.buffer.samples()[self.current_frame_offset];
            self.current_frame_offset += 1;
            return Some(sample);
        }
        None
    }
}

impl AudioSource for SymphoniaSource {
    fn channels(&self) -> u16 {
        self.spec.channels.count() as u16
    }

    fn sample_rate(&self) -> u32 {
        self.spec.rate
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration()
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.seek(position.as_millis() as u64)
            .map_err(|e| Box::new(std::io::Error::other(e.to_string())) as SeekError)
    }
}

impl ResettableSource for SymphoniaSource {
    fn seek_resettable(&mut self, position: Duration) -> Result<(), SeekError> {
        self.try_seek(position)
    }
}

pub(super) struct ResettablePlaybackSource<S, F>
where
    S: ResettableSource,
    F: Fn() -> Result<S, EngineError>,
{
    factory: F,
    converted: ChannelSampleRateConverter<S>,
    target_sample_rate: u32,
    total_duration: Option<Duration>,
}

impl<S, F> ResettablePlaybackSource<S, F>
where
    S: ResettableSource,
    F: Fn() -> Result<S, EngineError>,
{
    pub(super) fn new(factory: F, target_sample_rate: u32) -> Result<Self, EngineError> {
        let input = factory()?;
        let total_duration = input.total_duration();
        let converted =
            ChannelSampleRateConverter::new(input, target_sample_rate).ok_or_else(|| {
                EngineError::Playback("Playback source produced no samples".to_string())
            })?;
        Ok(Self {
            factory,
            converted,
            target_sample_rate,
            total_duration,
        })
    }

    pub(super) fn total_duration_ms(&self) -> Option<u64> {
        self.total_duration.map(|d| d.as_millis() as u64)
    }

    pub(super) fn seek_internal(&mut self, position: Duration) -> Result<(), SeekError> {
        let mut input = (self.factory)().map_err(|e| {
            Box::new(std::io::Error::other(format!(
                "Failed to rebuild playback source: {e}"
            ))) as SeekError
        })?;
        input.seek_resettable(position)?;
        self.total_duration = input.total_duration();
        self.converted = ChannelSampleRateConverter::new(input, self.target_sample_rate)
            .ok_or_else(|| {
                Box::new(std::io::Error::other("Rebuilt source produced no samples")) as SeekError
            })?;
        Ok(())
    }
}

impl<S, F> Iterator for ResettablePlaybackSource<S, F>
where
    S: ResettableSource,
    F: Fn() -> Result<S, EngineError>,
{
    type Item = i16;

    fn next(&mut self) -> Option<Self::Item> {
        self.converted.next()
    }
}
