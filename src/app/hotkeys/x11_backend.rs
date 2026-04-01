use log::{debug, info, warn};
use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::ffi::{CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use x11::xinput2;
use x11::xlib;

use super::backend_runtime::HotkeyBackend;
use super::error::hotkey_conflict;
use super::{normalize_capture_key, parse_hotkey_spec, HotkeyCode, HotkeyModifier};

/// X11 hotkey backend with cleanup tracking.
pub struct X11Backend {
    bindings: Arc<Mutex<HashMap<String, Binding>>>,
    started: AtomicBool,
    stop_flag: Arc<AtomicBool>,
    display_ptr: Arc<Mutex<Option<NonNullXDisplay>>>,
}

/// Wrapper for an X11 display pointer.
#[derive(Debug)]
struct NonNullXDisplay(*mut xlib::Display);

unsafe impl Send for NonNullXDisplay {}
unsafe impl Sync for NonNullXDisplay {}

impl NonNullXDisplay {
    unsafe fn close(self) {
        xlib::XCloseDisplay(self.0);
    }
}

#[derive(Clone, PartialEq, Eq)]
struct Binding {
    key: HotkeyCode,
    modifiers: Vec<HotkeyModifier>,
}

impl X11Backend {
    /// Create a new X11 backend.
    pub fn new() -> Result<Self, String> {
        if session_is_wayland() {
            return Err("Wayland session detected; X11 backend disabled".to_string());
        }

        if std::env::var("DISPLAY").is_err() {
            return Err("DISPLAY not set; X11 backend unavailable".to_string());
        }

        unsafe {
            let display = xlib::XOpenDisplay(ptr::null());
            if display.is_null() {
                return Err("Failed to open X11 display".to_string());
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
                return Err("XInput2 extension not available".to_string());
            }
        }

        Ok(Self {
            bindings: Arc::new(Mutex::new(HashMap::new())),
            started: AtomicBool::new(false),
            stop_flag: Arc::new(AtomicBool::new(false)),
            display_ptr: Arc::new(Mutex::new(None)),
        })
    }

    fn modifiers_match(expected: &[HotkeyModifier], active: &HashSet<HotkeyModifier>) -> bool {
        expected.len() == active.len() && expected.iter().all(|modifier| active.contains(modifier))
    }

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

    /// Clone the stop flag.
    fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop_flag)
    }

    /// Clone the display tracker.
    fn display_ptr(&self) -> Arc<Mutex<Option<NonNullXDisplay>>> {
        Arc::clone(&self.display_ptr)
    }
}

fn session_is_wayland() -> bool {
    match std::env::var("XDG_SESSION_TYPE")
        .ok()
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("wayland") => return true,
        Some("x11") => return false,
        _ => {}
    }

    std::env::var("WAYLAND_DISPLAY").is_ok()
}

impl Drop for X11Backend {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);

        if let Ok(mut display_guard) = self.display_ptr.lock() {
            if let Some(display) = display_guard.take() {
                unsafe {
                    display.close();
                    debug!("X11 display closed via Drop");
                }
            }
        }
    }
}

impl HotkeyBackend for X11Backend {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        let spec = parse_hotkey_spec(hotkey)?;
        debug!(
            "X11 register request: id='{}' key='{}' modifiers={:?}",
            sound_id,
            spec.key.token(),
            spec.modifiers
        );

        let mut bindings = self
            .bindings
            .lock()
            .map_err(|e| format!("Bindings lock poisoned: {}", e))?;
        for (id, existing) in bindings.iter() {
            if id != sound_id && existing.key == spec.key && existing.modifiers == spec.modifiers {
                return Err(hotkey_conflict(id));
            }
        }

        bindings.insert(
            sound_id.to_string(),
            Binding {
                key: spec.key,
                modifiers: spec.modifiers,
            },
        );
        Ok(())
    }

    fn unregister(&self, sound_id: &str) -> Result<(), String> {
        self.bindings
            .lock()
            .map_err(|e| format!("Bindings lock poisoned: {}", e))?
            .remove(sound_id);
        Ok(())
    }

    fn unregister_many(&self, sound_ids: &[String]) -> Result<(), String> {
        if sound_ids.is_empty() {
            return Ok(());
        }

        let mut bindings = self
            .bindings
            .lock()
            .map_err(|e| format!("Bindings lock poisoned: {}", e))?;
        for sound_id in sound_ids {
            bindings.remove(sound_id);
        }
        Ok(())
    }

    fn start_listener(&self, sender: Sender<String>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let bindings = Arc::clone(&self.bindings);
        let stop_flag = self.stop_flag();
        let display_ptr = self.display_ptr();

        thread::spawn(move || unsafe {
            let display = xlib::XOpenDisplay(ptr::null());
            if display.is_null() {
                warn!("X11 backend listener failed: cannot open display");
                return;
            }

            if let Ok(mut ptr_guard) = display_ptr.lock() {
                *ptr_guard = Some(NonNullXDisplay(display));
            }

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

            let mut active_mods = HashSet::new();
            let mut event: xlib::XEvent = std::mem::zeroed();

            loop {
                if stop_flag.load(Ordering::SeqCst) {
                    info!("X11 listener received stop signal");
                    break;
                }

                while xlib::XPending(display) == 0 {
                    if stop_flag.load(Ordering::SeqCst) {
                        info!("X11 listener received stop signal (in pending loop)");
                        break;
                    }
                    thread::sleep(std::time::Duration::from_millis(10));
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
                                    let snapshot: Vec<(String, Binding)> = match bindings.lock() {
                                        Ok(guard) => guard
                                            .iter()
                                            .map(|(id, binding)| (id.clone(), binding.clone()))
                                            .collect(),
                                        Err(e) => {
                                            warn!("Bindings lock poisoned in X11 listener: {}", e);
                                            Vec::new()
                                        }
                                    };

                                    for (id, binding) in snapshot {
                                        if binding.key == code
                                            && X11Backend::modifiers_match(
                                                &binding.modifiers,
                                                &active_mods,
                                            )
                                        {
                                            debug!("X11 hotkey triggered: {}", id);
                                            let _ = sender.send(id);
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

            if let Ok(mut ptr_guard) = display_ptr.lock() {
                if let Some(disp) = ptr_guard.take() {
                    disp.close();
                    debug!("X11 display closed on listener thread exit");
                }
            }
        });
    }
}

fn update_modifier(active: &mut HashSet<HotkeyModifier>, modifier: HotkeyModifier, pressed: bool) {
    if pressed {
        active.insert(modifier);
    } else {
        active.remove(&modifier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_x11_backend_creation_without_display() {
        let result = X11Backend::new();
        match result {
            Ok(backend) => {
                drop(backend);
            }
            Err(e) => {
                assert!(e.contains("DISPLAY") || e.contains("X11"));
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
            let guard = display_ptr.lock().unwrap();
            assert!(
                guard.is_none(),
                "Display pointer should be None before listener starts"
            );
        }
    }
}
