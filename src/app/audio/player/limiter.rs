use super::AutoGainDynamicParams;

/// Limiter gain-update cadence in interleaved sample frames. 256 frames ≈ 5.3 ms
/// at 48 kHz stereo — fast enough to track transients, slow enough that the
/// per-frame branch overhead is fully amortized.
const LIMITER_UPDATE_INTERVAL_FRAMES: usize = 256;

/// Limiter ceiling in linear gain (≈ −0.18 dBFS). Leaves ~2 % headroom for
/// inter-sample peaks that the digital domain cannot observe directly.
const LIMITER_TARGET_PEAK: f32 = 0.98;

pub(super) struct LookAheadLimiter {
    buffer: Vec<f32>,
    write_idx: usize,
    filled: usize,
    current_gain: f32,
    target_gain: f32,
    attack_samples: usize,
    release_samples: usize,
    update_interval: usize,
    update_counter: usize,
    target_peak: f32,
}

impl LookAheadLimiter {
    pub(super) fn new(sample_rate: u32, channels: u16, params: AutoGainDynamicParams) -> Self {
        let samples_per_ms = (sample_rate as f32 * channels as f32) / 1000.0;
        let lookahead_samples = (params.lookahead_ms as f32 * samples_per_ms)
            .round()
            .max(1.0) as usize;
        let attack_samples = (params.attack_ms as f32 * samples_per_ms).round().max(1.0) as usize;
        let release_samples = (params.release_ms as f32 * samples_per_ms).round().max(1.0) as usize;
        Self {
            buffer: vec![0.0; lookahead_samples + 1],
            write_idx: 0,
            filled: 0,
            current_gain: 1.0,
            target_gain: 1.0,
            attack_samples,
            release_samples,
            update_interval: LIMITER_UPDATE_INTERVAL_FRAMES,
            update_counter: 0,
            target_peak: LIMITER_TARGET_PEAK,
        }
    }

    pub(super) fn process(&mut self, sample: f32) -> f32 {
        self.buffer[self.write_idx] = sample;
        self.write_idx = (self.write_idx + 1) % self.buffer.len();
        if self.filled < self.buffer.len() {
            self.filled += 1;
        }

        self.update_counter += 1;
        if self.filled < self.buffer.len() || self.update_counter >= self.update_interval {
            self.update_counter = 0;
            let peak = self
                .buffer
                .iter()
                .take(self.filled)
                .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
            self.target_gain = if peak > 0.0 {
                (self.target_peak / peak).min(1.0)
            } else {
                1.0
            };
        }

        if self.target_gain < self.current_gain {
            let step = (self.current_gain - self.target_gain) / self.attack_samples as f32;
            self.current_gain = (self.current_gain - step).max(self.target_gain);
        } else if self.target_gain > self.current_gain {
            let step = (self.target_gain - self.current_gain) / self.release_samples as f32;
            self.current_gain = (self.current_gain + step).min(self.target_gain);
        }

        // Keep dynamic warmup click-free by passing audio through immediately.
        sample * self.current_gain
    }

    // No flush(): this limiter is a sliding peak-analysis window, not a delay
    // line. Every sample is returned immediately by process(), so there is
    // nothing buffered to drain on source exhaustion.
}
