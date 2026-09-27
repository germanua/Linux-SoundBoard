use super::*;

pub(super) const TRANSITION_FADE_SAMPLES: u64 = 480;

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
    pub(super) fade_in_remaining: u64,
    pub(super) pause_fade_out_remaining: u64,
    pub(super) pending_seek_ms: Option<u64>,
    pub(super) seek_fade_out_remaining: u64,
    pub(super) paused: bool,
    pub(super) finished: bool,
    pub(super) source_exhausted: bool,
    pub(super) local_limiter: Option<LookAheadLimiter>,
    pub(super) virtual_limiter: Option<LookAheadLimiter>,
    pub(super) last_dynamic_enabled: bool,
    pub(super) last_dynamic_mode: AutoGainMode,
    pub(super) last_dynamic_apply_to: AutoGainApplyTo,
    pub(super) last_dynamic_params: AutoGainDynamicParams,
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
            sound_true_peak_dbtp,
            playback_order,
            duration_ms,
            source,
            position_ms: 0,
            fallback_samples_written: 0,
            fade_in_remaining: TRANSITION_FADE_SAMPLES,
            pause_fade_out_remaining: 0,
            pending_seek_ms: None,
            seek_fade_out_remaining: 0,
            paused: false,
            finished: false,
            source_exhausted: false,
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
        self.last_dynamic_enabled = config.auto_gain.enabled;
        self.last_dynamic_mode = config.auto_gain.mode;
        self.last_dynamic_apply_to = config.auto_gain.apply_to;
        self.last_dynamic_params = config.auto_gain.dynamic;
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
        self.fade_in_remaining = TRANSITION_FADE_SAMPLES;
        self.reset_limiters(config);
        Ok(())
    }

    pub(super) fn request_seek(&mut self, position_ms: u64, config: &RuntimeConfig) {
        if self.paused && self.pause_fade_out_remaining == 0 {
            let _ = self.seek(position_ms, config);
            return;
        }
        let clamped = clamp_seek_position_ms(position_ms, self.duration_ms);
        self.position_ms = clamped;
        self.pending_seek_ms = Some(clamped);
        self.seek_fade_out_remaining = TRANSITION_FADE_SAMPLES;
        self.fade_in_remaining = 0;
    }

    pub(super) fn set_paused(&mut self, paused: bool) {
        if paused == self.paused {
            return;
        }
        self.paused = paused;
        self.pending_seek_ms = None;
        self.seek_fade_out_remaining = 0;
        if paused {
            self.pause_fade_out_remaining = TRANSITION_FADE_SAMPLES;
            self.fade_in_remaining = 0;
        } else {
            self.pause_fade_out_remaining = 0;
            self.fade_in_remaining = TRANSITION_FADE_SAMPLES;
        }
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
        if self.finished || (self.paused && self.pause_fade_out_remaining == 0) {
            return;
        }

        if self.last_dynamic_enabled != config.auto_gain.enabled
            || self.last_dynamic_mode != config.auto_gain.mode
            || self.last_dynamic_apply_to != config.auto_gain.apply_to
            || self.last_dynamic_params != config.auto_gain.dynamic
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
            let virtual_scaled = normalized * self.base_volume * config.mic_volume * virtual_gain;

            let mut transition_scale = 1.0;
            if self.fade_in_remaining > 0 {
                transition_scale *= (TRANSITION_FADE_SAMPLES - self.fade_in_remaining) as f32
                    / (TRANSITION_FADE_SAMPLES - 1) as f32;
                self.fade_in_remaining = self.fade_in_remaining.saturating_sub(1);
            }
            if self.paused && self.pause_fade_out_remaining > 0 {
                transition_scale *= (self.pause_fade_out_remaining - 1) as f32
                    / (TRANSITION_FADE_SAMPLES - 1) as f32;
                self.pause_fade_out_remaining = self.pause_fade_out_remaining.saturating_sub(1);
            }
            if self.pending_seek_ms.is_some() && self.seek_fade_out_remaining > 0 {
                transition_scale *= (self.seek_fade_out_remaining - 1) as f32
                    / (TRANSITION_FADE_SAMPLES - 1) as f32;
                self.seek_fade_out_remaining = self.seek_fade_out_remaining.saturating_sub(1);
            }
            let local_faded = local_scaled * transition_scale;
            let virtual_faded = virtual_scaled * transition_scale;

            local[index] = if let Some(limiter) = self.local_limiter.as_mut() {
                limiter.process(local_faded)
            } else {
                local_faded
            };

            let virtual_lufs_processed = if let Some(limiter) = self.virtual_limiter.as_mut() {
                limiter.process(virtual_faded)
            } else {
                virtual_faded
            };

            virtual_out[index] = virtual_lufs_processed * virtual_boost_gain;

            index += 1;

            if self.seek_fade_out_remaining == 0 {
                if let Some(position_ms) = self.pending_seek_ms.take() {
                    let _ = self.seek(position_ms, config);
                }
            }
            if self.paused && self.pause_fade_out_remaining == 0 {
                break;
            }
        }

        self.position_ms = (self.fallback_samples_written * 1000)
            / (TARGET_OUTPUT_SAMPLE_RATE as u64 * TARGET_OUTPUT_CHANNELS as u64);
    }
}
