//! Settings dialog — adw::PreferencesDialog with General + Control Hotkeys pages.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use gtk4::gio;
use gtk4::prelude::*;
use gtk4::Window;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app_meta::{APP_TITLE, APP_VERSION};
use crate::app_state::AppState;
use crate::commands;
use crate::config::{AutoGainApplyTo, AutoGainMode, ControlHotkeyAction, ListStyle, Theme};

use super::icons;

fn set_appearance_row_selected(row: &adw::ActionRow, selected: bool) {
    if selected {
        row.add_css_class("appearance-choice-selected");
    } else {
        row.remove_css_class("appearance-choice-selected");
    }
}

/// Open the settings dialog as a child of `parent`.
pub fn show_settings(
    parent: &Window,
    state: Arc<AppState>,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
    on_list_style_changed: Option<Rc<dyn Fn(String) + 'static>>,
) {
    let prefs = adw::PreferencesDialog::builder()
        .title("Settings")
        .content_width(600)
        .content_height(700)
        .build();

    prefs.add(&build_general_page(
        Arc::clone(&state),
        parent,
        on_library_changed,
        on_list_style_changed,
    ));
    prefs.add(&build_hotkeys_page(Arc::clone(&state)));

    prefs.present(Some(parent));
}

// ──────────────────────────────────────────────────────────────────────────────
// General Page
// ──────────────────────────────────────────────────────────────────────────────

fn build_general_page(
    state: Arc<AppState>,
    parent: &Window,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
    on_list_style_changed: Option<Rc<dyn Fn(String) + 'static>>,
) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name(icons::name(icons::SETTINGS))
        .build();

    // ── Sound Folders group ───────────────────────────────────────────
    let folders_group = adw::PreferencesGroup::builder()
        .title("Sound Folders")
        .description("Folders scanned for audio files on startup")
        .build();
    let folder_rows: Rc<RefCell<Vec<adw::ActionRow>>> = Rc::new(RefCell::new(Vec::new()));

    let add_folder_row = adw::ActionRow::builder()
        .title("Add Folder…")
        .activatable(true)
        .build();
    add_folder_row.add_prefix(&icons::image(icons::ADD));
    {
        let state2 = Arc::clone(&state);
        let parent = parent.clone();
        let folders_group2 = folders_group.clone();
        let add_folder_row2 = add_folder_row.clone();
        let folder_rows2 = Rc::clone(&folder_rows);
        let on_library_changed2 = on_library_changed.clone();
        add_folder_row.connect_activated(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Select Sound Folder")
                .build();
            let state3 = Arc::clone(&state2);
            let parent_for_dialog = parent.clone();
            let folders_group3 = folders_group2.clone();
            let add_folder_row3 = add_folder_row2.clone();
            let folder_rows3 = Rc::clone(&folder_rows2);
            let on_library_changed3 = on_library_changed2.clone();
            dialog.select_folder(
                Some(&parent_for_dialog),
                gtk4::gio::Cancellable::NONE,
                move |result| {
                    if let Ok(folder) = result {
                        if let Some(path) = folder.path() {
                            let path_str = path.to_string_lossy().to_string();
                            if let Err(e) =
                                commands::add_sound_folder(path_str, Arc::clone(&state3.config))
                            {
                                log::warn!("Add folder failed: {e}");
                                return;
                            }
                            if let Err(e) = commands::refresh_sounds(
                                Arc::clone(&state3.config),
                                Arc::clone(&state3.hotkeys),
                            ) {
                                log::warn!("Refresh after adding folder failed: {e}");
                            }
                            rebuild_sound_folder_rows(
                                &folders_group3,
                                &add_folder_row3,
                                Arc::clone(&state3),
                                Rc::clone(&folder_rows3),
                                on_library_changed3.clone(),
                            );
                            if let Some(cb) = on_library_changed3.as_ref() {
                                cb();
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
        on_library_changed.clone(),
    );
    page.add(&folders_group);

    // ── Playback group ────────────────────────────────────────────────
    let playback_group = adw::PreferencesGroup::builder().title("Playback").build();

    // ── Auto-Gain Normalization group (shown only when auto-gain is on) ──
    let auto_gain_group = adw::PreferencesGroup::builder()
        .title("Auto-Gain Normalization")
        .description("Fine-tune loudness normalization")
        .build();

    // Dynamic settings rows (shown only when mode = "dynamic")
    let lookahead_row = adw::SpinRow::with_range(5.0, 200.0, 1.0);
    let attack_row = adw::SpinRow::with_range(1.0, 50.0, 1.0);
    let release_row = adw::SpinRow::with_range(50.0, 1000.0, 10.0);

    {
        let (
            allow_multi,
            auto_gain,
            skip_del,
            target_lufs,
            ag_mode,
            ag_apply_to,
            lookahead_ms,
            attack_ms,
            release_ms,
        ) = {
            let cfg = state.config.lock().unwrap();
            let s = &cfg.settings;
            (
                s.allow_multiple_playbacks,
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

        // Allow multiple playbacks
        let multi_row = adw::SwitchRow::builder()
            .title("Allow Multiple Sounds")
            .subtitle("Play multiple sounds simultaneously")
            .active(allow_multi)
            .build();
        let state2 = Arc::clone(&state);
        multi_row.connect_active_notify(move |row| {
            let _ =
                commands::set_allow_multiple_playbacks(row.is_active(), Arc::clone(&state2.config));
        });
        playback_group.add(&multi_row);

        // Auto-gain toggle
        let auto_gain_row = adw::SwitchRow::builder()
            .title("Auto-Gain Normalization")
            .subtitle("Normalize loudness across all sounds")
            .active(auto_gain)
            .build();
        {
            let state3 = Arc::clone(&state);
            let ag_group = auto_gain_group.clone();
            auto_gain_row.connect_active_notify(move |row| {
                let _ = commands::set_auto_gain(
                    row.is_active(),
                    Arc::clone(&state3.config),
                    Arc::clone(&state3.player),
                );
                ag_group.set_visible(row.is_active());
            });
        }
        playback_group.add(&auto_gain_row);

        // Skip delete confirmation
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

        // ── Auto-gain group contents ─────────────────────────────────

        // (a) Target Volume
        let target_row = adw::SpinRow::with_range(-24.0, 0.0, 0.5);
        target_row.set_title("Target Volume (LUFS)");
        target_row.set_subtitle("Loudness target for normalization");
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

        // (b) Auto-gain Mode
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
            let la = lookahead_row.clone();
            let at = attack_row.clone();
            let rl = release_row.clone();
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
                la.set_visible(show_dyn);
                at.set_visible(show_dyn);
                rl.set_visible(show_dyn);
            });
        }
        auto_gain_group.add(&mode_row);

        // (c) Apply To
        let apply_to_row = adw::ComboRow::builder()
            .title("Apply To")
            .subtitle("Which output receives gain correction")
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

        // (d) Dynamic settings
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
            let at2 = attack_row.clone();
            let rl2 = release_row.clone();
            lookahead_row.connect_value_notify(move |row| {
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
            let la2 = lookahead_row.clone();
            let rl2 = release_row.clone();
            attack_row.connect_value_notify(move |row| {
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
            let la2 = lookahead_row.clone();
            let at2 = attack_row.clone();
            release_row.connect_value_notify(move |row| {
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

        // (e) Analyze Loudness button
        let analyze_row = adw::ActionRow::builder()
            .title("Analyze All Sounds")
            .subtitle("Scan all sounds for loudness normalization")
            .build();
        let analyze_btn = gtk4::Button::builder()
            .label("Analyze")
            .css_classes(vec!["suggested-action"])
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
            let spinner2 = spinner.clone();
            let btn2 = analyze_btn.clone();
            analyze_btn.connect_clicked(move |_| {
                spinner2.set_visible(true);
                spinner2.start();
                btn2.set_sensitive(false);
                let config = Arc::clone(&state2.config);
                let spinner3 = spinner2.clone();
                let btn3 = btn2.clone();
                gtk4::glib::spawn_future_local(async move {
                    let _ =
                        gio::spawn_blocking(move || commands::analyze_all_loudness(config)).await;
                    spinner3.stop();
                    spinner3.set_visible(false);
                    btn3.set_sensitive(true);
                });
            });
        }
        auto_gain_group.add(&analyze_row);

        auto_gain_group.set_visible(auto_gain);
    }
    page.add(&playback_group);
    page.add(&auto_gain_group);

    // ── Microphone Source group ───────────────────────────────────────
    let mic_group = adw::PreferencesGroup::builder()
        .title("Microphone Source")
        .description("Select which microphone to use for virtual mic passthrough")
        .build();

    {
        let sources = commands::list_audio_sources();
        let current_mic = {
            let cfg = state.config.lock().unwrap();
            cfg.settings.mic_source.clone()
        };

        let mic_row = adw::ComboRow::builder().title("Microphone").build();

        let mut items: Vec<&str> = vec!["Auto-detect (Default)"];
        let source_names: Vec<String> = sources.iter().map(|s| s.name.clone()).collect();
        for name in &source_names {
            items.push(name.as_str());
        }
        let model = gtk4::StringList::new(&items);
        mic_row.set_model(Some(&model));

        // Select the current mic source
        let selected_idx = match &current_mic {
            Some(src) => source_names
                .iter()
                .position(|n| n == src)
                .map(|i| (i + 1) as u32)
                .unwrap_or(0),
            None => 0,
        };
        mic_row.set_selected(selected_idx);

        let state2 = Arc::clone(&state);
        mic_row.connect_selected_notify(move |row| {
            let idx = row.selected();
            let source = if idx == 0 {
                None
            } else {
                row.model()
                    .and_then(|m| m.downcast::<gtk4::StringList>().ok())
                    .and_then(|sl| sl.string(idx).map(|s| s.to_string()))
            };
            let _ = commands::set_mic_source(source, Arc::clone(&state2.config));
        });

        mic_group.add(&mic_row);
    }
    page.add(&mic_group);

    // ── Theme group ───────────────────────────────────────────────────
    let theme_group = adw::PreferencesGroup::builder().title("Appearance").build();

    {
        let current_theme = {
            let cfg = state.config.lock().unwrap();
            cfg.settings.theme
        };

        // Dark theme swatch colors: bg, surface, accent, text
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
            let color_str = color.to_string();
            da.set_draw_func(move |_, cr, w, h| {
                let rgba = gtk4::gdk::RGBA::parse(&color_str).unwrap();
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
            let color_str = color.to_string();
            da.set_draw_func(move |_, cr, w, h| {
                let rgba = gtk4::gdk::RGBA::parse(&color_str).unwrap();
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

        // Dark row click
        {
            let state2 = Arc::clone(&state);
            let dr = dark_row.clone();
            let lr = light_row.clone();
            dark_row.connect_activated(move |_| {
                let _ = commands::set_theme("dark".to_string(), Arc::clone(&state2.config));
                crate::ui::theme::apply_theme(Theme::Dark);
                set_appearance_row_selected(&dr, true);
                set_appearance_row_selected(&lr, false);
            });
        }

        // Light row click
        {
            let state2 = Arc::clone(&state);
            let dr = dark_row.clone();
            let lr = light_row.clone();
            light_row.connect_activated(move |_| {
                let _ = commands::set_theme("light".to_string(), Arc::clone(&state2.config));
                crate::ui::theme::apply_theme(Theme::Light);
                set_appearance_row_selected(&dr, false);
                set_appearance_row_selected(&lr, true);
            });
        }

        theme_group.add(&dark_row);
        theme_group.add(&light_row);
    }

    // ── List Style selection ───────────────────────────────────────
    {
        let current_style = {
            let cfg = state.config.lock().unwrap();
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
            .subtitle("Spacious layout with visual polish")
            .activatable(true)
            .build();
        card_row.add_css_class("appearance-choice-row");
        set_appearance_row_selected(&card_row, current_style == ListStyle::Card);

        // Compact row click
        {
            let state2 = Arc::clone(&state);
            let cr = compact_row.clone();
            let cdr = card_row.clone();
            let on_list_style_changed_compact = on_list_style_changed.clone();
            compact_row.connect_activated(move |_| {
                let _ = commands::set_list_style(
                    ListStyle::Compact.as_str().to_string(),
                    Arc::clone(&state2.config),
                );
                set_appearance_row_selected(&cr, true);
                set_appearance_row_selected(&cdr, false);
                if let Some(cb) = on_list_style_changed_compact.as_ref() {
                    cb(ListStyle::Compact.as_str().to_string());
                }
            });
        }

        // Card row click
        {
            let state2 = Arc::clone(&state);
            let cr = compact_row.clone();
            let cdr = card_row.clone();
            let on_list_style_changed_card = on_list_style_changed.clone();
            card_row.connect_activated(move |_| {
                let _ = commands::set_list_style(
                    ListStyle::Card.as_str().to_string(),
                    Arc::clone(&state2.config),
                );
                set_appearance_row_selected(&cr, false);
                set_appearance_row_selected(&cdr, true);
                if let Some(cb) = on_list_style_changed_card.as_ref() {
                    cb(ListStyle::Card.as_str().to_string());
                }
            });
        }

        theme_group.add(&compact_row);
        theme_group.add(&card_row);
    }
    page.add(&theme_group);

    // ── About group ───────────────────────────────────────────────────
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
    folder_rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(&folder).build();

    let remove_btn = gtk4::Button::builder()
        .css_classes(vec!["flat", "destructive-action"])
        .valign(gtk4::Align::Center)
        .build();
    icons::apply_button_icon(&remove_btn, icons::REMOVE);

    {
        let folder_owned = folder.clone();
        let state2 = Arc::clone(&state);
        let folders_group2 = folders_group.clone();
        let add_folder_row2 = add_folder_row.clone();
        let folder_rows2 = Rc::clone(&folder_rows);
        let on_library_changed2 = on_library_changed.clone();
        remove_btn.connect_clicked(move |_| {
            if let Err(e) =
                commands::remove_sound_folder(folder_owned.clone(), Arc::clone(&state2.config))
            {
                log::warn!("Remove folder failed: {e}");
                return;
            }
            rebuild_sound_folder_rows(
                &folders_group2,
                &add_folder_row2,
                Arc::clone(&state2),
                Rc::clone(&folder_rows2),
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

fn rebuild_sound_folder_rows(
    folders_group: &adw::PreferencesGroup,
    add_folder_row: &adw::ActionRow,
    state: Arc<AppState>,
    folder_rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    on_library_changed: Option<Rc<dyn Fn() + 'static>>,
) {
    let existing_rows = std::mem::take(&mut *folder_rows.borrow_mut());
    for row in existing_rows {
        folders_group.remove(&row);
    }

    if add_folder_row.parent().is_some() {
        folders_group.remove(add_folder_row);
    }

    let folders = {
        let cfg = state.config.lock().unwrap();
        cfg.sound_folders.clone()
    };

    for folder in folders {
        let row = build_sound_folder_row(
            folder,
            Arc::clone(&state),
            folders_group,
            add_folder_row,
            Rc::clone(&folder_rows),
            on_library_changed.clone(),
        );
        folders_group.add(&row);
        folder_rows.borrow_mut().push(row);
    }

    folders_group.add(add_folder_row);
}

// ──────────────────────────────────────────────────────────────────────────────
// Control Hotkeys Page
// ──────────────────────────────────────────────────────────────────────────────

fn build_hotkeys_page(state: Arc<AppState>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Control Hotkeys")
        .icon_name(icons::name(icons::KEYBOARD))
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Global Control Hotkeys")
        .description("These X11 hotkeys work from anywhere on your desktop")
        .build();

    for meta in ControlHotkeyAction::all() {
        let row = build_hotkey_row(Arc::clone(&state), meta.action);
        group.add(&row);
    }

    page.add(&group);
    page
}

fn build_hotkey_row(state: Arc<AppState>, action: ControlHotkeyAction) -> adw::ActionRow {
    let current_hotkey = {
        let cfg = state.config.lock().unwrap();
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
        .css_classes(vec!["flat", "destructive-action"])
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

    // Record hotkey
    {
        let state2 = Arc::clone(&state);
        let lbl = hotkey_label.clone();
        let clear2 = clear_btn.clone();
        record_btn.connect_clicked(move |btn| {
            if let Some(win) = btn.root().and_then(|r| r.downcast::<gtk4::Window>().ok()) {
                let current = {
                    let cfg = state2.config.lock().unwrap();
                    cfg.settings.control_hotkeys.get_cloned(action)
                };
                let state3 = Arc::clone(&state2);
                let lbl2 = lbl.clone();
                let clear3 = clear2.clone();
                crate::ui::dialogs::show_hotkey_capture(&win, current.as_deref(), move |result| {
                    match result {
                        Some(hk) => {
                            match commands::set_control_hotkey(
                                action.id().to_string(),
                                Some(hk.clone()),
                                Arc::clone(&state3.config),
                                Arc::clone(&state3.hotkeys),
                            ) {
                                Ok(_) => {
                                    lbl2.set_text(&hk);
                                    clear3.set_sensitive(true);
                                }
                                Err(e) => log::warn!("Set control hotkey failed: {e}"),
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
                                    lbl2.set_text("Not set");
                                    clear3.set_sensitive(false);
                                }
                                Err(e) => log::warn!("Clear control hotkey failed: {e}"),
                            }
                        }
                    }
                });
            }
        });
    }

    // Clear hotkey
    {
        let state2 = Arc::clone(&state);
        let lbl = hotkey_label.clone();
        clear_btn.connect_clicked(move |btn| {
            match commands::set_control_hotkey(
                action.id().to_string(),
                None,
                Arc::clone(&state2.config),
                Arc::clone(&state2.hotkeys),
            ) {
                Ok(_) => {
                    lbl.set_text("Not set");
                    btn.set_sensitive(false);
                }
                Err(e) => log::warn!("Clear control hotkey failed: {e}"),
            }
        });
    }

    row
}
