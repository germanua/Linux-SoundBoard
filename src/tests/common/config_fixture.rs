use std::sync::{Arc, Mutex};

pub struct ConfigBuilder {
    config: linux_soundboard::config::Config,
}

impl ConfigBuilder {
    pub fn new() -> Self {
        Self {
            config: linux_soundboard::config::Config::default(),
        }
    }

    pub fn with_sound(mut self, name: &str, path: &str) -> Self {
        let sound = linux_soundboard::config::Sound::new(name.to_string(), path.to_string());
        self.config.sounds.push(sound);
        self
    }

    pub fn with_tab(mut self, name: &str) -> Self {
        let tab = linux_soundboard::config::SoundTab::new(
            name.to_string(),
            self.config.tabs.len() as u32,
        );
        self.config.tabs.push(tab);
        self
    }

    pub fn build(self) -> Arc<Mutex<linux_soundboard::config::Config>> {
        Arc::new(Mutex::new(self.config))
    }
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}
