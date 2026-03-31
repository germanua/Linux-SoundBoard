//! Main application window — layout scaffold and hotkey dispatch.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use glib;
use gtk4::prelude::*;
use gtk4::{Application, ApplicationWindow, Box as GtkBox, Orientation, Paned};
use libadwaita as adw;

use crate::app_meta::{APP_ICON_NAME, APP_TITLE, GENERAL_TAB_ID};
use crate::app_state::AppState;
use crate::audio::scanner;
use crate::commands;

use super::icons;
use super::sound_list::SoundList;
use super::tabs_sidebar::TabsSidebar;
use super::theme::apply_theme;
use super::transport::TransportBar;

#[derive(Clone, Copy)]
enum DropState {
    Hidden,
    Ready,
    Importing,
    Rejected,
}

/// Build and return the main application window and transport bar.
pub fn build_window(app: &Application, state: Arc<AppState>) -> (ApplicationWindow, TransportBar) {
    // Apply theme from config
    {
        let cfg = state.config.lock().unwrap();
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

    // Root vertical box: [setup_banner?] + [transport] + [content_pane]
    let root_box = GtkBox::new(Orientation::Vertical, 0);

    // ── Setup banner (shown only if PipeWire unavailable) ─────────────
    {
        let pw = state.pipewire_status.lock().unwrap();
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
            let hotkeys = state.hotkeys.lock().unwrap();
            hotkeys.availability_message()
        };
        if let Some(reason) = hotkey_message {
            let banner = adw::Banner::new(&format!("Global hotkeys unavailable — {}", reason));
            banner.set_button_label(Some("Dismiss"));
            banner.set_revealed(true);
            banner.connect_button_clicked(|b| b.set_revealed(false));
            root_box.append(&banner);
        }
    }

    // ── Transport bar ─────────────────────────────────────────────────
    let transport = TransportBar::new(Arc::clone(&state));
    root_box.append(transport.widget());

    // ── Content pane: tabs sidebar + sound list ───────────────────────
    let paned = Paned::new(Orientation::Horizontal);
    paned.set_vexpand(true);

    let tabs = TabsSidebar::new(Arc::clone(&state));
    paned.set_start_child(Some(tabs.widget()));
    paned.set_position(220);
    paned.set_shrink_start_child(false);
    paned.set_resize_start_child(false);

    let sound_list = SoundList::new(Arc::clone(&state));

    // Connect tab change → refresh sound list filter
    {
        let sl = sound_list.clone();
        tabs.connect_tab_selected(move |tab_id| {
            sl.set_active_tab(tab_id);
        });
    }

    // Refresh list after tab membership drag/drop operations from the sidebar.
    {
        let sl = sound_list.clone();
        tabs.connect_tab_membership_changed(move || {
            sl.refresh_from_state();
        });
    }

    // Connect search entry → filter sound list by name
    {
        let sl_search = sound_list.clone();
        transport.connect_search_changed(move |query| {
            sl_search.set_search_filter(query);
        });
    }

    // Connect sound list provider for prev/next/continue navigation
    {
        let sl_nav = sound_list.clone();
        transport.set_sound_list_provider(move || sl_nav.get_navigation_sounds());
    }

    // Event-driven sidebar count refresh when library membership changes.
    {
        let tabs_counts = tabs.clone();
        sound_list.connect_library_changed(move || {
            tabs_counts.reload_tabs();
        });
    }

    // Event-driven full library refresh after transport/settings-triggered changes.
    {
        let tabs_sync = tabs.clone();
        let sl_sync = sound_list.clone();
        transport.connect_library_changed(move || {
            sl_sync.refresh_from_state();
            tabs_sync.reload_tabs();
        });
    }

    // Event-driven list style switch from settings.
    {
        let sl_style = sound_list.clone();
        transport.connect_list_style_changed(move |style| {
            sl_style.set_list_style(&style);
        });
    }

    paned.set_end_child(Some(sound_list.widget()));
    root_box.append(&paned);

    // Wrap root_box in a ToastOverlay for notifications
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&root_box));

    // Connect toast notifications via mpsc channel (ToastOverlay isn't Send)
    let toast_timer_id = {
        let (toast_tx, toast_rx) = std::sync::mpsc::channel::<String>();
        let toast_tx_tabs = toast_tx.clone();
        transport.set_toast_sender(toast_tx);
        tabs.set_toast_sender(toast_tx_tabs);
        let toast_poll = toast_overlay.clone();
        Rc::new(RefCell::new(Some(glib::timeout_add_local(
            std::time::Duration::from_millis(100),
            move || {
                while let Ok(msg) = toast_rx.try_recv() {
                    show_toast(&toast_poll, &msg);
                }
                glib::ControlFlow::Continue
            },
        ))))
    };

    // ── 150ms poll: update sound list playing indicators ──────────
    let playing_timer_id = {
        let sl_playing = sound_list.clone();
        let state_playing = Arc::clone(&state);
        Rc::new(RefCell::new(Some(glib::timeout_add_local(
            std::time::Duration::from_millis(150),
            move || {
                let positions = commands::get_playback_positions(Arc::clone(&state_playing.player));
                let active: Vec<&crate::audio::player::PlaybackPosition> =
                    positions.iter().filter(|p| !p.finished).collect();
                let ids: std::collections::HashSet<String> =
                    active.iter().map(|p| p.sound_id.clone()).collect();
                let active_id = active.first().map(|p| p.sound_id.clone());
                sl_playing.set_playing_ids(ids);
                sl_playing.set_active_sound_id(active_id);
                glib::ControlFlow::Continue
            },
        ))))
    };

    // set_content requires AdwApplicationWindow; use gtk4::Window::set_child instead
    // (set after drop overlay is created below)

    // ── Drag-and-drop for audio files (accepts gdk::FileList and text/uri-list) ──────────────
    let drop_target_files = gtk4::DropTarget::new(
        gtk4::gdk::FileList::static_type(),
        gtk4::gdk::DragAction::COPY,
    );
    let drop_target_text = gtk4::DropTarget::new(glib::Type::STRING, gtk4::gdk::DragAction::COPY);

    // Drop zone overlay — shown while dragging files over the window
    let drop_overlay = gtk4::Overlay::new();
    drop_overlay.set_child(Some(&toast_overlay));

    let drop_zone = GtkBox::new(Orientation::Vertical, 8);
    drop_zone.set_halign(gtk4::Align::Center);
    drop_zone.set_valign(gtk4::Align::Center);
    drop_zone.add_css_class("drop-zone-overlay");
    drop_zone.set_size_request(360, 180);

    let drop_icon = icons::image(icons::DROP_ZONE);
    drop_icon.add_css_class("drop-zone-icon");
    let drop_title = gtk4::Label::builder()
        .label("")
        .css_classes(vec!["title-1"])
        .build();
    let drop_subtitle = gtk4::Label::builder()
        .label("")
        .css_classes(vec!["dim-label"])
        .build();
    drop_zone.append(&drop_icon);
    drop_zone.append(&drop_title);
    drop_zone.append(&drop_subtitle);
    drop_overlay.add_overlay(&drop_zone);
    set_drop_state(&drop_zone, &drop_title, &drop_subtitle, DropState::Hidden);

    // Track hover state across both drop targets so the overlay reliably hides.
    let drag_hover_count = Arc::new(AtomicUsize::new(0));

    {
        let dz = drop_zone.clone();
        let title = drop_title.clone();
        let subtitle = drop_subtitle.clone();
        let hover = Arc::clone(&drag_hover_count);
        drop_target_files.connect_enter(move |_, _, _| {
            hover.fetch_add(1, Ordering::SeqCst);
            set_drop_state(&dz, &title, &subtitle, DropState::Ready);
            gtk4::gdk::DragAction::COPY
        });
    }
    {
        let dz = drop_zone.clone();
        let title = drop_title.clone();
        let subtitle = drop_subtitle.clone();
        let hover = Arc::clone(&drag_hover_count);
        drop_target_text.connect_enter(move |_, _, _| {
            hover.fetch_add(1, Ordering::SeqCst);
            set_drop_state(&dz, &title, &subtitle, DropState::Ready);
            gtk4::gdk::DragAction::COPY
        });
    }
    {
        let dz = drop_zone.clone();
        let title = drop_title.clone();
        let subtitle = drop_subtitle.clone();
        let hover = Arc::clone(&drag_hover_count);
        drop_target_files.connect_leave(move |_| {
            let previous = hover.fetch_sub(1, Ordering::SeqCst);
            if previous <= 1 {
                hover.store(0, Ordering::SeqCst);
                set_drop_state(&dz, &title, &subtitle, DropState::Hidden);
            }
        });
    }
    {
        let dz = drop_zone.clone();
        let title = drop_title.clone();
        let subtitle = drop_subtitle.clone();
        let hover = Arc::clone(&drag_hover_count);
        drop_target_text.connect_leave(move |_| {
            let previous = hover.fetch_sub(1, Ordering::SeqCst);
            if previous <= 1 {
                hover.store(0, Ordering::SeqCst);
                set_drop_state(&dz, &title, &subtitle, DropState::Hidden);
            }
        });
    }

    let state_drop_files = Arc::clone(&state);
    let sl_drop_files = sound_list.clone();
    let toast_drop_files = toast_overlay.clone();
    let dz_drop_files = drop_zone.clone();
    let title_drop_files = drop_title.clone();
    let subtitle_drop_files = drop_subtitle.clone();
    let hover_drop_files = Arc::clone(&drag_hover_count);
    drop_target_files.connect_drop(move |_, value, _, _| {
        hover_drop_files.store(0, Ordering::SeqCst);
        set_drop_state(
            &dz_drop_files,
            &title_drop_files,
            &subtitle_drop_files,
            DropState::Importing,
        );

        let Ok(file_list) = value.get::<gtk4::gdk::FileList>() else {
            set_drop_state(
                &dz_drop_files,
                &title_drop_files,
                &subtitle_drop_files,
                DropState::Hidden,
            );
            return false;
        };

        let dropped_paths = file_list
            .files()
            .into_iter()
            .filter_map(|file| file.path().map(|path| path.to_string_lossy().to_string()))
            .collect::<Vec<_>>();

        handle_drop_import(
            &state_drop_files,
            &sl_drop_files,
            &toast_drop_files,
            &dz_drop_files,
            &title_drop_files,
            &subtitle_drop_files,
            dropped_paths,
        )
    });

    let state_drop_text = Arc::clone(&state);
    let sl_drop_text = sound_list.clone();
    let toast_drop_text = toast_overlay.clone();
    let dz_drop_text = drop_zone.clone();
    let title_drop_text = drop_title.clone();
    let subtitle_drop_text = drop_subtitle.clone();
    let hover_drop_text = Arc::clone(&drag_hover_count);
    drop_target_text.connect_drop(move |_, value, _, _| {
        hover_drop_text.store(0, Ordering::SeqCst);
        set_drop_state(
            &dz_drop_text,
            &title_drop_text,
            &subtitle_drop_text,
            DropState::Importing,
        );

        let Ok(uri_list) = value.get::<String>() else {
            set_drop_state(
                &dz_drop_text,
                &title_drop_text,
                &subtitle_drop_text,
                DropState::Hidden,
            );
            return false;
        };

        let dropped_paths = parse_dropped_paths(&uri_list);
        handle_drop_import(
            &state_drop_text,
            &sl_drop_text,
            &toast_drop_text,
            &dz_drop_text,
            &title_drop_text,
            &subtitle_drop_text,
            dropped_paths,
        )
    });

    window.add_controller(drop_target_files);
    window.add_controller(drop_target_text);
    window.set_child(Some(&drop_overlay));

    // ── Window close handler: cancel timers to prevent leaks ──────────
    let transport_cleanup = transport.clone();
    let tabs_cleanup = tabs.clone();
    let sound_list_cleanup = sound_list.clone();
    let toast_timer_close = Rc::clone(&toast_timer_id);
    let playing_timer_close = Rc::clone(&playing_timer_id);
    window.connect_close_request(move |_| {
        transport_cleanup.cleanup();
        tabs_cleanup.cleanup();
        sound_list_cleanup.cleanup();
        if let Some(source_id) = toast_timer_close.borrow_mut().take() {
            source_id.remove();
        }
        if let Some(source_id) = playing_timer_close.borrow_mut().take() {
            source_id.remove();
        }
        glib::Propagation::Proceed
    });

    // ── Startup: record loudness state without starting backfill ─────
    {
        let state_loudness = Arc::clone(&state);
        glib::idle_add_local_once(move || {
            crate::diagnostics::memory::log_memory_snapshot("startup:loudness_bg:check");
            let phase_recorded = state_loudness
                .config
                .lock()
                .map(|cfg| {
                    let missing_count = cfg
                        .sounds
                        .iter()
                        .filter(|sound| sound.loudness_lufs.is_none())
                        .count();
                    if cfg.settings.auto_gain && missing_count > 0 {
                        log::info!(
                            "Skipping automatic startup loudness backfill for {} sounds to keep initial idle memory stable",
                            missing_count
                        );
                    }
                    crate::diagnostics::record_phase_with_config("startup:loudness_check", &cfg);
                })
                .is_ok();
            if !phase_recorded {
                crate::diagnostics::record_phase("startup:loudness_check", None);
            }
        });
    }

    (window, transport)
}

fn handle_drop_import(
    state: &Arc<AppState>,
    sound_list: &SoundList,
    toast_overlay: &adw::ToastOverlay,
    drop_zone: &GtkBox,
    title: &gtk4::Label,
    subtitle: &gtk4::Label,
    dropped_paths: Vec<String>,
) -> bool {
    if dropped_paths.is_empty() {
        set_drop_state(drop_zone, title, subtitle, DropState::Rejected);
        show_toast(toast_overlay, "Drop supported audio files to add them");
        hide_drop_zone_later(drop_zone, title, subtitle);
        return true;
    }

    let mut valid_paths = Vec::new();
    let mut skipped_paths = 0usize;
    for path in dropped_paths {
        if scanner::is_audio_file(&path) {
            valid_paths.push(path);
        } else {
            skipped_paths += 1;
        }
    }

    if valid_paths.is_empty() {
        set_drop_state(drop_zone, title, subtitle, DropState::Rejected);
        show_toast(toast_overlay, "No supported audio files were dropped");
        hide_drop_zone_later(drop_zone, title, subtitle);
        return true;
    }

    let active_tab = sound_list.active_tab_id();
    let tab_id_opt = if active_tab == GENERAL_TAB_ID {
        None
    } else {
        Some(active_tab)
    };

    match commands::import_files_to_tab(valid_paths, tab_id_opt, Arc::clone(&state.config)) {
        Ok(new_sounds) => {
            let imported_count = new_sounds.len();
            if imported_count > 0 {
                sound_list.append_sounds(new_sounds);
            }
            let message = match (imported_count, skipped_paths) {
                (0, 0) => "Dropped files were already in the soundboard".to_string(),
                (0, skipped) => format!("Skipped {skipped} unsupported files"),
                (count, 0) => format!("Added {count} sounds"),
                (count, skipped) => format!("Added {count} sounds, skipped {skipped} files"),
            };
            show_toast(toast_overlay, &message);
        }
        Err(e) => {
            log::warn!("Drop import failed: {e}");
            show_toast(toast_overlay, "Failed to import dropped files");
        }
    }

    set_drop_state(drop_zone, title, subtitle, DropState::Hidden);
    true
}

/// Dispatch a fired hotkey ID to the appropriate action.
pub fn handle_hotkey(
    _window: &ApplicationWindow,
    state: &Arc<AppState>,
    transport: &TransportBar,
    id: &str,
) {
    if let Some(action) = crate::config::ControlHotkeyAction::from_binding_id(id) {
        handle_control_hotkey(state, transport, action);
    } else {
        // Sound hotkey — play the sound
        if let Err(e) = commands::play_sound(
            id.to_string(),
            Arc::clone(&state.config),
            Arc::clone(&state.player),
        ) {
            log::warn!("Hotkey playback failed for '{}': {}", id, e);
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
        }
        crate::config::ControlHotkeyAction::MuteRealMic => {
            transport.toggle_mic_mute();
        }
        crate::config::ControlHotkeyAction::CyclePlayMode => {
            transport.cycle_play_mode();
        }
    }
}

/// Display a brief toast notification in the overlay.
pub fn show_toast(overlay: &adw::ToastOverlay, message: &str) {
    let toast = adw::Toast::new(message);
    toast.set_timeout(2);
    overlay.add_toast(toast);
}

fn set_drop_state(
    drop_zone: &GtkBox,
    title: &gtk4::Label,
    subtitle: &gtk4::Label,
    state: DropState,
) {
    drop_zone.remove_css_class("drop-zone-ready");
    drop_zone.remove_css_class("drop-zone-importing");
    drop_zone.remove_css_class("drop-zone-rejected");

    match state {
        DropState::Hidden => {
            drop_zone.set_visible(false);
            title.set_label("");
            subtitle.set_label("");
        }
        DropState::Ready => {
            drop_zone.set_visible(true);
            drop_zone.add_css_class("drop-zone-ready");
            title.set_label("Drop Audio Files Here");
            subtitle.set_label("Supported: MP3, OGG, FLAC, M4A, AAC");
        }
        DropState::Importing => {
            drop_zone.set_visible(true);
            drop_zone.add_css_class("drop-zone-importing");
            title.set_label("Importing Files…");
            subtitle.set_label("Checking formats and adding sounds");
        }
        DropState::Rejected => {
            drop_zone.set_visible(true);
            drop_zone.add_css_class("drop-zone-rejected");
            title.set_label("Unsupported Drop");
            subtitle.set_label("Drop audio files in MP3, OGG, FLAC, M4A, or AAC format");
        }
    }
}

fn hide_drop_zone_later(drop_zone: &GtkBox, title: &gtk4::Label, subtitle: &gtk4::Label) {
    let drop_zone = drop_zone.clone();
    let title = title.clone();
    let subtitle = subtitle.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
        set_drop_state(&drop_zone, &title, &subtitle, DropState::Hidden);
    });
}

fn parse_dropped_paths(uri_list: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut paths = Vec::new();

    for line in uri_list.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let path = if line.starts_with("file://") {
            gio::File::for_uri(line)
                .path()
                .map(|path| path.to_string_lossy().to_string())
        } else {
            Some(line.to_string())
        };

        if let Some(path) = path {
            if seen.insert(path.clone()) {
                paths.push(path);
            }
        }
    }

    paths
}
