use log::{info, warn};
use std::sync::mpsc::SyncSender;

use crate::app_meta::{BACKEND_ENV_VAR, WAYLAND_BACKEND, X11_BACKEND};

use super::backend_runtime::HotkeyBackend;
use super::error::{unsupported_key_for_backend, HotkeyError};
use super::parse_hotkey_spec;
use super::swhkd_backend::SwhkdBackend;
use super::x11_backend::X11Backend;

pub struct HotkeyManager {
    backend: Option<Box<dyn HotkeyBackend>>,
    disabled_reason: Option<String>,
    deferred_sender: Option<SyncSender<String>>,
    deferred_start: bool,
}

#[cfg(test)]
struct NoopHotkeyBackend;

#[cfg(test)]
impl HotkeyBackend for NoopHotkeyBackend {
    fn name(&self) -> &'static str {
        "test-noop"
    }

    fn register(&self, _binding_id: &str, _hotkey: &str) -> Result<(), HotkeyError> {
        Ok(())
    }

    fn unregister(&self, _binding_id: &str) -> Result<(), HotkeyError> {
        Ok(())
    }

    fn start_listener(&self, _sender: SyncSender<String>) {}

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl HotkeyManager {
    pub fn new_deferred(sender: SyncSender<String>) -> Self {
        info!("Initializing hotkey backend manager");
        info!("Deferring hotkey backend startup until persisted bindings are projected");
        Self {
            backend: None,
            disabled_reason: None,
            deferred_sender: Some(sender),
            deferred_start: true,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_test_noop() -> Self {
        Self {
            backend: Some(Box::new(NoopHotkeyBackend)),
            disabled_reason: None,
            deferred_sender: None,
            deferred_start: false,
        }
    }

    pub fn project_hotkey_pages_blocking<F>(
        &mut self,
        mut next_page: F,
    ) -> Result<usize, HotkeyError>
    where
        F: FnMut() -> Result<Option<Vec<(String, String)>>, HotkeyError>,
    {
        let mut projected = 0_usize;
        let mut staged = self.backend.is_some();
        let mut first_error = None;
        if let Some(backend) = &self.backend {
            backend.begin_staged()?;
        }
        loop {
            let page = match next_page() {
                Ok(Some(page)) => page,
                Ok(None) => break,
                Err(error) => {
                    if let Some(backend) = &self.backend {
                        backend.abort_staged();
                    }
                    return Err(error);
                }
            };
            if page.is_empty() {
                continue;
            }
            self.ensure_backend_started()?;
            let backend = self.backend.as_ref().ok_or_else(|| {
                HotkeyError::BackendUnavailable("Global hotkeys unavailable".to_string())
            })?;
            if !staged {
                backend.begin_staged()?;
                staged = true;
            }
            projected = projected.saturating_add(page.len());
            if let Err(error) = backend.stage_many(&page) {
                first_error.get_or_insert(error);
            }
        }
        if staged {
            self.backend
                .as_ref()
                .ok_or_else(|| {
                    HotkeyError::BackendUnavailable("Global hotkeys unavailable".to_string())
                })?
                .commit_staged()?;
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(projected)
        }
    }

    pub fn register_hotkey_blocking(
        &mut self,
        sound_id: &str,
        hotkey: &str,
    ) -> Result<(), HotkeyError> {
        self.ensure_backend_started()?;
        self.backend
            .as_ref()
            .ok_or_else(|| {
                HotkeyError::BackendUnavailable(format!(
                    "Global hotkeys unavailable: {}",
                    self.disabled_reason.as_deref().unwrap_or("unknown")
                ))
            })
            .and_then(|backend| backend.register(sound_id, hotkey))
    }

    pub fn validate_hotkey_blocking(&mut self, hotkey: &str) -> Result<(), HotkeyError> {
        if let Some(backend) = &self.backend {
            return backend.validate_hotkey(hotkey);
        }

        Self::validate_without_starting_backend(hotkey)
    }

    pub fn unregister_hotkey_blocking(&mut self, sound_id: &str) -> Result<(), HotkeyError> {
        if let Some(backend) = &self.backend {
            backend.unregister(sound_id)
        } else {
            Ok(())
        }
    }

    pub fn unregister_hotkeys_blocking(&mut self, sound_ids: &[String]) -> Result<(), HotkeyError> {
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

    /// Kill the active backend so we don't leave orphan daemons behind.
    pub fn shutdown(&mut self) {
        if let Some(backend) = &self.backend {
            backend.shutdown();
        }
    }

    fn ensure_backend_started(&mut self) -> Result<(), HotkeyError> {
        if self.backend.is_some() {
            return Ok(());
        }
        if !self.deferred_start {
            return Err(HotkeyError::BackendUnavailable(format!(
                "Global hotkeys unavailable: {}",
                self.disabled_reason.as_deref().unwrap_or("unknown")
            )));
        }

        let sender = self.deferred_sender.take().ok_or_else(|| {
            HotkeyError::BackendUnavailable(
                "Global hotkeys unavailable: missing listener channel".to_string(),
            )
        })?;

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
                let reason_str = reason.to_string();
                self.disabled_reason = Some(reason_str.clone());
                // Keep state for a retry after install.
                self.deferred_sender = Some(sender);
                self.deferred_start = true;
                Err(HotkeyError::BackendUnavailable(format!(
                    "Global hotkeys unavailable: {}",
                    reason_str
                )))
            }
        }
    }

    fn select_backend() -> Result<Box<dyn HotkeyBackend>, HotkeyError> {
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
                Err(HotkeyError::BackendUnavailable(format!(
                    "no backend available ({})",
                    errors.join("; ")
                )))
            }
        }
    }

    fn validate_without_starting_backend(hotkey: &str) -> Result<(), HotkeyError> {
        let spec = parse_hotkey_spec(hotkey).map_err(|e| {
            unsupported_key_for_backend("hotkey", format!("{hotkey} is invalid. {e}"))
        })?;

        match session_backend_preference() {
            BackendPreference::Wayland => spec
                .swhkd_string()
                .map(|_| ())
                .map_err(|detail| unsupported_key_for_backend("swhkd", detail.to_string())),
            BackendPreference::X11 | BackendPreference::Auto => Ok(()),
        }
    }

    pub fn is_healthy(&self) -> Result<(), HotkeyError> {
        match &self.backend {
            Some(backend) => {
                if let Some(swhkd) = (**backend).as_any().downcast_ref::<SwhkdBackend>() {
                    swhkd.is_healthy()
                } else {
                    Ok(())
                }
            }
            None => Err(HotkeyError::BackendUnavailable(format!(
                "Hotkeys unavailable: {}",
                self.disabled_reason.as_deref().unwrap_or("unknown")
            ))),
        }
    }

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

fn select_wayland_backend() -> Result<Box<dyn HotkeyBackend>, HotkeyError> {
    let mut errors = Vec::new();

    match SwhkdBackend::new() {
        Ok(backend) => return Ok(Box::new(backend) as Box<dyn HotkeyBackend>),
        Err(err) => {
            warn!("swhkd backend unavailable: {}", err);
            errors.push(format!("swhkd: {err}"));
        }
    }

    Err(HotkeyError::BackendUnavailable(format!(
        "no Wayland hotkey backend available ({})",
        errors.join("; ")
    )))
}

fn select_x11_backend() -> Result<Box<dyn HotkeyBackend>, HotkeyError> {
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
            Err(HotkeyError::BackendUnavailable(format!(
                "no X11 hotkey backend available ({})",
                errors.join("; ")
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::any::Any;
    use std::collections::VecDeque;
    use std::sync::Arc;

    #[derive(Default)]
    struct ProjectionState {
        page_sizes: Vec<usize>,
        commits: usize,
    }

    struct ProjectionBackend(Arc<parking_lot::Mutex<ProjectionState>>);

    impl HotkeyBackend for ProjectionBackend {
        fn name(&self) -> &'static str {
            "projection-test"
        }

        fn register(&self, _binding_id: &str, _hotkey: &str) -> Result<(), HotkeyError> {
            Ok(())
        }

        fn stage_many(&self, bindings: &[(String, String)]) -> Result<(), HotkeyError> {
            self.0.lock().page_sizes.push(bindings.len());
            Ok(())
        }

        fn commit_staged(&self) -> Result<(), HotkeyError> {
            self.0.lock().commits += 1;
            Ok(())
        }

        fn unregister(&self, _binding_id: &str) -> Result<(), HotkeyError> {
            Ok(())
        }

        fn start_listener(&self, _sender: SyncSender<String>) {}

        fn as_any(&self) -> &dyn Any {
            self
        }
    }

    #[test]
    fn paged_projection_commits_once_after_all_pages() {
        let projection = Arc::new(parking_lot::Mutex::new(ProjectionState::default()));
        let mut manager = HotkeyManager {
            backend: Some(Box::new(ProjectionBackend(Arc::clone(&projection)))),
            disabled_reason: None,
            deferred_sender: None,
            deferred_start: false,
        };
        let mut pages = VecDeque::from([
            Ok(Some(vec![("one".to_string(), "Ctrl+KeyA".to_string())])),
            Ok(Some(vec![
                ("two".to_string(), "Ctrl+KeyB".to_string()),
                ("three".to_string(), "Ctrl+KeyC".to_string()),
            ])),
            Ok(None),
        ]);

        let projected = manager
            .project_hotkey_pages_blocking(|| pages.pop_front().expect("projection page"))
            .expect("project pages");

        assert_eq!(projected, 3);
        let projection = projection.lock();
        assert_eq!(projection.page_sizes, [0, 1, 2]);
        assert_eq!(projection.commits, 1);
    }

    #[test]
    fn empty_projection_clears_an_active_backend() {
        let projection = Arc::new(parking_lot::Mutex::new(ProjectionState::default()));
        let mut manager = HotkeyManager {
            backend: Some(Box::new(ProjectionBackend(Arc::clone(&projection)))),
            disabled_reason: None,
            deferred_sender: None,
            deferred_start: false,
        };

        assert_eq!(
            manager
                .project_hotkey_pages_blocking(|| Ok(None))
                .expect("project empty desired state"),
            0
        );
        let projection = projection.lock();
        assert_eq!(projection.page_sizes, [0]);
        assert_eq!(projection.commits, 1);
    }
}
