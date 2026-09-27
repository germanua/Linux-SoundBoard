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

    state.retire_finished_voices();
    state.publish_snapshot();
}

pub(super) fn fill_output_queues(state: &mut LoopState) {
    let playback_active = !state.active_playbacks.is_empty();
    let passthrough_active = state.runtime.mic_passthrough && state.capture_stream_active();
    let wants_local_output = playback_active;
    let wants_virtual_output = playback_active || passthrough_active;
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
            state.stream_runtime.record_lock_contention();
            return;
        };
        let wanted_samples = local_deficit.max(virtual_deficit);
        if wanted_samples == 0 {
            break;
        }

        let chunk_samples = wanted_samples.min(MIX_CHUNK_FRAMES * TARGET_OUTPUT_CHANNELS as usize);
        let pushed = enqueue_mixed_chunk(state, chunk_samples, passthrough_active);
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

fn enqueue_mixed_chunk(
    state: &mut LoopState,
    chunk_samples: usize,
    passthrough_active: bool,
) -> usize {
    let runtime = state.runtime.clone();
    let playback_active = !state.active_playbacks.is_empty();

    if passthrough_active && !playback_active {
        return if let Some(mut queues) = state.queues.try_lock() {
            enqueue_passthrough_chunk(&mut queues, chunk_samples)
        } else {
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

    if state.voice_local_scratch.len() != chunk_samples {
        state.voice_local_scratch.resize(chunk_samples, 0.0);
    }
    if state.voice_virtual_scratch.len() != chunk_samples {
        state.voice_virtual_scratch.resize(chunk_samples, 0.0);
    }

    for playback in &mut state.active_playbacks {
        playback.render_into(
            &mut state.voice_local_scratch,
            &mut state.voice_virtual_scratch,
            &runtime,
        );
        for (mixed, voice_sample) in state
            .local_mix_buffer
            .iter_mut()
            .zip(&state.voice_local_scratch)
        {
            *mixed += *voice_sample;
        }
        for (mixed, voice_sample) in state
            .virtual_mix_buffer
            .iter_mut()
            .zip(&state.voice_virtual_scratch)
        {
            *mixed += *voice_sample;
        }
    }

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
        add_mic_chunk(&mut state.virtual_mix_buffer, &slot[..dequeued]);
    }

    for sample in state.local_mix_buffer.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }
    for sample in state.virtual_mix_buffer.iter_mut() {
        *sample = sample.clamp(-1.0, 1.0);
    }

    if playback_active {
        queues.local.push_slice(&state.local_mix_buffer);
    }
    if playback_active || passthrough_active {
        queues.virtual_out.push_slice(&state.virtual_mix_buffer);
    }

    chunk_samples
}

fn add_mic_chunk(virtual_mix: &mut [f32], mic: &[f32]) {
    for (virtual_sample, mic_sample) in virtual_mix.iter_mut().zip(mic) {
        *virtual_sample += *mic_sample;
    }
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

const FADE_OUT_SAMPLES: usize = 480;

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

    let len = queue.samples.len();
    if len > FADE_OUT_SAMPLES {
        queue.samples.truncate(FADE_OUT_SAMPLES);
    }

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
    fn fade_out_long_queue_keeps_the_next_samples_and_discards_the_future_tail() {
        let input: Vec<f32> = (0..1000).map(|value| value as f32 / 1000.0).collect();
        let mut q = make_queue(&input);
        apply_fade_out(&mut q);
        assert_eq!(q.samples.len(), FADE_OUT_SAMPLES);
        let first = *q.samples.front().unwrap();
        let last = *q.samples.back().unwrap();
        assert!(first.abs() < 1e-6, "first={first}");
        assert!(last.abs() < 1e-6, "last={last}");
        assert!(
            q.samples.iter().copied().fold(0.0f32, f32::max) < 0.2,
            "fade must use the front of the queued audio rather than jumping to its tail"
        );
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

    fn mix_runtime() -> RuntimeConfig {
        let mut runtime =
            super::super::test_runtime_config_with_mode(crate::config::DefaultSourceMode::Manual);
        runtime.local_volume = 1.0;
        runtime.mic_volume = 1.0;
        runtime
    }

    fn constant_wav(value: i16) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("lsb-wp5-mix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create mix dir");
        let path = dir.join("tone.wav");
        let rate = 48_000u32;
        let channels = 2u16;
        let frames = 4_096usize;
        let mut pcm = Vec::with_capacity(frames * 2 * 2);
        for _ in 0..frames {
            for _ in 0..channels {
                pcm.extend_from_slice(&value.to_le_bytes());
            }
        }
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + pcm.len() as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend_from_slice(&(channels * 2).to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(pcm.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&pcm);
        std::fs::write(&path, bytes).expect("write constant wav");
        path
    }

    fn mix_chunk(
        runtime: &RuntimeConfig,
        voices: &[(&std::path::Path, f32)],
    ) -> (Vec<f32>, Vec<f32>) {
        mix_chunk_with_mic(runtime, voices, None)
    }

    fn mix_chunk_with_mic(
        runtime: &RuntimeConfig,
        voices: &[(&std::path::Path, f32)],
        mic: Option<f32>,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut state = LoopState::new(runtime.clone(), super::super::test_player_snapshot_store());
        for (index, (path, base_volume)) in voices.iter().enumerate() {
            let voice = ActivePlayback::new(
                format!("play-{index}"),
                format!("sound-{index}"),
                path.to_string_lossy().to_string(),
                index as u64,
                *base_volume,
                None,
                None,
                runtime,
            )
            .expect("create voice");
            state.adopt_voice(voice);
        }
        let chunk = MIX_CHUNK_FRAMES * TARGET_OUTPUT_CHANNELS as usize;
        let passthrough_active = if let Some(mic_value) = mic {
            let mic_chunk = vec![mic_value; chunk];
            state.queues.lock().mic_in.push_slice(&mic_chunk);
            true
        } else {
            false
        };
        assert_eq!(
            enqueue_mixed_chunk(&mut state, chunk, passthrough_active),
            chunk
        );
        let queues = state.queues.lock();
        (
            queues.local.samples.iter().copied().collect(),
            queues.virtual_out.samples.iter().copied().collect(),
        )
    }

    fn tail(samples: &[f32]) -> f32 {
        *samples.last().expect("mixed samples")
    }

    #[test]
    fn two_voices_sum_into_one_mix() {
        let runtime = mix_runtime();

        let path = constant_wav(8_192);
        let (local, virtual_out) = mix_chunk(&runtime, &[(&path, 1.0), (&path, 1.0)]);

        assert!((tail(&local) - 0.5).abs() < 1e-6, "local={}", tail(&local));
        assert!(
            (tail(&virtual_out) - 0.5).abs() < 1e-6,
            "virtual={}",
            tail(&virtual_out)
        );
        assert!(local[480..]
            .iter()
            .all(|sample| (sample - 0.5).abs() < 1e-6));
    }

    #[test]
    fn the_bus_clamp_bounds_the_summed_voices() {
        let runtime = mix_runtime();
        let loud = constant_wav(26_214);
        let (local, virtual_out) = mix_chunk(&runtime, &[(&loud, 1.0), (&loud, 1.0)]);
        assert_eq!(tail(&local), 1.0, "the sum is clamped at the bus");
        assert_eq!(tail(&virtual_out), 1.0);
        assert!(local.iter().all(|sample| sample.abs() <= 1.0));

        let quiet = constant_wav(-26_214);
        let (local, _) = mix_chunk(&runtime, &[(&quiet, 1.0), (&quiet, 1.0)]);
        assert_eq!(tail(&local), -1.0);
        assert!(local.iter().all(|sample| sample.abs() <= 1.0));
    }

    #[test]
    fn opposite_voices_cancel() {
        let runtime = mix_runtime();
        let positive = constant_wav(16_384);
        let negative = constant_wav(-16_384);
        let (local, virtual_out) = mix_chunk(&runtime, &[(&positive, 1.0), (&negative, 1.0)]);
        assert!(tail(&local).abs() < 1e-6, "local={}", tail(&local));
        assert!(tail(&virtual_out).abs() < 1e-6);
    }

    #[test]
    fn the_mix_does_not_depend_on_voice_order() {
        let runtime = mix_runtime();
        let loud = constant_wav(26_214);
        let quiet = constant_wav(8_192);
        let forward = mix_chunk(&runtime, &[(&loud, 1.0), (&quiet, 1.0)]);
        let reversed = mix_chunk(&runtime, &[(&quiet, 1.0), (&loud, 1.0)]);
        assert_eq!(forward.0, reversed.0);
        assert_eq!(forward.1, reversed.1);
    }

    #[test]
    fn every_voice_reaches_both_outputs() {
        let runtime = mix_runtime();
        let path = constant_wav(8_192);

        let (local, virtual_out) = mix_chunk(&runtime, &[(&path, 1.0), (&path, 0.5)]);
        assert!(
            (tail(&local) - 0.375).abs() < 1e-6,
            "local={}",
            tail(&local)
        );
        assert!(
            (tail(&virtual_out) - 0.375).abs() < 1e-6,
            "virtual={}",
            tail(&virtual_out)
        );
    }

    #[test]
    fn the_mic_is_summed_once_per_chunk() {
        let mut mix = vec![0.5f32; 4];
        add_mic_chunk(&mut mix, &[0.25f32; 4]);
        assert!(
            mix.iter().all(|sample| (sample - 0.75).abs() < 1e-6),
            "{mix:?}"
        );
    }

    #[test]
    fn the_mic_is_summed_once_however_many_voices_are_mixed() {
        let runtime = mix_runtime();
        let path = constant_wav(8_192);

        let (local, virtual_out) =
            mix_chunk_with_mic(&runtime, &[(&path, 1.0), (&path, 1.0)], Some(0.25));
        assert!(
            (tail(&virtual_out) - 0.75).abs() < 1e-6,
            "two voices + one mic chunk: virtual={}",
            tail(&virtual_out)
        );
        assert!((tail(&local) - 0.5).abs() < 1e-6, "local={}", tail(&local));
        assert!(virtual_out.iter().all(|sample| sample.abs() <= 1.0));

        let (_, single_voice) = mix_chunk_with_mic(&runtime, &[(&path, 1.0)], Some(0.25));
        assert!(
            (tail(&single_voice) - 0.5).abs() < 1e-6,
            "one voice + one mic chunk: virtual={}",
            tail(&single_voice)
        );
        let (_, three_voices) = mix_chunk_with_mic(
            &runtime,
            &[(&path, 1.0), (&path, 1.0), (&path, 1.0)],
            Some(0.25),
        );
        assert!(
            (tail(&three_voices) - 1.0).abs() < 1e-6,
            "three voices + one mic chunk: virtual={}",
            tail(&three_voices)
        );
    }
}
