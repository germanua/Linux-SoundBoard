//! EBU R128 loudness analysis for auto-gain.

use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use ebur128::{EbuR128, Mode};
use log::{debug, warn};
use symphonia::core::audio::{Channels, SampleBuffer, SignalSpec};
use symphonia::core::codecs::{Decoder, DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

/// Cap boost so very quiet files do not explode in volume.
const MAX_GAIN_FACTOR: f32 = 4.0;

/// Keep a floor so gain never goes to zero.
const MIN_GAIN_FACTOR: f32 = 0.01;

/// Smallest window we use for smart previews.
const MIN_PREVIEW_WINDOW_MS: u64 = 500;
/// Bias toward the louder window when previews disagree a lot.
const PREVIEW_SPREAD_LOUD_BIAS_LU: f64 = 5.0;

static ANALYSIS_CANCELLED: AtomicBool = AtomicBool::new(false);

pub fn cancel_loudness_analysis() {
    ANALYSIS_CANCELLED.store(true, Ordering::SeqCst);
}

pub fn is_loudness_analysis_cancelled() -> bool {
    ANALYSIS_CANCELLED.load(Ordering::SeqCst)
}

pub fn reset_loudness_analysis_cancelled() {
    ANALYSIS_CANCELLED.store(false, Ordering::SeqCst);
}

struct AnalysisDecoderContext {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn Decoder>,
    track_id: u32,
    spec: SignalSpec,
    rate: u32,
    channels: u32,
}

fn default_channels() -> Channels {
    Channels::FRONT_LEFT | Channels::FRONT_RIGHT
}

fn loudness_format_options() -> FormatOptions {
    FormatOptions {
        enable_gapless: true,
        ..Default::default()
    }
}

fn build_decoder_context(
    hint: &Hint,
    media_source: MediaSourceStream,
) -> Result<AnalysisDecoderContext, String> {
    let probed = symphonia::default::get_probe()
        .format(
            hint,
            media_source,
            &loudness_format_options(),
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("Failed to probe audio for loudness analysis: {e}"))?;

    let format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("No audio tracks found for loudness analysis")?;

    let track_id = track.id;
    let codec_params = &track.codec_params;
    let rate = codec_params.sample_rate.unwrap_or(44100);
    let channels = codec_params.channels.map(|c| c.count()).unwrap_or(2) as u32;

    let decoder = symphonia::default::get_codecs()
        .make(codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to create decoder for loudness analysis: {e}"))?;

    let spec = SignalSpec {
        rate,
        channels: codec_params.channels.unwrap_or(default_channels()),
    };

    Ok(AnalysisDecoderContext {
        format,
        decoder,
        track_id,
        spec,
        rate,
        channels,
    })
}

fn build_decoder_context_for_path(
    path: &Path,
    purpose: &'static str,
) -> Result<AnalysisDecoderContext, String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Failed to open audio file for loudness {purpose}: {e}"))?;

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    build_decoder_context(&hint, mss)
}

fn seek_context_to_ms(
    context: &mut AnalysisDecoderContext,
    source_path: &Path,
    start_ms: u64,
) -> Result<(), String> {
    if start_ms == 0 {
        return Ok(());
    }

    let time = Time::new(
        start_ms / 1000,
        (start_ms % 1000) as f64 / 1000.0, // fractional seconds
    );

    context
        .format
        .seek(
            SeekMode::Coarse,
            SeekTo::Time {
                time,
                track_id: Some(context.track_id),
            },
        )
        .map_err(|e| {
            format!(
                "Failed to seek audio for loudness preview at {}ms [{}]: {}",
                start_ms,
                source_path.display(),
                e
            )
        })?;

    context.decoder.reset();
    Ok(())
}

fn analyze_context(
    mut context: AnalysisDecoderContext,
    source_path: Option<&Path>,
    max_frames: Option<u64>,
) -> Result<f64, String> {
    let mut ebur128 = EbuR128::new(context.channels, context.rate, Mode::I)
        .map_err(|e| format!("Failed to create EBU R128 analyzer: {e:?}"))?;

    let source_suffix = source_path
        .map(|path| format!(" ({})", path.display()))
        .unwrap_or_default();

    let mut total_frames: u64 = 0;
    loop {
        if let Some(limit) = max_frames {
            if total_frames >= limit {
                break;
            }
        }

        if total_frames % 10000 == 0 && is_loudness_analysis_cancelled() {
            return Err("Analysis cancelled".to_string());
        }

        let packet = match context.format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => {
                warn!(
                    "Error reading packet during loudness analysis{}: {}",
                    source_suffix, e
                );
                break;
            }
        };

        if packet.track_id() != context.track_id {
            continue;
        }

        let decoded = match context.decoder.decode(&packet) {
            Ok(decoded) => decoded,
            Err(symphonia::core::errors::Error::DecodeError(msg)) => {
                warn!(
                    "Decode error during loudness analysis{}: {}",
                    source_suffix, msg
                );
                continue;
            }
            Err(_) => break,
        };

        let num_frames = decoded.frames();
        if num_frames == 0 {
            continue;
        }

        let mut sample_buf = SampleBuffer::<i16>::new(num_frames as u64, context.spec);
        sample_buf.copy_interleaved_ref(decoded);

        ebur128.add_frames_i16(sample_buf.samples()).map_err(|e| {
            format!("Failed to add frames to EBU R128 analyzer{source_suffix}: {e:?}")
        })?;

        total_frames += num_frames as u64;
    }

    if total_frames == 0 {
        return Err("No audio frames decoded for loudness analysis".to_string());
    }

    let loudness = ebur128
        .loudness_global()
        .map_err(|e| format!("Failed to compute global loudness: {e:?}"))?;

    if let Some(path) = source_path {
        if let Some(limit) = max_frames {
            debug!(
                "Loudness preview complete: {:.1} LUFS ({} frames, limit {}, {} channels, {} Hz) [{}]",
                loudness,
                total_frames,
                limit,
                context.channels,
                context.rate,
                path.display()
            );
        } else {
            debug!(
                "Loudness analysis complete: {:.1} LUFS ({} frames, {} channels, {} Hz) [{}]",
                loudness,
                total_frames,
                context.channels,
                context.rate,
                path.display()
            );
        }
    } else if let Some(limit) = max_frames {
        debug!(
            "Loudness preview complete: {:.1} LUFS ({} frames, limit {}, {} channels, {} Hz)",
            loudness, total_frames, limit, context.channels, context.rate
        );
    } else {
        debug!(
            "Loudness analysis complete: {:.1} LUFS ({} frames, {} channels, {} Hz)",
            loudness, total_frames, context.channels, context.rate
        );
    }

    Ok(loudness)
}

pub fn analyze_loudness(file_data: &[u8]) -> Result<f64, String> {
    let cursor = Cursor::new(file_data.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let hint = Hint::new();
    let context = build_decoder_context(&hint, mss)?;
    analyze_context(context, None, None)
}

/// Analyze loudness from a file on disk.
pub fn analyze_loudness_path(path: &Path) -> Result<f64, String> {
    let context = build_decoder_context_for_path(path, "analysis")?;
    analyze_context(context, Some(path), None)
}

/// Analyze just the start of a file for faster import-time estimates.
pub fn analyze_loudness_path_preview(path: &Path, preview_ms: u32) -> Result<f64, String> {
    let context = build_decoder_context_for_path(path, "preview")?;
    let preview_frames = ((context.rate as u64).saturating_mul(preview_ms as u64) / 1000).max(1);
    analyze_context(context, Some(path), Some(preview_frames))
}

fn combine_smart_preview_lufs(values: &[f64]) -> f64 {
    let finite_values: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if finite_values.is_empty() {
        return values[0];
    }
    if finite_values.len() == 1 {
        return finite_values[0];
    }

    let mut min_lufs = f64::INFINITY;
    let mut max_lufs = f64::NEG_INFINITY;
    let mut sum = 0.0;
    for lufs in &finite_values {
        min_lufs = min_lufs.min(*lufs);
        max_lufs = max_lufs.max(*lufs);
        sum += *lufs;
    }
    let mean = sum / finite_values.len() as f64;

    // Quiet intros can skew preview windows; prefer the louder read when spread is wide.
    if max_lufs - min_lufs >= PREVIEW_SPREAD_LOUD_BIAS_LU {
        max_lufs
    } else {
        mean
    }
}

/// Analyze multiple windows to reduce quiet-intro bias.
pub fn analyze_loudness_path_preview_smart(
    path: &Path,
    total_preview_ms: u32,
    duration_hint_ms: Option<u64>,
) -> Result<f64, String> {
    let total_preview_ms = (total_preview_ms as u64).max(1);
    let mut start_offsets = vec![0_u64];

    if let Some(duration_ms) = duration_hint_ms {
        if duration_ms >= 8_000 {
            start_offsets.push(duration_ms.saturating_mul(55) / 100);
        }
        if duration_ms >= 15_000 {
            start_offsets.push(duration_ms.saturating_mul(80) / 100);
        }
    }

    start_offsets.sort_unstable();
    start_offsets.dedup();

    if start_offsets.len() == 1 && start_offsets[0] == 0 {
        return analyze_loudness_path_preview(path, total_preview_ms as u32);
    }

    let mut per_window_ms = total_preview_ms / start_offsets.len() as u64;
    per_window_ms = per_window_ms
        .max(MIN_PREVIEW_WINDOW_MS)
        .min(total_preview_ms);

    if let Some(duration_ms) = duration_hint_ms {
        let max_start = duration_ms.saturating_sub(per_window_ms);
        for start in &mut start_offsets {
            *start = (*start).min(max_start);
        }
        start_offsets.sort_unstable();
        start_offsets.dedup();
    }

    let mut values = Vec::with_capacity(start_offsets.len());
    let mut first_err: Option<String> = None;

    for start_ms in start_offsets {
        let mut context = match build_decoder_context_for_path(path, "smart preview") {
            Ok(ctx) => ctx,
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
                continue;
            }
        };

        if let Err(e) = seek_context_to_ms(&mut context, path, start_ms) {
            warn!("{e}");
            if first_err.is_none() {
                first_err = Some(e);
            }
            continue;
        }

        let preview_frames = ((context.rate as u64).saturating_mul(per_window_ms) / 1000).max(1);

        match analyze_context(context, Some(path), Some(preview_frames)) {
            Ok(lufs) => values.push(lufs),
            Err(e) => {
                warn!(
                    "Smart preview loudness window failed at {}ms for '{}': {}",
                    start_ms,
                    path.display(),
                    e
                );
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }

    if values.is_empty() {
        return Err(first_err.unwrap_or_else(|| {
            format!(
                "Failed to compute smart loudness preview for '{}'",
                path.display()
            )
        }));
    }

    let combined = combine_smart_preview_lufs(&values);
    debug!(
        "Smart loudness preview complete: {:.1} LUFS from {} window(s) [{}]",
        combined,
        values.len(),
        path.display()
    );
    Ok(combined)
}

/// Convert measured loudness into a linear gain factor.
pub fn compute_gain_factor(sound_lufs: f64, target_lufs: f64) -> f32 {
    if !sound_lufs.is_finite() {
        return 1.0;
    }

    let gain_db = target_lufs - sound_lufs;
    let gain_linear = 10.0_f64.powf(gain_db / 20.0) as f32;

    gain_linear.clamp(MIN_GAIN_FACTOR, MAX_GAIN_FACTOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gain_factor_no_change() {
        let gain = compute_gain_factor(-14.0, -14.0);
        assert!((gain - 1.0).abs() < 0.001);
    }

    #[test]
    fn test_gain_factor_boost() {
        let gain = compute_gain_factor(-20.0, -14.0);
        assert!((gain - 2.0).abs() < 0.05);
    }

    #[test]
    fn test_gain_factor_attenuate() {
        let gain = compute_gain_factor(-8.0, -14.0);
        assert!((gain - 0.5).abs() < 0.05);
    }

    #[test]
    fn test_gain_factor_capped() {
        let gain = compute_gain_factor(-50.0, -14.0);
        assert_eq!(gain, MAX_GAIN_FACTOR);
    }

    #[test]
    fn test_gain_factor_infinite_lufs() {
        let gain = compute_gain_factor(f64::NEG_INFINITY, -14.0);
        assert_eq!(gain, 1.0);
    }

    #[test]
    fn test_combine_smart_preview_prefers_louder_when_spread_large() {
        let combined = combine_smart_preview_lufs(&[-25.0, -20.0, -14.0]);
        assert!((combined - -14.0).abs() < 0.001);
    }

    #[test]
    fn test_combine_smart_preview_uses_mean_when_spread_small() {
        let combined = combine_smart_preview_lufs(&[-15.0, -14.5, -14.0]);
        assert!((combined - -14.5).abs() < 0.001);
    }

    #[test]
    fn test_cancel_loudness_analysis() {
        reset_loudness_analysis_cancelled();
        assert!(!is_loudness_analysis_cancelled());

        cancel_loudness_analysis();
        assert!(is_loudness_analysis_cancelled());

        reset_loudness_analysis_cancelled();
        assert!(!is_loudness_analysis_cancelled());
    }
}
