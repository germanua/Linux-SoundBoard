//! Global hotkeys via evdev - works on both X11 and Wayland.
//!
//! Reads keyboard events directly from /dev/input devices.
//! Requires user to be in the 'input' group.

mod model;

use evdev::{Device, EventSummary, KeyCode};
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, thread};

pub use model::{
    canonicalize_hotkey_string, normalize_capture_key, parse_hotkey_spec, HotkeyCode,
    HotkeyModifier, HotkeySpec,
};

const INPUT_DIR: &str = "/dev/input";
const POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Key event values from evdev
const KEY_PRESS: i32 = 1;
const KEY_RELEASE: i32 = 0;

// Modifier keycodes
const CTRL_L: u16 = KeyCode::KEY_LEFTCTRL.0;
const CTRL_R: u16 = KeyCode::KEY_RIGHTCTRL.0;
const ALT_L: u16 = KeyCode::KEY_LEFTALT.0;
const ALT_R: u16 = KeyCode::KEY_RIGHTALT.0;
const SHIFT_L: u16 = KeyCode::KEY_LEFTSHIFT.0;
const SHIFT_R: u16 = KeyCode::KEY_RIGHTSHIFT.0;
const META_L: u16 = KeyCode::KEY_LEFTMETA.0;
const META_R: u16 = KeyCode::KEY_RIGHTMETA.0;
const CAPS: u16 = KeyCode::KEY_CAPSLOCK.0;
const NUMLOCK: u16 = KeyCode::KEY_NUMLOCK.0;

// ============================================================================
// Public API
// ============================================================================

pub struct HotkeyManager {
    backend: Option<EvdevBackend>,
    disabled_reason: Option<String>,
}

impl HotkeyManager {
    pub fn new_blocking(sender: Sender<String>, sounds: &[(String, String)]) -> Self {
        info!("Initializing evdev hotkey backend");

        match EvdevBackend::new() {
            Ok(backend) => {
                for (id, hk) in sounds {
                    if let Err(e) = backend.register(id, hk) {
                        warn!("Failed to register hotkey '{}' for '{}': {}", hk, id, e);
                    }
                }
                backend.start_listener(sender);
                Self { backend: Some(backend), disabled_reason: None }
            }
            Err(reason) => {
                warn!("Global hotkeys unavailable: {}", reason);
                Self { backend: None, disabled_reason: Some(reason) }
            }
        }
    }

    pub fn register_hotkey_blocking(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        self.backend.as_ref()
            .ok_or_else(|| format!("Global hotkeys unavailable: {}", self.disabled_reason.as_deref().unwrap_or("unknown")))
            .and_then(|b| b.register(sound_id, hotkey))
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
}

// ============================================================================
// Backend Implementation
// ============================================================================

struct EvdevBackend {
    bindings: Arc<Mutex<HashMap<String, Binding>>>,
    devices: Vec<String>,
    started: AtomicBool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Binding {
    keycode: u16,
    modifiers: Vec<HotkeyModifier>,
}

impl EvdevBackend {
    fn new() -> Result<Self, String> {
        let devices = find_keyboards()?;
        if devices.is_empty() {
            return Err("No keyboard devices found. Ensure you are in the 'input' group.".into());
        }
        info!("Found {} keyboard device(s)", devices.len());
        Ok(Self {
            bindings: Arc::new(Mutex::new(HashMap::new())),
            devices,
            started: AtomicBool::new(false),
        })
    }

    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        let spec = parse_hotkey_spec(hotkey)?;
        let binding = Binding {
            keycode: to_evdev_keycode(spec.key)?,
            modifiers: spec.modifiers.clone(),
        };

        let mut bindings = self.bindings.lock().unwrap();
        
        // Check for conflicts
        for (id, other) in bindings.iter() {
            if id != sound_id && other.keycode == binding.keycode && other.modifiers == binding.modifiers {
                return Err(format!("HOTKEY_CONFLICT:{id}"));
            }
        }

        debug!("Registered hotkey '{}' -> evdev {} {:?}", hotkey, binding.keycode, binding.modifiers);
        bindings.insert(sound_id.to_string(), binding);
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
        let devices = self.devices.clone();

        thread::spawn(move || run_event_loop(devices, bindings, sender));
    }
}

fn run_event_loop(
    device_paths: Vec<String>,
    bindings: Arc<Mutex<HashMap<String, Binding>>>,
    sender: Sender<String>,
) {
    let mut devices: Vec<Device> = device_paths.iter()
        .filter_map(|path| {
            Device::open(path).ok().and_then(|dev| {
                dev.set_nonblocking(true).ok()?;
                debug!("Listening on: {} ({})", path, dev.name().unwrap_or("?"));
                Some(dev)
            })
        })
        .collect();

    if devices.is_empty() {
        warn!("No keyboard devices could be opened");
        return;
    }

    let mut pressed: HashSet<u16> = HashSet::new();
    let mut active: HashSet<String> = HashSet::new();

    loop {
        for device in &mut devices {
            if let Ok(events) = device.fetch_events() {
                for event in events {
                    if let EventSummary::Key(_, key, value) = event.destructure() {
                        match value {
                            KEY_PRESS => { pressed.insert(key.0); }
                            KEY_RELEASE => { pressed.remove(&key.0); }
                            _ => continue,
                        }

                        let matches = find_matches(&pressed, &bindings.lock().unwrap());
                        for id in matches.difference(&active) {
                            debug!("Hotkey triggered: {}", id);
                            let _ = sender.send(id.clone());
                        }
                        active = matches;
                    }
                }
            }
        }
        thread::sleep(POLL_INTERVAL);
    }
}

// ============================================================================
// Matching Logic
// ============================================================================

fn find_matches(pressed: &HashSet<u16>, bindings: &HashMap<String, Binding>) -> HashSet<String> {
    let active_mods = detect_modifiers(pressed);
    let regular_keys: HashSet<u16> = pressed.iter()
        .copied()
        .filter(|&k| !is_modifier_or_lock(k))
        .collect();

    bindings.iter()
        .filter(|(_, b)| {
            regular_keys.len() == 1 
                && regular_keys.contains(&b.keycode)
                && b.modifiers == active_mods
        })
        .map(|(id, _)| id.clone())
        .collect()
}

fn detect_modifiers(pressed: &HashSet<u16>) -> Vec<HotkeyModifier> {
    let mut mods = Vec::new();
    if pressed.contains(&CTRL_L) || pressed.contains(&CTRL_R) {
        mods.push(HotkeyModifier::Ctrl);
    }
    if pressed.contains(&ALT_L) || pressed.contains(&ALT_R) {
        mods.push(HotkeyModifier::Alt);
    }
    if pressed.contains(&SHIFT_L) || pressed.contains(&SHIFT_R) {
        mods.push(HotkeyModifier::Shift);
    }
    if pressed.contains(&META_L) || pressed.contains(&META_R) {
        mods.push(HotkeyModifier::Super);
    }
    mods
}

fn is_modifier_or_lock(keycode: u16) -> bool {
    const MODS: [u16; 10] = [CTRL_L, CTRL_R, ALT_L, ALT_R, SHIFT_L, SHIFT_R, META_L, META_R, CAPS, NUMLOCK];
    MODS.contains(&keycode)
}

// ============================================================================
// Device Discovery
// ============================================================================

fn find_keyboards() -> Result<Vec<String>, String> {
    fs::read_dir(INPUT_DIR)
        .map_err(|e| format!("Cannot read {INPUT_DIR}: {e}. Ensure you are in the 'input' group."))?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            if !name.starts_with("event") {
                return None;
            }
            let device = Device::open(&path).ok()?;
            let keys = device.supported_keys()?;
            // A keyboard has letters and enter
            if keys.contains(KeyCode::KEY_A) && keys.contains(KeyCode::KEY_ENTER) {
                Some(path.to_string_lossy().into_owned())
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .pipe(Ok)
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R { f(self) }
}
impl<T> Pipe for T {}

// ============================================================================
// Keycode Mapping (token → evdev KeyCode)
// ============================================================================

fn to_evdev_keycode(key: HotkeyCode) -> Result<u16, String> {
    KEY_MAP.iter()
        .find(|(token, _)| *token == key.token())
        .map(|(_, code)| code.0)
        .ok_or_else(|| format!("Unsupported key: {}", key.token()))
}

static KEY_MAP: &[(&str, KeyCode)] = &[
    // Letters
    ("KeyA", KeyCode::KEY_A), ("KeyB", KeyCode::KEY_B), ("KeyC", KeyCode::KEY_C),
    ("KeyD", KeyCode::KEY_D), ("KeyE", KeyCode::KEY_E), ("KeyF", KeyCode::KEY_F),
    ("KeyG", KeyCode::KEY_G), ("KeyH", KeyCode::KEY_H), ("KeyI", KeyCode::KEY_I),
    ("KeyJ", KeyCode::KEY_J), ("KeyK", KeyCode::KEY_K), ("KeyL", KeyCode::KEY_L),
    ("KeyM", KeyCode::KEY_M), ("KeyN", KeyCode::KEY_N), ("KeyO", KeyCode::KEY_O),
    ("KeyP", KeyCode::KEY_P), ("KeyQ", KeyCode::KEY_Q), ("KeyR", KeyCode::KEY_R),
    ("KeyS", KeyCode::KEY_S), ("KeyT", KeyCode::KEY_T), ("KeyU", KeyCode::KEY_U),
    ("KeyV", KeyCode::KEY_V), ("KeyW", KeyCode::KEY_W), ("KeyX", KeyCode::KEY_X),
    ("KeyY", KeyCode::KEY_Y), ("KeyZ", KeyCode::KEY_Z),
    // Digits
    ("Digit0", KeyCode::KEY_0), ("Digit1", KeyCode::KEY_1), ("Digit2", KeyCode::KEY_2),
    ("Digit3", KeyCode::KEY_3), ("Digit4", KeyCode::KEY_4), ("Digit5", KeyCode::KEY_5),
    ("Digit6", KeyCode::KEY_6), ("Digit7", KeyCode::KEY_7), ("Digit8", KeyCode::KEY_8),
    ("Digit9", KeyCode::KEY_9),
    // Function keys
    ("F1", KeyCode::KEY_F1), ("F2", KeyCode::KEY_F2), ("F3", KeyCode::KEY_F3),
    ("F4", KeyCode::KEY_F4), ("F5", KeyCode::KEY_F5), ("F6", KeyCode::KEY_F6),
    ("F7", KeyCode::KEY_F7), ("F8", KeyCode::KEY_F8), ("F9", KeyCode::KEY_F9),
    ("F10", KeyCode::KEY_F10), ("F11", KeyCode::KEY_F11), ("F12", KeyCode::KEY_F12),
    // Navigation
    ("Escape", KeyCode::KEY_ESC), ("Tab", KeyCode::KEY_TAB), ("Enter", KeyCode::KEY_ENTER),
    ("Space", KeyCode::KEY_SPACE), ("Backspace", KeyCode::KEY_BACKSPACE),
    ("Insert", KeyCode::KEY_INSERT), ("Delete", KeyCode::KEY_DELETE),
    ("Home", KeyCode::KEY_HOME), ("End", KeyCode::KEY_END),
    ("PageUp", KeyCode::KEY_PAGEUP), ("PageDown", KeyCode::KEY_PAGEDOWN),
    ("ArrowUp", KeyCode::KEY_UP), ("ArrowDown", KeyCode::KEY_DOWN),
    ("ArrowLeft", KeyCode::KEY_LEFT), ("ArrowRight", KeyCode::KEY_RIGHT),
    // Numpad
    ("Numpad0", KeyCode::KEY_KP0), ("Numpad1", KeyCode::KEY_KP1), ("Numpad2", KeyCode::KEY_KP2),
    ("Numpad3", KeyCode::KEY_KP3), ("Numpad4", KeyCode::KEY_KP4), ("Numpad5", KeyCode::KEY_KP5),
    ("Numpad6", KeyCode::KEY_KP6), ("Numpad7", KeyCode::KEY_KP7), ("Numpad8", KeyCode::KEY_KP8),
    ("Numpad9", KeyCode::KEY_KP9), ("NumpadDecimal", KeyCode::KEY_KPDOT),
    ("NumpadDivide", KeyCode::KEY_KPSLASH), ("NumpadMultiply", KeyCode::KEY_KPASTERISK),
    ("NumpadSubtract", KeyCode::KEY_KPMINUS), ("NumpadAdd", KeyCode::KEY_KPPLUS),
    ("NumpadEnter", KeyCode::KEY_KPENTER),
    // Punctuation
    ("Minus", KeyCode::KEY_MINUS), ("Equal", KeyCode::KEY_EQUAL),
    ("BracketLeft", KeyCode::KEY_LEFTBRACE), ("BracketRight", KeyCode::KEY_RIGHTBRACE),
    ("Backslash", KeyCode::KEY_BACKSLASH), ("Semicolon", KeyCode::KEY_SEMICOLON),
    ("Quote", KeyCode::KEY_APOSTROPHE), ("Backquote", KeyCode::KEY_GRAVE),
    ("Comma", KeyCode::KEY_COMMA), ("Period", KeyCode::KEY_DOT), ("Slash", KeyCode::KEY_SLASH),
];

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycode_mapping() {
        assert_eq!(to_evdev_keycode(HotkeyCode::from_token("NumpadSubtract").unwrap()).unwrap(), KeyCode::KEY_KPMINUS.0);
        assert_eq!(to_evdev_keycode(HotkeyCode::from_token("NumpadAdd").unwrap()).unwrap(), KeyCode::KEY_KPPLUS.0);
        assert_eq!(to_evdev_keycode(HotkeyCode::from_token("KeyA").unwrap()).unwrap(), KeyCode::KEY_A.0);
    }

    #[test]
    fn modifier_detection() {
        let pressed: HashSet<u16> = [CTRL_L, SHIFT_R].into();
        assert_eq!(detect_modifiers(&pressed), vec![HotkeyModifier::Ctrl, HotkeyModifier::Shift]);
    }

    #[test]
    fn binding_match() {
        let mut bindings = HashMap::new();
        bindings.insert("test".to_string(), Binding {
            keycode: KeyCode::KEY_KPMINUS.0,
            modifiers: vec![HotkeyModifier::Ctrl],
        });

        // Match: Ctrl + NumpadSubtract
        let pressed: HashSet<u16> = [CTRL_L, KeyCode::KEY_KPMINUS.0].into();
        assert!(find_matches(&pressed, &bindings).contains("test"));

        // No match: missing Ctrl
        let pressed: HashSet<u16> = [KeyCode::KEY_KPMINUS.0].into();
        assert!(find_matches(&pressed, &bindings).is_empty());

        // No match: extra regular key
        let pressed: HashSet<u16> = [CTRL_L, KeyCode::KEY_KPMINUS.0, KeyCode::KEY_A.0].into();
        assert!(find_matches(&pressed, &bindings).is_empty());
    }
}
