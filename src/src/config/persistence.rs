//! Config persistence and sanitization helpers.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::config::defaults::{config_dir_name, default_auto_gain_target, CONFIG_FILE_NAME};
use crate::config::{Config, SoundTab};

impl Config {
    pub fn config_path() -> PathBuf {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(config_dir_name());

        let _ = fs::create_dir_all(&config_dir);
        config_dir.join(CONFIG_FILE_NAME)
    }

    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let path = Self::config_path();
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            let mut config: Config = serde_json::from_str(&content)?;
            config.sanitize_for_persistence();
            Ok(config)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<(), Box<dyn std::error::Error>> {
        let path = Self::config_path();
        let mut sanitized = self.clone();
        sanitized.sanitize_for_persistence();
        let content = serde_json::to_string_pretty(&sanitized)?;
        fs::write(path, content)?;
        Ok(())
    }

    pub fn add_sound_folder(&mut self, folder: String) {
        if !self.sound_folders.contains(&folder) {
            self.sound_folders.push(folder);
        }
    }

    pub fn remove_sound_folder(&mut self, folder: &str) {
        self.sound_folders.retain(|f| f != folder);
    }

    pub fn add_sound(&mut self, sound: crate::config::Sound) {
        if !self.sounds.iter().any(|s| s.path == sound.path) {
            self.sounds.push(sound);
        }
    }

    pub fn remove_sound(&mut self, id: &str) {
        self.remove_sounds(&[id.to_string()]);
    }

    pub fn remove_sounds(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }

        let remove_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        self.sounds
            .retain(|sound| !remove_set.contains(sound.id.as_str()));
        for tab in &mut self.tabs {
            tab.sound_ids
                .retain(|sound_id| !remove_set.contains(sound_id.as_str()));
        }
    }

    pub fn get_sound(&self, id: &str) -> Option<&crate::config::Sound> {
        self.sounds.iter().find(|s| s.id == id)
    }

    pub fn get_sound_mut(&mut self, id: &str) -> Option<&mut crate::config::Sound> {
        self.sounds.iter_mut().find(|s| s.id == id)
    }

    pub fn set_hotkey(&mut self, id: &str, hotkey: Option<String>) {
        if let Some(sound) = self.get_sound_mut(id) {
            sound.hotkey = hotkey;
        }
    }

    pub fn set_sound_name(&mut self, id: &str, name: String) {
        if let Some(sound) = self.get_sound_mut(id) {
            sound.name = name;
        }
    }

    pub fn sanitize_for_persistence(&mut self) {
        for sound in &mut self.sounds {
            if sound.source_path.is_none() {
                sound.source_path = Some(sound.path.clone());
            }
            if matches!(sound.loudness_lufs, Some(v) if !v.is_finite()) {
                log::warn!(
                    "Dropping non-finite loudness for sound '{}' [{}]",
                    sound.name,
                    sound.path
                );
                sound.loudness_lufs = None;
            }
        }

        if !self.settings.auto_gain_target_lufs.is_finite() {
            self.settings.auto_gain_target_lufs = default_auto_gain_target();
        }
        self.settings.auto_gain_target_lufs = self.settings.auto_gain_target_lufs.clamp(-24.0, 0.0);
        self.settings.allow_multiple_playbacks = false;
    }

    pub fn create_tab(&mut self, name: String) -> SoundTab {
        let order = self.tabs.iter().map(|t| t.order).max().unwrap_or(0) + 1;
        let tab = SoundTab::new(name, order);
        self.tabs.push(tab.clone());
        tab
    }

    pub fn rename_tab(&mut self, id: &str, name: String) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.name = name;
            true
        } else {
            false
        }
    }

    pub fn delete_tab(&mut self, id: &str) -> bool {
        let len_before = self.tabs.len();
        self.tabs.retain(|t| t.id != id);
        self.tabs.len() < len_before
    }

    pub fn get_tab(&self, id: &str) -> Option<&SoundTab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_tab_mut(&mut self, id: &str) -> Option<&mut SoundTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn add_sounds_to_tab(&mut self, tab_id: &str, sound_ids: Vec<String>) -> bool {
        if let Some(tab) = self.get_tab_mut(tab_id) {
            for sound_id in sound_ids {
                if !tab.sound_ids.contains(&sound_id) {
                    tab.sound_ids.push(sound_id);
                }
            }
            true
        } else {
            false
        }
    }

    pub fn remove_sound_from_tab(&mut self, tab_id: &str, sound_id: &str) -> bool {
        if let Some(tab) = self.get_tab_mut(tab_id) {
            let len_before = tab.sound_ids.len();
            tab.sound_ids.retain(|id| id != sound_id);
            tab.sound_ids.len() < len_before
        } else {
            false
        }
    }

    pub fn remove_sounds_from_tab(&mut self, tab_id: &str, sound_ids: &[String]) -> bool {
        let Some(tab) = self.get_tab_mut(tab_id) else {
            return false;
        };

        if sound_ids.is_empty() {
            return true;
        }

        let remove_set: HashSet<&str> = sound_ids.iter().map(String::as_str).collect();
        tab.sound_ids
            .retain(|sound_id| !remove_set.contains(sound_id.as_str()));
        true
    }
}
