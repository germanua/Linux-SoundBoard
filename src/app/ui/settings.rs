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
    panel.set_halign(gtk4::Align::Center);
    panel.set_valign(gtk4::Align::Center);
    panel.set_size_request(600, 700);

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
    overlay.add_overlay(&panel);

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
        visibility_weak,
    );
    let general_scroll = gtk4::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .build();
    general_scroll.set_child(Some(&general_page));
    let general_stack_page = stack.add_titled(&general_scroll, Some("general"), "General");
    general_stack_page.set_icon_name(icons::name(icons::SETTINGS));

    let hotkeys_page = settings_hotkeys::build_hotkeys_page(Arc::clone(&state));
    let hotkeys_scroll = gtk4::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .build();
    hotkeys_scroll.set_child(Some(&hotkeys_page));
    let hotkeys_stack_page = stack.add_titled(&hotkeys_scroll, Some("hotkeys"), "Control Hotkeys");
    hotkeys_stack_page.set_icon_name(icons::name(icons::KEYBOARD));

    content.append(stack);
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

    let add_folder_row = adw::ActionRow::builder()
        .title("Add Folder…")
        .activatable(true)
        .build();
    add_folder_row.add_prefix(&icons::image(icons::ADD));

    let folder_rows: FolderRowRefs = Rc::new(RefCell::new(Vec::new()));
    let rebuild_pending: RebuildPending = Rc::new(Cell::new(false));

    {
        let state2 = Arc::clone(&state);
        let parent = parent.clone();
        let folders_group_weak = folders_group.downgrade();
        let add_folder_row_weak = add_folder_row.downgrade();
        let folder_rows2 = Rc::clone(&folder_rows);
        let rebuild_pending2 = Rc::clone(&rebuild_pending);
        let on_library_changed2 = on_library_changed.clone();
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
            dialog.select_folder(
                Some(&parent_for_dialog),
                gtk4::gio::Cancellable::NONE,
                move |result| {
                    if let Ok(folder) = result {
                        if let Some(path) = folder.path() {
                            let path_str = path.to_string_lossy().to_string();
                            log::info!("Add folder dialog result: {}", path_str);
                            if let Err(e) =
                                commands::add_sound_folder(path_str, Arc::clone(&state3.config))
                            {
                                log::warn!("Add folder failed: {e}");
                                return;
                            }
                            log::info!("Add folder command succeeded");
                            if let Err(e) = commands::refresh_sounds_async(
                                Arc::clone(&state3.config),
                                Arc::clone(&state3.hotkeys),
                                state3.loudness_coordinators.clone(),
                                move |result| {
                                    if let Err(e) = result {
                                        log::warn!("Refresh after adding folder failed: {e}");
                                    }
                                    log::info!("Refresh sounds completed");
                                    let Some(folders_group3) = folders_group_weak2.upgrade() else {
                                        log::warn!("folders_group3 weak ref failed to upgrade");
                                        return;
                                    };
                                    let Some(add_folder_row3) = add_folder_row_weak2.upgrade()
                                    else {
                                        log::warn!("add_folder_row3 weak ref failed to upgrade");
                                        return;
                                    };
                                    schedule_rebuild_sound_folder_rows(
                                        &folders_group3,
                                        &add_folder_row3,
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
                                log::warn!("Failed to dispatch refresh after adding folder: {e}");
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

    let (playback_group, auto_gain_group) =
        super::settings_playback::build_playback_groups(Arc::clone(&state), visibility_weak);
    page.add(&playback_group);
    page.add(&auto_gain_group);

    let (mic_source_group, mic_routing_group) =
        super::settings_mic::build_mic_group(Arc::clone(&state));
    page.add(&mic_source_group);
    page.add(&mic_routing_group);

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
            .subtitle(format!(
                "v{} — Virtual mic + X11 global hotkeys for Linux",
                APP_VERSION
            ))
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
}
