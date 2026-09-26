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
        None,
        &runtime,
    )
    .expect("create active playback");

    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    state.adopt_voice(playback);

    fill_output_queues(&mut state);

    let queues = state.queues.lock();
    let target_samples = LOCAL_OUTPUT_QUEUE_TARGET_FRAMES * TARGET_OUTPUT_CHANNELS as usize;
    assert_eq!(queues.local.len(), target_samples);
    assert_eq!(queues.virtual_out.len(), target_samples);
    drop(queues);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn current_public_formats_decode_analyze_and_report_duration() {
    for (extension, fixture) in [
        ("mp3", TestEncodedFixture::Mp3Mono44100),
        ("ogg", TestEncodedFixture::VorbisMono44100),
        ("flac", TestEncodedFixture::FlacMono44100),
        ("aac", TestEncodedFixture::AacAdtsMono44100),
        ("m4a", TestEncodedFixture::AacMp4Mono44100),
        ("mp4", TestEncodedFixture::AacMp4Mono44100),
    ] {
        assert!(is_audio_file(&format!("/tmp/tone.{extension}")));
        assert!(is_audio_file(&format!(
            "/tmp/tone.{}",
            extension.to_ascii_uppercase()
        )));

        let audio_path = create_test_encoded_file(fixture, extension);
        let path = audio_path.to_string_lossy();
        let mut source = PlaybackSource::from_path(&path)
            .unwrap_or_else(|error| panic!("open real {extension} fixture: {error}"));
        assert!(matches!(&source, PlaybackSource::Symphonia(_)));
        assert!(source
            .total_duration()
            .is_some_and(|duration| { (800..=1_300).contains(&(duration.as_millis() as u64)) }));

        let samples = source.by_ref().take(2_048).collect::<Vec<_>>();
        assert_eq!(samples.len(), 2_048, "decode real {extension} fixture");
        assert!(
            samples.iter().any(|sample| *sample != 0),
            "real {extension} fixture must not decode as silence"
        );

        assert!(
            probe_duration_ms(&path).is_some_and(|duration| { (800..=1_300).contains(&duration) })
        );
        let (loudness, true_peak) =
            analyze_loudness_path_full(&audio_path, crate::audio::loudness::never_cancelled())
                .unwrap_or_else(|error| panic!("analyze real {extension} fixture: {error}"));
        assert!(
            (-24.0..=-16.0).contains(&loudness),
            "real {extension} LUFS drifted to {loudness}"
        );
        let true_peak = true_peak.expect("real fixture true peak");
        assert!(
            (-21.0..=-12.0).contains(&true_peak),
            "real {extension} true peak drifted to {true_peak}"
        );

        cleanup_test_audio_path(&audio_path);
    }
}

fn assert_container_codec_support(fixture: TestEncodedFixture, extension: &str) {
    let audio_path = create_test_encoded_file(fixture, extension);
    let path = audio_path.to_string_lossy();
    let source = PlaybackSource::from_path(&path)
        .unwrap_or_else(|error| panic!("decode {extension} fixture: {error}"));
    assert!(source.total_duration().is_some());

    let (loudness, true_peak) =
        analyze_loudness_path_full(&audio_path, crate::audio::loudness::never_cancelled())
            .unwrap_or_else(|error| panic!("analyze {extension} fixture: {error}"));
    for mode in [AutoGainMode::Static, AutoGainMode::DynamicLookAhead] {
        let mut runtime = test_runtime_config();
        runtime.auto_gain.enabled = true;
        runtime.auto_gain.mode = mode;
        runtime.auto_gain.target_lufs = -14.0;
        let mut playback = ActivePlayback::new(
            format!("play-{extension}-{mode:?}"),
            format!("sound-{extension}"),
            path.to_string(),
            0,
            1.0,
            Some(loudness),
            true_peak,
            &runtime,
        )
        .unwrap_or_else(|error| panic!("create {mode:?} {extension} playback: {error}"));
        let mut local = vec![0.0; 4_096];
        let mut virtual_out = vec![0.0; 4_096];
        playback.render_into(&mut local, &mut virtual_out, &runtime);
        assert!(local.iter().any(|sample| *sample != 0.0));
        assert!(virtual_out.iter().any(|sample| *sample != 0.0));
    }

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn m4a_alac_decodes_analyzes_and_uses_auto_gain() {
    assert_container_codec_support(TestEncodedFixture::AlacM4aMono44100, "m4a");
}

#[test]
fn mp4_audio_decodes_analyzes_and_uses_auto_gain() {
    assert_container_codec_support(TestEncodedFixture::OpusMp4Stereo48000, "mp4");
}

#[test]
fn ogg_route_selection_is_content_based() {
    assert!(is_audio_file("/tmp/tone.opus"));
    assert!(is_audio_file("/tmp/tone.OPUS"));

    let vorbis_lower = create_test_encoded_file(TestEncodedFixture::VorbisMono44100, "ogg");
    let vorbis_upper = create_test_encoded_file(TestEncodedFixture::VorbisMono44100, "OGG");
    let opus_ogg = create_test_ogg_opus_file(TestOggOpusFixture::default());
    let opus_extension = create_test_ogg_opus_file(TestOggOpusFixture {
        extension: "opus",
        ..Default::default()
    });
    let opus_upper = create_test_ogg_opus_file(TestOggOpusFixture {
        extension: "OPUS",
        ..Default::default()
    });

    for path in [&vorbis_lower, &vorbis_upper] {
        let source = PlaybackSource::from_path(&path.to_string_lossy()).expect("open Ogg Vorbis");
        assert!(matches!(source, PlaybackSource::Symphonia(_)));
    }
    for path in [&opus_ogg, &opus_extension, &opus_upper] {
        let source = PlaybackSource::from_path(&path.to_string_lossy()).expect("open Ogg Opus");
        assert!(matches!(source, PlaybackSource::OggOpus(_)));
    }

    for path in [
        vorbis_lower,
        vorbis_upper,
        opus_ogg,
        opus_extension,
        opus_upper,
    ] {
        cleanup_test_audio_path(&path);
    }
}

#[test]
fn symphonia_source_decodes_and_seeks_libvorbis_streams() {
    for (fixture, expected_channels, expected_rate) in [
        (TestVorbisFixture::Mono44100, 1, 44_100),
        (TestVorbisFixture::Stereo48000, 2, 48_000),
    ] {
        let audio_path = create_test_vorbis_file(fixture);
        let path = audio_path.to_string_lossy();
        let mut source = PlaybackSource::from_path(&path).expect("create libvorbis source");

        assert_eq!(source.channels(), expected_channels);
        assert_eq!(source.sample_rate(), expected_rate);
        assert!(source
            .total_duration()
            .is_some_and(|duration| duration >= Duration::from_millis(900)));

        let initial_samples = source.by_ref().take(2_048).collect::<Vec<_>>();
        assert_eq!(initial_samples.len(), 2_048);
        assert!(initial_samples.iter().any(|sample| *sample != 0));

        source
            .try_seek(Duration::from_millis(500))
            .expect("seek libvorbis source");
        let seeked_samples = source.by_ref().take(512).collect::<Vec<_>>();
        assert_eq!(seeked_samples.len(), 512);
        assert!(seeked_samples.iter().any(|sample| *sample != 0));

        cleanup_test_audio_path(&audio_path);
    }
}

#[test]
fn resettable_playback_preserves_last_frame_for_opus_and_vorbis() {
    let paths = [
        create_test_ogg_opus_file(TestOggOpusFixture::default()),
        create_test_vorbis_file(TestVorbisFixture::Stereo48000),
    ];

    for audio_path in paths {
        let path = audio_path.to_string_lossy().to_string();
        let decoded = PlaybackSource::from_path(&path).expect("open source for frame count");
        let channels = usize::from(decoded.channels());
        let expected_samples = decoded.count() / channels * 2;
        let factory_path = path.clone();
        let factory: Box<dyn Fn() -> Result<PlaybackSource, EngineError>> =
            Box::new(move || PlaybackSource::from_path(&factory_path));
        let converted = ResettablePlaybackSource::new(factory, OPUS_SAMPLE_RATE)
            .expect("create resettable playback source");

        assert_eq!(converted.count(), expected_samples, "{path}");
        cleanup_test_audio_path(&audio_path);
    }
}

#[test]
fn active_playback_routes_libvorbis_through_common_mix_path() {
    let audio_path = create_test_vorbis_file(TestVorbisFixture::Stereo48000);
    let runtime = test_runtime_config();
    let mut playback = ActivePlayback::new(
        "play-vorbis".to_string(),
        "sound-vorbis".to_string(),
        audio_path.to_string_lossy().to_string(),
        0,
        1.0,
        None,
        None,
        &runtime,
    )
    .expect("create active libvorbis playback");

    let mut local = vec![0.0; 512];
    let mut virtual_out = vec![0.0; 512];
    playback.render_into(&mut local, &mut virtual_out, &runtime);

    assert!(local.iter().any(|sample| sample.abs() > f32::EPSILON));
    assert!(virtual_out.iter().any(|sample| sample.abs() > f32::EPSILON));

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_decodes_and_seek_discards() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture::default());
    let mut source =
        OggOpusSource::from_path(&audio_path.to_string_lossy()).expect("create ogg opus source");

    assert_eq!(source.channels(), 1);
    assert_eq!(source.sample_rate(), OPUS_SAMPLE_RATE);
    assert!(source
        .total_duration()
        .is_some_and(|duration| duration >= Duration::from_millis(40)));

    let first_samples: Vec<_> = source.by_ref().take(960).collect();
    assert!(first_samples.iter().any(|sample| *sample != 0));

    source
        .try_seek(Duration::from_millis(20))
        .expect("seek ogg opus source");
    let seeked_samples: Vec<_> = source.take(128).collect();
    assert!(seeked_samples.iter().any(|sample| *sample != 0));

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_accepts_original_input_rate_metadata() {
    for input_rate in [0, 44_100, 96_000] {
        let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
            input_rate,
            ..Default::default()
        });

        OggOpusSource::from_path(&audio_path.to_string_lossy()).unwrap_or_else(|error| {
            panic!("OpusHead input rate {input_rate} is metadata, not the decode rate: {error}")
        });
        cleanup_test_audio_path(&audio_path);
    }
}

#[test]
fn ogg_opus_source_trims_to_final_granule_after_pre_skip() {
    let pre_skip = 312u16;
    let playable_frames = 1_920u64;
    for channels in [1, 2] {
        let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
            channels,
            pre_skip,
            packet_count: 3,
            final_granule: Some(u64::from(pre_skip) + playable_frames),
            ..Default::default()
        });
        let source = OggOpusSource::from_path(&audio_path.to_string_lossy())
            .expect("open pre-skip/end-trim Opus fixture");
        assert_eq!(source.total_duration(), Some(Duration::from_millis(40)));

        assert_eq!(source.count() as u64, playable_frames * u64::from(channels));
        cleanup_test_audio_path(&audio_path);
    }
}

#[test]
fn ogg_opus_source_exposes_signed_header_gain() {
    for output_gain_q8 in [6 * 256, -6 * 256] {
        let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
            output_gain_q8,
            ..Default::default()
        });
        let source = OggOpusSource::from_path(&audio_path.to_string_lossy())
            .expect("open header-gain Opus fixture");
        let expected = 10.0_f32.powf(output_gain_q8 as f32 / (20.0 * 256.0));

        assert!((source.output_gain_factor() - expected).abs() < 0.000_1);
        cleanup_test_audio_path(&audio_path);
    }
}

#[test]
fn ogg_opus_seek_preserves_trim_and_header_gain() {
    let pre_skip = 312u16;
    let playable_frames = 1_920u64;
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        pre_skip,
        output_gain_q8: 6 * 256,
        packet_count: 3,
        final_granule: Some(u64::from(pre_skip) + playable_frames),
        ..Default::default()
    });
    let path = audio_path.to_string_lossy().to_string();
    let factory_path = path.clone();
    let factory: Box<dyn Fn() -> Result<PlaybackSource, EngineError>> =
        Box::new(move || PlaybackSource::from_path(&factory_path));
    let mut source = ResettablePlaybackSource::new(factory, OPUS_SAMPLE_RATE)
        .expect("create resettable Ogg Opus source");
    let expected_gain = 10.0_f32.powf(6.0 / 20.0);
    assert!((source.output_gain_factor() - expected_gain).abs() < 0.000_1);

    source
        .seek_internal(Duration::from_millis(20))
        .expect("seek trimmed Ogg Opus source");

    assert!((source.output_gain_factor() - expected_gain).abs() < 0.000_1);
    assert!(source.take(128).any(|sample| sample != 0));

    let mut decoded = OggOpusSource::from_path(&path).expect("reopen trimmed Ogg Opus source");
    decoded
        .try_seek(Duration::from_millis(20))
        .expect("seek exact Ogg Opus decoder");
    assert_eq!(decoded.count(), 960);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_malformed_or_unsupported_headers() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        channel_mapping_family: 1,
        ..Default::default()
    });
    let error = match OggOpusSource::from_path(&audio_path.to_string_lossy()) {
        Ok(_) => panic!("unsupported channel mapping must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("channel mapping family: 1"));
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_invalid_final_granule() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        pre_skip: 312,
        final_granule: Some(311),
        ..Default::default()
    });
    let error = match OggOpusSource::from_path(&audio_path.to_string_lossy()) {
        Ok(_) => panic!("final granule smaller than pre-skip must fail"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("smaller than pre-skip"));
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_missing_finite_audio_granule() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        final_granule: Some(u64::MAX),
        ..Default::default()
    });
    let error = match OggOpusSource::from_path(&audio_path.to_string_lossy()) {
        Ok(_) => panic!("sentinel-only audio granule must fail"),
        Err(error) => error.to_string(),
    };

    assert!(error.contains("no valid final granule"));
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_truncated_stream() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture::default());
    let mut bytes = std::fs::read(&audio_path).expect("read Ogg Opus fixture");
    bytes.truncate(bytes.len() - 8);
    std::fs::write(&audio_path, bytes).expect("truncate Ogg Opus fixture");

    let error = match OggOpusSource::from_path(&audio_path.to_string_lossy()) {
        Ok(_) => panic!("truncated Ogg Opus stream must fail"),
        Err(error) => error.to_string(),
    };

    assert!(
        error.contains("no audio packets"),
        "unexpected error: {error}"
    );
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_decodes_conforming_mono_and_stereo_to_the_granule_bound() {
    let pre_skip = 312u16;
    let playable_frames = 1_920u64;
    for channels in [1, 2] {
        let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
            channels,
            pre_skip,
            packet_count: 3,
            final_granule: Some(u64::from(pre_skip) + playable_frames),
            ..Default::default()
        });

        assert!(test_ogg_has_end_of_stream_page(&audio_path));
        let mut source = OggOpusSource::from_path(&audio_path.to_string_lossy())
            .expect("open a conforming Ogg Opus fixture");
        assert_eq!(source.channels(), channels);
        assert_eq!(source.sample_rate(), OPUS_SAMPLE_RATE);
        assert_eq!(source.total_duration(), Some(Duration::from_millis(40)));

        let samples: Vec<i16> = source.by_ref().collect();
        assert_eq!(samples.len() as u64, playable_frames * u64::from(channels));
        assert!(
            samples.iter().any(|sample| *sample != 0),
            "decoded PCM must not be silent"
        );
        cleanup_test_audio_path(&audio_path);
    }
}

#[test]
fn ogg_opus_source_recovers_a_complete_stream_without_an_end_of_stream_page() {
    let pre_skip = 312u16;
    let playable_frames = 1_920u64;
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        pre_skip,
        packet_count: 3,
        final_granule: Some(u64::from(pre_skip) + playable_frames),
        final_page: TestOggOpusFinalPage::NoEndOfStreamMarker,
        ..Default::default()
    });
    assert!(!test_ogg_has_end_of_stream_page(&audio_path));

    let mut source =
        OggOpusSource::from_path(&audio_path.to_string_lossy()).unwrap_or_else(|error| {
            panic!("a complete stream without an end-of-stream page must decode: {error}")
        });

    assert_eq!(source.total_duration(), Some(Duration::from_millis(40)));
    let samples: Vec<i16> = source.by_ref().collect();
    assert_eq!(
        samples.len() as u64,
        playable_frames,
        "the granule bound is exact: no fabricated samples and no padding"
    );
    assert!(samples.iter().any(|sample| *sample != 0));
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_recovers_the_complete_pages_before_a_truncated_tail() {
    let pre_skip = 312u16;
    let playable_frames = 1_920u64;
    let granule = u64::from(pre_skip) + playable_frames;
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        pre_skip,
        packet_count: 3,
        final_granule: Some(granule),
        final_page: TestOggOpusFinalPage::NoEndOfStreamMarker,
        ..Default::default()
    });
    let complete_len = std::fs::metadata(&audio_path)
        .expect("stat the fixture")
        .len();
    append_truncated_test_ogg_page(&audio_path, TEST_OGG_OPUS_SERIAL, granule, 220, 40);
    assert!(
        std::fs::metadata(&audio_path)
            .expect("stat the fixture")
            .len()
            > complete_len,
        "the truncated page must have been appended"
    );

    let mut source =
        OggOpusSource::from_path(&audio_path.to_string_lossy()).unwrap_or_else(|error| {
            panic!("a truncated tail must not discard complete audio: {error}")
        });

    assert_eq!(source.total_duration(), Some(Duration::from_millis(40)));
    let samples: Vec<i16> = source.by_ref().collect();
    assert_eq!(samples.len() as u64, playable_frames);
    assert!(samples.iter().any(|sample| *sample != 0));
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_truncation_before_any_audio() {
    let headers_only = create_test_ogg_opus_file(TestOggOpusFixture {
        packet_count: 0,
        ..Default::default()
    });
    let error = match OggOpusSource::from_path(&headers_only.to_string_lossy()) {
        Ok(_) => panic!("a header-only stream has no audio to play"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("no audio packets"),
        "unexpected error: {error}"
    );

    let cut_header = create_test_ogg_opus_file(TestOggOpusFixture::default());
    let bytes = std::fs::read(&cut_header).expect("read the fixture");
    std::fs::write(&cut_header, &bytes[..20]).expect("truncate inside the header page");
    assert!(
        OggOpusSource::from_path(&cut_header.to_string_lossy()).is_err(),
        "a stream cut inside its header region must fail"
    );

    cleanup_test_audio_path(&headers_only);
    cleanup_test_audio_path(&cut_header);
}

fn append_audio_packet(path: &std::path::Path, granule: u64, packet: &[u8]) {
    append_test_ogg_page(path, TEST_OGG_OPUS_SERIAL, granule, 0x00, packet);
}

fn ogg_opus_headers_only() -> std::path::PathBuf {
    create_test_ogg_opus_file(TestOggOpusFixture {
        packet_count: 0,
        ..Default::default()
    })
}

fn assert_invalid_packet_rejected(path: &std::path::Path) {
    let error = match OggOpusSource::from_path(&path.to_string_lossy()) {
        Ok(source) => panic!(
            "an invalid Opus packet must fail construction, got {:?}",
            source.total_duration()
        ),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("Failed to decode Ogg Opus audio packet"),
        "the error must name the packet decode failure: {error}"
    );
    assert!(
        !error.contains("end-of-stream"),
        "codec corruption must not be reported as container truncation: {error}"
    );
}

#[test]
fn ogg_opus_source_rejects_an_invalid_audio_packet() {
    let audio_path = ogg_opus_headers_only();
    append_audio_packet(&audio_path, 960, INVALID_TEST_OPUS_PACKET);

    assert_invalid_packet_rejected(&audio_path);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_an_invalid_packet_after_a_valid_one() {
    let audio_path = ogg_opus_headers_only();
    append_audio_packet(&audio_path, 960, &encode_test_opus_audio_packet(0));
    append_audio_packet(&audio_path, 1_920, INVALID_TEST_OPUS_PACKET);

    assert_invalid_packet_rejected(&audio_path);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_an_invalid_packet_before_a_valid_one() {
    let audio_path = ogg_opus_headers_only();
    append_audio_packet(&audio_path, 960, INVALID_TEST_OPUS_PACKET);
    append_audio_packet(&audio_path, 1_920, &encode_test_opus_audio_packet(1));

    assert_invalid_packet_rejected(&audio_path);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_a_stream_whose_audio_packets_are_all_invalid() {
    let audio_path = ogg_opus_headers_only();
    for index in 1..=3u64 {
        append_audio_packet(&audio_path, index * 960, INVALID_TEST_OPUS_PACKET);
    }

    assert_invalid_packet_rejected(&audio_path);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_an_invalid_packet_without_an_end_of_stream_page() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        packet_count: 1,
        final_page: TestOggOpusFinalPage::NoEndOfStreamMarker,
        ..Default::default()
    });
    assert!(!test_ogg_has_end_of_stream_page(&audio_path));
    append_audio_packet(&audio_path, 1_920, INVALID_TEST_OPUS_PACKET);

    assert_invalid_packet_rejected(&audio_path);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_an_invalid_packet_before_a_truncated_tail() {
    let audio_path = ogg_opus_headers_only();
    append_audio_packet(&audio_path, 960, INVALID_TEST_OPUS_PACKET);
    append_truncated_test_ogg_page(&audio_path, TEST_OGG_OPUS_SERIAL, 1_920, 220, 40);

    assert_invalid_packet_rejected(&audio_path);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_rejects_terminal_eof_without_usable_audio_or_granule() {
    let no_audio = ogg_opus_headers_only();
    append_truncated_test_ogg_page(&no_audio, TEST_OGG_OPUS_SERIAL, 960, 220, 40);
    let error = match OggOpusSource::from_path(&no_audio.to_string_lossy()) {
        Ok(_) => panic!("terminal EOF with no audio must not construct a source"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("no audio packets"),
        "unexpected error: {error}"
    );

    let no_granule = create_test_ogg_opus_file(TestOggOpusFixture {
        packet_count: 1,
        final_granule: Some(u64::MAX),
        final_page: TestOggOpusFinalPage::NoEndOfStreamMarker,
        ..Default::default()
    });
    append_truncated_test_ogg_page(&no_granule, TEST_OGG_OPUS_SERIAL, 1_920, 220, 40);
    let error = match OggOpusSource::from_path(&no_granule.to_string_lossy()) {
        Ok(_) => panic!("terminal EOF with no usable granule must not construct a source"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("no valid final granule"),
        "unexpected error: {error}"
    );

    cleanup_test_audio_path(&no_audio);
    cleanup_test_audio_path(&no_granule);
}

#[test]
fn ogg_opus_source_rejects_mid_stream_crc_corruption() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        packet_count: 1,
        final_page: TestOggOpusFinalPage::NoEndOfStreamMarker,
        ..Default::default()
    });

    append_test_ogg_page(&audio_path, TEST_OGG_OPUS_SERIAL, 960, 0x00, &[0x03]);
    corrupt_test_ogg_page_body(&audio_path, 2);

    let error = match OggOpusSource::from_path(&audio_path.to_string_lossy()) {
        Ok(_) => panic!("CRC corruption must stay fatal"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("Failed to scan"),
        "unexpected error: {error}"
    );
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_accepts_a_spec_legal_nil_end_of_stream_page() {
    let pre_skip = 312u16;
    let playable_frames = 960u64;
    let granule = u64::from(pre_skip) + playable_frames;
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        pre_skip,
        packet_count: 2,
        final_granule: Some(granule),
        final_page: TestOggOpusFinalPage::NoEndOfStreamMarker,
        ..Default::default()
    });
    append_test_ogg_page(&audio_path, TEST_OGG_OPUS_SERIAL, granule, 0x04, &[]);
    assert!(
        test_ogg_has_end_of_stream_page(&audio_path),
        "the fixture must carry a nil end-of-stream page"
    );

    let mut source = OggOpusSource::from_path(&audio_path.to_string_lossy())
        .expect("a nil end-of-stream page must not fail the stream");
    assert_eq!(source.by_ref().count() as u64, playable_frames);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn ogg_opus_source_uses_only_the_first_logical_stream() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        packet_count: 1,
        final_page: TestOggOpusFinalPage::NoEndOfStreamMarker,
        ..Default::default()
    });

    let other_serial = 0x1122_3344;
    let mut head = b"OpusHead".to_vec();
    head.push(1);
    head.push(1);
    head.extend_from_slice(&0u16.to_le_bytes());
    head.extend_from_slice(&48_000u32.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    append_test_ogg_page(&audio_path, other_serial, 0, 0x02, &head);
    append_test_ogg_page(&audio_path, other_serial, 48_000, 0x00, &[0x03]);

    let mut source = OggOpusSource::from_path(&audio_path.to_string_lossy())
        .expect("the first logical stream still decodes");
    assert_eq!(
        source.by_ref().count(),
        960,
        "the second logical stream's granule must not extend the duration"
    );
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn active_playback_routes_ogg_opus_through_common_mix_path() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture::default());
    let runtime = test_runtime_config();
    let mut playback = ActivePlayback::new(
        "play-opus".to_string(),
        "sound-opus".to_string(),
        audio_path.to_string_lossy().to_string(),
        0,
        1.0,
        None,
        None,
        &runtime,
    )
    .expect("create active ogg opus playback");

    let mut local = vec![0.0; 512];
    let mut virtual_out = vec![0.0; 512];
    playback.render_into(&mut local, &mut virtual_out, &runtime);

    assert!(local.iter().any(|sample| sample.abs() > f32::EPSILON));
    assert!(virtual_out.iter().any(|sample| sample.abs() > f32::EPSILON));

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn active_playback_applies_signed_ogg_opus_header_gain() {
    let render_level = |output_gain_q8| {
        let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
            output_gain_q8,
            ..Default::default()
        });
        let runtime = test_runtime_config();
        let mut playback = ActivePlayback::new(
            "play-opus-gain".to_string(),
            "sound-opus-gain".to_string(),
            audio_path.to_string_lossy().to_string(),
            0,
            1.0,
            None,
            None,
            &runtime,
        )
        .expect("create header-gain Ogg Opus playback");
        let mut local = vec![0.0; 1_024];
        let mut virtual_out = vec![0.0; 1_024];
        playback.render_into(&mut local, &mut virtual_out, &runtime);
        cleanup_test_audio_path(&audio_path);
        local.into_iter().map(f32::abs).sum::<f32>()
    };

    let unity = render_level(0);
    let boosted = render_level(6 * 256);
    let attenuated = render_level(-6 * 256);
    let six_db = 10.0_f32.powf(6.0 / 20.0);

    assert!((boosted / unity - six_db).abs() < 0.01);
    assert!((attenuated / unity - six_db.recip()).abs() < 0.01);
}

#[test]
fn active_playback_applies_static_and_dynamic_auto_gain_to_ogg_opus_outputs() {
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        output_gain_q8: 3 * 256,
        packet_count: 60,
        ..Default::default()
    });
    let render = |runtime: &RuntimeConfig, true_peak_dbtp| {
        let mut playback = ActivePlayback::new(
            "play-opus-auto-gain".to_string(),
            "sound-opus-auto-gain".to_string(),
            audio_path.to_string_lossy().to_string(),
            0,
            1.0,
            Some(-20.0),
            true_peak_dbtp,
            runtime,
        )
        .expect("create auto-gain Ogg Opus playback");
        let mut local = vec![0.0; 1_024];
        let mut virtual_out = vec![0.0; 1_024];
        playback.render_into(&mut local, &mut virtual_out, runtime);
        (local, virtual_out)
    };

    let disabled = test_runtime_config();
    let (disabled_local, _) = render(&disabled, None);
    let disabled_level = disabled_local.into_iter().map(f32::abs).sum::<f32>();

    let mut static_gain = test_runtime_config();
    static_gain.auto_gain.enabled = true;
    static_gain.auto_gain.mode = AutoGainMode::Static;
    let (static_local, static_virtual) = render(&static_gain, None);
    let static_level = static_local.iter().copied().map(f32::abs).sum::<f32>();
    assert!((static_level / disabled_level - 10.0_f32.powf(6.0 / 20.0)).abs() < 0.01);
    assert_eq!(static_local, static_virtual);
    let (peak_limited_static, _) = render(&static_gain, Some(-1.0));
    let peak_limited_static_level = peak_limited_static
        .iter()
        .map(|sample| sample.abs())
        .sum::<f32>();

    let mut dynamic_gain = static_gain;
    dynamic_gain.auto_gain.mode = AutoGainMode::DynamicLookAhead;
    let (dynamic_local, dynamic_virtual) = render(&dynamic_gain, None);
    assert!(dynamic_local.iter().any(|sample| sample.abs() > 0.001));
    assert!(dynamic_virtual.iter().any(|sample| sample.abs() > 0.001));
    assert!(dynamic_local.iter().all(|sample| sample.abs() <= 1.0));
    assert!(dynamic_virtual.iter().all(|sample| sample.abs() <= 1.0));
    let (peak_limited_dynamic, _) = render(&dynamic_gain, Some(-1.0));
    let peak_limited_dynamic_level = peak_limited_dynamic
        .iter()
        .map(|sample| sample.abs())
        .sum::<f32>();
    assert!(peak_limited_dynamic_level > peak_limited_static_level * 1.9);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn active_playback_loops_trimmed_ogg_opus() {
    let pre_skip = 312u16;
    let audio_path = create_test_ogg_opus_file(TestOggOpusFixture {
        pre_skip,
        packet_count: 3,
        final_granule: Some(u64::from(pre_skip) + 1_920),
        ..Default::default()
    });
    let mut runtime = test_runtime_config();
    runtime.looping = true;
    let mut playback = ActivePlayback::new(
        "play-opus-loop".to_string(),
        "sound-opus-loop".to_string(),
        audio_path.to_string_lossy().to_string(),
        0,
        1.0,
        None,
        None,
        &runtime,
    )
    .expect("create looping Ogg Opus playback");
    let mut local = vec![0.0; 5_000];
    let mut virtual_out = vec![0.0; 5_000];

    playback.render_into(&mut local, &mut virtual_out, &runtime);

    assert!(!playback.finished);
    assert!(local[4_000..].iter().any(|sample| sample.abs() > 0.001));
    assert!(virtual_out[4_000..]
        .iter()
        .any(|sample| sample.abs() > 0.001));
    cleanup_test_audio_path(&audio_path);
}
