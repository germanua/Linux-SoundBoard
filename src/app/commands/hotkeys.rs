use std::sync::{Arc, Mutex};

use crate::config::{Config, ControlHotkeyAction};
use crate::hotkeys::HotkeyManager;

/// Lock config and surface poison errors.
fn with_config<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&Config) -> R,
{
    config
        .lock()
        .map(|guard| f(&guard))
        .map_err(|e| format!("Config lock poisoned: {}", e))
}

/// Lock config mutably and surface poison errors.
fn with_config_mut<F, R>(config: &Arc<Mutex<Config>>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut Config) -> R,
{
    config
        .lock()
        .map(|mut guard| f(&mut guard))
        .map_err(|e| format!("Config lock poisoned: {}", e))
}

/// Lock hotkeys and surface poison errors.
fn with_hotkeys<F, R>(hotkeys: &Arc<Mutex<HotkeyManager>>, f: F) -> Result<R, String>
where
    F: FnOnce(&mut HotkeyManager) -> R,
{
    hotkeys
        .lock()
        .map(|mut guard| f(&mut guard))
        .map_err(|e| format!("Hotkeys lock poisoned: {}", e))
}

pub fn set_hotkey(
    id: String,
    hotkey: Option<String>,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<(), String> {
    let canonical_new = match hotkey {
        Some(hk) => Some(crate::hotkeys::canonicalize_hotkey_string(&hk)?),
        None => None,
    };

    let previous_hotkey = with_config(&config, |cfg| {
        cfg.get_sound(&id).and_then(|s| s.hotkey.clone())
    })?;

    {
        with_hotkeys(&hotkeys, |manager| {
            if let Some(hk) = canonical_new.as_ref() {
                manager.register_hotkey_blocking(&id, hk)
            } else {
                manager.unregister_hotkey_blocking(&id)
            }
        })??;
    }
    if let Ok(status) = hotkeys.lock().map(|manager| manager.status_message()) {
        crate::diagnostics::set_hotkey_status(&status);
    }

    let save_result = with_config_mut(&config, |cfg| {
        cfg.set_hotkey(&id, canonical_new.clone());
        if let Err(e) = cfg.save() {
            cfg.set_hotkey(&id, previous_hotkey.clone());
            Err(e.to_string())
        } else {
            Ok(())
        }
    })?;

    if let Err(e) = save_result {
        let _ = with_hotkeys(&hotkeys, |manager| match previous_hotkey {
            Some(prev) => manager.register_hotkey_blocking(&id, &prev),
            None => manager.unregister_hotkey_blocking(&id),
        });
        return Err(e);
    }
    Ok(())
}

pub fn set_control_hotkey(
    action: String,
    hotkey: Option<String>,
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
) -> Result<(), String> {
    let action = ControlHotkeyAction::from_id(&action)
        .ok_or_else(|| "Invalid control hotkey action".to_string())?;
    let binding_id = action.binding_id();
    let canonical_new = match hotkey {
        Some(hk) => Some(crate::hotkeys::canonicalize_hotkey_string(&hk)?),
        None => None,
    };

    let previous_hotkey = with_config(&config, |cfg| {
        cfg.settings.control_hotkeys.get_cloned(action)
    })?;

    {
        with_hotkeys(&hotkeys, |manager| {
            if let Some(hk) = canonical_new.as_ref() {
                manager.register_hotkey_blocking(binding_id, hk)
            } else {
                manager.unregister_hotkey_blocking(binding_id)
            }
        })??;
    }
    if let Ok(status) = hotkeys.lock().map(|manager| manager.status_message()) {
        crate::diagnostics::set_hotkey_status(&status);
    }

    let save_result = with_config_mut(&config, |cfg| {
        cfg.settings
            .control_hotkeys
            .set_action(action, canonical_new.clone());
        if let Err(e) = cfg.save() {
            cfg.settings
                .control_hotkeys
                .set_action(action, previous_hotkey.clone());
            Err(e.to_string())
        } else {
            Ok(())
        }
    })?;

    if let Err(e) = save_result {
        let _ = with_hotkeys(&hotkeys, |manager| match previous_hotkey {
            Some(prev) => manager.register_hotkey_blocking(binding_id, &prev),
            None => manager.unregister_hotkey_blocking(binding_id),
        });
        return Err(e);
    }
    Ok(())
}

pub fn open_hotkey_settings(_hotkeys: Arc<Mutex<HotkeyManager>>) -> Result<(), String> {
    Ok(())
}
