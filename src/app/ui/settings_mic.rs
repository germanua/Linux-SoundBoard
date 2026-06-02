use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app_state::AppState;
use crate::commands;
use crate::config::{DefaultSourceMode, MicLatencyProfile};

/// (target value, checkmark image) for every source option, so the checkmark can
/// be moved to whichever row is the confirmed selection. Value `None` == auto.
type SourceChecks = Rc<RefCell<Vec<(Option<String>, gtk4::Image)>>>;

fn mic_latency_profile_subtitle(profile: MicLatencyProfile) -> &'static str {
    match profile {
        MicLatencyProfile::Balanced => "Stable default for most systems",
        MicLatencyProfile::Low => "Lower queueing delay with minimal extra CPU",
        MicLatencyProfile::Ultra => {
            "Lowest queue delay (may auto-fallback to Low if underruns occur)"
        }
    }
}

/// Build the two microphone preference groups: a single-pick checkmark list for
/// the passthrough source, and a routing/latency/status group.
pub(super) fn build_mic_group(
    state: Arc<AppState>,
) -> (adw::PreferencesGroup, adw::PreferencesGroup) {
    let sources = commands::list_audio_sources(Arc::clone(&state.player));
    let (current_mic, current_default_source_mode, current_latency_profile) = {
        let cfg = state.config.lock();
        (
            cfg.settings.mic_source.clone(),
            cfg.settings.default_source_mode,
            cfg.settings.mic_latency_profile,
        )
    };

    let source_group = build_source_group(Arc::clone(&state), &sources, current_mic);

    // ---- Routing + latency + status -------------------------------------
    let routing_group = adw::PreferencesGroup::builder()
        .title("Microphone Routing")
        .build();

    let default_mode_row = adw::ComboRow::builder()
        .title("Routing Mode")
        .subtitle("How apps receive the soundboard mic.")
        .build();
    let default_mode_items = gtk4::StringList::new(&[
        "Default — Soundboard is the system mic (recommended)",
        "Manual — I'll pick the default mic myself",
    ]);
    default_mode_row.set_model(Some(&default_mode_items));
    default_mode_row.set_selected(match current_default_source_mode {
        DefaultSourceMode::Default => 0,
        DefaultSourceMode::Manual => 1,
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
            1 => DefaultSourceMode::Manual,
            _ => DefaultSourceMode::Default,
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
    routing_group.add(&default_mode_row);

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
    routing_group.add(&latency_profile_row);

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
    routing_group.add(&status_row);

    (source_group, routing_group)
}

/// A single-pick list of mic sources. The active selection carries a checkmark;
/// activating a row sets it as the passthrough source. `Auto-detect (Default)`
/// (value `None`) is always the first row. Explicit selection of any listed
/// source is honoured by the engine even when auto-detect would skip it.
fn build_source_group(
    state: Arc<AppState>,
    sources: &[commands::AudioSource],
    current_mic: Option<String>,
) -> adw::PreferencesGroup {
    let source_group = adw::PreferencesGroup::builder()
        .title("Microphone Source")
        .description("Select which microphone to use for virtual mic passthrough")
        .build();

    let checks: SourceChecks = Rc::new(RefCell::new(Vec::new()));
    let selected: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(current_mic));
    let in_flight = Rc::new(Cell::new(false));

    let refresh_checks: Rc<dyn Fn()> = {
        let checks = Rc::clone(&checks);
        let selected = Rc::clone(&selected);
        Rc::new(move || {
            let sel = selected.borrow();
            for (value, image) in checks.borrow().iter() {
                // Opacity (not visibility) keeps every row's title left-aligned.
                image.set_opacity(if *value == *sel { 1.0 } else { 0.0 });
            }
        })
    };

    // Option rows: Auto-detect first, then one row per enumerated source.
    let mut options: Vec<(String, Option<String>, Option<String>)> = vec![(
        "Auto-detect (Default)".to_string(),
        Some("Prefers an enhancement chain, otherwise your hardware mic".to_string()),
        None,
    )];
    for source in sources {
        options.push((
            source.display_name.clone(),
            Some(source.name.clone()),
            Some(source.name.clone()),
        ));
    }

    for (title, subtitle, value) in options {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .build();
        if let Some(subtitle) = subtitle {
            row.set_subtitle(&subtitle);
        }
        let check = gtk4::Image::from_icon_name("object-select-symbolic");
        check.set_opacity(0.0);
        row.add_prefix(&check);
        checks.borrow_mut().push((value.clone(), check));

        let state_cb = Arc::clone(&state);
        let selected_cb = Rc::clone(&selected);
        let in_flight_cb = Rc::clone(&in_flight);
        let refresh_cb = Rc::clone(&refresh_checks);
        row.connect_activated(move |row| {
            if in_flight_cb.get() || *selected_cb.borrow() == value {
                return;
            }
            in_flight_cb.set(true);
            row.set_sensitive(false);

            let row_weak = row.downgrade();
            let selected_done = Rc::clone(&selected_cb);
            let in_flight_done = Rc::clone(&in_flight_cb);
            let refresh_done = Rc::clone(&refresh_cb);
            let value_done = value.clone();
            let dispatch = commands::set_mic_source_async(
                value.clone(),
                Arc::clone(&state_cb.config),
                Arc::clone(&state_cb.player),
                move |result| {
                    match result {
                        Ok(()) => *selected_done.borrow_mut() = value_done,
                        Err(err) => log::warn!("Set mic source failed: {err}"),
                    }
                    in_flight_done.set(false);
                    (refresh_done)();
                    if let Some(row) = row_weak.upgrade() {
                        row.set_sensitive(true);
                    }
                },
            );
            if let Err(e) = dispatch {
                log::warn!("Failed to dispatch mic source change: {e}");
                in_flight_cb.set(false);
                row.set_sensitive(true);
            }
        });

        source_group.add(&row);
    }

    // Show the checkmark on the currently-selected option.
    (refresh_checks)();

    source_group
}
