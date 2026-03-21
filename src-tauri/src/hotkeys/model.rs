use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyModifier {
    Ctrl,
    Alt,
    Shift,
    Super,
    AltGr,
}

impl HotkeyModifier {
    pub fn token(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Shift => "Shift",
            Self::Super => "Super",
            Self::AltGr => "AltGr",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "Ctrl" | "Control" => Some(Self::Ctrl),
            "Alt" => Some(Self::Alt),
            "Shift" => Some(Self::Shift),
            "Super" | "Win" | "Meta" => Some(Self::Super),
            "AltGr" | "AltGraph" => Some(Self::AltGr),
            _ => None,
        }
    }

    fn order(self) -> usize {
        match self {
            Self::Ctrl => 0,
            Self::Alt => 1,
            Self::Shift => 2,
            Self::Super => 3,
            Self::AltGr => 4,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct HotkeyCodeDef {
    token: &'static str,
    xkb_name: &'static str,
    capture_keycode: Option<u32>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeyCode(&'static HotkeyCodeDef);

impl fmt::Debug for HotkeyCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.token())
    }
}

impl HotkeyCode {
    pub fn token(self) -> &'static str {
        self.0.token
    }

    pub fn xkb_name(self) -> &'static str {
        self.0.xkb_name
    }

    pub fn from_token(token: &str) -> Option<Self> {
        HOTKEY_CODES.iter().find(|def| def.token == token).map(Self)
    }

    pub fn from_capture_keycode(keycode: u32) -> Option<Self> {
        HOTKEY_CODES
            .iter()
            .find(|def| def.capture_keycode == Some(keycode))
            .map(Self)
    }

    pub fn from_user_token(raw: &str) -> Result<Self, String> {
        let canonical = canonicalize_key_token(raw);
        Self::from_token(&canonical).ok_or_else(|| format!("Unsupported key: {raw}"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HotkeySpec {
    pub modifiers: Vec<HotkeyModifier>,
    pub key: HotkeyCode,
}

impl HotkeySpec {
    pub fn new(mut modifiers: Vec<HotkeyModifier>, key: HotkeyCode) -> Self {
        modifiers.sort_by_key(|modifier| modifier.order());
        modifiers.dedup();
        Self { modifiers, key }
    }

    pub fn canonical_string(&self) -> String {
        let mut parts: Vec<String> = self
            .modifiers
            .iter()
            .map(|modifier| modifier.token().to_string())
            .collect();
        parts.push(self.key.token().to_string());
        parts.join("+")
    }
}

pub fn parse_hotkey_spec(hk: &str) -> Result<HotkeySpec, String> {
    let mut modifiers = Vec::new();
    let mut key = None;

    for part in hk.split('+') {
        if let Some(modifier) = HotkeyModifier::from_token(part) {
            modifiers.push(modifier);
        } else {
            key = Some(HotkeyCode::from_user_token(part)?);
        }
    }

    let key = key.ok_or_else(|| "Hotkey must include a non-modifier key".to_string())?;
    Ok(HotkeySpec::new(modifiers, key))
}

pub fn canonicalize_hotkey_string(hk: &str) -> Result<String, String> {
    Ok(parse_hotkey_spec(hk)?.canonical_string())
}

pub fn normalize_capture_key(key_name: &str, keycode: u32) -> Option<HotkeyCode> {
    if is_disallowed_symbol_key_name(key_name) {
        return None;
    }

    if let Some(code) = HotkeyCode::from_capture_keycode(keycode) {
        return Some(code);
    }

    let normalized = normalize_key_name(key_name);
    HotkeyCode::from_token(&normalized)
}

fn is_disallowed_symbol_key_name(name: &str) -> bool {
    matches!(
        name,
        "apostrophe"
            | "quotedbl"
            | "comma"
            | "period"
            | "slash"
            | "backslash"
            | "semicolon"
            | "colon"
            | "minus"
            | "equal"
            | "grave"
            | "asciitilde"
            | "exclam"
            | "at"
            | "numbersign"
            | "dollar"
            | "percent"
            | "asciicircum"
            | "ampersand"
            | "asterisk"
            | "parenleft"
            | "parenright"
            | "plus"
            | "less"
            | "greater"
            | "question"
            | "bar"
            | "braceleft"
            | "braceright"
            | "bracketleft"
            | "bracketright"
            | "underscore"
    )
}

fn normalize_key_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix("KP_") {
        return normalize_keypad_name(rest);
    }

    match name {
        "space" | "Space" => "Space".to_string(),
        "Return" => "Enter".to_string(),
        "BackSpace" => "Backspace".to_string(),
        "Tab" | "ISO_Left_Tab" => "Tab".to_string(),
        "Delete" => "Delete".to_string(),
        "Insert" => "Insert".to_string(),
        "Home" => "Home".to_string(),
        "End" => "End".to_string(),
        "Page_Up" => "PageUp".to_string(),
        "Page_Down" => "PageDown".to_string(),
        "Up" => "ArrowUp".to_string(),
        "Down" => "ArrowDown".to_string(),
        "Left" => "ArrowLeft".to_string(),
        "Right" => "ArrowRight".to_string(),
        other => {
            if other.len() == 1 {
                let ch = other.chars().next().unwrap();
                if ch.is_ascii_lowercase() {
                    return format!("Key{}", ch.to_ascii_uppercase());
                }
                if ch.is_ascii_digit() {
                    return format!("Digit{}", ch);
                }
            }
            if other.starts_with('F') && other[1..].parse::<u32>().is_ok() {
                return other.to_string();
            }
            other.to_string()
        }
    }
}

fn canonicalize_key_token(raw: &str) -> String {
    if let Some(rest) = raw.strip_prefix("KP_") {
        return normalize_keypad_name(rest);
    }

    if let Some(alias) = canonicalize_numpad_alias(raw) {
        return alias.to_string();
    }

    if HotkeyCode::from_token(raw).is_some() {
        return raw.to_string();
    }

    if raw.starts_with("Key")
        || raw.starts_with("Digit")
        || raw.starts_with("Numpad")
        || raw.starts_with("Arrow")
        || raw.starts_with('F')
    {
        return raw.to_string();
    }

    if raw.len() == 1 {
        let ch = raw.chars().next().unwrap();
        if ch.is_ascii_alphabetic() {
            return format!("Key{}", ch.to_ascii_uppercase());
        }
        if ch.is_ascii_digit() {
            return format!("Digit{}", ch);
        }
    }

    match raw.to_ascii_lowercase().as_str() {
        "space" => "Space".to_string(),
        "tab" => "Tab".to_string(),
        "enter" | "return" => "Enter".to_string(),
        "esc" | "escape" => "Escape".to_string(),
        "backspace" => "Backspace".to_string(),
        "delete" | "del" => "Delete".to_string(),
        "insert" | "ins" => "Insert".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        "minus" => "Minus".to_string(),
        "equal" => "Equal".to_string(),
        "bracketleft" => "BracketLeft".to_string(),
        "bracketright" => "BracketRight".to_string(),
        "backslash" => "Backslash".to_string(),
        "semicolon" => "Semicolon".to_string(),
        "quote" => "Quote".to_string(),
        "backquote" | "grave" => "Backquote".to_string(),
        "comma" => "Comma".to_string(),
        "period" => "Period".to_string(),
        "slash" => "Slash".to_string(),
        other => other.to_string(),
    }
}

fn normalize_keypad_name(rest: &str) -> String {
    match canonicalize_numpad_alias(&format!("Numpad{rest}")) {
        Some(alias) => alias.to_string(),
        None => format!("Numpad{rest}"),
    }
}

fn canonicalize_numpad_alias(raw: &str) -> Option<&'static str> {
    match raw {
        "Numpad0" | "NumpadInsert" => Some("Numpad0"),
        "Numpad1" | "NumpadEnd" => Some("Numpad1"),
        "Numpad2" | "NumpadDown" => Some("Numpad2"),
        "Numpad3" | "NumpadNext" | "NumpadPage_Down" => Some("Numpad3"),
        "Numpad4" | "NumpadLeft" => Some("Numpad4"),
        "Numpad5" | "NumpadBegin" | "NumpadClear" => Some("Numpad5"),
        "Numpad6" | "NumpadRight" => Some("Numpad6"),
        "Numpad7" | "NumpadHome" => Some("Numpad7"),
        "Numpad8" | "NumpadUp" => Some("Numpad8"),
        "Numpad9" | "NumpadPrior" | "NumpadPage_Up" => Some("Numpad9"),
        "NumpadDecimal" | "NumpadDelete" | "NumpadSeparator" => Some("NumpadDecimal"),
        "NumpadDivide" => Some("NumpadDivide"),
        "NumpadMultiply" => Some("NumpadMultiply"),
        "NumpadSubtract" => Some("NumpadSubtract"),
        "NumpadAdd" => Some("NumpadAdd"),
        "NumpadEnter" => Some("NumpadEnter"),
        _ => None,
    }
}

macro_rules! key_def {
    ($token:literal, $xkb:literal, $capture_keycode:expr) => {
        HotkeyCodeDef {
            token: $token,
            xkb_name: $xkb,
            capture_keycode: $capture_keycode,
        }
    };
}

static HOTKEY_CODES: &[HotkeyCodeDef] = &[
    key_def!("KeyA", "AC01", Some(38)),
    key_def!("KeyB", "AB05", Some(56)),
    key_def!("KeyC", "AB03", Some(54)),
    key_def!("KeyD", "AC03", Some(40)),
    key_def!("KeyE", "AD03", Some(26)),
    key_def!("KeyF", "AC04", Some(41)),
    key_def!("KeyG", "AC05", Some(42)),
    key_def!("KeyH", "AC06", Some(43)),
    key_def!("KeyI", "AD08", Some(31)),
    key_def!("KeyJ", "AC07", Some(44)),
    key_def!("KeyK", "AC08", Some(45)),
    key_def!("KeyL", "AC09", Some(46)),
    key_def!("KeyM", "AB07", Some(58)),
    key_def!("KeyN", "AB06", Some(57)),
    key_def!("KeyO", "AD09", Some(32)),
    key_def!("KeyP", "AD10", Some(33)),
    key_def!("KeyQ", "AD01", Some(24)),
    key_def!("KeyR", "AD04", Some(27)),
    key_def!("KeyS", "AC02", Some(39)),
    key_def!("KeyT", "AD05", Some(28)),
    key_def!("KeyU", "AD07", Some(30)),
    key_def!("KeyV", "AB04", Some(55)),
    key_def!("KeyW", "AD02", Some(25)),
    key_def!("KeyX", "AB02", Some(53)),
    key_def!("KeyY", "AD06", Some(29)),
    key_def!("KeyZ", "AB01", Some(52)),
    key_def!("Digit1", "AE01", Some(10)),
    key_def!("Digit2", "AE02", Some(11)),
    key_def!("Digit3", "AE03", Some(12)),
    key_def!("Digit4", "AE04", Some(13)),
    key_def!("Digit5", "AE05", Some(14)),
    key_def!("Digit6", "AE06", Some(15)),
    key_def!("Digit7", "AE07", Some(16)),
    key_def!("Digit8", "AE08", Some(17)),
    key_def!("Digit9", "AE09", Some(18)),
    key_def!("Digit0", "AE10", Some(19)),
    key_def!("Escape", "ESC", Some(9)),
    key_def!("Tab", "TAB", Some(23)),
    key_def!("Enter", "RTRN", Some(36)),
    key_def!("Space", "SPCE", Some(65)),
    key_def!("Backspace", "BKSP", Some(22)),
    key_def!("Minus", "AE11", None),
    key_def!("Equal", "AE12", None),
    key_def!("BracketLeft", "AD11", None),
    key_def!("BracketRight", "AD12", None),
    key_def!("Backslash", "BKSL", None),
    key_def!("Semicolon", "AC10", None),
    key_def!("Quote", "AC11", None),
    key_def!("Backquote", "TLDE", None),
    key_def!("Comma", "AB08", None),
    key_def!("Period", "AB09", None),
    key_def!("Slash", "AB10", None),
    key_def!("Insert", "INS", Some(118)),
    key_def!("Delete", "DELE", Some(119)),
    key_def!("Home", "HOME", Some(110)),
    key_def!("End", "END", Some(115)),
    key_def!("PageUp", "PGUP", Some(112)),
    key_def!("PageDown", "PGDN", Some(117)),
    key_def!("ArrowUp", "UP", Some(111)),
    key_def!("ArrowLeft", "LEFT", Some(113)),
    key_def!("ArrowRight", "RGHT", Some(114)),
    key_def!("ArrowDown", "DOWN", Some(116)),
    key_def!("F1", "FK01", Some(67)),
    key_def!("F2", "FK02", Some(68)),
    key_def!("F3", "FK03", Some(69)),
    key_def!("F4", "FK04", Some(70)),
    key_def!("F5", "FK05", Some(71)),
    key_def!("F6", "FK06", Some(72)),
    key_def!("F7", "FK07", Some(73)),
    key_def!("F8", "FK08", Some(74)),
    key_def!("F9", "FK09", Some(75)),
    key_def!("F10", "FK10", Some(76)),
    key_def!("F11", "FK11", Some(95)),
    key_def!("F12", "FK12", Some(96)),
    key_def!("Numpad0", "KP0", Some(90)),
    key_def!("Numpad1", "KP1", Some(87)),
    key_def!("Numpad2", "KP2", Some(88)),
    key_def!("Numpad3", "KP3", Some(89)),
    key_def!("Numpad4", "KP4", Some(83)),
    key_def!("Numpad5", "KP5", Some(84)),
    key_def!("Numpad6", "KP6", Some(85)),
    key_def!("Numpad7", "KP7", Some(79)),
    key_def!("Numpad8", "KP8", Some(80)),
    key_def!("Numpad9", "KP9", Some(81)),
    key_def!("NumpadDecimal", "KPDL", Some(91)),
    key_def!("NumpadDivide", "KPDV", Some(106)),
    key_def!("NumpadMultiply", "KPMU", Some(63)),
    key_def!("NumpadSubtract", "KPSU", Some(82)),
    key_def!("NumpadAdd", "KPAD", Some(86)),
    key_def!("NumpadEnter", "KPEN", Some(104)),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonicalizes_legacy_letter_hotkey() {
        assert_eq!(
            canonicalize_hotkey_string("Alt+Ctrl+P").unwrap(),
            "Ctrl+Alt+KeyP"
        );
    }

    #[test]
    fn canonicalizes_numpad_aliases() {
        assert_eq!(
            canonicalize_hotkey_string("Shift+KP_End").unwrap(),
            "Shift+Numpad1"
        );
        assert_eq!(
            canonicalize_hotkey_string("Shift+NumpadPage_Down").unwrap(),
            "Shift+Numpad3"
        );
    }

    #[test]
    fn deduplicates_and_orders_modifiers() {
        assert_eq!(
            canonicalize_hotkey_string("Shift+Ctrl+Shift+Alt+KeyA").unwrap(),
            "Ctrl+Alt+Shift+KeyA"
        );
    }

    #[test]
    fn normalizes_capture_key_from_keycode() {
        assert_eq!(
            normalize_capture_key("whatever", 33).unwrap().token(),
            "KeyP"
        );
    }

    #[test]
    fn rejects_unsupported_symbol_capture_keys() {
        assert!(normalize_capture_key("slash", 0).is_none());
    }
}
