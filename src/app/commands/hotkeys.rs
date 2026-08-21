use parking_lot::Mutex;
use std::sync::Arc;

use std::str::FromStr;

use crate::config::{ControlHotkeyAction, TAB_BINDING_PREFIX};
use crate::hotkeys::{HotkeyManager, HotkeyProjectionCoordinator};
use crate::library_store::{HotkeyBindingOwner, HotkeyBindingRecord, LibraryStore};

use super::shared::dispatch_async_result;
use super::CommandError;

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

/// `multi_sound_hotkeys` is the Settings toggle: with it on, a chord another
/// sound already answers to is a group to join rather than a conflict.
///
/// `tab_scope` is the tab the binding answers in. `None` means every tab,
/// which is where bindings stay until tab hotkeys are turned on.
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

    ensure_store_hotkey_available(
        &library,
        &id,
        canonical_new.as_deref(),
        multi_sound_hotkeys,
        tab_scope.as_deref(),
    )?;

    if let Some(hotkey) = canonical_new.as_ref() {
        library
            .set_hotkey_binding(HotkeyBindingRecord {
                binding_id: id.clone(),
                owner: HotkeyBindingOwner::Sound(id.clone()),
                accelerator: hotkey.clone(),
                normalized: Some(hotkey.clone()),
                issue: None,
                tab_scope: tab_scope.clone(),
            })
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
    } else {
        library
            .delete_hotkey_binding(&id)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
    }

    projection
        .reconcile_blocking()
        .map_err(CommandError::HotkeyProjection)
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

    if let Some(hotkey) = canonical_new.as_ref() {
        library
            .set_hotkey_binding(HotkeyBindingRecord {
                binding_id: binding_id.to_string(),
                owner: HotkeyBindingOwner::Control(action.id().to_string()),
                accelerator: hotkey.clone(),
                normalized: Some(hotkey.clone()),
                issue: None,
                tab_scope: None,
            })
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
    } else {
        library
            .delete_hotkey_binding(binding_id)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
    }

    projection
        .reconcile_blocking()
        .map_err(CommandError::HotkeyProjection)
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

/// The tab a sound's hotkey is limited to, or `None` when it answers in every
/// tab. Read before offering the choice again, so reopening the dialog shows
/// what is stored rather than silently widening the binding on save.
pub fn hotkey_scope_async<F>(
    binding_id: String,
    library: LibraryStore,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<Option<String>, CommandError>) + 'static,
{
    dispatch_async_result(
        "hotkey_scope",
        move || {
            library
                .hotkey_binding(&binding_id)
                .recv()
                .map(|binding| binding.and_then(|binding| binding.tab_scope))
                .map_err(|error| CommandError::Library(error.to_string()))
        },
        on_complete,
    )
}

/// Who already answers to `hotkey` in this scope, described for the user, or
/// `None` when it is free. Used to ask before adding a sound to a shared
/// shortcut, so the group is never formed silently.
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

/// Answer only the active tab's sound hotkeys while it is showing.
pub fn set_tab_hotkeys(
    enabled: bool,
    config: Arc<Mutex<crate::config::Config>>,
) -> Result<(), CommandError> {
    super::shared::with_saved_config(&config, |cfg| {
        cfg.settings.tab_hotkeys = enabled;
    })
}

/// Allow several sounds to answer to one hotkey.
pub fn set_multi_sound_hotkeys(
    enabled: bool,
    config: Arc<Mutex<crate::config::Config>>,
) -> Result<(), CommandError> {
    super::shared::with_saved_config(&config, |cfg| {
        cfg.settings.multi_sound_hotkeys = enabled;
    })
}

/// Which member a press plays when several sounds share a hotkey.
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

/// Advance the shared-hotkey mode, and report where it landed.
pub fn cycle_group_mode(
    config: Arc<Mutex<crate::config::Config>>,
) -> Result<crate::config::GroupMode, CommandError> {
    super::shared::with_saved_config_result(&config, |cfg| {
        let next = cfg.settings.group_mode.next_mode();
        cfg.settings.group_mode = next;
        Ok(next)
    })
}

/// The binding id a tab's hotkey is stored and projected under.
pub fn tab_binding_id(scope_key: &str) -> String {
    format!("{TAB_BINDING_PREFIX}{scope_key}")
}

/// The tab a press activates, if the binding is a tab hotkey.
pub fn tab_from_binding_id(binding_id: &str) -> Option<&str> {
    binding_id.strip_prefix(TAB_BINDING_PREFIX)
}

/// Bind a hotkey to a tab, so pressing it makes that tab active. Always live
/// whichever tab is showing, so it never conflicts by scope and never shares a
/// chord with anything.
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

    if let Some(hotkey) = canonical_new.as_ref() {
        library
            .set_hotkey_binding(HotkeyBindingRecord {
                binding_id,
                owner: HotkeyBindingOwner::Tab(scope_key),
                accelerator: hotkey.clone(),
                normalized: Some(hotkey.clone()),
                issue: None,
                tab_scope: None,
            })
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
    } else {
        library
            .delete_hotkey_binding(&binding_id)
            .recv()
            .map_err(|error| CommandError::Library(error.to_string()))?;
    }

    projection
        .reconcile_blocking()
        .map_err(CommandError::HotkeyProjection)
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
