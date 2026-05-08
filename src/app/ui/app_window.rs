use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, Orientation, Paned};
use libadwaita as adw;

use crate::app_meta::{APP_ICON_NAME, APP_TITLE};
use crate::app_state::AppState;
use crate::commands;
use crate::timer_registry::TimerRegistry;

use super::dnd_import;
use super::settings;
use super::sound_list::SoundList;
use super::tabs_sidebar::TabsSidebar;
use super::theme::apply_theme;
use super::transport::TransportBar;

pub fn build_window(
    app: &Application,
    state: Arc<AppState>,
    _timers: &TimerRegistry,
) -> (ApplicationWindow, TransportBar) {
    {
        let cfg = state.config.lock().expect("config lock poisoned");
        apply_theme(cfg.settings.theme);
    }

    let window = ApplicationWindow::builder()
        .application(app)
        .title(APP_TITLE)
        .icon_name(APP_ICON_NAME)
        .default_width(1400)
        .default_height(850)
        .width_request(1100)
        .height_request(650)
        .build();
    window.add_css_class("main-window");

    let root_box = GtkBox::new(Orientation::Vertical, 0);

    {
        let pw = state
            .pipewire_status
            .lock()
            .expect("pipewire_status lock poisoned");
        if !pw.available {
            let banner = adw::Banner::new(
                "PipeWire not detected — virtual mic unavailable. \
                 Install PipeWire for full functionality.",
            );
            banner.set_button_label(Some("Dismiss"));
            banner.set_revealed(true);
            banner.connect_button_clicked(|b| b.set_revealed(false));
            root_box.append(&banner);
        }
    }

    {
        let hotkey_message = {
            let hotkeys = state.hotkeys.lock().expect("hotkeys lock poisoned");
            hotkeys.availability_message()
        };
        if let Some(reason) = hotkey_message {
            let banner = adw::Banner::new(&format!("Global hotkeys unavailable — {}", reason));
            let can_install = crate::hotkeys::should_offer_swhkd_install(&reason);
            banner.set_button_label(Some(if can_install { "Install" } else { "Dismiss" }));
            banner.set_revealed(true);
            if can_install {
                let window_weak = window.downgrade();
                let config = Arc::clone(&state.config);
                let hotkeys = Arc::clone(&state.hotkeys);
                let reason_text = reason.clone();
                banner.connect_button_clicked(move |b| {
                    if let Some(window) = window_weak.upgrade() {
                        crate::ui::dialogs::prompt_swhkd_install(
                            window.upcast_ref::<gtk4::Window>(),
                            Arc::clone(&config),
                            Arc::clone(&hotkeys),
                            &reason_text,
                        );
                    }
                    b.set_revealed(false);
                });
            } else {
                banner.connect_button_clicked(|b| b.set_revealed(false));
            }
            root_box.append(&banner);
        }
    }

    let transport = TransportBar::new(Arc::clone(&state));
    root_box.append(transport.widget());

    let paned = Paned::new(Orientation::Horizontal);
    paned.set_vexpand(true);

    let tabs = TabsSidebar::new(Arc::clone(&state));
    paned.set_start_child(Some(tabs.widget()));
    paned.set_position(220);
    paned.set_shrink_start_child(false);
    paned.set_resize_start_child(false);

    let sound_list = SoundList::new(Arc::clone(&state));

    {
        let transport_snapshot = transport.clone();
        let sl_snapshot = sound_list.clone();
        let last_playing: Rc<std::cell::RefCell<Vec<String>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));
        let last_active: Rc<std::cell::RefCell<Option<String>>> =
            Rc::new(std::cell::RefCell::new(None));
        crate::playback_bridge::set_snapshot_handler(move |snapshot| {
            let active_id_now: Option<String> = snapshot
                .playback_positions
                .iter()
                .find(|p| !p.finished)
                .map(|p| p.sound_id.clone());

            let playing_changed = *last_playing.borrow() != snapshot.playing_ids;
            if playing_changed {
                *last_playing.borrow_mut() = snapshot.playing_ids.clone();
                let ids: std::collections::HashSet<String> =
                    snapshot.playing_ids.iter().cloned().collect();
                sl_snapshot.set_playing_ids(ids);
            }

            let active_changed = *last_active.borrow() != active_id_now;
            if active_changed {
                *last_active.borrow_mut() = active_id_now.clone();
                sl_snapshot.set_active_sound_id(active_id_now);
            }

            transport_snapshot.handle_snapshot(snapshot);
        });
    }

    {
        let sl = sound_list.clone();
        tabs.connect_tab_selected(move |tab_id| {
            sl.set_active_tab(tab_id);
        });
    }

    {
        let sl = sound_list.clone();
        tabs.connect_tab_membership_changed(move || {
            sl.refresh_from_state();
        });
    }

    {
        let sl_search = sound_list.clone();
        transport.connect_search_changed(move |query| {
            sl_search.set_search_filter(query);
        });
    }

    {
        let sl_nav = sound_list.clone();
        transport.set_sound_list_provider(move || sl_nav.get_navigation_sounds());
    }

    {
        let sl_has = sound_list.clone();
        transport.set_has_sounds_checker(move || sl_has.has_navigation_sounds());
    }

    {
        let tabs_counts = tabs.clone();
        sound_list.connect_library_changed(move || {
            tabs_counts.reload_tabs();
        });
    }

    {
        let tabs_sync = tabs.clone();
        let sl_sync = sound_list.clone();
        transport.connect_library_changed(move || {
            sl_sync.refresh_from_state();
            tabs_sync.reload_tabs();
        });
    }

    {
        let sl_style = sound_list.clone();
        transport.connect_list_style_changed(move |style| {
            sl_style.set_list_style(&style);
        });
    }

    paned.set_end_child(Some(sound_list.widget()));
    root_box.append(&paned);

    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&root_box));
    {
        let toast_overlay = toast_overlay.clone();
        crate::ui_event_bridge::set_toast_handler(move |message| {
            show_toast(&toast_overlay, &message);
        });
    }

    {
        let (toast_tx, toast_rx) = std::sync::mpsc::channel::<String>();
        let toast_tx_tabs = toast_tx.clone();
        transport.set_toast_sender(toast_tx);
        tabs.set_toast_sender(toast_tx_tabs);
        if let Err(err) = std::thread::Builder::new()
            .name("toast-ui-bridge".to_string())
            .spawn(move || {
                while let Ok(message) = toast_rx.recv() {
                    crate::ui_event_bridge::post_toast(message);
                }
            })
        {
            log::warn!("Failed to start toast UI bridge: {}", err);
        }
    }

    // `AdwApplicationWindow` does not expose `set_content`; use `set_child` here.
    let drop_overlay =
        dnd_import::build_and_attach_drop_overlay(&window, &toast_overlay, &sound_list, &state);
    window.set_child(Some(&drop_overlay));

    {
        let settings_overlay: Rc<RefCell<Option<gtk4::Overlay>>> = Rc::new(RefCell::new(None));
        let drop_overlay = drop_overlay.clone();
        let parent_window = window.clone();
        let state_settings = Arc::clone(&state);
        let sl_settings = sound_list.clone();
        let tabs_settings = tabs.clone();
        let sl_style_settings = sound_list.clone();
        transport.connect_settings_requested(move || {
            if settings_overlay.borrow().is_none() {
                let on_library_changed: Rc<dyn Fn() + 'static> = {
                    let sl_settings = sl_settings.clone();
                    let tabs_settings = tabs_settings.clone();
                    Rc::new(move || {
                        sl_settings.refresh_from_state();
                        tabs_settings.reload_tabs();
                    })
                };
                let on_list_style_changed: Rc<dyn Fn(String) + 'static> = {
                    let sl_style_settings = sl_style_settings.clone();
                    Rc::new(move |style| {
                        sl_style_settings.set_list_style(&style);
                    })
                };

                let overlay = settings::build_settings_overlay(
                    parent_window.upcast_ref::<gtk4::Window>(),
                    Arc::clone(&state_settings),
                    Some(on_library_changed),
                    Some(on_list_style_changed),
                );
                drop_overlay.add_overlay(&overlay);
                *settings_overlay.borrow_mut() = Some(overlay);
                log::debug!("Settings overlay built lazily");
            }

            if let Some(overlay) = settings_overlay.borrow().as_ref() {
                overlay.set_visible(true);
                overlay.grab_focus();
            }
        });
    }

    let transport_cleanup = transport.clone();
    let tabs_cleanup = tabs.clone();
    let sound_list_cleanup = sound_list.clone();
    window.connect_close_request(move |_| {
        transport_cleanup.cleanup();
        tabs_cleanup.cleanup();
        sound_list_cleanup.cleanup();
        glib::Propagation::Proceed
    });

    (window, transport)
}

pub fn handle_hotkey(
    _window: &ApplicationWindow,
    state: &Arc<AppState>,
    transport: &TransportBar,
    id: &str,
) {
    if let Some(action) = crate::config::ControlHotkeyAction::from_binding_id(id) {
        handle_control_hotkey(state, transport, action);
    } else {
        let sound_id = id.to_string();
        let sound_id_for_log = sound_id.clone();
        crate::playback_bridge::mark_explicit_play_pending();
        if let Err(e) = commands::play_sound_async(
            sound_id,
            Arc::clone(&state.config),
            Arc::clone(&state.player),
            move |result| {
                if let Err(err) = result {
                    crate::playback_bridge::clear_explicit_play_pending();
                    log::warn!("Hotkey playback failed for '{}': {}", sound_id_for_log, err);
                }
            },
        ) {
            crate::playback_bridge::clear_explicit_play_pending();
            log::warn!("Failed to dispatch hotkey playback '{}': {}", id, e);
        }
    }
}

fn handle_control_hotkey(
    _state: &Arc<AppState>,
    transport: &TransportBar,
    action: crate::config::ControlHotkeyAction,
) {
    match action {
        crate::config::ControlHotkeyAction::StopAll => {
            transport.stop_all();
        }
        crate::config::ControlHotkeyAction::PlayPause => {
            transport.toggle_play_pause();
        }
        crate::config::ControlHotkeyAction::PreviousSound => {
            transport.play_previous();
        }
        crate::config::ControlHotkeyAction::NextSound => {
            transport.play_next();
        }
        crate::config::ControlHotkeyAction::MuteHeadphones => {
            transport.toggle_headphones_mute();
            transport.refresh_controls_from_state();
        }
        crate::config::ControlHotkeyAction::MuteRealMic => {
            transport.toggle_mic_mute();
            transport.refresh_controls_from_state();
        }
        crate::config::ControlHotkeyAction::CyclePlayMode => {
            transport.cycle_play_mode();
            transport.refresh_controls_from_state();
        }
    }
}

pub fn show_toast(overlay: &adw::ToastOverlay, message: &str) {
    let toast = adw::Toast::new(message);
    toast.set_timeout(2);
    overlay.add_toast(toast);
}
