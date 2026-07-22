use parking_lot::Mutex;
use std::sync::Arc;

use crate::config::{Config, ControlHotkeyAction};
use crate::hotkeys::{HotkeyManager, HotkeyProjectionCoordinator};
use crate::library_store::{HotkeyBindingOwner, HotkeyBindingRecord, LibraryStore};

use super::shared::dispatch_async_result;
use super::CommandError;

fn canonical_hotkey_matches(stored_hotkey: &str, canonical_hotkey: &str) -> bool {
    crate::hotkeys::canonicalize_hotkey_string(stored_hotkey)
        .map(|stored| stored == canonical_hotkey)
        .unwrap_or_else(|_| stored_hotkey == canonical_hotkey)
}

fn find_hotkey_conflict(
    config: &Config,
    current_binding_id: &str,
    canonical_hotkey: &str,
) -> Option<String> {
    config
        .sounds
        .iter()
        .find_map(|sound| {
            let hotkey = sound.hotkey.as_deref()?;
            if sound.id != current_binding_id && canonical_hotkey_matches(hotkey, canonical_hotkey)
            {
                Some(format!("sound \"{}\"", sound.name))
            } else {
                None
            }
        })
        .or_else(|| {
            ControlHotkeyAction::all().iter().find_map(|meta| {
                let hotkey = config.settings.control_hotkeys.get_cloned(meta.action)?;
                if meta.binding_id != current_binding_id
                    && canonical_hotkey_matches(&hotkey, canonical_hotkey)
                {
                    Some(format!("control action \"{}\"", meta.title))
                } else {
                    None
                }
            })
        })
}

fn ensure_hotkey_available(
    config: &Config,
    current_binding_id: &str,
    canonical_hotkey: Option<&str>,
) -> Result<(), CommandError> {
    let Some(canonical_hotkey) = canonical_hotkey else {
        return Ok(());
    };

    if let Some(conflict) = find_hotkey_conflict(config, current_binding_id, canonical_hotkey) {
        Err(CommandError::Hotkey(
            crate::hotkeys::hotkey_conflict(&conflict).to_string(),
        ))
    } else {
        Ok(())
    }
}

fn ensure_store_hotkey_available(
    library: &LibraryStore,
    current_binding_id: &str,
    canonical_hotkey: Option<&str>,
) -> Result<(), CommandError> {
    let Some(canonical_hotkey) = canonical_hotkey else {
        return Ok(());
    };
    if let Some(conflict) = library
        .hotkey_conflict(current_binding_id, canonical_hotkey)
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

pub fn validate_hotkey_available(
    config: &Config,
    current_binding_id: &str,
    hotkey: &str,
) -> Result<(), CommandError> {
    let canonical_hotkey = crate::hotkeys::canonicalize_hotkey_string(hotkey)
        .map_err(|e| CommandError::Hotkey(e.to_string()))?;
    ensure_hotkey_available(config, current_binding_id, Some(&canonical_hotkey))
}

pub fn set_hotkey(
    id: String,
    hotkey: Option<String>,
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

    ensure_store_hotkey_available(&library, &id, canonical_new.as_deref())?;

    if let Some(hotkey) = canonical_new.as_ref() {
        library
            .set_hotkey_binding(HotkeyBindingRecord {
                binding_id: id.clone(),
                owner: HotkeyBindingOwner::Sound(id.clone()),
                accelerator: hotkey.clone(),
                normalized: Some(hotkey.clone()),
                issue: None,
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
    library: LibraryStore,
    projection: HotkeyProjectionCoordinator,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<(), CommandError>) + 'static,
{
    dispatch_async_result(
        "set_hotkey",
        move || set_hotkey(id, hotkey, library, projection),
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

    ensure_store_hotkey_available(&library, binding_id, canonical_new.as_deref())?;

    if let Some(hotkey) = canonical_new.as_ref() {
        library
            .set_hotkey_binding(HotkeyBindingRecord {
                binding_id: binding_id.to_string(),
                owner: HotkeyBindingOwner::Control(action.id().to_string()),
                accelerator: hotkey.clone(),
                normalized: Some(hotkey.clone()),
                issue: None,
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

pub fn open_hotkey_settings(_hotkeys: Arc<Mutex<HotkeyManager>>) -> Result<(), CommandError> {
    Ok(())
}

pub fn install_swhkd_async<F>(
    hotkeys: Arc<Mutex<HotkeyManager>>,
    projection: HotkeyProjectionCoordinator,
    on_complete: F,
) -> Result<(), CommandError>
where
    F: FnOnce(Result<crate::hotkeys::SwhkdInstallReport, crate::hotkeys::SwhkdInstallError>)
        + 'static,
{
    dispatch_async_result(
        "install_swhkd",
        move || {
            let result = crate::hotkeys::install_swhkd_native_detailed();

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
