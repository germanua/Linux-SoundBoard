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
    state.adopt_voice(playback);

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
    let mut queues = ProcessQueues::new(8, 8, 8);
    queues.mic_in.push_slice(&[0.25, -0.5]);

    let pushed = enqueue_passthrough_chunk(&mut queues, 6);

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
            is_bluetooth_loopback: false,
        },
    );
    state.adopt_voice(
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
fn loudness_boost_is_mic_only_and_does_not_enable_peak_limiting() {
    let audio_path = create_test_audio_file("wav");
    let mut runtime = test_runtime_config();
    runtime.auto_gain.enabled = false;
    runtime.loudness_boost_enabled = true;
    runtime.loudness_boost_db = 6.0;

    assert_eq!(runtime.loudness_boost_gain(false), 1.0);
    let expected = 10.0_f32.powf(6.0 / 20.0);
    assert!((runtime.loudness_boost_gain(true) - expected).abs() < 0.001);

    let mut playback = ActivePlayback::new(
        "play-boost".to_string(),
        "sound-boost".to_string(),
        audio_path.to_string_lossy().to_string(),
        0,
        1.0,
        Some(-14.0),
        None,
        &runtime,
    )
    .expect("create boosted playback");

    assert!(playback.local_limiter.is_none());
    assert!(playback.virtual_limiter.is_none());

    let mut local = vec![0.0; 1_024];
    let mut virtual_out = vec![0.0; 1_024];
    playback.render_into(&mut local, &mut virtual_out, &runtime);
    let local_peak = local.iter().copied().map(f32::abs).fold(0.0, f32::max);
    let virtual_peak = virtual_out
        .iter()
        .copied()
        .map(f32::abs)
        .fold(0.0, f32::max);
    assert!((virtual_peak / local_peak - expected).abs() < 0.01);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn loudness_boost_runs_after_dynamic_lufs_limiting_and_hard_clips() {
    let audio_path = create_test_audio_file("wav");
    let mut runtime = test_runtime_config();
    runtime.auto_gain.enabled = true;
    runtime.auto_gain.mode = AutoGainMode::DynamicLookAhead;
    runtime.auto_gain.apply_to = AutoGainApplyTo::Both;
    runtime.auto_gain.target_lufs = -14.0;
    runtime.auto_gain.dynamic = AutoGainDynamicParams {
        lookahead_ms: 5,
        attack_ms: 1,
        release_ms: 50,
    };
    runtime.loudness_boost_enabled = true;
    runtime.loudness_boost_db = 20.0;

    let mut playback = ActivePlayback::new(
        "play-combined-boost".to_string(),
        "sound-combined-boost".to_string(),
        audio_path.to_string_lossy().to_string(),
        0,
        1.0,
        Some(-20.0),
        None,
        &runtime,
    )
    .expect("create boosted playback");
    let mut local = vec![0.0; 8_192];
    let mut virtual_out = vec![0.0; 8_192];

    playback.render_into(&mut local, &mut virtual_out, &runtime);

    assert!(playback.local_limiter.is_some());
    assert!(playback.virtual_limiter.is_some());

    assert!(
        virtual_out.iter().any(|sample| sample.abs() > 1.0),
        "the boosted voice is expected to exceed full scale before the bus clamp"
    );
    for samples in [&mut local, &mut virtual_out] {
        for sample in samples.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
    }
    assert!(local.iter().all(|sample| sample.abs() <= 1.0));
    assert!(virtual_out.iter().all(|sample| sample.abs() <= 1.0));
    assert_eq!(
        virtual_out[6_144..]
            .iter()
            .copied()
            .map(f32::abs)
            .fold(0.0, f32::max),
        1.0
    );

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

fn adopt_voice(state: &mut LoopState, playback: ActivePlayback) {
    state.adopt_voice(playback);
}

fn make_voice(
    state: &mut LoopState,
    runtime: &RuntimeConfig,
    play_id: &str,
    sound_id: &str,
    path: &str,
) -> ActivePlayback {
    let playback = ActivePlayback::new(
        play_id.to_string(),
        sound_id.to_string(),
        path.to_string(),
        state.next_playback_order,
        1.0,
        None,
        None,
        runtime,
    )
    .expect("create voice");
    state.next_playback_order = state.next_playback_order.saturating_add(1);
    playback
}

fn live_play_ids(state: &LoopState, sound_id: &str) -> Vec<String> {
    let mut ids: Vec<String> = state
        .snapshot_positions()
        .into_iter()
        .filter(|position| !position.finished && position.sound_id == sound_id)
        .map(|position| position.play_id)
        .collect();
    ids.sort();
    ids
}

#[test]
fn overlapping_plays_keep_both_voices_live() {
    let first_path = create_test_audio_file("wav");
    let second_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());

    let first = make_voice(
        &mut state,
        &runtime,
        "play-a",
        "sound-a",
        &first_path.to_string_lossy(),
    );
    adopt_voice(&mut state, first);
    let second = make_voice(
        &mut state,
        &runtime,
        "play-b",
        "sound-b",
        &second_path.to_string_lossy(),
    );
    adopt_voice(&mut state, second);

    assert_eq!(
        live_play_ids(&state, "sound-a"),
        vec!["play-a".to_string()],
        "playing a second sound must not stop the first voice"
    );
    assert_eq!(
        live_play_ids(&state, "sound-b"),
        vec!["play-b".to_string()],
        "the second voice must be live too"
    );
    let mut playing = state.playing_ids();
    playing.sort();
    assert_eq!(playing, vec!["sound-a".to_string(), "sound-b".to_string()]);

    cleanup_test_audio_path(&first_path);
    cleanup_test_audio_path(&second_path);
}

#[test]
fn repeated_plays_of_one_sound_keep_every_voice_live() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());

    for play_id in ["play-a", "play-b"] {
        let voice = make_voice(
            &mut state,
            &runtime,
            play_id,
            "sound-a",
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    assert_eq!(
        live_play_ids(&state, "sound-a"),
        vec!["play-a".to_string(), "play-b".to_string()],
        "spamming one sound must overlap, not replace"
    );
    assert_eq!(
        state.playing_ids(),
        vec!["sound-a".to_string()],
        "playing_ids stays unique per sound"
    );

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn spamming_one_sound_overlaps_into_six_voices() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());

    for index in 0..6 {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            "sound-a",
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    assert_eq!(
        live_play_ids(&state, "sound-a").len(),
        6,
        "six triggers of one sound must leave six independent voices"
    );
    assert_eq!(
        state.playing_ids(),
        vec!["sound-a".to_string()],
        "playing_ids lists a sound once however many voices it has"
    );

    state.stop_sound_voices("sound-a");
    assert!(
        state.active_playbacks.is_empty(),
        "StopSound removes them all"
    );
    assert!(state.playing_ids().is_empty());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn stopping_one_sound_leaves_the_other_sounds_playing() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());

    for (index, sound) in ["sound-a", "sound-b", "sound-c"].into_iter().enumerate() {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            sound,
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    state.stop_sound_voices("sound-b");

    assert_eq!(live_play_ids(&state, "sound-a"), vec!["play-0".to_string()]);
    assert!(live_play_ids(&state, "sound-b").is_empty());
    assert_eq!(live_play_ids(&state, "sound-c"), vec!["play-2".to_string()]);
    let mut playing = state.playing_ids();
    playing.sort();
    assert_eq!(playing, vec!["sound-a".to_string(), "sound-c".to_string()]);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn voice_controls_leave_the_shared_queues_alone() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for index in 0..2 {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            &format!("sound-{index}"),
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }
    state.queues.lock().local.push_slice(&[0.5f32; 8]);
    state.queues.lock().virtual_out.push_slice(&[0.5f32; 8]);

    state.stop_sound_voices("sound-0");
    state.seek_voice("play-1", 20);
    state.set_sound_paused("sound-1", true);
    state.set_sound_paused("sound-1", false);
    state.set_voice_paused("play-1", true);
    state.stop_voice("play-1");

    let queues = state.queues.lock();
    assert_eq!(
        queues.local.len(),
        8,
        "StopSound/Seek/Pause must not fade or clear the shared queues"
    );
    assert_eq!(queues.virtual_out.len(), 8);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn seeking_targets_exactly_one_voice() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for index in 0..2 {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            "sound-a",
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    assert!(state.seek_voice("play-1", 30));
    assert!(
        !state.seek_voice("play-missing", 30),
        "unknown id is a no-op"
    );

    let seeked = state
        .active_playbacks
        .iter()
        .find(|voice| voice.play_id == "play-1")
        .expect("voice");
    let untouched = state
        .active_playbacks
        .iter()
        .find(|voice| voice.play_id == "play-0")
        .expect("voice");
    assert_eq!(seeked.position_ms, 30);
    assert_eq!(untouched.position_ms, 0, "the other voice must not move");

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn pause_and_stop_target_one_voice_when_sound_overlaps() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for index in 0..2 {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            "sound-a",
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    state.set_voice_paused("play-1", true);
    assert!(state
        .active_playbacks
        .iter()
        .any(|voice| voice.play_id == "play-1" && voice.paused));
    assert!(state
        .active_playbacks
        .iter()
        .any(|voice| voice.play_id == "play-0" && !voice.paused));

    state.stop_voice("play-1");
    assert_eq!(live_play_ids(&state, "sound-a"), vec!["play-0".to_string()]);
    cleanup_test_audio_path(&audio_path);
}

#[test]
fn pause_and_resume_cover_every_voice_of_one_sound() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for (play_id, sound) in [
        ("play-0", "sound-a"),
        ("play-1", "sound-a"),
        ("play-2", "sound-b"),
    ] {
        let voice = make_voice(
            &mut state,
            &runtime,
            play_id,
            sound,
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    state.set_sound_paused("sound-a", true);
    let paused: Vec<bool> = state
        .active_playbacks
        .iter()
        .map(|voice| voice.paused)
        .collect();
    assert_eq!(paused, vec![true, true, false]);

    state.set_sound_paused("sound-a", false);
    assert!(state.active_playbacks.iter().all(|voice| !voice.paused));

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn pause_and_resume_are_smoothed_to_silence() {
    let audio_path =
        crate::test_support::audio_fixtures::create_test_audio_file_with_duration("wav", 1000);
    let mut runtime = test_runtime_config();
    runtime.auto_gain.enabled = false;
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    let mut voice = make_voice(
        &mut state,
        &runtime,
        "play-0",
        "sound-a",
        &audio_path.to_string_lossy(),
    );
    let mut local = vec![0.0f32; 1024];
    let mut virtual_out = vec![0.0f32; 1024];

    voice.render_into(&mut local, &mut virtual_out, &runtime);
    voice.set_paused(true);
    local.fill(0.0);
    virtual_out.fill(0.0);
    voice.render_into(&mut local, &mut virtual_out, &runtime);

    assert!(voice.paused);
    assert_eq!(voice.pause_fade_out_remaining, 0);
    assert!(local[..TRANSITION_FADE_SAMPLES as usize]
        .iter()
        .any(|sample| sample.abs() > 1e-4));
    assert!(local[TRANSITION_FADE_SAMPLES as usize..]
        .iter()
        .all(|sample| sample.abs() < 1e-7));
    assert!(local[TRANSITION_FADE_SAMPLES as usize - 1].abs() < 1e-7);

    let paused_position = voice.position_ms;
    voice.render_into(&mut local, &mut virtual_out, &runtime);
    assert_eq!(voice.position_ms, paused_position);
    assert!(local.iter().all(|sample| sample.abs() < 1e-7));

    voice.set_paused(false);
    voice.render_into(&mut local, &mut virtual_out, &runtime);
    assert!(!voice.paused);
    assert!(local[0].abs() < 1e-7);
    assert!(local[TRANSITION_FADE_SAMPLES as usize..]
        .iter()
        .any(|sample| sample.abs() > 1e-4));

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn seek_crossfades_through_zero_before_the_new_position() {
    let audio_path =
        crate::test_support::audio_fixtures::create_test_audio_file_with_duration("wav", 1000);
    let mut runtime = test_runtime_config();
    runtime.auto_gain.enabled = true;
    runtime.auto_gain.mode = AutoGainMode::DynamicLookAhead;
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    let mut voice = make_voice(
        &mut state,
        &runtime,
        "play-0",
        "sound-a",
        &audio_path.to_string_lossy(),
    );
    let mut local = vec![0.0f32; 2048];
    let mut virtual_out = vec![0.0f32; 2048];

    voice.render_into(&mut local, &mut virtual_out, &runtime);
    voice.request_seek(500, &runtime);
    local.fill(0.0);
    virtual_out.fill(0.0);
    voice.render_into(&mut local, &mut virtual_out, &runtime);

    let boundary = TRANSITION_FADE_SAMPLES as usize;
    assert!(voice.pending_seek_ms.is_none());
    assert_eq!(voice.seek_fade_out_remaining, 0);
    assert!(local[boundary - 1].abs() < 1e-7);
    assert!(local[boundary].abs() < 1e-7);
    assert!(local[..boundary - 1]
        .iter()
        .any(|sample| sample.abs() > 1e-4));
    assert!(local[boundary + TRANSITION_FADE_SAMPLES as usize..]
        .iter()
        .any(|sample| sample.abs() > 1e-4));
    assert!(voice.position_ms >= 500);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn the_voice_cap_retires_the_oldest_voice() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for index in 0..MAX_ACTIVE_PLAYBACKS {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            &format!("sound-{index}"),
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }
    assert_eq!(state.active_playbacks.len(), MAX_ACTIVE_PLAYBACKS);

    let extra = make_voice(
        &mut state,
        &runtime,
        "play-extra",
        "sound-extra",
        &audio_path.to_string_lossy(),
    );
    adopt_voice(&mut state, extra);

    assert_eq!(state.active_playbacks.len(), MAX_ACTIVE_PLAYBACKS);
    let ids: Vec<&str> = state
        .active_playbacks
        .iter()
        .map(|voice| voice.play_id.as_str())
        .collect();
    assert!(
        !ids.contains(&"play-0"),
        "the OLDEST voice is the one retired: {ids:?}"
    );
    assert!(ids.contains(&"play-extra"), "the new voice is live");
    for index in 1..MAX_ACTIVE_PLAYBACKS {
        assert!(
            ids.contains(&format!("play-{index}").as_str()),
            "play-{index} must survive"
        );
    }

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn the_voice_cap_is_shared_across_every_sound() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for index in 0..=MAX_ACTIVE_PLAYBACKS {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            "sound-a",
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    assert_eq!(state.active_playbacks.len(), MAX_ACTIVE_PLAYBACKS);
    assert_eq!(
        state.playing_ids(),
        vec!["sound-a".to_string()],
        "one sound at the cap still lists once"
    );
    assert_eq!(live_play_ids(&state, "sound-a").len(), MAX_ACTIVE_PLAYBACKS);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn a_broken_file_cannot_evict_a_voice_at_the_cap() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for index in 0..MAX_ACTIVE_PLAYBACKS {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            &format!("sound-{index}"),
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }
    let before: Vec<String> = state
        .active_playbacks
        .iter()
        .map(|voice| voice.play_id.clone())
        .collect();

    let candidate = ActivePlayback::new(
        "play-broken".to_string(),
        "sound-broken".to_string(),
        "/nonexistent/wp5-broken.wav".to_string(),
        MAX_ACTIVE_PLAYBACKS as u64,
        1.0,
        None,
        None,
        &runtime,
    );
    assert!(
        candidate.is_err(),
        "a missing file must not construct a voice"
    );

    let after: Vec<String> = state
        .active_playbacks
        .iter()
        .map(|voice| voice.play_id.clone())
        .collect();
    assert_eq!(before, after, "every existing voice must be untouched");

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn several_voices_finishing_in_one_tick_are_all_recorded() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for index in 0..4 {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            &format!("sound-{index}"),
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    state.active_playbacks[0].finished = true;
    state.active_playbacks[0].position_ms = 10;
    state.active_playbacks[2].finished = true;
    state.active_playbacks[2].position_ms = 30;

    assert_eq!(state.retire_finished_voices(), 2);

    let remaining: Vec<&str> = state
        .active_playbacks
        .iter()
        .map(|voice| voice.play_id.as_str())
        .collect();
    assert_eq!(remaining, vec!["play-1", "play-3"]);
    assert_eq!(state.finished_playbacks.len(), 2);
    assert_eq!(
        state.finished_playbacks["play-0"].position_ms, 10,
        "each retired voice keeps its own snapshot"
    );
    assert_eq!(state.finished_playbacks["play-2"].position_ms, 30);
    assert!(state.finished_playbacks["play-0"].finished);
    assert!(!state.playing_ids().contains(&"sound-0".to_string()));
    assert_eq!(
        state.playing_ids(),
        vec!["sound-1".to_string(), "sound-3".to_string()]
    );

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn playback_positions_list_every_voice_newest_first() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    for index in 0..3 {
        let voice = make_voice(
            &mut state,
            &runtime,
            &format!("play-{index}"),
            "sound-a",
            &audio_path.to_string_lossy(),
        );
        adopt_voice(&mut state, voice);
    }

    let positions = state.snapshot_positions();
    let ids: Vec<&str> = positions
        .iter()
        .map(|position| position.play_id.as_str())
        .collect();
    assert_eq!(
        ids,
        vec!["play-2", "play-1", "play-0"],
        "every live voice, newest first"
    );
    assert!(positions.iter().all(|position| !position.finished));

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn sustained_voice_churn_stays_bounded() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    let mut next_play = 0u64;
    let mut seen_samples = 0usize;

    for round in 0..12u64 {
        for _ in 0..8 {
            let voice = make_voice(
                &mut state,
                &runtime,
                &format!("play-{next_play}"),
                &format!("sound-{}", next_play % 5),
                &audio_path.to_string_lossy(),
            );
            adopt_voice(&mut state, voice);
            next_play += 1;
            assert!(
                state.active_playbacks.len() <= MAX_ACTIVE_PLAYBACKS,
                "cap must hold after every play"
            );
        }

        fill_output_queues(&mut state);
        {
            let mut queues = state.queues.lock();
            let mut drained = vec![0.0f32; queues.local.len()];
            let taken = queues.local.pop_into(&mut drained);
            assert!(drained[..taken].iter().all(|sample| sample.is_finite()));
            seen_samples += taken;
            let mut drained_virtual = vec![0.0f32; queues.virtual_out.len()];
            let taken = queues.virtual_out.pop_into(&mut drained_virtual);
            assert!(drained_virtual[..taken]
                .iter()
                .all(|sample| sample.is_finite()));
        }

        state.set_sound_paused(&format!("sound-{}", round % 5), true);
        state.set_sound_paused(&format!("sound-{}", round % 5), false);
        let _ = state.seek_voice(&format!("play-{}", next_play - 1), 10);
        state.stop_sound_voices(&format!("sound-{}", (round + 2) % 5));
        assert!(state.active_playbacks.len() <= MAX_ACTIVE_PLAYBACKS);

        if let Some(voice) = state.active_playbacks.first_mut() {
            voice.finished = true;
        }
        state.retire_finished_voices();
        assert!(state.active_playbacks.len() <= MAX_ACTIVE_PLAYBACKS);
        assert!(state.finished_playbacks.len() <= MAX_FINISHED_PLAYBACK_SNAPSHOTS);

        let mut ids: Vec<&str> = state
            .active_playbacks
            .iter()
            .map(|voice| voice.play_id.as_str())
            .collect();
        let count = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), count, "no duplicate play_ids");
    }

    assert!(seen_samples > 0, "the stress run must actually mix audio");
    cleanup_test_audio_path(&audio_path);
}

fn set_concurrent(state: &mut LoopState, enabled: bool) {
    state.set_allow_multiple_playbacks(enabled);
}

fn apply_play(
    state: &mut LoopState,
    candidate: Result<ActivePlayback, EngineError>,
) -> Result<String, EngineError> {
    state.apply_play_candidate(candidate)
}

fn play_into(
    state: &mut LoopState,
    runtime: &RuntimeConfig,
    play_id: &str,
    sound_id: &str,
    path: &std::path::Path,
) -> String {
    let voice = make_voice(state, runtime, play_id, sound_id, &path.to_string_lossy());
    apply_play(state, Ok(voice)).expect("play")
}

#[test]
fn a_play_replaces_the_current_sound_when_concurrent_playback_is_off() {
    let first_path = create_test_audio_file("wav");
    let second_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    set_concurrent(&mut state, false);

    let first = play_into(&mut state, &runtime, "play-a", "sound-a", &first_path);
    let second = play_into(&mut state, &runtime, "play-b", "sound-b", &second_path);
    assert_ne!(first, second);

    assert!(
        live_play_ids(&state, "sound-a").is_empty(),
        "the replaced sound must not still be playing"
    );
    assert_eq!(live_play_ids(&state, "sound-b"), vec!["play-b".to_string()]);
    assert_eq!(state.playing_ids(), vec!["sound-b".to_string()]);
    assert_eq!(
        state.active_playbacks.len(),
        1,
        "single playback keeps at most one live voice"
    );

    cleanup_test_audio_path(&first_path);
    cleanup_test_audio_path(&second_path);
}

#[test]
fn a_repeated_play_of_one_sound_replaces_when_concurrent_playback_is_off() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    set_concurrent(&mut state, false);

    play_into(&mut state, &runtime, "play-a", "sound-a", &audio_path);
    play_into(&mut state, &runtime, "play-b", "sound-a", &audio_path);

    assert_eq!(
        live_play_ids(&state, "sound-a"),
        vec!["play-b".to_string()],
        "the second trigger replaces the first from zero"
    );
    assert_eq!(state.active_playbacks.len(), 1);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn consecutive_plays_overlap_when_concurrent_playback_is_on() {
    let first_path = create_test_audio_file("wav");
    let second_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    set_concurrent(&mut state, true);

    play_into(&mut state, &runtime, "play-a", "sound-a", &first_path);
    play_into(&mut state, &runtime, "play-a2", "sound-a", &first_path);
    play_into(&mut state, &runtime, "play-b", "sound-b", &second_path);

    assert_eq!(
        live_play_ids(&state, "sound-a"),
        vec!["play-a".to_string(), "play-a2".to_string()],
        "the same sound overlaps with itself"
    );
    assert_eq!(live_play_ids(&state, "sound-b"), vec!["play-b".to_string()]);
    let mut playing = state.playing_ids();
    playing.sort();
    assert_eq!(playing, vec!["sound-a".to_string(), "sound-b".to_string()]);

    cleanup_test_audio_path(&first_path);
    cleanup_test_audio_path(&second_path);
}

#[test]
fn a_failed_play_leaves_the_current_voice_alone_in_both_modes() {
    for concurrent in [false, true] {
        let audio_path = create_test_audio_file("wav");
        let runtime = test_runtime_config();
        let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
        set_concurrent(&mut state, concurrent);

        let active = play_into(&mut state, &runtime, "play-a", "sound-a", &audio_path);
        let broken = ActivePlayback::new(
            "play-broken".to_string(),
            "sound-broken".to_string(),
            "/nonexistent/wp5r2-broken.wav".to_string(),
            99,
            1.0,
            None,
            None,
            &runtime,
        );
        let error = apply_play(&mut state, broken).expect_err("a missing file must not play");
        assert!(
            !error.to_string().is_empty(),
            "the failure must be reported, not swallowed"
        );

        assert_eq!(
            live_play_ids(&state, "sound-a"),
            vec![active],
            "concurrent={concurrent}: a failed Play must not disturb what is playing"
        );
        assert_eq!(state.active_playbacks.len(), 1);

        cleanup_test_audio_path(&audio_path);
    }
}

#[test]
fn turning_concurrent_playback_off_keeps_the_newest_voice() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    set_concurrent(&mut state, true);
    for (play_id, sound) in [
        ("play-a", "sound-a"),
        ("play-b", "sound-b"),
        ("play-c", "sound-c"),
    ] {
        play_into(&mut state, &runtime, play_id, sound, &audio_path);
    }
    state.active_playbacks[2].position_ms = 25;

    set_concurrent(&mut state, false);

    assert_eq!(state.active_playbacks.len(), 1, "only one voice survives");
    assert_eq!(live_play_ids(&state, "sound-c"), vec!["play-c".to_string()]);
    assert_eq!(
        state.active_playbacks[0].position_ms, 25,
        "the survivor is kept, not restarted"
    );

    set_concurrent(&mut state, false);
    assert_eq!(state.active_playbacks.len(), 1);
    set_concurrent(&mut state, true);
    assert_eq!(live_play_ids(&state, "sound-c"), vec!["play-c".to_string()]);
    assert_eq!(state.active_playbacks[0].position_ms, 25);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn turning_concurrent_playback_off_keeps_the_newest_voice_of_one_sound() {
    let audio_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    set_concurrent(&mut state, true);
    for play_id in ["play-a1", "play-a2", "play-a3"] {
        play_into(&mut state, &runtime, play_id, "sound-a", &audio_path);
    }

    set_concurrent(&mut state, false);

    assert_eq!(
        live_play_ids(&state, "sound-a"),
        vec!["play-a3".to_string()],
        "the newest of one sound survives"
    );

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn toggling_concurrent_playback_with_nothing_playing_is_inert() {
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime, test_player_snapshot_store());
    state.queues.lock().local.push_slice(&[0.5f32; 8]);
    state.queues.lock().virtual_out.push_slice(&[0.5f32; 8]);

    set_concurrent(&mut state, true);
    set_concurrent(&mut state, false);
    set_concurrent(&mut state, true);

    assert!(state.active_playbacks.is_empty());
    assert!(state.finished_playbacks.is_empty());
    let queues = state.queues.lock();
    assert_eq!(
        queues.local.len(),
        8,
        "a toggle must never fade or clear the shared queues"
    );
    assert_eq!(queues.virtual_out.len(), 8);
}

#[test]
fn turning_concurrent_playback_on_does_not_disturb_what_is_playing() {
    let first_path = create_test_audio_file("wav");
    let second_path = create_test_audio_file("wav");
    let runtime = test_runtime_config();
    let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
    set_concurrent(&mut state, false);
    let active = play_into(&mut state, &runtime, "play-a", "sound-a", &first_path);
    state.active_playbacks[0].position_ms = 40;

    set_concurrent(&mut state, true);

    assert_eq!(live_play_ids(&state, "sound-a"), vec![active]);
    assert_eq!(state.active_playbacks[0].position_ms, 40, "not restarted");

    play_into(&mut state, &runtime, "play-b", "sound-b", &second_path);
    assert_eq!(live_play_ids(&state, "sound-a").len(), 1);
    assert_eq!(live_play_ids(&state, "sound-b").len(), 1);

    cleanup_test_audio_path(&first_path);
    cleanup_test_audio_path(&second_path);
}

#[test]
fn repeated_activation_of_one_resolved_sound_respects_the_mode() {
    for (concurrent, expected_voices) in [(false, 1usize), (true, 3)] {
        let audio_path = create_test_audio_file("wav");
        let runtime = test_runtime_config();
        let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
        set_concurrent(&mut state, concurrent);

        for index in 0..3 {
            play_into(
                &mut state,
                &runtime,
                &format!("play-{index}"),
                "sound-a",
                &audio_path,
            );
        }

        assert_eq!(
            state.active_playbacks.len(),
            expected_voices,
            "concurrent={concurrent}: three activations of one sound"
        );
        assert_eq!(
            state.playing_ids(),
            vec!["sound-a".to_string()],
            "concurrent={concurrent}: one sound however many voices"
        );
        if !concurrent {
            assert_eq!(
                live_play_ids(&state, "sound-a"),
                vec!["play-2".to_string()],
                "each press replaced the previous voice"
            );
        }

        cleanup_test_audio_path(&audio_path);
    }
}

#[test]
fn different_sounds_overlap_only_when_concurrent_playback_is_on() {
    for (concurrent, expected_voices) in [(false, 1usize), (true, 2)] {
        let first_path = create_test_audio_file("wav");
        let second_path = create_test_audio_file("wav");
        let runtime = test_runtime_config();
        let mut state = LoopState::new(runtime.clone(), test_player_snapshot_store());
        set_concurrent(&mut state, concurrent);

        play_into(&mut state, &runtime, "play-a", "sound-a", &first_path);
        play_into(&mut state, &runtime, "play-b", "sound-b", &second_path);

        assert_eq!(
            state.active_playbacks.len(),
            expected_voices,
            "concurrent={concurrent}"
        );
        let mut playing = state.playing_ids();
        playing.sort();
        let expected: Vec<String> = if concurrent {
            vec!["sound-a".to_string(), "sound-b".to_string()]
        } else {
            vec!["sound-b".to_string()]
        };
        assert_eq!(playing, expected, "concurrent={concurrent}");

        cleanup_test_audio_path(&first_path);
        cleanup_test_audio_path(&second_path);
    }
}
