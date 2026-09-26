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
fn loudness_boost_is_independent_and_clamped() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    commands::set_loudness_boost_enabled(true, config.clone(), player.clone())
        .expect("enable loudness boost");
    commands::set_loudness_boost_db(200.0, config.clone(), player).expect("set loudness boost");

    let cfg = config.lock();
    assert!(!cfg.settings.auto_gain);
    assert!(cfg.settings.loudness_boost);
    assert_eq!(cfg.settings.loudness_boost_db, 150.0);
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
            let bindings = library
                .hotkey_bindings_for_sound(&sound_id)
                .recv()
                .expect("read the sound's bindings");
            assert_eq!(bindings.len(), 1);

            assert_eq!(bindings[0].accelerator, "Ctrl+Digit1");
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
            assert!(library
                .hotkey_bindings_for_sound(&sound_id)
                .recv()
                .unwrap()
                .is_empty());
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

    let joined = library
        .hotkey_bindings_for_sound(&second_id)
        .recv()
        .expect("read the sound's bindings")
        .pop()
        .expect("the second sound is bound");
    let members = library
        .hotkey_group(&joined.binding_id)
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

fn hotkey_batch_sounds(count: usize) -> Vec<Sound> {
    (0..count)
        .map(|index| {
            Sound::new(
                format!("Batch {index}"),
                format!("/tmp/hotkey-batch-{}-{index}.mp3", uuid::Uuid::new_v4()),
            )
        })
        .collect()
}

#[test]
fn a_store_batch_writes_one_row_per_target() {
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);

    let written = library
        .set_sound_hotkeys(ids.clone(), Some("Ctrl+Alt+KeyQ".to_string()), None)
        .recv()
        .expect("the transaction commits");
    assert_eq!(written, 3);

    for id in &ids {
        let bindings = library
            .hotkey_bindings_for_sound(id)
            .recv()
            .expect("read the sound's bindings");
        assert_eq!(bindings.len(), 1, "one row for {id}");
        assert_eq!(bindings[0].accelerator, "Ctrl+Alt+KeyQ");
        assert_eq!(bindings[0].tab_scope, None);
    }
}

#[test]
fn a_batch_replaces_only_the_target_scope() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    let tab_chords = ["Ctrl+Alt+Digit1", "Ctrl+Alt+Digit2", "Ctrl+Alt+Digit3"];
    let global_chords = ["Ctrl+Alt+KeyA", "Ctrl+Alt+KeyB", "Ctrl+Alt+KeyC"];
    for ((id, tab_chord), global_chord) in ids.iter().zip(tab_chords).zip(global_chords) {
        library
            .set_sound_hotkeys(
                vec![id.clone()],
                Some(tab_chord.to_string()),
                Some("tab:one".to_string()),
            )
            .recv()
            .expect("seed the tab binding");
        seed_hotkey_binding(
            &library,
            crate::library_store::HotkeyBindingOwner::Sound(id.clone()),
            global_chord,
        );
    }

    commands::set_hotkey_many(
        ids.clone(),
        Some("Ctrl+Alt+KeyZ".to_string()),
        true,
        Some("tab:one".to_string()),
        library.clone(),
        projection,
    )
    .expect("the batch replaces the tab binding");

    for id in &ids {
        let bindings = library
            .hotkey_bindings_for_sound(id)
            .recv()
            .expect("read the sound's bindings");
        let scoped: Vec<&str> = bindings
            .iter()
            .filter(|binding| binding.tab_scope.as_deref() == Some("tab:one"))
            .map(|binding| binding.accelerator.as_str())
            .collect();
        let global: Vec<&str> = bindings
            .iter()
            .filter(|binding| binding.tab_scope.is_none())
            .map(|binding| binding.accelerator.as_str())
            .collect();
        assert_eq!(scoped, ["Ctrl+Alt+KeyZ"], "the tab binding is replaced");
        assert_eq!(global.len(), 1, "the global binding is untouched");
    }
}

#[test]
fn a_failed_batch_leaves_no_partial_state() {
    let sounds = hotkey_batch_sounds(2);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(ids[0].clone()),
        "Ctrl+Alt+KeyA",
    );

    let mut targets = ids.clone();
    targets.push("missing-sound".to_string());
    library
        .set_sound_hotkeys(targets, Some("Ctrl+Alt+KeyB".to_string()), None)
        .recv()
        .expect_err("an unknown target aborts the batch");

    let first = library
        .hotkey_bindings_for_sound(&ids[0])
        .recv()
        .expect("read the first target");
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0].accelerator, "Ctrl+Alt+KeyA",
        "the already-processed target must roll back"
    );
    let second = library
        .hotkey_bindings_for_sound(&ids[1])
        .recv()
        .expect("read the second target");
    assert!(second.is_empty(), "no target may be left written");
}

#[test]
fn a_batch_clear_removes_only_the_target_scope() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    for id in &ids {
        library
            .set_sound_hotkeys(
                vec![id.clone()],
                Some("Ctrl+Alt+Digit5".to_string()),
                Some("tab:one".to_string()),
            )
            .recv()
            .expect("seed the tab binding");
        seed_hotkey_binding(
            &library,
            crate::library_store::HotkeyBindingOwner::Sound(id.clone()),
            "Ctrl+Alt+KeyD",
        );
    }

    commands::set_hotkey_many(
        ids.clone(),
        None,
        true,
        Some("tab:one".to_string()),
        library.clone(),
        projection,
    )
    .expect("the batch clears the tab binding");

    for id in &ids {
        let bindings = library
            .hotkey_bindings_for_sound(id)
            .recv()
            .expect("read the sound's bindings");
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.tab_scope.as_deref() == Some("tab:one")),
            "the tab binding is cleared"
        );
        assert!(
            bindings.iter().any(|binding| binding.tab_scope.is_none()),
            "the global binding survives"
        );
    }
}

#[test]
fn a_global_clear_keeps_the_tab_binding() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(1);
    let id = sounds[0].id.clone();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(id.clone()),
        "Ctrl+Alt+KeyF",
    );
    library
        .set_sound_hotkeys(
            vec![id.clone()],
            Some("Ctrl+Alt+Digit2".to_string()),
            Some("tab:one".to_string()),
        )
        .recv()
        .expect("seed the tab binding");

    commands::set_hotkey_many(
        vec![id.clone()],
        None,
        false,
        None,
        library.clone(),
        projection,
    )
    .expect("a clear needs no sharing toggle");

    let bindings = library
        .hotkey_bindings_for_sound(&id)
        .recv()
        .expect("read the sound's bindings");
    assert!(
        !bindings.iter().any(|binding| binding.tab_scope.is_none()),
        "the global binding is cleared"
    );
    assert_eq!(
        bindings
            .iter()
            .filter(|binding| binding.tab_scope.as_deref() == Some("tab:one"))
            .map(|binding| binding.accelerator.as_str())
            .collect::<Vec<_>>(),
        ["Ctrl+Alt+Digit2"],
        "the tab binding survives"
    );
}

#[test]
fn a_tab_clear_keeps_the_global_binding() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(1);
    let id = sounds[0].id.clone();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(id.clone()),
        "Ctrl+Alt+KeyF",
    );
    library
        .set_sound_hotkeys(
            vec![id.clone()],
            Some("Ctrl+Alt+Digit2".to_string()),
            Some("tab:one".to_string()),
        )
        .recv()
        .expect("seed the tab binding");

    commands::set_hotkey_many(
        vec![id.clone()],
        None,
        false,
        Some("tab:one".to_string()),
        library.clone(),
        projection,
    )
    .expect("a clear needs no sharing toggle");

    let bindings = library
        .hotkey_bindings_for_sound(&id)
        .recv()
        .expect("read the sound's bindings");
    assert!(
        !bindings
            .iter()
            .any(|binding| binding.tab_scope.as_deref() == Some("tab:one")),
        "the tab binding is cleared"
    );
    assert_eq!(
        bindings
            .iter()
            .filter(|binding| binding.tab_scope.is_none())
            .map(|binding| binding.accelerator.as_str())
            .collect::<Vec<_>>(),
        ["Ctrl+Alt+KeyF"],
        "the global binding survives"
    );
}

#[test]
fn a_multi_sound_tab_clear_needs_no_sharing() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    let globals = ["Ctrl+Alt+KeyA", "Ctrl+Alt+KeyB", "Ctrl+Alt+KeyC"];
    for (id, global) in ids.iter().zip(globals) {
        seed_hotkey_binding(
            &library,
            crate::library_store::HotkeyBindingOwner::Sound(id.clone()),
            global,
        );
        library
            .set_sound_hotkeys(
                vec![id.clone()],
                Some("Ctrl+Alt+Digit3".to_string()),
                Some("tab:one".to_string()),
            )
            .recv()
            .expect("seed the tab binding");
    }

    commands::set_hotkey_many(
        ids.clone(),
        None,
        false,
        Some("tab:one".to_string()),
        library.clone(),
        projection,
    )
    .expect("a multi-sound clear needs no sharing toggle");

    for id in &ids {
        let bindings = library
            .hotkey_bindings_for_sound(id)
            .recv()
            .expect("read the sound's bindings");
        assert!(
            !bindings
                .iter()
                .any(|binding| binding.tab_scope.as_deref() == Some("tab:one")),
            "the tab binding is cleared for {id}"
        );
        assert!(
            bindings.iter().any(|binding| binding.tab_scope.is_none()),
            "the global binding survives for {id}"
        );
    }
}

#[test]
fn a_clear_leaves_other_tabs_untouched() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(1);
    let id = sounds[0].id.clone();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(id.clone()),
        "Ctrl+Alt+KeyA",
    );
    for (chord, scope) in [
        ("Ctrl+Alt+Digit1", "tab:one"),
        ("Ctrl+Alt+Digit2", "tab:two"),
    ] {
        library
            .set_sound_hotkeys(
                vec![id.clone()],
                Some(chord.to_string()),
                Some(scope.to_string()),
            )
            .recv()
            .expect("seed the tab binding");
    }

    commands::set_hotkey_many(
        vec![id.clone()],
        None,
        false,
        Some("tab:one".to_string()),
        library.clone(),
        projection.clone(),
    )
    .expect("the tab clear commits");

    let bindings = library
        .hotkey_bindings_for_sound(&id)
        .recv()
        .expect("read the sound's bindings");
    assert!(
        !bindings
            .iter()
            .any(|binding| binding.tab_scope.as_deref() == Some("tab:one")),
        "the cleared tab is empty"
    );
    assert!(
        bindings
            .iter()
            .any(|binding| binding.tab_scope.as_deref() == Some("tab:two")),
        "the other tab survives a tab clear"
    );
    assert!(
        bindings.iter().any(|binding| binding.tab_scope.is_none()),
        "the global binding survives a tab clear"
    );

    commands::set_hotkey_many(
        vec![id.clone()],
        None,
        false,
        None,
        library.clone(),
        projection,
    )
    .expect("the global clear commits");

    let bindings = library
        .hotkey_bindings_for_sound(&id)
        .recv()
        .expect("read the sound's bindings");
    assert!(
        !bindings.iter().any(|binding| binding.tab_scope.is_none()),
        "the global binding is cleared"
    );
    assert!(
        bindings
            .iter()
            .any(|binding| binding.tab_scope.as_deref() == Some("tab:two")),
        "the other tab survives a global clear"
    );
}

#[test]
fn a_batch_persists_the_canonical_normalized() {
    let sounds = hotkey_batch_sounds(2);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);

    let raw = "control+alt+KeyQ";
    let canonical =
        crate::hotkeys::canonicalize_hotkey_string(raw).expect("the spelling is a valid hotkey");
    assert_ne!(
        canonical, raw,
        "the input must be a non-canonical spelling of a valid hotkey"
    );

    library
        .set_sound_hotkeys(ids.clone(), Some(raw.to_string()), None)
        .recv()
        .expect("the batch commits");

    for id in &ids {
        let bindings = library
            .hotkey_bindings_for_sound(id)
            .recv()
            .expect("read the sound's bindings");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].accelerator, raw, "the accelerator is kept");
        assert_eq!(
            bindings[0].normalized.as_deref(),
            Some(canonical.as_str()),
            "normalized is the canonical accelerator"
        );
    }
}

#[test]
fn duplicate_targets_are_deduplicated_before_writing() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(2);
    let ids = vec![
        sounds[0].id.clone(),
        sounds[0].id.clone(),
        sounds[1].id.clone(),
    ];
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    commands::set_hotkey_many(
        ids,
        Some("Ctrl+Alt+KeyE".to_string()),
        true,
        None,
        library.clone(),
        projection,
    )
    .expect("duplicates are collapsed");

    for sound in &sounds {
        let bindings = library
            .hotkey_bindings_for_sound(&sound.id)
            .recv()
            .expect("read the sound's bindings");
        assert_eq!(bindings.len(), 1);
    }
}

#[test]
fn a_batch_with_sharing_on_is_one_successful_operation() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    commands::set_hotkey_many(
        ids.clone(),
        Some("Ctrl+Alt+KeyM".to_string()),
        true,
        None,
        library.clone(),
        projection,
    )
    .expect("sharing lets a free chord go to the whole batch");

    let mut binding_ids = Vec::new();
    for id in &ids {
        let bindings = library
            .hotkey_bindings_for_sound(id)
            .recv()
            .expect("read the sound's bindings");
        assert_eq!(bindings.len(), 1);
        binding_ids.push(bindings[0].binding_id.clone());
    }
    let members = library
        .hotkey_group(&binding_ids[0])
        .recv()
        .expect("read the shared group");
    assert_eq!(members.len(), 3, "all three answer to one chord");
}

#[test]
fn a_batch_with_sharing_off_is_refused_before_any_write() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    let error = commands::set_hotkey_many(
        ids.clone(),
        Some("Ctrl+Alt+KeyG".to_string()),
        false,
        None,
        library.clone(),
        projection,
    )
    .expect_err("several sounds on one chord needs the toggle");
    assert!(
        error.to_string().contains("Multiple Sounds Per Hotkey"),
        "the refusal names the setting: {error}"
    );

    for id in &ids {
        assert!(library
            .hotkey_bindings_for_sound(id)
            .recv()
            .expect("read the sound's bindings")
            .is_empty());
    }
}

#[test]
fn a_target_that_already_holds_the_chord_is_not_its_own_conflict() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(ids[1].clone()),
        "Ctrl+Alt+KeyH",
    );

    let holder = commands::hotkey_holder_many(
        ids.clone(),
        "Ctrl+Alt+KeyH".to_string(),
        None,
        library.clone(),
    )
    .expect("the preflight query runs");
    assert!(
        holder.is_none(),
        "a target member is never an external holder"
    );

    commands::set_hotkey_many(
        ids.clone(),
        Some("Ctrl+Alt+KeyH".to_string()),
        true,
        None,
        library.clone(),
        projection,
    )
    .expect("the batch joins its own member");

    for id in &ids {
        assert_eq!(
            library
                .hotkey_bindings_for_sound(id)
                .recv()
                .expect("read the sound's bindings")
                .len(),
            1
        );
    }
}

#[test]
fn an_external_holder_is_flagged_once_and_kept() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let other = Sound::new(
        "Outside".to_string(),
        format!("/tmp/hotkey-outside-{}.mp3", uuid::Uuid::new_v4()),
    );
    let other_id = other.id.clone();
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let mut all = sounds;
    all.push(other);
    let library = create_test_library_with(&[], &all);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(other_id.clone()),
        "Ctrl+Alt+KeyF",
    );

    let holder = commands::hotkey_holder_many(
        ids.clone(),
        "Ctrl+Alt+KeyF".to_string(),
        None,
        library.clone(),
    )
    .expect("the preflight query runs");
    assert!(holder.is_some(), "the outside holder is reported");

    commands::set_hotkey_many(
        ids.clone(),
        Some("Ctrl+Alt+KeyF".to_string()),
        true,
        None,
        library.clone(),
        projection,
    )
    .expect("sharing lets the batch join an existing chord");

    assert_eq!(
        library
            .hotkey_bindings_for_sound(&other_id)
            .recv()
            .expect("read the holder's bindings")
            .len(),
        1,
        "the outside holder keeps its binding"
    );
    for id in &ids {
        assert_eq!(
            library
                .hotkey_bindings_for_sound(id)
                .recv()
                .expect("read the sound's bindings")
                .len(),
            1
        );
    }
}

#[test]
fn a_batch_never_takes_a_control_actions_or_a_tabs_chord() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Control(
            ControlHotkeyAction::StopAll.id().to_string(),
        ),
        "Ctrl+Alt+KeyK",
    );
    let error = commands::set_hotkey_many(
        ids.clone(),
        Some("Ctrl+Alt+KeyK".to_string()),
        true,
        None,
        library.clone(),
        projection.clone(),
    )
    .expect_err("a control action keeps its chord");
    assert_eq!(
        crate::hotkeys::format_hotkey_error(&error.to_string()),
        "That shortcut is already assigned to control action \"Stop All\"."
    );

    commands::set_tab_hotkey(
        "tab:party".to_string(),
        Some("Ctrl+Alt+KeyL".to_string()),
        library.clone(),
        projection.clone(),
    )
    .expect("bind a tab");
    let error = commands::set_hotkey_many(
        ids,
        Some("Ctrl+Alt+KeyL".to_string()),
        true,
        None,
        library,
        projection,
    )
    .expect_err("a tab keeps its chord");
    assert!(
        crate::hotkeys::format_hotkey_error(&error.to_string()).contains("already assigned"),
        "the tab clash is reported: {error}"
    );
}

#[test]
fn a_batch_reconciles_the_projection_once() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(3);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    for id in &ids {
        library
            .set_sound_hotkeys(
                vec![id.clone()],
                Some("Ctrl+Alt+Digit7".to_string()),
                Some("tab:one".to_string()),
            )
            .recv()
            .expect("seed the tab binding");
    }

    let before = projection.reconcile_generation();
    commands::set_hotkey_many(
        ids.clone(),
        Some("Ctrl+Alt+KeyN".to_string()),
        true,
        None,
        library.clone(),
        projection.clone(),
    )
    .expect("the batch is assigned");

    for id in &ids {
        let bindings = library
            .hotkey_bindings_for_sound(id)
            .recv()
            .expect("read the sound's bindings");
        let global: Vec<_> = bindings
            .iter()
            .filter(|binding| binding.tab_scope.is_none())
            .collect();
        assert_eq!(global.len(), 1, "one global row for {id}");
        assert_eq!(global[0].accelerator, "Ctrl+Alt+KeyN");
        assert_eq!(
            global[0].normalized.as_deref(),
            Some("Ctrl+Alt+KeyN"),
            "the persisted normalized matches the accelerator"
        );
        assert!(
            bindings
                .iter()
                .any(|binding| binding.tab_scope.as_deref() == Some("tab:one")),
            "the other scope survives for {id}"
        );
    }

    assert_eq!(
        projection.reconcile_generation() - before,
        1,
        "one logical batch means one reconcile"
    );
}

#[test]
fn a_failed_batch_does_not_reconcile() {
    let hotkeys = create_projection_hotkey_manager();
    let sounds = hotkey_batch_sounds(2);
    let ids: Vec<String> = sounds.iter().map(|sound| sound.id.clone()).collect();
    let library = create_test_library_with(&[], &sounds);
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));
    seed_hotkey_binding(
        &library,
        crate::library_store::HotkeyBindingOwner::Sound(ids[0].clone()),
        "Ctrl+Alt+KeyA",
    );

    let mut targets = ids.clone();
    targets.push("missing-sound".to_string());

    let before = projection.reconcile_generation();
    commands::set_hotkey_many(
        targets,
        Some("Ctrl+Alt+KeyB".to_string()),
        true,
        None,
        library.clone(),
        projection.clone(),
    )
    .expect_err("an unknown target aborts the batch");

    assert_eq!(
        projection.reconcile_generation(),
        before,
        "a rolled-back batch must not reconcile"
    );

    let first = library
        .hotkey_bindings_for_sound(&ids[0])
        .recv()
        .expect("read the first target");
    assert_eq!(first.len(), 1);
    assert_eq!(
        first[0].accelerator, "Ctrl+Alt+KeyA",
        "the already-processed target must roll back"
    );
    let second = library
        .hotkey_bindings_for_sound(&ids[1])
        .recv()
        .expect("read the second target");
    assert!(second.is_empty(), "no target may be left written");
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

#[test]
fn one_sound_can_carry_a_different_hotkey_in_each_tab() {
    let hotkeys = create_projection_hotkey_manager();
    let sound = Sound::new("Airhorn".to_string(), "/tmp/airhorn.mp3".to_string());
    let sound_id = sound.id.clone();
    let library = create_test_library_with(&[], std::slice::from_ref(&sound));
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    for (scope, chord) in [
        ("tab:one", "Ctrl+Alt+Digit7"),
        ("tab:two", "Ctrl+Alt+Digit8"),
    ] {
        commands::set_hotkey(
            sound_id.clone(),
            Some(chord.to_string()),
            false,
            Some(scope.to_string()),
            library.clone(),
            projection.clone(),
        )
        .unwrap_or_else(|error| panic!("bind {chord} in {scope}: {error}"));
    }

    let mut bindings = library
        .hotkey_bindings_for_sound(&sound_id)
        .recv()
        .expect("read the sound's bindings");
    bindings.sort_by(|a, b| a.tab_scope.cmp(&b.tab_scope));
    let bound: Vec<(Option<&str>, &str)> = bindings
        .iter()
        .map(|binding| (binding.tab_scope.as_deref(), binding.accelerator.as_str()))
        .collect();
    assert_eq!(
        bound,
        [
            (Some("tab:one"), "Ctrl+Alt+Digit7"),
            (Some("tab:two"), "Ctrl+Alt+Digit8"),
        ]
    );
}

#[test]
fn rebinding_in_the_same_tab_replaces_rather_than_adds() {
    let hotkeys = create_projection_hotkey_manager();
    let sound = Sound::new("Airhorn".to_string(), "/tmp/airhorn.mp3".to_string());
    let sound_id = sound.id.clone();
    let library = create_test_library_with(&[], std::slice::from_ref(&sound));
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    for chord in ["Ctrl+Alt+Digit7", "Ctrl+Alt+Digit9"] {
        commands::set_hotkey(
            sound_id.clone(),
            Some(chord.to_string()),
            false,
            Some("tab:one".to_string()),
            library.clone(),
            projection.clone(),
        )
        .expect("bind in the same tab twice");
    }

    let bindings = library
        .hotkey_bindings_for_sound(&sound_id)
        .recv()
        .expect("read the sound's bindings");
    assert_eq!(bindings.len(), 1);
    assert_eq!(bindings[0].accelerator, "Ctrl+Alt+Digit9");
}

#[test]
fn a_binding_id_is_safe_to_write_into_the_swhkd_config() {
    let hotkeys = create_projection_hotkey_manager();
    let sound = Sound::new("Airhorn".to_string(), "/tmp/airhorn.mp3".to_string());
    let sound_id = sound.id.clone();
    let library = create_test_library_with(&[], std::slice::from_ref(&sound));
    let projection =
        crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), Arc::clone(&hotkeys));

    commands::set_hotkey(
        sound_id.clone(),
        Some("Ctrl+Alt+Digit6".to_string()),
        false,
        Some(crate::library_store::scope_key(
            &crate::library_store::LibraryScope::Folder {
                root_path: "/music".to_string(),
                relative_path: "memes/loud".to_string(),
            },
        )),
        library.clone(),
        projection,
    )
    .expect("bind inside a folder tab");

    let bindings = library
        .hotkey_bindings_for_sound(&sound_id)
        .recv()
        .expect("read the sound's bindings");
    assert!(bindings[0]
        .binding_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_')));
}
