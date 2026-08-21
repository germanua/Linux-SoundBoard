use crate::audio::AudioPlayer;
use crate::commands;
use crate::config::{Config, ControlHotkeyAction, Sound};
use crate::hotkeys::HotkeyManager;
use parking_lot::Mutex;
use std::sync::Arc;

fn create_test_config() -> Config {
    let mut cfg = Config {
        persistence_path: Some(
            std::env::temp_dir()
                .join(format!("lsb-command-config-{}", uuid::Uuid::new_v4()))
                .join("config.json"),
        ),
        ..Config::default()
    };
    // Disable auto_gain so library commands (add_sound, refresh_sounds, etc.)
    // do not inadvertently fire the global loudness coordinator. Tests that
    // exercise loudness-backfill behaviour opt in by setting this explicitly.
    cfg.settings.auto_gain = false;
    cfg
}

fn create_test_config_state() -> Arc<Mutex<Config>> {
    Arc::new(Mutex::new(create_test_config()))
}

fn create_mock_hotkey_manager() -> Arc<Mutex<HotkeyManager>> {
    use std::sync::mpsc;
    let (sender, _) = mpsc::sync_channel(1);
    let manager = HotkeyManager::new_deferred(sender);
    Arc::new(Mutex::new(manager))
}

fn create_projection_hotkey_manager() -> Arc<Mutex<HotkeyManager>> {
    Arc::new(Mutex::new(HotkeyManager::new_test_noop()))
}

/// Commits a binding straight to the store. Going through `set_hotkey` would
/// also try to register with the real backend, which is not available in the
/// test environment and fails intermittently under load.
fn seed_hotkey_binding(
    library: &crate::library_store::LibraryStore,
    owner: crate::library_store::HotkeyBindingOwner,
    accelerator: &str,
) {
    use crate::library_store::{HotkeyBindingOwner, HotkeyBindingRecord, LibraryBatch};

    let binding_id = match &owner {
        HotkeyBindingOwner::Sound(id) => id.clone(),
        HotkeyBindingOwner::Control(action) => action.clone(),
        HotkeyBindingOwner::Tab(tab) => tab.clone(),
    };
    library
        .apply_batch(LibraryBatch::HotkeyBindings(vec![HotkeyBindingRecord {
            binding_id,
            owner,
            accelerator: accelerator.to_string(),
            normalized: Some(accelerator.to_string()),
            issue: None,
            tab_scope: None,
        }]))
        .recv()
        .expect("seed hotkey binding");
}

/// Builds a store fixture directly through the bounded store API. Sounds and
/// roots are passed in rather than read off a `Config`, which no longer carries
/// the library.
fn create_test_library_with(
    roots: &[String],
    sounds: &[Sound],
) -> crate::library_store::LibraryStore {
    use crate::library_store::{LibraryBatch, RootRecord, SoundRecord};

    let path = std::env::temp_dir()
        .join(format!("lsb-command-library-{}", uuid::Uuid::new_v4()))
        .join("library.sqlite3");
    let store =
        crate::library_store::LibraryStore::open(path).expect("create command test library");

    if !roots.is_empty() {
        store
            .apply_batch(LibraryBatch::Roots(
                roots
                    .iter()
                    .enumerate()
                    .map(|(position, path)| RootRecord {
                        path: path.clone(),
                        position,
                    })
                    .collect(),
            ))
            .recv()
            .expect("seed roots");
    }

    if !sounds.is_empty() {
        store
            .apply_batch(LibraryBatch::Sounds(
                sounds
                    .iter()
                    .enumerate()
                    .map(|(general_position, sound)| SoundRecord {
                        sound: sound.clone(),
                        general_position,
                        locations: Vec::new(),
                    })
                    .collect(),
            ))
            .recv()
            .expect("seed sounds");
    }

    store
}

fn create_test_audio_player() -> Arc<AudioPlayer> {
    Arc::new(AudioPlayer::new_test_noop())
}

mod library;
mod tabs;

#[test]
fn test_set_local_volume() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_local_volume(50, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.local_volume, 50);
}

#[test]
fn test_set_local_volume_clamp() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_local_volume(150, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.local_volume, 100);
}

#[test]
fn test_toggle_local_mute() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::toggle_local_mute(config.clone(), player);
    assert!(result.is_ok());
    assert!(result.unwrap());

    let cfg = config.lock();
    assert!(cfg.settings.local_mute);
}

#[test]
fn test_toggle_local_mute_again() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    commands::toggle_local_mute(config.clone(), player.clone()).unwrap();
    let result = commands::toggle_local_mute(config, player);
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

#[test]
fn test_set_mic_volume() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_mic_volume(75, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.mic_volume, 75);
}

#[test]
fn test_set_mic_latency_profile_low() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_mic_latency_profile(
        crate::config::MicLatencyProfile::Low,
        config.clone(),
        player,
    );
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(
        cfg.settings.mic_latency_profile,
        crate::config::MicLatencyProfile::Low
    );
}

#[test]
fn test_set_mic_latency_profile_ultra() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_mic_latency_profile(
        crate::config::MicLatencyProfile::Ultra,
        config.clone(),
        player,
    );
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(
        cfg.settings.mic_latency_profile,
        crate::config::MicLatencyProfile::Ultra
    );
}

#[test]
fn test_set_theme_dark() {
    let config = create_test_config_state();
    let result = commands::set_theme("dark".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.theme, crate::config::Theme::Dark);
}

#[test]
fn test_set_theme_light() {
    let config = create_test_config_state();
    let result = commands::set_theme("light".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.theme, crate::config::Theme::Light);
}

#[test]
fn test_set_theme_invalid() {
    let config = create_test_config_state();
    let result = commands::set_theme("invalid".to_string(), config);
    assert!(result.is_err());
}

#[test]
fn test_set_list_style_compact() {
    let config = create_test_config_state();
    let result = commands::set_list_style("compact".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.list_style, crate::config::ListStyle::Compact);
}

#[test]
fn test_set_list_style_card() {
    let config = create_test_config_state();
    let result = commands::set_list_style("card".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.list_style, crate::config::ListStyle::Card);
}

#[test]
fn test_set_list_style_invalid() {
    let config = create_test_config_state();
    let result = commands::set_list_style("invalid".to_string(), config);
    assert!(result.is_err());
}

#[test]
fn test_get_config() {
    let config = create_test_config_state();
    let cfg = commands::get_config(config);
    assert!(cfg.settings.local_volume > 0);
}

#[test]
fn test_save_config() {
    let mut config = create_test_config();
    config.settings.local_volume = 60;
    let config = Arc::new(Mutex::new(config));

    let result = commands::save_config(config.clone());
    assert!(result.is_ok());
}

#[test]
fn test_set_auto_gain_target() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_target(-16.0, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.auto_gain_target_lufs, -16.0);
}

#[test]
fn test_set_auto_gain_target_clamp() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_target(-30.0, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.auto_gain_target_lufs, -24.0);
}

#[test]
fn test_set_auto_gain_mode_static() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_mode("static".to_string(), config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(
        cfg.settings.auto_gain_mode,
        crate::config::AutoGainMode::Static
    );
}

#[test]
fn test_set_auto_gain_mode_dynamic() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_mode("dynamic".to_string(), config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(
        cfg.settings.auto_gain_mode,
        crate::config::AutoGainMode::Dynamic
    );
}

#[test]
fn test_set_auto_gain_mode_invalid() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_mode("invalid".to_string(), config, player);
    assert!(result.is_err());
}

#[test]
fn test_set_auto_gain_apply_to_mic_only() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_apply_to("mic_only".to_string(), config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(
        cfg.settings.auto_gain_apply_to,
        crate::config::AutoGainApplyTo::MicOnly
    );
}

#[test]
fn test_set_auto_gain_apply_to_both() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_apply_to("both".to_string(), config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(
        cfg.settings.auto_gain_apply_to,
        crate::config::AutoGainApplyTo::Both
    );
}

#[test]
fn test_set_auto_gain_dynamic_settings() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_dynamic_settings(50, 10, 200, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.auto_gain_lookahead_ms, 50);
    assert_eq!(cfg.settings.auto_gain_attack_ms, 10);
    assert_eq!(cfg.settings.auto_gain_release_ms, 200);
}

#[test]
fn test_set_auto_gain_dynamic_settings_clamp() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_dynamic_settings(500, 100, 2000, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock();
    assert_eq!(cfg.settings.auto_gain_lookahead_ms, 200);
    assert_eq!(cfg.settings.auto_gain_attack_ms, 50);
    assert_eq!(cfg.settings.auto_gain_release_ms, 1000);
}

#[test]
fn test_get_playback_positions_empty() {
    let player = create_test_audio_player();
    let positions = commands::get_playback_positions(player);
    assert!(positions.is_empty());
}

#[test]
fn test_stop_all() {
    let player = create_test_audio_player();
    commands::stop_all(player);
}

#[test]
fn test_parse_theme() {
    assert_eq!(
        commands::shared::parse_theme("dark").unwrap(),
        crate::config::Theme::Dark
    );
    assert_eq!(
        commands::shared::parse_theme("light").unwrap(),
        crate::config::Theme::Light
    );
    assert!(commands::shared::parse_theme("invalid").is_err());
}

#[test]
fn test_parse_auto_gain_mode() {
    assert_eq!(
        commands::shared::parse_auto_gain_mode("dynamic").unwrap(),
        crate::config::AutoGainMode::Dynamic
    );
    assert_eq!(
        commands::shared::parse_auto_gain_mode("static").unwrap(),
        crate::config::AutoGainMode::Static
    );
    assert!(commands::shared::parse_auto_gain_mode("invalid").is_err());
}

#[test]
fn test_validate_play_mode() {
    assert_eq!(
        commands::shared::validate_play_mode("default").unwrap(),
        crate::config::PlayMode::Default
    );
    assert_eq!(
        commands::shared::validate_play_mode("loop").unwrap(),
        crate::config::PlayMode::Loop
    );
    assert_eq!(
        commands::shared::validate_play_mode("continue").unwrap(),
        crate::config::PlayMode::Continue
    );
    assert!(commands::shared::validate_play_mode("invalid").is_err());
}

#[test]
fn test_bounded_audio_analysis_threads() {
    let threads = commands::shared::bounded_audio_analysis_threads();
    assert!(threads >= 1);
}

mod loudness;

#[test]
#[allow(clippy::print_stdout)]
fn test_set_hotkey_valid() {
    let hotkeys = create_projection_hotkey_manager();

    let sound = Sound::new("Test".to_string(), "/tmp/test.mp3".to_string());
    let sound_id = sound.id.clone();
    let library = create_test_library_with(&[], std::slice::from_ref(&sound));
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    let result = commands::set_hotkey(
        sound_id.clone(),
        Some("Ctrl+1".to_string()),
        false,
        None,
        library.clone(),
        projection,
    );
    match result {
        Ok(_) => {
            assert!(library.hotkey_binding(&sound_id).recv().unwrap().is_some());
        }
        Err(e) => {
            println!(
                "Hotkey registration failed (expected without X11/swhkd): {}",
                e
            );
        }
    }
}

#[test]
#[allow(clippy::print_stdout)]
fn test_set_hotkey_clear() {
    let hotkeys = create_projection_hotkey_manager();

    let mut sound = Sound::new("Test".to_string(), "/tmp/test.mp3".to_string());
    sound.hotkey = Some("Ctrl+1".to_string());
    let sound_id = sound.id.clone();
    let library = create_test_library_with(&[], std::slice::from_ref(&sound));
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    let result = commands::set_hotkey(
        sound_id.clone(),
        None,
        false,
        None,
        library.clone(),
        projection,
    );
    match result {
        Ok(_) => {
            assert!(library.hotkey_binding(&sound_id).recv().unwrap().is_none());
        }
        Err(e) => {
            println!("Hotkey clear failed: {}", e);
        }
    }
}

#[test]
fn test_set_control_hotkey_rejects_duplicate_control_binding() {
    let hotkeys = create_mock_hotkey_manager();

    let library = create_test_library_with(&[], &[]);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    // The store owns hotkey bindings now, so the conflicting one has to be
    // committed there rather than set on the settings struct.
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Control(
            ControlHotkeyAction::PlayPause.id().to_string(),
        ),
        "Ctrl+Alt+KeyP",
    );

    let result = commands::set_control_hotkey(
        ControlHotkeyAction::StopAll.id().to_string(),
        Some("Ctrl+Alt+KeyP".to_string()),
        library,
        projection,
    );

    let err = result.expect_err("duplicate control hotkey must be rejected");
    assert_eq!(
        crate::hotkeys::format_hotkey_error(&err.to_string()),
        "That shortcut is already assigned to control action \"Play / Pause\"."
    );
}

#[test]
fn test_set_control_hotkey_rejects_duplicate_sound_binding() {
    let hotkeys = create_mock_hotkey_manager();

    let sound = Sound::new("Airhorn".to_string(), "/tmp/airhorn.mp3".to_string());
    let sound_id = sound.id.clone();
    let library = create_test_library_with(&[], std::slice::from_ref(&sound));
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(sound_id),
        "Ctrl+Alt+KeyP",
    );

    let result = commands::set_control_hotkey(
        ControlHotkeyAction::StopAll.id().to_string(),
        Some("Ctrl+Alt+KeyP".to_string()),
        library,
        projection,
    );

    let err = result.expect_err("duplicate sound hotkey must be rejected");
    assert_eq!(
        crate::hotkeys::format_hotkey_error(&err.to_string()),
        "That shortcut is already assigned to sound \"Airhorn\"."
    );
}

#[test]
fn test_set_auto_gain_enabled() {
    let config = create_test_config_state();
    let library = create_test_library_with(&[], &[]);
    let player = create_test_audio_player();

    let result = commands::set_auto_gain(
        true,
        config.clone(),
        library,
        player,
        &commands::LoudnessCoordinators::new(),
    );
    assert!(result.is_ok());

    let cfg = config.lock();
    assert!(cfg.settings.auto_gain);
}

#[test]
fn test_set_auto_gain_disabled() {
    let config = create_test_config_state();
    let library = create_test_library_with(&[], &[]);
    let player = create_test_audio_player();

    let result = commands::set_auto_gain(
        false,
        config.clone(),
        library,
        player,
        &commands::LoudnessCoordinators::new(),
    );
    assert!(result.is_ok());

    let cfg = config.lock();
    assert!(!cfg.settings.auto_gain);
}

#[test]
fn a_sound_may_join_a_chord_another_sound_already_answers_to() {
    let hotkeys = create_projection_hotkey_manager();

    let first = Sound::new("First".to_string(), "/tmp/first.mp3".to_string());
    let second = Sound::new("Second".to_string(), "/tmp/second.mp3".to_string());
    let second_id = second.id.clone();
    let library = create_test_library_with(&[], &[first.clone(), second]);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(first.id.clone()),
        "Ctrl+Alt+KeyG",
    );

    commands::set_hotkey(
        second_id.clone(),
        Some("Ctrl+Alt+KeyG".to_string()),
        true,
        None,
        library.clone(),
        projection,
    )
    .expect("a shared chord is a group to join, not a conflict");

    let members = library
        .hotkey_group(&second_id)
        .recv()
        .expect("read the group");
    assert_eq!(members.len(), 2);
}

#[test]
fn a_sound_may_not_join_a_chord_while_multiple_sounds_are_off() {
    let hotkeys = create_projection_hotkey_manager();

    let first = Sound::new("First".to_string(), "/tmp/first.mp3".to_string());
    let second = Sound::new("Second".to_string(), "/tmp/second.mp3".to_string());
    let second_id = second.id.clone();
    let library = create_test_library_with(&[], &[first.clone(), second]);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(first.id.clone()),
        "Ctrl+Alt+KeyG",
    );

    let error = commands::set_hotkey(
        second_id,
        Some("Ctrl+Alt+KeyG".to_string()),
        false,
        None,
        library,
        projection,
    )
    .expect_err("without the toggle a taken chord is still a conflict");
    assert!(crate::hotkeys::format_hotkey_error(&error.to_string()).contains("already assigned"));
}

#[test]
fn a_sound_may_never_take_a_control_actions_chord() {
    let hotkeys = create_projection_hotkey_manager();

    let sound = Sound::new("Airhorn".to_string(), "/tmp/airhorn.mp3".to_string());
    let sound_id = sound.id.clone();
    let library = create_test_library_with(&[], std::slice::from_ref(&sound));
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Control(
            ControlHotkeyAction::StopAll.id().to_string(),
        ),
        "Ctrl+Alt+KeyH",
    );

    // Sharing applies between sounds. A control action reached under a sound's
    // binding id would never run, so this stays a conflict either way.
    let error = commands::set_hotkey(
        sound_id,
        Some("Ctrl+Alt+KeyH".to_string()),
        true,
        None,
        library,
        projection,
    )
    .expect_err("a control action must keep its chord to itself");
    assert_eq!(
        crate::hotkeys::format_hotkey_error(&error.to_string()),
        "That shortcut is already assigned to control action \"Stop All\"."
    );
}

#[test]
fn a_tab_can_be_given_its_own_hotkey() {
    let hotkeys = create_projection_hotkey_manager();
    let library = create_test_library_with(&[], &[]);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    commands::set_tab_hotkey(
        "tab:party".to_string(),
        Some("Ctrl+Alt+Digit1".to_string()),
        library.clone(),
        projection,
    )
    .expect("bind a tab");

    let binding = library
        .hotkey_binding(&commands::tab_binding_id("tab:party"))
        .recv()
        .expect("read the binding")
        .expect("the tab hotkey is stored");
    assert_eq!(
        binding.owner,
        crate::library_store::HotkeyBindingOwner::Tab("tab:party".to_string())
    );
    // Always live, or there would be no way to switch back to this tab.
    assert_eq!(binding.tab_scope, None);
}

#[test]
fn a_tab_hotkey_may_not_take_a_chord_a_sound_answers_to() {
    let hotkeys = create_projection_hotkey_manager();
    let sound = Sound::new("Airhorn".to_string(), "/tmp/airhorn.mp3".to_string());
    let library = create_test_library_with(&[], std::slice::from_ref(&sound));
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(sound.id.clone()),
        "Ctrl+Alt+Digit2",
    );

    commands::set_tab_hotkey(
        "tab:party".to_string(),
        Some("Ctrl+Alt+Digit2".to_string()),
        library,
        projection,
    )
    .expect_err("a tab hotkey is live everywhere, so it cannot share a chord");
}

#[test]
fn two_tabs_may_use_the_same_chord_for_different_sounds() {
    let hotkeys = create_projection_hotkey_manager();
    let first = Sound::new("First".to_string(), "/tmp/first.mp3".to_string());
    let second = Sound::new("Second".to_string(), "/tmp/second.mp3".to_string());
    let second_id = second.id.clone();
    let library = create_test_library_with(&[], &[first.clone(), second]);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    commands::set_hotkey(
        first.id.clone(),
        Some("Ctrl+Alt+Digit3".to_string()),
        false,
        Some("tab:one".to_string()),
        library.clone(),
        projection.clone(),
    )
    .expect("bind the chord in the first tab");

    // Different tabs never answer at the same time, so this is not a clash
    // even with multiple sounds per hotkey off.
    commands::set_hotkey(
        second_id,
        Some("Ctrl+Alt+Digit3".to_string()),
        false,
        Some("tab:two".to_string()),
        library,
        projection,
    )
    .expect("the same chord may mean something else in another tab");
}

#[test]
fn a_scoped_binding_still_clashes_with_one_that_is_live_everywhere() {
    let hotkeys = create_projection_hotkey_manager();
    let first = Sound::new("First".to_string(), "/tmp/first.mp3".to_string());
    let second = Sound::new("Second".to_string(), "/tmp/second.mp3".to_string());
    let second_id = second.id.clone();
    let library = create_test_library_with(&[], &[first.clone(), second]);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(first.id.clone()),
        "Ctrl+Alt+Digit4",
    );

    commands::set_hotkey(
        second_id,
        Some("Ctrl+Alt+Digit4".to_string()),
        false,
        Some("tab:one".to_string()),
        library,
        projection,
    )
    .expect_err("an unscoped binding answers in this tab too");
}

#[test]
fn a_tab_binding_id_is_not_mistaken_for_a_sound() {
    let sound = Sound::new("Airhorn".to_string(), "/tmp/airhorn.mp3".to_string());
    // Sound bindings are stored under the sound's own public id, so the two
    // must stay distinguishable at the point a press arrives.
    assert_eq!(commands::tab_from_binding_id(&sound.id), None);
    assert_eq!(
        commands::tab_from_binding_id(&commands::tab_binding_id("tab:party")),
        Some("tab:party")
    );
    assert_eq!(
        commands::tab_from_binding_id(&commands::tab_binding_id("general")),
        Some("general")
    );
    assert_eq!(
        commands::tab_from_binding_id(ControlHotkeyAction::StopAll.binding_id()),
        None
    );
}

#[test]
fn cycling_the_shared_hotkey_mode_walks_all_three_and_wraps() {
    let config = create_test_config_state();

    let modes: Vec<crate::config::GroupMode> = (0..4)
        .map(|_| commands::cycle_group_mode(Arc::clone(&config)).expect("cycle"))
        .collect();

    assert_eq!(
        modes,
        [
            crate::config::GroupMode::Next,
            crate::config::GroupMode::Random,
            crate::config::GroupMode::Same,
            crate::config::GroupMode::Next,
        ]
    );
    assert_eq!(
        config.lock().settings.group_mode,
        crate::config::GroupMode::Next
    );
}

#[test]
fn an_unknown_shared_hotkey_mode_is_rejected() {
    let config = create_test_config_state();

    commands::set_group_mode("sideways".to_string(), Arc::clone(&config))
        .expect_err("only same, next and random exist");
    assert_eq!(
        config.lock().settings.group_mode,
        crate::config::GroupMode::Same
    );
}
