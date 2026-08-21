use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app_state::AppState;
use crate::commands;
use crate::config::ControlHotkeyAction;

use super::dialogs::DialogHost;
use super::icons;

pub(super) fn build_hotkeys_page(
    state: Arc<AppState>,
    dialog_host: DialogHost,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Control Hotkeys")
        .icon_name(icons::name(icons::KEYBOARD))
        .build();

    let unavailable_reason = {
        let hotkeys = state.hotkeys.lock();
        hotkeys.availability_message()
    };

    let description = unavailable_reason
        .as_ref()
        .map(|reason| {
            // The group description is parsed as markup, and remediation commands
            // contain "&&", which aborts the parse and blanks the whole text.
            format!(
                "These global hotkeys use the native Wayland backend when available and the X11 backend only in X11 sessions. Currently unavailable: {}",
                glib::markup_escape_text(reason)
            )
        })
        .unwrap_or_else(|| {
            "These global hotkeys work from anywhere on your desktop using the native backend for your session".to_string()
        });

    let group = adw::PreferencesGroup::builder()
        .title("Global Control Hotkeys")
        .description(&description)
        .build();

    if let Some(reason) = unavailable_reason {
        if crate::hotkeys::should_offer_swhkd_install(&reason) {
            let row = adw::ActionRow::builder()
                .title("Install Wayland hotkey support")
                .subtitle("One-click install for missing swhkd requirements")
                .build();

            let install_btn = gtk4::Button::builder()
                .label("Install")
                .css_classes(vec!["suggested-action"])
                .valign(gtk4::Align::Center)
                .build();

            let hotkeys = Arc::clone(&state.hotkeys);
            let projection = state.hotkey_projection.clone();
            let reason_text = reason.clone();
            let dialog_host_install = dialog_host.clone();
            install_btn.connect_clicked(move |_| {
                dialog_host_install.prompt_swhkd_install(
                    Arc::clone(&hotkeys),
                    projection.clone(),
                    &reason_text,
                );
            });

            row.add_suffix(&install_btn);
            group.add(&row);
        }
    }

    for meta in ControlHotkeyAction::all() {
        let row = build_hotkey_row(Arc::clone(&state), dialog_host.clone(), meta.action);
        group.add(&row);
    }

    page.add(&group);
    page
}

fn build_hotkey_row(
    state: Arc<AppState>,
    dialog_host: DialogHost,
    action: ControlHotkeyAction,
) -> adw::ActionRow {
    let current_hotkey = Rc::new(RefCell::new(None::<String>));

    let hotkey_label = gtk4::Label::builder()
        .label("Loading…")
        .css_classes(vec!["hotkey-badge"])
        .valign(gtk4::Align::Center)
        .build();

    let record_btn = gtk4::Button::builder()
        .label("Record")
        .css_classes(vec!["flat"])
        .valign(gtk4::Align::Center)
        .build();

    let clear_btn = gtk4::Button::builder()
        .label("Clear")
        .css_classes(vec!["flat", "settings-danger-btn"])
        .valign(gtk4::Align::Center)
        .sensitive(false)
        .build();

    let row = adw::ActionRow::builder()
        .title(action.title())
        .subtitle(action.subtitle())
        .build();
    row.add_suffix(&hotkey_label);
    row.add_suffix(&record_btn);
    row.add_suffix(&clear_btn);

    {
        let response = state.library.hotkey_binding(action.binding_id());
        let current = Rc::clone(&current_hotkey);
        let label = hotkey_label.downgrade();
        let clear = clear_btn.downgrade();
        if let Err(error) = commands::dispatch_async_result(
            "load_control_hotkey",
            move || response.recv(),
            move |result| match result {
                Ok(binding) => {
                    let value = binding.map(|binding| binding.accelerator);
                    if let Some(label) = label.upgrade() {
                        label.set_text(value.as_deref().unwrap_or("Not set"));
                    }
                    if let Some(clear) = clear.upgrade() {
                        clear.set_sensitive(value.is_some());
                    }
                    *current.borrow_mut() = value;
                }
                Err(error) => {
                    log::warn!("Failed to load control hotkey: {error}");
                    if let Some(label) = label.upgrade() {
                        label.set_text("Unavailable");
                    }
                }
            },
        ) {
            log::warn!("Failed to dispatch control hotkey load: {error}");
        }
    }

    {
        let state2 = Arc::clone(&state);
        let lbl = hotkey_label.downgrade();
        let clear2 = clear_btn.downgrade();
        let current2 = Rc::clone(&current_hotkey);
        let dialog_host_record = dialog_host.clone();
        record_btn.connect_clicked(move |_| {
            let current = current2.borrow().clone();
            let state3 = Arc::clone(&state2);
            let lbl2 = lbl.clone();
            let clear3 = clear2.clone();
            let current3 = Rc::clone(&current2);
            let dialog_host_weak = dialog_host_record.downgrade();
            dialog_host_record.show_hotkey_capture(
                current.as_deref(),
                move |hotkey| {
                    crate::hotkeys::canonicalize_hotkey_string(hotkey)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                },
                move |hotkey| {
                    let display_hotkey = hotkey.clone();
                    let state_done = Arc::clone(&state3);
                    let dialog_done = dialog_host_weak.clone();
                    let lbl_done = lbl2.clone();
                    let clear_done = clear3.clone();
                    let current_done = Rc::clone(&current3);
                    let dispatch = commands::set_control_hotkey_async(
                        action.id().to_string(),
                        hotkey,
                        state3.library.clone(),
                        state3.hotkey_projection.clone(),
                        move |result| match result {
                            Ok(_) => {
                                *current_done.borrow_mut() = display_hotkey.clone();
                                if let Some(label) = lbl_done.upgrade() {
                                    label.set_text(display_hotkey.as_deref().unwrap_or("Not set"));
                                }
                                if let Some(clear) = clear_done.upgrade() {
                                    clear.set_sensitive(display_hotkey.is_some());
                                }
                            }
                            Err(e) => {
                                if matches!(&e, commands::CommandError::HotkeyProjection(_)) {
                                    *current_done.borrow_mut() = display_hotkey.clone();
                                    if let Some(label) = lbl_done.upgrade() {
                                        label.set_text(
                                            display_hotkey.as_deref().unwrap_or("Not set"),
                                        );
                                    }
                                    if let Some(clear) = clear_done.upgrade() {
                                        clear.set_sensitive(display_hotkey.is_some());
                                    }
                                }
                                log::warn!("Set control hotkey failed: {e}");
                                let detail = e.to_string();
                                let message = crate::hotkeys::format_hotkey_error(&detail);
                                if let Some(dialog_host) = dialog_done.upgrade() {
                                    if crate::hotkeys::should_offer_swhkd_install(&detail) {
                                        dialog_host.show_hotkey_error_with_install_option(
                                            "Failed to Set Control Hotkey",
                                            &message,
                                            Arc::clone(&state_done.hotkeys),
                                            state_done.hotkey_projection.clone(),
                                        );
                                    } else {
                                        dialog_host
                                            .show_error("Failed to Set Control Hotkey", &message);
                                    }
                                }
                            }
                        },
                    );
                    if let Err(error) = dispatch {
                        log::warn!("Failed to dispatch control hotkey update: {error}");
                        if let Some(dialog_host) = dialog_host_weak.upgrade() {
                            dialog_host
                                .show_error("Failed to Set Control Hotkey", &error.to_string());
                        }
                    }
                },
            );
        });
    }

    {
        let state2 = Arc::clone(&state);
        let lbl = hotkey_label.downgrade();
        let dialog_host_clear = dialog_host.clone();
        let current = Rc::clone(&current_hotkey);
        clear_btn.connect_clicked(move |btn| {
            let btn = btn.downgrade();
            let lbl_done = lbl.clone();
            let dialog_done = dialog_host_clear.downgrade();
            let current_done = Rc::clone(&current);
            let dispatch = commands::set_control_hotkey_async(
                action.id().to_string(),
                None,
                state2.library.clone(),
                state2.hotkey_projection.clone(),
                move |result| match result {
                    Ok(_) => {
                        *current_done.borrow_mut() = None;
                        if let Some(label) = lbl_done.upgrade() {
                            label.set_text("Not set");
                        }
                        if let Some(button) = btn.upgrade() {
                            button.set_sensitive(false);
                        }
                    }
                    Err(e) => {
                        if matches!(&e, commands::CommandError::HotkeyProjection(_)) {
                            *current_done.borrow_mut() = None;
                            if let Some(label) = lbl_done.upgrade() {
                                label.set_text("Not set");
                            }
                            if let Some(button) = btn.upgrade() {
                                button.set_sensitive(false);
                            }
                        }
                        log::warn!("Clear control hotkey failed: {e}");
                        if let Some(dialog_host) = dialog_done.upgrade() {
                            dialog_host
                                .show_error("Failed to Clear Control Hotkey", &e.to_string());
                        }
                    }
                },
            );
            if let Err(error) = dispatch {
                log::warn!("Failed to dispatch control hotkey clear: {error}");
                dialog_host_clear.show_error("Failed to Clear Control Hotkey", &error.to_string());
            }
        });
    }

    row
}
