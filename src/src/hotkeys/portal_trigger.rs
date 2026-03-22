use super::{parse_hotkey_spec, HotkeyCode, HotkeyModifier};

pub fn canonical_hotkey_to_portal_trigger(hotkey: &str) -> Result<String, String> {
    let spec = parse_hotkey_spec(hotkey)?;
    let mut parts: Vec<String> = Vec::new();

    for modifier in spec.modifiers {
        parts.push(portal_modifier(modifier)?.to_string());
    }

    parts.push(portal_key(spec.key)?);
    Ok(parts.join("+"))
}

fn portal_modifier(modifier: HotkeyModifier) -> Result<&'static str, String> {
    match modifier {
        HotkeyModifier::Ctrl => Ok("CTRL"),
        HotkeyModifier::Alt => Ok("ALT"),
        HotkeyModifier::Shift => Ok("SHIFT"),
        HotkeyModifier::Super => Ok("LOGO"),
        HotkeyModifier::AltGr => Err("AltGr is not supported by portal trigger mapping".to_string()),
    }
}

fn portal_key(code: HotkeyCode) -> Result<String, String> {
    let token = code.token();

    if let Some(letter) = token.strip_prefix("Key") {
        return Ok(letter.to_ascii_lowercase());
    }

    if let Some(digit) = token.strip_prefix("Digit") {
        return Ok(digit.to_string());
    }

    if token.starts_with('F') {
        return Ok(token.to_string());
    }

    let mapped = match token {
        "Escape" => "Escape",
        "Tab" => "Tab",
        "Enter" => "Return",
        "Space" => "space",
        "Backspace" => "BackSpace",
        "Insert" => "Insert",
        "Delete" => "Delete",
        "Home" => "Home",
        "End" => "End",
        "PageUp" => "Page_Up",
        "PageDown" => "Page_Down",
        "ArrowUp" => "Up",
        "ArrowDown" => "Down",
        "ArrowLeft" => "Left",
        "ArrowRight" => "Right",
        "Minus" => "minus",
        "Equal" => "equal",
        "BracketLeft" => "bracketleft",
        "BracketRight" => "bracketright",
        "Backslash" => "backslash",
        "Semicolon" => "semicolon",
        "Quote" => "apostrophe",
        "Backquote" => "grave",
        "Comma" => "comma",
        "Period" => "period",
        "Slash" => "slash",
        "Numpad0" => "KP_0",
        "Numpad1" => "KP_1",
        "Numpad2" => "KP_2",
        "Numpad3" => "KP_3",
        "Numpad4" => "KP_4",
        "Numpad5" => "KP_5",
        "Numpad6" => "KP_6",
        "Numpad7" => "KP_7",
        "Numpad8" => "KP_8",
        "Numpad9" => "KP_9",
        "NumpadDecimal" => "KP_Decimal",
        "NumpadDivide" => "KP_Divide",
        "NumpadMultiply" => "KP_Multiply",
        "NumpadSubtract" => "KP_Subtract",
        "NumpadAdd" => "KP_Add",
        "NumpadEnter" => "KP_Enter",
        _ => {
            return Err(format!(
                "Unsupported key token for portal trigger mapping: {}",
                token
            ))
        }
    };

    Ok(mapped.to_string())
}

#[cfg(test)]
mod tests {
    use super::canonical_hotkey_to_portal_trigger;

    #[test]
    fn maps_letters_and_modifiers() {
        assert_eq!(
            canonical_hotkey_to_portal_trigger("Ctrl+Alt+KeyP").unwrap(),
            "CTRL+ALT+p"
        );
    }

    #[test]
    fn maps_super_and_arrows() {
        assert_eq!(
            canonical_hotkey_to_portal_trigger("Super+ArrowUp").unwrap(),
            "LOGO+Up"
        );
    }

    #[test]
    fn maps_numpad_keys() {
        assert_eq!(
            canonical_hotkey_to_portal_trigger("Ctrl+NumpadAdd").unwrap(),
            "CTRL+KP_Add"
        );
    }

    #[test]
    fn rejects_altgr() {
        let err = canonical_hotkey_to_portal_trigger("AltGr+KeyA").unwrap_err();
        assert!(err.contains("AltGr"));
    }
}