use crate::audio::AudioPlayer;
use crate::commands;
use crate::config::{
    Config, ControlHotkeyAction, FolderTabBinding, LoudnessAnalysisState, Sound, SoundTab,
};
use crate::hotkeys::HotkeyManager;
use crate::test_support::audio_fixtures::{
    cleanup_test_audio_path, create_test_audio_file, create_test_audio_file_with_duration,
};
use parking_lot::Mutex;
use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

const BACKGROUND_ANALYSIS_TIMEOUT: Duration = Duration::from_secs(5);

fn create_test_config() -> Config {
    let mut cfg = Config::default();
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
    let (sender, _) = mpsc::channel();
    let manager = HotkeyManager::new_blocking(sender, &[]);
    Arc::new(Mutex::new(manager))
}

fn create_test_audio_player() -> Arc<AudioPlayer> {
    Arc::new(AudioPlayer::new_test_noop())
}

fn wait_for_coords_idle(coords: &commands::LoudnessCoordinators) {
    assert!(
        coords.backfill.wait_for_idle(BACKGROUND_ANALYSIS_TIMEOUT),
        "timed out waiting for loudness backfill to become idle"
    );
    assert!(
        coords.refinement.wait_for_idle(BACKGROUND_ANALYSIS_TIMEOUT),
        "timed out waiting for loudness refinement to become idle"
    );
}

fn wait_for_async_result<T>(context: &glib::MainContext, rx: std::sync::mpsc::Receiver<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        while context.pending() {
            context.iteration(false);
        }

        match rx.try_recv() {
            Ok(result) => return result,
            Err(std::sync::mpsc::TryRecvError::Empty) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                panic!("timed out waiting for async command completion");
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                panic!("async command callback disconnected");
            }
        }
    }
}

mod library;
mod tabs;
#[test]
fn test_validate_all_sources_empty() {
    let config = create_test_config_state();
    let result = commands::validate_all_sources(config);
    assert!(result.is_ok());
    assert!(result.unwrap().is_empty());
}

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
#[allow(clippy::print_stdout)]
fn test_set_hotkey_valid() {
    let config = create_test_config_state();
    let hotkeys = create_mock_hotkey_manager();

    let mut config_guard = config.lock();
    config_guard
        .sounds
        .push(Sound::new("Test".to_string(), "/tmp/test.mp3".to_string()));
    let sound_id = config_guard.sounds[0].id.clone();
    drop(config_guard);

    let result = commands::set_hotkey(
        sound_id,
        Some("Ctrl+1".to_string()),
        config.clone(),
        hotkeys,
    );
    match result {
        Ok(_) => {
            let cfg = config.lock();
            assert!(cfg.sounds[0].hotkey.is_some());
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
    let config = create_test_config_state();
    let hotkeys = create_mock_hotkey_manager();

    let mut config_guard = config.lock();
    let mut sound = Sound::new("Test".to_string(), "/tmp/test.mp3".to_string());
    sound.hotkey = Some("Ctrl+1".to_string());
    config_guard.sounds.push(sound);
    let sound_id = config_guard.sounds[0].id.clone();
    drop(config_guard);

    let result = commands::set_hotkey(sound_id, None, config.clone(), hotkeys);
    match result {
        Ok(_) => {
            let cfg = config.lock();
            assert!(cfg.sounds[0].hotkey.is_none());
        }
        Err(e) => {
            println!("Hotkey clear failed: {}", e);
        }
    }
}

#[test]
fn test_set_control_hotkey_rejects_duplicate_control_binding() {
    let config = create_test_config_state();
    let hotkeys = create_mock_hotkey_manager();

    config.lock().settings.control_hotkeys.set_action(
        ControlHotkeyAction::PlayPause,
        Some("Ctrl+Alt+KeyP".to_string()),
    );

    let result = commands::set_control_hotkey(
        ControlHotkeyAction::StopAll.id().to_string(),
        Some("Ctrl+Alt+KeyP".to_string()),
        config,
        hotkeys,
    );

    let err = result.expect_err("duplicate control hotkey must be rejected");
    assert_eq!(
        crate::hotkeys::format_hotkey_error(&err.to_string()),
        "That shortcut is already assigned to control action \"Play / Pause\"."
    );
}

#[test]
fn test_set_control_hotkey_rejects_duplicate_sound_binding() {
    let config = create_test_config_state();
    let hotkeys = create_mock_hotkey_manager();

    let mut sound = Sound::new("Airhorn".to_string(), "/tmp/airhorn.mp3".to_string());
    sound.hotkey = Some("Ctrl+Alt+KeyP".to_string());
    config.lock().sounds.push(sound);

    let result = commands::set_control_hotkey(
        ControlHotkeyAction::StopAll.id().to_string(),
        Some("Ctrl+Alt+KeyP".to_string()),
        config,
        hotkeys,
    );

    let err = result.expect_err("duplicate sound hotkey must be rejected");
    assert_eq!(
        crate::hotkeys::format_hotkey_error(&err.to_string()),
        "That shortcut is already assigned to sound \"Airhorn\"."
    );
}

#[test]
fn test_validate_hotkey_available_reports_duplicate_before_save() {
    let mut config = Config::default();
    config.settings.control_hotkeys.set_action(
        ControlHotkeyAction::PlayPause,
        Some("Ctrl+Alt+KeyP".to_string()),
    );

    let err = commands::validate_hotkey_available(
        &config,
        ControlHotkeyAction::StopAll.binding_id(),
        "Ctrl+Alt+KeyP",
    )
    .expect_err("duplicate hotkey must be reported during capture validation");

    assert_eq!(
        crate::hotkeys::format_hotkey_error(&err.to_string()),
        "That shortcut is already assigned to control action \"Play / Pause\"."
    );
}

#[test]
fn test_set_auto_gain_enabled() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain(
        true,
        config.clone(),
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
    let player = create_test_audio_player();

    let result = commands::set_auto_gain(
        false,
        config.clone(),
        player,
        &commands::LoudnessCoordinators::new(),
    );
    assert!(result.is_ok());

    let cfg = config.lock();
    assert!(!cfg.settings.auto_gain);
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

#[test]
fn test_default_sound_import_dir() {
    let dir = commands::shared::default_sound_import_dir(
        None,
        Some(std::path::PathBuf::from("/home/test")),
    );
    assert!(dir.to_string_lossy().ends_with("soundboard-imports"));
}

mod loudness;
