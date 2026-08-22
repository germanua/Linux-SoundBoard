//! The playback surface the `commands` layer drives.
//!
//! `commands::playback` doesn't need a concrete [`AudioPlayer`], only something
//! it can tell to play, stop, seek and query. Going through the trait lets
//! tests swap in a double and assert on dispatches with no live backend.
//!
//! Narrow on purpose: transport only, no mic/source routing and no auto-gain.
//! Those stay on `AudioPlayer`, since their handlers are driven through config
//! state rather than engine dispatch.

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

/// Production impl: thin pass-throughs to [`AudioPlayer`]'s inherent methods,
/// so the trait buys an injection seam and nothing else.
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
