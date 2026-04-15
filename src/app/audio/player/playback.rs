use super::*;
use std::collections::VecDeque;

pub(super) struct ActivePlayback {
    pub(super) play_id: String,
    pub(super) sound_id: String,
    pub(super) base_volume: f32,
    pub(super) sound_lufs: Option<f64>,
    pub(super) playback_order: u64,
    pub(super) duration_ms: Option<u64>,
    pub(super) source:
        ResettablePlaybackSource<SymphoniaSource, Box<dyn Fn() -> Result<SymphoniaSource, String>>>,
    pub(super) position_ms: u64,
    pub(super) fallback_samples_written: u64,
    pub(super) paused: bool,
    pub(super) finished: bool,
    pub(super) source_exhausted: bool,
    pub(super) pending_local_tail: VecDeque<f32>,
    pub(super) pending_virtual_tail: VecDeque<f32>,
    pub(super) local_limiter: Option<LookAheadLimiter>,
    pub(super) virtual_limiter: Option<LookAheadLimiter>,
    pub(super) last_dynamic_enabled: bool,
    pub(super) last_dynamic_mode: AutoGainMode,
    pub(super) last_dynamic_apply_to: AutoGainApplyTo,
    pub(super) last_dynamic_params: AutoGainDynamicParams,
}

impl ActivePlayback {
    pub(super) fn new(
        play_id: String,
        sound_id: String,
        path: String,
        playback_order: u64,
        base_volume: f32,
        sound_lufs: Option<f64>,
        config: &RuntimeConfig,
    ) -> Result<Self, String> {
        let factory_path = path.clone();
        let factory: Box<dyn Fn() -> Result<SymphoniaSource, String>> =
            Box::new(move || SymphoniaSource::from_path(&factory_path));
        let source = ResettablePlaybackSource::new(
            factory,
            TARGET_OUTPUT_CHANNELS as u16,
            TARGET_OUTPUT_SAMPLE_RATE,
        )?;
        let duration_ms = source.total_duration_ms();
        let local_dynamic_enabled =
            config.auto_gain.enabled && config.auto_gain.mode == AutoGainMode::DynamicLookAhead;
        let local_limiter =
            if local_dynamic_enabled && config.auto_gain.apply_to.applies_to_output(false) {
                Some(LookAheadLimiter::new(
                    TARGET_OUTPUT_SAMPLE_RATE,
                    TARGET_OUTPUT_CHANNELS as u16,
                    config.auto_gain.dynamic,
                ))
            } else {
                None
            };
        let virtual_limiter =
            if local_dynamic_enabled && config.auto_gain.apply_to.applies_to_output(true) {
                Some(LookAheadLimiter::new(
                    TARGET_OUTPUT_SAMPLE_RATE,
                    TARGET_OUTPUT_CHANNELS as u16,
                    config.auto_gain.dynamic,
                ))
            } else {
                None
            };

        Ok(Self {
            play_id,
            sound_id,
            base_volume,
            sound_lufs,
            playback_order,
            duration_ms,
            source,
            position_ms: 0,
            fallback_samples_written: 0,
            paused: false,
            finished: false,
            source_exhausted: false,
            pending_local_tail: VecDeque::new(),
            pending_virtual_tail: VecDeque::new(),
            local_limiter,
            virtual_limiter,
            last_dynamic_enabled: config.auto_gain.enabled,
            last_dynamic_mode: config.auto_gain.mode,
            last_dynamic_apply_to: config.auto_gain.apply_to,
            last_dynamic_params: config.auto_gain.dynamic,
        })
    }

    pub(super) fn reset_limiters(&mut self, config: &RuntimeConfig) {
        let dynamic_enabled =
            config.auto_gain.enabled && config.auto_gain.mode == AutoGainMode::DynamicLookAhead;
        self.local_limiter =
            if dynamic_enabled && config.auto_gain.apply_to.applies_to_output(false) {
                Some(LookAheadLimiter::new(
                    TARGET_OUTPUT_SAMPLE_RATE,
                    TARGET_OUTPUT_CHANNELS as u16,
                    config.auto_gain.dynamic,
                ))
            } else {
                None
            };
        self.virtual_limiter =
            if dynamic_enabled && config.auto_gain.apply_to.applies_to_output(true) {
                Some(LookAheadLimiter::new(
                    TARGET_OUTPUT_SAMPLE_RATE,
                    TARGET_OUTPUT_CHANNELS as u16,
                    config.auto_gain.dynamic,
                ))
            } else {
                None
            };
        self.pending_local_tail.clear();
        self.pending_virtual_tail.clear();
        self.last_dynamic_enabled = config.auto_gain.enabled;
        self.last_dynamic_mode = config.auto_gain.mode;
        self.last_dynamic_apply_to = config.auto_gain.apply_to;
        self.last_dynamic_params = config.auto_gain.dynamic;
    }

    pub(super) fn seek(&mut self, position_ms: u64, config: &RuntimeConfig) -> Result<(), String> {
        let clamped = clamp_seek_position_ms(position_ms, self.duration_ms);
        self.source
            .seek_internal(Duration::from_millis(clamped))
            .map_err(|e| format!("Seek failed: {e}"))?;
        self.fallback_samples_written =
            (clamped * TARGET_OUTPUT_SAMPLE_RATE as u64 * TARGET_OUTPUT_CHANNELS as u64) / 1000;
        self.position_ms = clamped;
        self.source_exhausted = false;
        self.pending_local_tail.clear();
        self.pending_virtual_tail.clear();
        self.finished = false;
        self.reset_limiters(config);
        Ok(())
    }

    pub(super) fn render(
        &mut self,
        wanted_samples: usize,
        config: &RuntimeConfig,
    ) -> (Vec<f32>, Vec<f32>) {
        let mut local = vec![0.0; wanted_samples];
        let mut virtual_out = vec![0.0; wanted_samples];
        if self.finished || self.paused {
            return (local, virtual_out);
        }

        if self.last_dynamic_enabled != config.auto_gain.enabled
            || self.last_dynamic_mode != config.auto_gain.mode
            || self.last_dynamic_apply_to != config.auto_gain.apply_to
            || self.last_dynamic_params != config.auto_gain.dynamic
        {
            self.reset_limiters(config);
        }

        let local_gain = config.auto_gain.gain_for(self.sound_lufs, false);
        let virtual_gain = config.auto_gain.gain_for(self.sound_lufs, true);
        let mut index = 0usize;

        while index < wanted_samples {
            if let Some(sample) = self.pending_local_tail.pop_front() {
                local[index] = sample;
            }
            if let Some(sample) = self.pending_virtual_tail.pop_front() {
                virtual_out[index] = sample;
            }
            if local[index] != 0.0 || virtual_out[index] != 0.0 {
                index += 1;
                continue;
            }

            if self.source_exhausted {
                if config.looping {
                    if self.seek(0, config).is_ok() {
                        continue;
                    }
                }
                self.finished = true;
                break;
            }

            let Some(sample) = self.source.next() else {
                self.source_exhausted = true;
                if let Some(limiter) = self.local_limiter.as_mut() {
                    self.pending_local_tail.extend(limiter.flush());
                }
                if let Some(limiter) = self.virtual_limiter.as_mut() {
                    self.pending_virtual_tail.extend(limiter.flush());
                }
                continue;
            };

            self.fallback_samples_written = self.fallback_samples_written.saturating_add(1);
            let normalized = sample as f32 / 32768.0;
            let local_scaled = normalized * self.base_volume * config.local_volume * local_gain;
            let virtual_scaled = normalized * self.base_volume * config.mic_volume * virtual_gain;

            local[index] = if let Some(limiter) = self.local_limiter.as_mut() {
                limiter.process(local_scaled)
            } else {
                local_scaled
            }
            .clamp(-1.0, 1.0);

            virtual_out[index] = if let Some(limiter) = self.virtual_limiter.as_mut() {
                limiter.process(virtual_scaled)
            } else {
                virtual_scaled
            }
            .clamp(-1.0, 1.0);

            index += 1;
        }

        self.position_ms = (self.fallback_samples_written * 1000)
            / (TARGET_OUTPUT_SAMPLE_RATE as u64 * TARGET_OUTPUT_CHANNELS as u64);
        (local, virtual_out)
    }
}
