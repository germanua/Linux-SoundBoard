//! A `PlaybackEngine` test double.
//!
//! `FakeAudioPlayer` records what the `commands` layer dispatches and keeps just
//! enough playback state for query-based commands (e.g. `seek_sound`, which
//! looks up the active play id for a sound). Tests inject it in place of the
//! real `AudioPlayer` and assert on what was dispatched, with no audio backend.

use parking_lot::Mutex;
use std::collections::HashMap;

use crate::audio::{EngineError, PlaybackEngine, PlaybackPosition};

/// One `play` dispatch, captured verbatim so tests can assert the engine was
/// told to play the right sound with the right volume/loudness metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayCall {
    pub sound_id: String,
    pub path: String,
    pub base_volume: f32,
    pub sound_lufs: Option<f64>,
    pub sound_true_peak_dbtp: Option<f32>,
}

#[derive(Default)]
struct Recorder {
    plays: Vec<PlayCall>,
    /// Active (and finished) playbacks keyed by the play id `play` handed out.
    positions: HashMap<String, PlaybackPosition>,
    stopped_sounds: Vec<String>,
    seeks: Vec<(String, u64)>,
    stop_all_calls: usize,
    next_play_seq: u64,
}

pub struct FakeAudioPlayer {
    recorder: Mutex<Recorder>,
}

impl FakeAudioPlayer {
    pub fn new() -> Self {
        Self {
            recorder: Mutex::new(Recorder::default()),
        }
    }

    // --- assertion / inspection helpers ---

    pub fn play_calls(&self) -> Vec<PlayCall> {
        self.recorder.lock().plays.clone()
    }

    pub fn assert_played(&self, sound_id: &str) {
        let plays = self.recorder.lock();
        assert!(
            plays.plays.iter().any(|call| call.sound_id == sound_id),
            "expected sound {sound_id:?} to be played, got {:?}",
            plays.plays
        );
    }

    pub fn assert_no_plays(&self) {
        let plays = self.recorder.lock();
        assert!(
            plays.plays.is_empty(),
            "expected no plays, got {:?}",
            plays.plays
        );
    }

    pub fn stop_all_calls(&self) -> usize {
        self.recorder.lock().stop_all_calls
    }
}

impl Default for FakeAudioPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackEngine for FakeAudioPlayer {
    fn play(
        &self,
        sound_id: &str,
        path: &str,
        base_volume: f32,
        sound_lufs: Option<f64>,
        sound_true_peak_dbtp: Option<f32>,
    ) -> Result<String, EngineError> {
        let mut rec = self.recorder.lock();
        rec.plays.push(PlayCall {
            sound_id: sound_id.to_string(),
            path: path.to_string(),
            base_volume,
            sound_lufs,
            sound_true_peak_dbtp,
        });
        rec.next_play_seq += 1;
        let play_id = format!("fake-play-{}", rec.next_play_seq);
        rec.positions.insert(
            play_id.clone(),
            PlaybackPosition {
                play_id: play_id.clone(),
                sound_id: sound_id.to_string(),
                position_ms: 0,
                paused: false,
                finished: false,
                duration_ms: Some(1000),
            },
        );
        Ok(play_id)
    }

    fn stop_sound(&self, sound_id: &str) -> Result<(), EngineError> {
        let mut rec = self.recorder.lock();
        rec.stopped_sounds.push(sound_id.to_string());
        rec.positions
            .retain(|_, position| position.sound_id != sound_id);
        Ok(())
    }

    fn stop_all(&self) {
        let mut rec = self.recorder.lock();
        rec.stop_all_calls += 1;
        rec.positions.clear();
    }

    fn seek_playback(&self, play_id: &str, position_ms: u64) {
        let mut rec = self.recorder.lock();
        rec.seeks.push((play_id.to_string(), position_ms));
        if let Some(position) = rec.positions.get_mut(play_id) {
            position.position_ms = position_ms;
        }
    }

    fn pause(&self, sound_id: &str) {
        let mut rec = self.recorder.lock();
        for position in rec.positions.values_mut() {
            if position.sound_id == sound_id {
                position.paused = true;
            }
        }
    }

    fn resume(&self, sound_id: &str) {
        let mut rec = self.recorder.lock();
        for position in rec.positions.values_mut() {
            if position.sound_id == sound_id {
                position.paused = false;
            }
        }
    }

    fn get_playing(&self) -> Vec<String> {
        self.recorder
            .lock()
            .positions
            .values()
            .filter(|position| !position.finished)
            .map(|position| position.sound_id.clone())
            .collect()
    }

    fn get_playback_positions(&self) -> Vec<PlaybackPosition> {
        self.recorder.lock().positions.values().cloned().collect()
    }
}
