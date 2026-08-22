//! Pre-schema-8 config shape, for building migration fixtures.
//!
//! Runtime `Config` dropped the library when SQLite took over, but files on
//! disk still carry `sounds`, `tabs` and `sound_folders`. Migration tests have
//! to write those, so the old shape lives here instead of in the type the app
//! actually loads.

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
