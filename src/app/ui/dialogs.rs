use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::WidgetExt;
use gtk4::{self, Box as GtkBox, EventControllerKey, Label, Orientation, Window};
use libadwaita as adw;
use libadwaita::prelude::*;
use log::debug;

use crate::hotkeys::{
    format_hotkey_error, normalize_capture_key, HotkeyCode, HotkeyModifier, HotkeySpec,
};

fn push_capture_candidate(candidates: &mut Vec<String>, candidate: &str) {
    if candidate.is_empty() || candidates.iter().any(|existing| existing == candidate) {
        return;
    }
    candidates.push(candidate.to_string());
}

fn resolve_capture_key_candidates<'a, I>(
    key_name: &str,
    keycode: u32,
    mapped_key_names: I,
) -> Option<crate::hotkeys::HotkeyCode>
where
    I: IntoIterator<Item = &'a str>,
{
    for candidate in mapped_key_names {
        if !candidate.starts_with("KP_") {
            continue;
        }

        if let Some(code) = normalize_capture_key(candidate, keycode) {
            return Some(code);
        }
    }

    normalize_capture_key(key_name, keycode)
}

fn resolve_capture_key(key_name: &str, keycode: u32) -> Option<crate::hotkeys::HotkeyCode> {
    let mut keypad_candidates = Vec::new();

    if let Some(display) = gtk4::gdk::Display::default() {
        if let Some(mapped_keys) = display.map_keycode(keycode) {
            for (_, mapped_keyval) in mapped_keys {
                if let Some(mapped_name) = mapped_keyval.name() {
                    let mapped_name = mapped_name.to_string();
                    if mapped_name.starts_with("KP_") {
                        push_capture_candidate(&mut keypad_candidates, &mapped_name);
                    }
                }
            }
        }

        let resolved = resolve_capture_key_candidates(
            key_name,
            keycode,
            keypad_candidates.iter().map(String::as_str),
        );

        if let Some(code) = resolved {
            debug!(
                "Captured key '{}' (hardware code {}, backend {:?}) -> '{}'",
                key_name,
                keycode,
                display.backend(),
                code.token()
            );
        } else {
            debug!(
                "Unable to resolve captured key '{}' (hardware code {}, backend {:?}); keypad candidates: {:?}",
                key_name,
                keycode,
                display.backend(),
                keypad_candidates
            );
        }

        return resolved;
    }

    resolve_capture_key_candidates(key_name, keycode, std::iter::empty())
}

fn build_captured_combo<F>(
    key_token: HotkeyCode,
    modifiers: Vec<HotkeyModifier>,
    validate_hotkey: &F,
) -> Result<String, String>
where
    F: Fn(&str) -> Result<(), String>,
{
    let combo = HotkeySpec::new(modifiers, key_token).canonical_string();
    validate_hotkey(&combo)?;
    Ok(combo)
}

pub fn show_error(parent: &Window, title: &str, message: &str) {
    let dialog = adw::AlertDialog::new(Some(title), Some(message));
    dialog.add_responses(&[("ok", "OK")]);
    dialog.connect_response(None, |d, _| d.force_close());
    dialog.present(Some(parent));
}

pub fn show_message(parent: &Window, title: &str, message: &str) {
    let dialog = adw::AlertDialog::new(Some(title), Some(message));
    dialog.add_responses(&[("ok", "OK")]);
    dialog.connect_response(None, |d, _| d.force_close());
    dialog.present(Some(parent));
}

fn copy_text_to_clipboard(text: &str) -> bool {
    if let Some(display) = gtk4::gdk::Display::default() {
        display.clipboard().set_text(text);
        true
    } else {
        false
    }
}

pub fn show_confirm<F>(
    parent: &Window,
    title: &str,
    message: &str,
    confirm_label: &str,
    on_confirm: F,
) where
    F: Fn() + 'static,
{
    let dialog = adw::AlertDialog::new(Some(title), Some(message));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", confirm_label)]);
    dialog.connect_response(None, move |d, response| {
        if response == "confirm" {
            on_confirm();
        }
        d.force_close();
    });
    dialog.present(Some(parent));
}

pub fn show_hotkey_error_with_install_option(
    parent: &Window,
    title: &str,
    message: &str,
    config: Arc<Mutex<crate::config::Config>>,
    hotkeys: Arc<Mutex<crate::hotkeys::HotkeyManager>>,
) {
    let dialog = adw::AlertDialog::new(Some(title), Some(message));
    dialog.add_responses(&[("close", "Close"), ("install", "Install swhkd")]);
    let install_window = parent.clone();
    let message_text = message.to_string();
    dialog.connect_response(None, move |d, response| {
        if response == "install" {
            prompt_swhkd_install(
                &install_window,
                Arc::clone(&config),
                Arc::clone(&hotkeys),
                &message_text,
            );
        }
        d.force_close();
    });
    dialog.present(Some(parent));
}

pub fn prompt_swhkd_install(
    parent: &Window,
    config: Arc<Mutex<crate::config::Config>>,
    hotkeys: Arc<Mutex<crate::hotkeys::HotkeyManager>>,
    reason: &str,
) {
    let prompt = format!(
        "Native Wayland hotkeys require swhkd.\n\nCurrent issue:\n{}\n\nInstall now?",
        reason
    );
    let dialog = adw::AlertDialog::new(Some("Install Wayland Hotkey Support"), Some(&prompt));
    dialog.add_responses(&[("cancel", "Cancel"), ("install", "Install")]);

    let parent_weak = parent.downgrade();
    dialog.connect_response(None, move |d, response| {
        if response == "install" {
            if let Some(parent) = parent_weak.upgrade() {
                show_message(
                    &parent,
                    "Installing swhkd",
                    "Installation started. This can take a few minutes.",
                );

                let result_parent = parent.downgrade();
                if let Err(err) = crate::commands::install_swhkd_async(
                    Arc::clone(&config),
                    Arc::clone(&hotkeys),
                    move |result| {
                        if let Some(result_parent) = result_parent.upgrade() {
                            match result {
                                Ok(report) => {
                                    let state_labels = report
                                        .states
                                        .iter()
                                        .map(|state| format!("- {:?}", state))
                                        .collect::<Vec<_>>()
                                        .join("\n");
                                    let body = format!(
                                        "{}\n\n{}\n\nLifecycle:\n{}",
                                        report.summary, report.details, state_labels
                                    );
                                    show_message(&result_parent, "Hotkey Support Installed", &body);
                                }
                                Err(err) => {
                                    show_swhkd_install_failed_dialog(&result_parent, &err);
                                }
                            }
                        }
                    },
                ) {
                    show_error(&parent, "Failed to Start Installer", &err);
                }
            }
        }
        d.force_close();
    });
    dialog.present(Some(parent));
}

fn show_swhkd_install_failed_dialog(parent: &Window, err: &crate::hotkeys::SwhkdInstallError) {
    let manual_guide = crate::hotkeys::SWHKD_UPSTREAM_INSTALL_URL.to_string();
    let manual_commands = crate::hotkeys::manual_swhkd_install_commands();

    let body = format!(
        "{}\n\n{}\n\nFailure kind: {:?}\nFailure state: {:?}\n\nManual guide:\n{}",
        err.summary, err.details, err.kind, err.state, manual_guide
    );

    let dialog = adw::AlertDialog::new(Some("swhkd Installation Failed"), Some(&body));
    let commands_label = Label::builder()
        .label(&format!("Console commands:\n{}", manual_commands))
        .selectable(true)
        .wrap(true)
        .xalign(0.0)
        .css_classes(vec!["monospace"])
        .build();
    dialog.set_extra_child(Some(&commands_label));
    dialog.add_responses(&[
        ("close", "Close"),
        ("copy_link", "Copy Manual Link"),
        ("copy_commands", "Copy Commands"),
    ]);

    let parent_for_copy = parent.clone();
    dialog.connect_response(None, move |d, response| match response {
        "copy_link" => {
            if copy_text_to_clipboard(&manual_guide) {
                show_message(
                    &parent_for_copy,
                    "Copied",
                    "Manual guide link copied to clipboard.",
                );
            } else {
                show_error(
                    &parent_for_copy,
                    "Copy Failed",
                    "Clipboard is unavailable on this display.",
                );
            }
        }
        "copy_commands" => {
            if copy_text_to_clipboard(&manual_commands) {
                show_message(
                    &parent_for_copy,
                    "Copied",
                    "Console commands copied to clipboard.",
                );
            } else {
                show_error(
                    &parent_for_copy,
                    "Copy Failed",
                    "Clipboard is unavailable on this display.",
                );
            }
        }
        _ => d.force_close(),
    });

    dialog.present(Some(parent));
}

pub fn show_input<F>(
    parent: &Window,
    title: &str,
    message: &str,
    initial_value: &str,
    confirm_label: &str,
    on_confirm: F,
) where
    F: Fn(String) + 'static,
{
    let dialog = adw::AlertDialog::new(Some(title), Some(message));
    let entry = gtk4::Entry::builder().text(initial_value).build();
    entry.select_region(0, -1);
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "Cancel"), ("confirm", confirm_label)]);

    // Enter submits the dialog.
    let dialog_for_entry = dialog.clone();
    entry.connect_activate(move |_| {
        dialog_for_entry.emit_by_name::<()>("response", &[&"confirm"]);
    });

    let entry2 = entry.clone();
    dialog.connect_response(None, move |d, response| {
        if response == "confirm" {
            on_confirm(entry2.text().to_string());
        }
        d.force_close();
    });

    dialog.present(Some(parent));
}

pub fn show_missing_file<FLocate, FRemove>(
    parent: &Window,
    sound_name: &str,
    sound_path: &str,
    on_locate: FLocate,
    on_remove: FRemove,
) where
    FLocate: Fn() + 'static,
    FRemove: Fn() + 'static,
{
    let msg = format!(
        "The source file for '{}' is missing or has been moved.\nMissing path:\n{}",
        sound_name, sound_path
    );
    let dialog = adw::AlertDialog::new(Some("File Not Found"), Some(&msg));
    dialog.add_responses(&[
        ("cancel", "Cancel"),
        ("remove", "Remove Sound"),
        ("locate", "Locate File…"),
    ]);
    dialog.connect_response(None, move |d, response| {
        match response {
            "locate" => on_locate(),
            "remove" => on_remove(),
            _ => {}
        }
        d.force_close();
    });
    dialog.present(Some(parent));
}

pub fn show_path_info(parent: &Window, sound_name: &str, path: &str) {
    let msg = format!("File path for '{}':", sound_name);
    let dialog = adw::AlertDialog::new(Some("File Location"), Some(&msg));

    let path_label = Label::builder()
        .label(path)
        .selectable(true)
        .wrap(true)
        .css_classes(vec!["monospace"])
        .build();
    dialog.set_extra_child(Some(&path_label));

    dialog.add_responses(&[("close", "Close"), ("copy", "Copy to Clipboard")]);

    let path_owned = path.to_string();
    dialog.connect_response(None, move |d, response| {
        if response == "copy" {
            if let Some(display) = gtk4::gdk::Display::default() {
                display.clipboard().set_text(&path_owned);
            }
        }
        d.force_close();
    });

    dialog.present(Some(parent));
}

pub fn show_hotkey_capture<F, V>(
    parent: &Window,
    current_hotkey: Option<&str>,
    validate_hotkey: V,
    on_confirm: F,
) where
    V: Fn(&str) -> Result<(), String> + 'static,
    F: Fn(Option<String>) + 'static,
{
    let dialog = adw::AlertDialog::new(Some("Set Hotkey"), None);

    let vbox = GtkBox::new(Orientation::Vertical, 12);

    let instruction = Label::builder()
        .label("Click the capture zone below, then press your key combination.")
        .wrap(true)
        .build();
    vbox.append(&instruction);

    let capture_box = GtkBox::new(Orientation::Vertical, 8);
    capture_box.add_css_class("hotkey-capture-zone");
    capture_box.set_focusable(true);
    capture_box.set_can_focus(true);
    capture_box.set_size_request(300, 80);
    capture_box.set_halign(gtk4::Align::Center);

    let status_label = Label::builder()
        .label("Click here, then press keys…")
        .css_classes(vec!["hotkey-recording"])
        .wrap(true)
        .build();
    capture_box.append(&status_label);

    let preview_label = Label::builder()
        .label(current_hotkey.unwrap_or("Not set"))
        .css_classes(vec!["monospace"])
        .build();
    capture_box.append(&preview_label);

    vbox.append(&capture_box);
    dialog.set_extra_child(Some(&vbox));

    dialog.add_responses(&[("cancel", "Cancel"), ("clear", "Clear"), ("save", "Save")]);

    let captured: Rc<RefCell<Option<String>>> =
        Rc::new(RefCell::new(current_hotkey.map(|s| s.to_string())));

    let key_ctrl = EventControllerKey::new();
    let key_ctrl_for_response = key_ctrl.clone();
    let capture_box_for_response = capture_box.downgrade();
    let captured_for_key = Rc::clone(&captured);
    let preview_for_key = preview_label.downgrade();
    let status_for_key = status_label.downgrade();
    key_ctrl.connect_key_pressed(move |_, keyval, keycode, modifier_state| {
        let Some(status_for_key) = status_for_key.upgrade() else {
            return glib::Propagation::Stop;
        };

        let key_name = keyval.name().unwrap_or_default().to_string();
        if matches!(
            key_name.as_str(),
            "Shift_L"
                | "Shift_R"
                | "Control_L"
                | "Control_R"
                | "Alt_L"
                | "Alt_R"
                | "Super_L"
                | "Super_R"
                | "Meta_L"
                | "Meta_R"
                | "ISO_Level3_Shift"
                | "Num_Lock"
                | "Caps_Lock"
                | "Scroll_Lock"
        ) {
            return glib::Propagation::Stop;
        }

        if key_name == "Escape" {
            status_for_key.set_text("Cancelled. Click to try again…");
            return glib::Propagation::Stop;
        }

        let Some(key_token) = resolve_capture_key(&key_name, keycode) else {
            status_for_key
                .set_text("Unsupported key. Use standard keys, symbols, function keys, arrows, or numpad keys.");
            return glib::Propagation::Stop;
        };

        let mut modifiers = Vec::new();
        if modifier_state.contains(gtk4::gdk::ModifierType::CONTROL_MASK) {
            modifiers.push(HotkeyModifier::Ctrl);
        }
        if modifier_state.contains(gtk4::gdk::ModifierType::ALT_MASK) {
            modifiers.push(HotkeyModifier::Alt);
        }
        if modifier_state.contains(gtk4::gdk::ModifierType::SHIFT_MASK) {
            modifiers.push(HotkeyModifier::Shift);
        }
        if modifier_state.contains(gtk4::gdk::ModifierType::SUPER_MASK) {
            modifiers.push(HotkeyModifier::Super);
        }

        let combo = match build_captured_combo(key_token, modifiers, &validate_hotkey) {
            Ok(combo) => combo,
            Err(err) => {
                status_for_key.set_text(&format_hotkey_error(&err));
                return glib::Propagation::Stop;
            }
        };
        if let Some(preview_for_key) = preview_for_key.upgrade() {
            preview_for_key.set_text(&combo);
        }
        status_for_key.set_text("Captured! Press Save or try again.");
        *captured_for_key.borrow_mut() = Some(combo);

        glib::Propagation::Stop
    });
    capture_box.add_controller(key_ctrl);

    let capture_box_focus = capture_box.clone();
    glib::idle_add_local_once(move || {
        capture_box_focus.grab_focus();
    });

    let captured_for_resp = Rc::clone(&captured);
    dialog.connect_response(None, move |d, response| {
        if let Some(capture_box_for_response) = capture_box_for_response.upgrade() {
            capture_box_for_response.remove_controller(&key_ctrl_for_response);
        }
        match response {
            "save" => on_confirm(captured_for_resp.borrow().clone()),
            "clear" => on_confirm(None),
            _ => {}
        }
        d.force_close();
    });

    dialog.present(Some(parent));
}

#[cfg(test)]
mod tests {
    use super::{build_captured_combo, resolve_capture_key_candidates};
    use crate::hotkeys::{format_hotkey_error, HotkeyCode, HotkeyModifier};

    #[test]
    fn capture_prefers_actual_symbol_key_over_unrelated_mapped_name() {
        let resolved = resolve_capture_key_candidates("/", 0, ["BackSpace", "slash"]).unwrap();
        assert_eq!(resolved.token(), "Slash");
    }

    #[test]
    fn capture_uses_keypad_mapped_names_when_needed() {
        assert_eq!(
            resolve_capture_key_candidates("plus", 0, ["KP_Add"])
                .unwrap()
                .token(),
            "NumpadAdd"
        );
        assert_eq!(
            resolve_capture_key_candidates("slash", 0, ["KP_Divide"])
                .unwrap()
                .token(),
            "NumpadDivide"
        );
    }

    #[test]
    fn capture_rejects_combo_when_validator_fails() {
        let err = build_captured_combo(
            HotkeyCode::from_token("NumpadDivide").unwrap(),
            vec![HotkeyModifier::Ctrl],
            &|hotkey| {
                Err(format!(
                    "UNSUPPORTED_KEY_FOR_BACKEND:swhkd:{hotkey} cannot be represented by swhkd."
                ))
            },
        )
        .unwrap_err();

        assert_eq!(
            format_hotkey_error(&err),
            "This shortcut is not supported by the active hotkey backend. Ctrl+NumpadDivide cannot be represented by swhkd."
        );
    }

    #[test]
    fn capture_accepts_combo_when_validator_passes() {
        let combo = build_captured_combo(
            HotkeyCode::from_token("Slash").unwrap(),
            vec![HotkeyModifier::Ctrl],
            &|_| Ok(()),
        )
        .unwrap();
        assert_eq!(combo, "Ctrl+Slash");
    }
}
