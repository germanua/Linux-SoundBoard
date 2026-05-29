//! The playback dispatch surface the `commands` layer drives.
//!
//! `commands::playback` does not need a concrete [`AudioPlayer`]; it only needs
//! something it can tell to play, stop, seek, and query. Depending on this
//! trait instead of the concrete type lets tests substitute a double and assert
//! what was dispatched (e.g. "after `play_sound(\"x\")` the engine was told to
//! play `x`") without a live audio backend.
//!
//! The trait is deliberately narrow: it covers only the playback transport, not
//! mic/source routing or auto-gain configuration. Those remain on the concrete
//! `AudioPlayer` because their command handlers are exercised through config
//! state, not engine dispatch.

use super::player::{AudioPlayer, EngineError, PlaybackPosition};

/// Playback transport operations invoked by `commands::playback`.
pub trait PlaybackEngine: Send + Sync {
    fn play(
        &self,
        sound_id: &str,
        path: &str,
        base_volume: f32,
        sound_lufs: Option<f64>,
        sound_true_peak_dbtp: Option<f32>,
    ) -> Result<String, EngineError>;
    fn stop_sound(&self, sound_id: &str) -> Result<(), EngineError>;
    fn stop_all(&self);
    fn seek_playback(&self, play_id: &str, position_ms: u64);
    fn pause(&self, sound_id: &str);
    fn resume(&self, sound_id: &str);
    fn get_playing(&self) -> Vec<String>;
    fn get_playback_positions(&self) -> Vec<PlaybackPosition>;
}

/// The production implementation: forward to the inherent methods on
/// [`AudioPlayer`]. Kept as thin pass-throughs so the trait adds an injection
/// seam without changing any runtime behavior.
impl PlaybackEngine for AudioPlayer {
    fn play(
        &self,
        sound_id: &str,
        path: &str,
        base_volume: f32,
        sound_lufs: Option<f64>,
        sound_true_peak_dbtp: Option<f32>,
    ) -> Result<String, EngineError> {
        AudioPlayer::play(
            self,
            sound_id,
            path,
            base_volume,
            sound_lufs,
            sound_true_peak_dbtp,
        )
    }

    fn stop_sound(&self, sound_id: &str) -> Result<(), EngineError> {
        AudioPlayer::stop_sound(self, sound_id)
    }

    fn stop_all(&self) {
        AudioPlayer::stop_all(self)
    }

    fn seek_playback(&self, play_id: &str, position_ms: u64) {
        AudioPlayer::seek_playback(self, play_id, position_ms)
    }

    fn pause(&self, sound_id: &str) {
        AudioPlayer::pause(self, sound_id)
    }

    fn resume(&self, sound_id: &str) {
        AudioPlayer::resume(self, sound_id)
    }

    fn get_playing(&self) -> Vec<String> {
        AudioPlayer::get_playing(self)
    }

    fn get_playback_positions(&self) -> Vec<PlaybackPosition> {
        AudioPlayer::get_playback_positions(self)
    }
}
