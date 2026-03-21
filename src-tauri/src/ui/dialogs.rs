//! Reusable GTK4 dialogs for the soundboard app.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::prelude::WidgetExt;
use gtk4::{self, Box as GtkBox, EventControllerKey, Label, Orientation, Window};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::hotkeys::{normalize_capture_key, HotkeyModifier, HotkeySpec};

/// Show a simple error message dialog.
#[allow(dead_code)]
pub fn show_error(parent: &Window, title: &str, message: &str) {
    let dialog = adw::AlertDialog::new(Some(title), Some(message));
    dialog.add_responses(&[("ok", "OK")]);
    dialog.connect_response(None, |d, _| d.force_close());
    dialog.present(Some(parent));
}

/// Show a confirmation dialog with a destructive action.
///
/// `on_confirm` is called only when the user clicks the confirm button.
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
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.connect_response(None, move |d, response| {
        if response == "confirm" {
            on_confirm();
        }
        d.force_close();
    });
    dialog.present(Some(parent));
}

/// Show a text-input dialog (e.g., for renaming).
///
/// `on_confirm` receives the entered string only when the user confirms.
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
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Suggested);

    // Allow Enter key in the entry to confirm
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

/// Show a "file not found" dialog for a missing audio source.
///
/// Offers the user three choices: cancel, remove the sound, or locate the file.
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
    dialog.set_response_appearance("locate", adw::ResponseAppearance::Suggested);
    dialog.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
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

/// Show file path info with a "Copy to Clipboard" button.
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
    dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);

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

/// Show a hotkey capture dialog for recording key combinations.
///
/// The dialog presents a capture zone that listens for key presses and displays
/// the captured combination. `on_confirm` receives `Some(combo)` on save or
/// `None` when the user clears the hotkey.
pub fn show_hotkey_capture<F>(parent: &Window, current_hotkey: Option<&str>, on_confirm: F)
where
    F: Fn(Option<String>) + 'static,
{
    let dialog = adw::AlertDialog::new(Some("Set Hotkey"), None);

    let vbox = GtkBox::new(Orientation::Vertical, 12);

    let instruction = Label::builder()
        .label("Click the capture zone below, then press your key combination.")
        .wrap(true)
        .build();
    vbox.append(&instruction);

    // Capture zone — a focusable box that receives key events
    let capture_box = GtkBox::new(Orientation::Vertical, 8);
    capture_box.add_css_class("hotkey-capture-zone");
    capture_box.set_focusable(true);
    capture_box.set_can_focus(true);
    capture_box.set_size_request(300, 80);
    capture_box.set_halign(gtk4::Align::Center);

    let status_label = Label::builder()
        .label("Click here, then press keys…")
        .css_classes(vec!["hotkey-recording"])
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
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);

    // Shared state for captured hotkey
    let captured: Rc<RefCell<Option<String>>> =
        Rc::new(RefCell::new(current_hotkey.map(|s| s.to_string())));

    // Key event controller on the capture box
    let key_ctrl = EventControllerKey::new();
    let captured_for_key = Rc::clone(&captured);
    let preview_for_key = preview_label.clone();
    let status_for_key = status_label.clone();
    key_ctrl.connect_key_pressed(move |_, keyval, keycode, modifier_state| {
        // Ignore lone modifier presses
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

        // Escape cancels capture without closing
        if key_name == "Escape" {
            status_for_key.set_text("Cancelled. Click to try again…");
            return glib::Propagation::Stop;
        }

        let Some(key_token) = normalize_capture_key(&key_name, keycode) else {
            status_for_key.set_text("Unsupported key. Use A-Z, 0-9, F-keys, arrows, or numpad.");
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

        let combo = HotkeySpec::new(modifiers, key_token).canonical_string();
        preview_for_key.set_text(&combo);
        status_for_key.set_text("Captured! Press Save or try again.");
        *captured_for_key.borrow_mut() = Some(combo);

        glib::Propagation::Stop
    });
    capture_box.add_controller(key_ctrl);

    // Auto-focus the capture box when the dialog is shown
    let capture_box_focus = capture_box.clone();
    glib::idle_add_local_once(move || {
        capture_box_focus.grab_focus();
    });

    let captured_for_resp = Rc::clone(&captured);
    dialog.connect_response(None, move |d, response| {
        match response {
            "save" => on_confirm(captured_for_resp.borrow().clone()),
            "clear" => on_confirm(None),
            _ => {}
        }
        d.force_close();
    });

    dialog.present(Some(parent));
}
