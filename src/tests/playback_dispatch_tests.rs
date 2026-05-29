//! Engine-dispatch coverage for the playback command layer.
//!
//! These tests inject a `FakeAudioPlayer` (a `PlaybackEngine` double) in place
//! of the real `AudioPlayer` and assert *what the command dispatched* — the
//! behavior the ~1,500 lines of config-only command tests never reached.

mod common;

use std::sync::Arc;

use parking_lot::Mutex;

use common::{ConfigBuilder, FakeAudioPlayer, PlayCall, TempConfigDir};
use linux_soundboard::commands::{self, CommandError};
use linux_soundboard::config::Config;

/// One enabled sound whose source file actually exists on disk (so `play_sound`
/// gets past its existence guard). The temp dir is removed when the fixture
/// drops.
struct SoundFixture {
    config: Arc<Mutex<Config>>,
    sound_id: String,
    path: String,
    _dir: TempConfigDir,
}

fn fixture_with_sound(name: &str) -> SoundFixture {
    let dir = TempConfigDir::new();
    let path = dir.path().join("clip.wav");
    std::fs::write(&path, b"placeholder - only needs to exist on disk").expect("write temp clip");
    let path = path.to_string_lossy().to_string();

    let config = ConfigBuilder::new().with_sound(name, &path).build();
    let sound_id = config.lock().sounds[0].id.clone();

    SoundFixture {
        config,
        sound_id,
        path,
        _dir: dir,
    }
}

#[test]
fn play_sound_dispatches_play_with_resolved_volume() {
    let fixture = fixture_with_sound("Airhorn");
    // 50% sound volume must reach the engine as a 0.5 base gain.
    fixture.config.lock().sounds[0].volume = 50;

    let fake = Arc::new(FakeAudioPlayer::new());
    let play_id = commands::play_sound(
        fixture.sound_id.clone(),
        fixture.config.clone(),
        fake.clone(),
    )
    .expect("play_sound should succeed for an enabled, existing sound");

    fake.assert_played(&fixture.sound_id);
    let calls: Vec<PlayCall> = fake.play_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].sound_id, fixture.sound_id);
    assert_eq!(calls[0].path, fixture.path);
    assert_eq!(calls[0].base_volume, 0.5);
    assert!(play_id.starts_with("fake-play-"));
    // play_sound stops everything before starting the new clip.
    assert_eq!(fake.stop_all_calls(), 1);
}

#[test]
fn play_sound_disabled_does_not_dispatch() {
    let fixture = fixture_with_sound("Disabled");
    fixture.config.lock().sounds[0].enabled = false;

    let fake = Arc::new(FakeAudioPlayer::new());
    let err = commands::play_sound(
        fixture.sound_id.clone(),
        fixture.config.clone(),
        fake.clone(),
    )
    .expect_err("disabled sound must not play");

    assert_eq!(err, CommandError::SoundDisabled);
    fake.assert_no_plays();
}

#[test]
fn play_sound_missing_source_does_not_dispatch() {
    // No file on disk for this path.
    let config = ConfigBuilder::new()
        .with_sound("Gone", "/nonexistent/path/clip.wav")
        .build();
    let sound_id = config.lock().sounds[0].id.clone();

    let fake = Arc::new(FakeAudioPlayer::new());
    let err = commands::play_sound(sound_id, config, fake.clone())
        .expect_err("missing source must not play");

    assert!(matches!(err, CommandError::SourceUnavailable(_)));
    fake.assert_no_plays();
}

#[test]
fn stop_all_dispatches_to_engine() {
    let fake = Arc::new(FakeAudioPlayer::new());
    commands::stop_all(fake.clone());
    assert_eq!(fake.stop_all_calls(), 1);
}

#[test]
fn stop_sound_dispatches_sound_id_to_engine() {
    let fake = Arc::new(FakeAudioPlayer::new());
    commands::stop_sound("sound-xyz".to_string(), fake.clone()).expect("stop_sound ok");
    assert_eq!(fake.stopped_sounds(), vec!["sound-xyz".to_string()]);
}

#[test]
fn seek_sound_dispatches_seek_for_the_active_play() {
    let fixture = fixture_with_sound("Seekable");
    let fake = Arc::new(FakeAudioPlayer::new());

    commands::play_sound(
        fixture.sound_id.clone(),
        fixture.config.clone(),
        fake.clone(),
    )
    .expect("play_sound ok");
    commands::seek_sound(fixture.sound_id.clone(), 5_000, fake.clone()).expect("seek_sound ok");

    // play_sound handed out "fake-play-1"; seek_sound must resolve the active
    // play for the sound and forward the position.
    assert_eq!(fake.seeks(), vec![("fake-play-1".to_string(), 5_000)]);
}

#[test]
fn pause_then_resume_dispatch_to_engine() {
    let fixture = fixture_with_sound("Pausable");
    let fake = Arc::new(FakeAudioPlayer::new());

    commands::play_sound(
        fixture.sound_id.clone(),
        fixture.config.clone(),
        fake.clone(),
    )
    .expect("play_sound ok");

    commands::pause_sound(fixture.sound_id.clone(), fake.clone());
    assert!(fake.is_paused(&fixture.sound_id));

    commands::resume_sound(fixture.sound_id.clone(), fake.clone());
    assert!(!fake.is_paused(&fixture.sound_id));
}
