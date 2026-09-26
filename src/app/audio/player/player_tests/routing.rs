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
    append_test_ogg_page, append_truncated_test_ogg_page, cleanup_test_audio_path,
    corrupt_test_ogg_page_body, create_test_audio_file, create_test_encoded_file,
    create_test_ogg_opus_file, create_test_vorbis_file, encode_test_opus_audio_packet,
    test_ogg_has_end_of_stream_page, TestEncodedFixture, TestOggOpusFinalPage, TestOggOpusFixture,
    TestVorbisFixture, INVALID_TEST_OPUS_PACKET, TEST_OGG_OPUS_SERIAL,
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
        is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
        },
    );

    assert_eq!(
        source_routing::transient_restore_target(&state),
        Some((1, "alsa_input.previous".to_string()))
    );
}

#[test]
fn transient_shutdown_restores_previous_bluetooth_loopback_default() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    let source = bluetooth_loopback_source(77, 2010);
    state.previous_default_source_name = Some(source.node_name.clone());
    state.sources.insert(source.id, source);

    assert_eq!(
        source_routing::transient_restore_target(&state),
        Some((77, "bluez_input.88_0E_85_3E_F1_E2".to_string()))
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_virtual: true,
            is_hardware_backed: false,
            is_bluetooth_loopback: false,
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

    assert_eq!(
        resolve_capture_target_from_default(&state, Some("alsa_input.low".to_string())).as_deref(),
        Some("easyeffects_source")
    );

    state.previous_default_source_name = Some("alsa_input.low".to_string());
    assert_eq!(
        resolve_capture_target_from_default(&state, Some(VIRTUAL_SOURCE_NAME.to_string()))
            .as_deref(),
        Some("easyeffects_source")
    );

    assert_eq!(
        best_upstream_mic_source_name(&state.sources).as_deref(),
        Some("easyeffects_source")
    );

    state.previous_default_source_name = None;
    assert_eq!(
        resolve_capture_target_from_default(&state, Some(VIRTUAL_SOURCE_NAME.to_string()))
            .as_deref(),
        Some("easyeffects_source")
    );
}

#[test]
fn auto_capture_falls_back_to_physical_mic_when_no_enhancement_source() {
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

    assert_eq!(
        resolve_capture_target_from_default(&state, Some("alsa_input.low".to_string())).as_deref(),
        Some("alsa_input.high")
    );
}

fn bluetooth_loopback_source(id: u32, priority: i32) -> SourceDescriptor {
    let mut source = test_source(
        id,
        "bluez_input.88_0E_85_3E_F1_E2",
        "soundcore Q30",
        priority,
    );
    source.is_hardware_backed = true;
    source.is_bluetooth_loopback = true;
    source
}

#[test]
fn auto_capture_does_not_choose_wireplumber_bluetooth_loopback_over_real_mic() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.sources.insert(1, bluetooth_loopback_source(1, 2010));
    state.sources.insert(
        2,
        test_source(2, "alsa_input.internal", "Internal Mic", 100),
    );

    assert_eq!(
        best_upstream_mic_source_name(&state.sources).as_deref(),
        Some("alsa_input.internal")
    );
}

#[test]
fn auto_capture_returns_none_when_only_bluetooth_autoswitch_proxy_exists() {
    let mut state = LoopState::new(test_runtime_config(), test_player_snapshot_store());
    state.sources.insert(1, bluetooth_loopback_source(1, 9999));

    assert_eq!(best_upstream_mic_source_name(&state.sources), None);
    assert_eq!(resolve_capture_target_from_default(&state, None), None);
}

#[test]
fn explicit_bluetooth_loopback_selection_remains_allowed() {
    let mut runtime = test_runtime_config();
    runtime.mic_source = Some("bluez_input.88_0E_85_3E_F1_E2".to_string());
    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    state.sources.insert(1, bluetooth_loopback_source(1, 2010));

    assert_eq!(
        resolve_capture_target_from_default(&state, None).as_deref(),
        Some("bluez_input.88_0E_85_3E_F1_E2")
    );
}

#[test]
fn auto_capture_ignores_unrecognized_virtual_source_in_favor_of_hardware_mic() {
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
            is_bluetooth_loopback: false,
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
