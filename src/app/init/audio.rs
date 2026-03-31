//! Audio initialization phase.

/// Initialize audio player with settings from config.
pub fn init_player(config: &crate::config::Config) -> crate::audio::player::AudioPlayer {
    let initial_local_volume = if config.settings.local_mute {
        0.0
    } else {
        config.settings.local_volume as f32 / 100.0
    };
    let initial_mic_volume = config.settings.mic_volume as f32 / 100.0;

    let player = crate::audio::player::AudioPlayer::new_with_initial_volumes(
        initial_local_volume,
        initial_mic_volume,
    );

    // Apply auto-gain settings
    player.set_auto_gain_enabled(config.settings.auto_gain);
    player.set_auto_gain_target(config.settings.auto_gain_target_lufs);
    player.set_auto_gain_mode(config.settings.auto_gain_mode.player_value());
    player.set_auto_gain_apply_to(config.settings.auto_gain_apply_to.player_value());
    player.set_auto_gain_dynamic_settings(
        config.settings.auto_gain_lookahead_ms,
        config.settings.auto_gain_attack_ms,
        config.settings.auto_gain_release_ms,
    );
    player.set_looping(config.settings.play_mode.should_loop());

    player
}

/// Volume configuration extracted from settings.
#[derive(Debug, Clone)]
pub struct VolumeConfig {
    pub local_volume: f32,
    pub mic_volume: f32,
    pub local_muted: bool,
}

impl From<&crate::config::Config> for VolumeConfig {
    fn from(config: &crate::config::Config) -> Self {
        Self {
            local_volume: config.settings.local_volume as f32 / 100.0,
            mic_volume: config.settings.mic_volume as f32 / 100.0,
            local_muted: config.settings.local_mute,
        }
    }
}
