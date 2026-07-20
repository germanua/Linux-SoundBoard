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
            format!(
                "These global hotkeys use the native Wayland backend when available and the X11 backend only in X11 sessions. Currently unavailable: {}",
                reason
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
            let config = Arc::clone(&state.config);
            let reason_text = reason.clone();
            let dialog_host_install = dialog_host.clone();
            install_btn.connect_clicked(move |_| {
                dialog_host_install.prompt_swhkd_install(
                    Arc::clone(&config),
                    Arc::clone(&hotkeys),
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
    let current_hotkey = {
        let cfg = state.config.lock();
        cfg.settings.control_hotkeys.get_cloned(action)
    };

    let hotkey_label = gtk4::Label::builder()
        .label(current_hotkey.as_deref().unwrap_or("Not set"))
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
        .sensitive(current_hotkey.is_some())
        .build();

    let row = adw::ActionRow::builder()
        .title(action.title())
        .subtitle(action.subtitle())
        .build();
    row.add_suffix(&hotkey_label);
    row.add_suffix(&record_btn);
    row.add_suffix(&clear_btn);

    {
        let state2 = Arc::clone(&state);
        let lbl = hotkey_label.downgrade();
        let clear2 = clear_btn.downgrade();
        let dialog_host_record = dialog_host.clone();
        record_btn.connect_clicked(move |_| {
            let current = {
                let cfg = state2.config.lock();
                cfg.settings.control_hotkeys.get_cloned(action)
            };
            let state3 = Arc::clone(&state2);
            let lbl2 = lbl.clone();
            let clear3 = clear2.clone();
            let dialog_host_weak = dialog_host_record.downgrade();
            let config_for_capture = Arc::clone(&state2.config);
            let hotkeys_for_capture = Arc::clone(&state2.hotkeys);
            dialog_host_record.show_hotkey_capture(
                current.as_deref(),
                move |hotkey| {
                    {
                        let cfg = config_for_capture.lock();
                        commands::validate_hotkey_available(&cfg, action.binding_id(), hotkey)
                            .map_err(|e| e.to_string())?;
                    }
                    hotkeys_for_capture
                        .lock()
                        .validate_hotkey_blocking(hotkey)
                        .map_err(|e| e.to_string())
                },
                move |result| match result {
                    Some(hk) => {
                        match commands::set_control_hotkey(
                            action.id().to_string(),
                            Some(hk.clone()),
                            Arc::clone(&state3.config),
                            Arc::clone(&state3.hotkeys),
                            state3.library.clone(),
                        ) {
                            Ok(_) => {
                                if let Some(lbl2) = lbl2.upgrade() {
                                    lbl2.set_text(&hk);
                                }
                                if let Some(clear3) = clear3.upgrade() {
                                    clear3.set_sensitive(true);
                                }
                            }
                            Err(e) => {
                                log::warn!("Set control hotkey failed: {e}");
                                let detail = e.to_string();
                                let message = crate::hotkeys::format_hotkey_error(&detail);
                                if let Some(dialog_host) = dialog_host_weak.upgrade() {
                                    if crate::hotkeys::should_offer_swhkd_install(&detail) {
                                        dialog_host.show_hotkey_error_with_install_option(
                                            "Failed to Set Control Hotkey",
                                            &message,
                                            Arc::clone(&state3.config),
                                            Arc::clone(&state3.hotkeys),
                                        );
                                    } else {
                                        dialog_host
                                            .show_error("Failed to Set Control Hotkey", &message);
                                    }
                                }
                            }
                        }
                    }
                    None => {
                        match commands::set_control_hotkey(
                            action.id().to_string(),
                            None,
                            Arc::clone(&state3.config),
                            Arc::clone(&state3.hotkeys),
                            state3.library.clone(),
                        ) {
                            Ok(_) => {
                                if let Some(lbl2) = lbl2.upgrade() {
                                    lbl2.set_text("Not set");
                                }
                                if let Some(clear3) = clear3.upgrade() {
                                    clear3.set_sensitive(false);
                                }
                            }
                            Err(e) => {
                                log::warn!("Clear control hotkey failed: {e}");
                                if let Some(dialog_host) = dialog_host_weak.upgrade() {
                                    dialog_host.show_error(
                                        "Failed to Clear Control Hotkey",
                                        &e.to_string(),
                                    );
                                }
                            }
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
        clear_btn.connect_clicked(move |btn| {
            match commands::set_control_hotkey(
                action.id().to_string(),
                None,
                Arc::clone(&state2.config),
                Arc::clone(&state2.hotkeys),
                state2.library.clone(),
            ) {
                Ok(_) => {
                    if let Some(lbl) = lbl.upgrade() {
                        lbl.set_text("Not set");
                    }
                    btn.set_sensitive(false);
                }
                Err(e) => {
                    log::warn!("Clear control hotkey failed: {e}");
                    dialog_host_clear.show_error("Failed to Clear Control Hotkey", &e.to_string());
                }
            }
        });
    }

    row
}
