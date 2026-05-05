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
/// Tight spreads are stable enough to use weighted mean.
const PREVIEW_SPREAD_TIGHT_MEAN_LU: f64 = 1.2;
/// Intro guard prevents anchoring on silence-only fades.
const PREVIEW_INTRO_GUARD_MS: u64 = 500;
/// Preferred analysis window length for each smart-preview window.
const PREVIEW_TARGET_WINDOW_MS: u64 = 2_500;
const PREVIEW_ANCHORS_MEDIUM_PCT: [u64; 4] = [8, 35, 65, 90];
const PREVIEW_ANCHORS_LONG_PCT: [u64; 5] = [5, 25, 50, 75, 92];

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmartPreviewMetrics {
    pub lufs: f64,
    pub confidence: f32,
    pub valid_window_count: usize,
    pub requested_window_count: usize,
    pub spread_lu: f64,
    pub decoded_coverage_ratio: f32,
}

#[derive(Debug, Clone, Copy)]
struct AnalysisResult {
    loudness: f64,
    decoded_frames: u64,
}

#[derive(Debug, Clone, Copy)]
struct SmartPreviewWindow {
    start_ms: u64,
    window_ms: u64,
}

#[derive(Debug, Clone, Copy)]
struct SmartPreviewWindowResult {
    lufs: f64,
    decoded_frames: u64,
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

    let time = Time::new(start_ms / 1000, (start_ms % 1000) as f64 / 1000.0);

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

fn analyze_context_with_stats(
    mut context: AnalysisDecoderContext,
    source_path: Option<&Path>,
    max_frames: Option<u64>,
) -> Result<AnalysisResult, String> {
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

    Ok(AnalysisResult {
        loudness,
        decoded_frames: total_frames,
    })
}

fn analyze_context(
    context: AnalysisDecoderContext,
    source_path: Option<&Path>,
    max_frames: Option<u64>,
) -> Result<f64, String> {
    analyze_context_with_stats(context, source_path, max_frames).map(|result| result.loudness)
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

fn weighted_mean(values: &[(f64, f64)]) -> f64 {
    let mut weighted_sum = 0.0;
    let mut total_weight = 0.0;
    for (value, weight) in values {
        let weight = weight.max(0.0);
        if !value.is_finite() || !weight.is_finite() || weight == 0.0 {
            continue;
        }
        weighted_sum += value * weight;
        total_weight += weight;
    }
    if total_weight > 0.0 {
        weighted_sum / total_weight
    } else {
        values.first().map(|(v, _)| *v).unwrap_or(0.0)
    }
}

fn weighted_median(values: &[(f64, f64)]) -> f64 {
    let mut sorted = values
        .iter()
        .copied()
        .filter(|(value, weight)| value.is_finite() && weight.is_finite() && *weight > 0.0)
        .collect::<Vec<_>>();
    if sorted.is_empty() {
        return values.first().map(|(v, _)| *v).unwrap_or(0.0);
    }

    sorted.sort_by(|left, right| {
        left.0
            .partial_cmp(&right.0)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total_weight: f64 = sorted.iter().map(|(_, weight)| *weight).sum();
    let half_weight = total_weight / 2.0;
    let mut cumulative_weight = 0.0;
    for (value, weight) in sorted {
        cumulative_weight += weight;
        if cumulative_weight >= half_weight {
            return value;
        }
    }

    values.first().map(|(v, _)| *v).unwrap_or(0.0)
}

fn combine_smart_preview_windows(values: &[SmartPreviewWindowResult]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }

    let weighted = values
        .iter()
        .filter(|window| window.lufs.is_finite())
        .map(|window| {
            let weight = (window.decoded_frames.max(1)) as f64;
            (window.lufs, weight)
        })
        .collect::<Vec<_>>();

    if weighted.is_empty() {
        return (values[0].lufs, 0.0);
    }

    let min_lufs = weighted
        .iter()
        .map(|(value, _)| *value)
        .fold(f64::INFINITY, f64::min);
    let max_lufs = weighted
        .iter()
        .map(|(value, _)| *value)
        .fold(f64::NEG_INFINITY, f64::max);
    let spread = max_lufs - min_lufs;

    let combined = if spread >= PREVIEW_SPREAD_LOUD_BIAS_LU {
        max_lufs
    } else if spread <= PREVIEW_SPREAD_TIGHT_MEAN_LU {
        weighted_mean(&weighted)
    } else {
        weighted_median(&weighted)
    };

    (combined, spread)
}

fn estimate_smart_preview_confidence(
    valid_window_count: usize,
    requested_window_count: usize,
    decoded_coverage_ratio: f32,
    spread_lu: f64,
) -> f32 {
    let requested = requested_window_count.max(1) as f32;
    let valid_ratio = (valid_window_count as f32 / requested).clamp(0.0, 1.0);

    let spread_score = if spread_lu <= PREVIEW_SPREAD_TIGHT_MEAN_LU {
        1.0
    } else if spread_lu >= PREVIEW_SPREAD_LOUD_BIAS_LU {
        0.35
    } else {
        let spread_range = (PREVIEW_SPREAD_LOUD_BIAS_LU - PREVIEW_SPREAD_TIGHT_MEAN_LU) as f32;
        let offset = (spread_lu - PREVIEW_SPREAD_TIGHT_MEAN_LU) as f32;
        (1.0 - (offset / spread_range) * 0.65).clamp(0.35, 1.0)
    };

    (valid_ratio * 0.40 + decoded_coverage_ratio.clamp(0.0, 1.0) * 0.35 + spread_score * 0.25)
        .clamp(0.0, 1.0)
}

fn build_smart_preview_windows(
    total_preview_ms: u64,
    duration_hint_ms: Option<u64>,
) -> Vec<SmartPreviewWindow> {
    let total_preview_ms = total_preview_ms.max(1);

    let Some(duration_ms) = duration_hint_ms else {
        return vec![SmartPreviewWindow {
            start_ms: 0,
            window_ms: total_preview_ms,
        }];
    };

    if duration_ms <= 12_000 {
        return vec![SmartPreviewWindow {
            start_ms: 0,
            window_ms: duration_ms.max(1),
        }];
    }

    let anchors = if duration_ms <= 90_000 {
        PREVIEW_ANCHORS_MEDIUM_PCT.as_slice()
    } else {
        PREVIEW_ANCHORS_LONG_PCT.as_slice()
    };

    let requested_windows = anchors.len().max(1) as u64;
    let mut per_window_ms = (total_preview_ms / requested_windows).max(1);
    if per_window_ms >= MIN_PREVIEW_WINDOW_MS {
        per_window_ms = per_window_ms.min(PREVIEW_TARGET_WINDOW_MS);
    }
    per_window_ms = per_window_ms.min(duration_ms.max(1));

    let max_start = duration_ms.saturating_sub(per_window_ms);
    let intro_guard = PREVIEW_INTRO_GUARD_MS.min(max_start);

    let mut windows = anchors
        .iter()
        .map(|anchor_pct| {
            let center_ms = duration_ms.saturating_mul(*anchor_pct) / 100;
            let preferred_start = center_ms.saturating_sub(per_window_ms / 2);
            let start_ms = preferred_start.clamp(intro_guard, max_start);
            SmartPreviewWindow {
                start_ms,
                window_ms: per_window_ms,
            }
        })
        .collect::<Vec<_>>();

    windows.sort_by_key(|window| window.start_ms);
    windows.dedup_by(|left, right| left.start_ms == right.start_ms);

    if windows.is_empty() {
        windows.push(SmartPreviewWindow {
            start_ms: 0,
            window_ms: per_window_ms,
        });
    }

    windows
}

pub fn analyze_loudness_path_preview_smart_with_metrics(
    path: &Path,
    total_preview_ms: u32,
    duration_hint_ms: Option<u64>,
) -> Result<SmartPreviewMetrics, String> {
    let total_preview_ms = (total_preview_ms as u64).max(1);
    let windows = build_smart_preview_windows(total_preview_ms, duration_hint_ms);

    if windows.len() == 1 && windows[0].start_ms == 0 {
        let context = build_decoder_context_for_path(path, "smart preview")?;
        let preview_frames =
            ((context.rate as u64).saturating_mul(windows[0].window_ms) / 1000).max(1);
        let result = analyze_context_with_stats(context, Some(path), Some(preview_frames))?;
        return Ok(SmartPreviewMetrics {
            lufs: result.loudness,
            confidence: 1.0,
            valid_window_count: 1,
            requested_window_count: 1,
            spread_lu: 0.0,
            decoded_coverage_ratio: (result.decoded_frames as f32 / preview_frames as f32)
                .clamp(0.0, 1.0),
        });
    }

    let mut values = Vec::with_capacity(windows.len());
    let mut first_err: Option<String> = None;
    let mut requested_total_frames: u64 = 0;
    let mut decoded_total_frames: u64 = 0;

    for window in &windows {
        let mut context = match build_decoder_context_for_path(path, "smart preview") {
            Ok(ctx) => ctx,
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
                continue;
            }
        };

        if let Err(e) = seek_context_to_ms(&mut context, path, window.start_ms) {
            warn!("{e}");
            if first_err.is_none() {
                first_err = Some(e);
            }
            continue;
        }

        let preview_frames = ((context.rate as u64).saturating_mul(window.window_ms) / 1000).max(1);
        requested_total_frames = requested_total_frames.saturating_add(preview_frames);

        match analyze_context_with_stats(context, Some(path), Some(preview_frames)) {
            Ok(result) => {
                decoded_total_frames =
                    decoded_total_frames.saturating_add(result.decoded_frames.min(preview_frames));
                values.push(SmartPreviewWindowResult {
                    lufs: result.loudness,
                    decoded_frames: result.decoded_frames,
                });
            }
            Err(e) => {
                warn!(
                    "Smart preview loudness window failed at {}ms for '{}': {}",
                    window.start_ms,
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

    let (combined, spread_lu) = combine_smart_preview_windows(&values);
    let decoded_coverage_ratio = if requested_total_frames > 0 {
        (decoded_total_frames as f32 / requested_total_frames as f32).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let confidence = estimate_smart_preview_confidence(
        values.len(),
        windows.len(),
        decoded_coverage_ratio,
        spread_lu,
    );

    debug!(
        "Smart loudness preview complete: {:.1} LUFS from {}/{} window(s), spread {:.2} LU, coverage {:.2}, confidence {:.2} [{}]",
        combined,
        values.len(),
        windows.len(),
        spread_lu,
        decoded_coverage_ratio,
        confidence,
        path.display()
    );

    Ok(SmartPreviewMetrics {
        lufs: combined,
        confidence,
        valid_window_count: values.len(),
        requested_window_count: windows.len(),
        spread_lu,
        decoded_coverage_ratio,
    })
}

/// Analyze multiple windows to reduce quiet-intro bias.
pub fn analyze_loudness_path_preview_smart(
    path: &Path,
    total_preview_ms: u32,
    duration_hint_ms: Option<u64>,
) -> Result<f64, String> {
    analyze_loudness_path_preview_smart_with_metrics(path, total_preview_ms, duration_hint_ms)
        .map(|metrics| metrics.lufs)
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
        let windows = vec![
            SmartPreviewWindowResult {
                lufs: -25.0,
                decoded_frames: 1_000,
            },
            SmartPreviewWindowResult {
                lufs: -20.0,
                decoded_frames: 1_000,
            },
            SmartPreviewWindowResult {
                lufs: -14.0,
                decoded_frames: 1_000,
            },
        ];
        let (combined, _spread) = combine_smart_preview_windows(&windows);
        assert!((combined - -14.0).abs() < 0.001);
    }

    #[test]
    fn test_combine_smart_preview_uses_mean_when_spread_small() {
        let windows = vec![
            SmartPreviewWindowResult {
                lufs: -15.0,
                decoded_frames: 1_000,
            },
            SmartPreviewWindowResult {
                lufs: -14.5,
                decoded_frames: 1_000,
            },
            SmartPreviewWindowResult {
                lufs: -14.0,
                decoded_frames: 1_000,
            },
        ];
        let (combined, _spread) = combine_smart_preview_windows(&windows);
        assert!((combined - -14.5).abs() < 0.001);
    }

    #[test]
    fn test_build_smart_preview_windows_without_duration_hint_uses_single_window() {
        let windows = build_smart_preview_windows(8_000, None);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].start_ms, 0);
        assert_eq!(windows[0].window_ms, 8_000);
    }

    #[test]
    fn test_build_smart_preview_windows_short_track_uses_full_duration() {
        let windows = build_smart_preview_windows(8_000, Some(9_000));
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].start_ms, 0);
        assert_eq!(windows[0].window_ms, 9_000);
    }

    #[test]
    fn test_build_smart_preview_windows_medium_track_spreads_windows() {
        let windows = build_smart_preview_windows(8_000, Some(60_000));
        assert_eq!(windows.len(), PREVIEW_ANCHORS_MEDIUM_PCT.len());
        assert!(windows
            .iter()
            .all(|window| window.start_ms >= PREVIEW_INTRO_GUARD_MS));
        assert!(windows
            .windows(2)
            .all(|pair| pair[0].start_ms < pair[1].start_ms));
    }

    #[test]
    fn test_build_smart_preview_windows_long_track_uses_long_anchor_profile() {
        let windows = build_smart_preview_windows(12_000, Some(180_000));
        assert_eq!(windows.len(), PREVIEW_ANCHORS_LONG_PCT.len());
        assert!(windows
            .windows(2)
            .all(|pair| pair[0].start_ms < pair[1].start_ms));
    }

    #[test]
    fn test_estimate_smart_preview_confidence_penalizes_large_spread() {
        let tight = estimate_smart_preview_confidence(4, 4, 1.0, 0.8);
        let wide = estimate_smart_preview_confidence(4, 4, 1.0, 6.0);
        assert!(tight > wide);
    }

    #[test]
    fn test_estimate_smart_preview_confidence_penalizes_missing_windows_and_coverage() {
        let complete = estimate_smart_preview_confidence(4, 4, 1.0, 1.0);
        let sparse = estimate_smart_preview_confidence(2, 4, 0.45, 1.0);
        assert!(complete > sparse);
    }

    #[test]
    fn test_combine_smart_preview_windows_uses_frame_weighting() {
        let windows = vec![
            SmartPreviewWindowResult {
                lufs: -20.0,
                decoded_frames: 1_000,
            },
            SmartPreviewWindowResult {
                lufs: -10.0,
                decoded_frames: 8_000,
            },
        ];

        let (combined, spread) = combine_smart_preview_windows(&windows);
        assert!((spread - 10.0).abs() < 0.001);
        assert!((combined - -10.0).abs() < 0.001);
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
