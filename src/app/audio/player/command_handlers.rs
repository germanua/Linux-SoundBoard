use super::*;

pub(super) fn audio_command_kind(cmd: &AudioCommand) -> &'static str {
    match cmd {
        AudioCommand::Play { .. } => "Play",
        AudioCommand::StopSound { .. } => "StopSound",
        AudioCommand::StopPlayback { .. } => "StopPlayback",
        AudioCommand::StopAll => "StopAll",
        AudioCommand::Seek { .. } => "Seek",
        AudioCommand::Pause { .. } => "Pause",
        AudioCommand::Resume { .. } => "Resume",
        AudioCommand::SetPlaybackPaused { .. } => "SetPlaybackPaused",
        AudioCommand::SetLocalVolume { .. } => "SetLocalVolume",
        AudioCommand::SetMicVolume { .. } => "SetMicVolume",
        AudioCommand::SetAutoGainEnabled { .. } => "SetAutoGainEnabled",
        AudioCommand::SetAutoGainTarget { .. } => "SetAutoGainTarget",
        AudioCommand::SetAutoGainMode { .. } => "SetAutoGainMode",
        AudioCommand::SetAutoGainApplyTo { .. } => "SetAutoGainApplyTo",
        AudioCommand::SetAutoGainDynamicSettings { .. } => "SetAutoGainDynamicSettings",
        AudioCommand::SetLoudnessBoostEnabled { .. } => "SetLoudnessBoostEnabled",
        AudioCommand::SetLoudnessBoostDb { .. } => "SetLoudnessBoostDb",
        AudioCommand::SetLooping { .. } => "SetLooping",
        AudioCommand::SetAllowMultiplePlaybacks { .. } => "SetAllowMultiplePlaybacks",
        AudioCommand::SetMicPassthrough { .. } => "SetMicPassthrough",
        AudioCommand::SetMicSource { .. } => "SetMicSource",
        AudioCommand::SetDefaultSourceMode { .. } => "SetDefaultSourceMode",
        AudioCommand::SetMicLatencyProfile { .. } => "SetMicLatencyProfile",
        AudioCommand::Shutdown { .. } => "Shutdown",
    }
}

pub(super) fn handle_audio_command(
    _mainloop: &pw::main_loop::MainLoopRc,
    state_rc: &Rc<RefCell<LoopState>>,
    cmd: AudioCommand,
) -> bool {
    let mut state = state_rc.borrow_mut();
    match cmd {
        AudioCommand::Play {
            sound_id,
            path,
            base_volume,
            sound_lufs,
            sound_true_peak_dbtp,
            response,
        } => {
            let play_started_at = Instant::now();
            if !state.backend_playback_available() {
                let _ = response.send(Err(EngineError::Playback(
                    "PipeWire backend unavailable".to_string(),
                )));
            } else {
                let play_id = uuid::Uuid::new_v4().to_string();
                let init_started_at = Instant::now();
                let candidate = ActivePlayback::new(
                    play_id.clone(),
                    sound_id,
                    path,
                    state.next_playback_order,
                    base_volume,
                    sound_lufs,
                    sound_true_peak_dbtp,
                    &state.runtime,
                );
                let init_elapsed_ms = init_started_at.elapsed().as_millis();

                match state.apply_play_candidate(candidate) {
                    Ok(_) => {
                        if init_elapsed_ms >= 100 {
                            debug!(
                                "ActivePlayback initialization was slow: elapsed_ms={} play_id={}",
                                init_elapsed_ms, play_id
                            );
                        }
                        state.next_playback_order = state.next_playback_order.saturating_add(1);
                        let _ = response.send(Ok(play_id));
                    }
                    Err(err) => {
                        debug!(
                            "ActivePlayback initialization failed: elapsed_ms={} error={}",
                            init_elapsed_ms, err
                        );
                        let _ = response.send(Err(err));
                    }
                }
            }
            let play_elapsed_ms = play_started_at.elapsed().as_millis();
            if play_elapsed_ms >= 100 {
                debug!("Play command completed: elapsed_ms={}", play_elapsed_ms);
            }
        }
        AudioCommand::StopSound { sound_id } => {
            state.stop_sound_voices(&sound_id);
        }
        AudioCommand::StopPlayback { play_id } => {
            state.stop_voice(&play_id);
        }
        AudioCommand::StopAll => {
            state.stop_all_voices();
        }
        AudioCommand::Seek {
            play_id,
            position_ms,
        } => {
            state.seek_voice(&play_id, position_ms);
        }
        AudioCommand::Pause { sound_id } => {
            state.set_sound_paused(&sound_id, true);
        }
        AudioCommand::Resume { sound_id } => {
            state.set_sound_paused(&sound_id, false);
        }
        AudioCommand::SetPlaybackPaused { play_id, paused } => {
            state.set_voice_paused(&play_id, paused);
        }
        AudioCommand::SetLocalVolume { volume } => state.runtime.local_volume = volume,
        AudioCommand::SetMicVolume { volume } => state.runtime.mic_volume = volume,
        AudioCommand::SetAutoGainEnabled { enabled } => {
            state.runtime.auto_gain.enabled = enabled;
            state.reset_voice_limiters();
        }
        AudioCommand::SetAutoGainTarget { target_lufs } => {
            state.runtime.auto_gain.target_lufs = target_lufs;
        }
        AudioCommand::SetAutoGainMode { mode } => {
            state.runtime.auto_gain.mode = AutoGainMode::from_u32(mode);
            state.reset_voice_limiters();
        }
        AudioCommand::SetAutoGainApplyTo { apply_to } => {
            state.runtime.auto_gain.apply_to = AutoGainApplyTo::from_u32(apply_to);
            state.reset_voice_limiters();
        }
        AudioCommand::SetAutoGainDynamicSettings {
            lookahead_ms,
            attack_ms,
            release_ms,
        } => {
            state.runtime.auto_gain.dynamic = AutoGainDynamicParams {
                lookahead_ms,
                attack_ms,
                release_ms,
            };
            state.reset_voice_limiters();
        }
        AudioCommand::SetLoudnessBoostEnabled { enabled } => {
            state.runtime.loudness_boost_enabled = enabled;
        }
        AudioCommand::SetLoudnessBoostDb { boost_db } => {
            state.runtime.loudness_boost_db = crate::config::normalize_loudness_boost_db(boost_db);
        }
        AudioCommand::SetLooping { enabled } => state.runtime.looping = enabled,
        AudioCommand::SetAllowMultiplePlaybacks { enabled } => {
            state.set_allow_multiple_playbacks(enabled);
        }
        AudioCommand::SetMicPassthrough { enabled, response } => {
            state.runtime.mic_passthrough = enabled;
            state.stream_runtime.apply_runtime(&state.runtime);
            let result = recreate_capture_stream(&mut state);
            let _ = response.send(result);
        }
        AudioCommand::SetMicSource { source, response } => {
            state.runtime.mic_source = source;
            let result = recreate_capture_stream(&mut state);
            let _ = response.send(result);
        }
        AudioCommand::SetDefaultSourceMode { mode, response } => {
            state.runtime.default_source_mode = mode;
            let result = apply_default_source_mode(&mut state);
            let _ = response.send(result);
        }
        AudioCommand::SetMicLatencyProfile { profile, response } => {
            state.runtime.mic_latency_profile = profile;
            state.ultra_starvation_ticks = 0;
            state.stream_runtime.apply_runtime(&state.runtime);
            let result = recreate_capture_stream(&mut state);
            let _ = response.send(result);
        }
        AudioCommand::Shutdown { policy, response } => {
            state.active_playbacks.clear();
            clear_all_queues(&state.queues);
            drop_feeder_links(&mut state);
            if let Some(backend) = state.backend.as_mut() {
                backend.stop_streams_for_shutdown();
            }
            state.active_capture_target = None;
            if policy.restores_default_source() {
                super::source_routing::restore_default_source_for_transient_shutdown(&mut state);
            }
            let _ = response.send(());
            return true;
        }
    }

    state.publish_snapshot();
    false
}
