//! EBU R128 loudness analysis for auto-gain.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ebur128::{EbuR128, Mode};
use log::{debug, warn};

use super::player::{DecodedAudioSource, DecodedPlaybackSource};

/// Cap boost so very quiet files do not explode in volume.
const MAX_GAIN_FACTOR: f32 = 8.0;

/// True-peak ceiling (dBTP) the output must stay under after gain.
const TRUE_PEAK_CEILING_DBTP: f32 = -1.0;

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

static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);

/// Token for analyses with no run to cancel: single-sound playback gain and the
/// acceptance harness. It is never set, so those decodes always run to the end.
pub fn never_cancelled() -> &'static AtomicBool {
    &NEVER_CANCELLED
}

#[derive(Debug, thiserror::Error)]
pub enum LoudnessError {
    /// Audio file could not be opened.
    #[error("{0}")]
    Io(String),
    /// Audio probe, decode, or analysis computation failed.
    #[error("{0}")]
    Decode(String),
    /// No valid loudness result could be computed.
    #[error("{0}")]
    NoResult(String),
    /// Analysis was interrupted by a cancel request.
    #[error("Analysis cancelled")]
    Cancelled,
}

struct AnalysisDecoderContext {
    source: DecodedPlaybackSource,
    rate: u32,
    channels: u32,
    output_gain_factor: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SmartPreviewMetrics {
    pub lufs: f64,
    pub confidence: f32,
    pub valid_window_count: usize,
    pub requested_window_count: usize,
    pub spread_lu: f64,
    pub decoded_coverage_ratio: f32,
    pub true_peak_dbtp: Option<f32>,
}

#[derive(Debug, Clone, Copy)]
struct AnalysisResult {
    loudness: f64,
    decoded_frames: u64,
    true_peak_dbtp: Option<f32>,
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
    true_peak_dbtp: Option<f32>,
}

fn build_decoder_context_for_path(
    path: &Path,
    purpose: &'static str,
) -> Result<AnalysisDecoderContext, LoudnessError> {
    std::fs::File::open(path).map_err(|error| {
        LoudnessError::Io(format!(
            "Failed to open audio file for loudness {purpose}: {error}"
        ))
    })?;
    let source = DecodedPlaybackSource::from_path(&path.to_string_lossy()).map_err(|error| {
        LoudnessError::Decode(format!(
            "Failed to open audio for loudness {purpose}: {error}"
        ))
    })?;
    let rate = source.sample_rate();
    let channels = u32::from(source.channels());
    let output_gain_factor = source.output_gain_factor();
    Ok(AnalysisDecoderContext {
        source,
        rate,
        channels,
        output_gain_factor,
    })
}

fn seek_context_to_ms(
    context: &mut AnalysisDecoderContext,
    source_path: &Path,
    start_ms: u64,
) -> Result<(), LoudnessError> {
    if start_ms == 0 {
        return Ok(());
    }

    context
        .source
        .try_seek(Duration::from_millis(start_ms))
        .map_err(|e| {
            LoudnessError::Decode(format!(
                "Failed to seek audio for loudness preview at {}ms [{}]: {}",
                start_ms,
                source_path.display(),
                e
            ))
        })
}

fn analyze_context_with_stats(
    mut context: AnalysisDecoderContext,
    source_path: Option<&Path>,
    max_frames: Option<u64>,
    cancel: &AtomicBool,
) -> Result<AnalysisResult, LoudnessError> {
    let mut ebur128 = EbuR128::new(context.channels, context.rate, Mode::I | Mode::TRUE_PEAK)
        .map_err(|e| LoudnessError::Decode(format!("Failed to create EBU R128 analyzer: {e:?}")))?;

    let source_suffix = source_path
        .map(|path| format!(" ({})", path.display()))
        .unwrap_or_default();

    let channels = context.channels as usize;
    let mut samples = Vec::with_capacity(4_096 * channels);
    let mut total_frames: u64 = 0;
    loop {
        if let Some(limit) = max_frames {
            if total_frames >= limit {
                break;
            }
        }

        if cancel.load(Ordering::SeqCst) {
            return Err(LoudnessError::Cancelled);
        }

        let wanted_frames = max_frames
            .map(|limit| limit.saturating_sub(total_frames).min(4_096))
            .unwrap_or(4_096) as usize;
        let wanted_samples = wanted_frames * channels;
        samples.clear();
        samples.extend(
            context
                .source
                .by_ref()
                .take(wanted_samples)
                .map(|sample| sample as f32 / 32768.0 * context.output_gain_factor),
        );
        samples.truncate(samples.len() / channels * channels);
        if samples.is_empty() {
            break;
        }

        ebur128.add_frames_f32(&samples).map_err(|e| {
            LoudnessError::Decode(format!(
                "Failed to add frames to EBU R128 analyzer{source_suffix}: {e:?}"
            ))
        })?;

        let decoded_frames = (samples.len() / channels) as u64;
        total_frames += decoded_frames;
        if decoded_frames < wanted_frames as u64 {
            break;
        }
    }

    if total_frames == 0 {
        return Err(LoudnessError::NoResult(
            "No audio frames decoded for loudness analysis".to_string(),
        ));
    }

    let loudness = ebur128
        .loudness_global()
        .map_err(|e| LoudnessError::Decode(format!("Failed to compute global loudness: {e:?}")))?;

    let true_peak_dbtp = extract_true_peak_dbtp(&ebur128, context.channels);

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
        true_peak_dbtp,
    })
}

/// Maximum true-peak across all channels, in dBTP. Returns None if ebur128
/// rejects every channel query (e.g. true-peak mode not actually enabled).
fn extract_true_peak_dbtp(ebur128: &EbuR128, channels: u32) -> Option<f32> {
    let mut max_peak: f64 = 0.0;
    let mut any_ok = false;
    for ch in 0..channels {
        if let Ok(peak) = ebur128.true_peak(ch) {
            any_ok = true;
            if peak > max_peak {
                max_peak = peak;
            }
        }
    }
    if !any_ok {
        return None;
    }
    if max_peak <= 0.0 {
        return Some(f32::NEG_INFINITY);
    }
    Some((20.0 * max_peak.log10()) as f32)
}

#[cfg(test)]
fn analyze_context(
    context: AnalysisDecoderContext,
    source_path: Option<&Path>,
    max_frames: Option<u64>,
) -> Result<f64, LoudnessError> {
    analyze_context_with_stats(context, source_path, max_frames, never_cancelled())
        .map(|result| result.loudness)
}

#[cfg(test)]
pub fn analyze_loudness_path(path: &Path) -> Result<f64, LoudnessError> {
    let context = build_decoder_context_for_path(path, "analysis")?;
    analyze_context(context, Some(path), None)
}

pub fn analyze_loudness_path_full(
    path: &Path,
    cancel: &AtomicBool,
) -> Result<(f64, Option<f32>), LoudnessError> {
    let context = build_decoder_context_for_path(path, "analysis")?;
    let result = analyze_context_with_stats(context, Some(path), None, cancel)?;
    Ok((result.loudness, result.true_peak_dbtp))
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
    cancel: &AtomicBool,
) -> Result<SmartPreviewMetrics, LoudnessError> {
    let total_preview_ms = (total_preview_ms as u64).max(1);
    let windows = build_smart_preview_windows(total_preview_ms, duration_hint_ms);

    if windows.len() == 1 && windows[0].start_ms == 0 {
        let context = build_decoder_context_for_path(path, "smart preview")?;
        let preview_frames =
            ((context.rate as u64).saturating_mul(windows[0].window_ms) / 1000).max(1);
        let result = analyze_context_with_stats(context, Some(path), Some(preview_frames), cancel)?;
        return Ok(SmartPreviewMetrics {
            lufs: result.loudness,
            confidence: 1.0,
            valid_window_count: 1,
            requested_window_count: 1,
            spread_lu: 0.0,
            decoded_coverage_ratio: (result.decoded_frames as f32 / preview_frames as f32)
                .clamp(0.0, 1.0),
            true_peak_dbtp: result.true_peak_dbtp,
        });
    }

    let mut values = Vec::with_capacity(windows.len());
    let mut first_err: Option<LoudnessError> = None;
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

        match analyze_context_with_stats(context, Some(path), Some(preview_frames), cancel) {
            Ok(result) => {
                decoded_total_frames =
                    decoded_total_frames.saturating_add(result.decoded_frames.min(preview_frames));
                values.push(SmartPreviewWindowResult {
                    lufs: result.loudness,
                    decoded_frames: result.decoded_frames,
                    true_peak_dbtp: result.true_peak_dbtp,
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
            LoudnessError::NoResult(format!(
                "Failed to compute smart loudness preview for '{}'",
                path.display()
            ))
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

    let true_peak_dbtp = values
        .iter()
        .filter_map(|window| window.true_peak_dbtp)
        .filter(|tp| tp.is_finite())
        .fold(None, |max: Option<f32>, tp| {
            Some(match max {
                Some(prev) => prev.max(tp),
                None => tp,
            })
        });

    Ok(SmartPreviewMetrics {
        lufs: combined,
        confidence,
        valid_window_count: values.len(),
        requested_window_count: windows.len(),
        spread_lu,
        decoded_coverage_ratio,
        true_peak_dbtp,
    })
}

pub fn compute_gain_factor(sound_lufs: f64, target_lufs: f64, true_peak_dbtp: Option<f32>) -> f32 {
    if !sound_lufs.is_finite() {
        return 1.0;
    }

    let mut gain_db = target_lufs - sound_lufs;
    if let Some(tp) = true_peak_dbtp {
        if tp.is_finite() {
            let headroom_db = (TRUE_PEAK_CEILING_DBTP - tp) as f64;
            if gain_db > headroom_db {
                gain_db = headroom_db;
            }
        }
    }
    let gain_linear = 10.0_f64.powf(gain_db / 20.0) as f32;

    gain_linear.clamp(MIN_GAIN_FACTOR, MAX_GAIN_FACTOR)
}

#[cfg(test)]
#[path = "loudness_tests.rs"]
mod tests;
