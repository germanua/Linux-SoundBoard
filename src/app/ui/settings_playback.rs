use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app_state::AppState;
use crate::commands;
use crate::config::{AutoGainApplyTo, AutoGainMode};

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
        analyze_btn.set_sensitive(summary.pending_count > 0);
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

pub(super) fn build_playback_groups(
    state: Arc<AppState>,
    visibility_weak: gtk4::glib::WeakRef<gtk4::Widget>,
) -> (adw::PreferencesGroup, adw::PreferencesGroup) {
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
            let cfg = state.config.lock();
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
                    &state3.loudness_coordinators,
                );
                if let Some(ag_group) = ag_group.upgrade() {
                    ag_group.set_visible(row.is_active());
                }
            });
        }
        playback_group.add(&auto_gain_row);

        let skip_del_row = adw::SwitchRow::builder()
            .title("Never Ask to Confirm Removal")
            .subtitle("Skip the confirmation dialog when removing sounds")
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
                let in_flight = commands::get_loudness_status_summary(
                    Arc::clone(&state2.config),
                    &state2.loudness_coordinators,
                )
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
                    &state2.loudness_coordinators,
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
                let in_flight = commands::get_loudness_status_summary(
                    Arc::clone(&state2.config),
                    &state2.loudness_coordinators,
                )
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
                    &state2.loudness_coordinators,
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

                let summary = match commands::get_loudness_status_summary(
                    Arc::clone(&state2.config),
                    &state2.loudness_coordinators,
                ) {
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

    (playback_group, auto_gain_group)
}
