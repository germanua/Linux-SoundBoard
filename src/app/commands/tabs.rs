use std::sync::atomic::{AtomicBool, Ordering};

use crate::app_meta::GENERAL_TAB_ID;
use crate::config::SoundTab;
use crate::library_store::{LibraryStore, ManualMembershipRecord, ManualTabRecord};

use super::shared::dispatch_async_result;
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

fn find_manual_tab(
    library: &LibraryStore,
    id: &str,
) -> Result<Option<crate::library_store::ManualTabItem>, CommandError> {
    let mut page_index = 0_usize;
    loop {
        let page = library
            .manual_tabs(page_index)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
        if let Some(tab) = page.tabs.into_iter().find(|tab| tab.public_id == id) {
            return Ok(Some(tab));
        }
        if page_index
            .saturating_add(1)
            .saturating_mul(crate::library_store::PAGE_SIZE)
            >= page.total
        {
            return Ok(None);
        }
        page_index = page_index.saturating_add(1);
    }
}

pub fn create_tab_with_store(
    name: String,
    library: LibraryStore,
) -> Result<SoundTab, CommandError> {
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        return Err(CommandError::Invalid(
            "Tab name cannot be empty".to_string(),
        ));
    }
    let order = u32::try_from(
        library
            .manual_tabs(0)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?
            .total,
    )
    .map_err(|_| CommandError::Invalid("Too many tabs".to_string()))?;
    let tab = SoundTab::new(trimmed_name, order);
    library
        .upsert_manual_tab(ManualTabRecord {
            public_id: tab.id.clone(),
            name: tab.name.clone(),
            position: tab.order as usize,
        })
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    Ok(tab)
}

pub fn create_tab_with_store_async<F>(
    name: String,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<SoundTab, CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "create_tab",
        move || create_tab_with_store(name, library),
        on_complete,
    )
}

pub fn rename_tab_with_store(
    id: String,
    name: String,
    library: LibraryStore,
) -> Result<(), CommandError> {
    let trimmed_name = name.trim().to_string();
    if trimmed_name.is_empty() {
        return Err(CommandError::Invalid(
            "Tab name cannot be empty".to_string(),
        ));
    }
    let position = find_manual_tab(&library, &id)?
        .map(|tab| tab.position)
        .ok_or(CommandError::NotFound("Tab"))?;
    library
        .upsert_manual_tab(ManualTabRecord {
            public_id: id.clone(),
            name: trimmed_name.clone(),
            position,
        })
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    Ok(())
}

pub fn rename_tab_with_store_async<F>(
    id: String,
    name: String,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "rename_tab",
        move || rename_tab_with_store(id, name, library),
        on_complete,
    )
}

pub fn delete_tab_with_store_async<F>(
    id: String,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "delete_tab",
        move || {
            let deleted = library
                .delete_manual_tab(&id)
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))?;
            if deleted {
                Ok(())
            } else {
                Err(CommandError::NotFound("Tab"))
            }
        },
        on_complete,
    )
}

pub fn add_sounds_to_tab_with_store(
    tab_id: String,
    sound_ids: Vec<String>,
    library: LibraryStore,
) -> Result<(), CommandError> {
    let base = find_manual_tab(&library, &tab_id)?
        .map(|tab| tab.sound_count)
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
    Ok(())
}

pub fn add_sounds_to_tab_with_store_async<F>(
    tab_id: String,
    sound_ids: Vec<String>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "add_sounds_to_tab",
        move || add_sounds_to_tab_with_store(tab_id, sound_ids, library),
        on_complete,
    )
}

pub fn remove_sound_from_tab_with_store(
    tab_id: String,
    sound_id: String,
    library: LibraryStore,
) -> Result<(), CommandError> {
    library
        .remove_manual_membership(&tab_id, &sound_id)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    Ok(())
}

pub fn remove_sounds_from_tab_with_store(
    tab_id: String,
    sound_ids: Vec<String>,
    library: LibraryStore,
) -> Result<(), CommandError> {
    let sound_ids = normalize_sound_ids(sound_ids);
    library
        .remove_manual_memberships(&tab_id, sound_ids.clone())
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    Ok(())
}

pub fn remove_sounds_from_tab_with_store_async<F>(
    tab_id: String,
    sound_ids: Vec<String>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "remove_sounds_from_tab",
        move || remove_sounds_from_tab_with_store(tab_id, sound_ids, library),
        on_complete,
    )
}

pub fn apply_sound_tab_drop_with_store(
    source_tab_id: String,
    target_tab_id: String,
    sound_ids: Vec<String>,
    library: LibraryStore,
) -> Result<bool, CommandError> {
    let sound_ids = normalize_sound_ids(sound_ids);
    if sound_ids.is_empty() {
        return Ok(false);
    }
    let operation = resolve_tab_drop_operation(&source_tab_id, &target_tab_id);
    let target_base = find_manual_tab(&library, &target_tab_id)?
        .map(|tab| tab.sound_count)
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
    let changed = library
        .apply_manual_memberships(additions, removals)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    Ok(changed)
}

pub fn apply_sound_tab_drop_with_store_async<F>(
    source_tab_id: String,
    target_tab_id: String,
    sound_ids: Vec<String>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<bool, CommandError>) + 'static,
{
    dispatch_tab_mutation(
        "apply_sound_tab_drop",
        move || apply_sound_tab_drop_with_store(source_tab_id, target_tab_id, sound_ids, library),
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
}
