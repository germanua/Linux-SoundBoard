use std::sync::{Arc, Mutex};

use crate::config::{Config, SoundTab};

use super::shared::{with_saved_config_checked, with_saved_config_result};

pub fn create_tab(name: String, config: Arc<Mutex<Config>>) -> Result<SoundTab, String> {
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        return Err("Tab name cannot be empty".to_string());
    }
    with_saved_config_result(&config, |cfg| cfg.create_tab(trimmed_name))
}

pub fn rename_tab(id: String, name: String, config: Arc<Mutex<Config>>) -> Result<(), String> {
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        return Err("Tab name cannot be empty".to_string());
    }
    with_saved_config_checked(&config, |cfg| {
        if !cfg.rename_tab(&id, trimmed_name) {
            return Err("Tab not found".to_string());
        }
        Ok(())
    })
}

pub fn delete_tab(id: String, config: Arc<Mutex<Config>>) -> Result<(), String> {
    with_saved_config_checked(&config, |cfg| {
        if !cfg.delete_tab(&id) {
            return Err("Tab not found".to_string());
        }
        Ok(())
    })
}

pub fn add_sounds_to_tab(
    tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<(), String> {
    with_saved_config_checked(&config, |cfg| {
        if !cfg.add_sounds_to_tab(&tab_id, sound_ids) {
            return Err("Tab not found".to_string());
        }
        Ok(())
    })
}

pub fn remove_sound_from_tab(
    tab_id: String,
    sound_id: String,
    config: Arc<Mutex<Config>>,
) -> Result<(), String> {
    with_saved_config_checked(&config, |cfg| {
        if !cfg.remove_sound_from_tab(&tab_id, &sound_id) {
            return Err("Tab or sound not found".to_string());
        }
        Ok(())
    })
}
