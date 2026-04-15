use super::*;

pub(super) fn mix_tick(state_rc: &Rc<RefCell<LoopState>>) {
    let mut state = state_rc.borrow_mut();
    if !state.available {
        state.publish_snapshot();
        return;
    }

    fill_output_queues(&mut state);

    let finished_snapshot = state
        .active_playback
        .as_ref()
        .filter(|playback| playback.finished)
        .map(|playback| PlaybackSnapshot {
            sound_id: playback.sound_id.clone(),
            playback_order: playback.playback_order,
            position_ms: playback.position_ms,
            paused: playback.paused,
            duration_ms: playback.duration_ms,
            finished: true,
        });
    if let Some(snapshot) = finished_snapshot {
        let play_id = state
            .active_playback
            .as_ref()
            .map(|playback| playback.play_id.clone())
            .unwrap_or_default();
        state.finished_playbacks.insert(play_id, snapshot);
        state.trim_finished_playbacks(MAX_FINISHED_PLAYBACK_SNAPSHOTS);
        state.active_playback = None;
    } else if let Some(play_id) = state
        .active_playback
        .as_ref()
        .map(|playback| playback.play_id.clone())
    {
        state.finished_playbacks.remove(&play_id);
    }
    state.publish_snapshot();
}

pub(super) fn fill_output_queues(state: &mut LoopState) {
    let playback_active = state.active_playback.is_some();
    let capture_stream_active = state
        .backend
        .as_ref()
        .and_then(|backend| backend.capture_stream.as_ref())
        .is_some();
    let wants_local_output = playback_active;
    let wants_virtual_output =
        playback_active || (state.runtime.mic_passthrough && capture_stream_active);
    if !wants_local_output && !wants_virtual_output {
        state.ultra_starvation_ticks = 0;
        return;
    }

    trim_latency_backlog(state, wants_virtual_output);

    let local_target_samples = if wants_local_output {
        state.runtime.local_output_target_samples()
    } else {
        0
    };
    let virtual_target_samples = if wants_virtual_output {
        state.runtime.virtual_output_target_samples()
    } else {
        0
    };
    let max_fill_batches = state
        .runtime
        .max_fill_batches_per_tick(wants_local_output, wants_virtual_output);

    let fill_started_at = Instant::now();
    let mut batches = 0usize;
    while batches < max_fill_batches {
        let Some((local_deficit, virtual_deficit)) = current_queue_deficits(
            &state.queues,
            local_target_samples,
            virtual_target_samples,
        )
        else {
            return;
        };
        let wanted_samples = local_deficit.max(virtual_deficit);
        if wanted_samples == 0 {
            break;
        }

        let chunk_samples = wanted_samples.min(MIX_CHUNK_FRAMES * TARGET_OUTPUT_CHANNELS as usize);
        enqueue_mixed_chunk(state, chunk_samples);
        batches = batches.saturating_add(1);
    }

    if let Some((local_deficit, virtual_deficit)) = current_queue_deficits(
        &state.queues,
        local_target_samples,
        virtual_target_samples,
    )
    {
        let needs_more_audio = local_deficit > 0 || virtual_deficit > 0;
        if needs_more_audio {
            let elapsed_ms = fill_started_at.elapsed().as_millis();
            if batches >= max_fill_batches {
                debug!(
                    "Mix fill budget exhausted: batches={} elapsed_ms={} local_deficit_samples={} virtual_deficit_samples={}",
                    batches,
                    elapsed_ms,
                    local_deficit,
                    virtual_deficit
                );
            }
            debug!(
                "Output queues remain short after fill: batches={} elapsed_ms={} local_deficit_samples={} virtual_deficit_samples={}",
                batches,
                elapsed_ms,
                local_deficit,
                virtual_deficit
            );
        }

        if state.runtime.mic_latency_profile == MicLatencyProfile::Ultra && wants_virtual_output {
            if needs_more_audio {
                state.ultra_starvation_ticks = state.ultra_starvation_ticks.saturating_add(1);
                if state.ultra_starvation_ticks >= ULTRA_STARVATION_TICK_FALLBACK_THRESHOLD {
                    warn!(
                        "Ultra mic latency profile is underrunning; falling back to low latency profile"
                    );
                    state.runtime.mic_latency_profile = MicLatencyProfile::Low;
                    state.stream_runtime.apply_runtime(&state.runtime);
                    state.ultra_starvation_ticks = 0;
                    clear_virtual_mic_queues(&state.queues);
                }
            } else {
                state.ultra_starvation_ticks = 0;
            }
        } else {
            state.ultra_starvation_ticks = 0;
        }
    }
}

fn trim_latency_backlog(state: &mut LoopState, wants_virtual_output: bool) {
    if !wants_virtual_output {
        return;
    }

    let max_virtual_backlog_samples = state.stream_runtime.max_virtual_backlog_samples();
    let max_mic_backlog_samples = state.stream_runtime.max_mic_backlog_samples();

    if let Ok(mut queues) = state.queues.lock() {
        let dropped_virtual = queues.virtual_out.trim_oldest_to(max_virtual_backlog_samples);
        let dropped_mic = queues.mic_in.trim_oldest_to(max_mic_backlog_samples);
        if dropped_virtual > 0 || dropped_mic > 0 {
            debug!(
                "Dropped stale mic backlog: dropped_virtual_samples={} dropped_mic_samples={} profile={}",
                dropped_virtual,
                dropped_mic,
                state.runtime.mic_latency_profile.as_str()
            );
        }
    }
}

fn current_queue_deficits(
    queues: &std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
    local_target_samples: usize,
    virtual_target_samples: usize,
) -> Option<(usize, usize)> {
    let queues = queues.lock().ok()?;
    let local_deficit = local_target_samples.saturating_sub(queues.local.len());
    let virtual_deficit = virtual_target_samples.saturating_sub(queues.virtual_out.len());
    Some((local_deficit, virtual_deficit))
}

fn enqueue_mixed_chunk(state: &mut LoopState, chunk_samples: usize) {
    let runtime = state.runtime.clone();
    let playback_active = state.active_playback.is_some();
    let capture_stream_active = state
        .backend
        .as_ref()
        .and_then(|backend| backend.capture_stream.as_ref())
        .is_some();
    let passthrough_active = state.runtime.mic_passthrough && capture_stream_active;

    if passthrough_active && !playback_active {
        if let Ok(mut queues) = state.queues.lock() {
            let mut passthrough_samples = vec![0.0; chunk_samples];
            let dequeued = queues.mic_in.pop_into(&mut passthrough_samples);
            if dequeued > 0 {
                queues.virtual_out.push_slice(&passthrough_samples[..dequeued]);
            }
        }
        return;
    }

    let (local_samples, mut virtual_samples) = if let Some(playback) = state.active_playback.as_mut()
    {
        playback.render(chunk_samples, &runtime)
    } else {
        (vec![0.0; chunk_samples], vec![0.0; chunk_samples])
    };

    if let Ok(mut queues) = state.queues.lock() {
        if passthrough_active {
            let mic_samples = queues.mic_in.pop_samples(chunk_samples);
            for (virtual_sample, mic_sample) in virtual_samples.iter_mut().zip(mic_samples) {
                *virtual_sample = (*virtual_sample + mic_sample).clamp(-1.0, 1.0);
            }
        }

        if playback_active {
            queues.local.push_slice(&local_samples);
        }
        if playback_active || passthrough_active {
            queues.virtual_out.push_slice(&virtual_samples);
        }
    }
}

pub(super) fn clear_output_queues(queues: &std::sync::Arc<std::sync::Mutex<ProcessQueues>>) {
    if let Ok(mut queues) = queues.lock() {
        queues.local.samples.clear();
        queues.virtual_out.samples.clear();
    }
}

pub(super) fn clear_virtual_mic_queues(
    queues: &std::sync::Arc<std::sync::Mutex<ProcessQueues>>,
) {
    if let Ok(mut queues) = queues.lock() {
        queues.mic_in.samples.clear();
        queues.virtual_out.samples.clear();
    }
}

pub(super) fn clear_all_queues(queues: &std::sync::Arc<std::sync::Mutex<ProcessQueues>>) {
    if let Ok(mut queues) = queues.lock() {
        queues.local.samples.clear();
        queues.virtual_out.samples.clear();
        queues.mic_in.samples.clear();
    }
}
