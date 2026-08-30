use super::{
    estimated_loudness_refinement_trigger, fast_loudness_preview_budget_ms,
    missing_loudness_analysis_trigger, EstimatedLoudnessRefinementTrigger,
    MissingLoudnessAnalysisTrigger, FAST_LUFS_PREVIEW_TOTAL_MS_LONG,
    FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM,
};

#[test]
fn fast_loudness_preview_budget_uses_medium_default_without_duration_hint() {
    assert_eq!(
        fast_loudness_preview_budget_ms(None),
        FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM
    );
}

#[test]
fn fast_loudness_preview_budget_uses_medium_budget_for_medium_tracks() {
    assert_eq!(
        fast_loudness_preview_budget_ms(Some(60_000)),
        FAST_LUFS_PREVIEW_TOTAL_MS_MEDIUM
    );
}

#[test]
fn fast_loudness_preview_budget_uses_long_budget_for_long_tracks() {
    assert_eq!(
        fast_loudness_preview_budget_ms(Some(120_000)),
        FAST_LUFS_PREVIEW_TOTAL_MS_LONG
    );
}

#[test]
fn estimated_refinement_trigger_skips_without_candidates() {
    assert_eq!(
        estimated_loudness_refinement_trigger(true, false, false, false),
        EstimatedLoudnessRefinementTrigger::SkippedNoCandidates
    );
}

#[test]
fn estimated_refinement_trigger_skips_when_auto_gain_disabled() {
    assert_eq!(
        estimated_loudness_refinement_trigger(false, true, false, false),
        EstimatedLoudnessRefinementTrigger::SkippedAutoGainDisabled
    );
}

#[test]
fn estimated_refinement_trigger_starts_with_force_even_if_auto_gain_disabled() {
    assert_eq!(
        estimated_loudness_refinement_trigger(false, true, true, false),
        EstimatedLoudnessRefinementTrigger::Started
    );
}

#[test]
fn missing_loudness_analysis_skips_when_auto_gain_disabled() {
    assert_eq!(
        missing_loudness_analysis_trigger(false, true, false, false),
        MissingLoudnessAnalysisTrigger::SkippedAutoGainDisabled
    );
}

#[test]
fn missing_loudness_analysis_skips_when_no_sounds_need_backfill() {
    assert_eq!(
        missing_loudness_analysis_trigger(true, false, false, false),
        MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds
    );
}

#[test]
fn missing_loudness_analysis_skips_when_job_already_running() {
    assert_eq!(
        missing_loudness_analysis_trigger(true, true, false, true),
        MissingLoudnessAnalysisTrigger::SkippedAlreadyRunning
    );
}

#[test]
fn missing_loudness_analysis_force_mode_bypasses_auto_gain_setting() {
    assert_eq!(
        missing_loudness_analysis_trigger(false, true, true, false),
        MissingLoudnessAnalysisTrigger::Started
    );
}

mod dispatch {
    use std::sync::Arc;

    use crate::audio::PlaybackEngine;
    use crate::commands::CommandError;
    use crate::config::{LoudnessAnalysisState, Sound};
    use crate::library_store::{
        LibraryBatch, LibraryScope, LibraryStore, LoudnessUpdate, SoundRecord,
    };
    use crate::test_support::audio_mock::FakeAudioPlayer;

    struct ClipOnDisk(std::path::PathBuf);

    impl Drop for ClipOnDisk {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sound_on_disk(name: &str) -> (Sound, ClipOnDisk, String) {
        let dir = std::env::temp_dir().join(format!("lsb-dispatch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let path = dir.join("clip.wav");
        std::fs::write(&path, b"placeholder - only needs to exist on disk").expect("write clip");
        let path = path.to_string_lossy().to_string();
        (
            Sound::new(name.to_string(), path.clone()),
            ClipOnDisk(dir),
            path,
        )
    }

    /// A library holding `sounds`, each already bound to `chord`.
    fn library_with_shared_chord(
        dir: &std::path::Path,
        sounds: &[Sound],
        chord: &str,
    ) -> LibraryStore {
        let store = LibraryStore::open(dir.join("library.sqlite3")).expect("open library");
        store
            .apply_batch(LibraryBatch::Sounds(
                sounds
                    .iter()
                    .enumerate()
                    .map(|(position, sound)| SoundRecord {
                        sound: sound.clone(),
                        general_position: position,
                        locations: Vec::new(),
                    })
                    .collect(),
            ))
            .recv()
            .expect("seed sounds");
        for sound in sounds {
            store
                .set_hotkey_binding(crate::library_store::HotkeyBindingRecord {
                    binding_id: sound.id.clone(),
                    owner: crate::library_store::HotkeyBindingOwner::Sound(sound.id.clone()),
                    accelerator: chord.to_string(),
                    normalized: Some(chord.to_string()),
                    issue: None,
                    tab_scope: None,
                })
                .recv()
                .expect("bind the chord");
        }
        store
    }

    fn press(multi_sound: bool, mode: crate::config::GroupMode) -> super::super::HotkeyPress {
        super::super::HotkeyPress {
            toggles: crate::hotkeys::HotkeyToggles {
                tab_hotkeys: false,
                multi_sound,
            },
            mode,
            active_scope: crate::app_meta::GENERAL_TAB_ID.to_string(),
            cursor: Arc::new(parking_lot::Mutex::new(std::collections::HashMap::new())),
        }
    }

    #[test]
    fn an_unshared_chord_plays_its_own_sound() {
        let (sound, dir, _path) = sound_on_disk("Alone");
        let sound_id = sound.id.clone();
        let store = library_with_shared_chord(&dir.0, std::slice::from_ref(&sound), "Ctrl+KeyA");
        let fake = Arc::new(FakeAudioPlayer::new());

        super::super::play_sound_from_library(
            &sound_id,
            &super::super::SoundLookup::HotkeyBinding(press(false, crate::config::GroupMode::Same)),
            &store,
            fake.clone() as Arc<dyn PlaybackEngine>,
        )
        .expect("an ordinary binding is a group of one");

        fake.assert_played(&sound_id);
    }

    #[test]
    fn a_shared_chord_plays_one_of_its_sounds() {
        let (first, dir, _path) = sound_on_disk("First");
        let (second, _second_dir, _second_path) = sound_on_disk("Second");
        let first_id = first.id.clone();
        let store = library_with_shared_chord(&dir.0, &[first, second], "Ctrl+KeyA");
        let fake = Arc::new(FakeAudioPlayer::new());

        super::super::play_sound_from_library(
            &first_id,
            &super::super::SoundLookup::HotkeyBinding(press(true, crate::config::GroupMode::Same)),
            &store,
            fake.clone() as Arc<dyn PlaybackEngine>,
        )
        .expect("a shared chord plays its first member");

        fake.assert_played(&first_id);
    }

    #[test]
    fn next_walks_the_shared_chord_one_sound_at_a_time() {
        let (first, dir, _path) = sound_on_disk("First");
        let (second, _second_dir, _second_path) = sound_on_disk("Second");
        let first_id = first.id.clone();
        let second_id = second.id.clone();
        let store = library_with_shared_chord(&dir.0, &[first, second], "Ctrl+KeyA");
        let fake = Arc::new(FakeAudioPlayer::new());
        let context =
            super::super::SoundLookup::HotkeyBinding(press(true, crate::config::GroupMode::Next));

        for _ in 0..2 {
            super::super::play_sound_from_library(
                &first_id,
                &context,
                &store,
                fake.clone() as Arc<dyn PlaybackEngine>,
            )
            .expect("play a member");
        }

        let played: Vec<String> = fake
            .play_calls()
            .into_iter()
            .map(|call| call.sound_id)
            .collect();
        assert_eq!(played, [first_id, second_id]);
    }

    #[test]
    fn continue_from_sound_id_plays_the_following_sound() {
        let (first, dir, _first_path) = sound_on_disk("First");
        let (second, _second_dir, _second_path) = sound_on_disk("Second");
        let (third, _third_dir, _third_path) = sound_on_disk("Third");
        let second_id = second.id.clone();
        let third_id = third.id.clone();
        let store = LibraryStore::open(dir.0.join("library.sqlite3")).expect("open library");
        store
            .apply_batch(LibraryBatch::Sounds(vec![
                SoundRecord {
                    sound: first,
                    general_position: 0,
                    locations: Vec::new(),
                },
                SoundRecord {
                    sound: second,
                    general_position: 1,
                    locations: Vec::new(),
                },
                SoundRecord {
                    sound: third,
                    general_position: 2,
                    locations: Vec::new(),
                },
            ]))
            .recv()
            .expect("seed sounds");
        let fake = Arc::new(FakeAudioPlayer::new());

        super::super::play_adjacent_from_sound_id_from_library(
            LibraryScope::General,
            "",
            &second_id,
            1,
            &store,
            fake.clone() as Arc<dyn PlaybackEngine>,
        )
        .expect("continue from second sound");

        let played: Vec<String> = fake
            .play_calls()
            .into_iter()
            .map(|call| call.sound_id)
            .collect();
        assert_eq!(played, [third_id]);
    }

    #[test]
    fn continue_from_last_sound_wraps_to_first_sound() {
        let (first, dir, _first_path) = sound_on_disk("First");
        let (second, _second_dir, _second_path) = sound_on_disk("Second");
        let (third, _third_dir, _third_path) = sound_on_disk("Third");
        let first_id = first.id.clone();
        let third_id = third.id.clone();
        let store = LibraryStore::open(dir.0.join("library.sqlite3")).expect("open library");
        store
            .apply_batch(LibraryBatch::Sounds(vec![
                SoundRecord {
                    sound: first,
                    general_position: 0,
                    locations: Vec::new(),
                },
                SoundRecord {
                    sound: second,
                    general_position: 1,
                    locations: Vec::new(),
                },
                SoundRecord {
                    sound: third,
                    general_position: 2,
                    locations: Vec::new(),
                },
            ]))
            .recv()
            .expect("seed sounds");
        let fake = Arc::new(FakeAudioPlayer::new());

        super::super::play_adjacent_from_sound_id_from_library(
            LibraryScope::General,
            "",
            &third_id,
            1,
            &store,
            fake.clone() as Arc<dyn PlaybackEngine>,
        )
        .expect("wrap Continue playback");

        let played: Vec<String> = fake
            .play_calls()
            .into_iter()
            .map(|call| call.sound_id)
            .collect();
        assert_eq!(played, [first_id]);
    }

    #[test]
    fn a_shared_chord_stays_silent_while_multiple_sounds_are_off() {
        let (first, dir, _path) = sound_on_disk("First");
        let (second, _second_dir, _second_path) = sound_on_disk("Second");
        let first_id = first.id.clone();
        let store = library_with_shared_chord(&dir.0, &[first, second], "Ctrl+KeyA");
        let fake = Arc::new(FakeAudioPlayer::new());

        let error = super::super::play_sound_from_library(
            &first_id,
            &super::super::SoundLookup::HotkeyBinding(press(false, crate::config::GroupMode::Same)),
            &store,
            fake.clone() as Arc<dyn PlaybackEngine>,
        )
        .expect_err("guessing between two sounds is worse than doing nothing");

        assert!(error.to_string().contains("Multiple sounds per hotkey"));
        fake.assert_no_plays();
    }

    #[test]
    fn dispatches_play_with_resolved_volume_after_stopping_everything() {
        let (mut sound, _dir, path) = sound_on_disk("Airhorn");
        sound.volume = 50;
        let sound_id = sound.id.clone();
        let fake = Arc::new(FakeAudioPlayer::new());

        let play_id =
            super::super::play_resolved_sound(sound, fake.clone() as Arc<dyn PlaybackEngine>)
                .expect("an enabled sound that exists should play");

        fake.assert_played(&sound_id);
        let calls = fake.play_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].path, path);
        // 50% sound volume must reach the engine as a 0.5 base gain.
        assert_eq!(calls[0].base_volume, 0.5);
        assert!(play_id.starts_with("fake-play-"));
        assert_eq!(fake.stop_all_calls(), 1);
    }

    #[test]
    fn library_playback_uses_loudness_written_after_the_row_was_loaded() {
        let (sound, dir, _path) = sound_on_disk("Analyzed later");
        let sound_id = sound.id.clone();
        let stale_loaded_sound = sound.clone();
        let store = LibraryStore::open(dir.0.join("library.sqlite3")).expect("open library");
        store
            .apply_batch(LibraryBatch::Sounds(vec![SoundRecord {
                sound,
                general_position: 0,
                locations: Vec::new(),
            }]))
            .recv()
            .expect("seed sound");
        store
            .apply_loudness_updates(vec![LoudnessUpdate {
                sound_id: sound_id.clone(),
                lufs: Some(-20.0),
                state: LoudnessAnalysisState::Refined,
                confidence: Some(1.0),
                true_peak_dbtp: Some(-1.5),
            }])
            .recv()
            .expect("store loudness");
        assert_eq!(stale_loaded_sound.loudness_lufs, None);

        let fake = Arc::new(FakeAudioPlayer::new());
        super::super::play_sound_from_library(
            &sound_id,
            &super::super::SoundLookup::ById,
            &store,
            fake.clone() as Arc<dyn PlaybackEngine>,
        )
        .expect("play current library row");

        let calls = fake.play_calls();
        assert_eq!(calls[0].sound_lufs, Some(-20.0));
        assert_eq!(calls[0].sound_true_peak_dbtp, Some(-1.5));
    }

    #[test]
    fn disabled_sound_does_not_reach_the_engine() {
        let (mut sound, _dir, _path) = sound_on_disk("Disabled");
        sound.enabled = false;
        let fake = Arc::new(FakeAudioPlayer::new());

        let result =
            super::super::play_resolved_sound(sound, fake.clone() as Arc<dyn PlaybackEngine>);

        assert!(matches!(result, Err(CommandError::SoundDisabled)));
        fake.assert_no_plays();
        assert_eq!(fake.stop_all_calls(), 0);
    }

    #[test]
    fn missing_source_does_not_reach_the_engine() {
        let mut sound = Sound::new("Gone".to_string(), "/nonexistent/clip.wav".to_string());
        sound.enabled = true;
        let fake = Arc::new(FakeAudioPlayer::new());

        let result =
            super::super::play_resolved_sound(sound, fake.clone() as Arc<dyn PlaybackEngine>);

        assert!(matches!(result, Err(CommandError::SourceUnavailable(_))));
        fake.assert_no_plays();
        assert_eq!(fake.stop_all_calls(), 0);
    }
}
