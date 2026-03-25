use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use crate::app_meta::GENERAL_TAB_ID;
use crate::config::{Config, SoundTab};

use super::shared::{with_saved_config_checked, with_saved_config_result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabDropOperation {
    Noop,
    AddToTarget,
    RemoveFromSource,
    MoveBetweenCustomTabs,
}

fn resolve_tab_drop_operation(source_tab_id: &str, target_tab_id: &str) -> TabDropOperation {
    if source_tab_id == target_tab_id {
        return TabDropOperation::Noop;
    }

    let source_is_general = source_tab_id == GENERAL_TAB_ID;
    let target_is_general = target_tab_id == GENERAL_TAB_ID;

    match (source_is_general, target_is_general) {
        // Both general, or same tab (handled above) → nothing to do
        (true, true) => TabDropOperation::Noop,
        // General → Custom: add sound to custom tab; General always shows all sounds
        (true, false) => TabDropOperation::AddToTarget,
        // Custom → General: remove from source custom tab
        (false, true) => TabDropOperation::RemoveFromSource,
        // Custom → Custom: move sound between tabs
        (false, false) => TabDropOperation::MoveBetweenCustomTabs,
    }
}

fn normalize_sound_ids(sound_ids: Vec<String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for sound_id in sound_ids {
        let sound_id = sound_id.trim();
        if sound_id.is_empty() {
            continue;
        }
        if normalized.iter().any(|id: &String| id == sound_id) {
            continue;
        }
        normalized.push(sound_id.to_string());
    }
    normalized
}

fn add_sounds_to_existing_tab(
    cfg: &mut Config,
    tab_id: &str,
    sound_ids: &[String],
) -> Result<bool, String> {
    let Some(tab) = cfg.get_tab_mut(tab_id) else {
        return Err("Target tab not found".to_string());
    };

    let mut changed = false;
    for sound_id in sound_ids {
        if tab.sound_ids.iter().any(|existing| existing == sound_id) {
            continue;
        }
        tab.sound_ids.push(sound_id.clone());
        changed = true;
    }

    Ok(changed)
}

fn remove_sounds_from_existing_tab(
    cfg: &mut Config,
    tab_id: &str,
    sound_ids: &[String],
    not_found_error: &str,
) -> Result<bool, String> {
    let Some(tab) = cfg.get_tab_mut(tab_id) else {
        return Err(not_found_error.to_string());
    };

    if sound_ids.is_empty() || tab.sound_ids.is_empty() {
        return Ok(false);
    }

    let remove_set: HashSet<&str> = sound_ids.iter().map(String::as_str).collect();
    let len_before = tab.sound_ids.len();
    tab.sound_ids
        .retain(|sound_id| !remove_set.contains(sound_id.as_str()));
    Ok(tab.sound_ids.len() != len_before)
}

fn apply_sound_tab_drop_to_config(
    cfg: &mut Config,
    source_tab_id: &str,
    target_tab_id: &str,
    sound_ids: &[String],
) -> Result<bool, String> {
    if sound_ids.is_empty() {
        return Ok(false);
    }

    let op = resolve_tab_drop_operation(source_tab_id, target_tab_id);
    match op {
        TabDropOperation::Noop => Ok(false),
        TabDropOperation::AddToTarget => add_sounds_to_existing_tab(cfg, target_tab_id, sound_ids),
        TabDropOperation::RemoveFromSource => {
            remove_sounds_from_existing_tab(cfg, source_tab_id, sound_ids, "Source tab not found")
        }
        TabDropOperation::MoveBetweenCustomTabs => {
            let source_exists = cfg.get_tab(source_tab_id).is_some();
            if !source_exists {
                return Err("Source tab not found".to_string());
            }
            let target_exists = cfg.get_tab(target_tab_id).is_some();
            if !target_exists {
                return Err("Target tab not found".to_string());
            }

            let source_snapshot = cfg
                .get_tab(source_tab_id)
                .map(|tab| tab.sound_ids.clone())
                .unwrap_or_default();
            let source_set: HashSet<&str> = source_snapshot.iter().map(String::as_str).collect();
            let movable_sound_ids = sound_ids
                .iter()
                .filter(|sound_id| source_set.contains(sound_id.as_str()))
                .cloned()
                .collect::<Vec<_>>();

            if movable_sound_ids.is_empty() {
                return Ok(false);
            }

            let added = add_sounds_to_existing_tab(cfg, target_tab_id, &movable_sound_ids)?;
            let removed = remove_sounds_from_existing_tab(
                cfg,
                source_tab_id,
                &movable_sound_ids,
                "Source tab not found",
            )?;
            Ok(added || removed)
        }
    }
}

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

pub fn remove_sounds_from_tab(
    tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<(), String> {
    let sound_ids = normalize_sound_ids(sound_ids);
    with_saved_config_checked(&config, |cfg| {
        if !cfg.remove_sounds_from_tab(&tab_id, &sound_ids) {
            return Err("Tab not found".to_string());
        }
        Ok(())
    })
}

pub fn apply_sound_tab_drop(
    source_tab_id: String,
    target_tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<bool, String> {
    let sound_ids = normalize_sound_ids(sound_ids);
    if sound_ids.is_empty() {
        return Ok(false);
    }

    let op = resolve_tab_drop_operation(&source_tab_id, &target_tab_id);
    log::info!(
        "Tab drop operation: {:?} (source={}, target={}, sounds={})",
        op,
        source_tab_id,
        target_tab_id,
        sound_ids.len()
    );
    with_saved_config_checked(&config, |cfg| {
        apply_sound_tab_drop_to_config(cfg, &source_tab_id, &target_tab_id, &sound_ids)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_tab_drop_operation_matches_matrix() {
        assert_eq!(
            resolve_tab_drop_operation(GENERAL_TAB_ID, GENERAL_TAB_ID),
            TabDropOperation::Noop
        );
        assert_eq!(
            resolve_tab_drop_operation(GENERAL_TAB_ID, "custom-a"),
            TabDropOperation::AddToTarget
        );
        assert_eq!(
            resolve_tab_drop_operation("custom-a", GENERAL_TAB_ID),
            TabDropOperation::RemoveFromSource
        );
        assert_eq!(
            resolve_tab_drop_operation("custom-a", "custom-b"),
            TabDropOperation::MoveBetweenCustomTabs
        );
        assert_eq!(
            resolve_tab_drop_operation("custom-a", "custom-a"),
            TabDropOperation::Noop
        );
    }

    #[test]
    fn normalize_sound_ids_dedups_and_ignores_empty() {
        let normalized = normalize_sound_ids(vec![
            "sound-1".to_string(),
            "".to_string(),
            "  ".to_string(),
            "sound-2".to_string(),
            "sound-1".to_string(),
            " sound-3 ".to_string(),
        ]);

        assert_eq!(normalized, vec!["sound-1", "sound-2", "sound-3"]);
    }

    #[test]
    fn apply_sound_tab_drop_to_config_returns_false_for_empty_and_same_tab() {
        let mut cfg = Config::default();
        let mut tab = SoundTab::new("Custom".to_string(), 1);
        tab.id = "custom-a".to_string();
        tab.sound_ids = vec!["sound-1".to_string()];
        cfg.tabs.push(tab);

        let changed_empty =
            apply_sound_tab_drop_to_config(&mut cfg, "custom-a", "custom-b", &[]).unwrap();
        assert!(!changed_empty);

        let changed_same_tab = apply_sound_tab_drop_to_config(
            &mut cfg,
            "custom-a",
            "custom-a",
            &["sound-1".to_string()],
        )
        .unwrap();
        assert!(!changed_same_tab);
        assert_eq!(
            cfg.get_tab("custom-a").unwrap().sound_ids,
            vec!["sound-1".to_string()]
        );
    }

    #[test]
    fn apply_sound_tab_drop_to_config_custom_to_general_removes_from_source() {
        let mut cfg = Config::default();
        let mut source = SoundTab::new("Source".to_string(), 1);
        source.id = "custom-a".to_string();
        source.sound_ids = vec![
            "sound-1".to_string(),
            "sound-2".to_string(),
            "sound-3".to_string(),
        ];
        cfg.tabs.push(source);

        let changed = apply_sound_tab_drop_to_config(
            &mut cfg,
            "custom-a",
            GENERAL_TAB_ID,
            &["sound-1".to_string(), "missing".to_string()],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(
            cfg.get_tab("custom-a").unwrap().sound_ids,
            vec!["sound-2".to_string(), "sound-3".to_string()]
        );
    }

    #[test]
    fn apply_sound_tab_drop_to_config_move_between_custom_tabs_only_when_source_membership_exists()
    {
        let mut cfg = Config::default();
        let mut source = SoundTab::new("Source".to_string(), 1);
        source.id = "custom-a".to_string();
        source.sound_ids = vec!["sound-1".to_string(), "sound-2".to_string()];

        let mut target = SoundTab::new("Target".to_string(), 2);
        target.id = "custom-b".to_string();
        target.sound_ids = vec!["sound-2".to_string()];

        cfg.tabs.push(source);
        cfg.tabs.push(target);

        let changed = apply_sound_tab_drop_to_config(
            &mut cfg,
            "custom-a",
            "custom-b",
            &[
                "sound-2".to_string(),
                "sound-3".to_string(),
                "sound-1".to_string(),
            ],
        )
        .unwrap();

        assert!(changed);
        assert_eq!(
            cfg.get_tab("custom-a").unwrap().sound_ids,
            Vec::<String>::new()
        );
        assert_eq!(
            cfg.get_tab("custom-b").unwrap().sound_ids,
            vec!["sound-2".to_string(), "sound-1".to_string()]
        );
    }
}
