use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, GestureDrag, Orientation};
use libadwaita as adw;
use libadwaita::prelude::BreakpointBinExt;

use crate::app_meta::{APP_ICON_NAME, APP_TITLE};
use crate::app_state::AppState;
use crate::commands;
use crate::timer_registry::TimerRegistry;

use super::dialogs::DialogHost;
use super::dnd_import;
use super::settings;
use super::sound_list::SoundList;
use super::tabs_sidebar::TabsSidebar;
use super::theme::apply_theme;
use super::transport::TransportBar;

const MIN_SIDEBAR_WIDTH: f64 = 180.0;
const MAX_SIDEBAR_WIDTH: f64 = 600.0;

fn sidebar_width_from_drag(start_width: i32, offset_x: f64) -> f64 {
    (f64::from(start_width) + offset_x)
        .round()
        .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
}

pub fn build_window(
    app: &Application,
    state: Arc<AppState>,
    _timers: &TimerRegistry,
) -> (ApplicationWindow, TransportBar) {
    {
        let cfg = state.config.lock();
        apply_theme(cfg.settings.theme);
    }

    let window = ApplicationWindow::builder()
        .application(app)
        .title(APP_TITLE)
        .icon_name(APP_ICON_NAME)
        .default_width(1400)
        .default_height(850)
        .width_request(520)
        .height_request(400)
        .build();
    window.add_css_class("main-window");

    let dialog_host = DialogHost::new();
    let root_box = GtkBox::new(Orientation::Vertical, 0);

    {
        let pw = state.pipewire_status.lock();
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
            let hotkeys = state.hotkeys.lock();
            hotkeys.availability_message()
        };
        if let Some(reason) = hotkey_message {
            let banner = adw::Banner::new(&format!("Global hotkeys unavailable — {}", reason));
            let can_install = crate::hotkeys::should_offer_swhkd_install(&reason);
            banner.set_button_label(Some(if can_install { "Install" } else { "Dismiss" }));
            banner.set_revealed(true);
            if can_install {
                let dialog_host = dialog_host.clone();
                let hotkeys = Arc::clone(&state.hotkeys);
                let projection = state.hotkey_projection.clone();
                let reason_text = reason.clone();
                banner.connect_button_clicked(move |b| {
                    dialog_host.prompt_swhkd_install(
                        Arc::clone(&hotkeys),
                        projection.clone(),
                        &reason_text,
                    );
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

    let split_view = adw::OverlaySplitView::new();
    split_view.set_vexpand(true);
    split_view.set_min_sidebar_width(MIN_SIDEBAR_WIDTH);
    split_view.set_max_sidebar_width(MAX_SIDEBAR_WIDTH);
    split_view.set_sidebar_width_fraction(0.16);

    let tabs = TabsSidebar::new(Arc::clone(&state), dialog_host.clone());
    let sidebar_box = GtkBox::new(Orientation::Horizontal, 0);
    tabs.widget().set_hexpand(true);
    sidebar_box.append(tabs.widget());
    let resize_handle = GtkBox::new(Orientation::Vertical, 0);
    resize_handle.set_vexpand(true);
    resize_handle.set_size_request(6, -1);
    resize_handle.set_cursor_from_name(Some("col-resize"));
    resize_handle.set_tooltip_text(Some("Drag to resize the sidebar"));
    let drag = GestureDrag::new();
    let drag_start_width = Rc::new(Cell::new(220));
    let pending_sidebar_width = Rc::new(Cell::new(None));
    let resize_tick_pending = Rc::new(Cell::new(false));
    {
        let sidebar_box = sidebar_box.clone();
        let drag_start_width = Rc::clone(&drag_start_width);
        drag.connect_drag_begin(move |_, _, _| {
            drag_start_width.set(sidebar_box.width());
        });
    }
    {
        let split_view = split_view.clone();
        let drag_start_width = Rc::clone(&drag_start_width);
        let pending_sidebar_width = Rc::clone(&pending_sidebar_width);
        let resize_tick_pending = Rc::clone(&resize_tick_pending);
        drag.connect_drag_update(move |_, offset_x, _| {
            let width = sidebar_width_from_drag(drag_start_width.get(), offset_x);
            pending_sidebar_width.set(Some(width));
            if resize_tick_pending.replace(true) {
                return;
            }
            let pending_sidebar_width = Rc::clone(&pending_sidebar_width);
            let resize_tick_pending = Rc::clone(&resize_tick_pending);
            split_view.add_tick_callback(move |split_view, _| {
                if let Some(width) = pending_sidebar_width.take() {
                    let total_width = f64::from(split_view.width().max(1));
                    split_view.set_sidebar_width_fraction(width / total_width);
                }
                resize_tick_pending.set(false);
                glib::ControlFlow::Break
            });
        });
    }
    resize_handle.add_controller(drag);
    sidebar_box.append(&resize_handle);
    split_view.set_sidebar(Some(&sidebar_box));

    let sound_list = SoundList::new(Arc::clone(&state), dialog_host.clone());

    {
        let transport_snapshot = transport.clone();
        let sl_snapshot = sound_list.clone();
        let last_playing: Rc<std::cell::RefCell<Vec<String>>> =
            Rc::new(std::cell::RefCell::new(Vec::new()));
        let last_active: Rc<std::cell::RefCell<Option<String>>> =
            Rc::new(std::cell::RefCell::new(None));
        crate::ui_event_bridge::set_snapshot_handler(move |snapshot| {
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
        tabs.connect_tab_selected(move |selection| {
            sl.set_active_scope(selection.identity, selection.scope);
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
        transport.set_sound_list_provider(move || sl_nav.navigation_context());
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

    split_view.set_content(Some(sound_list.widget()));
    root_box.append(&split_view);

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

    let drop_overlay =
        dnd_import::build_and_attach_drop_overlay(&window, &toast_overlay, &sound_list, &state);

    // Responsive layout: below a narrow width the sidebar collapses into an overlay and
    // the transport bar reflows onto a second row. A `BreakpointBin` lets us drive adw
    // breakpoints without switching the window away from `gtk4::ApplicationWindow`.
    let breakpoint_bin = adw::BreakpointBin::new();
    breakpoint_bin.set_size_request(520, 400);
    breakpoint_bin.set_child(Some(&drop_overlay));

    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        960.0,
        adw::LengthUnit::Px,
    ));
    {
        let split_view = split_view.clone();
        let transport = transport.clone();
        breakpoint.connect_apply(move |_| {
            split_view.set_collapsed(true);
            transport.set_compact(true);
        });
    }
    {
        let split_view = split_view.clone();
        let transport = transport.clone();
        breakpoint.connect_unapply(move |_| {
            split_view.set_collapsed(false);
            transport.set_compact(false);
        });
    }
    breakpoint_bin.add_breakpoint(breakpoint);

    // Hide the overlay sidebar while collapsed; show it inline once expanded again.
    split_view.connect_collapsed_notify(move |split_view| {
        split_view.set_show_sidebar(!split_view.is_collapsed());
        resize_handle.set_visible(!split_view.is_collapsed());
    });

    // The sidebar reveal button only appears while the sidebar is collapsed.
    {
        let toggle = transport.sidebar_toggle_button().clone();
        split_view
            .bind_property("collapsed", &toggle, "visible")
            .sync_create()
            .build();
        let split_view = split_view.clone();
        toggle.connect_clicked(move |_| {
            let shown = split_view.property::<bool>("show-sidebar");
            split_view.set_show_sidebar(!shown);
        });
    }

    window.set_child(Some(&breakpoint_bin));

    {
        let sl_settings = sound_list.clone();
        let tabs_settings = tabs.clone();
        let sl_style_settings = sound_list.clone();
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

        let settings_overlay = settings::build_settings_overlay(
            window.upcast_ref::<gtk4::Window>(),
            Arc::clone(&state),
            dialog_host.clone(),
            Some(on_library_changed),
            Some(on_list_style_changed),
        );
        drop_overlay.add_overlay(&settings_overlay);
        drop_overlay.add_overlay(dialog_host.widget());

        transport.connect_settings_requested(move || {
            settings_overlay.set_visible(true);
            settings_overlay.grab_focus();
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
        crate::ui_event_bridge::mark_explicit_play_pending();
        if let Err(e) =
            commands::play_hotkey_sound_async(sound_id, Arc::clone(state), move |result| {
                if let Err(err) = result {
                    crate::ui_event_bridge::clear_explicit_play_pending();
                    log::warn!("Hotkey playback failed for '{}': {}", sound_id_for_log, err);
                }
            })
        {
            crate::ui_event_bridge::clear_explicit_play_pending();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_drag_width_is_clamped() {
        assert_eq!(sidebar_width_from_drag(220, 80.0), 300.0);
        assert_eq!(sidebar_width_from_drag(220, 80.4), 300.0);
        assert_eq!(sidebar_width_from_drag(220, -100.0), 180.0);
        assert_eq!(sidebar_width_from_drag(500, 200.0), 600.0);
    }
}
