use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use gtk4::Window;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app_meta::{APP_TITLE, APP_VERSION};
use crate::app_state::AppState;
use crate::commands;
use crate::config::{ListStyle, Theme};

use super::dialogs::DialogHost;
use super::dnd_import::supported_audio_formats;
use super::icons;
use super::settings_folders::{
    rebuild_sound_folder_rows, schedule_rebuild_sound_folder_rows, FolderRowRefs, RebuildPending,
};
use super::settings_hotkeys;

#[cfg(test)]
fn should_poll_loudness_summary(dialog_visible: bool) -> bool {
    dialog_visible
}

fn set_appearance_row_selected(row: &adw::ActionRow, selected: bool) {
    if selected {
        row.add_css_class("appearance-choice-selected");
    } else {
        row.remove_css_class("appearance-choice-selected");
    }
}

pub fn build_settings_overlay(
    parent: &Window,
    state: Arc<AppState>,
    dialog_host: DialogHost,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
    on_list_style_changed: Option<Rc<dyn Fn(String) + 'static>>,
) -> gtk4::Overlay {
    let overlay = gtk4::Overlay::builder()
        .visible(false)
        .can_focus(true)
        .focusable(true)
        .build();
    overlay.add_css_class("lsb-settings-dialog");
    overlay.add_css_class("lsb-settings-overlay");

    let backdrop = gtk4::Button::builder()
        .can_focus(false)
        .css_classes(vec!["settings-overlay-backdrop"])
        .build();
    backdrop.set_hexpand(true);
    backdrop.set_vexpand(true);
    overlay.set_child(Some(&backdrop));

    let panel = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    panel.add_css_class("settings-overlay-panel");
    panel.set_halign(gtk4::Align::Fill);
    panel.set_valign(gtk4::Align::Center);
    panel.set_hexpand(true);

    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    header.add_css_class("settings-overlay-header");

    let stack = gtk4::Stack::builder()
        .hexpand(true)
        .vexpand(true)
        .transition_type(gtk4::StackTransitionType::None)
        .build();
    let selector = gtk4::Box::builder()
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .homogeneous(true)
        .css_classes(vec!["settings-overlay-switcher"])
        .build();
    let general_tab = build_settings_selector_button(icons::SETTINGS, "General");
    let hotkeys_tab = build_settings_selector_button(icons::KEYBOARD, "Control Hotkeys");
    hotkeys_tab.set_group(Some(&general_tab));
    selector.append(&general_tab);
    selector.append(&hotkeys_tab);

    let header_start = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    header_start.set_hexpand(true);
    let header_end = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    header_end.set_hexpand(true);
    header_end.set_halign(gtk4::Align::End);

    let close_btn = gtk4::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close settings")
        .css_classes(vec!["flat", "settings-overlay-close-btn"])
        .valign(gtk4::Align::Center)
        .build();
    header_end.append(&close_btn);
    header.append(&header_start);
    header.append(&selector);
    header.append(&header_end);
    panel.append(&header);

    let overlay_widget: gtk4::Widget = overlay.clone().upcast();
    let content = build_settings_content(
        &stack,
        Arc::clone(&state),
        parent,
        dialog_host,
        on_library_changed,
        on_list_style_changed,
        overlay_widget.downgrade(),
    );
    panel.append(&content);
    {
        let stack = stack.clone();
        general_tab.connect_toggled(move |button| {
            if button.is_active() {
                stack.set_visible_child_name("general");
            }
        });
    }
    {
        let stack = stack.clone();
        hotkeys_tab.connect_toggled(move |button| {
            if button.is_active() {
                stack.set_visible_child_name("hotkeys");
            }
        });
    }
    general_tab.set_active(true);

    // Keep the settings panel adaptive: cap its width on wide windows and let it shrink
    // with side margins on narrow ones, instead of forcing a fixed 600x700 size.
    let panel_clamp = adw::Clamp::builder()
        .maximum_size(640)
        .tightening_threshold(520)
        .hexpand(true)
        .halign(gtk4::Align::Fill)
        .valign(gtk4::Align::Center)
        .margin_start(24)
        .margin_end(24)
        .margin_top(24)
        .margin_bottom(24)
        .child(&panel)
        .build();
    overlay.add_overlay(&panel_clamp);

    {
        // The clamp fills the overlay, so a press beside the panel lands on it
        // and never reaches the backdrop button underneath.
        let overlay_dismiss = overlay.clone();
        super::dialogs::dismiss_on_press_outside(&overlay, &panel, move || {
            overlay_dismiss.set_visible(false);
        });
    }
    {
        let overlay = overlay.clone();
        backdrop.connect_clicked(move |_| {
            overlay.set_visible(false);
        });
    }
    {
        let overlay = overlay.clone();
        close_btn.connect_clicked(move |_| {
            overlay.set_visible(false);
        });
    }
    {
        let overlay_for_key = overlay.clone();
        let key = gtk4::EventControllerKey::new();
        key.set_propagation_phase(gtk4::PropagationPhase::Capture);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval.name().as_deref() == Some("Escape") {
                overlay_for_key.set_visible(false);
                return gtk4::glib::Propagation::Stop;
            }
            gtk4::glib::Propagation::Proceed
        });
        overlay.add_controller(key);
    }

    overlay
}

/// Tray icon and what the close button does.
///
/// Closing to the tray only ever happens while an icon is really showing, so
/// leaving both on costs nothing on a desktop that has no tray.
fn build_tray_group(state: Arc<AppState>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("System Tray")
        .build();

    let (tray_enabled, close_to_tray) = {
        let config = state.config.lock();
        (config.settings.tray_enabled, config.settings.close_to_tray)
    };

    let icon_row = adw::SwitchRow::builder()
        .title("Show Tray Icon")
        .subtitle("Put Linux Soundboard in the system tray")
        .active(tray_enabled)
        .build();

    let close_row = adw::SwitchRow::builder()
        .title("Close Button Minimises To Tray")
        .subtitle("Keep running in the background so global shortcuts still work")
        .active(close_to_tray)
        .sensitive(tray_enabled)
        .build();

    {
        let state = Arc::clone(&state);
        let close_row = close_row.clone();
        icon_row.connect_active_notify(move |row| {
            let enabled = row.is_active();
            if let Err(error) = commands::set_tray_enabled(enabled, Arc::clone(&state.config)) {
                log::warn!("Could not save the tray icon setting: {error}");
                return;
            }
            close_row.set_sensitive(enabled);
            crate::ui_event_bridge::post_tray_enabled(enabled);
        });
    }

    {
        let state = Arc::clone(&state);
        close_row.connect_active_notify(move |row| {
            if let Err(error) =
                commands::set_close_to_tray(row.is_active(), Arc::clone(&state.config))
            {
                log::warn!("Could not save the close-to-tray setting: {error}");
            }
        });
    }

    let media_row = adw::SwitchRow::builder()
        .title("Show In Media Controls")
        .subtitle("Show the playing sound in the panel; takes the media keys while one plays")
        .active(state.config.lock().settings.mpris_enabled)
        .build();
    {
        let state = Arc::clone(&state);
        media_row.connect_active_notify(move |row| {
            if let Err(error) =
                commands::set_mpris_enabled(row.is_active(), Arc::clone(&state.config))
            {
                log::warn!("Could not save the media controls setting: {error}");
                return;
            }
            // Runs the now-playing handler, which claims or releases the
            // player name to match the setting straight away.
            crate::ui_event_bridge::post_now_playing(None);
        });
    }

    group.add(&icon_row);
    group.add(&close_row);
    group.add(&media_row);
    group
}

fn build_settings_selector_button(icon: icons::IconPair, label: &str) -> gtk4::ToggleButton {
    let button = gtk4::ToggleButton::builder()
        .tooltip_text(label)
        .css_classes(vec!["settings-overlay-tab"])
        .build();
    let content = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    content.set_halign(gtk4::Align::Center);
    content.set_valign(gtk4::Align::Center);

    let image = icons::image(icon);
    let label = gtk4::Label::builder().label(label).build();
    content.append(&image);
    content.append(&label);
    button.set_child(Some(&content));
    button
}

fn build_settings_content(
    stack: &gtk4::Stack,
    state: Arc<AppState>,
    parent: &Window,
    dialog_host: DialogHost,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
    on_list_style_changed: Option<Rc<dyn Fn(String) + 'static>>,
    visibility_weak: gtk4::glib::WeakRef<gtk4::Widget>,
) -> gtk4::Box {
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content.add_css_class("settings-overlay-content");
    content.set_vexpand(true);

    let general_page = build_general_page(
        Arc::clone(&state),
        parent,
        on_library_changed,
        on_list_style_changed,
        visibility_weak.clone(),
    );
    let general_scroll = gtk4::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(600)
        .build();
    general_scroll.set_child(Some(&general_page));
    let general_stack_page = stack.add_titled(&general_scroll, Some("general"), "General");
    general_stack_page.set_icon_name(icons::name(icons::SETTINGS));

    let hotkeys_page = settings_hotkeys::build_hotkeys_page(Arc::clone(&state), dialog_host);
    let hotkeys_scroll = gtk4::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(600)
        .build();
    hotkeys_scroll.set_child(Some(&hotkeys_page));
    let hotkeys_stack_page = stack.add_titled(&hotkeys_scroll, Some("hotkeys"), "Control Hotkeys");
    hotkeys_stack_page.set_icon_name(icons::name(icons::KEYBOARD));

    content.append(stack);

    // Cap the scrollable content height to the available window height so the panel is
    // content-sized when it fits and scrolls (rather than overflowing) when the window
    // is short. Tracked live via a tick callback on the mapped content.
    content.add_tick_callback(move |_content, _clock| {
        if let Some(overlay) = visibility_weak.upgrade() {
            let available = overlay.height();
            if available > 0 {
                let max = (available - 160).max(220);
                if general_scroll.max_content_height() != max {
                    general_scroll.set_max_content_height(max);
                }
                if hotkeys_scroll.max_content_height() != max {
                    hotkeys_scroll.set_max_content_height(max);
                }
            }
        }
        gtk4::glib::ControlFlow::Continue
    });

    content
}

fn build_general_page(
    state: Arc<AppState>,
    parent: &Window,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
    on_list_style_changed: Option<Rc<dyn Fn(String) + 'static>>,
    visibility_weak: gtk4::glib::WeakRef<gtk4::Widget>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name(icons::name(icons::SETTINGS))
        .build();

    let folders_group = adw::PreferencesGroup::builder()
        .title("Sound Folders")
        .description("Folders scanned for audio files on startup")
        .build();

    let (hidden_folders_group, refresh_hidden_folders) =
        super::settings_folders::build_hidden_folders_group(
            Arc::clone(&state),
            on_library_changed.clone(),
        );
    // Removing a root deletes the folders under it, so anything hidden there is
    // gone too. Refreshing from the shared callback keeps the list honest
    // without threading it through the folder rebuild.
    let on_library_changed = {
        let refresh_hidden_folders = Rc::clone(&refresh_hidden_folders);
        let inner = on_library_changed.clone();
        Some(Rc::new(move || {
            refresh_hidden_folders();
            if let Some(callback) = inner.as_ref() {
                callback();
            }
        }) as Rc<dyn Fn() + 'static>)
    };

    let add_folder_row = adw::ActionRow::builder()
        .title("Add Folder…")
        .activatable(true)
        .build();
    add_folder_row.add_prefix(&icons::image(icons::ADD));

    let folder_rows: FolderRowRefs = Rc::new(RefCell::new(Vec::new()));
    let rebuild_pending: RebuildPending = Rc::new(Cell::new(false));
    // Handle for the scan that follows adding a folder, so the Stop button
    // below can cancel it. The row itself keeps adding folders throughout.
    let add_folder_cancel: Rc<RefCell<Option<Arc<std::sync::atomic::AtomicBool>>>> =
        Rc::new(RefCell::new(None));
    // The row keeps adding folders while a scan runs, so a second scan can
    // start before the first finishes. Only the newest one owns the Stop
    // button; an older run finishing must not hide it out from under the
    // newer one.
    let scan_generation: Rc<Cell<u64>> = Rc::new(Cell::new(0));
    let scan_stop_btn = gtk4::Button::builder()
        .label("Stop")
        .css_classes(vec!["settings-primary-btn"])
        .valign(gtk4::Align::Center)
        .visible(false)
        .tooltip_text("Cancel the folder scan")
        .build();
    add_folder_row.add_suffix(&scan_stop_btn);

    {
        let add_folder_cancel_stop = Rc::clone(&add_folder_cancel);
        let add_folder_row_stop = add_folder_row.downgrade();
        scan_stop_btn.connect_clicked(move |btn| {
            // Copy the handle out before touching widgets: GTK can re-enter a
            // handler, and a borrow held across a widget call aborts.
            let pending = add_folder_cancel_stop.borrow().as_ref().map(Arc::clone);
            let Some(cancelled) = pending else {
                return;
            };
            cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
            btn.set_sensitive(false);
            if let Some(row) = add_folder_row_stop.upgrade() {
                row.set_subtitle("Cancelling scan…");
            }
        });
    }

    {
        let state2 = Arc::clone(&state);
        let parent = parent.clone();
        let folders_group_weak = folders_group.downgrade();
        let add_folder_row_weak = add_folder_row.downgrade();
        let folder_rows2 = Rc::clone(&folder_rows);
        let rebuild_pending2 = Rc::clone(&rebuild_pending);
        let on_library_changed2 = on_library_changed.clone();
        let add_folder_cancel2 = Rc::clone(&add_folder_cancel);
        let scan_generation2 = Rc::clone(&scan_generation);
        let scan_stop_btn2 = scan_stop_btn.clone();
        add_folder_row.connect_activated(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Select Sound Folder")
                .build();
            let state3 = Arc::clone(&state2);
            let parent_for_dialog = parent.clone();
            let folders_group_weak2 = folders_group_weak.clone();
            let add_folder_row_weak2 = add_folder_row_weak.clone();
            let folder_rows3 = Rc::clone(&folder_rows2);
            let rebuild_pending3 = Rc::clone(&rebuild_pending2);
            let on_library_changed3 = on_library_changed2.clone();
            let add_folder_cancel3 = Rc::clone(&add_folder_cancel2);
            let scan_generation3 = Rc::clone(&scan_generation2);
            let scan_stop_btn3 = scan_stop_btn2.clone();
            dialog.select_folder(
                Some(&parent_for_dialog),
                gtk4::gio::Cancellable::NONE,
                move |result| {
                    if let Ok(folder) = result {
                        if let Some(path) = folder.path() {
                            let path_str = path.to_string_lossy().to_string();
                            log::info!("Add folder dialog result: {}", path_str);
                            let Some(add_folder_row3) = add_folder_row_weak2.upgrade() else {
                                log::warn!("add_folder_row3 weak ref failed to upgrade");
                                return;
                            };
                            add_folder_row3.set_sensitive(false);
                            let add_folder_row_done = add_folder_row3.clone();
                            if let Err(e) = commands::add_sound_folder_with_store_async(
                                path_str,
                                state3.library.clone(),
                                move |result| match result {
                                    Ok(()) => {
                                        log::info!("Add folder command succeeded");
                                        let add_folder_row_refresh = add_folder_row_done.clone();
                                        let cancel_done = Rc::clone(&add_folder_cancel3);
                                        let stop_btn_done = scan_stop_btn3.clone();
                                        // Claim the generation before
                                        // dispatching: the completion runs on
                                        // this same main loop, so it cannot
                                        // fire before this returns.
                                        let scan_id =
                                            scan_generation3.get().wrapping_add(1);
                                        scan_generation3.set(scan_id);
                                        let generation_done = Rc::clone(&scan_generation3);
                                        match commands::refresh_sounds_with_store_async(
                                            Arc::clone(&state3.config),
                                            state3.library.clone(),
                                            state3.hotkey_projection.clone(),
                                            state3.loudness_coordinators.clone(),
                                            move |result| {
                                                if generation_done.get() == scan_id {
                                                    cancel_done.borrow_mut().take();
                                                    stop_btn_done.set_visible(false);
                                                    stop_btn_done.set_sensitive(true);
                                                    add_folder_row_refresh.set_sensitive(true);
                                                    add_folder_row_refresh.set_subtitle("");
                                                }
                                                match result {
                                                    Err(e)
                                                        if e.to_string().contains("cancelled") =>
                                                    {
                                                        log::info!(
                                                            "Scan after adding folder cancelled"
                                                        );
                                                    }
                                                    Err(e) => log::warn!(
                                                        "Refresh after adding folder failed: {e}"
                                                    ),
                                                    Ok(_) => log::info!("Refresh sounds completed"),
                                                }
                                                let Some(folders_group3) =
                                                    folders_group_weak2.upgrade()
                                                else {
                                                    log::warn!(
                                                        "folders_group3 weak ref failed to upgrade"
                                                    );
                                                    return;
                                                };
                                                schedule_rebuild_sound_folder_rows(
                                                    &folders_group3,
                                                    &add_folder_row_refresh,
                                                    Arc::clone(&state3),
                                                    Rc::clone(&folder_rows3),
                                                    Rc::clone(&rebuild_pending3),
                                                    on_library_changed3.clone(),
                                                );
                                                if let Some(cb) = on_library_changed3.as_ref() {
                                                    cb();
                                                }
                                            },
                                        ) {
                                            Ok(cancelled) => {
                                                // The scan is the slow part. Show
                                                // Stop beside the row and leave
                                                // the row itself free to add
                                                // another folder.
                                                *add_folder_cancel3.borrow_mut() = Some(cancelled);
                                                add_folder_row_done
                                                    .set_subtitle("Scanning for audio files…");
                                                add_folder_row_done.set_sensitive(true);
                                                scan_stop_btn3.set_sensitive(true);
                                                scan_stop_btn3.set_visible(true);
                                            }
                                            Err(e) => {
                                                add_folder_row_done.set_sensitive(true);
                                                log::warn!(
                                                "Failed to dispatch refresh after adding folder: {e}"
                                            );
                                            }
                                        }
                                    }
                                    Err(e) => {
                                        add_folder_row_done.set_sensitive(true);
                                        log::warn!("Add folder failed: {e}");
                                    }
                                },
                            ) {
                                add_folder_row3.set_sensitive(true);
                                log::warn!("Failed to dispatch folder addition: {e}");
                            }
                        }
                    }
                },
            );
        });
    }
    rebuild_sound_folder_rows(
        &folders_group,
        &add_folder_row,
        Arc::clone(&state),
        Rc::clone(&folder_rows),
        Rc::clone(&rebuild_pending),
        on_library_changed.clone(),
    );
    page.add(&folders_group);
    // The overlay is built once and only shown and hidden afterwards, so the
    // list has to reload every time the page appears; folders removed from the
    // sidebar in between would otherwise be missing from it.
    {
        let refresh_hidden_folders = Rc::clone(&refresh_hidden_folders);
        page.connect_map(move |_| refresh_hidden_folders());
    }
    page.add(&hidden_folders_group);

    let (playback_group, auto_gain_group) =
        super::settings_playback::build_playback_groups(Arc::clone(&state), visibility_weak);
    page.add(&playback_group);
    page.add(&auto_gain_group);

    let mic_group = super::settings_mic::build_mic_group(Arc::clone(&state));
    page.add(&mic_group);

    page.add(&build_tray_group(Arc::clone(&state)));

    let theme_group = adw::PreferencesGroup::builder().title("Appearance").build();

    {
        let current_theme = {
            let cfg = state.config.lock();
            cfg.settings.theme
        };

        let dark_colors = ["#222831", "#393E46", "#948979", "#DFD0B8"];
        let light_colors = ["#f7f4ef", "#fffdfb", "#A88D52", "#332B1F"];

        let dark_row = adw::ActionRow::builder()
            .title("Dark")
            .subtitle("Warm beige-grey palette")
            .activatable(true)
            .build();
        dark_row.add_css_class("appearance-choice-row");
        set_appearance_row_selected(&dark_row, current_theme == Theme::Dark);

        let dark_swatches = gtk4::Box::new(gtk4::Orientation::Horizontal, 3);
        dark_swatches.set_valign(gtk4::Align::Center);
        for color in &dark_colors {
            let da = gtk4::DrawingArea::builder()
                .width_request(16)
                .height_request(16)
                .css_classes(vec!["theme-swatch"])
                .build();
            let rgba = gtk4::gdk::RGBA::parse(*color)
                .expect("hardcoded dark swatch color failed to parse");
            da.set_draw_func(move |_, cr, w, h| {
                cr.set_source_rgba(
                    rgba.red() as f64,
                    rgba.green() as f64,
                    rgba.blue() as f64,
                    1.0,
                );
                cr.arc(
                    w as f64 / 2.0,
                    h as f64 / 2.0,
                    (w.min(h) as f64 / 2.0) - 1.0,
                    0.0,
                    2.0 * std::f64::consts::PI,
                );
                let _ = cr.fill();
            });
            dark_swatches.append(&da);
        }
        dark_row.add_suffix(&dark_swatches);

        let light_row = adw::ActionRow::builder()
            .title("Light")
            .subtitle("Warm gold-cream palette")
            .activatable(true)
            .build();
        light_row.add_css_class("appearance-choice-row");
        set_appearance_row_selected(&light_row, current_theme == Theme::Light);

        let light_swatches = gtk4::Box::new(gtk4::Orientation::Horizontal, 3);
        light_swatches.set_valign(gtk4::Align::Center);
        for color in &light_colors {
            let da = gtk4::DrawingArea::builder()
                .width_request(16)
                .height_request(16)
                .css_classes(vec!["theme-swatch"])
                .build();
            let rgba = gtk4::gdk::RGBA::parse(*color)
                .expect("hardcoded light swatch color failed to parse");
            da.set_draw_func(move |_, cr, w, h| {
                cr.set_source_rgba(
                    rgba.red() as f64,
                    rgba.green() as f64,
                    rgba.blue() as f64,
                    1.0,
                );
                cr.arc(
                    w as f64 / 2.0,
                    h as f64 / 2.0,
                    (w.min(h) as f64 / 2.0) - 1.0,
                    0.0,
                    2.0 * std::f64::consts::PI,
                );
                let _ = cr.fill();
            });
            light_swatches.append(&da);
        }
        light_row.add_suffix(&light_swatches);

        {
            let state2 = Arc::clone(&state);
            let dr = dark_row.downgrade();
            let lr = light_row.downgrade();
            dark_row.connect_activated(move |_| {
                let _ = commands::set_theme("dark".to_string(), Arc::clone(&state2.config));
                crate::ui::theme::apply_theme(Theme::Dark);
                if let Some(dr) = dr.upgrade() {
                    set_appearance_row_selected(&dr, true);
                }
                if let Some(lr) = lr.upgrade() {
                    set_appearance_row_selected(&lr, false);
                }
            });
        }

        {
            let state2 = Arc::clone(&state);
            let dr = dark_row.downgrade();
            let lr = light_row.downgrade();
            light_row.connect_activated(move |_| {
                let _ = commands::set_theme("light".to_string(), Arc::clone(&state2.config));
                crate::ui::theme::apply_theme(Theme::Light);
                if let Some(dr) = dr.upgrade() {
                    set_appearance_row_selected(&dr, false);
                }
                if let Some(lr) = lr.upgrade() {
                    set_appearance_row_selected(&lr, true);
                }
            });
        }

        theme_group.add(&dark_row);
        theme_group.add(&light_row);
    }

    {
        let current_style = {
            let cfg = state.config.lock();
            cfg.settings.list_style
        };

        let compact_row = adw::ActionRow::builder()
            .title("Compact")
            .subtitle("Dense list, more sounds visible")
            .activatable(true)
            .build();
        compact_row.add_css_class("appearance-choice-row");
        set_appearance_row_selected(&compact_row, current_style == ListStyle::Compact);

        let card_row = adw::ActionRow::builder()
            .title("Card")
            .subtitle("Balanced layout with about 1.6x the space of compact")
            .activatable(true)
            .build();
        card_row.add_css_class("appearance-choice-row");
        set_appearance_row_selected(&card_row, current_style == ListStyle::Card);

        {
            let state2 = Arc::clone(&state);
            let cr = compact_row.downgrade();
            let cdr = card_row.downgrade();
            let on_list_style_changed_compact = on_list_style_changed.clone();
            compact_row.connect_activated(move |_| {
                let _ = commands::set_list_style(
                    ListStyle::Compact.as_str().to_string(),
                    Arc::clone(&state2.config),
                );
                if let Some(cr) = cr.upgrade() {
                    set_appearance_row_selected(&cr, true);
                }
                if let Some(cdr) = cdr.upgrade() {
                    set_appearance_row_selected(&cdr, false);
                }
                if let Some(cb) = on_list_style_changed_compact.as_ref() {
                    cb(ListStyle::Compact.as_str().to_string());
                }
            });
        }

        {
            let state2 = Arc::clone(&state);
            let cr = compact_row.downgrade();
            let cdr = card_row.downgrade();
            let on_list_style_changed_card = on_list_style_changed.clone();
            card_row.connect_activated(move |_| {
                let _ = commands::set_list_style(
                    ListStyle::Card.as_str().to_string(),
                    Arc::clone(&state2.config),
                );
                if let Some(cr) = cr.upgrade() {
                    set_appearance_row_selected(&cr, false);
                }
                if let Some(cdr) = cdr.upgrade() {
                    set_appearance_row_selected(&cdr, true);
                }
                if let Some(cb) = on_list_style_changed_card.as_ref() {
                    cb(ListStyle::Card.as_str().to_string());
                }
            });
        }

        theme_group.add(&compact_row);
        theme_group.add(&card_row);
    }
    page.add(&theme_group);

    let about_group = adw::PreferencesGroup::builder().title("About").build();
    about_group.add(
        &adw::ActionRow::builder()
            .title(APP_TITLE)
            .subtitle(format!("Version {APP_VERSION}"))
            .build(),
    );
    about_group.add(
        &adw::ActionRow::builder()
            .title("Supported Audio Formats")
            .subtitle(supported_audio_formats())
            .build(),
    );
    page.add(&about_group);

    page
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loudness_poll_pauses_when_dialog_hidden() {
        assert!(should_poll_loudness_summary(true));
        assert!(!should_poll_loudness_summary(false));
    }

    #[test]
    fn about_uses_the_supported_audio_format_list() {
        assert_eq!(
            crate::ui::dnd_import::supported_audio_formats(),
            "WAV, MP3, OGG, OPUS, FLAC, M4A, AAC, MP4"
        );
    }
}
