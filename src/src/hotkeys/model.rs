use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyModifier {
    Ctrl,
    Alt,
    Shift,
    Super,
}

impl HotkeyModifier {
    pub fn token(self) -> &'static str {
        match self {
            Self::Ctrl => "Ctrl",
            Self::Alt => "Alt",
            Self::Shift => "Shift",
            Self::Super => "Super",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token.to_lowercase().as_str() {
            "ctrl" | "control" => Some(Self::Ctrl),
            "alt" => Some(Self::Alt),
            "shift" => Some(Self::Shift),
            "super" | "win" | "meta" => Some(Self::Super),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HotkeySpec {
    pub modifiers: Vec<HotkeyModifier>,
    pub key: String,
}

impl HotkeySpec {
    pub fn new(mut modifiers: Vec<HotkeyModifier>, key: &str) -> Self {
        modifiers.sort_by_key(|m| *m as usize);
        modifiers.dedup();
        Self {
            modifiers,
            key: key.to_string(),
        }
    }

    pub fn canonical_string(&self) -> String {
        let mut parts = Vec::new();
        for m in &self.modifiers {
            parts.push(m.token().to_string());
        }
        parts.push(self.key.clone());
        parts.join("+")
    }
}

pub fn parse_hotkey_spec(hotkey: &str) -> Result<HotkeySpec, String> {
    let parts: Vec<&str> = hotkey.split('+').map(|s| s.trim()).collect();
    if parts.is_empty() {
        return Err("Empty hotkey string".to_string());
    }

    let mut modifiers = Vec::new();
    let mut key = None;

    for part in parts {
        if let Some(mod_token) = HotkeyModifier::from_token(part) {
            modifiers.push(mod_token);
        } else {
            if key.is_some() {
                return Err("Multiple primary keys found in hotkey string".to_string());
            }
            key = Some(part.to_string());
        }
    }

    let key = key.ok_or_else(|| "No primary key found in hotkey string".to_string())?;
    Ok(HotkeySpec::new(modifiers, &key))
}

pub fn canonicalize_hotkey_string(hotkey: &str) -> Result<String, String> {
    parse_hotkey_spec(hotkey).map(|spec| spec.canonical_string())
}