use super::*;

#[test]
fn test_create_tab() {
    let config = create_test_config_state();
    let result = commands::create_tab("Test Tab".to_string(), config.clone());
    assert!(result.is_ok());

    let tab = result.unwrap();
    assert_eq!(tab.name, "Test Tab");

    let cfg = config.lock();
    assert_eq!(cfg.tabs.len(), 1);
}

#[test]
fn test_create_tab_empty_name() {
    let config = create_test_config_state();
    let result = commands::create_tab("".to_string(), config);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("empty"));
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

    let cfg = config.lock();
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

    let cfg = config.lock();
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

    let cfg = config.lock();
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

    let cfg = config.lock();
    assert!(cfg.tabs[0].sound_ids.is_empty());
}
