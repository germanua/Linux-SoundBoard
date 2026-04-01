pub const HOTKEY_CONFLICT: &str = "HOTKEY_CONFLICT";
pub const UNSUPPORTED_KEY_FOR_BACKEND: &str = "UNSUPPORTED_KEY_FOR_BACKEND";
pub const PORTAL_UNAVAILABLE: &str = "PORTAL_UNAVAILABLE";
pub const PORTAL_REJECTED_BY_COMPOSITOR: &str = "PORTAL_REJECTED_BY_COMPOSITOR";
pub const X11_BACKEND_UNAVAILABLE: &str = "X11_BACKEND_UNAVAILABLE";

pub fn hotkey_conflict(existing_id: &str) -> String {
    coded_error(HOTKEY_CONFLICT, existing_id)
}

pub fn unsupported_key_for_backend(backend: &str, detail: impl AsRef<str>) -> String {
    coded_error(
        UNSUPPORTED_KEY_FOR_BACKEND,
        format!("{backend}:{}", detail.as_ref()),
    )
}

#[cfg(test)]
pub fn portal_rejected_by_compositor(detail: impl AsRef<str>) -> String {
    coded_error(PORTAL_REJECTED_BY_COMPOSITOR, detail)
}

pub fn format_hotkey_error(raw: &str) -> String {
    if let Some(detail) = raw.strip_prefix("Global hotkeys unavailable: ") {
        return format_hotkey_error(detail);
    }

    let (code, detail) = match raw.split_once(':') {
        Some((code, detail)) => (code, detail.trim()),
        None => return raw.to_string(),
    };

    match code {
        HOTKEY_CONFLICT => {
            "That shortcut is already assigned to another sound or control action.".to_string()
        }
        UNSUPPORTED_KEY_FOR_BACKEND => append_detail(
            "This shortcut is not supported by the active hotkey backend.",
            simplify_backend_detail(detail),
        ),
        PORTAL_UNAVAILABLE => append_detail(
            "Wayland global shortcuts are unavailable on this desktop session.",
            detail,
        ),
        PORTAL_REJECTED_BY_COMPOSITOR => {
            append_detail("Your desktop environment declined this shortcut.", detail)
        }
        X11_BACKEND_UNAVAILABLE => append_detail("X11 global hotkeys are unavailable.", detail),
        _ => raw.to_string(),
    }
}

fn coded_error(code: &str, detail: impl AsRef<str>) -> String {
    let detail = detail.as_ref().trim();
    if detail.is_empty() {
        code.to_string()
    } else {
        format!("{code}:{detail}")
    }
}

fn append_detail(prefix: &str, detail: &str) -> String {
    if detail.is_empty() {
        prefix.to_string()
    } else {
        format!("{prefix} {detail}")
    }
}

fn simplify_backend_detail(detail: &str) -> &str {
    detail
        .split_once(':')
        .map(|(_, rest)| rest.trim())
        .unwrap_or(detail)
}

#[cfg(test)]
mod tests {
    use super::{format_hotkey_error, portal_rejected_by_compositor, unsupported_key_for_backend};

    #[test]
    fn formats_backend_specific_errors() {
        assert_eq!(
            format_hotkey_error(&unsupported_key_for_backend(
                "portal",
                "Ctrl+AltGr+KeyP cannot be represented by the Wayland shortcuts portal."
            )),
            "This shortcut is not supported by the active hotkey backend. Ctrl+AltGr+KeyP cannot be represented by the Wayland shortcuts portal."
        );
    }

    #[test]
    fn formats_portal_rejection_errors() {
        assert_eq!(
            format_hotkey_error(&portal_rejected_by_compositor(
                "Try a different combination or check your compositor shortcut settings."
            )),
            "Your desktop environment declined this shortcut. Try a different combination or check your compositor shortcut settings."
        );
    }

    #[test]
    fn formats_swhkd_unsupported_errors() {
        assert_eq!(
            format_hotkey_error(&unsupported_key_for_backend(
                "swhkd",
                "Ctrl+NumpadDivide cannot be represented by swhkd."
            )),
            "This shortcut is not supported by the active hotkey backend. Ctrl+NumpadDivide cannot be represented by swhkd."
        );
    }
}
