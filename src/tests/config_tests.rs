use linux_soundboard::config::{Config, Theme, CURRENT_SCHEMA_VERSION};

#[test]
fn test_config_default_has_correct_values() {
    let config = Config::default();
    assert_eq!(config.settings.theme, Theme::Dark);
    assert_eq!(config.settings.local_volume, 80);
    assert_eq!(config.settings.mic_volume, 100);
    assert!(!config.settings.local_mute);
    assert!(config.settings.mic_passthrough);
    assert!(!config.settings.auto_gain);
}

#[test]
fn test_config_default_has_schema_version() {
    let config = Config::default();
    assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
}

#[test]
fn test_config_default_has_empty_collections() {
    let config = Config::default();
    assert!(config.sounds.is_empty());
    assert!(config.sound_folders.is_empty());
    assert!(config.tabs.is_empty());
}
