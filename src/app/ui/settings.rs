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
use crate::config::{
    AutoGainApplyTo, AutoGainMode, ControlHotkeyAction, DefaultSourceMode, ListStyle,
    MicLatencyProfile, Theme,
};

use super::icons;

type FolderRowRefs = Rc<RefCell<Vec<gtk4::glib::WeakRef<adw::ActionRow>>>>;
type RebuildPending = Rc<Cell<bool>>;

#[cfg(test)]
fn should_poll_loudness_summary(dialog_visible: bool) -> bool {
    dialog_visible
}

fn try_set_rebuild_pending(rebuild_pending: &Cell<bool>) -> bool {
    if rebuild_pending.get() {
        return false;
    }
    rebuild_pending.set(true);
    true
}

fn clear_rebuild_pending(rebuild_pending: &Cell<bool>) {
    rebuild_pending.set(false);
}

fn should_attach_add_folder_row(has_parent: bool) -> bool {
    !has_parent
}

fn set_appearance_row_selected(row: &adw::ActionRow, selected: bool) {
    if selected {
        row.add_css_class("appearance-choice-selected");
    } else {
        row.remove_css_class("appearance-choice-selected");
    }
}

fn format_loudness_status_subtitle(status: &commands::LoudnessStatusSummary) -> String {
    format!(
        "Pending {} | Estimated {} | Refined {} | Unavailable {}",
        status.pending_count,
        status.estimated_count,
        status.refined_count,
        status.unavailable_count
    )
}

fn loudness_activity_text(status: &commands::LoudnessStatusSummary) -> &'static str {
    if status.in_flight_backfill && status.in_flight_refinement {
        "Analyzing + Refining"
    } else if status.in_flight_backfill {
        "Analyzing"
    } else if status.in_flight_refinement {
        "Refining"
    } else if status.estimated_count > 0 {
        "Idle (Refine Available)"
    } else {
        "Idle"
    }
}

fn set_spinner_running(spinner: &gtk4::Spinner, running: bool) {
    if running {
        spinner.set_visible(true);
        spinner.start();
    } else {
        spinner.stop();
        spinner.set_visible(false);
    }
}

fn apply_loudness_status_summary(
    summary: &commands::LoudnessStatusSummary,
    status_row: &adw::ActionRow,
    status_badge: &gtk4::Label,
    analyze_btn: &gtk4::Button,
    analyze_spinner: &gtk4::Spinner,
    refine_btn: &gtk4::Button,
    refine_spinner: &gtk4::Spinner,
) {
    status_row.set_subtitle(&format_loudness_status_subtitle(summary));
    status_badge.set_text(loudness_activity_text(summary));
    for class_name in ["hotkey-badge", "dim-label", "warning-label"] {
        status_badge.remove_css_class(class_name);
    }
    if summary.in_flight_backfill || summary.in_flight_refinement {
        status_badge.add_css_class("hotkey-badge");
    } else if summary.unavailable_count > 0 {
        status_badge.add_css_class("warning-label");
    } else {
        status_badge.add_css_class("dim-label");
    }

    if summary.in_flight_backfill || summary.in_flight_refinement {
        analyze_btn.set_label("Stop");
        analyze_btn.set_sensitive(true);
    } else {
        analyze_btn.set_label("Analyze");
        analyze_btn.set_sensitive(true);
    }
    set_spinner_running(analyze_spinner, summary.in_flight_backfill);

    if summary.in_flight_backfill || summary.in_flight_refinement {
        refine_btn.set_label("Stop");
        refine_btn.set_sensitive(true);
    } else {
        refine_btn.set_label("Refine");
        refine_btn.set_sensitive(summary.estimated_count > 0);
    }
    set_spinner_running(refine_spinner, summary.in_flight_refinement);
}

fn mic_latency_profile_subtitle(profile: MicLatencyProfile) -> &'static str {
    match profile {
        MicLatencyProfile::Balanced => "Stable default for most systems",
        MicLatencyProfile::Low => "Lower queueing delay with minimal extra CPU",
        MicLatencyProfile::Ultra => {
            "Lowest queue delay (may auto-fallback to Low if underruns occur)"
        }
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

    let hotkeys_page = build_hotkeys_page(Arc::clone(&state));
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

    let playback_group = adw::PreferencesGroup::builder().title("Playback").build();

    let auto_gain_group = adw::PreferencesGroup::builder()
        .title("Auto-Gain Normalization")
        .description("Fine-tune loudness normalization")
        .build();

    let lookahead_row = adw::SpinRow::with_range(5.0, 200.0, 1.0);
    let attack_row = adw::SpinRow::with_range(1.0, 50.0, 1.0);
    let release_row = adw::SpinRow::with_range(50.0, 1000.0, 10.0);

    {
        let (
            auto_gain,
            skip_del,
            target_lufs,
            ag_mode,
            ag_apply_to,
            lookahead_ms,
            attack_ms,
            release_ms,
        ) = {
            let cfg = state.config.lock().expect("config lock poisoned");
            let s = &cfg.settings;
            (
                s.auto_gain,
                s.skip_delete_confirm,
                s.auto_gain_target_lufs,
                s.auto_gain_mode,
                s.auto_gain_apply_to,
                s.auto_gain_lookahead_ms,
                s.auto_gain_attack_ms,
                s.auto_gain_release_ms,
            )
        };

        let auto_gain_row = adw::SwitchRow::builder()
            .title("Auto-Gain Normalization")
            .subtitle("Normalize loudness across all sounds")
            .active(auto_gain)
            .build();
        {
            let state3 = Arc::clone(&state);
            let ag_group = auto_gain_group.downgrade();
            auto_gain_row.connect_active_notify(move |row| {
                let _ = commands::set_auto_gain(
                    row.is_active(),
                    Arc::clone(&state3.config),
                    Arc::clone(&state3.player),
                );
                if let Some(ag_group) = ag_group.upgrade() {
                    ag_group.set_visible(row.is_active());
                }
            });
        }
        playback_group.add(&auto_gain_row);

        let skip_del_row = adw::SwitchRow::builder()
            .title("Never Ask to Confirm Delete")
            .subtitle("Skip the confirmation dialog when deleting sounds")
            .active(skip_del)
            .build();
        let state2 = Arc::clone(&state);
        skip_del_row.connect_active_notify(move |row| {
            let _ = commands::set_skip_delete_confirm(row.is_active(), Arc::clone(&state2.config));
        });
        playback_group.add(&skip_del_row);

        let target_row = adw::SpinRow::with_range(-24.0, 0.0, 0.5);
        target_row.set_title("Target Volume (LUFS)");
        target_row.set_subtitle("Loudness target applied to the selected output(s)");
        target_row.set_value(target_lufs);
        {
            let state2 = Arc::clone(&state);
            target_row.connect_value_notify(move |row| {
                let _ = commands::set_auto_gain_target(
                    row.value(),
                    Arc::clone(&state2.config),
                    Arc::clone(&state2.player),
                );
            });
        }
        auto_gain_group.add(&target_row);

        let mode_row = adw::ComboRow::builder()
            .title("Auto-Gain Mode")
            .subtitle("How loudness correction is applied")
            .build();
        let mode_model = gtk4::StringList::new(&["Static (precomputed)", "Dynamic (look-ahead)"]);
        mode_row.set_model(Some(&mode_model));
        let is_dynamic = ag_mode == AutoGainMode::Dynamic;
        mode_row.set_selected(if is_dynamic { 1 } else { 0 });
        {
            let state2 = Arc::clone(&state);
            let la = lookahead_row.downgrade();
            let at = attack_row.downgrade();
            let rl = release_row.downgrade();
            mode_row.connect_selected_notify(move |row| {
                let mode = if row.selected() == 1 {
                    AutoGainMode::Dynamic
                } else {
                    AutoGainMode::Static
                };
                let _ = commands::set_auto_gain_mode(
                    mode.as_str().to_string(),
                    Arc::clone(&state2.config),
                    Arc::clone(&state2.player),
                );
                let show_dyn = mode == AutoGainMode::Dynamic;
                if let Some(la) = la.upgrade() {
                    la.set_visible(show_dyn);
                }
                if let Some(at) = at.upgrade() {
                    at.set_visible(show_dyn);
                }
                if let Some(rl) = rl.upgrade() {
                    rl.set_visible(show_dyn);
                }
            });
        }
        auto_gain_group.add(&mode_row);

        let apply_to_row = adw::ComboRow::builder()
            .title("Apply To")
            .subtitle("Auto-gain only affects the selected output path")
            .build();
        let apply_model = gtk4::StringList::new(&["Mic only (recommended)", "Mic + headphones"]);
        apply_to_row.set_model(Some(&apply_model));
        apply_to_row.set_selected(if ag_apply_to == AutoGainApplyTo::MicOnly {
            0
        } else {
            1
        });
        {
            let state2 = Arc::clone(&state);
            apply_to_row.connect_selected_notify(move |row| {
                let scope = if row.selected() == 0 {
                    AutoGainApplyTo::MicOnly
                } else {
                    AutoGainApplyTo::Both
                };
                let _ = commands::set_auto_gain_apply_to(
                    scope.as_str().to_string(),
                    Arc::clone(&state2.config),
                    Arc::clone(&state2.player),
                );
            });
        }
        auto_gain_group.add(&apply_to_row);

        lookahead_row.set_title("Look-ahead (ms)");
        lookahead_row.set_subtitle("Anticipation window for gain changes");
        lookahead_row.set_value(lookahead_ms as f64);
        lookahead_row.set_visible(is_dynamic);

        attack_row.set_title("Attack (ms)");
        attack_row.set_subtitle("How quickly gain reductions are applied");
        attack_row.set_value(attack_ms as f64);
        attack_row.set_visible(is_dynamic);

        release_row.set_title("Release (ms)");
        release_row.set_subtitle("How quickly gain returns to normal");
        release_row.set_value(release_ms as f64);
        release_row.set_visible(is_dynamic);

        {
            let state2 = Arc::clone(&state);
            let at2 = attack_row.downgrade();
            let rl2 = release_row.downgrade();
            lookahead_row.connect_value_notify(move |row| {
                let Some(at2) = at2.upgrade() else {
                    return;
                };
                let Some(rl2) = rl2.upgrade() else {
                    return;
                };
                let _ = commands::set_auto_gain_dynamic_settings(
                    row.value() as u32,
                    at2.value() as u32,
                    rl2.value() as u32,
                    Arc::clone(&state2.config),
                    Arc::clone(&state2.player),
                );
            });
        }
        {
            let state2 = Arc::clone(&state);
            let la2 = lookahead_row.downgrade();
            let rl2 = release_row.downgrade();
            attack_row.connect_value_notify(move |row| {
                let Some(la2) = la2.upgrade() else {
                    return;
                };
                let Some(rl2) = rl2.upgrade() else {
                    return;
                };
                let _ = commands::set_auto_gain_dynamic_settings(
                    la2.value() as u32,
                    row.value() as u32,
                    rl2.value() as u32,
                    Arc::clone(&state2.config),
                    Arc::clone(&state2.player),
                );
            });
        }
        {
            let state2 = Arc::clone(&state);
            let la2 = lookahead_row.downgrade();
            let at2 = attack_row.downgrade();
            release_row.connect_value_notify(move |row| {
                let Some(la2) = la2.upgrade() else {
                    return;
                };
                let Some(at2) = at2.upgrade() else {
                    return;
                };
                let _ = commands::set_auto_gain_dynamic_settings(
                    la2.value() as u32,
                    at2.value() as u32,
                    row.value() as u32,
                    Arc::clone(&state2.config),
                    Arc::clone(&state2.player),
                );
            });
        }
        auto_gain_group.add(&lookahead_row);
        auto_gain_group.add(&attack_row);
        auto_gain_group.add(&release_row);

        let analyze_row = adw::ActionRow::builder()
            .title("Analyze All Sounds")
            .subtitle("Scan sounds that still lack loudness data")
            .build();
        let analyze_btn = gtk4::Button::builder()
            .label("Analyze")
            .css_classes(vec!["settings-primary-btn"])
            .valign(gtk4::Align::Center)
            .build();
        let spinner = gtk4::Spinner::builder()
            .valign(gtk4::Align::Center)
            .visible(false)
            .build();
        analyze_row.add_suffix(&spinner);
        analyze_row.add_suffix(&analyze_btn);
        {
            let state2 = Arc::clone(&state);
            let spinner2 = spinner.downgrade();
            analyze_btn.connect_clicked(move |btn| {
                let in_flight = commands::get_loudness_status_summary(Arc::clone(&state2.config))
                    .map(|summary| summary.in_flight_backfill || summary.in_flight_refinement)
                    .unwrap_or(false);
                if in_flight {
                    commands::cancel_loudness_analysis();
                    crate::ui_event_bridge::post_loudness_status_refresh();
                    return;
                }
                match commands::trigger_missing_loudness_analysis(
                    Arc::clone(&state2.config),
                    true,
                    Some(Box::new(|_| {
                        crate::ui_event_bridge::post_loudness_status_refresh();
                    })),
                ) {
                    Ok(commands::MissingLoudnessAnalysisTrigger::Started) => {
                        if let Some(spinner2) = spinner2.upgrade() {
                            spinner2.set_visible(true);
                            spinner2.start();
                        }
                        btn.set_sensitive(false);
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("Failed to schedule manual loudness analysis: {e}"),
                }
            });
        }
        auto_gain_group.add(&analyze_row);

        let refine_row = adw::ActionRow::builder()
            .title("Refine Estimated Sounds")
            .subtitle("Run full loudness analysis for sounds that are still estimated")
            .build();
        let refine_btn = gtk4::Button::builder()
            .label("Refine")
            .css_classes(vec!["settings-primary-btn"])
            .valign(gtk4::Align::Center)
            .build();
        let refine_spinner = gtk4::Spinner::builder()
            .valign(gtk4::Align::Center)
            .visible(false)
            .build();
        refine_row.add_suffix(&refine_spinner);
        refine_row.add_suffix(&refine_btn);
        {
            let state2 = Arc::clone(&state);
            let refine_spinner2 = refine_spinner.downgrade();
            refine_btn.connect_clicked(move |btn| {
                let in_flight = commands::get_loudness_status_summary(Arc::clone(&state2.config))
                    .map(|summary| summary.in_flight_backfill || summary.in_flight_refinement)
                    .unwrap_or(false);
                if in_flight {
                    commands::cancel_loudness_analysis();
                    crate::ui_event_bridge::post_loudness_status_refresh();
                    return;
                }
                match commands::trigger_estimated_loudness_refinement(
                    Arc::clone(&state2.config),
                    true,
                ) {
                    Ok(commands::EstimatedLoudnessRefinementTrigger::Started) => {
                        if let Some(refine_spinner2) = refine_spinner2.upgrade() {
                            refine_spinner2.set_visible(true);
                            refine_spinner2.start();
                        }
                        btn.set_sensitive(false);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        log::warn!("Failed to schedule manual loudness refinement: {e}");
                    }
                }
            });
        }
        auto_gain_group.add(&refine_row);

        let status_row = adw::ActionRow::builder()
            .title("Loudness Status")
            .subtitle("Loading loudness state…")
            .build();
        let status_badge = gtk4::Label::builder()
            .label("Checking…")
            .valign(gtk4::Align::Center)
            .build();
        status_row.add_suffix(&status_badge);
        auto_gain_group.add(&status_row);

        let state2 = Arc::clone(&state);
        let status_row_weak = status_row.downgrade();
        let status_badge_weak = status_badge.downgrade();
        let analyze_btn_weak = analyze_btn.downgrade();
        let analyze_spinner_weak = spinner.downgrade();
        let refine_btn_weak = refine_btn.downgrade();
        let refine_spinner_weak = refine_spinner.downgrade();

        let refresh_loudness_status: Rc<dyn Fn()> = Rc::new({
            let state2 = Arc::clone(&state2);
            let status_row_weak = status_row_weak.clone();
            let status_badge_weak = status_badge_weak.clone();
            let analyze_btn_weak = analyze_btn_weak.clone();
            let analyze_spinner_weak = analyze_spinner_weak.clone();
            let refine_btn_weak = refine_btn_weak.clone();
            let refine_spinner_weak = refine_spinner_weak.clone();
            move || {
                let Some(status_row) = status_row_weak.upgrade() else {
                    return;
                };
                let Some(status_badge) = status_badge_weak.upgrade() else {
                    return;
                };
                let Some(analyze_btn) = analyze_btn_weak.upgrade() else {
                    return;
                };
                let Some(analyze_spinner) = analyze_spinner_weak.upgrade() else {
                    return;
                };
                let Some(refine_btn) = refine_btn_weak.upgrade() else {
                    return;
                };
                let Some(refine_spinner) = refine_spinner_weak.upgrade() else {
                    return;
                };

                let summary =
                    match commands::get_loudness_status_summary(Arc::clone(&state2.config)) {
                        Ok(summary) => summary,
                        Err(e) => {
                            log::warn!("Failed to read loudness status summary: {e}");
                            return;
                        }
                    };

                apply_loudness_status_summary(
                    &summary,
                    &status_row,
                    &status_badge,
                    &analyze_btn,
                    &analyze_spinner,
                    &refine_btn,
                    &refine_spinner,
                );
            }
        });

        refresh_loudness_status();
        {
            let refresh_loudness_status = Rc::clone(&refresh_loudness_status);
            crate::ui_event_bridge::set_loudness_status_refresh_handler(move || {
                refresh_loudness_status();
            });
        }

        // Refresh once when the settings overlay opens; completion callbacks refresh explicitly.
        {
            let refresh_loudness_status = Rc::clone(&refresh_loudness_status);
            if let Some(visibility_widget) = visibility_weak.upgrade() {
                visibility_widget.connect_visible_notify(move |widget| {
                    if widget.is_visible() {
                        refresh_loudness_status();
                    }
                });
            }
        }

        auto_gain_group.set_visible(auto_gain);
    }
    page.add(&playback_group);
    page.add(&auto_gain_group);

    let mic_group = adw::PreferencesGroup::builder()
        .title("Microphone Source")
        .description("Select which microphone to use for virtual mic passthrough")
        .build();

    {
        let sources = commands::list_audio_sources(Arc::clone(&state.player));
        let (current_mic, current_default_source_mode, current_latency_profile) = {
            let cfg = state.config.lock().expect("config lock poisoned");
            (
                cfg.settings.mic_source.clone(),
                cfg.settings.default_source_mode,
                cfg.settings.mic_latency_profile,
            )
        };

        let mic_row = adw::ComboRow::builder().title("Microphone").build();

        let mut items: Vec<&str> = vec!["Auto-detect (Default)"];
        let source_names: Vec<String> = sources.iter().map(|s| s.name.clone()).collect();
        let source_labels: Vec<String> = sources.iter().map(|s| s.display_name.clone()).collect();
        for label in &source_labels {
            items.push(label.as_str());
        }
        let model = gtk4::StringList::new(&items);
        mic_row.set_model(Some(&model));

        let selected_idx = match &current_mic {
            Some(src) => source_names
                .iter()
                .position(|n| n == src)
                .map(|i| (i + 1) as u32)
                .unwrap_or(0),
            None => 0,
        };
        mic_row.set_selected(selected_idx);
        let confirmed_mic_selection = Rc::new(RefCell::new(selected_idx));
        let suppress_mic_selection = Rc::new(Cell::new(false));

        let state2 = Arc::clone(&state);
        let confirmed_mic_selection2 = Rc::clone(&confirmed_mic_selection);
        let suppress_mic_selection2 = Rc::clone(&suppress_mic_selection);
        mic_row.connect_selected_notify(move |row| {
            if suppress_mic_selection2.get() {
                return;
            }
            let idx = row.selected();
            let previous_selected = *confirmed_mic_selection2.borrow();
            if idx == previous_selected {
                return;
            }
            let source = if idx == 0 {
                None
            } else {
                source_names.get(idx as usize - 1).cloned()
            };
            row.set_sensitive(false);
            let row_weak = row.downgrade();
            let confirmed_mic_selection3 = Rc::clone(&confirmed_mic_selection2);
            let suppress_mic_selection3 = Rc::clone(&suppress_mic_selection2);
            if let Err(e) = commands::set_mic_source_async(
                source,
                Arc::clone(&state2.config),
                Arc::clone(&state2.player),
                move |result| {
                    let Some(row) = row_weak.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(()) => {
                            *confirmed_mic_selection3.borrow_mut() = idx;
                        }
                        Err(err) => {
                            log::warn!("Set mic source failed: {err}");
                            suppress_mic_selection3.set(true);
                            row.set_selected(previous_selected);
                            suppress_mic_selection3.set(false);
                        }
                    }
                    row.set_sensitive(true);
                },
            ) {
                log::warn!("Failed to dispatch mic source change: {e}");
                suppress_mic_selection2.set(true);
                row.set_selected(previous_selected);
                suppress_mic_selection2.set(false);
                row.set_sensitive(true);
            }
        });

        mic_group.add(&mic_row);

        let default_mode_row = adw::ComboRow::builder()
            .title("Default Microphone")
            .subtitle("Controls whether the app claims the system default mic for games")
            .build();
        let default_mode_items = gtk4::StringList::new(&["Manual", "Auto While Running"]);
        default_mode_row.set_model(Some(&default_mode_items));
        default_mode_row.set_selected(match current_default_source_mode {
            DefaultSourceMode::Manual => 0,
            DefaultSourceMode::AutoWhileRunning => 1,
        });
        let confirmed_default_mode_selection = Rc::new(RefCell::new(default_mode_row.selected()));
        let suppress_default_mode_selection = Rc::new(Cell::new(false));

        let state3 = Arc::clone(&state);
        let confirmed_default_mode_selection2 = Rc::clone(&confirmed_default_mode_selection);
        let suppress_default_mode_selection2 = Rc::clone(&suppress_default_mode_selection);
        default_mode_row.connect_selected_notify(move |row| {
            if suppress_default_mode_selection2.get() {
                return;
            }
            let selected = row.selected();
            let previous_selected = *confirmed_default_mode_selection2.borrow();
            if selected == previous_selected {
                return;
            }
            let mode = match row.selected() {
                1 => DefaultSourceMode::AutoWhileRunning,
                _ => DefaultSourceMode::Manual,
            };
            row.set_sensitive(false);
            let row_weak = row.downgrade();
            let confirmed_default_mode_selection3 = Rc::clone(&confirmed_default_mode_selection2);
            let suppress_default_mode_selection3 = Rc::clone(&suppress_default_mode_selection2);
            if let Err(e) = commands::set_default_source_mode_async(
                mode,
                Arc::clone(&state3.config),
                Arc::clone(&state3.player),
                move |result| {
                    let Some(row) = row_weak.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(()) => {
                            *confirmed_default_mode_selection3.borrow_mut() = selected;
                        }
                        Err(err) => {
                            log::warn!("Set default source mode failed: {err}");
                            suppress_default_mode_selection3.set(true);
                            row.set_selected(previous_selected);
                            suppress_default_mode_selection3.set(false);
                        }
                    }
                    row.set_sensitive(true);
                },
            ) {
                log::warn!("Failed to dispatch default source mode change: {e}");
                suppress_default_mode_selection2.set(true);
                row.set_selected(previous_selected);
                suppress_default_mode_selection2.set(false);
                row.set_sensitive(true);
            }
        });
        mic_group.add(&default_mode_row);

        let latency_profile_row = adw::ComboRow::builder()
            .title("Mic Latency Profile")
            .subtitle(mic_latency_profile_subtitle(current_latency_profile))
            .build();
        let latency_profile_items = gtk4::StringList::new(&[
            "Balanced (recommended)",
            "Low latency",
            "Ultra latency (experimental)",
        ]);
        latency_profile_row.set_model(Some(&latency_profile_items));
        latency_profile_row.set_selected(match current_latency_profile {
            MicLatencyProfile::Balanced => 0,
            MicLatencyProfile::Low => 1,
            MicLatencyProfile::Ultra => 2,
        });
        let confirmed_latency_selection = Rc::new(RefCell::new(latency_profile_row.selected()));
        let suppress_latency_selection = Rc::new(Cell::new(false));

        let state4 = Arc::clone(&state);
        let confirmed_latency_selection2 = Rc::clone(&confirmed_latency_selection);
        let suppress_latency_selection2 = Rc::clone(&suppress_latency_selection);
        latency_profile_row.connect_selected_notify(move |row| {
            if suppress_latency_selection2.get() {
                return;
            }

            let selected = row.selected();
            let previous_selected = *confirmed_latency_selection2.borrow();
            if selected == previous_selected {
                return;
            }

            let profile = match selected {
                1 => MicLatencyProfile::Low,
                2 => MicLatencyProfile::Ultra,
                _ => MicLatencyProfile::Balanced,
            };
            let previous_profile = match previous_selected {
                1 => MicLatencyProfile::Low,
                2 => MicLatencyProfile::Ultra,
                _ => MicLatencyProfile::Balanced,
            };

            row.set_subtitle(mic_latency_profile_subtitle(profile));
            row.set_sensitive(false);
            let row_weak = row.downgrade();
            let confirmed_latency_selection3 = Rc::clone(&confirmed_latency_selection2);
            let suppress_latency_selection3 = Rc::clone(&suppress_latency_selection2);

            if let Err(e) = commands::set_mic_latency_profile_async(
                profile,
                Arc::clone(&state4.config),
                Arc::clone(&state4.player),
                move |result| {
                    let Some(row) = row_weak.upgrade() else {
                        return;
                    };
                    match result {
                        Ok(()) => {
                            *confirmed_latency_selection3.borrow_mut() = selected;
                            row.set_subtitle(mic_latency_profile_subtitle(profile));
                        }
                        Err(err) => {
                            log::warn!("Set mic latency profile failed: {err}");
                            suppress_latency_selection3.set(true);
                            row.set_selected(previous_selected);
                            suppress_latency_selection3.set(false);
                            row.set_subtitle(mic_latency_profile_subtitle(previous_profile));
                        }
                    }
                    row.set_sensitive(true);
                },
            ) {
                log::warn!("Failed to dispatch mic latency profile change: {e}");
                suppress_latency_selection2.set(true);
                row.set_selected(previous_selected);
                suppress_latency_selection2.set(false);
                row.set_subtitle(mic_latency_profile_subtitle(previous_profile));
                row.set_sensitive(true);
            }
        });
        mic_group.add(&latency_profile_row);

        let active_target = commands::active_capture_target(Arc::clone(&state.player));
        let active_target_label = match active_target.as_deref() {
            Some(name) => {
                let display = sources
                    .iter()
                    .find(|s| s.name == name)
                    .map(|s| s.display_name.as_str())
                    .unwrap_or(name);
                format!("Active: {display}")
            }
            None => "Waiting for microphone…".to_string(),
        };
        let status_row = adw::ActionRow::builder()
            .title("Passthrough Status")
            .subtitle(&active_target_label)
            .build();
        mic_group.add(&status_row);
    }
    page.add(&mic_group);

    let app_hints_group = adw::PreferencesGroup::builder()
        .title("App Setup")
        .description(
            "The soundboard mic is your system default. \
            For apps where you previously chose a specific mic, \
            switch them to \"Default\" or select \"Linux Soundboard Mic\" directly.",
        )
        .build();

    for (app_name, hint) in [
        (
            "Discord",
            "Settings → Voice & Video → Input Device → Default",
        ),
        ("OBS Studio", "Audio → Mic/Aux → Default or Linux Soundboard Mic"),
        (
            "Steam Voice",
            "Steam → Settings → Voice → Microphone Device → Default Device",
        ),
    ] {
        let row = adw::ActionRow::builder()
            .title(app_name)
            .subtitle(hint)
            .build();
        app_hints_group.add(&row);
    }
    page.add(&app_hints_group);

    let theme_group = adw::PreferencesGroup::builder().title("Appearance").build();

    {
        let current_theme = {
            let cfg = state.config.lock().expect("config lock poisoned");
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
            let cfg = state.config.lock().expect("config lock poisoned");
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

fn build_sound_folder_row(
    folder: String,
    state: Arc<AppState>,
    folders_group: &adw::PreferencesGroup,
    add_folder_row: &adw::ActionRow,
    folder_rows: FolderRowRefs,
    rebuild_pending: RebuildPending,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(&folder).build();

    let remove_btn = gtk4::Button::builder()
        .css_classes(vec!["flat", "settings-folder-remove-btn"])
        .has_frame(false)
        .width_request(28)
        .height_request(28)
        .valign(gtk4::Align::Center)
        .tooltip_text("Remove folder")
        .build();
    icons::apply_button_icon(&remove_btn, icons::DELETE);

    {
        let folder_owned = folder.clone();
        let state2 = Arc::clone(&state);
        let folders_group2 = folders_group.downgrade();
        let add_folder_row2 = add_folder_row.downgrade();
        let folder_rows2 = Rc::clone(&folder_rows);
        let rebuild_pending2 = Rc::clone(&rebuild_pending);
        let on_library_changed2 = on_library_changed.clone();
        remove_btn.connect_clicked(move |_| {
            log::info!("Remove folder button clicked: {}", folder_owned);
            if let Err(e) = commands::remove_sound_folder(
                folder_owned.clone(),
                Arc::clone(&state2.config),
                Arc::clone(&state2.hotkeys),
            ) {
                log::warn!("Remove folder failed: {e}");
                return;
            }
            log::info!("Remove folder command succeeded");
            let Some(folders_group2) = folders_group2.upgrade() else {
                log::warn!("folders_group2 weak ref failed to upgrade");
                return;
            };
            let Some(add_folder_row2) = add_folder_row2.upgrade() else {
                log::warn!("add_folder_row2 weak ref failed to upgrade");
                return;
            };
            schedule_rebuild_sound_folder_rows(
                &folders_group2,
                &add_folder_row2,
                Arc::clone(&state2),
                Rc::clone(&folder_rows2),
                Rc::clone(&rebuild_pending2),
                on_library_changed2.clone(),
            );
            if let Some(cb) = on_library_changed2.as_ref() {
                cb();
            }
        });
    }

    row.add_suffix(&remove_btn);
    row
}

fn schedule_rebuild_sound_folder_rows(
    folders_group: &adw::PreferencesGroup,
    add_folder_row: &adw::ActionRow,
    state: Arc<AppState>,
    folder_rows: FolderRowRefs,
    rebuild_pending: RebuildPending,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) {
    if !try_set_rebuild_pending(rebuild_pending.as_ref()) {
        log::debug!("schedule_rebuild_sound_folder_rows: Rebuild already pending");
        return;
    }

    log::info!("schedule_rebuild_sound_folder_rows: Scheduling rebuild");
    let folders_group = folders_group.clone();
    let add_folder_row = add_folder_row.clone();
    let folder_rows = Rc::clone(&folder_rows);
    let rebuild_pending = Rc::clone(&rebuild_pending);

    // Use idle_add to ensure this runs after the current event is processed
    gtk4::glib::idle_add_local_once(move || {
        log::info!("schedule_rebuild_sound_folder_rows: Idle callback executing");
        rebuild_sound_folder_rows(
            &folders_group,
            &add_folder_row,
            state,
            folder_rows,
            Rc::clone(&rebuild_pending),
            on_library_changed,
        );

        clear_rebuild_pending(rebuild_pending.as_ref());
    });
}

fn rebuild_sound_folder_rows(
    folders_group: &adw::PreferencesGroup,
    add_folder_row: &adw::ActionRow,
    state: Arc<AppState>,
    folder_rows: FolderRowRefs,
    rebuild_pending: RebuildPending,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) {
    log::info!("rebuild_sound_folder_rows: Starting rebuild");

    let existing_rows = {
        let mut tracked = folder_rows.borrow_mut();
        std::mem::take(&mut *tracked)
    };
    log::info!(
        "rebuild_sound_folder_rows: Removing {} existing rows",
        existing_rows.len()
    );
    for row_weak in existing_rows {
        let Some(row) = row_weak.upgrade() else {
            continue;
        };

        if row.parent().is_none() {
            continue;
        }

        // PreferencesGroup manages internal wrappers; remove via the group API.
        folders_group.remove(&row);
    }

    let folders = {
        let cfg = state.config.lock().expect("config lock poisoned");
        log::info!(
            "rebuild_sound_folder_rows: Config has {} folders: {:?}",
            cfg.sound_folders.len(),
            cfg.sound_folders
        );
        cfg.sound_folders.clone()
    };

    let mut added_rows = 0usize;
    for folder in folders {
        let row = build_sound_folder_row(
            folder,
            Arc::clone(&state),
            folders_group,
            add_folder_row,
            Rc::clone(&folder_rows),
            Rc::clone(&rebuild_pending),
            on_library_changed.clone(),
        );
        folders_group.add(&row);
        folder_rows.borrow_mut().push(row.downgrade());
        added_rows = added_rows.saturating_add(1);
    }

    if should_attach_add_folder_row(add_folder_row.parent().is_some()) {
        folders_group.add(add_folder_row);
    } else {
        log::debug!("rebuild_sound_folder_rows: Add Folder row already attached");
    }

    log::info!(
        "rebuild_sound_folder_rows: Rebuild complete, {} rows added",
        added_rows
    );
}

fn build_hotkeys_page(state: Arc<AppState>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Control Hotkeys")
        .icon_name(icons::name(icons::KEYBOARD))
        .build();

    let unavailable_reason = {
        let hotkeys = state.hotkeys.lock().expect("hotkeys lock poisoned");
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
            install_btn.connect_clicked(move |btn| {
                if let Some(win) = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok()) {
                    crate::ui::dialogs::prompt_swhkd_install(
                        &win,
                        Arc::clone(&config),
                        Arc::clone(&hotkeys),
                        &reason_text,
                    );
                }
            });

            row.add_suffix(&install_btn);
            group.add(&row);
        }
    }

    for meta in ControlHotkeyAction::all() {
        let row = build_hotkey_row(Arc::clone(&state), meta.action);
        group.add(&row);
    }

    page.add(&group);
    page
}

fn build_hotkey_row(state: Arc<AppState>, action: ControlHotkeyAction) -> adw::ActionRow {
    let current_hotkey = {
        let cfg = state.config.lock().expect("config lock poisoned");
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
        record_btn.connect_clicked(move |btn| {
            if let Some(win) = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok()) {
                let current = {
                    let cfg = state2.config.lock().expect("config lock poisoned");
                    cfg.settings.control_hotkeys.get_cloned(action)
                };
                let state3 = Arc::clone(&state2);
                let lbl2 = lbl.clone();
                let clear3 = clear2.clone();
                let error_window = win.downgrade();
                let config_for_capture = Arc::clone(&state2.config);
                let hotkeys_for_capture = Arc::clone(&state2.hotkeys);
                crate::ui::dialogs::show_hotkey_capture(
                    &win,
                    current.as_deref(),
                    move |hotkey| {
                        {
                            let cfg = config_for_capture
                                .lock()
                                .map_err(|e| format!("Config lock poisoned: {}", e))?;
                            commands::validate_hotkey_available(&cfg, action.binding_id(), hotkey)?;
                        }
                        hotkeys_for_capture
                            .lock()
                            .map_err(|e| format!("Hotkeys lock poisoned: {}", e))?
                            .validate_hotkey_blocking(hotkey)
                    },
                    move |result| match result {
                        Some(hk) => {
                            match commands::set_control_hotkey(
                                action.id().to_string(),
                                Some(hk.clone()),
                                Arc::clone(&state3.config),
                                Arc::clone(&state3.hotkeys),
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
                                    let message = crate::hotkeys::format_hotkey_error(&e);
                                    if let Some(error_window) = error_window.upgrade() {
                                        if crate::hotkeys::should_offer_swhkd_install(&e) {
                                            crate::ui::dialogs::show_hotkey_error_with_install_option(
                                                &error_window,
                                                "Failed to Set Control Hotkey",
                                                &message,
                                                Arc::clone(&state3.config),
                                                Arc::clone(&state3.hotkeys),
                                            );
                                        } else {
                                            crate::ui::dialogs::show_error(
                                                &error_window,
                                                "Failed to Set Control Hotkey",
                                                &message,
                                            );
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
                                    if let Some(error_window) = error_window.upgrade() {
                                        crate::ui::dialogs::show_error(
                                            &error_window,
                                            "Failed to Clear Control Hotkey",
                                            &e,
                                        );
                                    }
                                }
                            }
                        }
                    },
                );
            }
        });
    }

    {
        let state2 = Arc::clone(&state);
        let lbl = hotkey_label.downgrade();
        clear_btn.connect_clicked(move |btn| {
            match commands::set_control_hotkey(
                action.id().to_string(),
                None,
                Arc::clone(&state2.config),
                Arc::clone(&state2.hotkeys),
            ) {
                Ok(_) => {
                    if let Some(lbl) = lbl.upgrade() {
                        lbl.set_text("Not set");
                    }
                    btn.set_sensitive(false);
                }
                Err(e) => {
                    log::warn!("Clear control hotkey failed: {e}");
                    if let Some(win) = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok()) {
                        crate::ui::dialogs::show_error(&win, "Failed to Clear Control Hotkey", &e);
                    }
                }
            }
        });
    }

    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_pending_coalesces_duplicate_schedules() {
        let pending = Cell::new(false);

        assert!(try_set_rebuild_pending(&pending));
        assert!(!try_set_rebuild_pending(&pending));
    }

    #[test]
    fn rebuild_pending_can_be_rearmed_after_clear() {
        let pending = Cell::new(false);

        assert!(try_set_rebuild_pending(&pending));
        clear_rebuild_pending(&pending);
        assert!(try_set_rebuild_pending(&pending));
    }

    #[test]
    fn add_folder_row_attach_is_idempotent() {
        assert!(should_attach_add_folder_row(false));
        assert!(!should_attach_add_folder_row(true));
    }

    #[test]
    fn loudness_poll_pauses_when_dialog_hidden() {
        assert!(should_poll_loudness_summary(true));
        assert!(!should_poll_loudness_summary(false));
    }
}
