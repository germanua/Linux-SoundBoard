use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::app_meta::GENERAL_TAB_ID;
use crate::config::{Config, SoundTab};
use crate::library_store::{LibraryStore, ManualMembershipRecord, ManualTabRecord};

use super::shared::{
    dispatch_async_result, with_config_mut, with_saved_config_checked, with_saved_config_result,
};
use super::CommandError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabDropOperation {
    Noop,
    AddToTarget,
    RemoveFromSource,
    MoveBetweenCustomTabs,
}

static TAB_MUTATION_PENDING: AtomicBool = AtomicBool::new(false);

struct TabMutationGuard;

impl Drop for TabMutationGuard {
    fn drop(&mut self) {
        TAB_MUTATION_PENDING.store(false, Ordering::Release);
    }
}

fn dispatch_tab_mutation<T, F, C>(
    task_name: &'static str,
    task: F,
    on_complete: C,
) -> Result<(), CommandError>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
    C: FnOnce(T) + 'static,
{
    TAB_MUTATION_PENDING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| CommandError::Invalid("Another tab update is still running".to_string()))?;
    let dispatched = dispatch_async_result(
        task_name,
        move || {
            let _guard = TabMutationGuard;
            task()
        },
        on_complete,
    );
    if dispatched.is_err() {
        TAB_MUTATION_PENDING.store(false, Ordering::Release);
    }
    dispatched
}

fn resolve_tab_drop_operation(source_tab_id: &str, target_tab_id: &str) -> TabDropOperation {
    if source_tab_id == target_tab_id {
        return TabDropOperation::Noop;
    }

    let source_is_general = source_tab_id == GENERAL_TAB_ID;
    let target_is_general = target_tab_id == GENERAL_TAB_ID;

    match (source_is_general, target_is_general) {
        (true, true) => TabDropOperation::Noop,
        (true, false) => TabDropOperation::AddToTarget,
        (false, true) => TabDropOperation::RemoveFromSource,
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
) -> Result<bool, CommandError> {
    let Some(tab) = cfg.get_tab_mut(tab_id) else {
        return Err(CommandError::NotFound("Target tab"));
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
    not_found_subject: &'static str,
) -> Result<bool, CommandError> {
    let Some(tab) = cfg.get_tab_mut(tab_id) else {
        return Err(CommandError::NotFound(not_found_subject));
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
) -> Result<bool, CommandError> {
    if sound_ids.is_empty() {
        return Ok(false);
    }

    let op = resolve_tab_drop_operation(source_tab_id, target_tab_id);
    match op {
        TabDropOperation::Noop => Ok(false),
        TabDropOperation::AddToTarget => add_sounds_to_existing_tab(cfg, target_tab_id, sound_ids),
        TabDropOperation::RemoveFromSource => {
            remove_sounds_from_existing_tab(cfg, source_tab_id, sound_ids, "Source tab")
        }
        TabDropOperation::MoveBetweenCustomTabs => {
            let source_exists = cfg.get_tab(source_tab_id).is_some();
            if !source_exists {
                return Err(CommandError::NotFound("Source tab"));
            }
            let target_exists = cfg.get_tab(target_tab_id).is_some();
            if !target_exists {
                return Err(CommandError::NotFound("Target tab"));
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
                "Source tab",
            )?;
            Ok(added || removed)
        }
    }
}

pub fn create_tab(name: String, config: Arc<Mutex<Config>>) -> Result<SoundTab, CommandError> {
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        return Err(CommandError::Invalid(
            "Tab name cannot be empty".to_string(),
        ));
    }
    with_saved_config_result(&config, |cfg| Ok(cfg.create_tab(trimmed_name)))
}

pub fn create_tab_with_store(
    name: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
) -> Result<SoundTab, CommandError> {
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        return Err(CommandError::Invalid(
            "Tab name cannot be empty".to_string(),
        ));
    }
    let order = config
        .lock()
        .tabs
        .iter()
        .map(|tab| tab.order)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let tab = SoundTab::new(trimmed_name, order);
    library
        .upsert_manual_tab(ManualTabRecord {
            public_id: tab.id.clone(),
            name: tab.name.clone(),
            position: tab.order as usize,
        })
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    with_saved_config_result(&config, |cfg| {
        cfg.tabs.push(tab.clone());
        Ok(tab)
    })
}

pub fn create_tab_with_store_async<F>(
    name: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<SoundTab, CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "create_tab",
        move || create_tab_with_store(name, config, library),
        on_complete,
    )
}

pub fn rename_tab(
    id: String,
    name: String,
    config: Arc<Mutex<Config>>,
) -> Result<(), CommandError> {
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        return Err(CommandError::Invalid(
            "Tab name cannot be empty".to_string(),
        ));
    }
    with_saved_config_checked(&config, |cfg| {
        if !cfg.rename_tab(&id, trimmed_name) {
            return Err(CommandError::NotFound("Tab"));
        }
        Ok(())
    })
}

pub fn rename_tab_with_store(
    id: String,
    name: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
) -> Result<(), CommandError> {
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        return Err(CommandError::Invalid(
            "Tab name cannot be empty".to_string(),
        ));
    }
    let position = config
        .lock()
        .get_tab(&id)
        .map(|tab| tab.order as usize)
        .ok_or(CommandError::NotFound("Tab"))?;
    library
        .upsert_manual_tab(ManualTabRecord {
            public_id: id.clone(),
            name: trimmed_name.clone(),
            position,
        })
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    rename_tab(id, trimmed_name, config)
}

pub fn rename_tab_with_store_async<F>(
    id: String,
    name: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "rename_tab",
        move || rename_tab_with_store(id, name, config, library),
        on_complete,
    )
}

pub fn delete_tab(id: String, config: Arc<Mutex<Config>>) -> Result<(), CommandError> {
    with_config_mut(&config, |cfg| {
        if cfg.get_tab(&id).is_none() {
            return Err(CommandError::NotFound("Tab"));
        }

        let mut candidate = cfg.clone();
        candidate.delete_tab(&id);
        candidate.save().map_err(CommandError::config_save)?;
        *cfg = candidate;
        Ok(())
    })?
}

pub fn delete_tab_async<F>(
    id: String,
    config: Arc<Mutex<Config>>,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation("delete_tab", move || delete_tab(id, config), on_complete)
}

pub fn delete_tab_with_store_async<F>(
    id: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "delete_tab",
        move || {
            library
                .delete_manual_tab(&id)
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))?;
            delete_tab(id, config)
        },
        on_complete,
    )
}

pub fn add_sounds_to_tab(
    tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<(), CommandError> {
    with_saved_config_checked(&config, |cfg| {
        if !cfg.add_sounds_to_tab(&tab_id, sound_ids) {
            return Err(CommandError::NotFound("Tab"));
        }
        Ok(())
    })
}

pub fn add_sounds_to_tab_with_store(
    tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
) -> Result<(), CommandError> {
    let base = config
        .lock()
        .get_tab(&tab_id)
        .map(|tab| tab.sound_ids.len())
        .ok_or(CommandError::NotFound("Tab"))?;
    let additions = sound_ids
        .iter()
        .enumerate()
        .map(|(offset, sound_id)| ManualMembershipRecord {
            tab_public_id: tab_id.clone(),
            sound_public_id: sound_id.clone(),
            position: base.saturating_add(offset),
        })
        .collect();
    library
        .apply_manual_memberships(additions, Vec::new())
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    add_sounds_to_tab(tab_id, sound_ids, config)
}

pub fn add_sounds_to_tab_with_store_async<F>(
    tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "add_sounds_to_tab",
        move || add_sounds_to_tab_with_store(tab_id, sound_ids, config, library),
        on_complete,
    )
}

pub fn remove_sound_from_tab(
    tab_id: String,
    sound_id: String,
    config: Arc<Mutex<Config>>,
) -> Result<(), CommandError> {
    with_saved_config_checked(&config, |cfg| {
        if !cfg.remove_sound_from_tab(&tab_id, &sound_id) {
            return Err(CommandError::NotFound("Tab or sound"));
        }
        Ok(())
    })
}

pub fn remove_sound_from_tab_with_store(
    tab_id: String,
    sound_id: String,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
) -> Result<(), CommandError> {
    library
        .remove_manual_membership(&tab_id, &sound_id)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    remove_sound_from_tab(tab_id, sound_id, config)
}

pub fn remove_sounds_from_tab(
    tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<(), CommandError> {
    let sound_ids = normalize_sound_ids(sound_ids);
    with_saved_config_checked(&config, |cfg| {
        if !cfg.remove_sounds_from_tab(&tab_id, &sound_ids) {
            return Err(CommandError::NotFound("Tab"));
        }
        Ok(())
    })
}

pub fn remove_sounds_from_tab_with_store(
    tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
) -> Result<(), CommandError> {
    let sound_ids = normalize_sound_ids(sound_ids);
    library
        .remove_manual_memberships(&tab_id, sound_ids.clone())
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    remove_sounds_from_tab(tab_id, sound_ids, config)
}

pub fn remove_sounds_from_tab_with_store_async<F>(
    tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "remove_sounds_from_tab",
        move || remove_sounds_from_tab_with_store(tab_id, sound_ids, config, library),
        on_complete,
    )
}

pub fn apply_sound_tab_drop(
    source_tab_id: String,
    target_tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
) -> Result<bool, CommandError> {
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
    with_saved_config_result(&config, |cfg| {
        apply_sound_tab_drop_to_config(cfg, &source_tab_id, &target_tab_id, &sound_ids)
    })
}

pub fn apply_sound_tab_drop_with_store(
    source_tab_id: String,
    target_tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
) -> Result<bool, CommandError> {
    let sound_ids = normalize_sound_ids(sound_ids);
    if sound_ids.is_empty() {
        return Ok(false);
    }
    let operation = resolve_tab_drop_operation(&source_tab_id, &target_tab_id);
    let target_base = config
        .lock()
        .get_tab(&target_tab_id)
        .map(|tab| tab.sound_ids.len())
        .unwrap_or(0);
    let additions = match operation {
        TabDropOperation::Noop => return Ok(false),
        TabDropOperation::AddToTarget | TabDropOperation::MoveBetweenCustomTabs => sound_ids
            .iter()
            .enumerate()
            .map(|(offset, sound_id)| ManualMembershipRecord {
                tab_public_id: target_tab_id.clone(),
                sound_public_id: sound_id.clone(),
                position: target_base.saturating_add(offset),
            })
            .collect(),
        TabDropOperation::RemoveFromSource => Vec::new(),
    };
    let removals = if matches!(
        operation,
        TabDropOperation::RemoveFromSource | TabDropOperation::MoveBetweenCustomTabs
    ) {
        sound_ids
            .iter()
            .map(|sound_id| (source_tab_id.clone(), sound_id.clone()))
            .collect()
    } else {
        Vec::new()
    };
    library
        .apply_manual_memberships(additions, removals)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    apply_sound_tab_drop(source_tab_id, target_tab_id, sound_ids, config)
}

pub fn apply_sound_tab_drop_with_store_async<F>(
    source_tab_id: String,
    target_tab_id: String,
    sound_ids: Vec<String>,
    config: Arc<Mutex<Config>>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<bool, CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "apply_sound_tab_drop",
        move || {
            apply_sound_tab_drop_with_store(
                source_tab_id,
                target_tab_id,
                sound_ids,
                config,
                library,
            )
        },
        on_complete,
    )
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
