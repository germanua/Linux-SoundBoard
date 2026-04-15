pub fn init_player(config: &crate::config::Config) -> crate::audio::player::AudioPlayer {
    crate::audio::player::AudioPlayer::new_with_config(config)
}

#[derive(Debug, Clone)]
pub struct VolumeConfig {
    pub local_volume: f32,
    pub mic_volume: f32,
    pub local_muted: bool,
}

impl From<&crate::config::Config> for VolumeConfig {
    fn from(config: &crate::config::Config) -> Self {
        let volume = config.settings.volume_domain();
        Self {
            local_volume: volume.local_volume as f32 / 100.0,
            mic_volume: volume.mic_volume as f32 / 100.0,
            local_muted: volume.local_mute,
        }
    }
}
