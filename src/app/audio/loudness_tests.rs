use super::*;

#[test]
fn test_gain_factor_no_change() {
    let gain = compute_gain_factor(-14.0, -14.0, None);
    assert!((gain - 1.0).abs() < 0.001);
}

#[test]
fn test_gain_factor_boost() {
    let gain = compute_gain_factor(-20.0, -14.0, None);
    assert!((gain - 2.0).abs() < 0.05);
}

#[test]
fn test_gain_factor_attenuate() {
    let gain = compute_gain_factor(-8.0, -14.0, None);
    assert!((gain - 0.5).abs() < 0.05);
}

#[test]
fn test_gain_factor_capped() {
    let gain = compute_gain_factor(-60.0, -14.0, None);
    assert_eq!(gain, MAX_GAIN_FACTOR);
}

#[test]
fn test_gain_factor_infinite_lufs() {
    let gain = compute_gain_factor(f64::NEG_INFINITY, -14.0, None);
    assert_eq!(gain, 1.0);
}

#[test]
fn test_gain_factor_true_peak_clamps_high_boost() {
    // -30 LUFS, target -14 wants +16 dB boost, but file's true peak is -3 dBTP.
    // Ceiling is -1 dBTP, so max safe gain is -1 - (-3) = +2 dB.
    let gain = compute_gain_factor(-30.0, -14.0, Some(-3.0));
    let expected = 10.0_f64.powf(2.0 / 20.0) as f32;
    assert!(
        (gain - expected).abs() < 0.05,
        "expected {expected}, got {gain}"
    );
}

#[test]
fn test_gain_factor_true_peak_attenuation_does_not_relax() {
    // -8 LUFS, target -14 wants -6 dB (already attenuating). True peak 0.5 dBTP.
    // Headroom is -1 - 0.5 = -1.5 dB, but -6 dB is already lower; -6 wins.
    let gain = compute_gain_factor(-8.0, -14.0, Some(0.5));
    let expected = 10.0_f64.powf(-6.0 / 20.0) as f32;
    assert!(
        (gain - expected).abs() < 0.05,
        "expected {expected}, got {gain}"
    );
}

#[test]
fn test_gain_factor_true_peak_infinite_is_ignored() {
    // Silence file: true_peak = -inf dBTP. Treat as no constraint.
    let gain = compute_gain_factor(-20.0, -14.0, Some(f32::NEG_INFINITY));
    assert!((gain - 2.0).abs() < 0.05);
}

#[test]
fn test_combine_smart_preview_prefers_louder_when_spread_large() {
    let windows = vec![
        SmartPreviewWindowResult {
            lufs: -25.0,
            decoded_frames: 1_000,
            true_peak_dbtp: None,
        },
        SmartPreviewWindowResult {
            lufs: -20.0,
            decoded_frames: 1_000,
            true_peak_dbtp: None,
        },
        SmartPreviewWindowResult {
            lufs: -14.0,
            decoded_frames: 1_000,
            true_peak_dbtp: None,
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
            true_peak_dbtp: None,
        },
        SmartPreviewWindowResult {
            lufs: -14.5,
            decoded_frames: 1_000,
            true_peak_dbtp: None,
        },
        SmartPreviewWindowResult {
            lufs: -14.0,
            decoded_frames: 1_000,
            true_peak_dbtp: None,
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
            true_peak_dbtp: None,
        },
        SmartPreviewWindowResult {
            lufs: -10.0,
            decoded_frames: 8_000,
            true_peak_dbtp: None,
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
