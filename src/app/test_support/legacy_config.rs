use serde::Serialize;

use crate::config::{Settings, Sound, SoundTab};

#[derive(Debug, Clone, Serialize)]
pub struct LegacyConfigFixture {
    pub schema_version: u32,
    pub sound_folders: Vec<String>,
    pub sounds: Vec<Sound>,
    pub tabs: Vec<SoundTab>,
    pub settings: Settings,
}

impl Default for LegacyConfigFixture {
    fn default() -> Self {
        Self {
            schema_version: crate::config::LAST_LEGACY_SCHEMA_VERSION,
            sound_folders: Vec::new(),
            sounds: Vec::new(),
            tabs: Vec::new(),
            settings: Settings::default(),
        }
    }
}
