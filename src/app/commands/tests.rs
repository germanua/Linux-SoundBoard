//! Command tests.

use crate::audio::player::AudioPlayer;
use crate::commands;
use crate::config::{Config, LoudnessAnalysisState, Sound, SoundTab};
use crate::hotkeys::HotkeyManager;
use crate::test_support::audio_fixtures::{
    cleanup_test_audio_path, create_test_audio_file, create_test_audio_file_with_duration,
};
use std::fs;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const BACKGROUND_ANALYSIS_TIMEOUT: Duration = Duration::from_secs(5);

fn create_test_config() -> Config {
    Config::default()
}

fn create_test_config_state() -> Arc<Mutex<Config>> {
    Arc::new(Mutex::new(Config::default()))
}

fn create_mock_hotkey_manager() -> Arc<Mutex<HotkeyManager>> {
    use std::sync::mpsc;
    let (sender, _) = mpsc::channel();
    let manager = HotkeyManager::new_blocking(sender, &[]);
    Arc::new(Mutex::new(manager))
}

fn create_test_audio_player() -> Arc<AudioPlayer> {
    let mut config = Config::default();
    config.settings.local_volume = 80;
    config.settings.mic_volume = 100;
    Arc::new(AudioPlayer::new_with_config(&config))
}

fn background_analysis_test_guard() -> std::sync::MutexGuard<'static, ()> {
    static TEST_GUARD: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = TEST_GUARD
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("background analysis test mutex poisoned");
    wait_for_background_analysis_idle();
    super::playback::reset_missing_loudness_analysis_test_state();
    super::playback::reset_estimated_loudness_refinement_test_state();
    guard
}

fn wait_for_background_analysis_idle() {
    assert!(
        super::playback::wait_for_missing_loudness_analysis_to_finish(BACKGROUND_ANALYSIS_TIMEOUT),
        "timed out waiting for loudness analysis to become idle"
    );
    assert!(
        super::playback::wait_for_estimated_loudness_refinement_to_finish(
            BACKGROUND_ANALYSIS_TIMEOUT,
        ),
        "timed out waiting for loudness refinement to become idle"
    );
}

#[test]
fn test_list_sounds_returns_empty_when_no_sounds() {
    let config = create_test_config_state();
    let sounds = commands::list_sounds(config);
    assert!(sounds.is_empty());
}

#[test]
fn test_list_sounds_with_sounds() {
    let mut config = create_test_config();
    config.sounds.push(Sound::new(
        "Test Sound".to_string(),
        "/tmp/test.mp3".to_string(),
    ));
    let config = Arc::new(Mutex::new(config));

    let sounds = commands::list_sounds(config);
    assert_eq!(sounds.len(), 1);
    assert_eq!(sounds[0].name, "Test Sound");
}

#[test]
fn test_play_sound_not_found() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::play_sound("nonexistent-id".to_string(), config, player);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err(), "Sound not found");
}

#[test]
fn test_play_sound_disabled() {
    let mut config = create_test_config();
    let mut sound = Sound::new("Test".to_string(), "/tmp/test.mp3".to_string());
    sound.enabled = false;
    config.sounds.push(sound);
    let sound_id = config.sounds[0].id.clone();
    let config = Arc::new(Mutex::new(config));
    let player = create_test_audio_player();

    let result = commands::play_sound(sound_id, config, player);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("disabled"));
}

#[test]
fn test_set_allow_multiple_playbacks_is_ignored() {
    let config = create_test_config_state();
    let result = commands::set_allow_multiple_playbacks(true, config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert!(!cfg.settings.allow_multiple_playbacks);
}

#[test]
fn test_set_skip_delete_confirm() {
    let config = create_test_config_state();
    let result = commands::set_skip_delete_confirm(true, config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert!(cfg.settings.skip_delete_confirm);
}

#[test]
fn test_add_sound_file_not_exist() {
    let config = create_test_config_state();
    let result = commands::add_sound(
        "Test".to_string(),
        "/nonexistent/path/audio.mp3".to_string(),
        config,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("exist"));
}

#[test]
fn test_add_sound_populates_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let config = create_test_config_state();

    let sound = commands::add_sound(
        "Test".to_string(),
        audio_path.to_string_lossy().to_string(),
        config,
    )
    .expect("add sound succeeds");

    assert!(sound.duration_ms.is_some());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_add_sound_folder_valid_path() {
    let config = create_test_config_state();
    let result = commands::add_sound_folder("/tmp".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert!(cfg.sound_folders.contains(&"/tmp".to_string()));
}

#[test]
fn test_add_sound_folder_nonexistent() {
    let config = create_test_config_state();
    let result = commands::add_sound_folder("/nonexistent/folder".to_string(), config);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("exist"));
}

#[test]
fn test_remove_sound_folder() {
    let mut config = create_test_config();
    config.sound_folders.push("/tmp".to_string());
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let result = commands::remove_sound_folder("/tmp".to_string(), config.clone(), hotkeys);
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert!(!cfg.sound_folders.contains(&"/tmp".to_string()));
}

#[test]
fn test_remove_sound_removes_tab_membership() {
    let mut config = create_test_config();
    let sound_a = Sound::new("A".to_string(), "/tmp/a.wav".to_string());
    let sound_b = Sound::new("B".to_string(), "/tmp/b.wav".to_string());
    let removed_id = sound_b.id.clone();
    let kept_id = sound_a.id.clone();

    let mut tab = SoundTab::new("Custom".to_string(), 1);
    tab.sound_ids = vec![kept_id.clone(), removed_id.clone()];

    config.sounds = vec![sound_a, sound_b];
    config.tabs.push(tab);

    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    commands::remove_sound(removed_id.clone(), config.clone(), hotkeys).expect("remove succeeds");

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.sounds.len(), 1);
    assert_eq!(cfg.sounds[0].id, kept_id);
    assert_eq!(cfg.tabs[0].sound_ids, vec![cfg.sounds[0].id.clone()]);
}

#[test]
fn test_remove_sounds_batch_removes_multiple_sounds_once() {
    let mut config = create_test_config();
    let sound_a = Sound::new("A".to_string(), "/tmp/a.wav".to_string());
    let mut sound_b = Sound::new("B".to_string(), "/tmp/b.wav".to_string());
    sound_b.hotkey = Some("Ctrl+Alt+KeyB".to_string());
    let sound_c = Sound::new("C".to_string(), "/tmp/c.wav".to_string());

    let kept_id = sound_a.id.clone();
    let remove_b = sound_b.id.clone();
    let remove_c = sound_c.id.clone();

    let mut tab = SoundTab::new("Custom".to_string(), 1);
    tab.sound_ids = vec![kept_id.clone(), remove_b.clone(), remove_c.clone()];

    config.sounds = vec![sound_a, sound_b, sound_c];
    config.tabs.push(tab);

    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    commands::remove_sounds(
        vec![remove_b.clone(), "missing-id".to_string(), remove_c.clone()],
        config.clone(),
        hotkeys,
    )
    .expect("batch remove succeeds");

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.sounds.len(), 1);
    assert_eq!(cfg.sounds[0].id, kept_id);
    assert_eq!(cfg.tabs[0].sound_ids, vec![cfg.sounds[0].id.clone()]);
}

#[test]
fn test_refresh_sounds_empty_folders() {
    let config = create_test_config_state();
    let hotkeys = create_mock_hotkey_manager();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        commands::refresh_sounds(config.clone(), hotkeys)
    }));

    assert!(result.is_ok());
    let sounds = result.unwrap().unwrap();
    assert!(sounds.is_empty());
}

#[test]
fn test_refresh_sounds_with_folder() {
    let mut config = create_test_config();
    config.sound_folders.push("/tmp".to_string());
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        commands::refresh_sounds(config.clone(), hotkeys)
    }));

    assert!(result.is_ok());
}

#[test]
fn test_refresh_sounds_populates_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.sound_folders.push(
        audio_path
            .parent()
            .expect("audio temp dir")
            .to_string_lossy()
            .to_string(),
    );
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let sounds = commands::refresh_sounds(config, hotkeys).expect("refresh succeeds");

    assert_eq!(sounds.len(), 1);
    assert!(sounds[0].duration_ms.is_some());
    assert!(sounds[0].loudness_source_fingerprint.is_some());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_import_dropped_files_populates_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let target_root =
        std::env::temp_dir().join(format!("lsb-import-target-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&target_root).expect("create target dir");

    let mut config = create_test_config();
    config
        .sound_folders
        .push(target_root.to_string_lossy().to_string());
    let config = Arc::new(Mutex::new(config));

    let imported =
        commands::import_dropped_files(vec![audio_path.to_string_lossy().to_string()], config)
            .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert!(imported[0].duration_ms.is_some());
    assert!(imported[0].loudness_source_fingerprint.is_some());

    cleanup_test_audio_path(&audio_path);
    let _ = fs::remove_dir_all(&target_root);
}

#[test]
fn test_import_files_to_tab_populates_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let config = create_test_config_state();

    let imported =
        commands::import_files_to_tab(vec![audio_path.to_string_lossy().to_string()], None, config)
            .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert!(imported[0].duration_ms.is_some());
    assert!(imported[0].loudness_source_fingerprint.is_some());

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_update_sound_source_refreshes_duration_metadata() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.sounds.push(Sound::new(
        "Test".to_string(),
        "/tmp/original.mp3".to_string(),
    ));
    let sound_id = config.sounds[0].id.clone();
    let config = Arc::new(Mutex::new(config));

    let updated =
        commands::update_sound_source(sound_id, audio_path.to_string_lossy().to_string(), config)
            .expect("update succeeds");

    assert!(updated.duration_ms.is_some());
            assert!(updated.loudness_source_fingerprint.is_some());

    cleanup_test_audio_path(&audio_path);
}

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

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.settings.local_volume, 50);
}

#[test]
fn test_set_local_volume_clamp() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_local_volume(150, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.settings.local_volume, 100);
}

#[test]
fn test_toggle_local_mute() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::toggle_local_mute(config.clone(), player);
    assert!(result.is_ok());
    assert!(result.unwrap());

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.settings.mic_latency_profile, crate::config::MicLatencyProfile::Low);
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

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.settings.theme, crate::config::Theme::Dark);
}

#[test]
fn test_set_theme_light() {
    let config = create_test_config_state();
    let result = commands::set_theme("light".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.settings.list_style, crate::config::ListStyle::Compact);
}

#[test]
fn test_set_list_style_card() {
    let config = create_test_config_state();
    let result = commands::set_list_style("card".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
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
fn test_create_tab() {
    let config = create_test_config_state();
    let result = commands::create_tab("Test Tab".to_string(), config.clone());
    assert!(result.is_ok());

    let tab = result.unwrap();
    assert_eq!(tab.name, "Test Tab");

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.tabs.len(), 1);
}

#[test]
fn test_create_tab_empty_name() {
    let config = create_test_config_state();
    let result = commands::create_tab("".to_string(), config);
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("empty"));
}

#[test]
fn test_create_tab_whitespace_name() {
    let config = create_test_config_state();
    let result = commands::create_tab("   ".to_string(), config);
    assert!(result.is_err());
}

#[test]
fn test_rename_tab() {
    let mut config = create_test_config();
    config.tabs.push(SoundTab::new("Original".to_string(), 0));
    let tab_id = config.tabs[0].id.clone();
    let config = Arc::new(Mutex::new(config));

    let result = commands::rename_tab(tab_id, "New Name".to_string(), config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.tabs[0].name, "New Name");
}

#[test]
fn test_rename_tab_not_found() {
    let config = create_test_config_state();
    let result = commands::rename_tab("nonexistent".to_string(), "New Name".to_string(), config);
    assert!(result.is_err());
}

#[test]
fn test_delete_tab() {
    let mut config = create_test_config();
    config.tabs.push(SoundTab::new("To Delete".to_string(), 0));
    let tab_id = config.tabs[0].id.clone();
    let config = Arc::new(Mutex::new(config));

    let result = commands::delete_tab(tab_id, config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert!(cfg.tabs.is_empty());
}

#[test]
fn test_delete_tab_not_found() {
    let config = create_test_config_state();
    let result = commands::delete_tab("nonexistent".to_string(), config);
    assert!(result.is_err());
}

#[test]
fn test_add_sounds_to_tab() {
    let mut config = create_test_config();
    config.tabs.push(SoundTab::new("Test Tab".to_string(), 0));
    let tab_id = config.tabs[0].id.clone();
    config.sounds.push(Sound::new(
        "Sound 1".to_string(),
        "/tmp/sound1.mp3".to_string(),
    ));
    let sound_id = config.sounds[0].id.clone();
    let config = Arc::new(Mutex::new(config));

    let result = commands::add_sounds_to_tab(tab_id, vec![sound_id], config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.tabs[0].sound_ids.len(), 1);
}

#[test]
fn test_add_sounds_to_tab_not_found() {
    let config = create_test_config_state();
    let result = commands::add_sounds_to_tab(
        "nonexistent".to_string(),
        vec!["sound-id".to_string()],
        config,
    );
    assert!(result.is_err());
}

#[test]
fn test_remove_sound_from_tab() {
    let mut config = create_test_config();
    let mut tab = SoundTab::new("Test Tab".to_string(), 0);
    let sound = Sound::new("Sound".to_string(), "/tmp/sound.mp3".to_string());
    tab.sound_ids.push(sound.id.clone());
    config.tabs.push(tab);
    config.sounds.push(sound);
    let tab_id = config.tabs[0].id.clone();
    let sound_id = config.sounds[0].id.clone();
    let config = Arc::new(Mutex::new(config));

    let result = commands::remove_sound_from_tab(tab_id, sound_id, config.clone());
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert!(cfg.tabs[0].sound_ids.is_empty());
}

#[test]
fn test_set_hotkey_valid() {
    let config = create_test_config_state();
    let hotkeys = create_mock_hotkey_manager();

    let mut config_guard = config.lock().unwrap();
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
            let cfg = config.lock().unwrap();
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
fn test_set_hotkey_clear() {
    let config = create_test_config_state();
    let hotkeys = create_mock_hotkey_manager();

    let mut config_guard = config.lock().unwrap();
    let mut sound = Sound::new("Test".to_string(), "/tmp/test.mp3".to_string());
    sound.hotkey = Some("Ctrl+1".to_string());
    config_guard.sounds.push(sound);
    let sound_id = config_guard.sounds[0].id.clone();
    drop(config_guard);

    let result = commands::set_hotkey(sound_id, None, config.clone(), hotkeys);
    match result {
        Ok(_) => {
            let cfg = config.lock().unwrap();
            assert!(cfg.sounds[0].hotkey.is_none());
        }
        Err(e) => {
            println!("Hotkey clear failed: {}", e);
        }
    }
}

#[test]
fn test_set_auto_gain_enabled() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain(true, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert!(cfg.settings.auto_gain);
}

#[test]
fn test_set_auto_gain_disabled() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain(false, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert!(!cfg.settings.auto_gain);
}

#[test]
fn test_set_auto_gain_target() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_target(-16.0, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.settings.auto_gain_target_lufs, -16.0);
}

#[test]
fn test_set_auto_gain_target_clamp() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_target(-30.0, config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
    assert_eq!(cfg.settings.auto_gain_target_lufs, -24.0);
}

#[test]
fn test_set_auto_gain_mode_static() {
    let config = create_test_config_state();
    let player = create_test_audio_player();

    let result = commands::set_auto_gain_mode("static".to_string(), config.clone(), player);
    assert!(result.is_ok());

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
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

    let cfg = config.lock().unwrap();
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

#[test]
fn test_trigger_missing_loudness_analysis_backfills_existing_sounds() {
    let _guard = background_analysis_test_guard();
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;
    config.sounds.push(Sound::new(
        "Startup Sound".to_string(),
        audio_path.to_string_lossy().to_string(),
    ));
    let config = Arc::new(Mutex::new(config));

    let result = commands::trigger_missing_loudness_analysis(Arc::clone(&config), false, None);

    assert!(matches!(
        result,
        Ok(commands::MissingLoudnessAnalysisTrigger::Started)
    ));
    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 1);
    wait_for_background_analysis_idle();

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_trigger_missing_loudness_analysis_skips_unavailable_sounds() {
    let _guard = background_analysis_test_guard();
    let mut config = create_test_config();
    config.settings.auto_gain = true;

    let mut sound = Sound::new("Unavailable Sound".to_string(), "/tmp/missing.wav".to_string());
    sound.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
    sound.loudness_lufs = None;
    config.sounds.push(sound);

    let config = Arc::new(Mutex::new(config));
    let result = commands::trigger_missing_loudness_analysis(config, false, None);

    assert!(matches!(
        result,
        Ok(commands::MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds)
    ));
    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 0);
}

#[test]
fn test_trigger_missing_loudness_analysis_starts_refinement_for_estimated_sounds() {
    let _guard = background_analysis_test_guard();
    let audio_path = create_test_audio_file("wav");

    let mut config = create_test_config();
    config.settings.auto_gain = true;
    let mut sound = Sound::new(
        "Estimated Sound".to_string(),
        audio_path.to_string_lossy().to_string(),
    );
    sound.loudness_lufs = Some(-16.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(0.5);
    let sound_id = sound.id.clone();
    config.sounds.push(sound);
    let config = Arc::new(Mutex::new(config));

    let result = commands::trigger_missing_loudness_analysis(Arc::clone(&config), false, None);
    assert!(matches!(
        result,
        Ok(commands::MissingLoudnessAnalysisTrigger::SkippedNoMissingSounds)
    ));
    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 0);
    assert_eq!(super::playback::estimated_loudness_refinement_start_count(), 1);

    wait_for_background_analysis_idle();

    let cfg = config.lock().expect("config lock");
    let stored = cfg.get_sound(&sound_id).expect("sound exists");
    assert_ne!(stored.loudness_analysis_state, LoudnessAnalysisState::Unavailable);
    assert!(stored.loudness_confidence.unwrap_or(0.0) >= 0.5);
    drop(cfg);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_trigger_estimated_loudness_refinement_force_refines_high_confidence_sound() {
    let _guard = background_analysis_test_guard();
    let audio_path = create_test_audio_file_with_duration("wav", 3_000);

    let mut config = create_test_config();
    config.settings.auto_gain = false;
    let mut sound = Sound::new(
        "Estimated High Confidence".to_string(),
        audio_path.to_string_lossy().to_string(),
    );
    sound.loudness_lufs = Some(-16.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(0.98);
    let sound_id = sound.id.clone();
    config.sounds.push(sound);
    let config = Arc::new(Mutex::new(config));

    let result = commands::trigger_estimated_loudness_refinement(Arc::clone(&config), true);
    assert!(matches!(
        result,
        Ok(commands::EstimatedLoudnessRefinementTrigger::Started)
    ));
    assert_eq!(super::playback::estimated_loudness_refinement_start_count(), 1);

    wait_for_background_analysis_idle();

    let cfg = config.lock().expect("config lock");
    let stored = cfg.get_sound(&sound_id).expect("sound exists");
    assert_eq!(stored.loudness_analysis_state, LoudnessAnalysisState::Refined);
    assert_eq!(stored.loudness_confidence, Some(1.0));
    assert!(stored.loudness_lufs.is_some());
    drop(cfg);

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_trigger_estimated_loudness_refinement_skips_high_confidence_without_force() {
    let _guard = background_analysis_test_guard();
    let mut config = create_test_config();
    config.settings.auto_gain = true;

    let mut sound = Sound::new("High Confidence".to_string(), "/tmp/high.wav".to_string());
    sound.loudness_lufs = Some(-15.0);
    sound.loudness_analysis_state = LoudnessAnalysisState::Estimated;
    sound.loudness_confidence = Some(0.95);
    config.sounds.push(sound);

    let config = Arc::new(Mutex::new(config));
    let result = commands::trigger_estimated_loudness_refinement(config, false);

    assert!(matches!(
        result,
        Ok(commands::EstimatedLoudnessRefinementTrigger::SkippedNoCandidates)
    ));
    assert_eq!(super::playback::estimated_loudness_refinement_start_count(), 0);
}

#[test]
fn test_set_auto_gain_schedules_loudness_backfill_for_missing_sounds() {
    let _guard = background_analysis_test_guard();
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.sounds.push(Sound::new(
        "Backfill Sound".to_string(),
        audio_path.to_string_lossy().to_string(),
    ));
    let config = Arc::new(Mutex::new(config));
    let player = create_test_audio_player();

    let result = commands::set_auto_gain(true, Arc::clone(&config), player);

    assert!(result.is_ok());
    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 1);
    wait_for_background_analysis_idle();

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_add_sound_backfills_loudness_when_auto_gain_is_enabled() {
    let _guard = background_analysis_test_guard();
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;
    let config = Arc::new(Mutex::new(config));

    commands::add_sound(
        "Added Sound".to_string(),
        audio_path.to_string_lossy().to_string(),
        Arc::clone(&config),
    )
    .expect("add sound succeeds");

    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 1);
    wait_for_background_analysis_idle();

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_import_files_to_tab_backfills_loudness_when_auto_gain_is_enabled() {
    let _guard = background_analysis_test_guard();
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;
    let config = Arc::new(Mutex::new(config));

    let imported = commands::import_files_to_tab(
        vec![audio_path.to_string_lossy().to_string()],
        None,
        Arc::clone(&config),
    )
    .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 1);
    wait_for_background_analysis_idle();

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_import_dropped_files_backfills_loudness_when_auto_gain_is_enabled() {
    let _guard = background_analysis_test_guard();
    let audio_path = create_test_audio_file("mp3");
    let target_root =
        std::env::temp_dir().join(format!("lsb-import-target-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&target_root).expect("create target dir");

    let mut config = create_test_config();
    config.settings.auto_gain = true;
    config
        .sound_folders
        .push(target_root.to_string_lossy().to_string());
    let config = Arc::new(Mutex::new(config));

    let imported =
        commands::import_dropped_files(vec![audio_path.to_string_lossy().to_string()], config)
            .expect("import succeeds");

    assert_eq!(imported.len(), 1);
    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 1);
    wait_for_background_analysis_idle();

    cleanup_test_audio_path(&audio_path);
    let _ = fs::remove_dir_all(&target_root);
}

#[test]
fn test_refresh_sounds_backfills_loudness_for_new_library_files() {
    let _guard = background_analysis_test_guard();
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;
    config.sound_folders.push(
        audio_path
            .parent()
            .expect("audio temp dir")
            .to_string_lossy()
            .to_string(),
    );
    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let sounds = commands::refresh_sounds(Arc::clone(&config), hotkeys).expect("refresh succeeds");

    assert_eq!(sounds.len(), 1);
    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 1);
    wait_for_background_analysis_idle();

    cleanup_test_audio_path(&audio_path);
}

#[test]
fn test_update_sound_source_invalidates_loudness_and_schedules_backfill() {
    let _guard = background_analysis_test_guard();

    let new_audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = true;

    let mut sound = Sound::new("Source Test".to_string(), "/tmp/original.mp3".to_string());
    sound.loudness_lufs = Some(-14.0);
    let sound_id = sound.id.clone();
    config.sounds.push(sound);
    let config = Arc::new(Mutex::new(config));

    let updated = commands::update_sound_source(
        sound_id.clone(),
        new_audio_path.to_string_lossy().to_string(),
        Arc::clone(&config),
    )
    .expect("update succeeds");

    assert!(updated.loudness_lufs.is_none());
    assert_eq!(updated.loudness_analysis_state, LoudnessAnalysisState::Pending);
    assert!(updated.loudness_confidence.is_none());
    assert!(updated.loudness_source_fingerprint.is_some());
    assert_eq!(super::playback::missing_loudness_analysis_start_count(), 1);
    wait_for_background_analysis_idle();

    let cfg = config.lock().expect("config lock");
    let stored = cfg.get_sound(&sound_id).expect("updated sound exists");
    assert!(stored.loudness_lufs.is_none());
    assert_ne!(stored.loudness_analysis_state, LoudnessAnalysisState::Estimated);
    assert!(stored.loudness_confidence.is_none());
    assert!(stored.loudness_source_fingerprint.is_some());
    drop(cfg);

    cleanup_test_audio_path(&new_audio_path);
}

#[test]
fn test_refresh_sounds_invalidates_stale_loudness_fingerprint() {
    let audio_path = create_test_audio_file("mp3");
    let mut config = create_test_config();
    config.settings.auto_gain = false;

    let mut sound = Sound::new(
        "Fingerprint Drift".to_string(),
        audio_path.to_string_lossy().to_string(),
    );
    sound.duration_ms = commands::probe_duration_ms(&sound.path);
    sound.loudness_lufs = Some(-14.5);
    sound.loudness_analysis_state = LoudnessAnalysisState::Refined;
    sound.loudness_confidence = Some(1.0);
    sound.loudness_source_fingerprint = Some("stale-fingerprint".to_string());

    config.sound_folders.push(
        audio_path
            .parent()
            .expect("audio temp dir")
            .to_string_lossy()
            .to_string(),
    );
    let sound_id = sound.id.clone();
    config.sounds.push(sound);

    let config = Arc::new(Mutex::new(config));
    let hotkeys = create_mock_hotkey_manager();

    let refreshed = commands::refresh_sounds(Arc::clone(&config), hotkeys).expect("refresh succeeds");
    assert_eq!(refreshed.len(), 1);

    let cfg = config.lock().expect("config lock");
    let stored = cfg.get_sound(&sound_id).expect("sound exists");
    assert!(stored.loudness_lufs.is_none());
    assert_eq!(stored.loudness_analysis_state, LoudnessAnalysisState::Pending);
    assert!(stored.loudness_confidence.is_none());
    assert!(stored.loudness_source_fingerprint.is_some());
    assert_ne!(
        stored.loudness_source_fingerprint.as_deref(),
        Some("stale-fingerprint")
    );
    drop(cfg);

    cleanup_test_audio_path(&audio_path);
}
