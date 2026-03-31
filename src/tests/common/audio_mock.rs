//! Fake audio player for headless integration testing.
//! Does NOT require PulseAudio or any audio system.

use std::collections::HashMap;
use std::sync::Mutex;

/// Fake playback position for testing
#[derive(Debug, Clone)]
pub struct FakePlaybackPosition {
    pub play_id: String,
    pub sound_id: String,
    pub position_ms: u64,
    pub paused: bool,
    pub finished: bool,
    pub duration_ms: Option<u64>,
}

/// Fake audio player for headless testing
pub struct FakeAudioPlayer {
    plays: Mutex<Vec<(String, String)>>, // (sound_id, path)
    positions: Mutex<HashMap<String, FakePlaybackPosition>>,
}

impl FakeAudioPlayer {
    pub fn new() -> Self {
        Self {
            plays: Mutex::new(Vec::new()),
            positions: Mutex::new(HashMap::new()),
        }
    }

    pub fn play(
        &self,
        sound_id: &str,
        path: &str,
        _base_volume: f32,
        _sound_lufs: Option<f64>,
    ) -> Result<String, String> {
        let play_id = format!("fake-{}", uuid::Uuid::new_v4());
        self.plays
            .lock()
            .unwrap()
            .push((sound_id.to_string(), path.to_string()));
        self.positions.lock().unwrap().insert(
            play_id.clone(),
            FakePlaybackPosition {
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

    pub fn stop_all(&self) {
        self.plays.lock().unwrap().clear();
        self.positions.lock().unwrap().clear();
    }

    pub fn get_playback_positions(&self) -> Vec<FakePlaybackPosition> {
        self.positions.lock().unwrap().values().cloned().collect()
    }

    pub fn assert_played(&self, sound_id: &str) {
        let plays = self.plays.lock().unwrap();
        assert!(
            plays.iter().any(|(id, _)| id == sound_id),
            "Expected sound {} to be played",
            sound_id
        );
    }

    pub fn assert_no_plays(&self) {
        let plays = self.plays.lock().unwrap();
        assert!(
            plays.is_empty(),
            "Expected no plays but got {}",
            plays.len()
        );
    }
}

impl Default for FakeAudioPlayer {
    fn default() -> Self {
        Self::new()
    }
}
