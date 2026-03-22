use log::{debug, info, warn};
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
use super::{normalize_capture_key, parse_hotkey_spec, HotkeyCode, HotkeyModifier};

pub struct X11Backend {
    bindings: Arc<Mutex<HashMap<String, Binding>>>,
    started: AtomicBool,
}

#[derive(Clone, PartialEq, Eq)]
struct Binding {
    key: HotkeyCode,
    modifiers: Vec<HotkeyModifier>,
}

impl X11Backend {
    pub fn new() -> Result<Self, String> {
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
}

impl HotkeyBackend for X11Backend {
    fn name(&self) -> &'static str {
        "x11"
    }

    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        let spec = parse_hotkey_spec(hotkey)?;

        let mut bindings = self.bindings.lock().unwrap();
        for (id, existing) in bindings.iter() {
            if id != sound_id && existing.key == spec.key && existing.modifiers == spec.modifiers {
                return Err(format!("HOTKEY_CONFLICT:{id}"));
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
        self.bindings.lock().unwrap().remove(sound_id);
        Ok(())
    }

    fn start_listener(&self, sender: Sender<String>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }

        let bindings = Arc::clone(&self.bindings);
        thread::spawn(move || unsafe {
            let display = xlib::XOpenDisplay(ptr::null());
            if display.is_null() {
                warn!("X11 backend listener failed: cannot open display");
                return;
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
                xlib::XNextEvent(display, &mut event);

                if event.get_type() != xlib::GenericEvent {
                    continue;
                }

                let mut cookie = event.generic_event_cookie;
                if xlib::XGetEventData(display, &mut cookie) == 0 {
                    continue;
                }

                if cookie.evtype == xinput2::XI_RawKeyPress || cookie.evtype == xinput2::XI_RawKeyRelease {
                    let raw = &*(cookie.data as *const xinput2::XIRawEvent);
                    let is_press = cookie.evtype == xinput2::XI_RawKeyPress;
                    let keycode = raw.detail as u32;

                    if let Some(key_name) = X11Backend::keycode_to_name(display, keycode) {
                        match key_name.as_str() {
                            "Control_L" | "Control_R" => update_modifier(&mut active_mods, HotkeyModifier::Ctrl, is_press),
                            "Alt_L" | "Alt_R" => update_modifier(&mut active_mods, HotkeyModifier::Alt, is_press),
                            "Shift_L" | "Shift_R" => update_modifier(&mut active_mods, HotkeyModifier::Shift, is_press),
                            "Super_L" | "Super_R" | "Meta_L" | "Meta_R" => {
                                update_modifier(&mut active_mods, HotkeyModifier::Super, is_press)
                            }
                            "ISO_Level3_Shift" => update_modifier(&mut active_mods, HotkeyModifier::AltGr, is_press),
                            _ if is_press => {
                                if let Some(code) = normalize_capture_key(&key_name, keycode) {
                                    let snapshot: Vec<(String, Binding)> = bindings
                                        .lock()
                                        .unwrap()
                                        .iter()
                                        .map(|(id, binding)| (id.clone(), binding.clone()))
                                        .collect();

                                    for (id, binding) in snapshot {
                                        if binding.key == code
                                            && X11Backend::modifiers_match(&binding.modifiers, &active_mods)
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
