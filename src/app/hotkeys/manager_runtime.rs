use log::{info, warn};
use std::sync::mpsc::Sender;

use crate::app_meta::{BACKEND_ENV_VAR, WAYLAND_BACKEND, X11_BACKEND};

use super::backend_runtime::HotkeyBackend;
use super::swhkd_backend::SwhkdBackend;
use super::x11_backend::X11Backend;

pub struct HotkeyManager {
    backend: Option<Box<dyn HotkeyBackend>>,
    disabled_reason: Option<String>,
    deferred_sender: Option<Sender<String>>,
    deferred_start: bool,
}

impl HotkeyManager {
    pub fn new_blocking(sender: Sender<String>, sounds: &[(String, String)]) -> Self {
        info!("Initializing hotkey backend manager");

        if sounds.is_empty() {
            info!("Deferring hotkey backend startup until the first binding is added");
            return Self {
                backend: None,
                disabled_reason: None,
                deferred_sender: Some(sender),
                deferred_start: true,
            };
        }

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
                    deferred_sender: None,
                    deferred_start: false,
                }
            }
            Err(reason) => {
                warn!("Global hotkeys unavailable: {}", reason);
                Self {
                    backend: None,
                    disabled_reason: Some(reason),
                    // Preserve sender so backend can be retried after remediation.
                    deferred_sender: Some(sender),
                    deferred_start: true,
                }
            }
        }
    }

    pub fn register_hotkey_blocking(&mut self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        self.ensure_backend_started()?;
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

    pub fn validate_hotkey_blocking(&mut self, hotkey: &str) -> Result<(), String> {
        self.ensure_backend_started()?;
        self.backend
            .as_ref()
            .ok_or_else(|| {
                format!(
                    "Global hotkeys unavailable: {}",
                    self.disabled_reason.as_deref().unwrap_or("unknown")
                )
            })
            .and_then(|backend| backend.validate_hotkey(hotkey))
    }

    pub fn unregister_hotkey_blocking(&mut self, sound_id: &str) -> Result<(), String> {
        if let Some(backend) = &self.backend {
            backend.unregister(sound_id)
        } else {
            Ok(())
        }
    }

    pub fn unregister_hotkeys_blocking(&mut self, sound_ids: &[String]) -> Result<(), String> {
        if sound_ids.is_empty() {
            return Ok(());
        }

        if let Some(backend) = &self.backend {
            backend.unregister_many(sound_ids)
        } else {
            Ok(())
        }
    }

    pub fn availability_message(&self) -> Option<String> {
        self.disabled_reason.clone()
    }

    fn ensure_backend_started(&mut self) -> Result<(), String> {
        if self.backend.is_some() {
            return Ok(());
        }
        if !self.deferred_start {
            return Err(format!(
                "Global hotkeys unavailable: {}",
                self.disabled_reason.as_deref().unwrap_or("unknown")
            ));
        }

        let sender = self
            .deferred_sender
            .take()
            .ok_or_else(|| "Global hotkeys unavailable: missing listener channel".to_string())?;

        match Self::select_backend() {
            Ok(backend) => {
                info!("Selected hotkey backend: {} (lazy start)", backend.name());
                backend.start_listener(sender);
                self.backend = Some(backend);
                self.disabled_reason = None;
                self.deferred_start = false;
                Ok(())
            }
            Err(reason) => {
                warn!("Global hotkeys unavailable: {}", reason);
                self.disabled_reason = Some(reason.clone());
                // Keep deferred sender/state so install remediation can retry startup.
                self.deferred_sender = Some(sender);
                self.deferred_start = true;
                Err(format!("Global hotkeys unavailable: {}", reason))
            }
        }
    }

    fn select_backend() -> Result<Box<dyn HotkeyBackend>, String> {
        match session_backend_preference() {
            BackendPreference::Wayland => return select_wayland_backend(),
            BackendPreference::X11 => return select_x11_backend(),
            BackendPreference::Auto => {}
        }

        let mut errors = Vec::new();

        match SwhkdBackend::new() {
            Ok(backend) => return Ok(Box::new(backend) as Box<dyn HotkeyBackend>),
            Err(err) => {
                warn!("swhkd backend unavailable: {}", err);
                errors.push(format!("swhkd: {err}"));
            }
        }

        match X11Backend::new() {
            Ok(backend) => Ok(Box::new(backend) as Box<dyn HotkeyBackend>),
            Err(err) => {
                warn!("X11 backend unavailable: {}", err);
                errors.push(format!("x11: {err}"));
                Err(format!("no backend available ({})", errors.join("; ")))
            }
        }
    }

    /// Check that the active backend is healthy.
    pub fn is_healthy(&self) -> Result<(), String> {
        match &self.backend {
            Some(backend) => {
                if let Some(swhkd) = (**backend).as_any().downcast_ref::<SwhkdBackend>() {
                    swhkd.is_healthy()
                } else {
                    Ok(())
                }
            }
            None => Err(format!(
                "Hotkeys unavailable: {}",
                self.disabled_reason.as_deref().unwrap_or("unknown")
            )),
        }
    }

    /// Format a status message for the UI.
    pub fn status_message(&self) -> String {
        if self.deferred_start && self.disabled_reason.is_none() {
            return "Hotkeys: Idle (no bindings)".to_string();
        }
        match self.backend {
            Some(_) => {
                if self.is_healthy().is_ok() {
                    "Hotkeys: Active".to_string()
                } else {
                    "Hotkeys: Error (see logs)".to_string()
                }
            }
            None => {
                format!(
                    "Hotkeys: Disabled ({})",
                    self.disabled_reason.as_deref().unwrap_or("unavailable")
                )
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendPreference {
    Wayland,
    X11,
    Auto,
}

fn session_backend_preference() -> BackendPreference {
    let explicit = std::env::var(BACKEND_ENV_VAR)
        .ok()
        .map(|value| value.to_ascii_lowercase());
    if matches!(explicit.as_deref(), Some(WAYLAND_BACKEND)) {
        return BackendPreference::Wayland;
    }
    if matches!(explicit.as_deref(), Some(X11_BACKEND)) {
        return BackendPreference::X11;
    }

    match std::env::var("XDG_SESSION_TYPE")
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("wayland") => return BackendPreference::Wayland,
        Some("x11") => return BackendPreference::X11,
        _ => {}
    }

    if std::env::var("WAYLAND_DISPLAY").is_ok() {
        BackendPreference::Wayland
    } else if std::env::var("DISPLAY").is_ok() {
        BackendPreference::X11
    } else {
        BackendPreference::Auto
    }
}

fn select_wayland_backend() -> Result<Box<dyn HotkeyBackend>, String> {
    let mut errors = Vec::new();

    match SwhkdBackend::new() {
        Ok(backend) => return Ok(Box::new(backend) as Box<dyn HotkeyBackend>),
        Err(err) => {
            warn!("swhkd backend unavailable: {}", err);
            errors.push(format!("swhkd: {err}"));
        }
    }

    Err(format!(
        "no Wayland hotkey backend available ({})",
        errors.join("; ")
    ))
}

fn select_x11_backend() -> Result<Box<dyn HotkeyBackend>, String> {
    let mut errors = Vec::new();

    match X11Backend::new() {
        Ok(backend) => return Ok(Box::new(backend) as Box<dyn HotkeyBackend>),
        Err(err) => {
            warn!("X11 backend unavailable: {}", err);
            errors.push(format!("x11: {err}"));
        }
    }

    match SwhkdBackend::new() {
        Ok(backend) => Ok(Box::new(backend) as Box<dyn HotkeyBackend>),
        Err(err) => {
            warn!("swhkd backend unavailable: {}", err);
            errors.push(format!("swhkd: {err}"));
            Err(format!(
                "no X11 hotkey backend available ({})",
                errors.join("; ")
            ))
        }
    }
}
