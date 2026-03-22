use log::{info, warn};
use std::sync::mpsc::Sender;

use super::backend_runtime::HotkeyBackend;
use super::portal_backend::PortalBackend;
use super::x11_backend::X11Backend;

pub struct HotkeyManager {
    backend: Option<Box<dyn HotkeyBackend>>,
    disabled_reason: Option<String>,
}

impl HotkeyManager {
    pub fn new_blocking(sender: Sender<String>, sounds: &[(String, String)]) -> Self {
        info!("Initializing hotkey backend manager");

        match Self::select_backend() {
            Ok(backend) => {
                info!("Selected hotkey backend: {}", backend.name());
                info!("Prebound hotkeys to register: {}", sounds.len());
                for (id, hk) in sounds {
                    info!("Registering hotkey binding: id='{}' hotkey='{}'", id, hk);
                    if let Err(e) = backend.register(id, hk) {
                        warn!("Failed to register hotkey '{}' for '{}': {}", hk, id, e);
                    }
                }
                backend.start_listener(sender);
                Self {
                    backend: Some(backend),
                    disabled_reason: None,
                }
            }
            Err(reason) => {
                warn!("Global hotkeys unavailable: {}", reason);
                Self {
                    backend: None,
                    disabled_reason: Some(reason),
                }
            }
        }
    }

    pub fn register_hotkey_blocking(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        self.backend
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "Global hotkeys unavailable: {}",
                    self.disabled_reason.as_deref().unwrap_or("unknown")
                )
            })
            .and_then(|backend| backend.register(sound_id, hotkey))
    }

    pub fn unregister_hotkey_blocking(&self, sound_id: &str) -> Result<(), String> {
        if let Some(backend) = &self.backend {
            backend.unregister(sound_id)
        } else {
            Ok(())
        }
    }

    pub fn availability_message(&self) -> Option<String> {
        self.disabled_reason.clone()
    }

    fn select_backend() -> Result<Box<dyn HotkeyBackend>, String> {
        let mut errors = Vec::new();

        match X11Backend::new() {
            Ok(backend) => return Ok(Box::new(backend) as Box<dyn HotkeyBackend>),
            Err(err) => {
                warn!("X11 backend unavailable: {}", err);
                errors.push(format!("x11: {err}"));
            }
        }

        if portal_opt_in_enabled() {
            match PortalBackend::new() {
                Ok(backend) => Ok(Box::new(backend) as Box<dyn HotkeyBackend>),
                Err(portal_err) => {
                    warn!("Portal backend unavailable: {}", portal_err);
                    errors.push(format!("portal: {portal_err}"));
                    Err(format!("no backend available ({})", errors.join("; ")))
                }
            }
        } else {
            Err(format!(
                "no backend available ({}; portal disabled by default)",
                errors.join("; ")
            ))
        }
    }
}

fn portal_opt_in_enabled() -> bool {
    matches!(
        std::env::var("LSB_ENABLE_PORTAL_HOTKEYS").ok().as_deref(),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}
