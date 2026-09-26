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

pub struct HotkeyScopePrompt {
    pub scoped_now: bool,
}

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
    hotkey_scope_check: gtk4::CheckButton,
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

pub(super) fn dismiss_on_press_outside<F>(
    overlay: &gtk4::Overlay,
    panel: &impl IsA<gtk4::Widget>,
    on_dismiss: F,
) where
    F: Fn() + 'static,
{
    let gesture = gtk4::GestureClick::new();
    gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
    let overlay_coords = overlay.clone();
    let panel = panel.clone().upcast::<gtk4::Widget>();
    gesture.connect_pressed(move |gesture, _, x, y| {
        let Some(bounds) = panel.compute_bounds(&overlay_coords) else {
            return;
        };
        if bounds.contains_point(&gtk4::graphene::Point::new(x as f32, y as f32)) {
            return;
        }
        gesture.set_state(gtk4::EventSequenceState::Claimed);
        on_dismiss();
    });
    overlay.add_controller(gesture);
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

        let hotkey_scope_check = gtk4::CheckButton::builder()
            .label("Only while this tab is open")
            .halign(gtk4::Align::Center)
            .visible(false)
            .build();
        hotkey_page.append(&hotkey_scope_check);
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
                hotkey_scope_check,
                hotkey_preview_label,
                captured_hotkey: RefCell::new(None),
                hotkey_validator: RefCell::new(None),
                response_handler: RefCell::new(None),
            }),
        };
        host.connect_once(backdrop, close_btn, panel);
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

    pub fn is_capturing_hotkey(&self) -> bool {
        self.inner.hotkey_validator.borrow().is_some()
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
        hotkeys: Arc<Mutex<crate::hotkeys::HotkeyManager>>,
        projection: crate::hotkeys::HotkeyProjectionCoordinator,
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
                        Arc::clone(&hotkeys),
                        projection.clone(),
                        &message_text,
                    );
                }
            }
        });
        self.present(None);
    }

    pub fn prompt_swhkd_install(
        &self,
        hotkeys: Arc<Mutex<crate::hotkeys::HotkeyManager>>,
        projection: crate::hotkeys::HotkeyProjectionCoordinator,
        reason: &str,
    ) {
        if crate::hotkeys::uinput_unavailable() {
            self.prompt_uinput_then_install(hotkeys, projection, reason);
            return;
        }
        self.prompt_swhkd_install_with_uinput(hotkeys, projection, reason, false);
    }

    fn prompt_uinput_then_install(
        &self,
        hotkeys: Arc<Mutex<crate::hotkeys::HotkeyManager>>,
        projection: crate::hotkeys::HotkeyProjectionCoordinator,
        reason: &str,
    ) {
        self.prepare(
            "message",
            "Load the uinput kernel module?",
            "swhkd reads your keyboards directly, so it has to type the keys it does not \
             use back to the system through a virtual keyboard. The kernel's uinput module \
             provides that, and it is not available here — swhkd would exit at startup.\n\n\
             Loading it makes no other change, and enabling it at boot keeps hotkeys working \
             after a restart. You can decline and load it yourself later.",
        );
        self.configure_actions(
            Some(ActionSpec::default("cancel", "Cancel")),
            Some(ActionSpec::default("skip", "Install without it")),
            Some(ActionSpec::primary("uinput", "Load uinput")),
        );

        let host = self.downgrade();
        let reason_text = reason.to_string();
        self.set_response_handler(move |response| {
            if response == "cancel" {
                return;
            }
            let enable_uinput = response == "uinput";
            if let Some(host) = host.upgrade() {
                host.prompt_swhkd_install_with_uinput(
                    Arc::clone(&hotkeys),
                    projection.clone(),
                    &reason_text,
                    enable_uinput,
                );
            }
        });
        self.present(None);
    }

    fn prompt_swhkd_install_with_uinput(
        &self,
        hotkeys: Arc<Mutex<crate::hotkeys::HotkeyManager>>,
        projection: crate::hotkeys::HotkeyProjectionCoordinator,
        reason: &str,
        enable_uinput: bool,
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
                Arc::clone(&hotkeys),
                projection.clone(),
                enable_uinput,
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
        scope_prompt: Option<HotkeyScopePrompt>,
        validate_hotkey: V,
        on_confirm: F,
    ) where
        V: Fn(&str) -> Result<(), String> + 'static,
        F: Fn(Option<String>, bool) + 'static,
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
        match scope_prompt {
            Some(prompt) => {
                self.inner.hotkey_scope_check.set_active(prompt.scoped_now);
                self.inner.hotkey_scope_check.set_visible(true);
            }
            None => {
                self.inner.hotkey_scope_check.set_active(false);
                self.inner.hotkey_scope_check.set_visible(false);
            }
        }

        self.configure_actions(
            Some(ActionSpec::default("cancel", "Cancel")),
            Some(ActionSpec::danger("clear", "Clear")),
            Some(ActionSpec::primary("save", "Save")),
        );
        self.sync_save_sensitivity();

        let host = self.downgrade();
        self.set_response_handler(move |response| {
            let Some(host) = host.upgrade() else {
                return;
            };
            let captured = host.inner.captured_hotkey.borrow().clone();
            match capture_action(response, captured.as_deref()) {
                CaptureAction::Forward(payload) => {
                    on_confirm(payload, host.inner.hotkey_scope_check.is_active());
                }
                CaptureAction::NoChange | CaptureAction::Ignore => {}
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

    fn connect_once(&self, backdrop: gtk4::Button, close_btn: gtk4::Button, panel: GtkBox) {
        {
            let host = self.downgrade();
            dismiss_on_press_outside(&self.inner.overlay, &panel, move || {
                if let Some(host) = host.upgrade() {
                    host.dismiss();
                }
            });
        }
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
        {
            let capture_box = self.inner.hotkey_capture_box.clone();
            let gesture = gtk4::GestureClick::new();
            gesture.set_button(1);
            gesture.connect_pressed(move |_, _, _, _| {
                capture_box.grab_focus();
            });
            self.inner.hotkey_capture_box.add_controller(gesture);
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
        self.raise_to_front();
        self.inner.overlay.set_visible(true);
        self.inner.overlay.grab_focus();
        if let Some(widget) = focus_widget {
            glib::idle_add_local_once(move || {
                widget.grab_focus();
            });
        }
    }

    fn raise_to_front(&self) {
        let Some(parent) = self
            .inner
            .overlay
            .parent()
            .and_then(|parent| parent.downcast::<gtk4::Overlay>().ok())
        else {
            return;
        };
        parent.remove_overlay(&self.inner.overlay);
        parent.add_overlay(&self.inner.overlay);
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
        button.set_sensitive(true);
        button.remove_css_class("dialog-host-action-btn");
        button.remove_css_class("flat");
        button.remove_css_class("settings-primary-btn");
        button.remove_css_class("settings-danger-btn");
    }

    fn sync_save_sensitivity(&self) {
        let captured = self.inner.captured_hotkey.borrow().is_some();
        self.inner.primary_btn.set_sensitive(captured);
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
        self.sync_save_sensitivity();

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

#[derive(Debug, PartialEq, Eq)]
enum CaptureAction {
    Forward(Option<String>),
    NoChange,
    Ignore,
}

fn capture_action(response: &str, captured: Option<&str>) -> CaptureAction {
    match response {
        "clear" => CaptureAction::Forward(None),
        "save" => match captured {
            Some(chord) => CaptureAction::Forward(Some(chord.to_string())),
            None => CaptureAction::NoChange,
        },
        _ => CaptureAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_captured_combo, capture_action, resolve_capture_key_candidates, CaptureAction,
        DialogHost, HotkeyScopePrompt,
    };
    use crate::hotkeys::{format_hotkey_error, HotkeyCode, HotkeyModifier};
    use gtk4::prelude::*;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::Arc;

    type Recorded = Rc<RefCell<Option<(Option<String>, bool)>>>;

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

    #[test]
    fn a_save_with_nothing_captured_is_not_a_clear() {
        assert_eq!(
            capture_action("save", None),
            CaptureAction::NoChange,
            "a save with nothing captured asks for no change"
        );
        assert_eq!(
            capture_action("save", Some("F8")),
            CaptureAction::Forward(Some("F8".to_string())),
            "a save forwards the captured chord"
        );
        assert_eq!(
            capture_action("clear", None),
            CaptureAction::Forward(None),
            "clear is explicit even with nothing captured"
        );
        assert_eq!(
            capture_action("clear", Some("F8")),
            CaptureAction::Forward(None),
            "clear ignores any captured chord"
        );
        assert_eq!(capture_action("cancel", None), CaptureAction::Ignore);
        assert_eq!(
            capture_action("ok", Some("F8")),
            CaptureAction::Ignore,
            "only save and clear are acted on"
        );
    }

    fn open_capture(
        host: &DialogHost,
        prefill: Option<&str>,
        scoped_now: bool,
        checkbox: bool,
    ) -> Recorded {
        let recorded = Rc::new(RefCell::new(None));
        let sink = Rc::clone(&recorded);
        host.show_hotkey_capture(
            prefill,
            Some(HotkeyScopePrompt { scoped_now }),
            |_| Ok(()),
            move |hotkey, scoped| {
                *sink.borrow_mut() = Some((hotkey, scoped));
            },
        );
        host.inner.hotkey_scope_check.set_active(checkbox);
        recorded
    }

    fn open_capture_with_sink<F>(
        host: &DialogHost,
        prefill: Option<&str>,
        scoped_now: bool,
        checkbox: bool,
        sink: F,
    ) where
        F: Fn(Option<String>, bool) + 'static,
    {
        host.show_hotkey_capture(
            prefill,
            Some(HotkeyScopePrompt { scoped_now }),
            |_| Ok(()),
            sink,
        );
        host.inner.hotkey_scope_check.set_active(checkbox);
    }

    fn capture_response(
        host: &DialogHost,
        prefill: Option<&str>,
        scoped_now: bool,
        checkbox: bool,
        response: &str,
    ) -> Option<(Option<String>, bool)> {
        let recorded = open_capture(host, prefill, scoped_now, checkbox);
        host.handle_response(response);
        let result = recorded.borrow().clone();
        result
    }

    fn scope_chords(
        library: &crate::library_store::LibraryStore,
        sound_id: &str,
        tab_scope: Option<&str>,
    ) -> Vec<String> {
        let bindings = library
            .hotkey_bindings_for_sound(sound_id)
            .recv()
            .expect("read the sound's bindings");
        let mut chords: Vec<String> = bindings
            .iter()
            .filter(|binding| binding.tab_scope.as_deref() == tab_scope)
            .map(|binding| binding.accelerator.clone())
            .collect();
        chords.sort();
        chords
    }

    fn mirror_production_callback(
        ids: Vec<String>,
        active_scope: String,
        library: crate::library_store::LibraryStore,
        projection: crate::hotkeys::HotkeyProjectionCoordinator,
    ) -> impl Fn(Option<String>, bool) + 'static {
        move |hotkey, scoped| {
            let tab_scope = scoped.then(|| active_scope.clone());
            crate::commands::set_hotkey_many(
                ids.clone(),
                hotkey,
                true,
                tab_scope,
                library.clone(),
                projection.clone(),
            )
            .expect("the mirrored callback commits");
        }
    }

    #[test]
    #[ignore = "drives the shared GTK main context: needs a display and must \
                run alone, e.g. xvfb-run -a cargo test --lib -- --ignored --exact \
                ui::dialogs::tests::clear_forwards_the_live_scope_toggle_like_save"]
    #[allow(clippy::print_stderr)]
    fn clear_forwards_the_live_scope_toggle_like_save() {
        if gtk4::init().is_err() {
            eprintln!("skipped: no display available");
            return;
        }

        let host = DialogHost::new();
        let prefill = Some("Ctrl+Alt+KeyA");

        assert_eq!(
            capture_response(&host, prefill, true, true, "clear"),
            Some((None, true))
        );
        assert_eq!(
            capture_response(&host, prefill, true, false, "clear"),
            Some((None, false))
        );
        assert_eq!(
            capture_response(&host, prefill, false, true, "clear"),
            Some((None, true))
        );

        for checkbox in [true, false] {
            let saved = capture_response(&host, prefill, true, checkbox, "save");
            let cleared = capture_response(&host, prefill, true, checkbox, "clear");
            assert_eq!(
                saved.map(|(_, scoped)| scoped),
                cleared.map(|(_, scoped)| scoped),
                "Save and Clear must report the same scope"
            );
        }
    }

    #[test]
    #[ignore = "drives the shared GTK main context: needs a display and must \
                run alone, e.g. xvfb-run -a cargo test --lib -- --ignored --exact \
                ui::dialogs::tests::a_save_without_a_captured_chord_preserves_the_bindings"]
    #[allow(clippy::print_stderr)]
    fn a_save_without_a_captured_chord_preserves_the_bindings() {
        if gtk4::init().is_err() {
            eprintln!("skipped: no display available");
            return;
        }

        let temp_dir =
            std::env::temp_dir().join(format!("lsb-capture-dialog-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create the test directory");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open a disposable library store");
        let hotkeys = Arc::new(parking_lot::Mutex::new(
            crate::hotkeys::HotkeyManager::new_test_noop(),
        ));
        let projection = crate::hotkeys::HotkeyProjectionCoordinator::new(library.clone(), hotkeys);

        let sound_a = crate::config::Sound::new("Alpha".to_string(), "/tmp/alpha.wav".to_string());
        let sound_b = crate::config::Sound::new("Beta".to_string(), "/tmp/beta.wav".to_string());
        let id_a = sound_a.id.clone();
        let id_b = sound_b.id.clone();
        library
            .apply_batch(crate::library_store::LibraryBatch::Sounds(vec![
                crate::library_store::SoundRecord {
                    sound: sound_a,
                    general_position: 0,
                    locations: Vec::new(),
                },
                crate::library_store::SoundRecord {
                    sound: sound_b,
                    general_position: 1,
                    locations: Vec::new(),
                },
            ]))
            .recv()
            .expect("seed the sounds");

        for (id, global, scoped) in [
            (&id_a, "F8", "Ctrl+Alt+Digit1"),
            (&id_b, "F9", "Ctrl+Alt+Digit2"),
        ] {
            library
                .set_sound_hotkeys(vec![id.clone()], Some(global.to_string()), None)
                .recv()
                .expect("seed the global binding");
            library
                .set_sound_hotkeys(
                    vec![id.clone()],
                    Some(scoped.to_string()),
                    Some("tab:one".to_string()),
                )
                .recv()
                .expect("seed the tab binding");
        }
        assert_eq!(scope_chords(&library, &id_a, None), ["F8"]);
        assert_eq!(scope_chords(&library, &id_b, None), ["F9"]);

        let ids = vec![id_a.clone(), id_b.clone()];
        let db_host = DialogHost::new();

        open_capture_with_sink(
            &db_host,
            None,
            false,
            false,
            mirror_production_callback(
                ids.clone(),
                "tab:one".to_string(),
                library.clone(),
                projection.clone(),
            ),
        );
        db_host.handle_response("save");
        assert_eq!(
            scope_chords(&library, &id_a, None),
            ["F8"],
            "a neutral save must not delete Alpha's global binding"
        );
        assert_eq!(
            scope_chords(&library, &id_b, None),
            ["F9"],
            "a neutral save must not delete Beta's global binding"
        );

        open_capture_with_sink(
            &db_host,
            None,
            false,
            true,
            mirror_production_callback(
                ids.clone(),
                "tab:one".to_string(),
                library.clone(),
                projection.clone(),
            ),
        );
        db_host.handle_response("save");
        assert_eq!(
            scope_chords(&library, &id_a, Some("tab:one")),
            ["Ctrl+Alt+Digit1"],
            "a neutral save must not delete Alpha's tab binding"
        );
        assert_eq!(
            scope_chords(&library, &id_b, Some("tab:one")),
            ["Ctrl+Alt+Digit2"],
            "a neutral save must not delete Beta's tab binding"
        );

        open_capture_with_sink(
            &db_host,
            None,
            false,
            false,
            mirror_production_callback(
                ids.clone(),
                "tab:one".to_string(),
                library.clone(),
                projection.clone(),
            ),
        );
        db_host.handle_response("clear");
        assert!(
            scope_chords(&library, &id_a, None).is_empty(),
            "a global clear must remove Alpha's global binding"
        );
        assert!(
            scope_chords(&library, &id_b, None).is_empty(),
            "a global clear must remove Beta's global binding"
        );
        assert_eq!(
            scope_chords(&library, &id_a, Some("tab:one")),
            ["Ctrl+Alt+Digit1"],
            "a global clear must leave the tab scope alone"
        );

        open_capture_with_sink(
            &db_host,
            None,
            false,
            true,
            mirror_production_callback(ids, "tab:one".to_string(), library.clone(), projection),
        );
        db_host.handle_response("clear");
        assert!(
            scope_chords(&library, &id_a, Some("tab:one")).is_empty(),
            "a tab clear must remove Alpha's tab binding"
        );
        assert!(
            scope_chords(&library, &id_b, Some("tab:one")).is_empty(),
            "a tab clear must remove Beta's tab binding"
        );

        let host = DialogHost::new();
        let chord = Some("F8");

        assert_eq!(
            capture_response(&host, None, false, false, "save"),
            None,
            "a save with nothing captured must forward nothing"
        );
        assert_eq!(
            capture_response(&host, None, false, false, "clear"),
            Some((None, false)),
            "an explicit clear must still work from the neutral state"
        );
        assert_eq!(
            capture_response(&host, chord, false, true, "save"),
            Some((Some("F8".to_string()), true)),
            "a captured chord saves with the live scope"
        );

        open_capture(&host, None, false, false);
        assert!(
            !host.inner.primary_btn.is_sensitive(),
            "Save must be insensitive with nothing captured"
        );
        assert!(
            host.inner.secondary_btn.is_sensitive(),
            "Clear must stay usable in the neutral state"
        );
        open_capture(&host, chord, false, false);
        assert!(
            host.inner.primary_btn.is_sensitive(),
            "Save must be sensitive once a chord is prefilled"
        );

        open_capture(&host, None, false, false);
        assert!(!host.inner.primary_btn.is_sensitive());
        let propagation =
            host.handle_hotkey_key_pressed(gtk4::gdk::Key::F8, 0, gtk4::gdk::ModifierType::empty());
        assert_eq!(propagation, glib::Propagation::Stop);
        assert_eq!(
            host.inner.captured_hotkey.borrow().as_deref(),
            Some("F8"),
            "the key handler must record the captured chord"
        );
        assert!(
            host.inner.primary_btn.is_sensitive(),
            "Save must become sensitive after a capture"
        );

        open_capture(&host, chord, false, false);
        assert!(host.inner.primary_btn.is_sensitive());
        assert_eq!(
            capture_response(&host, chord, false, false, "save"),
            Some((Some("F8".to_string()), false))
        );
        open_capture(&host, None, false, false);
        assert!(
            !host.inner.primary_btn.is_sensitive(),
            "reopening with no chord must clear Save's sensitivity"
        );
        assert!(
            host.inner.captured_hotkey.borrow().is_none(),
            "no stale chord may survive a reopen"
        );
    }
}
