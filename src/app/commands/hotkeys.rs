use parking_lot::Mutex;
use std::sync::Arc;

use std::str::FromStr;

use uuid::Uuid;

use crate::config::{ControlHotkeyAction, TAB_BINDING_PREFIX};
use crate::hotkeys::{HotkeyManager, HotkeyProjectionCoordinator};
use crate::library_store::{HotkeyBindingOwner, HotkeyBindingRecord, LibraryStore, MAX_BATCH_ROWS};

use super::shared::dispatch_async_result;
use super::CommandError;

const MULTI_SOUND_HOTKEY_REQUIRED: &str =
    "Turn on \"Multiple Sounds Per Hotkey\" to give several sounds the same shortcut.";

fn normalize_target_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    ids.into_iter()
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

fn ensure_store_hotkey_available(
    library: &LibraryStore,
    current_binding_id: &str,
    canonical_hotkey: Option<&str>,
    sounds_may_share: bool,
    tab_scope: Option<&str>,
) -> Result<(), CommandError> {
    let Some(canonical_hotkey) = canonical_hotkey else {
        return Ok(());
    };
    if let Some(conflict) = library
        .hotkey_conflict(
            current_binding_id,
            canonical_hotkey,
            sounds_may_share,
            tab_scope,
        )
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?
    {
        Err(CommandError::Hotkey(
            crate::hotkeys::hotkey_conflict(&conflict).to_string(),
        ))
    } else {
        Ok(())
    }
}

fn commit_hotkey_binding(
    library: &LibraryStore,
    projection: &HotkeyProjectionCoordinator,
    binding_id: String,
    owner: HotkeyBindingOwner,
    tab_scope: Option<String>,
    canonical: Option<&str>,
) -> Result<(), CommandError> {
    match canonical {
        Some(hotkey) => library.set_hotkey_binding(HotkeyBindingRecord {
            binding_id,
            owner,
            accelerator: hotkey.to_string(),
            normalized: Some(hotkey.to_string()),
            issue: None,
            tab_scope,
        }),
        None => library.delete_hotkey_binding(&binding_id),
    }
    .recv()
    .map_err(|error| CommandError::Library(error.to_string()))?;

    projection
        .reconcile_blocking()
        .map_err(CommandError::HotkeyProjection)
}

pub fn set_hotkey(
    id: String,
    hotkey: Option<String>,
    multi_sound_hotkeys: bool,
    tab_scope: Option<String>,
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
) -> Result<(), CommandError> {
    let canonical_new = match hotkey {
        Some(hk) => Some(
            crate::hotkeys::canonicalize_hotkey_string(&hk)
                .map_err(|e| CommandError::Hotkey(e.to_string()))?,
        ),
        None => None,
    };

    let existing = library
        .hotkey_bindings_for_sound(&id)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;
    let binding_id = existing
        .iter()
        .find(|binding| binding.tab_scope.as_deref() == tab_scope.as_deref())
        .map(|binding| binding.binding_id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    ensure_store_hotkey_available(
        &library,
        &binding_id,
        canonical_new.as_deref(),
        multi_sound_hotkeys,
        tab_scope.as_deref(),
    )?;

    commit_hotkey_binding(
        &library,
        &projection,
        binding_id,
        HotkeyBindingOwner::Sound(id.clone()),
        tab_scope.clone(),
        canonical_new.as_deref(),
    )
}

pub fn set_hotkey_async<F>(
    id: String,
    hotkey: Option<String>,
    multi_sound_hotkeys: bool,
    tab_scope: Option<String>,
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "set_hotkey",
        move || {
            set_hotkey(
                id,
                hotkey,
                multi_sound_hotkeys,
                tab_scope,
                library,
                projection,
            )
        },
        on_complete,
    )
}

pub fn set_hotkey_many(
    ids: Vec<String>,
    hotkey: Option<String>,
    multi_sound_hotkeys: bool,
    tab_scope: Option<String>,
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
) -> Result<(), CommandError> {
    let ids = normalize_target_ids(ids);
    if ids.is_empty() {
        return Err(CommandError::Invalid("No sounds selected".to_string()));
    }
    if ids.len() > MAX_BATCH_ROWS {
        return Err(CommandError::Invalid(format!(
            "At most {MAX_BATCH_ROWS} sounds can share one shortcut"
        )));
    }

    let canonical_new = match hotkey {
        Some(hk) => Some(
            crate::hotkeys::canonicalize_hotkey_string(&hk)
                .map_err(|e| CommandError::Hotkey(e.to_string()))?,
        ),
        None => None,
    };

    if canonical_new.is_some() && ids.len() > 1 && !multi_sound_hotkeys {
        return Err(CommandError::Hotkey(
            MULTI_SOUND_HOTKEY_REQUIRED.to_string(),
        ));
    }

    if let Some(canonical) = canonical_new.as_deref() {
        let conflict = library
            .hotkey_conflict_excluding_sounds(
                canonical,
                &ids,
                multi_sound_hotkeys,
                tab_scope.as_deref(),
            )
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
        if let Some(conflict) = conflict {
            return Err(CommandError::Hotkey(
                crate::hotkeys::hotkey_conflict(&conflict).to_string(),
            ));
        }
    }

    library
        .set_sound_hotkeys(ids, canonical_new, tab_scope)
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))?;

    projection
        .reconcile_blocking()
        .map_err(CommandError::HotkeyProjection)
}

pub fn set_hotkey_many_async<F>(
    ids: Vec<String>,
    hotkey: Option<String>,
    multi_sound_hotkeys: bool,
    tab_scope: Option<String>,
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "set_hotkey_many",
        move || {
            set_hotkey_many(
                ids,
                hotkey,
                multi_sound_hotkeys,
                tab_scope,
                library,
                projection,
            )
        },
        on_complete,
    )
}

pub fn set_control_hotkey(
    action: String,
    hotkey: Option<String>,
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
) -> Result<(), CommandError> {
    let action = ControlHotkeyAction::from_id(&action)
        .ok_or_else(|| CommandError::Invalid("Invalid control hotkey action".to_string()))?;
    let binding_id = action.binding_id();
    let canonical_new = match hotkey {
        Some(hk) => Some(
            crate::hotkeys::canonicalize_hotkey_string(&hk)
                .map_err(|e| CommandError::Hotkey(e.to_string()))?,
        ),
        None => None,
    };

    ensure_store_hotkey_available(&library, binding_id, canonical_new.as_deref(), false, None)?;

    commit_hotkey_binding(
        &library,
        &projection,
        binding_id.to_string(),
        HotkeyBindingOwner::Control(action.id().to_string()),
        None,
        canonical_new.as_deref(),
    )
}

pub fn set_control_hotkey_async<F>(
    action: String,
    hotkey: Option<String>,
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "set_control_hotkey",
        move || set_control_hotkey(action, hotkey, library, projection),
        on_complete,
    )
}

pub fn hotkey_binding_async<F>(
    binding_id: String,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Option<HotkeyBindingRecord>, CommandError>) + 'static,
{
    dispatch_async_result(
        "hotkey_binding",
        move || {
            library
                .hotkey_binding(&binding_id)
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))
        },
        on_complete,
    )
}

pub fn hotkey_bindings_for_sound_async<F>(
    sound_id: String,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Vec<HotkeyBindingRecord>, CommandError>) + 'static,
{
    dispatch_async_result(
        "hotkey_bindings_for_sound",
        move || {
            library
                .hotkey_bindings_for_sound(&sound_id)
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))
        },
        on_complete,
    )
}

pub fn hotkey_holder_async<F>(
    binding_id: String,
    hotkey: String,
    tab_scope: Option<String>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Option<String>, CommandError>) + 'static,
{
    dispatch_async_result(
        "hotkey_holder",
        move || {
            let canonical = crate::hotkeys::canonicalize_hotkey_string(&hotkey)
                .map_err(|e| CommandError::Hotkey(e.to_string()))?;
            library
                .hotkey_conflict(&binding_id, &canonical, false, tab_scope.as_deref())
                .recv()
                .map_err(|error| CommandError::Library(error.to_string()))
        },
        on_complete,
    )
}

pub fn hotkey_holder_many(
    target_ids: Vec<String>,
    hotkey: String,
    tab_scope: Option<String>,
    library: LibraryStore,
) -> Result<Option<String>, CommandError> {
    let canonical = crate::hotkeys::canonicalize_hotkey_string(&hotkey)
        .map_err(|e| CommandError::Hotkey(e.to_string()))?;
    let target_ids = normalize_target_ids(target_ids);
    library
        .hotkey_conflict_excluding_sounds(&canonical, &target_ids, false, tab_scope.as_deref())
        .recv()
        .map_err(|error| CommandError::Library(error.to_string()))
}

pub fn hotkey_holder_many_async<F>(
    target_ids: Vec<String>,
    hotkey: String,
    tab_scope: Option<String>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Option<String>, CommandError>) + 'static,
{
    dispatch_async_result(
        "hotkey_holder_many",
        move || hotkey_holder_many(target_ids, hotkey, tab_scope, library),
        on_complete,
    )
}

pub fn hotkey_bindings_for_sounds_async<F>(
    sound_ids: Vec<String>,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Vec<HotkeyBindingRecord>, CommandError>) + 'static,
{
    dispatch_async_result(
        "hotkey_bindings_for_sounds",
        move || {
            let mut bindings = Vec::new();
            for sound_id in &sound_ids {
                let found = library
                    .hotkey_bindings_for_sound(sound_id)
                    .recv()
                    .map_err(|error| CommandError::Library(error.to_string()))?;
                bindings.extend(found);
            }
            Ok(bindings)
        },
        on_complete,
    )
}

pub fn set_tab_hotkeys(
    enabled: bool,
    config: Arc<Mutex<crate::config::Config>>,
) -> Result<(), CommandError> {
    super::shared::with_saved_config(&config, |cfg| {
        cfg.settings.tab_hotkeys = enabled;
    })
}

pub fn set_multi_sound_hotkeys(
    enabled: bool,
    config: Arc<Mutex<crate::config::Config>>,
) -> Result<(), CommandError> {
    super::shared::with_saved_config(&config, |cfg| {
        cfg.settings.multi_sound_hotkeys = enabled;
    })
}

pub fn set_group_mode(
    mode: String,
    config: Arc<Mutex<crate::config::Config>>,
) -> Result<(), CommandError> {
    let mode = crate::config::GroupMode::from_str(&mode)
        .map_err(|()| CommandError::Invalid(format!("Unknown shared hotkey mode: {mode}")))?;
    super::shared::with_saved_config(&config, |cfg| {
        cfg.settings.group_mode = mode;
    })
}

pub fn cycle_group_mode(
    config: Arc<Mutex<crate::config::Config>>,
) -> Result<crate::config::GroupMode, CommandError> {
    super::shared::with_saved_config_result(&config, |cfg| {
        let next = cfg.settings.group_mode.next_mode();
        cfg.settings.group_mode = next;
        Ok(next)
    })
}

pub fn tab_binding_id(scope_key: &str) -> String {
    format!("{TAB_BINDING_PREFIX}{scope_key}")
}

pub fn tab_from_binding_id(binding_id: &str) -> Option<&str> {
    binding_id.strip_prefix(TAB_BINDING_PREFIX)
}

pub fn set_tab_hotkey(
    scope_key: String,
    hotkey: Option<String>,
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
) -> Result<(), CommandError> {
    if scope_key.trim().is_empty() {
        return Err(CommandError::Invalid("Invalid tab".to_string()));
    }
    let binding_id = tab_binding_id(&scope_key);
    let canonical_new = match hotkey {
        Some(hk) => Some(
            crate::hotkeys::canonicalize_hotkey_string(&hk)
                .map_err(|e| CommandError::Hotkey(e.to_string()))?,
        ),
        None => None,
    };

    ensure_store_hotkey_available(&library, &binding_id, canonical_new.as_deref(), false, None)?;

    commit_hotkey_binding(
        &library,
        &projection,
        binding_id,
        HotkeyBindingOwner::Tab(scope_key),
        None,
        canonical_new.as_deref(),
    )
}

pub fn set_tab_hotkey_async<F>(
    scope_key: String,
    hotkey: Option<String>,
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "set_tab_hotkey",
        move || set_tab_hotkey(scope_key, hotkey, library, projection),
        on_complete,
    )
}

pub fn open_hotkey_settings(_hotkeys: Arc<Mutex<HotkeyManager>>) -> Result<(), CommandError> {
    Ok(())
}

pub fn install_swhkd_async<F>(
    hotkeys: Arc<Mutex<HotkeyManager>>,
    projection: HotkeyProjectionCoordinator,
    enable_uinput: bool,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<crate::hotkeys::SwhkdInstallReport, crate::hotkeys::SwhkdInstallError>)
        + 'static,
{
    dispatch_async_result(
        "install_swhkd",
        move || {
            let result = crate::hotkeys::install_swhkd_native_detailed(enable_uinput);

            if let Ok(report) = &result {
                let rebind_result = projection.reconcile_blocking();

                if let Err(rebind_err) = rebind_result {
                    return Err(crate::hotkeys::SwhkdInstallError {
                        kind: crate::hotkeys::SwhkdInstallErrorKind::VerificationFailed,
                        summary: "Installation succeeded but hotkey rebind failed.".to_string(),
                        details: format!(
                            "{}\n\nInstaller summary:\n{}\n{}",
                            rebind_err, report.summary, report.details
                        ),
                        state: crate::hotkeys::SwhkdInstallState::Failed,
                    });
                }
            }

            crate::diagnostics::set_hotkey_status(&hotkeys.lock().status_message());

            result
        },
        on_complete,
    )
}
