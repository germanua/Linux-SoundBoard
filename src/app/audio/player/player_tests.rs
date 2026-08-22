use super::source_routing;
use super::*;

#[test]
fn shutdown_policy_restores_only_transient_engines() {
    assert!(!ShutdownPolicy::Persistent.restores_default_source());
    assert!(ShutdownPolicy::Transient.restores_default_source());
}
use crate::app_meta::VIRTUAL_MIC_DESCRIPTION;
use crate::audio::loudness::analyze_loudness_path_full;
use crate::audio::metadata::probe_duration_ms;
use crate::audio::scanner::is_audio_file;
use crate::test_support::audio_fixtures::{
    cleanup_test_audio_path, create_test_audio_file, create_test_encoded_file,
    create_test_ogg_opus_file, create_test_vorbis_file, TestEncodedFixture, TestOggOpusFixture,
    TestVorbisFixture,
};
use std::sync::Arc;

fn test_runtime_config() -> RuntimeConfig {
    super::test_runtime_config_with_mode(DefaultSourceMode::Manual)
}

fn test_player_snapshot_store() -> Arc<RwLock<PlayerSnapshot>> {
    super::test_player_snapshot_store()
}

#[test]
fn dynamic_auto_gain_uses_limiter_instead_of_static_true_peak_clamp() {
    let mut runtime = test_runtime_config();
    runtime.auto_gain.enabled = true;
    runtime.auto_gain.target_lufs = -12.0;

    runtime.auto_gain.mode = AutoGainMode::Static;
    let static_gain = runtime.auto_gain.gain_for(Some(-14.0), Some(-1.0), false);
    assert!((static_gain - 1.0).abs() < 0.001);

    runtime.auto_gain.mode = AutoGainMode::DynamicLookAhead;
    let dynamic_gain = runtime.auto_gain.gain_for(Some(-14.0), Some(-1.0), false);
    let expected = 10.0_f32.powf(2.0 / 20.0);
    assert!((dynamic_gain - expected).abs() < 0.001);
}

fn test_source(id: u32, node_name: &str, display_name: &str, priority: i32) -> SourceDescriptor {
    SourceDescriptor {
        id,
        serial: None,
        node_name: node_name.to_string(),
        display_name: display_name.to_string(),
        priority_session: priority,
        is_monitor: node_name.ends_with(".monitor"),
        is_our_virtual_mic: node_name == VIRTUAL_SOURCE_NAME,
        is_virtual: false,
        is_hardware_backed: node_name.starts_with("alsa_input.")
            || node_name.starts_with("bluez_input.")
            || node_name.starts_with("v4l2_input."),
    }
}

#[test]
fn parse_wpctl_node_name_extracts_quoted_name() {
    let output = r#"
id 72, type PipeWire:Interface:Node
  * node.name = "alsa_input.pci-0000_12_00.6.analog-stereo"
"#;
    assert_eq!(
        parse_wpctl_node_name(output).as_deref(),
        Some("alsa_input.pci-0000_12_00.6.analog-stereo")
    );
}

#[test]
fn list_audio_sources_includes_virtual_third_parties_excludes_own_and_monitors() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());

    state.sources.insert(
        10,
        SourceDescriptor {
            id: 10,
            serial: None,
            node_name: "alsa_input.pci-0000_12_00.6.analog-stereo".to_string(),
            display_name: "Ryzen HD Audio".to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        },
    );
    state.sources.insert(
        11,
        SourceDescriptor {
            id: 11,
            serial: None,
            node_name: "easyeffects_source".to_string(),
            display_name: "Easy Effects Source".to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );
    state.sources.insert(
        12,
        SourceDescriptor {
            id: 12,
            serial: None,
            node_name: VIRTUAL_SOURCE_NAME.to_string(),
            display_name: VIRTUAL_MIC_DESCRIPTION.to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: true,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );
    state.sources.insert(
        13,
        SourceDescriptor {
            id: 13,
            serial: None,
            node_name: "alsa_output.pci-0000_12_00.6.analog-stereo.monitor".to_string(),
            display_name: "Speaker Monitor".to_string(),
            priority_session: 0,
            is_monitor: true,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        },
    );

    let listed = state.list_audio_sources();
    let names: Vec<_> = listed.iter().map(|s| s.node_name.as_str()).collect();
    assert!(names.contains(&"alsa_input.pci-0000_12_00.6.analog-stereo"));
    assert!(names.contains(&"easyeffects_source"));
    assert!(!names.contains(&VIRTUAL_SOURCE_NAME));
    assert!(!names.iter().any(|name| name.ends_with(".monitor")));
}

#[test]
fn build_playback_positions_prefers_newest_unfinished_entries() {
    let mut registry = HashMap::new();
    registry.insert(
        "play-old".to_string(),
        PlaybackSnapshot {
            sound_id: "sound-old".to_string(),
            playback_order: 1,
            position_ms: 1_000,
            paused: false,
            duration_ms: Some(10_000),
            finished: false,
        },
    );
    registry.insert(
        "play-new".to_string(),
        PlaybackSnapshot {
            sound_id: "sound-new".to_string(),
            playback_order: 2,
            position_ms: 250,
            paused: false,
            duration_ms: Some(10_000),
            finished: false,
        },
    );
    registry.insert(
        "play-finished".to_string(),
        PlaybackSnapshot {
            sound_id: "sound-finished".to_string(),
            playback_order: 3,
            position_ms: 10_000,
            paused: false,
            duration_ms: Some(10_000),
            finished: true,
        },
    );

    let positions = build_playback_positions(&registry);
    assert_eq!(positions[0].play_id, "play-new");
    assert_eq!(positions[1].play_id, "play-old");
    assert_eq!(positions[2].play_id, "play-finished");
}

#[test]
fn resolve_source_id_by_name_finds_matching_source() {
    let sources = HashMap::from([(
        7,
        SourceDescriptor {
            id: 7,
            serial: None,
            node_name: "alsa_input.pci-0000_12_00.6.analog-stereo".to_string(),
            display_name: "Mic".to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        },
    )]);

    assert_eq!(
        resolve_source_id_by_name(&sources, "alsa_input.pci-0000_12_00.6.analog-stereo"),
        Some(7)
    );
    assert_eq!(resolve_source_id_by_name(&sources, "missing"), None);
}

#[test]
fn restore_default_source_stops_claim_without_random_fallback() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.claimed_default = true;
    state.previous_default_source_name = Some("missing.source".to_string());
    state.sources.insert(
        2,
        test_source(2, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 0),
    );

    source_routing::restore_default_source(&mut state).unwrap();

    assert!(!state.claimed_default);
    assert_eq!(
        state.previous_default_source_name.as_deref(),
        Some("missing.source")
    );
}

#[test]
fn transient_shutdown_prefers_previous_valid_source() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.previous_default_source_name = Some("alsa_input.previous".to_string());
    state
        .sources
        .insert(1, test_source(1, "alsa_input.previous", "Previous Mic", 1));
    state.sources.insert(
        2,
        SourceDescriptor {
            id: 2,
            serial: None,
            node_name: "easyeffects_source".to_string(),
            display_name: "Easy Effects Source".to_string(),
            priority_session: 9_000,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );

    assert_eq!(
        source_routing::transient_restore_target(&state),
        Some((1, "alsa_input.previous".to_string()))
    );
}

#[test]
fn transient_shutdown_uses_ranked_fallback_when_previous_source_is_missing() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.previous_default_source_name = Some("missing.source".to_string());
    state.sources.insert(
        1,
        test_source(1, "alsa_input.hardware", "Hardware Mic", 9_000),
    );
    state.sources.insert(
        2,
        SourceDescriptor {
            id: 2,
            serial: None,
            node_name: "easyeffects_source".to_string(),
            display_name: "Easy Effects Source".to_string(),
            priority_session: 1,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );

    assert_eq!(
        source_routing::transient_restore_target(&state),
        Some((2, "easyeffects_source".to_string()))
    );
}

#[test]
fn transient_shutdown_never_selects_monitor_or_unrelated_virtual_cable() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.previous_default_source_name = Some("alsa_output.monitor".to_string());
    state.sources.insert(
        1,
        SourceDescriptor {
            id: 1,
            serial: None,
            node_name: "alsa_output.monitor".to_string(),
            display_name: "Monitor".to_string(),
            priority_session: 9_000,
            is_monitor: true,
            is_our_virtual_mic: false,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );
    state.sources.insert(
        2,
        SourceDescriptor {
            id: 2,
            serial: None,
            node_name: "virtual.cable".to_string(),
            display_name: "Virtual Cable".to_string(),
            priority_session: 9_999,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );

    assert_eq!(source_routing::transient_restore_target(&state), None);
}

#[test]
fn explicit_selected_mic_waits_for_exact_source() {
    let mut runtime = test_runtime_config();
    runtime.mic_source = Some("easyeffects_source".to_string());
    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    state
        .sources
        .insert(7, test_source(7, "alsa_input.real", "Real Mic", 2000));

    assert_eq!(
        resolve_capture_target_from_default(&state, Some("alsa_input.real".to_string())),
        None
    );

    state.sources.insert(
        8,
        test_source(8, "easyeffects_source", "Easy Effects", 1000),
    );
    assert_eq!(
        resolve_capture_target_from_default(&state, Some("alsa_input.real".to_string())).as_deref(),
        Some("easyeffects_source")
    );
}

#[test]
fn explicit_selected_mic_rejects_linux_soundboard_virtual_mic() {
    let mut runtime = test_runtime_config();
    runtime.mic_source = Some(VIRTUAL_SOURCE_NAME.to_string());
    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    state.sources.insert(
        8,
        test_source(8, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 5000),
    );

    assert_eq!(
        resolve_capture_target_from_default(&state, Some(VIRTUAL_SOURCE_NAME.to_string())),
        None
    );
}

#[test]
fn auto_capture_prefers_enhancement_source_over_default_and_previous() {
    // easyeffects_source registered alongside physical mics: it wins whatever
    // PipeWire calls the default and whatever previous_default_source_name holds.
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.sources.insert(
        6,
        SourceDescriptor {
            id: 6,
            serial: None,
            node_name: "easyeffects_source".to_string(),
            display_name: "Easy Effects Source".to_string(),
            priority_session: 10,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: true,          // Audio/Source/Virtual at runtime
            is_hardware_backed: false, // no device.api
        },
    );
    state
        .sources
        .insert(7, test_source(7, "alsa_input.low", "Low Priority", 100));
    state
        .sources
        .insert(8, test_source(8, "alsa_input.high", "High Priority", 200));
    state.sources.insert(
        9,
        test_source(9, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 5000),
    );
    state.sources.insert(
        10,
        test_source(10, "alsa_output.speakers.monitor", "Monitor", 9000),
    );

    // Even when default_source is a valid physical mic, enhancement wins.
    assert_eq!(
        resolve_capture_target_from_default(&state, Some("alsa_input.low".to_string())).as_deref(),
        Some("easyeffects_source")
    );

    // Even when previous_default_source_name is set to a physical mic, enhancement wins.
    state.previous_default_source_name = Some("alsa_input.low".to_string());
    assert_eq!(
        resolve_capture_target_from_default(&state, Some(VIRTUAL_SOURCE_NAME.to_string()))
            .as_deref(),
        Some("easyeffects_source")
    );

    // best_upstream_mic_source_name independently confirms easyeffects_source scores highest.
    assert_eq!(
        best_upstream_mic_source_name(&state.sources).as_deref(),
        Some("easyeffects_source")
    );

    // Without previous_default_source_name, still returns easyeffects_source.
    state.previous_default_source_name = None;
    assert_eq!(
        resolve_capture_target_from_default(&state, Some(VIRTUAL_SOURCE_NAME.to_string()))
            .as_deref(),
        Some("easyeffects_source")
    );
}

#[test]
fn auto_capture_falls_back_to_physical_mic_when_no_enhancement_source() {
    // When EasyEffects is not running, best_upstream_mic_source_name picks
    // the highest-priority physical mic by priority_session score.
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state
        .sources
        .insert(7, test_source(7, "alsa_input.low", "Low Priority", 100));
    state
        .sources
        .insert(8, test_source(8, "alsa_input.high", "High Priority", 200));
    state.sources.insert(
        9,
        test_source(9, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 5000),
    );

    // alsa_input.high has higher priority_session (200 > 100) so it wins.
    assert_eq!(
        resolve_capture_target_from_default(&state, Some("alsa_input.low".to_string())).as_deref(),
        Some("alsa_input.high")
    );
}

#[test]
fn auto_capture_ignores_unrecognized_virtual_source_in_favor_of_hardware_mic() {
    // An unrecognised virtual source (Vencord screenshare null sink) is neither
    // a known enhancement chain nor hardware-backed, so auto-detect must skip it
    // for a real mic. A high priority.session must not rescue it.
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.sources.insert(
        1,
        SourceDescriptor {
            id: 1,
            serial: None,
            node_name: "vencord-screen-share".to_string(),
            display_name: "vencord-screen-share".to_string(),
            priority_session: 9999,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );
    state.sources.insert(
        2,
        SourceDescriptor {
            id: 2,
            serial: None,
            node_name: "alsa_input.usb_mic".to_string(),
            display_name: "USB Microphone".to_string(),
            priority_session: 100,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        },
    );
    assert_eq!(
        best_upstream_mic_source_name(&state.sources).as_deref(),
        Some("alsa_input.usb_mic"),
        "an unrecognised virtual source (screenshare) must never beat a real mic"
    );
}

#[test]
fn auto_capture_trusts_named_enhancement_over_hardware_mic() {
    // A recognised enhancement chain (matched by node name) is the user's
    // deliberate processed-mic feed and outranks a raw hardware mic even though it
    // is not hardware-backed itself.
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.sources.insert(
        1,
        SourceDescriptor {
            id: 1,
            serial: None,
            node_name: "noisetorch".to_string(),
            display_name: "NoiseTorch Microphone".to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );
    state.sources.insert(
        2,
        SourceDescriptor {
            id: 2,
            serial: None,
            node_name: "alsa_input.usb_mic".to_string(),
            display_name: "USB Microphone".to_string(),
            priority_session: 9999,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        },
    );
    assert_eq!(
        best_upstream_mic_source_name(&state.sources).as_deref(),
        Some("noisetorch"),
        "a recognised enhancement chain still beats a raw hardware mic"
    );
}

#[test]
fn auto_capture_returns_none_when_only_unrecognized_virtual_source_present() {
    // Only a screenshare null sink registered: pick nothing and wait for a real
    // mic rather than pipe app audio into the virtual mic. The default/previous
    // fallback must not resurrect it either.
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.sources.insert(
        1,
        SourceDescriptor {
            id: 1,
            serial: None,
            node_name: "vencord-screen-share".to_string(),
            display_name: "vencord-screen-share".to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );
    assert_eq!(best_upstream_mic_source_name(&state.sources), None);
    assert_eq!(
        resolve_capture_target_from_default(&state, Some("vencord-screen-share".to_string())),
        None,
        "an unrecognised virtual source must not be selected even as a fallback"
    );
}

#[test]
fn selected_enhancement_source_does_not_silently_fall_back() {
    // User picked easyeffects_source and it isn't registered. Even with a
    // perfectly good physical mic sitting right there, resolution returns None
    // and the caller warns and waits. Never silently swap in the raw mic.
    let mut runtime = test_runtime_config();
    runtime.mic_source = Some("easyeffects_source".to_string());
    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    state
        .sources
        .insert(1, test_source(1, "alsa_input.usb_mic", "USB Mic", 9000));

    assert_eq!(
        resolve_capture_target_from_default(&state, Some("alsa_input.usb_mic".to_string())),
        None,
        "must not fall back to the physical mic when the user picked an enhancement source"
    );

    // When the enhancement source DOES appear, resolution returns it.
    state.sources.insert(
        2,
        test_source(2, "easyeffects_source", "Easy Effects Source", 10),
    );
    assert_eq!(
        resolve_capture_target_from_default(&state, None).as_deref(),
        Some("easyeffects_source")
    );
}

#[test]
fn pipewire_capture_health_requires_non_error_linked_expected_target() {
    let sources = HashMap::from([(
        78,
        test_source(
            78,
            "alsa_input.pci-0000_12_00.6.analog-stereo",
            "Real Mic",
            2000,
        ),
    )]);
    let mut links = HashMap::new();

    assert!(!pipewire_capture_link_healthy(
        Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
        Some(253),
        ManagedStreamState::Streaming,
        &sources,
        &links,
    ));

    links.insert(
        1,
        LinkDescriptor {
            id: 1,
            output_node_id: 78,
            input_node_id: 999,
            output_port_id: None,
            input_port_id: None,
        },
    );
    assert!(!pipewire_capture_link_healthy(
        Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
        Some(253),
        ManagedStreamState::Streaming,
        &sources,
        &links,
    ));

    links.insert(
        2,
        LinkDescriptor {
            id: 2,
            output_node_id: 78,
            input_node_id: 253,
            output_port_id: None,
            input_port_id: None,
        },
    );
    assert!(pipewire_capture_link_healthy(
        Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
        Some(253),
        ManagedStreamState::Paused,
        &sources,
        &links,
    ));
    assert!(!pipewire_capture_link_healthy(
        Some("alsa_input.pci-0000_12_00.6.analog-stereo"),
        Some(253),
        ManagedStreamState::Error,
        &sources,
        &links,
    ));
}

#[test]
fn pipewire_capture_health_rejects_self_capture_from_virtual_mic() {
    let sources = HashMap::from([(
        32,
        test_source(32, VIRTUAL_SOURCE_NAME, VIRTUAL_MIC_DESCRIPTION, 5000),
    )]);
    let links = HashMap::from([(
        1,
        LinkDescriptor {
            id: 1,
            output_node_id: 32,
            input_node_id: 253,
            output_port_id: None,
            input_port_id: None,
        },
    )]);

    assert!(!pipewire_capture_link_healthy(
        Some(VIRTUAL_SOURCE_NAME),
        Some(253),
        ManagedStreamState::Streaming,
        &sources,
        &links,
    ));
}

#[test]
fn loop_state_filters_virtual_and_monitor_sources() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.sources.insert(
        1,
        SourceDescriptor {
            id: 1,
            serial: None,
            node_name: "alsa_input.real".to_string(),
            display_name: "Real Mic".to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        },
    );
    state.sources.insert(
        2,
        SourceDescriptor {
            id: 2,
            serial: None,
            node_name: "alsa_output.monitor".to_string(),
            display_name: "Monitor".to_string(),
            priority_session: 0,
            is_monitor: true,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        },
    );
    state.sources.insert(
        3,
        SourceDescriptor {
            id: 3,
            serial: None,
            node_name: VIRTUAL_SOURCE_NAME.to_string(),
            display_name: VIRTUAL_MIC_DESCRIPTION.to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: true,
            is_virtual: true,
            is_hardware_backed: false,
        },
    );

    let visible = state.list_audio_sources();
    assert_eq!(
        visible,
        vec![AudioSourceInfo {
            node_name: "alsa_input.real".to_string(),
            display_name: "Real Mic".to_string(),
            is_virtual: false,
            is_hardware_backed: true,
        }]
    );
}

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
    state.active_playback = Some(playback);

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
    assert!(error.contains("truncated") || error.contains("Failed to scan"));
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

#[test]
fn fill_output_queues_respects_per_tick_batch_budget() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let playback = ActivePlayback::new(
        "play-budget".to_string(),
        "sound-budget".to_string(),
        audio_path.to_string_lossy().to_string(),
        0,
        1.0,
        None,
        None,
        &runtime,
    )
    .expect("create active playback");

    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    state.active_playback = Some(playback);

    fill_output_queues(&mut state);

    let queues = state.queues.lock();
    let max_samples_per_tick = state.runtime.max_fill_batches_per_tick(true, true)
        * MIX_CHUNK_FRAMES
        * TARGET_OUTPUT_CHANNELS as usize;
    assert!(queues.local.len() <= max_samples_per_tick);
    assert!(queues.virtual_out.len() <= max_samples_per_tick);
    drop(queues);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn fill_output_queues_mic_passthrough_without_capture_stream_keeps_queues_idle() {
    let mut runtime = test_runtime_config();
    runtime.mic_passthrough = true;

    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    fill_output_queues(&mut state);
    fill_output_queues(&mut state);

    let queues = state.queues.lock();
    assert_eq!(queues.local.len(), 0);
    assert_eq!(queues.virtual_out.len(), 0);
}

#[test]
fn passthrough_chunk_skips_when_mic_in_below_threshold() {
    // When mic_in has fewer samples than chunk_samples, no output should be
    // pushed. Padding with zeros would cause a silence discontinuity through
    // the consumer's resampler ~40 ms later.
    let mut queues = ProcessQueues::new(8, 8, 8);
    queues.mic_in.push_slice(&[0.25, -0.5]); // only 2 samples

    let pushed = enqueue_passthrough_chunk(&mut queues, 6); // needs 6

    assert_eq!(pushed, 0);
    assert_eq!(queues.virtual_out.len(), 0);
}

#[test]
fn passthrough_chunk_pushes_when_mic_in_has_full_chunk() {
    let mut queues = ProcessQueues::new(64, 64, 64);
    queues.mic_in.push_slice(&[0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);

    let pushed = enqueue_passthrough_chunk(&mut queues, 6);

    assert_eq!(pushed, 6);
    let mut output = vec![0.0; 6];
    let dequeued = queues.virtual_out.pop_into(&mut output);
    assert_eq!(dequeued, 6);
    assert_eq!(output, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
}

#[test]
fn runtime_latency_profile_low_reduces_virtual_target() {
    let mut runtime = test_runtime_config();
    runtime.mic_latency_profile = MicLatencyProfile::Low;

    assert!(runtime.virtual_output_target_samples() < runtime.local_output_target_samples());
    assert!(runtime.max_virtual_callback_samples() < MAX_LOCAL_OUTPUT_CALLBACK_SAMPLES);
}

#[test]
fn runtime_latency_profile_ultra_is_smallest_virtual_target() {
    let mut low = test_runtime_config();
    low.mic_latency_profile = MicLatencyProfile::Low;
    let mut ultra = test_runtime_config();
    ultra.mic_latency_profile = MicLatencyProfile::Ultra;

    assert!(ultra.virtual_output_target_samples() < low.virtual_output_target_samples());
    assert!(ultra.max_virtual_callback_samples() < low.max_virtual_callback_samples());
}

#[test]
fn clear_virtual_mic_queues_resets_mic_path_only() {
    let state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    {
        let mut queues = state.queues.lock();
        queues.local.push_slice(&[0.1, 0.2]);
        queues.virtual_out.push_slice(&[0.3, 0.4, 0.5]);
        queues.mic_in.push_slice(&[0.6, 0.7, 0.8, 0.9]);
    }

    clear_virtual_mic_queues(&state.queues);

    let queues = state.queues.lock();
    assert_eq!(queues.local.len(), 2);
    assert_eq!(queues.virtual_out.len(), 0);
    assert_eq!(queues.mic_in.len(), 0);
}

#[test]
fn clear_all_queues_resets_local_virtual_and_mic_buffers() {
    let state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    {
        let mut queues = state.queues.lock();
        queues.local.push_slice(&[0.1, 0.2]);
        queues.virtual_out.push_slice(&[0.3, 0.4, 0.5]);
        queues.mic_in.push_slice(&[0.6, 0.7, 0.8, 0.9]);
    }

    clear_all_queues(&state.queues);

    let queues = state.queues.lock();
    assert_eq!(queues.local.len(), 0);
    assert_eq!(queues.virtual_out.len(), 0);
    assert_eq!(queues.mic_in.len(), 0);
}

#[test]
fn recreate_capture_stream_clears_mic_input_without_dropping_soundboard_output() {
    let mut runtime = test_runtime_config();
    runtime.mic_passthrough = true;
    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    {
        let mut queues = state.queues.lock();
        queues.local.push_slice(&[0.1, 0.2]);
        queues.virtual_out.push_slice(&[0.3, 0.4, 0.5]);
        queues.mic_in.push_slice(&[0.6, 0.7, 0.8, 0.9]);
    }

    let result = recreate_capture_stream(&mut state);
    assert!(result.is_ok());

    let queues = state.queues.lock();
    assert_eq!(queues.local.len(), 2);
    assert_eq!(queues.virtual_out.len(), 3);
    assert_eq!(queues.mic_in.len(), 0);
}

#[test]
fn publish_snapshot_includes_visible_sources_and_active_playback() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let snapshot = test_player_snapshot_store();
    let mut state = LoopState::new(runtime.clone(), snapshot.clone());
    state.available = true;
    state.sources.insert(
        1,
        SourceDescriptor {
            id: 1,
            serial: None,
            node_name: "alsa_input.real".to_string(),
            display_name: "Real Mic".to_string(),
            priority_session: 0,
            is_monitor: false,
            is_our_virtual_mic: false,
            is_virtual: false,
            is_hardware_backed: true,
        },
    );
    state.active_playback = Some(
        ActivePlayback::new(
            "play-1".to_string(),
            "sound-1".to_string(),
            audio_path.to_string_lossy().to_string(),
            0,
            1.0,
            None,
            None,
            &runtime,
        )
        .expect("create active playback"),
    );
    state.publish_snapshot();

    let snapshot = snapshot.read().clone();
    assert!(snapshot.available);
    assert_eq!(snapshot.playing_ids, vec!["sound-1".to_string()]);
    assert_eq!(snapshot.audio_sources.len(), 1);
    assert_eq!(snapshot.audio_sources[0].node_name, "alsa_input.real");

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn dynamic_lookahead_mode_warmup_does_not_output_initial_silence() {
    let audio_path = create_test_audio_file("wav");
    let mut runtime = test_runtime_config();
    runtime.auto_gain.enabled = true;
    runtime.auto_gain.mode = AutoGainMode::DynamicLookAhead;
    runtime.auto_gain.apply_to = AutoGainApplyTo::Both;

    let mut playback = ActivePlayback::new(
        "play-warmup".to_string(),
        "sound-warmup".to_string(),
        audio_path.to_string_lossy().to_string(),
        0,
        1.0,
        Some(-14.0),
        None,
        &runtime,
    )
    .expect("create active playback");

    let mut local = vec![0.0; 512];
    let mut virtual_out = vec![0.0; 512];
    playback.render_into(&mut local, &mut virtual_out, &runtime);

    assert!(local.iter().any(|sample| sample.abs() > f32::EPSILON));
    assert!(virtual_out.iter().any(|sample| sample.abs() > f32::EPSILON));

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn dynamic_apply_to_switch_rebuilds_live_limiter_scope() {
    let audio_path = create_test_audio_file("wav");
    let mut runtime = test_runtime_config();
    runtime.auto_gain.enabled = true;
    runtime.auto_gain.mode = AutoGainMode::DynamicLookAhead;
    runtime.auto_gain.apply_to = AutoGainApplyTo::Both;

    let mut playback = ActivePlayback::new(
        "play-scope".to_string(),
        "sound-scope".to_string(),
        audio_path.to_string_lossy().to_string(),
        0,
        1.0,
        Some(-14.0),
        None,
        &runtime,
    )
    .expect("create active playback");

    assert!(playback.local_limiter.is_some());
    assert!(playback.virtual_limiter.is_some());

    runtime.auto_gain.apply_to = AutoGainApplyTo::MicOnly;
    let mut local = vec![0.0; 128];
    let mut virtual_out = vec![0.0; 128];
    playback.render_into(&mut local, &mut virtual_out, &runtime);

    assert!(playback.local_limiter.is_none());
    assert!(playback.virtual_limiter.is_some());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn loop_state_trim_finished_playbacks_discards_oldest_entries() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.finished_playbacks.insert(
        "play-1".to_string(),
        PlaybackSnapshot {
            sound_id: "sound-1".to_string(),
            playback_order: 1,
            position_ms: 100,
            paused: false,
            duration_ms: Some(1_000),
            finished: true,
        },
    );
    state.finished_playbacks.insert(
        "play-2".to_string(),
        PlaybackSnapshot {
            sound_id: "sound-2".to_string(),
            playback_order: 2,
            position_ms: 200,
            paused: false,
            duration_ms: Some(1_000),
            finished: true,
        },
    );
    state.finished_playbacks.insert(
        "play-3".to_string(),
        PlaybackSnapshot {
            sound_id: "sound-3".to_string(),
            playback_order: 3,
            position_ms: 300,
            paused: false,
            duration_ms: Some(1_000),
            finished: true,
        },
    );

    state.trim_finished_playbacks(2);

    assert_eq!(state.finished_playbacks.len(), 2);
    assert!(!state.finished_playbacks.contains_key("play-1"));
    assert!(state.finished_playbacks.contains_key("play-2"));
    assert!(state.finished_playbacks.contains_key("play-3"));
}
