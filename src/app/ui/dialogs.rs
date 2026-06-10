use parking_lot::Mutex;
use std::cell::RefCell;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::*;
use gtk4::{self, Box as GtkBox, EventControllerKey, Label, Orientation};
use libadwaita as adw;
use log::debug;

use crate::hotkeys::{
    format_hotkey_error, normalize_capture_key, HotkeyCode, HotkeyModifier, HotkeySpec,
};

type ResponseHandler = Box<dyn FnMut(&str) + 'static>;
type HotkeyValidator = Box<dyn Fn(&str) -> Result<(), String> + 'static>;

#[derive(Clone)]
pub struct DialogHost {
    inner: Rc<DialogHostInner>,
}

#[derive(Clone)]
pub struct DialogHostWeak {
    inner: Weak<DialogHostInner>,
}

struct DialogHostInner {
    overlay: gtk4::Overlay,
    title_label: Label,
    message_label: Label,
    content_stack: gtk4::Stack,
    cancel_btn: gtk4::Button,
    secondary_btn: gtk4::Button,
    primary_btn: gtk4::Button,
    input_entry: gtk4::Entry,
    path_label: Label,
    hotkey_capture_box: GtkBox,
    hotkey_status_label: Label,
    hotkey_preview_label: Label,
    captured_hotkey: RefCell<Option<String>>,
    hotkey_validator: RefCell<Option<HotkeyValidator>>,
    response_handler: RefCell<Option<ResponseHandler>>,
}

#[derive(Clone, Copy)]
enum ActionStyle {
    Default,
    Primary,
    Danger,
}

struct ActionSpec<'a> {
    response: &'a str,
    label: &'a str,
    style: ActionStyle,
}

impl DialogHostWeak {
    pub fn upgrade(&self) -> Option<DialogHost> {
        self.inner.upgrade().map(|inner| DialogHost { inner })
    }
}

impl DialogHost {
    pub fn new() -> Self {
        let overlay = gtk4::Overlay::builder()
            .visible(false)
            .can_focus(true)
            .focusable(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        overlay.add_css_class("lsb-settings-dialog");
        overlay.add_css_class("lsb-settings-overlay");
        overlay.add_css_class("lsb-dialog-host");

        let backdrop = gtk4::Button::builder()
            .can_focus(false)
            .css_classes(vec!["settings-overlay-backdrop"])
            .build();
        backdrop.set_hexpand(true);
        backdrop.set_vexpand(true);
        overlay.set_child(Some(&backdrop));

        let panel = GtkBox::new(Orientation::Vertical, 0);
        panel.add_css_class("settings-overlay-panel");
        panel.add_css_class("dialog-host-panel");
        panel.set_hexpand(true);

        let header = GtkBox::new(Orientation::Horizontal, 8);
        header.add_css_class("settings-overlay-header");
        header.add_css_class("dialog-host-header");

        let title_label = Label::builder()
            .css_classes(vec!["dialog-host-title"])
            .hexpand(true)
            .xalign(0.0)
            .wrap(true)
            .build();
        let close_btn = gtk4::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Close")
            .css_classes(vec!["flat", "settings-overlay-close-btn"])
            .valign(gtk4::Align::Center)
            .build();
        header.append(&title_label);
        header.append(&close_btn);
        panel.append(&header);

        let content = GtkBox::new(Orientation::Vertical, 10);
        content.add_css_class("dialog-host-content");

        let message_label = Label::builder()
            .css_classes(vec!["dialog-host-message"])
            .wrap(true)
            .xalign(0.0)
            .build();
        content.append(&message_label);

        // Size the stack to the page that is actually visible, not to the tallest /
        // widest page. Without this the small input/message dialogs reserve the height
        // of the path and hotkey pages and look mostly empty.
        let content_stack = gtk4::Stack::builder()
            .transition_type(gtk4::StackTransitionType::None)
            .vhomogeneous(false)
            .hhomogeneous(false)
            .build();

        let message_page = GtkBox::new(Orientation::Vertical, 0);
        content_stack.add_named(&message_page, Some("message"));

        let input_page = GtkBox::new(Orientation::Vertical, 0);
        let input_entry = gtk4::Entry::builder()
            .css_classes(vec!["dialog-host-input"])
            .hexpand(true)
            .build();
        input_page.append(&input_entry);
        content_stack.add_named(&input_page, Some("input"));

        let path_label = Label::builder()
            .selectable(true)
            .wrap(true)
            .xalign(0.0)
            .yalign(0.0)
            .css_classes(vec!["monospace", "dialog-host-path-label"])
            .build();
        let path_scroll = gtk4::ScrolledWindow::builder()
            .child(&path_label)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .min_content_height(96)
            .max_content_height(220)
            .build();
        path_scroll.add_css_class("dialog-host-path-scroll");
        content_stack.add_named(&path_scroll, Some("path"));

        let hotkey_page = GtkBox::new(Orientation::Vertical, 12);
        let instruction = Label::builder()
            .label("Click the capture zone below, then press your key combination.")
            .wrap(true)
            .xalign(0.0)
            .build();
        hotkey_page.append(&instruction);

        let hotkey_capture_box = GtkBox::new(Orientation::Vertical, 8);
        hotkey_capture_box.add_css_class("hotkey-capture-zone");
        hotkey_capture_box.set_focusable(true);
        hotkey_capture_box.set_can_focus(true);
        hotkey_capture_box.set_size_request(300, 80);
        hotkey_capture_box.set_halign(gtk4::Align::Center);

        let hotkey_status_label = Label::builder()
            .label("Click here, then press keys...")
            .css_classes(vec!["hotkey-recording"])
            .wrap(true)
            .build();
        hotkey_capture_box.append(&hotkey_status_label);

        let hotkey_preview_label = Label::builder()
            .label("Not set")
            .css_classes(vec!["monospace"])
            .build();
        hotkey_capture_box.append(&hotkey_preview_label);

        hotkey_page.append(&hotkey_capture_box);
        content_stack.add_named(&hotkey_page, Some("hotkey"));
        content.append(&content_stack);
        panel.append(&content);

        let actions = GtkBox::new(Orientation::Horizontal, 8);
        actions.add_css_class("dialog-host-actions");
        actions.set_halign(gtk4::Align::End);

        let cancel_btn = gtk4::Button::builder().visible(false).build();
        let secondary_btn = gtk4::Button::builder().visible(false).build();
        let primary_btn = gtk4::Button::builder().visible(false).build();
        actions.append(&cancel_btn);
        actions.append(&secondary_btn);
        actions.append(&primary_btn);
        panel.append(&actions);

        // Constrain the dialog width and let it shrink with the window instead of
        // forcing a fixed size. The clamp caps the panel at a compact maximum on wide
        // windows and tightens it down with side margins on narrow ones.
        let clamp = adw::Clamp::builder()
            .maximum_size(440)
            .tightening_threshold(360)
            .hexpand(true)
            .halign(gtk4::Align::Fill)
            .valign(gtk4::Align::Center)
            .margin_start(16)
            .margin_end(16)
            .margin_top(16)
            .margin_bottom(16)
            .child(&panel)
            .build();
        overlay.add_overlay(&clamp);

        let host = Self {
            inner: Rc::new(DialogHostInner {
                overlay,
                title_label,
                message_label,
                content_stack,
                cancel_btn,
                secondary_btn,
                primary_btn,
                input_entry,
                path_label,
                hotkey_capture_box,
                hotkey_status_label,
                hotkey_preview_label,
                captured_hotkey: RefCell::new(None),
                hotkey_validator: RefCell::new(None),
                response_handler: RefCell::new(None),
            }),
        };
        host.connect_once(backdrop, close_btn);
        host
    }

    pub fn widget(&self) -> &gtk4::Overlay {
        &self.inner.overlay
    }

    pub fn downgrade(&self) -> DialogHostWeak {
        DialogHostWeak {
            inner: Rc::downgrade(&self.inner),
        }
    }

    pub fn show_error(&self, title: &str, message: &str) {
        self.show_message(title, message);
    }

    pub fn show_message(&self, title: &str, message: &str) {
        self.prepare("message", title, message);
        self.configure_actions(None, None, Some(ActionSpec::primary("ok", "OK")));
        self.present(None);
    }

    pub fn show_confirm<F>(&self, title: &str, message: &str, confirm_label: &str, on_confirm: F)
    where
        F: Fn() + 'static,
    {
        self.prepare("message", title, message);
        let confirm_style = if confirm_label.eq_ignore_ascii_case("delete")
            || confirm_label.eq_ignore_ascii_case("remove")
        {
            ActionStyle::Danger
        } else {
            ActionStyle::Primary
        };
        self.configure_actions(
            Some(ActionSpec::default("cancel", "Cancel")),
            None,
            Some(ActionSpec::new("confirm", confirm_label, confirm_style)),
        );
        self.set_response_handler(move |response| {
            if response == "confirm" {
                on_confirm();
            }
        });
        self.present(None);
    }

    pub fn show_hotkey_error_with_install_option(
        &self,
        title: &str,
        message: &str,
        config: Arc<Mutex<crate::config::Config>>,
        hotkeys: Arc<Mutex<crate::hotkeys::HotkeyManager>>,
    ) {
        self.prepare("message", title, message);
        self.configure_actions(
            Some(ActionSpec::default("close", "Close")),
            None,
            Some(ActionSpec::primary("install", "Install swhkd")),
        );

        let host = self.downgrade();
        let message_text = message.to_string();
        self.set_response_handler(move |response| {
            if response == "install" {
                if let Some(host) = host.upgrade() {
                    host.prompt_swhkd_install(
                        Arc::clone(&config),
                        Arc::clone(&hotkeys),
                        &message_text,
                    );
                }
            }
        });
        self.present(None);
    }

    pub fn prompt_swhkd_install(
        &self,
        config: Arc<Mutex<crate::config::Config>>,
        hotkeys: Arc<Mutex<crate::hotkeys::HotkeyManager>>,
        reason: &str,
    ) {
        let prompt = format!(
            "Native Wayland hotkeys require swhkd.\n\nCurrent issue:\n{}\n\nInstall now?",
            reason
        );
        self.prepare("message", "Install Wayland Hotkey Support", &prompt);
        self.configure_actions(
            Some(ActionSpec::default("cancel", "Cancel")),
            None,
            Some(ActionSpec::primary("install", "Install")),
        );

        let host = self.downgrade();
        self.set_response_handler(move |response| {
            if response != "install" {
                return;
            }

            if let Some(host) = host.upgrade() {
                host.show_message(
                    "Installing swhkd",
                    "Installation started. This can take a few minutes.",
                );
            }

            let result_host = host.clone();
            if let Err(err) = crate::commands::install_swhkd_async(
                Arc::clone(&config),
                Arc::clone(&hotkeys),
                move |result| {
                    if let Some(host) = result_host.upgrade() {
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
                                host.show_message("Hotkey Support Installed", &body);
                            }
                            Err(err) => host.show_swhkd_install_failed_dialog(&err),
                        }
                    }
                },
            ) {
                if let Some(host) = host.upgrade() {
                    host.show_error("Failed to Start Installer", &err.to_string());
                }
            }
        });
        self.present(None);
    }

    pub fn show_input<F>(
        &self,
        title: &str,
        message: &str,
        initial_value: &str,
        confirm_label: &str,
        on_confirm: F,
    ) where
        F: Fn(String) + 'static,
    {
        self.prepare("input", title, message);
        self.inner.input_entry.set_text(initial_value);
        self.inner.input_entry.select_region(0, -1);
        self.configure_actions(
            Some(ActionSpec::default("cancel", "Cancel")),
            None,
            Some(ActionSpec::primary("confirm", confirm_label)),
        );

        let entry = self.inner.input_entry.clone();
        self.set_response_handler(move |response| {
            if response == "confirm" {
                on_confirm(entry.text().to_string());
            }
        });
        self.present(Some(self.inner.input_entry.clone().upcast()));
    }

    pub fn show_missing_file<FLocate, FRemove>(
        &self,
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
        self.prepare("message", "File Not Found", &msg);
        self.configure_actions(
            Some(ActionSpec::default("cancel", "Cancel")),
            Some(ActionSpec::danger("remove", "Remove Sound")),
            Some(ActionSpec::primary("locate", "Locate File...")),
        );
        self.set_response_handler(move |response| match response {
            "locate" => on_locate(),
            "remove" => on_remove(),
            _ => {}
        });
        self.present(None);
    }

    pub fn show_path_info(&self, sound_name: &str, path: &str) {
        let msg = format!("File path for '{}':", sound_name);
        self.prepare("path", "File Location", &msg);
        self.inner.path_label.set_text(path);
        self.configure_actions(
            Some(ActionSpec::default("close", "Close")),
            None,
            Some(ActionSpec::primary("copy", "Copy to Clipboard")),
        );

        let host = self.downgrade();
        let path_owned = path.to_string();
        self.set_response_handler(move |response| {
            if response == "copy" {
                if copy_text_to_clipboard(&path_owned) {
                    if let Some(host) = host.upgrade() {
                        host.show_message("Copied", "File path copied to clipboard.");
                    }
                } else if let Some(host) = host.upgrade() {
                    host.show_error("Copy Failed", "Clipboard is unavailable on this display.");
                }
            }
        });
        self.present(None);
    }

    pub fn show_hotkey_capture<F, V>(
        &self,
        current_hotkey: Option<&str>,
        validate_hotkey: V,
        on_confirm: F,
    ) where
        V: Fn(&str) -> Result<(), String> + 'static,
        F: Fn(Option<String>) + 'static,
    {
        self.prepare("hotkey", "Set Hotkey", "");
        self.inner
            .hotkey_status_label
            .set_text("Click here, then press keys...");
        self.inner
            .hotkey_preview_label
            .set_text(current_hotkey.unwrap_or("Not set"));
        *self.inner.captured_hotkey.borrow_mut() = current_hotkey.map(str::to_string);
        *self.inner.hotkey_validator.borrow_mut() = Some(Box::new(validate_hotkey));

        self.configure_actions(
            Some(ActionSpec::default("cancel", "Cancel")),
            Some(ActionSpec::danger("clear", "Clear")),
            Some(ActionSpec::primary("save", "Save")),
        );

        let host = self.downgrade();
        self.set_response_handler(move |response| {
            let Some(host) = host.upgrade() else {
                return;
            };
            match response {
                "save" => {
                    let captured_hotkey = host.inner.captured_hotkey.borrow().clone();
                    on_confirm(captured_hotkey);
                }
                "clear" => on_confirm(None),
                _ => {}
            }
        });
        self.present(Some(self.inner.hotkey_capture_box.clone().upcast()));
    }

    fn show_swhkd_install_failed_dialog(&self, err: &crate::hotkeys::SwhkdInstallError) {
        let manual_guide = crate::hotkeys::SWHKD_UPSTREAM_INSTALL_URL.to_string();
        let manual_commands = crate::hotkeys::manual_swhkd_install_commands();
        let body = format!(
            "{}\n\n{}\n\nFailure kind: {:?}\nFailure state: {:?}\n\nManual guide:\n{}",
            err.summary, err.details, err.kind, err.state, manual_guide
        );

        self.prepare("path", "swhkd Installation Failed", &body);
        self.inner
            .path_label
            .set_text(&format!("Console commands:\n{}", manual_commands));
        self.configure_actions(
            Some(ActionSpec::default("close", "Close")),
            Some(ActionSpec::default("copy_link", "Copy Manual Link")),
            Some(ActionSpec::primary("copy_commands", "Copy Commands")),
        );

        let host = self.downgrade();
        self.set_response_handler(move |response| match response {
            "copy_link" => {
                let copied = copy_text_to_clipboard(&manual_guide);
                if let Some(host) = host.upgrade() {
                    if copied {
                        host.show_message("Copied", "Manual guide link copied to clipboard.");
                    } else {
                        host.show_error("Copy Failed", "Clipboard is unavailable on this display.");
                    }
                }
            }
            "copy_commands" => {
                let copied = copy_text_to_clipboard(&manual_commands);
                if let Some(host) = host.upgrade() {
                    if copied {
                        host.show_message("Copied", "Console commands copied to clipboard.");
                    } else {
                        host.show_error("Copy Failed", "Clipboard is unavailable on this display.");
                    }
                }
            }
            _ => {}
        });
        self.present(None);
    }

    fn connect_once(&self, backdrop: gtk4::Button, close_btn: gtk4::Button) {
        {
            let host = self.downgrade();
            backdrop.connect_clicked(move |_| {
                if let Some(host) = host.upgrade() {
                    host.dismiss();
                }
            });
        }
        {
            let host = self.downgrade();
            close_btn.connect_clicked(move |_| {
                if let Some(host) = host.upgrade() {
                    host.dismiss();
                }
            });
        }
        {
            let host = self.downgrade();
            self.inner.cancel_btn.connect_clicked(move |button| {
                if let Some(host) = host.upgrade() {
                    host.handle_response(&button.widget_name());
                }
            });
        }
        {
            let host = self.downgrade();
            self.inner.secondary_btn.connect_clicked(move |button| {
                if let Some(host) = host.upgrade() {
                    host.handle_response(&button.widget_name());
                }
            });
        }
        {
            let host = self.downgrade();
            self.inner.primary_btn.connect_clicked(move |button| {
                if let Some(host) = host.upgrade() {
                    host.handle_response(&button.widget_name());
                }
            });
        }
        {
            let host = self.downgrade();
            self.inner.input_entry.connect_activate(move |_| {
                if let Some(host) = host.upgrade() {
                    host.handle_response("confirm");
                }
            });
        }
        {
            let host = self.downgrade();
            let key = EventControllerKey::new();
            key.set_propagation_phase(gtk4::PropagationPhase::Capture);
            key.connect_key_pressed(move |_, keyval, _, _| {
                if keyval.name().as_deref() == Some("Escape") {
                    if let Some(host) = host.upgrade() {
                        host.dismiss();
                    }
                    return gtk4::glib::Propagation::Stop;
                }
                gtk4::glib::Propagation::Proceed
            });
            self.inner.overlay.add_controller(key);
        }
        {
            let host = self.downgrade();
            let key_ctrl = EventControllerKey::new();
            key_ctrl.connect_key_pressed(move |_, keyval, keycode, modifier_state| {
                let Some(host) = host.upgrade() else {
                    return glib::Propagation::Stop;
                };
                host.handle_hotkey_key_pressed(keyval, keycode, modifier_state)
            });
            self.inner.hotkey_capture_box.add_controller(key_ctrl);
        }
    }

    fn prepare(&self, page: &str, title: &str, message: &str) {
        self.clear_runtime_state();
        self.inner.title_label.set_text(title);
        self.inner.message_label.set_text(message);
        self.inner.message_label.set_visible(!message.is_empty());
        self.inner.content_stack.set_visible_child_name(page);
    }

    fn present(&self, focus_widget: Option<gtk4::Widget>) {
        self.inner.overlay.set_visible(true);
        self.inner.overlay.grab_focus();
        if let Some(widget) = focus_widget {
            glib::idle_add_local_once(move || {
                widget.grab_focus();
            });
        }
    }

    fn set_response_handler<F>(&self, handler: F)
    where
        F: FnMut(&str) + 'static,
    {
        *self.inner.response_handler.borrow_mut() = Some(Box::new(handler));
    }

    fn handle_response(&self, response: &str) {
        let mut handler = self.inner.response_handler.borrow_mut().take();
        self.inner.hotkey_validator.borrow_mut().take();
        self.inner.overlay.set_visible(false);
        if let Some(handler) = handler.as_mut() {
            handler(response);
        }
        if !self.inner.overlay.is_visible() {
            self.reset_widgets();
        }
    }

    fn dismiss(&self) {
        self.inner.response_handler.borrow_mut().take();
        self.inner.hotkey_validator.borrow_mut().take();
        self.inner.overlay.set_visible(false);
        self.reset_widgets();
    }

    fn clear_runtime_state(&self) {
        self.inner.response_handler.borrow_mut().take();
        self.inner.hotkey_validator.borrow_mut().take();
        *self.inner.captured_hotkey.borrow_mut() = None;
        self.reset_widgets();
    }

    fn reset_widgets(&self) {
        self.inner.input_entry.set_text("");
        self.inner.path_label.set_text("");
        self.inner
            .hotkey_status_label
            .set_text("Click here, then press keys...");
        self.inner.hotkey_preview_label.set_text("Not set");
        self.clear_button(&self.inner.cancel_btn);
        self.clear_button(&self.inner.secondary_btn);
        self.clear_button(&self.inner.primary_btn);
    }

    fn configure_actions(
        &self,
        cancel: Option<ActionSpec<'_>>,
        secondary: Option<ActionSpec<'_>>,
        primary: Option<ActionSpec<'_>>,
    ) {
        self.configure_button(&self.inner.cancel_btn, cancel);
        self.configure_button(&self.inner.secondary_btn, secondary);
        self.configure_button(&self.inner.primary_btn, primary);
    }

    fn configure_button(&self, button: &gtk4::Button, spec: Option<ActionSpec<'_>>) {
        self.clear_button(button);
        let Some(spec) = spec else {
            return;
        };

        button.set_label(spec.label);
        button.set_widget_name(spec.response);
        button.add_css_class("dialog-host-action-btn");
        match spec.style {
            ActionStyle::Default => button.add_css_class("flat"),
            ActionStyle::Primary => button.add_css_class("settings-primary-btn"),
            ActionStyle::Danger => button.add_css_class("settings-danger-btn"),
        }
        button.set_visible(true);
    }

    fn clear_button(&self, button: &gtk4::Button) {
        button.set_visible(false);
        button.set_label("");
        button.set_widget_name("");
        button.remove_css_class("dialog-host-action-btn");
        button.remove_css_class("flat");
        button.remove_css_class("settings-primary-btn");
        button.remove_css_class("settings-danger-btn");
    }

    fn handle_hotkey_key_pressed(
        &self,
        keyval: gtk4::gdk::Key,
        keycode: u32,
        modifier_state: gtk4::gdk::ModifierType,
    ) -> glib::Propagation {
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
            self.inner
                .hotkey_status_label
                .set_text("Cancelled. Click to try again...");
            return glib::Propagation::Stop;
        }

        let Some(key_token) = resolve_capture_key(&key_name, keycode) else {
            self.inner.hotkey_status_label.set_text(
                "Unsupported key. Use standard keys, symbols, function keys, arrows, or numpad keys.",
            );
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

        let validator = self.inner.hotkey_validator.borrow();
        let Some(validate_hotkey) = validator.as_ref() else {
            return glib::Propagation::Stop;
        };
        let combo = match build_captured_combo(key_token, modifiers, validate_hotkey.as_ref()) {
            Ok(combo) => combo,
            Err(err) => {
                self.inner
                    .hotkey_status_label
                    .set_text(&format_hotkey_error(&err));
                return glib::Propagation::Stop;
            }
        };

        self.inner.hotkey_preview_label.set_text(&combo);
        self.inner
            .hotkey_status_label
            .set_text("Captured! Press Save or try again.");
        *self.inner.captured_hotkey.borrow_mut() = Some(combo);

        glib::Propagation::Stop
    }
}

impl ActionSpec<'_> {
    fn new<'a>(response: &'a str, label: &'a str, style: ActionStyle) -> ActionSpec<'a> {
        ActionSpec {
            response,
            label,
            style,
        }
    }

    fn default<'a>(response: &'a str, label: &'a str) -> ActionSpec<'a> {
        Self::new(response, label, ActionStyle::Default)
    }

    fn primary<'a>(response: &'a str, label: &'a str) -> ActionSpec<'a> {
        Self::new(response, label, ActionStyle::Primary)
    }

    fn danger<'a>(response: &'a str, label: &'a str) -> ActionSpec<'a> {
        Self::new(response, label, ActionStyle::Danger)
    }
}

fn copy_text_to_clipboard(text: &str) -> bool {
    if let Some(display) = gtk4::gdk::Display::default() {
        display.clipboard().set_text(text);
        true
    } else {
        false
    }
}

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

fn build_captured_combo(
    key_token: HotkeyCode,
    modifiers: Vec<HotkeyModifier>,
    validate_hotkey: &dyn Fn(&str) -> Result<(), String>,
) -> Result<String, String> {
    let combo = HotkeySpec::new(modifiers, key_token).canonical_string();
    validate_hotkey(&combo)?;
    Ok(combo)
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
