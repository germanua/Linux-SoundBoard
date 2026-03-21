use std::sync::{Arc, Mutex};

use crate::config::{Config, ControlHotkeyAction};
use crate::hotkeys::HotkeyManager;

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
    let previous_hotkey = {
        let config = config.lock().unwrap();
        config.get_sound(&id).and_then(|s| s.hotkey.clone())
    };
    {
        let manager = hotkeys.lock().unwrap();
        if let Some(hk) = canonical_new.as_ref() {
            manager.register_hotkey_blocking(&id, hk)?;
        } else {
            manager.unregister_hotkey_blocking(&id)?;
        }
    }
    let save_result = {
        let mut cfg = config.lock().unwrap();
        cfg.set_hotkey(&id, canonical_new.clone());
        if let Err(e) = cfg.save() {
            cfg.set_hotkey(&id, previous_hotkey.clone());
            Err(e.to_string())
        } else {
            Ok(())
        }
    };
    if let Err(e) = save_result {
        let manager = hotkeys.lock().unwrap();
        match previous_hotkey {
            Some(prev) => {
                let _ = manager.register_hotkey_blocking(&id, &prev);
            }
            None => {
                let _ = manager.unregister_hotkey_blocking(&id);
            }
        }
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

    let previous_hotkey = {
        let cfg = config.lock().unwrap();
        cfg.settings.control_hotkeys.get_cloned(action)
    };

    {
        let manager = hotkeys.lock().unwrap();
        if let Some(hk) = canonical_new.as_ref() {
            manager.register_hotkey_blocking(binding_id, hk)?;
        } else {
            manager.unregister_hotkey_blocking(binding_id)?;
        }
    }

    let save_result = {
        let mut cfg = config.lock().unwrap();
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
    };

    if let Err(e) = save_result {
        let manager = hotkeys.lock().unwrap();
        match previous_hotkey {
            Some(prev) => {
                let _ = manager.register_hotkey_blocking(binding_id, &prev);
            }
            None => {
                let _ = manager.unregister_hotkey_blocking(binding_id);
            }
        }
        return Err(e);
    }
    Ok(())
}

#[allow(dead_code)]
pub fn open_hotkey_settings(_hotkeys: Arc<Mutex<HotkeyManager>>) -> Result<(), String> {
    Ok(())
}
