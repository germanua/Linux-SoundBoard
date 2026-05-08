use std::sync::{Arc, Mutex};

use crate::config::{Config, ControlHotkeyAction};
use crate::hotkeys::HotkeyManager;

use super::shared::{dispatch_async_result, with_config, with_config_mut};

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
) -> Result<(), String> {
    let Some(canonical_hotkey) = canonical_hotkey else {
        return Ok(());
    };

    if let Some(conflict) = find_hotkey_conflict(config, current_binding_id, canonical_hotkey) {
        Err(crate::hotkeys::hotkey_conflict(&conflict))
    } else {
        Ok(())
    }
}

pub fn validate_hotkey_available(
    config: &Config,
    current_binding_id: &str,
    hotkey: &str,
) -> Result<(), String> {
    let canonical_hotkey = crate::hotkeys::canonicalize_hotkey_string(hotkey)?;
    ensure_hotkey_available(config, current_binding_id, Some(&canonical_hotkey))
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
        ensure_hotkey_available(cfg, &id, canonical_new.as_deref())?;
        Ok::<Option<String>, String>(cfg.get_sound(&id).and_then(|s| s.hotkey.clone()))
    })?;
    let previous_hotkey = previous_hotkey?;

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
        ensure_hotkey_available(cfg, binding_id, canonical_new.as_deref())?;
        Ok::<Option<String>, String>(cfg.settings.control_hotkeys.get_cloned(action))
    })?;
    let previous_hotkey = previous_hotkey?;

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

fn collect_all_bindings(config: &Config) -> Vec<(String, String)> {
    let mut bindings = config
        .sounds
        .iter()
        .filter_map(|sound| {
            sound
                .hotkey
                .as_ref()
                .map(|hotkey| (sound.id.clone(), hotkey.clone()))
        })
        .collect::<Vec<_>>();

    for meta in ControlHotkeyAction::all() {
        if let Some(hotkey) = config.settings.control_hotkeys.get_cloned(meta.action) {
            bindings.push((meta.binding_id.to_string(), hotkey));
        }
    }

    bindings
}

fn rebind_all_hotkeys(config: &Config, manager: &mut HotkeyManager) -> Result<(), String> {
    // Start backend (or refresh disabled reason) before replaying persisted bindings.
    let _ = manager.validate_hotkey_blocking("F1");

    let bindings = collect_all_bindings(config);
    if bindings.is_empty() {
        return Ok(());
    }

    let mut failed = Vec::new();
    for (binding_id, hotkey) in bindings {
        if let Err(err) = manager.register_hotkey_blocking(&binding_id, &hotkey) {
            failed.push(format!("{}={} ({})", binding_id, hotkey, err));
        }
    }

    if failed.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Some hotkeys failed to rebind after installation:\n{}",
            failed.join("\n")
        ))
    }
}

pub fn install_swhkd_async<F>(
    config: Arc<Mutex<Config>>,
    hotkeys: Arc<Mutex<HotkeyManager>>,
    on_complete: F,
) -> Result<(), String>
where
    F: FnOnce(Result<crate::hotkeys::SwhkdInstallReport, crate::hotkeys::SwhkdInstallError>)
        + 'static,
{
    dispatch_async_result(
        "install_swhkd",
        move || {
            let result = crate::hotkeys::install_swhkd_native_detailed();

            if let Ok(report) = &result {
                let rebind_result = (|| {
                    let cfg = config
                        .lock()
                        .map_err(|e| format!("Config lock poisoned: {}", e))?;
                    let mut manager = hotkeys
                        .lock()
                        .map_err(|e| format!("Hotkeys lock poisoned: {}", e))?;
                    rebind_all_hotkeys(&cfg, &mut manager)
                })();

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

            if let Ok(status) = hotkeys.lock().map(|manager| manager.status_message()) {
                crate::diagnostics::set_hotkey_status(&status);
            }

            result
        },
        on_complete,
    )
}
