use super::*;

#[derive(Debug, Clone)]
pub(super) struct RuntimeConfig {
    pub(super) local_volume: f32,
    pub(super) mic_volume: f32,
    pub(super) mic_passthrough: bool,
    pub(super) mic_source: Option<String>,
    pub(super) default_source_mode: DefaultSourceMode,
    pub(super) mic_latency_profile: MicLatencyProfile,
    pub(super) auto_gain: AutoGainState,
    pub(super) loudness_boost_enabled: bool,
    pub(super) loudness_boost_db: f64,
    pub(super) looping: bool,
    pub(super) audio_backend: AudioBackendKind,
}

impl RuntimeConfig {
    pub(super) fn from_config(config: &crate::config::Config) -> Self {
        let volume = config.settings.volume_domain();
        let routing = config.settings.mic_routing_domain();
        let playback = config.settings.playback_domain();
        Self {
            local_volume: if volume.local_mute {
                0.0
            } else {
                volume.local_volume as f32 / 100.0
            },
            mic_volume: volume.mic_volume as f32 / 100.0,
            mic_passthrough: routing.mic_passthrough,
            mic_source: routing.mic_source,
            default_source_mode: routing.default_source_mode,
            mic_latency_profile: routing.mic_latency_profile,
            auto_gain: AutoGainState::from_config(config),
            loudness_boost_enabled: config.settings.loudness_boost,
            loudness_boost_db: crate::config::normalize_loudness_boost_db(
                config.settings.loudness_boost_db,
            ),
            looping: playback.play_mode.should_loop(),
            audio_backend: AudioBackendKind::PipeWire,
        }
    }

    pub(super) fn loudness_boost_gain(&self, is_virtual_output: bool) -> f32 {
        if !self.loudness_boost_enabled || !is_virtual_output {
            return 1.0;
        }
        let boost_db = crate::config::normalize_loudness_boost_db(self.loudness_boost_db);
        10.0_f64.powf(boost_db / 20.0) as f32
    }

    pub(super) fn latency_tuning(&self) -> LatencyTuning {
        match self.mic_latency_profile {
            MicLatencyProfile::Balanced => LatencyTuning {
                virtual_target_frames: BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES,
                max_virtual_backlog_frames: BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                max_mic_backlog_frames: BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                callback_cap_frames: BALANCED_VIRTUAL_QUEUE_TARGET_FRAMES,
                pipewire_latency_hint: "1024/48000",
            },
            MicLatencyProfile::Low => LatencyTuning {
                virtual_target_frames: LOW_VIRTUAL_QUEUE_TARGET_FRAMES,
                max_virtual_backlog_frames: LOW_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                max_mic_backlog_frames: LOW_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                callback_cap_frames: LOW_VIRTUAL_QUEUE_TARGET_FRAMES,
                pipewire_latency_hint: "512/48000",
            },
            MicLatencyProfile::Ultra => LatencyTuning {
                virtual_target_frames: ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES,
                max_virtual_backlog_frames: ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                max_mic_backlog_frames: ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES * 2,
                callback_cap_frames: ULTRA_VIRTUAL_QUEUE_TARGET_FRAMES,
                pipewire_latency_hint: "256/48000",
            },
        }
    }

    pub(super) fn local_output_target_samples(&self) -> usize {
        LOCAL_OUTPUT_QUEUE_TARGET_FRAMES * TARGET_OUTPUT_CHANNELS as usize
    }

    pub(super) fn virtual_output_target_samples(&self) -> usize {
        self.latency_tuning().virtual_target_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    pub(super) fn max_fill_batches_per_tick(
        &self,
        wants_local_output: bool,
        wants_virtual_output: bool,
    ) -> usize {
        let mut target_frames = 0usize;
        if wants_local_output {
            target_frames = target_frames.max(LOCAL_OUTPUT_QUEUE_TARGET_FRAMES);
        }
        if wants_virtual_output {
            target_frames = target_frames.max(self.latency_tuning().virtual_target_frames);
        }

        (target_frames / MIX_CHUNK_FRAMES).max(1)
    }

    pub(super) fn max_virtual_callback_samples(&self) -> usize {
        self.latency_tuning().callback_cap_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    pub(super) fn max_virtual_backlog_samples(&self) -> usize {
        self.latency_tuning().max_virtual_backlog_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    pub(super) fn max_mic_backlog_samples(&self) -> usize {
        self.latency_tuning().max_mic_backlog_frames * TARGET_OUTPUT_CHANNELS as usize
    }

    pub(super) fn pipewire_latency_hint(&self) -> &'static str {
        self.latency_tuning().pipewire_latency_hint
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LatencyTuning {
    pub(super) virtual_target_frames: usize,
    pub(super) max_virtual_backlog_frames: usize,
    pub(super) max_mic_backlog_frames: usize,
    pub(super) callback_cap_frames: usize,
    pub(super) pipewire_latency_hint: &'static str,
}

#[derive(Debug)]
pub(super) struct StreamRuntimeShared {
    max_virtual_callback_samples: AtomicUsize,
    max_virtual_backlog_samples: AtomicUsize,
    max_mic_backlog_samples: AtomicUsize,
    local_underruns: AtomicU64,
    virtual_underruns: AtomicU64,
    lock_contention: AtomicU64,
}

impl StreamRuntimeShared {
    pub(super) fn new(runtime: &RuntimeConfig) -> Self {
        let tuning = runtime.latency_tuning();
        Self {
            max_virtual_callback_samples: AtomicUsize::new(
                tuning.callback_cap_frames * TARGET_OUTPUT_CHANNELS as usize,
            ),
            max_virtual_backlog_samples: AtomicUsize::new(
                tuning.max_virtual_backlog_frames * TARGET_OUTPUT_CHANNELS as usize,
            ),
            max_mic_backlog_samples: AtomicUsize::new(
                tuning.max_mic_backlog_frames * TARGET_OUTPUT_CHANNELS as usize,
            ),
            local_underruns: AtomicU64::new(0),
            virtual_underruns: AtomicU64::new(0),
            lock_contention: AtomicU64::new(0),
        }
    }

    pub(super) fn apply_runtime(&self, runtime: &RuntimeConfig) {
        self.max_virtual_callback_samples
            .store(runtime.max_virtual_callback_samples(), Ordering::Relaxed);
        self.max_virtual_backlog_samples
            .store(runtime.max_virtual_backlog_samples(), Ordering::Relaxed);
        self.max_mic_backlog_samples
            .store(runtime.max_mic_backlog_samples(), Ordering::Relaxed);
    }

    pub(super) fn max_virtual_callback_samples(&self) -> usize {
        self.max_virtual_callback_samples
            .load(Ordering::Relaxed)
            .max(TARGET_OUTPUT_CHANNELS as usize)
    }

    pub(super) fn max_virtual_backlog_samples(&self) -> usize {
        self.max_virtual_backlog_samples
            .load(Ordering::Relaxed)
            .max(TARGET_OUTPUT_CHANNELS as usize)
    }

    pub(super) fn max_mic_backlog_samples(&self) -> usize {
        self.max_mic_backlog_samples
            .load(Ordering::Relaxed)
            .max(TARGET_OUTPUT_CHANNELS as usize)
    }

    pub(super) fn record_local_underrun(&self) {
        self.local_underruns.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_virtual_underrun(&self) {
        self.virtual_underruns.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn record_lock_contention(&self) {
        self.lock_contention.fetch_add(1, Ordering::Relaxed);
    }

    pub(super) fn snapshot_counters(&self) -> (u64, u64, u64) {
        (
            self.local_underruns.load(Ordering::Relaxed),
            self.virtual_underruns.load(Ordering::Relaxed),
            self.lock_contention.load(Ordering::Relaxed),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutoGainMode {
    Static,
    DynamicLookAhead,
}

impl AutoGainMode {
    pub(super) fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::DynamicLookAhead,
            _ => Self::Static,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AutoGainApplyTo {
    Both,
    MicOnly,
}

impl AutoGainApplyTo {
    pub(super) fn from_u32(value: u32) -> Self {
        match value {
            1 => Self::MicOnly,
            _ => Self::Both,
        }
    }

    pub(super) fn applies_to_output(self, is_virtual_output: bool) -> bool {
        match self {
            Self::Both => true,
            Self::MicOnly => is_virtual_output,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AutoGainDynamicParams {
    pub(super) lookahead_ms: u32,
    pub(super) attack_ms: u32,
    pub(super) release_ms: u32,
}

#[derive(Debug, Clone)]
pub(super) struct AutoGainState {
    pub(super) enabled: bool,
    pub(super) mode: AutoGainMode,
    pub(super) apply_to: AutoGainApplyTo,
    pub(super) target_lufs: f64,
    pub(super) dynamic: AutoGainDynamicParams,
}

impl AutoGainState {
    pub(super) fn from_config(config: &crate::config::Config) -> Self {
        let auto_gain = config.settings.auto_gain_domain();
        Self {
            enabled: auto_gain.enabled,
            mode: AutoGainMode::from_u32(auto_gain.mode.player_value()),
            apply_to: AutoGainApplyTo::from_u32(auto_gain.apply_to.player_value()),
            target_lufs: auto_gain.target_lufs,
            dynamic: AutoGainDynamicParams {
                lookahead_ms: auto_gain.lookahead_ms,
                attack_ms: auto_gain.attack_ms,
                release_ms: auto_gain.release_ms,
            },
        }
    }

    pub(super) fn gain_for(
        &self,
        sound_lufs: Option<f64>,
        sound_true_peak_dbtp: Option<f32>,
        is_virtual_output: bool,
    ) -> f32 {
        if !self.enabled || !self.apply_to.applies_to_output(is_virtual_output) {
            return 1.0;
        }
        match sound_lufs {
            Some(lufs) => {
                let true_peak = match self.mode {
                    AutoGainMode::Static => sound_true_peak_dbtp,
                    AutoGainMode::DynamicLookAhead => None,
                };
                crate::audio::loudness::compute_gain_factor(lufs, self.target_lufs, true_peak)
            }
            None => 1.0,
        }
    }
}
