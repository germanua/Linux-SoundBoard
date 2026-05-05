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
use super::sound_list::SoundList;
use super::tabs_sidebar::TabsSidebar;
use super::theme::apply_theme;
use super::transport::TransportBar;

pub fn build_window(
    app: &Application,
    state: Arc<AppState>,
    timers: &TimerRegistry,
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
        let (toast_tx, toast_rx) = std::sync::mpsc::channel::<String>();
        let toast_tx_tabs = toast_tx.clone();
        transport.set_toast_sender(toast_tx);
        tabs.set_toast_sender(toast_tx_tabs);
        let toast_poll = toast_overlay.clone();
        let source_id = glib::timeout_add_local(std::time::Duration::from_millis(100), move || {
            while let Ok(msg) = toast_rx.try_recv() {
                show_toast(&toast_poll, &msg);
            }
            glib::ControlFlow::Continue
        });
        timers.register(source_id);
    }

    {
        let sl_playing = sound_list.clone();
        let state_playing = Arc::clone(&state);
        let source_id = glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
            let positions = commands::get_playback_positions(Arc::clone(&state_playing.player));
            let active: Vec<&crate::audio::player::PlaybackPosition> =
                positions.iter().filter(|p| !p.finished).collect();
            let ids: std::collections::HashSet<String> =
                active.iter().map(|p| p.sound_id.clone()).collect();
            let active_id = active.first().map(|p| p.sound_id.clone());
            sl_playing.set_playing_ids(ids);
            sl_playing.set_active_sound_id(active_id);
            glib::ControlFlow::Continue
        });
        timers.register(source_id);
    }

    // `AdwApplicationWindow` does not expose `set_content`; use `set_child` here.
    let drop_overlay =
        dnd_import::build_and_attach_drop_overlay(&window, &toast_overlay, &sound_list, &state);
    window.set_child(Some(&drop_overlay));

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
        if let Err(e) = commands::play_sound_async(
            sound_id,
            Arc::clone(&state.config),
            Arc::clone(&state.player),
            move |result| {
                if let Err(err) = result {
                    log::warn!("Hotkey playback failed for '{}': {}", sound_id_for_log, err);
                }
            },
        ) {
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
