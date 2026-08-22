use super::*;

pub(super) fn mix_tick(state_rc: &Rc<RefCell<LoopState>>) {
    let mut state = state_rc.borrow_mut();
    if !state.backend_playback_available() {
        state.available = false;
        state.publish_snapshot();
        return;
    }
    state.available = true;

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
    let capture_stream_active = state.capture_stream_active();
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
        let Some((local_deficit, virtual_deficit)) =
            current_queue_deficits(&state.queues, local_target_samples, virtual_target_samples)
        else {
            // Queue mutex is contended by an RT callback right now. Bail and
            // retry on the next tick (2 ms) instead of blocking the main loop.
            state.stream_runtime.record_lock_contention();
            return;
        };
        let wanted_samples = local_deficit.max(virtual_deficit);
        if wanted_samples == 0 {
            break;
        }

        let chunk_samples = wanted_samples.min(MIX_CHUNK_FRAMES * TARGET_OUTPUT_CHANNELS as usize);
        let pushed = enqueue_mixed_chunk(state, chunk_samples);
        if pushed == 0 {
            break;
        }
        batches = batches.saturating_add(1);
    }

    if let Some((local_deficit, virtual_deficit)) =
        current_queue_deficits(&state.queues, local_target_samples, virtual_target_samples)
    {
        let needs_more_audio = local_deficit > 0 || virtual_deficit > 0;
        if needs_more_audio {
            let elapsed_ms = fill_started_at.elapsed().as_millis();
            if batches >= max_fill_batches {
                trace!(
                    "Mix fill budget exhausted: batches={} elapsed_ms={} local_deficit_samples={} virtual_deficit_samples={}",
                    batches,
                    elapsed_ms,
                    local_deficit,
                    virtual_deficit
                );
            }
            trace!(
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

    if let Some(mut queues) = state.queues.try_lock() {
        let dropped_virtual = queues
            .virtual_out
            .trim_oldest_to(max_virtual_backlog_samples);
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
    // Contention: skip trim this tick; next tick will catch up. Backlog is a
    // soft constraint, dropping a tick of trim has no audible impact.
}

fn current_queue_deficits(
    queues: &RtSharedQueues,
    local_target_samples: usize,
    virtual_target_samples: usize,
) -> Option<(usize, usize)> {
    let queues = queues.try_lock()?;
    let local_deficit = local_target_samples.saturating_sub(queues.local.len());
    let virtual_deficit = virtual_target_samples.saturating_sub(queues.virtual_out.len());
    Some((local_deficit, virtual_deficit))
}

fn enqueue_mixed_chunk(state: &mut LoopState, chunk_samples: usize) -> usize {
    let runtime = state.runtime.clone();
    let playback_active = state.active_playback.is_some();
    let capture_stream_active = state.capture_stream_active();
    let passthrough_active = state.runtime.mic_passthrough && capture_stream_active;

    if passthrough_active && !playback_active {
        return if let Some(mut queues) = state.queues.try_lock() {
            enqueue_passthrough_chunk(&mut queues, chunk_samples)
        } else {
            // Contended — try again on next mix tick. Blocking here would
            // invert priority: the main loop waiting on the RT callback's lock.
            state.stream_runtime.record_lock_contention();
            0
        };
    }

    if state.local_mix_buffer.len() != chunk_samples {
        state.local_mix_buffer.resize(chunk_samples, 0.0);
    } else {
        state.local_mix_buffer.fill(0.0);
    }
    if state.virtual_mix_buffer.len() != chunk_samples {
        state.virtual_mix_buffer.resize(chunk_samples, 0.0);
    } else {
        state.virtual_mix_buffer.fill(0.0);
    }

    if let Some(playback) = state.active_playback.as_mut() {
        playback.render_into(
            &mut state.local_mix_buffer,
            &mut state.virtual_mix_buffer,
            &runtime,
        );
    }

    // Pre-size the mic scratch buffer before acquiring the lock so the
    // allocator never runs while the RT callback is waiting on try_lock.
    if state.mic_scratch_buffer.len() < chunk_samples {
        state.mic_scratch_buffer.resize(chunk_samples, 0.0);
    }

    let Some(mut queues) = state.queues.try_lock() else {
        state.stream_runtime.record_lock_contention();
        return 0;
    };

    if passthrough_active && queues.mic_in.len() >= chunk_samples {
        let slot = &mut state.mic_scratch_buffer[..chunk_samples];
        let dequeued = queues.mic_in.pop_into(slot);
        for (virtual_sample, mic_sample) in
            state.virtual_mix_buffer.iter_mut().zip(&slot[..dequeued])
        {
            *virtual_sample = (*virtual_sample + *mic_sample).clamp(-1.0, 1.0);
        }
    }

    if playback_active {
        queues.local.push_slice(&state.local_mix_buffer);
    }
    if playback_active || passthrough_active {
        queues.virtual_out.push_slice(&state.virtual_mix_buffer);
    }

    chunk_samples
}

pub(super) fn enqueue_passthrough_chunk(queues: &mut ProcessQueues, chunk_samples: usize) -> usize {
    if queues.mic_in.len() < chunk_samples {
        return 0;
    }
    let mut samples = vec![0.0; chunk_samples];
    let dequeued = queues.mic_in.pop_into(&mut samples);
    queues.virtual_out.push_slice(&samples[..dequeued]);
    dequeued
}

pub(super) fn clear_virtual_mic_queues(queues: &RtSharedQueues) {
    // Best-effort clear: if contended right now, the trim_latency_backlog on
    // the next tick will catch up by draining stale samples. We never block.
    if let Some(mut queues) = queues.try_lock() {
        queues.mic_in.samples.clear();
        queues.virtual_out.samples.clear();
    }
}

pub(super) fn clear_mic_input_queue(queues: &RtSharedQueues) {
    if let Some(mut queues) = queues.try_lock() {
        queues.mic_in.samples.clear();
    }
}

pub(super) fn clear_all_queues(queues: &RtSharedQueues) {
    if let Some(mut queues) = queues.try_lock() {
        queues.local.samples.clear();
        queues.virtual_out.samples.clear();
        queues.mic_in.samples.clear();
    }
}

// ~5 ms at 48 kHz stereo — enough to ramp to silence without audible delay
const FADE_OUT_SAMPLES: usize = 480;

/// Replace the output queue backlogs with a short linear fade-to-zero so that
/// a Stop or Seek does not cut the waveform at an arbitrary phase.
pub(super) fn fade_output_queues(queues: &RtSharedQueues) {
    if let Some(mut queues) = queues.try_lock() {
        apply_fade_out(&mut queues.local);
        apply_fade_out(&mut queues.virtual_out);
    }
}

pub(super) fn apply_fade_out(queue: &mut SampleQueue) {
    if queue.samples.is_empty() {
        return;
    }
    // Trim all but the last FADE_OUT_SAMPLES so the stop is immediate.
    let len = queue.samples.len();
    if len > FADE_OUT_SAMPLES {
        queue.samples.drain(..len - FADE_OUT_SAMPLES);
    }
    // Apply linear ramp: index 0 → full amplitude, last index → 0.0
    let total = queue.samples.len();
    for (i, sample) in queue.samples.iter_mut().enumerate() {
        let scale = 1.0 - (i as f32 / (total - 1).max(1) as f32);
        *sample *= scale;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_queue(values: &[f32]) -> SampleQueue {
        let mut q = SampleQueue::new(4096);
        q.push_slice(values);
        q
    }

    #[test]
    fn fade_out_empty_queue_is_noop() {
        let mut q = SampleQueue::new(4096);
        apply_fade_out(&mut q);
        assert_eq!(q.samples.len(), 0);
    }

    #[test]
    fn fade_out_two_samples_last_becomes_zero() {
        let mut q = make_queue(&[0.8, 0.8]);
        apply_fade_out(&mut q);
        let samples: Vec<f32> = q.samples.iter().copied().collect();
        assert_eq!(samples.len(), 2);
        assert!(
            (samples[0] - 0.8).abs() < 1e-6,
            "first sample={}",
            samples[0]
        );
        assert!(samples[1].abs() < 1e-6, "last sample={}", samples[1]);
    }

    #[test]
    fn fade_out_long_queue_trims_to_fade_window() {
        let input = vec![1.0f32; 1000];
        let mut q = make_queue(&input);
        apply_fade_out(&mut q);
        assert_eq!(q.samples.len(), FADE_OUT_SAMPLES);
        let first = *q.samples.front().unwrap();
        let last = *q.samples.back().unwrap();
        assert!((first - 1.0).abs() < 1e-6, "first={}", first);
        assert!(last.abs() < 1e-6, "last={}", last);
    }

    #[test]
    fn passthrough_chunk_returns_zero_when_mic_empty() {
        let mut queues = ProcessQueues::new(4096, 4096, 4096);
        let result = enqueue_passthrough_chunk(&mut queues, 128);
        assert_eq!(result, 0);
        assert_eq!(queues.virtual_out.len(), 0);
    }

    #[test]
    fn passthrough_chunk_transfers_when_mic_has_enough() {
        let mut queues = ProcessQueues::new(4096, 4096, 4096);
        queues.mic_in.push_slice(&[0.5f32; 256]);
        let result = enqueue_passthrough_chunk(&mut queues, 128);
        assert_eq!(result, 128);
        assert_eq!(queues.virtual_out.len(), 128);
        assert_eq!(queues.mic_in.len(), 128);
    }

    #[test]
    fn passthrough_chunk_returns_zero_when_mic_shorter_than_chunk() {
        let mut queues = ProcessQueues::new(4096, 4096, 4096);
        queues.mic_in.push_slice(&[0.5f32; 64]);
        let result = enqueue_passthrough_chunk(&mut queues, 128);
        assert_eq!(result, 0);
        assert_eq!(queues.virtual_out.len(), 0);
        assert_eq!(queues.mic_in.len(), 64, "mic_in should be untouched");
    }
}
