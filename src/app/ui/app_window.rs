use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, Orientation, Paned};
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

const MAX_SIDEBAR_WIDTH: i32 = 600;

fn cap_sidebar_width(width: i32) -> i32 {
    width.min(MAX_SIDEBAR_WIDTH)
}

pub fn build_window(
    app: &Application,
    state: Arc<AppState>,
    _timers: &TimerRegistry,
    initial_sound_count: usize,
    initial_sound_page: crate::library_store::SoundPage,
) -> ApplicationWindow {
    let build_started = Instant::now();
    {
        let cfg = state.config.lock();
        apply_theme(cfg.settings.theme);
    }
    log::debug!(
        "Window build latency: phase=theme elapsed_us={}",
        build_started.elapsed().as_micros()
    );

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
    log::debug!(
        "Window build latency: phase=window elapsed_us={}",
        build_started.elapsed().as_micros()
    );

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
            // Banner titles are parsed as markup; remediation commands contain "&&".
            let banner = adw::Banner::new(&format!(
                "Global hotkeys unavailable — {}",
                glib::markup_escape_text(&reason)
            ));
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
    log::debug!(
        "Window build latency: phase=banners elapsed_us={}",
        build_started.elapsed().as_micros()
    );

    let transport = TransportBar::new(Arc::clone(&state));
    log::debug!(
        "Window build latency: phase=transport elapsed_us={}",
        build_started.elapsed().as_micros()
    );
    root_box.append(transport.widget());

    let tabs = TabsSidebar::new(Arc::clone(&state), dialog_host.clone());
    log::debug!(
        "Window build latency: phase=tabs elapsed_us={}",
        build_started.elapsed().as_micros()
    );
    let sound_list = SoundList::new(
        Arc::clone(&state),
        dialog_host.clone(),
        initial_sound_count,
        initial_sound_page,
    );
    log::debug!(
        "Window build latency: phase=sound_list elapsed_us={}",
        build_started.elapsed().as_micros()
    );
    let sidebar_paned = Paned::new(Orientation::Horizontal);
    sidebar_paned.set_vexpand(true);
    sidebar_paned.set_wide_handle(true);
    sidebar_paned.set_resize_start_child(false);
    sidebar_paned.set_resize_end_child(true);
    sidebar_paned.set_shrink_start_child(false);
    sidebar_paned.set_start_child(Some(tabs.widget()));
    sidebar_paned.set_end_child(Some(sound_list.widget()));
    sidebar_paned.set_position(220);
    sidebar_paned.connect_position_notify(|paned| {
        let position = cap_sidebar_width(paned.position());
        if position != paned.position() {
            paned.set_position(position);
        }
    });

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

    root_box.append(&sidebar_paned);

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
    drop_overlay.add_overlay(dialog_host.widget());

    // Below a narrow width the sidebar hides and the transport bar reflows.
    // The same GtkPaned keeps ownership at every size, avoiding reparenting during resize.
    let breakpoint_bin = adw::BreakpointBin::new();
    breakpoint_bin.set_size_request(520, 400);
    breakpoint_bin.set_child(Some(&drop_overlay));

    let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        960.0,
        adw::LengthUnit::Px,
    ));
    let sidebar_toggle = transport.sidebar_toggle_button().clone();
    sidebar_toggle.set_visible(false);
    {
        let tabs_widget = tabs.widget().clone();
        sidebar_toggle.connect_clicked(move |_| {
            tabs_widget.set_visible(!tabs_widget.is_visible());
        });
    }
    {
        let tabs_widget = tabs.widget().clone();
        let toggle = sidebar_toggle.clone();
        let transport = transport.clone();
        breakpoint.connect_apply(move |_| {
            tabs_widget.set_visible(false);
            toggle.set_visible(true);
            transport.set_compact(true);
        });
    }
    {
        let tabs_widget = tabs.widget().clone();
        let toggle = sidebar_toggle;
        let transport = transport.clone();
        breakpoint.connect_unapply(move |_| {
            tabs_widget.set_visible(true);
            toggle.set_visible(false);
            transport.set_compact(false);
        });
    }
    breakpoint_bin.add_breakpoint(breakpoint);

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

        let settings_overlay = Rc::new(std::cell::RefCell::new(None));
        let window = window.clone();
        let state = Arc::clone(&state);
        let dialog_host = dialog_host.clone();
        let drop_overlay = drop_overlay.clone();
        transport.connect_settings_requested(move || {
            let mut settings_overlay = settings_overlay.borrow_mut();
            let settings_overlay = settings_overlay.get_or_insert_with(|| {
                let overlay = settings::build_settings_overlay(
                    window.upcast_ref::<gtk4::Window>(),
                    Arc::clone(&state),
                    dialog_host.clone(),
                    Some(Rc::clone(&on_library_changed)),
                    Some(Rc::clone(&on_list_style_changed)),
                );
                // This panel is attached after the dialog host, so it sits above
                // it in the overlay's paint order. `DialogHost::present` raises
                // itself before showing, which is what keeps dialogs opened from
                // here on top.
                drop_overlay.add_overlay(&overlay);
                overlay
            });
            settings_overlay.set_visible(true);
            settings_overlay.grab_focus();
        });
    }

    {
        // Registered here rather than in bootstrap: resolving a press needs the
        // sidebar and the sound list, which only exist inside this function.
        let state_hk = Arc::clone(&state);
        let window_hk = window.clone();
        let transport_hk = transport.clone();
        let tabs_hk = tabs.clone();
        let sound_list_hk = sound_list.clone();
        let dialog_host_hk = dialog_host.clone();
        crate::ui_event_bridge::set_hotkey_handler(move |binding_id| {
            handle_hotkey(
                &window_hk,
                &state_hk,
                &transport_hk,
                &tabs_hk,
                &sound_list_hk,
                &dialog_host_hk,
                &binding_id,
            );
        });
    }

    {
        // Registered here rather than in bootstrap for the same reason as the
        // hotkey handler: acting on a click needs the transport and the window.
        let state_tray = Arc::clone(&state);
        let window_tray = window.clone();
        let transport_tray = transport.clone();
        crate::ui_event_bridge::set_tray_action_handler(move |action| {
            handle_tray_action(&window_tray, &state_tray, &transport_tray, action);
        });
    }

    {
        let state_mpris = Arc::clone(&state);
        let window_mpris = window.clone();
        let transport_mpris = transport.clone();
        crate::ui_event_bridge::set_mpris_command_handler(move |command| {
            handle_mpris_command(&window_mpris, &state_mpris, &transport_mpris, command);
        });
    }

    let transport_cleanup = transport.clone();
    let tabs_cleanup = tabs.clone();
    let sound_list_cleanup = sound_list.clone();
    let state_cleanup = Arc::clone(&state);
    window.connect_close_request(move |window| {
        // The first close-request handler to run, and so the only one that can
        // stop the teardown before the rest of it starts: bootstrap's handler
        // shuts the player and the hotkey backends down, which is exactly what
        // running in the background must not do.
        if crate::ui_event_bridge::close_should_hide_to_tray() {
            window.set_visible(false);
            refresh_tray_menu(&state_cleanup, false);
            return glib::Propagation::Stop;
        }
        transport_cleanup.cleanup();
        tabs_cleanup.cleanup();
        sound_list_cleanup.cleanup();
        glib::Propagation::Proceed
    });

    log::debug!(
        "Window build latency: phase=complete elapsed_us={}",
        build_started.elapsed().as_micros()
    );
    window
}

/// The settings a press is resolved against, read on the UI thread and carried
/// into the worker.
fn hotkey_press_context(state: &Arc<AppState>, sound_list: &SoundList) -> commands::HotkeyPress {
    let (tab_hotkeys, multi_sound, mode) = {
        let config = state.config.lock();
        (
            config.settings.tab_hotkeys,
            config.settings.multi_sound_hotkeys,
            config.settings.group_mode,
        )
    };
    commands::HotkeyPress {
        toggles: crate::hotkeys::HotkeyToggles {
            tab_hotkeys,
            multi_sound,
        },
        mode,
        active_scope: sound_list.active_scope_key(),
        cursor: Arc::clone(&state.hotkey_group_cursor),
    }
}

pub fn handle_hotkey(
    _window: &ApplicationWindow,
    state: &Arc<AppState>,
    transport: &TransportBar,
    tabs: &TabsSidebar,
    sound_list: &SoundList,
    dialog_host: &DialogHost,
    id: &str,
) {
    // Recording a shortcut must not also trigger it. The capture dialog reads
    // keys through its own focus, while the global backend keeps delivering
    // the same press here.
    if dialog_host.is_capturing_hotkey() {
        log::debug!("Ignoring hotkey '{id}' while a shortcut is being recorded");
        return;
    }
    if let Some(action) = crate::config::ControlHotkeyAction::from_binding_id(id) {
        handle_control_hotkey(state, transport, action);
    } else if let Some(scope_key) = commands::tab_from_binding_id(id) {
        if !tabs.activate_tab(scope_key) {
            log::warn!("Hotkey names a tab that is not in the sidebar: {scope_key}");
        }
    } else {
        let sound_id = id.to_string();
        let sound_id_for_log = sound_id.clone();
        let press = hotkey_press_context(state, sound_list);
        crate::ui_event_bridge::mark_explicit_play_pending();
        if let Err(e) =
            commands::play_hotkey_sound_async(sound_id, press, Arc::clone(state), move |result| {
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

/// Send the tray a menu that matches the state the app is in now.
///
/// `window_visible` is passed in rather than read back off the window: the
/// close handler runs before the hide takes effect, so asking the widget there
/// would report the value that is about to change.
fn refresh_tray_menu(state: &Arc<AppState>, window_visible: bool) {
    let real_mic_muted = !state.config.lock().settings.mic_passthrough;
    crate::ui_event_bridge::post_tray_menu(crate::tray::menu::build(
        window_visible,
        real_mic_muted,
    ));
}

fn handle_tray_action(
    window: &ApplicationWindow,
    state: &Arc<AppState>,
    transport: &TransportBar,
    action: crate::tray::TrayAction,
) {
    use crate::tray::{MenuAction, TrayAction};

    let action = match action {
        TrayAction::Activate => MenuAction::ToggleWindow,
        TrayAction::MenuItem(id) => match crate::tray::menu::action_for(id) {
            Some(action) => action,
            None => {
                log::debug!("Tray: ignoring a click on unknown row {id}");
                return;
            }
        },
    };

    match action {
        MenuAction::ToggleWindow => {
            let showing = window.is_visible();
            if showing {
                window.set_visible(false);
            } else {
                window.present();
            }
            refresh_tray_menu(state, !showing);
        }
        MenuAction::Control(control) => {
            handle_control_hotkey(state, transport, control);
            refresh_tray_menu(state, window.is_visible());
        }
        MenuAction::Quit => {
            // Marked first so the window's own close handler lets the close
            // through instead of hiding to the tray again.
            crate::ui_event_bridge::mark_quit_requested();
            window.close();
        }
    }
}

/// Act on a button pressed in the desktop's media controls.
///
/// Everything except Raise and Quit lands on the same dispatcher the control
/// hotkeys use, so a panel button and a shortcut always do the same thing.
fn handle_mpris_command(
    window: &ApplicationWindow,
    state: &Arc<AppState>,
    transport: &TransportBar,
    command: crate::mpris::MprisCommand,
) {
    use crate::config::ControlHotkeyAction;
    use crate::mpris::MprisCommand;

    let control = match command {
        MprisCommand::PlayPause => ControlHotkeyAction::PlayPause,
        MprisCommand::Stop => ControlHotkeyAction::StopAll,
        MprisCommand::Next => ControlHotkeyAction::NextSound,
        MprisCommand::Previous => ControlHotkeyAction::PreviousSound,
        MprisCommand::Raise => {
            window.present();
            refresh_tray_menu(state, true);
            return;
        }
        MprisCommand::Quit => {
            crate::ui_event_bridge::mark_quit_requested();
            window.close();
            return;
        }
    };
    handle_control_hotkey(state, transport, control);
}

fn handle_control_hotkey(
    state: &Arc<AppState>,
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
        crate::config::ControlHotkeyAction::CycleGroupMode => {
            match commands::cycle_group_mode(Arc::clone(&state.config)) {
                Ok(mode) => {
                    crate::ui_event_bridge::post_group_mode_changed(mode);
                    crate::ui_event_bridge::post_toast(format!(
                        "Shared hotkeys: {}",
                        match mode {
                            crate::config::GroupMode::Same => "play the same sound",
                            crate::config::GroupMode::Next => "play the next sound",
                            crate::config::GroupMode::Random => "play a random sound",
                        }
                    ));
                }
                Err(error) => log::warn!("Could not cycle the shared hotkey mode: {error}"),
            }
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
    fn native_sidebar_resize_is_capped() {
        assert_eq!(cap_sidebar_width(220), 220);
        assert_eq!(cap_sidebar_width(700), 600);
    }
}
