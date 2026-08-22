use log::{debug, info, warn};
use parking_lot::Mutex;
use std::any::Any;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use x11::xinput2;
use x11::xlib;

use crate::app_meta::{BACKEND_ENV_VAR, FORCE_X11_ENV_VAR, X11_BACKEND};

use super::backend_runtime::{try_dispatch_hotkey, HotkeyBackend};
use super::error::{hotkey_conflict, HotkeyError};
use super::{normalize_capture_key, parse_hotkey_spec, HotkeyCode, HotkeyModifier, HotkeySpec};

pub struct X11Backend {
    bindings: Arc<Mutex<BindingIndex>>,
    staged_bindings: Mutex<Option<BindingIndex>>,
    started: AtomicBool,
    stop_flag: Arc<AtomicBool>,
    display_ptr: Arc<Mutex<Option<NonNullXDisplay>>>,
    wake_reader: Mutex<Option<UnixStream>>,
    wake_writer: Mutex<UnixStream>,
}

#[derive(Debug)]
struct NonNullXDisplay(*mut xlib::Display);

// SAFETY: the owning mutex serializes Xlib calls.
unsafe impl Send for NonNullXDisplay {}
// SAFETY: same Mutex, same reason.
unsafe impl Sync for NonNullXDisplay {}

impl NonNullXDisplay {
    // SAFETY: caller holds the Xlib mutex and the display is open.
    unsafe fn close(self) {
        xlib::XCloseDisplay(self.0);
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Chord {
    key: HotkeyCode,
    modifiers: u8,
}

impl Chord {
    fn from_spec(spec: HotkeySpec) -> Self {
        Self {
            key: spec.key,
            modifiers: spec
                .modifiers
                .into_iter()
                .fold(0, |mask, modifier| mask | modifier_mask(modifier)),
        }
    }
}

#[derive(Default)]
struct BindingIndex {
    by_id: HashMap<String, Chord>,
    by_chord: HashMap<Chord, String>,
}

impl BindingIndex {
    fn insert(&mut self, id: &str, chord: Chord) -> Result<(), HotkeyError> {
        if let Some(existing_id) = self.by_chord.get(&chord) {
            if existing_id != id {
                return Err(hotkey_conflict(existing_id));
            }
        }
        if let Some(previous) = self.by_id.insert(id.to_string(), chord) {
            self.by_chord.remove(&previous);
        }
        self.by_chord.insert(chord, id.to_string());
        Ok(())
    }

    fn remove(&mut self, id: &str) {
        if let Some(chord) = self.by_id.remove(id) {
            self.by_chord.remove(&chord);
        }
    }

    fn id_for(&self, chord: Chord) -> Option<&str> {
        self.by_chord.get(&chord).map(String::as_str)
    }
}

impl X11Backend {
    #[allow(clippy::multiple_unsafe_ops_per_block)]
    pub fn new() -> Result<Self, HotkeyError> {
        if should_disable_x11_backend() {
            return Err(HotkeyError::BackendUnavailable(
                "Wayland session detected; X11 backend disabled unless X11 is explicitly selected"
                    .to_string(),
            ));
        }

        if std::env::var("DISPLAY").is_err() {
            return Err(HotkeyError::BackendUnavailable(
                "DISPLAY not set; X11 backend unavailable".to_string(),
            ));
        }

        // SAFETY: display stays local and closes on every exit.
        unsafe {
            let display = xlib::XOpenDisplay(ptr::null());
            if display.is_null() {
                return Err(HotkeyError::BackendUnavailable(
                    "Failed to open X11 display".to_string(),
                ));
            }

            let mut opcode = 0;
            let mut event = 0;
            let mut error = 0;
            let ext_name = CString::new("XInputExtension").unwrap();
            let xi_available = xlib::XQueryExtension(
                display,
                ext_name.as_ptr(),
                &mut opcode,
                &mut event,
                &mut error,
            ) != 0;
            xlib::XCloseDisplay(display);

            if !xi_available {
                return Err(HotkeyError::BackendUnavailable(
                    "XInput2 extension not available".to_string(),
                ));
            }
        }

        let (wake_reader, wake_writer) = UnixStream::pair().map_err(|error| {
            HotkeyError::Io(format!("Failed to create X11 wake socket: {error}"))
        })?;

        Ok(Self {
            bindings: Arc::new(Mutex::new(BindingIndex::default())),
            staged_bindings: Mutex::new(None),
            started: AtomicBool::new(false),
            stop_flag: Arc::new(AtomicBool::new(false)),
            display_ptr: Arc::new(Mutex::new(None)),
            wake_reader: Mutex::new(Some(wake_reader)),
            wake_writer: Mutex::new(wake_writer),
        })
    }

    // SAFETY: display stays live; copy Xlib-owned bytes before returning.
    unsafe fn keycode_to_name(display: *mut xlib::Display, keycode: u32) -> Option<String> {
        let keysym = xlib::XKeycodeToKeysym(display, keycode as u8, 0);
        if keysym == 0 {
            return None;
        }

        let ptr = xlib::XKeysymToString(keysym);
        if ptr.is_null() {
            return None;
        }
        Some(CStr::from_ptr(ptr).to_string_lossy().into_owned())
    }

    fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop_flag)
    }

    fn display_ptr(&self) -> Arc<Mutex<Option<NonNullXDisplay>>> {
        Arc::clone(&self.display_ptr)
    }
}

fn is_wayland_session(xdg_session_type: Option<&str>, has_wayland_display: bool) -> bool {
    match xdg_session_type
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("wayland") => return true,
        Some("x11") => return false,
        _ => {}
    }

    has_wayland_display
}

fn session_is_wayland() -> bool {
    let session_type = std::env::var("XDG_SESSION_TYPE").ok();
    is_wayland_session(
        session_type.as_deref(),
        std::env::var("WAYLAND_DISPLAY").is_ok(),
    )
}

fn is_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn x11_requested(backend: Option<&str>, force_x11: Option<&str>) -> bool {
    backend
        .map(|value| value.trim().eq_ignore_ascii_case(X11_BACKEND))
        .unwrap_or(false)
        || force_x11.map(is_truthy).unwrap_or(false)
}

fn explicit_x11_requested() -> bool {
    let backend = std::env::var(BACKEND_ENV_VAR).ok();
    let force_x11 = std::env::var(FORCE_X11_ENV_VAR).ok();
    x11_requested(backend.as_deref(), force_x11.as_deref())
}

#[cfg(test)]
fn should_disable_x11_backend_for_env(
    xdg_session_type: Option<&str>,
    has_wayland_display: bool,
    backend: Option<&str>,
    force_x11: Option<&str>,
) -> bool {
    is_wayland_session(xdg_session_type, has_wayland_display) && !x11_requested(backend, force_x11)
}

fn should_disable_x11_backend() -> bool {
    session_is_wayland() && !explicit_x11_requested()
}

impl Drop for X11Backend {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = self.wake_writer.lock().write_all(&[1]);
    }
}

impl HotkeyBackend for X11Backend {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), HotkeyError> {
        let spec = parse_hotkey_spec(hotkey)?;
        debug!(
            "X11 register request: id='{}' key='{}' modifiers={:?}",
            sound_id,
            spec.key.token(),
            spec.modifiers
        );

        self.bindings
            .lock()
            .insert(sound_id, Chord::from_spec(spec))
    }

    fn stage_many(&self, bindings: &[(String, String)]) -> Result<(), HotkeyError> {
        let mut staged = self.staged_bindings.lock();
        let staged = staged.get_or_insert_with(BindingIndex::default);
        for (binding_id, hotkey) in bindings {
            staged.insert(binding_id, Chord::from_spec(parse_hotkey_spec(hotkey)?))?;
        }
        Ok(())
    }

    fn commit_staged(&self) -> Result<(), HotkeyError> {
        if let Some(staged) = self.staged_bindings.lock().take() {
            *self.bindings.lock() = staged;
        }
        Ok(())
    }

    fn abort_staged(&self) {
        self.staged_bindings.lock().take();
    }

    fn unregister(&self, sound_id: &str) -> Result<(), HotkeyError> {
        self.bindings.lock().remove(sound_id);
        Ok(())
    }

    fn unregister_many(&self, sound_ids: &[String]) -> Result<(), HotkeyError> {
        if sound_ids.is_empty() {
            return Ok(());
        }

        let mut bindings = self.bindings.lock();
        for sound_id in sound_ids {
            bindings.remove(sound_id);
        }
        Ok(())
    }

    #[allow(clippy::multiple_unsafe_ops_per_block)]
    fn start_listener(&self, sender: SyncSender<String>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let bindings = Arc::clone(&self.bindings);
        let stop_flag = self.stop_flag();
        let display_ptr = self.display_ptr();
        let Some(mut wake_reader) = self.wake_reader.lock().take() else {
            warn!("X11 backend listener has no wake socket");
            return;
        };

        // SAFETY: this thread owns the display until storing it for Drop.
        thread::spawn(move || unsafe {
            let display = xlib::XOpenDisplay(ptr::null());
            if display.is_null() {
                warn!("X11 backend listener failed: cannot open display");
                return;
            }

            *display_ptr.lock() = Some(NonNullXDisplay(display));

            let root = xlib::XDefaultRootWindow(display);

            let mask_len = ((xinput2::XI_LASTEVENT + 7) / 8) as usize;
            let mut mask = vec![0u8; mask_len.max(2)];
            xinput2::XISetMask(&mut mask, xinput2::XI_RawKeyPress);
            xinput2::XISetMask(&mut mask, xinput2::XI_RawKeyRelease);

            let mut event_mask = xinput2::XIEventMask {
                deviceid: xinput2::XIAllMasterDevices,
                mask_len: mask.len() as i32,
                mask: mask.as_mut_ptr(),
            };

            xinput2::XISelectEvents(display, root, &mut event_mask, 1);
            xlib::XFlush(display);
            info!("X11 backend listener started");

            let mut active_mods = 0_u8;
            let mut event: xlib::XEvent = std::mem::zeroed();

            let connection_fd = xlib::XConnectionNumber(display);
            loop {
                // SAFETY: Xlib owns the fd while the display is open.
                let x_fd = BorrowedFd::borrow_raw(connection_fd);
                let mut poll_fds = [
                    nix::poll::PollFd::new(x_fd, nix::poll::PollFlags::POLLIN),
                    nix::poll::PollFd::new(wake_reader.as_fd(), nix::poll::PollFlags::POLLIN),
                ];
                match nix::poll::poll(&mut poll_fds, nix::poll::PollTimeout::NONE) {
                    Ok(_) => {}
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(error) => {
                        warn!("X11 listener poll failed: {error}");
                        break;
                    }
                }
                if poll_fds[1]
                    .revents()
                    .is_some_and(|events| events.contains(nix::poll::PollFlags::POLLIN))
                {
                    let mut byte = [0_u8; 1];
                    let _ = wake_reader.read(&mut byte);
                    info!("X11 listener received stop signal");
                    break;
                }
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }

                xlib::XNextEvent(display, &mut event);

                if event.get_type() != xlib::GenericEvent {
                    continue;
                }

                let mut cookie = event.generic_event_cookie;
                if xlib::XGetEventData(display, &mut cookie) == 0 {
                    continue;
                }

                if cookie.evtype == xinput2::XI_RawKeyPress
                    || cookie.evtype == xinput2::XI_RawKeyRelease
                {
                    let raw = &*(cookie.data as *const xinput2::XIRawEvent);
                    let is_press = cookie.evtype == xinput2::XI_RawKeyPress;
                    let keycode = raw.detail as u32;

                    if let Some(key_name) = X11Backend::keycode_to_name(display, keycode) {
                        match key_name.as_str() {
                            "Control_L" | "Control_R" => {
                                update_modifier(&mut active_mods, HotkeyModifier::Ctrl, is_press)
                            }
                            "Alt_L" | "Alt_R" => {
                                update_modifier(&mut active_mods, HotkeyModifier::Alt, is_press)
                            }
                            "Shift_L" | "Shift_R" => {
                                update_modifier(&mut active_mods, HotkeyModifier::Shift, is_press)
                            }
                            "Super_L" | "Super_R" | "Meta_L" | "Meta_R" => {
                                update_modifier(&mut active_mods, HotkeyModifier::Super, is_press)
                            }
                            "ISO_Level3_Shift" => {
                                update_modifier(&mut active_mods, HotkeyModifier::AltGr, is_press)
                            }
                            _ if is_press => {
                                if let Some(code) = normalize_capture_key(&key_name, keycode) {
                                    let chord = Chord {
                                        key: code,
                                        modifiers: active_mods,
                                    };
                                    let id = bindings.lock().id_for(chord).map(str::to_string);
                                    if let Some(id) = id {
                                        debug!("X11 hotkey triggered: {}", id);
                                        if !try_dispatch_hotkey(&sender, id) {
                                            debug!("Dropped X11 hotkey repeat because the queue is full");
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }

                xlib::XFreeEventData(display, &mut cookie);
            }

            if let Some(disp) = display_ptr.lock().take() {
                disp.close();
                debug!("X11 display closed on listener thread exit");
            }
        });
    }
}

fn modifier_mask(modifier: HotkeyModifier) -> u8 {
    match modifier {
        HotkeyModifier::Ctrl => 1 << 0,
        HotkeyModifier::Alt => 1 << 1,
        HotkeyModifier::Shift => 1 << 2,
        HotkeyModifier::Super => 1 << 3,
        HotkeyModifier::AltGr => 1 << 4,
    }
}

fn update_modifier(active: &mut u8, modifier: HotkeyModifier, pressed: bool) {
    let mask = modifier_mask(modifier);
    if pressed {
        *active |= mask;
    } else {
        *active &= !mask;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_index_resolves_chords_without_scanning_ids() {
        let mut index = BindingIndex::default();
        let chord = Chord::from_spec(parse_hotkey_spec("Ctrl+Alt+KeyP").unwrap());
        index.insert("sound-1", chord).unwrap();

        assert_eq!(index.id_for(chord), Some("sound-1"));
        assert!(index
            .insert("sound-2", chord)
            .unwrap_err()
            .to_string()
            .contains("sound-1"));

        index.remove("sound-1");
        assert_eq!(index.id_for(chord), None);
    }

    #[test]
    fn test_x11_backend_creation_without_display() {
        let result = X11Backend::new();
        match result {
            Ok(backend) => {
                drop(backend);
            }
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("DISPLAY") || msg.contains("X11"));
            }
        }
    }

    #[test]
    fn test_stop_flag_creation() {
        let result = X11Backend::new();
        if result.is_ok() {
            let backend = result.unwrap();
            let stop_flag = backend.stop_flag();
            assert!(
                !stop_flag.load(Ordering::SeqCst),
                "Stop flag should start as false"
            );
        }
    }

    #[test]
    fn test_display_ptr_initially_none() {
        let result = X11Backend::new();
        if result.is_ok() {
            let backend = result.unwrap();
            let display_ptr = backend.display_ptr();
            let guard = display_ptr.lock();
            assert!(
                guard.is_none(),
                "Display pointer should be None before listener starts"
            );
        }
    }

    #[test]
    fn wayland_session_disables_x11_by_default() {
        assert!(should_disable_x11_backend_for_env(
            Some("wayland"),
            true,
            None,
            None
        ));
    }

    #[test]
    fn explicit_gtk_x11_allows_x11_backend_in_wayland_session() {
        assert!(!should_disable_x11_backend_for_env(
            Some("wayland"),
            true,
            Some("x11"),
            None
        ));
    }

    #[test]
    fn force_x11_allows_x11_backend_in_wayland_session() {
        assert!(!should_disable_x11_backend_for_env(
            Some("wayland"),
            true,
            None,
            Some("1")
        ));
    }

    #[test]
    fn false_force_x11_does_not_allow_x11_backend_in_wayland_session() {
        assert!(should_disable_x11_backend_for_env(
            Some("wayland"),
            true,
            None,
            Some("false")
        ));
    }

    #[test]
    fn x11_session_allows_x11_backend_even_if_wayland_display_exists() {
        assert!(!should_disable_x11_backend_for_env(
            Some("x11"),
            true,
            None,
            None
        ));
    }
}
