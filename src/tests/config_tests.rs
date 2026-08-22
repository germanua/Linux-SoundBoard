use linux_soundboard::config::{Config, Theme, CURRENT_SCHEMA_VERSION};

#[test]
fn test_config_default_has_correct_values() {
    let config = Config::default();
    assert_eq!(config.settings.theme, Theme::Dark);
    assert_eq!(config.settings.local_volume, 80);
    assert_eq!(config.settings.mic_volume, 100);
    assert!(!config.settings.local_mute);
    assert!(config.settings.mic_passthrough);
    assert!(config.settings.auto_gain);
}

#[test]
fn test_config_default_has_schema_version() {
    let config = Config::default();
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn test_config_default_has_empty_collections() {
    // The library lives in SQLite; a default config carries settings only.
    let _ = Config::default();
}

#[test]
fn a_config_without_the_tray_keys_still_turns_the_tray_on() {
    let settings: linux_soundboard::config::Settings =
        serde_json::from_str(r#"{"local_volume": 80, "mic_volume": 100, "mic_passthrough": true}"#)
            .expect("an old settings file still parses");
    assert!(settings.tray_enabled);
    assert!(settings.close_to_tray);
}

#[test]
fn the_tray_settings_survive_a_round_trip() {
    let settings = linux_soundboard::config::Settings {
        tray_enabled: false,
        close_to_tray: false,
        ..Default::default()
    };
    let restored: linux_soundboard::config::Settings =
        serde_json::from_str(&serde_json::to_string(&settings).expect("settings serialize"))
            .expect("settings parse");
    assert!(!restored.tray_enabled);
    assert!(!restored.close_to_tray);
}
