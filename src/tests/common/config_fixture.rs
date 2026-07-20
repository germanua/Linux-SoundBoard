use std::sync::Arc;

use parking_lot::Mutex;

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

    pub fn with_generated_sounds(mut self, count: usize) -> Self {
        self.config.sounds.reserve(count);
        for index in 0..count {
            let mut sound = linux_soundboard::config::Sound::new(
                format!("Звук {index:06} — Sound collection item"),
                format!(
                    "/home/test/Музика/Collection {:02}/Album {:03}/Disc {:02}/track-{index:06}.flac",
                    index % 24,
                    index % 512,
                    index % 4,
                ),
            );
            sound.id = format!("sound-{index:06}");
            self.config.sounds.push(sound);
        }
        self
    }

    pub fn with_partitioned_tabs(mut self, count: usize) -> Self {
        if count == 0 {
            return self;
        }

        let mut tabs = (0..count)
            .map(|index| {
                let mut tab = linux_soundboard::config::SoundTab::new(
                    format!("Manual {index:02}"),
                    index as u32,
                );
                tab.id = format!("manual-{index:02}");
                tab
            })
            .collect::<Vec<_>>();
        for (index, sound) in self.config.sounds.iter().enumerate() {
            tabs[index % count].sound_ids.push(sound.id.clone());
        }
        self.config.tabs.extend(tabs);
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
