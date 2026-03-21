//! Global hotkeys module using X11 passive key grabs.
//!
//! This backend targets X11 directly and may also work under XWayland when the
//! compositor exposes an X11 root window for passive grabs.

mod model;

use log::{info, warn};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xkb::{ConnectionExt as _, NameDetail, ID};
use x11rb::protocol::xproto::{
    ConnectionExt as _, GrabMode, KeyPressEvent, KeyReleaseEvent, ModMask, Window,
};
use x11rb::protocol::ErrorKind;
use x11rb::rust_connection::RustConnection;

pub use model::{
    canonicalize_hotkey_string, normalize_capture_key, parse_hotkey_spec, HotkeyCode,
    HotkeyModifier, HotkeySpec,
};

pub struct HotkeyManager {
    backend: X11Backend,
}

impl HotkeyManager {
    pub fn new_blocking(
        sender: Sender<String>,
        sounds: &[(String, String)],
    ) -> Result<Self, String> {
        info!("Initializing X11 hotkey backend (may work under XWayland)");

        let backend = X11Backend::new()?;

        for (id, hk) in sounds {
            if let Err(e) = backend.register(id, hk) {
                warn!(
                    "Failed to register hotkey '{}' for sound '{}': {}",
                    hk, id, e
                );
            }
        }

        backend.start_listener(sender);

        Ok(Self { backend })
    }

    pub fn register_hotkey_blocking(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        self.backend.register(sound_id, hotkey)
    }

    pub fn unregister_hotkey_blocking(&self, sound_id: &str) -> Result<(), String> {
        self.backend.unregister(sound_id)
    }
}

#[derive(Debug, Clone)]
struct RegisteredGrab {
    keycode: u8,
    modifiers: u16,
}

#[derive(Debug, Clone, Copy)]
struct ModifierMasks {
    ctrl: u16,
    alt: u16,
    shift: u16,
    super_mask: u16,
    altgr: Option<u16>,
    caps: u16,
    numlock: u16,
}

impl ModifierMasks {
    fn detect(conn: &RustConnection) -> Self {
        Self {
            ctrl: detect_modifier_mask(conn, &[0xffe3, 0xffe4])
                .unwrap_or(u16::from(ModMask::CONTROL)),
            alt: detect_modifier_mask(conn, &[0xffe9, 0xffea]).unwrap_or(u16::from(ModMask::M1)),
            shift: detect_modifier_mask(conn, &[0xffe1, 0xffe2])
                .unwrap_or(u16::from(ModMask::SHIFT)),
            super_mask: detect_modifier_mask(conn, &[0xffeb, 0xffec])
                .unwrap_or(u16::from(ModMask::M4)),
            altgr: detect_modifier_mask(conn, &[0xfe03, 0xff7e]),
            caps: detect_modifier_mask(conn, &[0xffe5]).unwrap_or(u16::from(ModMask::LOCK)),
            numlock: detect_modifier_mask(conn, &[0xff7f]).unwrap_or(u16::from(ModMask::M2)),
        }
    }

    fn ignore_lock_mask(self) -> u16 {
        self.caps | self.numlock
    }
}

struct X11Backend {
    conn: Arc<RustConnection>,
    root: Window,
    grabs: Arc<Mutex<HashMap<String, RegisteredGrab>>>,
    keycodes_by_xkb_name: HashMap<String, u8>,
    modifier_masks: ModifierMasks,
    listener_started: AtomicBool,
}

impl X11Backend {
    fn new() -> Result<Self, String> {
        let (conn, screen_num) =
            x11rb::connect(None).map_err(|e| format!("X11 connect failed: {e}"))?;

        conn.xkb_use_extension(1, 0)
            .map_err(|e| format!("XKB extension probe failed: {e}"))?
            .reply()
            .map_err(|e| format!("XKB extension probe reply failed: {e}"))
            .and_then(|reply| {
                if reply.supported {
                    Ok(())
                } else {
                    Err("XKB extension is not available on this X11 server".to_string())
                }
            })?;

        let root = conn.setup().roots[screen_num].root;
        let keycodes_by_xkb_name = fetch_xkb_keycodes(&conn)?;
        let modifier_masks = ModifierMasks::detect(&conn);

        Ok(Self {
            conn: Arc::new(conn),
            root,
            grabs: Arc::new(Mutex::new(HashMap::new())),
            keycodes_by_xkb_name,
            modifier_masks,
            listener_started: AtomicBool::new(false),
        })
    }

    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        let spec = parse_hotkey_spec(hotkey)?;
        let keycode = self.keycode_from_hotkey(spec.key)?;
        let modifiers = self.mods_to_mask(&spec.modifiers)?;

        let ignore_mask = self.modifier_masks.ignore_lock_mask();
        let existing_binding = {
            let grabs_map = self.grabs.lock().unwrap();

            if let Some(existing) = grabs_map.get(sound_id) {
                let cleaned_existing = existing.modifiers & !ignore_mask;
                let cleaned_new = modifiers & !ignore_mask;
                if existing.keycode == keycode && cleaned_existing == cleaned_new {
                    return Ok(());
                }
            }

            for (other_id, other) in grabs_map.iter() {
                if other_id == sound_id {
                    continue;
                }
                let cleaned_other = other.modifiers & !ignore_mask;
                let cleaned_new = modifiers & !ignore_mask;
                if other.keycode == keycode && cleaned_other == cleaned_new {
                    return Err(format!("HOTKEY_CONFLICT:{other_id}"));
                }
            }

            grabs_map.get(sound_id).cloned()
        };

        if let Some(old) = existing_binding.as_ref() {
            self.ungrab_binding(old.keycode, old.modifiers);
            self.grabs.lock().unwrap().remove(sound_id);
        }

        if let Err(error) = self.grab_binding_checked(keycode, modifiers) {
            if let Some(old) = existing_binding {
                if self
                    .grab_binding_checked(old.keycode, old.modifiers)
                    .is_ok()
                {
                    self.grabs
                        .lock()
                        .unwrap()
                        .insert(sound_id.to_string(), old.clone());
                } else {
                    warn!("Failed to restore previous hotkey for '{}'", sound_id);
                }
            }
            return Err(error);
        }

        self.grabs
            .lock()
            .unwrap()
            .insert(sound_id.to_string(), RegisteredGrab { keycode, modifiers });
        Ok(())
    }

    fn unregister(&self, sound_id: &str) -> Result<(), String> {
        if let Some(existing) = self.grabs.lock().unwrap().remove(sound_id) {
            self.ungrab_binding(existing.keycode, existing.modifiers);
        }
        Ok(())
    }

    fn start_listener(&self, sender: Sender<String>) {
        if self.listener_started.swap(true, Ordering::SeqCst) {
            return;
        }

        let conn = self.conn.clone();
        let grabs = self.grabs.clone();
        let ignore_mask = self.modifier_masks.ignore_lock_mask();

        thread::spawn(move || {
            let mut pressed: HashSet<u8> = HashSet::new();
            loop {
                let event = match conn.wait_for_event() {
                    Ok(ev) => ev,
                    Err(e) => {
                        warn!("X11 event error: {}", e);
                        break;
                    }
                };

                match event {
                    x11rb::protocol::Event::KeyPress(KeyPressEvent { detail, state, .. }) => {
                        if !pressed.insert(detail) {
                            continue;
                        }

                        let grabs_map = grabs.lock().unwrap();
                        for (sound_id, grab) in grabs_map.iter() {
                            let cleaned = u16::from(state) & !ignore_mask;
                            let expected = grab.modifiers & !ignore_mask;
                            if grab.keycode == detail && cleaned == expected {
                                let _ = sender.send(sound_id.clone());
                            }
                        }
                    }
                    x11rb::protocol::Event::KeyRelease(KeyReleaseEvent { detail, .. }) => {
                        pressed.remove(&detail);
                    }
                    _ => {}
                }
            }
        });
    }

    fn keycode_from_hotkey(&self, key: HotkeyCode) -> Result<u8, String> {
        self.keycodes_by_xkb_name
            .get(key.xkb_name())
            .copied()
            .ok_or_else(|| format!("No X11 keycode found for {}", key.token()))
    }

    fn mods_to_mask(&self, modifiers: &[HotkeyModifier]) -> Result<u16, String> {
        let mut mask = 0u16;
        for modifier in modifiers {
            mask |= match modifier {
                HotkeyModifier::Ctrl => self.modifier_masks.ctrl,
                HotkeyModifier::Alt => self.modifier_masks.alt,
                HotkeyModifier::Shift => self.modifier_masks.shift,
                HotkeyModifier::Super => self.modifier_masks.super_mask,
                HotkeyModifier::AltGr => self
                    .modifier_masks
                    .altgr
                    .ok_or_else(|| "AltGr is not available on this keyboard layout".to_string())?,
            };
        }
        Ok(mask)
    }

    fn grab_binding_checked(&self, keycode: u8, modifiers: u16) -> Result<(), String> {
        let masks = with_lock_variants(modifiers, self.modifier_masks);
        let mut grabbed: Vec<u16> = Vec::new();

        for mask in masks {
            let cookie = self
                .conn
                .grab_key(
                    false,
                    self.root,
                    mask.into(),
                    keycode,
                    GrabMode::ASYNC,
                    GrabMode::ASYNC,
                )
                .map_err(|e| format!("grab_key request failed: {e}"))?;

            if let Err(error) = cookie.check() {
                for already_grabbed in grabbed {
                    let _ = self
                        .conn
                        .ungrab_key(keycode, self.root, already_grabbed.into());
                }
                let _ = self.conn.flush();
                return Err(format_grab_error(error));
            }

            grabbed.push(mask);
        }

        self.conn
            .flush()
            .map_err(|e| format!("X11 flush failed: {e}"))?;
        Ok(())
    }

    fn ungrab_binding(&self, keycode: u8, modifiers: u16) {
        for mask in with_lock_variants(modifiers, self.modifier_masks) {
            let _ = self.conn.ungrab_key(keycode, self.root, mask.into());
        }
        let _ = self.conn.flush();
    }
}

fn with_lock_variants(modifiers: u16, masks: ModifierMasks) -> [u16; 4] {
    [
        modifiers,
        modifiers | masks.caps,
        modifiers | masks.numlock,
        modifiers | masks.caps | masks.numlock,
    ]
}

fn format_grab_error(error: ReplyError) -> String {
    match error {
        ReplyError::X11Error(err) if err.error_kind == ErrorKind::Access => {
            "grab_key failed: key combination is already grabbed by another client".to_string()
        }
        other => format!("grab_key failed: {other}"),
    }
}

fn fetch_xkb_keycodes(conn: &RustConnection) -> Result<HashMap<String, u8>, String> {
    let reply = conn
        .xkb_get_names(ID::USE_CORE_KBD.into(), NameDetail::KEY_NAMES)
        .map_err(|e| format!("XKB get_names request failed: {e}"))?
        .reply()
        .map_err(|e| format!("XKB get_names reply failed: {e}"))?;

    let key_names = reply
        .value_list
        .key_names
        .ok_or_else(|| "XKB key-name map is unavailable".to_string())?;

    let mut result = HashMap::new();
    for (index, key_name) in key_names.iter().enumerate() {
        let keycode = reply.first_key + index as u8;
        let trimmed = trim_key_name(key_name.name);
        if !trimmed.is_empty() {
            result.insert(trimmed, keycode);
        }
    }
    Ok(result)
}

fn trim_key_name(name: [u8; 4]) -> String {
    let bytes: Vec<u8> = name.into_iter().take_while(|byte| *byte != 0).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

fn detect_modifier_mask(conn: &RustConnection, keysyms: &[u32]) -> Option<u16> {
    let reply = conn.get_modifier_mapping().ok()?.reply().ok()?;
    let masks: [u16; 8] = [
        u16::from(ModMask::SHIFT),
        u16::from(ModMask::LOCK),
        u16::from(ModMask::CONTROL),
        u16::from(ModMask::M1),
        u16::from(ModMask::M2),
        u16::from(ModMask::M3),
        u16::from(ModMask::M4),
        u16::from(ModMask::M5),
    ];

    let keycodes_per_modifier = reply.keycodes_per_modifier() as usize;
    for (modifier_index, chunk) in reply.keycodes.chunks(keycodes_per_modifier).enumerate() {
        for &keycode in chunk {
            if keycode == 0 {
                continue;
            }
            let mapping = conn.get_keyboard_mapping(keycode, 1).ok()?.reply().ok()?;
            if mapping
                .keysyms
                .iter()
                .any(|keysym| keysyms.contains(keysym))
            {
                return masks.get(modifier_index).copied();
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_lock_variants_covers_caps_and_numlock() {
        let variants = with_lock_variants(
            0x10,
            ModifierMasks {
                ctrl: 0,
                alt: 0,
                shift: 0,
                super_mask: 0,
                altgr: None,
                caps: 0x02,
                numlock: 0x04,
            },
        );
        assert_eq!(variants, [0x10, 0x12, 0x14, 0x16]);
    }

    #[test]
    fn trims_xkb_names() {
        assert_eq!(trim_key_name(*b"AE01"), "AE01");
        assert_eq!(trim_key_name([b'E', b'S', b'C', 0]), "ESC");
    }
}
