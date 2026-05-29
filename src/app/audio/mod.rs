pub(crate) mod analysis_worker;
pub(crate) mod command_runner;
pub(crate) mod engine_ipc;
pub(crate) mod engine_server;
pub(crate) mod file_link;
pub(crate) mod legacy_cleanup;
pub(crate) mod loudness;
#[cfg(test)]
pub(crate) mod loudness_acceptance;
pub(crate) mod metadata;
pub(crate) mod pipewire_detection;
pub(crate) mod playback_engine;
pub(crate) mod player;
pub(crate) mod scanner;

pub use loudness::LoudnessError;
pub use playback_engine::PlaybackEngine;
pub use player::{
    AudioBackendKind, AudioPlayer, AudioSourceInfo, EngineError, PlaybackPosition, PlayerSnapshot,
};
