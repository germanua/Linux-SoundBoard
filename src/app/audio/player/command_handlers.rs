use super::*;

pub(super) fn audio_command_kind(cmd: &AudioCommand) -> &'static str {
    match cmd {
        AudioCommand::Play { .. } => "Play",
        AudioCommand::StopSound { .. } => "StopSound",
        AudioCommand::StopAll => "StopAll",
        AudioCommand::Seek { .. } => "Seek",
        AudioCommand::Pause { .. } => "Pause",
        AudioCommand::Resume { .. } => "Resume",
        AudioCommand::SetLocalVolume { .. } => "SetLocalVolume",
        AudioCommand::SetMicVolume { .. } => "SetMicVolume",
        AudioCommand::SetAutoGainEnabled { .. } => "SetAutoGainEnabled",
        AudioCommand::SetAutoGainTarget { .. } => "SetAutoGainTarget",
        AudioCommand::SetAutoGainMode { .. } => "SetAutoGainMode",
        AudioCommand::SetAutoGainApplyTo { .. } => "SetAutoGainApplyTo",
        AudioCommand::SetAutoGainDynamicSettings { .. } => "SetAutoGainDynamicSettings",
        AudioCommand::SetLooping { .. } => "SetLooping",
        AudioCommand::SetMicPassthrough { .. } => "SetMicPassthrough",
        AudioCommand::SetMicSource { .. } => "SetMicSource",
        AudioCommand::SetDefaultSourceMode { .. } => "SetDefaultSourceMode",
        AudioCommand::SetMicLatencyProfile { .. } => "SetMicLatencyProfile",
        AudioCommand::Shutdown => "Shutdown",
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
            response,
        } => {
            let play_started_at = Instant::now();
            if !state.available {
                let _ = response.send(Err("PipeWire backend unavailable".to_string()));
            } else {
                state.finished_playbacks.clear();
                let play_id = uuid::Uuid::new_v4().to_string();
                let init_started_at = Instant::now();
                match ActivePlayback::new(
                    play_id.clone(),
                    sound_id,
                    path,
                    state.next_playback_order,
                    base_volume,
                    sound_lufs,
                    &state.runtime,
                ) {
                    Ok(playback) => {
                        let init_elapsed = init_started_at.elapsed();
                        let init_elapsed_ms = init_elapsed.as_millis();
                        if init_elapsed_ms >= 100 {
                            debug!(
                                "ActivePlayback initialization was slow: elapsed_ms={} play_id={}",
                                init_elapsed_ms, play_id
                            );
                        }
                        state.next_playback_order = state.next_playback_order.saturating_add(1);
                        state.active_playback = Some(playback);
                        let _ = response.send(Ok(play_id));
                    }
                    Err(err) => {
                        let init_elapsed_ms = init_started_at.elapsed().as_millis();
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
            if state
                .active_playback
                .as_ref()
                .is_some_and(|playback| playback.sound_id == sound_id)
            {
                state.active_playback = None;
                clear_output_queues(&state.queues);
            }
        }
        AudioCommand::StopAll => {
            state.active_playback = None;
            state.finished_playbacks.clear();
            clear_all_queues(&state.queues);
        }
        AudioCommand::Seek {
            play_id,
            position_ms,
        } => {
            let runtime = state.runtime.clone();
            if let Some(playback) = state
                .active_playback
                .as_mut()
                .filter(|playback| playback.play_id == play_id)
            {
                let _ = playback.seek(position_ms, &runtime);
                clear_output_queues(&state.queues);
            }
        }
        AudioCommand::Pause { sound_id } => {
            if let Some(playback) = state
                .active_playback
                .as_mut()
                .filter(|playback| playback.sound_id == sound_id)
            {
                playback.paused = true;
            }
        }
        AudioCommand::Resume { sound_id } => {
            if let Some(playback) = state
                .active_playback
                .as_mut()
                .filter(|playback| playback.sound_id == sound_id)
            {
                playback.paused = false;
            }
        }
        AudioCommand::SetLocalVolume { volume } => state.runtime.local_volume = volume,
        AudioCommand::SetMicVolume { volume } => state.runtime.mic_volume = volume,
        AudioCommand::SetAutoGainEnabled { enabled } => {
            state.runtime.auto_gain.enabled = enabled;
            let runtime = state.runtime.clone();
            if let Some(playback) = state.active_playback.as_mut() {
                playback.reset_limiters(&runtime);
            }
        }
        AudioCommand::SetAutoGainTarget { target_lufs } => {
            state.runtime.auto_gain.target_lufs = target_lufs;
        }
        AudioCommand::SetAutoGainMode { mode } => {
            state.runtime.auto_gain.mode = AutoGainMode::from_u32(mode);
            let runtime = state.runtime.clone();
            if let Some(playback) = state.active_playback.as_mut() {
                playback.reset_limiters(&runtime);
            }
        }
        AudioCommand::SetAutoGainApplyTo { apply_to } => {
            state.runtime.auto_gain.apply_to = AutoGainApplyTo::from_u32(apply_to);
            let runtime = state.runtime.clone();
            if let Some(playback) = state.active_playback.as_mut() {
                playback.reset_limiters(&runtime);
            }
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
            let runtime = state.runtime.clone();
            if let Some(playback) = state.active_playback.as_mut() {
                playback.reset_limiters(&runtime);
            }
        }
        AudioCommand::SetLooping { enabled } => state.runtime.looping = enabled,
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
        AudioCommand::Shutdown => {
            let _ = restore_default_source(&mut state);
            clear_all_queues(&state.queues);
            return true;
        }
    }

    state.publish_snapshot();
    false
}
