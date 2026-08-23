use super::*;

pub(super) struct ActivePlayback {
    pub(super) play_id: String,
    pub(super) sound_id: String,
    pub(super) base_volume: f32,
    pub(super) sound_lufs: Option<f64>,
    pub(super) sound_true_peak_dbtp: Option<f32>,
    pub(super) playback_order: u64,
    pub(super) duration_ms: Option<u64>,
    pub(super) source: ResettablePlaybackSource<
        PlaybackSource,
        Box<dyn Fn() -> Result<PlaybackSource, EngineError>>,
    >,
    pub(super) position_ms: u64,
    pub(super) fallback_samples_written: u64,
    pub(super) paused: bool,
    pub(super) finished: bool,
    pub(super) source_exhausted: bool,
    pub(super) local_limiter: Option<LookAheadLimiter>,
    pub(super) virtual_limiter: Option<LookAheadLimiter>,
    pub(super) last_dynamic_enabled: bool,
    pub(super) last_dynamic_mode: AutoGainMode,
    pub(super) last_dynamic_apply_to: AutoGainApplyTo,
    pub(super) last_dynamic_params: AutoGainDynamicParams,
    pub(super) last_loudness_boost_enabled: bool,
}

impl ActivePlayback {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        play_id: String,
        sound_id: String,
        path: String,
        playback_order: u64,
        base_volume: f32,
        sound_lufs: Option<f64>,
        sound_true_peak_dbtp: Option<f32>,
        config: &RuntimeConfig,
    ) -> Result<Self, EngineError> {
        let factory_path = path.clone();
        let factory: Box<dyn Fn() -> Result<PlaybackSource, EngineError>> =
            Box::new(move || PlaybackSource::from_path(&factory_path));
        let source = ResettablePlaybackSource::new(factory, TARGET_OUTPUT_SAMPLE_RATE)?;
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
        let virtual_limiter = if (local_dynamic_enabled
            && config.auto_gain.apply_to.applies_to_output(true))
            || config.loudness_boost_enabled
        {
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
            sound_true_peak_dbtp,
            playback_order,
            duration_ms,
            source,
            position_ms: 0,
            fallback_samples_written: 0,
            paused: false,
            finished: false,
            source_exhausted: false,
            local_limiter,
            virtual_limiter,
            last_dynamic_enabled: config.auto_gain.enabled,
            last_dynamic_mode: config.auto_gain.mode,
            last_dynamic_apply_to: config.auto_gain.apply_to,
            last_dynamic_params: config.auto_gain.dynamic,
            last_loudness_boost_enabled: config.loudness_boost_enabled,
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
        self.virtual_limiter = if (dynamic_enabled
            && config.auto_gain.apply_to.applies_to_output(true))
            || config.loudness_boost_enabled
        {
            Some(LookAheadLimiter::new(
                TARGET_OUTPUT_SAMPLE_RATE,
                TARGET_OUTPUT_CHANNELS as u16,
                config.auto_gain.dynamic,
            ))
        } else {
            None
        };
        self.last_dynamic_enabled = config.auto_gain.enabled;
        self.last_dynamic_mode = config.auto_gain.mode;
        self.last_dynamic_apply_to = config.auto_gain.apply_to;
        self.last_dynamic_params = config.auto_gain.dynamic;
        self.last_loudness_boost_enabled = config.loudness_boost_enabled;
    }

    pub(super) fn seek(
        &mut self,
        position_ms: u64,
        config: &RuntimeConfig,
    ) -> Result<(), EngineError> {
        let clamped = clamp_seek_position_ms(position_ms, self.duration_ms);
        self.source
            .seek_internal(Duration::from_millis(clamped))
            .map_err(|e| EngineError::Playback(format!("Seek failed: {e}")))?;
        self.fallback_samples_written =
            (clamped * TARGET_OUTPUT_SAMPLE_RATE as u64 * TARGET_OUTPUT_CHANNELS as u64) / 1000;
        self.position_ms = clamped;
        self.source_exhausted = false;
        self.finished = false;
        self.reset_limiters(config);
        Ok(())
    }

    pub(super) fn render_into(
        &mut self,
        local: &mut [f32],
        virtual_out: &mut [f32],
        config: &RuntimeConfig,
    ) {
        debug_assert_eq!(local.len(), virtual_out.len());
        local.fill(0.0);
        virtual_out.fill(0.0);
        let wanted_samples = local.len();
        if self.finished || self.paused {
            return;
        }

        if self.last_dynamic_enabled != config.auto_gain.enabled
            || self.last_dynamic_mode != config.auto_gain.mode
            || self.last_dynamic_apply_to != config.auto_gain.apply_to
            || self.last_dynamic_params != config.auto_gain.dynamic
            || self.last_loudness_boost_enabled != config.loudness_boost_enabled
        {
            self.reset_limiters(config);
        }

        let local_gain =
            config
                .auto_gain
                .gain_for(self.sound_lufs, self.sound_true_peak_dbtp, false);
        let virtual_gain =
            config
                .auto_gain
                .gain_for(self.sound_lufs, self.sound_true_peak_dbtp, true);
        let virtual_boost_gain = config.loudness_boost_gain(true);
        let mut index = 0usize;

        while index < wanted_samples {
            if self.source_exhausted {
                if config.looping && self.seek(0, config).is_ok() {
                    continue;
                }
                self.finished = true;
                break;
            }

            let Some(sample) = self.source.next() else {
                self.source_exhausted = true;
                continue;
            };

            self.fallback_samples_written = self.fallback_samples_written.saturating_add(1);
            let normalized = sample as f32 / 32768.0 * self.source.output_gain_factor();
            let local_scaled = normalized * self.base_volume * config.local_volume * local_gain;
            let virtual_scaled = normalized
                * self.base_volume
                * config.mic_volume
                * virtual_gain
                * virtual_boost_gain;

            // Fade in to avoid a cold-start click.
            const FADE_IN_SAMPLES: u64 = 480; // ~5 ms at 48 kHz stereo
            let fade_scale = if self.fallback_samples_written <= FADE_IN_SAMPLES {
                self.fallback_samples_written as f32 / FADE_IN_SAMPLES as f32
            } else {
                1.0
            };
            let local_faded = local_scaled * fade_scale;
            let virtual_faded = virtual_scaled * fade_scale;

            local[index] = if let Some(limiter) = self.local_limiter.as_mut() {
                limiter.process(local_faded)
            } else {
                local_faded
            }
            .clamp(-1.0, 1.0);

            virtual_out[index] = if let Some(limiter) = self.virtual_limiter.as_mut() {
                limiter.process(virtual_faded)
            } else {
                virtual_faded
            }
            .clamp(-1.0, 1.0);

            index += 1;
        }

        self.position_ms = (self.fallback_samples_written * 1000)
            / (TARGET_OUTPUT_SAMPLE_RATE as u64 * TARGET_OUTPUT_CHANNELS as u64);
    }
}
