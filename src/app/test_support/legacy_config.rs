//! Pre-schema-8 configuration shape, for building migration fixtures.
//!
//! The runtime `Config` stopped carrying the library when SQLite became
//! authoritative, but legacy files on disk still contain `sounds`, `tabs` and
//! `sound_folders`. Migration tests need to write those files, so the old shape
//! lives here as a test-only serializable struct rather than in the type the
//! application loads.

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
