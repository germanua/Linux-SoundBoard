use super::*;
use crate::test_support::audio_fixtures::{
    cleanup_test_audio_path, create_test_ogg_opus_file, create_test_vorbis_file,
    TestOggOpusFixture, TestVorbisFixture,
};

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
    let gain = compute_gain_factor(-30.0, -14.0, Some(-3.0));
    let expected = 10.0_f64.powf(2.0 / 20.0) as f32;
    assert!(
        (gain - expected).abs() < 0.05,
        "expected {expected}, got {gain}"
    );
}

#[test]
fn test_gain_factor_true_peak_attenuation_does_not_relax() {
    let gain = compute_gain_factor(-8.0, -14.0, Some(0.5));
    let expected = 10.0_f64.powf(-6.0 / 20.0) as f32;
    assert!(
        (gain - expected).abs() < 0.05,
        "expected {expected}, got {gain}"
    );
}

#[test]
fn test_gain_factor_true_peak_infinite_is_ignored() {
    // Silence has no true-peak limit.
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
fn test_missing_file_remains_an_io_error() {
    let path = std::env::temp_dir().join(format!("lsb-missing-audio-{}.ogg", uuid::Uuid::new_v4()));

    assert!(matches!(
        analyze_loudness_path_full(&path, never_cancelled()),
        Err(LoudnessError::Io(_))
    ));
}

#[test]
fn test_loudness_analysis_accepts_libvorbis_after_empty_priming_packet() {
    let audio_path = create_test_vorbis_file(TestVorbisFixture::Mono44100);

    let (loudness, true_peak) = analyze_loudness_path_full(&audio_path, never_cancelled())
        .expect("analyze libvorbis loudness");

    assert!(loudness.is_finite());
    assert!(true_peak.is_some_and(f32::is_finite));

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_loudness_analysis_accepts_ogg_opus() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        extension: "opus",
        pre_skip: 312,
        packet_count: 60,
        ..Default::default()
    });

    let (loudness, true_peak) = analyze_loudness_path_full(&audio_path, never_cancelled())
        .expect("analyze Ogg Opus loudness");

    assert!(loudness.is_finite());
    assert!(true_peak.is_some_and(f32::is_finite));
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_ogg_opus_header_gain_shifts_loudness_and_true_peak() {
    let unity_path = create_test_ogg_opus_file(TestOggOpusFixture {
        packet_count: 60,
        ..Default::default()
    });
    let boosted_path = create_test_ogg_opus_file(TestOggOpusFixture {
        output_gain_q8: 6 * 256,
        packet_count: 60,
        ..Default::default()
    });
    let attenuated_path = create_test_ogg_opus_file(TestOggOpusFixture {
        output_gain_q8: -6 * 256,
        packet_count: 60,
        ..Default::default()
    });

    let (unity_lufs, unity_peak) = analyze_loudness_path_full(&unity_path, never_cancelled())
        .expect("analyze unity-gain Ogg Opus");
    let (boosted_lufs, boosted_peak) = analyze_loudness_path_full(&boosted_path, never_cancelled())
        .expect("analyze boosted Ogg Opus");
    let (attenuated_lufs, attenuated_peak) =
        analyze_loudness_path_full(&attenuated_path, never_cancelled())
            .expect("analyze attenuated Ogg Opus");

    assert!((boosted_lufs - unity_lufs - 6.0).abs() < 0.15);
    assert!((attenuated_lufs - unity_lufs + 6.0).abs() < 0.15);
    assert!(
        (boosted_peak.expect("boosted true peak") - unity_peak.expect("unity true peak") - 6.0)
            .abs()
            < 0.15
    );
    assert!(
        (attenuated_peak.expect("attenuated true peak") - unity_peak.expect("unity true peak")
            + 6.0)
            .abs()
            < 0.15
    );
    cleanup_test_audio_path(&unity_path);
    cleanup_test_audio_path(&boosted_path);
    cleanup_test_audio_path(&attenuated_path);
}

#[test]
fn test_smart_preview_accepts_short_and_long_ogg_opus() {
    for (packet_count, duration_ms) in [(60, 1_200), (700, 14_000)] {
        let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
            packet_count,
            ..Default::default()
        });

        let metrics = analyze_loudness_path_preview_smart_with_metrics(
            &audio_path,
            4_000,
            Some(duration_ms),
            never_cancelled(),
        )
        .expect("analyze Ogg Opus smart preview");

        assert!(metrics.lufs.is_finite());
        assert!(metrics.true_peak_dbtp.is_some_and(f32::is_finite));
        assert!(metrics.valid_window_count > 0);
        assert!(metrics.decoded_coverage_ratio > 0.0);
        cleanup_test_audio_path(&audio_path);
    }
}
