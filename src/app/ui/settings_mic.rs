use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::app_state::AppState;
use crate::commands;
use crate::config::{DefaultSourceMode, MicLatencyProfile};

fn mic_latency_profile_subtitle(profile: MicLatencyProfile) -> &'static str {
    match profile {
        MicLatencyProfile::Balanced => "Stable default for most systems",
        MicLatencyProfile::Low => "Lower queueing delay with minimal extra CPU",
        MicLatencyProfile::Ultra => {
            "Lowest queue delay (may auto-fallback to Low if underruns occur)"
        }
    }
}

pub(super) fn build_mic_group(state: Arc<AppState>) -> adw::PreferencesGroup {
    let mic_group = adw::PreferencesGroup::builder()
        .title("Microphone Source")
        .description("Select which microphone to use for virtual mic passthrough")
        .build();

    {
        let sources = commands::list_audio_sources(Arc::clone(&state.player));
        let (current_mic, current_default_source_mode, current_latency_profile) = {
            let cfg = state.config.lock();
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
            .title("Microphone Routing")
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

    mic_group
}
