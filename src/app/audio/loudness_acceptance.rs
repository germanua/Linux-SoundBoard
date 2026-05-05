//! Phase-1 acceptance harness for loudness performance and quality baselines.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::commands;
use crate::config::{Config, LoudnessAnalysisState, Sound};
use crate::diagnostics;

pub const ACCEPTANCE_FIRST_PASS_TARGET_200_TRACKS_MS: u128 = 30_000;
pub const ACCEPTANCE_MEDIAN_ABS_ERROR_TARGET_LU: f64 = 0.7;
pub const ACCEPTANCE_P90_ABS_ERROR_TARGET_LU: f64 = 1.0;
pub const ACCEPTANCE_SOFT_RSS_DELTA_TARGET_KB: u64 = 50 * 1024;
pub const ACCEPTANCE_MIN_ANALYZED_RATIO: f64 = 0.95;
pub const ACCEPTANCE_MIN_COMPARABLE_RATIO: f64 = 0.90;

const RSS_SAMPLER_INTERVAL_MS: u64 = 20;

#[derive(Debug, Clone, PartialEq)]
pub struct LoudnessAcceptanceMetrics {
    pub track_count: usize,
    pub analyzed_count: u32,
    pub comparable_count: usize,
    pub first_pass_wall_clock_ms: u128,
    pub median_abs_error_lu: f64,
    pub p90_abs_error_lu: f64,
    pub rss_start_kb: Option<u64>,
    pub rss_peak_kb: Option<u64>,
    pub rss_end_kb: Option<u64>,
    pub rss_peak_delta_kb: Option<i64>,
    pub rss_end_delta_kb: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoudnessAcceptanceIssue {
    pub metric: &'static str,
    pub message: String,
}

pub fn run_loudness_acceptance_harness(
    corpus_paths: &[String],
) -> Result<LoudnessAcceptanceMetrics, String> {
    if corpus_paths.is_empty() {
        return Err("Acceptance harness requires at least one input path".to_string());
    }

    let config = build_harness_config(corpus_paths);

    let rss_start_kb = diagnostics::read_memory_snapshot().and_then(|s| s.vm_rss_kb);
    let rss_sampler = start_rss_peak_sampler(rss_start_kb);

    let first_pass_started_at = Instant::now();
    let analyzed_count = commands::analyze_all_loudness(Arc::clone(&config))?;
    let first_pass_wall_clock_ms = first_pass_started_at.elapsed().as_millis();

    let rss_peak_kb = stop_rss_peak_sampler(rss_sampler);
    let rss_end_kb = diagnostics::read_memory_snapshot().and_then(|s| s.vm_rss_kb);

    let (comparable_count, median_abs_error_lu, p90_abs_error_lu) =
        compute_fast_vs_full_error_stats(&config)?;

    Ok(LoudnessAcceptanceMetrics {
        track_count: corpus_paths.len(),
        analyzed_count,
        comparable_count,
        first_pass_wall_clock_ms,
        median_abs_error_lu,
        p90_abs_error_lu,
        rss_start_kb,
        rss_peak_kb,
        rss_end_kb,
        rss_peak_delta_kb: compute_delta_kb(rss_start_kb, rss_peak_kb),
        rss_end_delta_kb: compute_delta_kb(rss_start_kb, rss_end_kb),
    })
}

pub fn evaluate_loudness_acceptance(
    metrics: &LoudnessAcceptanceMetrics,
) -> Vec<LoudnessAcceptanceIssue> {
    let mut issues = Vec::new();

    if metrics.track_count > 0 {
        let analyzed_ratio = metrics.analyzed_count as f64 / metrics.track_count as f64;
        if analyzed_ratio < ACCEPTANCE_MIN_ANALYZED_RATIO {
            issues.push(LoudnessAcceptanceIssue {
                metric: "analyzed_coverage_ratio",
                message: format!(
                    "Analyzed coverage {:.1}% is below required {:.1}% ({}/{})",
                    analyzed_ratio * 100.0,
                    ACCEPTANCE_MIN_ANALYZED_RATIO * 100.0,
                    metrics.analyzed_count,
                    metrics.track_count
                ),
            });
        }
    }

    if metrics.analyzed_count > 0 {
        let comparable_ratio = metrics.comparable_count as f64 / metrics.analyzed_count as f64;
        if comparable_ratio < ACCEPTANCE_MIN_COMPARABLE_RATIO {
            issues.push(LoudnessAcceptanceIssue {
                metric: "comparable_coverage_ratio",
                message: format!(
                    "Comparable coverage {:.1}% is below required {:.1}% ({}/{})",
                    comparable_ratio * 100.0,
                    ACCEPTANCE_MIN_COMPARABLE_RATIO * 100.0,
                    metrics.comparable_count,
                    metrics.analyzed_count
                ),
            });
        }
    }

    if metrics.track_count >= 200
        && metrics.first_pass_wall_clock_ms > ACCEPTANCE_FIRST_PASS_TARGET_200_TRACKS_MS
    {
        issues.push(LoudnessAcceptanceIssue {
            metric: "first_pass_wall_clock_ms",
            message: format!(
                "Fast-pass wall clock {}ms exceeded target {}ms for {} tracks",
                metrics.first_pass_wall_clock_ms,
                ACCEPTANCE_FIRST_PASS_TARGET_200_TRACKS_MS,
                metrics.track_count
            ),
        });
    }

    if !metrics.median_abs_error_lu.is_finite() {
        issues.push(LoudnessAcceptanceIssue {
            metric: "median_abs_error_lu",
            message: "Median absolute LUFS error is non-finite".to_string(),
        });
    } else if metrics.median_abs_error_lu > ACCEPTANCE_MEDIAN_ABS_ERROR_TARGET_LU {
        issues.push(LoudnessAcceptanceIssue {
            metric: "median_abs_error_lu",
            message: format!(
                "Median absolute LUFS error {:.3} exceeded target {:.3}",
                metrics.median_abs_error_lu, ACCEPTANCE_MEDIAN_ABS_ERROR_TARGET_LU
            ),
        });
    }

    if !metrics.p90_abs_error_lu.is_finite() {
        issues.push(LoudnessAcceptanceIssue {
            metric: "p90_abs_error_lu",
            message: "p90 absolute LUFS error is non-finite".to_string(),
        });
    } else if metrics.p90_abs_error_lu > ACCEPTANCE_P90_ABS_ERROR_TARGET_LU {
        issues.push(LoudnessAcceptanceIssue {
            metric: "p90_abs_error_lu",
            message: format!(
                "p90 absolute LUFS error {:.3} exceeded target {:.3}",
                metrics.p90_abs_error_lu, ACCEPTANCE_P90_ABS_ERROR_TARGET_LU
            ),
        });
    }

    if metrics.median_abs_error_lu.is_finite()
        && metrics.p90_abs_error_lu.is_finite()
        && metrics.p90_abs_error_lu + f64::EPSILON < metrics.median_abs_error_lu
    {
        issues.push(LoudnessAcceptanceIssue {
            metric: "error_percentile_order",
            message: format!(
                "p90 absolute LUFS error {:.3} is below median {:.3}",
                metrics.p90_abs_error_lu, metrics.median_abs_error_lu
            ),
        });
    }

    if let Some(rss_peak_delta_kb) = metrics.rss_peak_delta_kb {
        if rss_peak_delta_kb > ACCEPTANCE_SOFT_RSS_DELTA_TARGET_KB as i64 {
            issues.push(LoudnessAcceptanceIssue {
                metric: "rss_peak_delta_kb",
                message: format!(
                    "Peak RSS delta {}kB exceeded soft target {}kB",
                    rss_peak_delta_kb, ACCEPTANCE_SOFT_RSS_DELTA_TARGET_KB
                ),
            });
        }
    }

    issues
}

fn build_harness_config(corpus_paths: &[String]) -> Arc<Mutex<Config>> {
    let mut config = Config::default();
    config.settings.auto_gain = false;

    config.sounds = corpus_paths
        .iter()
        .enumerate()
        .map(|(idx, path)| {
            let mut sound = Sound::new(format!("Acceptance Sound {}", idx + 1), path.clone());
            sound.loudness_lufs = None;
            sound.loudness_analysis_state = LoudnessAnalysisState::Pending;
            sound.loudness_confidence = None;
            sound
        })
        .collect();

    Arc::new(Mutex::new(config))
}

fn compute_fast_vs_full_error_stats(
    config: &Arc<Mutex<Config>>,
) -> Result<(usize, f64, f64), String> {
    let sounds = {
        let cfg = config
            .lock()
            .map_err(|e| format!("Config lock poisoned: {}", e))?;
        cfg.sounds.clone()
    };

    let mut abs_errors = Vec::new();
    for sound in sounds {
        let Some(fast_lufs) = sound.loudness_lufs else {
            continue;
        };
        if !fast_lufs.is_finite() {
            continue;
        }

        let path = Path::new(&sound.path);
        if !path.exists() {
            continue;
        }

        let full_lufs = match crate::audio::loudness::analyze_loudness_path(path) {
            Ok(value) if value.is_finite() => value,
            Ok(_) => continue,
            Err(_) => continue,
        };

        abs_errors.push((fast_lufs - full_lufs).abs());
    }

    if abs_errors.is_empty() {
        return Err("No comparable fast/full LUFS values were produced".to_string());
    }

    abs_errors.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let median = percentile_sorted(&abs_errors, 0.50);
    let p90 = percentile_sorted(&abs_errors, 0.90);
    Ok((abs_errors.len(), median, p90))
}

fn percentile_sorted(values: &[f64], quantile: f64) -> f64 {
    debug_assert!(!values.is_empty());
    debug_assert!((0.0..=1.0).contains(&quantile));

    if values.len() == 1 {
        return values[0];
    }

    let position = quantile * (values.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        values[lower]
    } else {
        let weight = position - lower as f64;
        values[lower] + (values[upper] - values[lower]) * weight
    }
}

fn compute_delta_kb(before: Option<u64>, after: Option<u64>) -> Option<i64> {
    Some(after? as i64 - before? as i64)
}

struct RssPeakSampler {
    stop: Arc<AtomicBool>,
    peak_kb: Arc<AtomicU64>,
    handle: thread::JoinHandle<()>,
}

fn start_rss_peak_sampler(initial_kb: Option<u64>) -> RssPeakSampler {
    let stop = Arc::new(AtomicBool::new(false));
    let peak_kb = Arc::new(AtomicU64::new(initial_kb.unwrap_or(0)));

    let stop_flag = Arc::clone(&stop);
    let peak_value = Arc::clone(&peak_kb);
    let handle = thread::spawn(move || {
        while !stop_flag.load(Ordering::Acquire) {
            if let Some(current_rss) = diagnostics::read_memory_snapshot().and_then(|s| s.vm_rss_kb)
            {
                let mut previous_peak = peak_value.load(Ordering::Acquire);
                while current_rss > previous_peak {
                    match peak_value.compare_exchange(
                        previous_peak,
                        current_rss,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(observed) => previous_peak = observed,
                    }
                }
            }
            thread::sleep(Duration::from_millis(RSS_SAMPLER_INTERVAL_MS));
        }
    });

    RssPeakSampler {
        stop,
        peak_kb,
        handle,
    }
}

fn stop_rss_peak_sampler(sampler: RssPeakSampler) -> Option<u64> {
    sampler.stop.store(true, Ordering::Release);
    let _ = sampler.handle.join();
    let peak = sampler.peak_kb.load(Ordering::Acquire);
    (peak > 0).then_some(peak)
}

#[cfg(test)]
mod tests {
    use super::{
        evaluate_loudness_acceptance, percentile_sorted, run_loudness_acceptance_harness,
        LoudnessAcceptanceMetrics,
    };
    use crate::test_support::audio_fixtures::{
        cleanup_test_audio_path, create_test_audio_file_with_duration,
    };
    use std::collections::HashSet;

    #[test]
    fn percentile_sorted_interpolates_expected_values() {
        let values = vec![0.1, 0.2, 0.4, 0.9, 1.0];
        assert!((percentile_sorted(&values, 0.50) - 0.4).abs() < f64::EPSILON);
        assert!((percentile_sorted(&values, 0.90) - 0.96).abs() < 1e-9);
    }

    #[test]
    fn loudness_acceptance_harness_reports_metrics_for_small_corpus() {
        let durations_ms = [300_u32, 600, 1_200, 2_500, 4_000, 7_500, 9_000, 12_000];
        let paths = durations_ms
            .iter()
            .map(|duration_ms| create_test_audio_file_with_duration("wav", *duration_ms))
            .collect::<Vec<_>>();

        let corpus = paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        let metrics = run_loudness_acceptance_harness(&corpus)
            .expect("acceptance harness should produce metrics");

        assert_eq!(metrics.track_count, paths.len());
        assert!(metrics.analyzed_count > 0);
        assert!(metrics.comparable_count > 0);
        assert!(metrics.median_abs_error_lu.is_finite());
        assert!(metrics.p90_abs_error_lu.is_finite());
        assert!(metrics.first_pass_wall_clock_ms > 0);

        for path in &paths {
            cleanup_test_audio_path(path);
        }
    }

    #[test]
    fn evaluate_loudness_acceptance_reports_threshold_violations() {
        let metrics = LoudnessAcceptanceMetrics {
            track_count: 200,
            analyzed_count: 200,
            comparable_count: 200,
            first_pass_wall_clock_ms: 31_000,
            median_abs_error_lu: 0.9,
            p90_abs_error_lu: 1.4,
            rss_start_kb: Some(100_000),
            rss_peak_kb: Some(170_000),
            rss_end_kb: Some(160_000),
            rss_peak_delta_kb: Some(70_000),
            rss_end_delta_kb: Some(60_000),
        };

        let issues = evaluate_loudness_acceptance(&metrics);
        assert_eq!(issues.len(), 4);
    }

    #[test]
    fn evaluate_loudness_acceptance_reports_coverage_and_non_finite_issues() {
        let metrics = LoudnessAcceptanceMetrics {
            track_count: 100,
            analyzed_count: 80,
            comparable_count: 60,
            first_pass_wall_clock_ms: 15_000,
            median_abs_error_lu: f64::NAN,
            p90_abs_error_lu: f64::INFINITY,
            rss_start_kb: None,
            rss_peak_kb: None,
            rss_end_kb: None,
            rss_peak_delta_kb: None,
            rss_end_delta_kb: None,
        };

        let issues = evaluate_loudness_acceptance(&metrics);
        let issue_metrics = issues
            .iter()
            .map(|issue| issue.metric)
            .collect::<HashSet<_>>();

        assert!(issue_metrics.contains("analyzed_coverage_ratio"));
        assert!(issue_metrics.contains("comparable_coverage_ratio"));
        assert!(issue_metrics.contains("median_abs_error_lu"));
        assert!(issue_metrics.contains("p90_abs_error_lu"));
    }

    #[test]
    fn evaluate_loudness_acceptance_reports_percentile_ordering_issues() {
        let metrics = LoudnessAcceptanceMetrics {
            track_count: 20,
            analyzed_count: 20,
            comparable_count: 20,
            first_pass_wall_clock_ms: 10_000,
            median_abs_error_lu: 0.5,
            p90_abs_error_lu: 0.4,
            rss_start_kb: Some(100_000),
            rss_peak_kb: Some(120_000),
            rss_end_kb: Some(110_000),
            rss_peak_delta_kb: Some(20_000),
            rss_end_delta_kb: Some(10_000),
        };

        let issues = evaluate_loudness_acceptance(&metrics);
        assert!(issues
            .iter()
            .any(|issue| issue.metric == "error_percentile_order"));
    }

    #[test]
    #[ignore = "Manual benchmark gate for target profile (200-track acceptance run)"]
    fn manual_loudness_acceptance_gate_for_target_profile() {
        let paths = (0..200_u32)
            .map(|idx| {
                let duration_ms = match idx % 3 {
                    0 => 1_000,
                    1 => 3_000,
                    _ => 7_000,
                };
                create_test_audio_file_with_duration("wav", duration_ms)
            })
            .collect::<Vec<_>>();
        let corpus = paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        let metrics = run_loudness_acceptance_harness(&corpus)
            .expect("acceptance harness should produce metrics");
        let issues = evaluate_loudness_acceptance(&metrics);

        for path in &paths {
            cleanup_test_audio_path(path);
        }

        assert!(
            issues.is_empty(),
            "Acceptance gate failed with metrics {:?}: {:?}",
            metrics,
            issues
        );
    }
}
