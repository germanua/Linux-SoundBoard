use std::collections::HashSet;
use std::sync::Arc;

use gio::prelude::*;
use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{DragSource, EventControllerKey, GestureClick, Widget};

use crate::app_meta::GENERAL_TAB_ID;
use crate::commands;
use crate::ui::is_unmodified_delete_shortcut;

use crate::ui::{menu, tab_dnd};

use super::{SoundListInner, SoundRowData, SOUND_CONTEXT_NAMESPACE};

impl SoundListInner {
    pub(super) fn connect_activate(self: &Arc<Self>) {
        let inner_weak = Arc::downgrade(self);
        let store = self.store.clone();
        let invalid_ids = Arc::clone(&self.invalid_ids);

        self.col_view.connect_activate(move |cv, pos| {
            let Some(inner) = inner_weak.upgrade() else {
                return;
            };
            let Some(obj) = store
                .item(pos)
                .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
            else {
                return;
            };
            let sound = obj.borrow::<SoundRowData>().clone();
            let is_invalid = invalid_ids.lock().contains(&sound.id);
            let is_missing_on_demand = is_invalid;

            if !is_missing_on_demand {
                let sound_name = sound.name.clone();
                let sound_id = sound.id.clone();
                let invalid_ids_for_play = Arc::clone(&invalid_ids);
                let inner_weak_for_play = Arc::downgrade(&inner);
                crate::ui_event_bridge::mark_explicit_play_pending();
                if let Err(e) = commands::play_sound_async(
                    sound.id.clone(),
                    Arc::clone(&inner.state),
                    move |result| {
                        if let Err(e) = result {
                            crate::ui_event_bridge::clear_explicit_play_pending();
                            log::warn!("Play failed for '{}': {}", sound_name, e);
                            if matches!(e, commands::CommandError::SourceUnavailable(_)) {
                                invalid_ids_for_play.lock().insert(sound_id.clone());
                                if let Some(inner) = inner_weak_for_play.upgrade() {
                                    let changed_ids = HashSet::from([sound_id.clone()]);
                                    inner.rebind_rows_for_ids(&changed_ids);
                                }
                            }
                        }
                    },
                ) {
                    crate::ui_event_bridge::clear_explicit_play_pending();
                    log::warn!("Failed to dispatch play for '{}': {}", sound.name, e);
                }
                return;
            }

            let Some(full_sound) = inner.lookup_sound(&sound.id) else {
                log::warn!("Sound row '{}' no longer exists in config", sound.id);
                return;
            };

            let Some(win) = cv
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok())
            else {
                return;
            };

            let sound_name = full_sound.name.clone();
            let sound_path = full_sound
                .source_path
                .clone()
                .unwrap_or_else(|| full_sound.path.clone());
            let state_locate = Arc::clone(&inner.state);
            let invalid_locate = Arc::clone(&inner.invalid_ids);
            let inner_weak_locate = Arc::downgrade(&inner);
            let inner_weak_remove = Arc::downgrade(&inner);
            let sound_id_locate = sound.id.clone();
            let sound_id_remove = sound.id.clone();
            let win_locate = win.clone();

            inner.dialog_host.show_missing_file(
                &sound_name,
                &sound_path,
                move || {
                    let file_dialog = gtk4::FileDialog::builder()
                        .title("Locate Audio File")
                        .build();
                    let state = Arc::clone(&state_locate);
                    let invalid_ids = Arc::clone(&invalid_locate);
                    let inner_weak_refresh = inner_weak_locate.clone();
                    let sound_id = sound_id_locate.clone();
                    file_dialog.open(Some(&win_locate), gio::Cancellable::NONE, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let new_path = path.to_string_lossy().to_string();
                                if let Err(e) = commands::update_sound_source_async(
                                    sound_id.clone(),
                                    new_path,
                                    Arc::clone(&state.config),
                                    state.loudness_coordinators.clone(),
                                    move |result| match result {
                                        Ok(_) => {
                                            invalid_ids.lock().remove(&sound_id);
                                            if let Some(inner_refresh) =
                                                inner_weak_refresh.upgrade()
                                            {
                                                inner_refresh.refresh_from_state_inner();
                                            }
                                        }
                                        Err(e) => log::warn!("Update source failed: {e}"),
                                    },
                                ) {
                                    log::warn!("Failed to dispatch source update: {e}");
                                }
                            }
                        }
                    });
                },
                move || {
                    if let Some(inner_remove) = inner_weak_remove.upgrade() {
                        inner_remove.remove_sounds_now(vec![sound_id_remove.clone()]);
                    }
                },
            );
        });
    }

    pub(super) fn connect_remove_shortcut(self: &Arc<Self>) {
        let key = EventControllerKey::new();
        let inner_weak = Arc::downgrade(self);
        key.connect_key_pressed(move |_, keyval, _, modifiers| {
            let Some(inner) = inner_weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !is_unmodified_delete_shortcut(keyval, modifiers) {
                return glib::Propagation::Proceed;
            }

            if inner.request_sound_removal(inner.selected_sound_ids()) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.col_view.add_controller(key);
    }

    pub(super) fn setup_drag_drop(self: &Arc<Self>) {
        let drop_target_files = gtk4::DropTarget::new(
            gtk4::gdk::FileList::static_type(),
            gtk4::gdk::DragAction::COPY,
        );

        let drop_target_text =
            gtk4::DropTarget::new(glib::Type::STRING, gtk4::gdk::DragAction::COPY);

        {
            let inner_weak = Arc::downgrade(self);
            drop_target_files.connect_drop(move |_, value, _, _| {
                let Some(inner) = inner_weak.upgrade() else {
                    return false;
                };
                log::info!("File drop detected in sound list");
                let Ok(file_list) = value.get::<gtk4::gdk::FileList>() else {
                    log::warn!("Failed to get file list from drop");
                    return false;
                };

                let dropped_paths = file_list
                    .files()
                    .into_iter()
                    .filter_map(|file| file.path().map(|path| path.to_string_lossy().to_string()))
                    .collect::<Vec<_>>();

                log::info!("Dropped {} files: {:?}", dropped_paths.len(), dropped_paths);
                inner.handle_dropped_files(dropped_paths)
            });
        }

        {
            let inner_weak = Arc::downgrade(self);
            drop_target_text.connect_drop(move |_, value, _, _| {
                let Some(inner) = inner_weak.upgrade() else {
                    return false;
                };
                log::info!("Text/URI drop detected in sound list");
                let Ok(uri_list) = value.get::<String>() else {
                    log::warn!("Failed to get URI list from drop");
                    return false;
                };

                log::info!("Dropped URI list: {}", uri_list);
                let dropped_paths = parse_uri_list(&uri_list);
                log::info!("Parsed {} paths from URI list", dropped_paths.len());
                inner.handle_dropped_files(dropped_paths)
            });
        }

        self.scroll.add_controller(drop_target_files);
        self.scroll.add_controller(drop_target_text);
        log::info!("Drag & drop handlers installed on sound list");
    }

    fn handle_dropped_files(self: &Arc<Self>, paths: Vec<String>) -> bool {
        if paths.is_empty() {
            log::warn!("No paths to import");
            return false;
        }

        let tab_id = self.active_tab_id.lock().clone();
        log::info!("Importing to tab: {}", tab_id);

        let tab_id_opt = if tab_id == crate::app_meta::GENERAL_TAB_ID {
            None
        } else {
            Some(tab_id.clone())
        };

        let inner_weak = Arc::downgrade(self);
        match commands::import_files_to_tab_async(
            paths,
            tab_id_opt,
            Arc::clone(&self.state.config),
            self.state.loudness_coordinators.clone(),
            move |result| match result {
                Ok(new_sounds) => {
                    log::info!("Successfully imported {} sounds", new_sounds.len());
                    if !new_sounds.is_empty() {
                        if let Some(inner) = inner_weak.upgrade() {
                            inner.refresh_from_state_inner();
                            inner.emit_library_changed();
                        }
                    }
                }
                Err(e) => log::warn!("Drop import failed: {e}"),
            },
        ) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("Failed to dispatch drop import: {e}");
                false
            }
        }
    }

    pub(super) fn install_context_menu(self: &Arc<Self>, widget: &impl IsA<gtk4::Widget>) {
        let gesture = GestureClick::new();
        gesture.set_button(3);
        gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
        gesture.connect_pressed(|gesture, _, _, _| {
            // Keep the selection when opening the context menu.
            gesture.set_state(gtk4::EventSequenceState::Claimed);
        });

        let inner_weak = Arc::downgrade(self);
        gesture.connect_released(move |gesture, _, x, y| {
            let Some(inner) = inner_weak.upgrade() else {
                return;
            };
            let Some(widget) = gesture.widget() else {
                return;
            };
            let sound_id = widget.widget_name().to_string();
            if sound_id.is_empty() {
                return;
            }
            inner.show_context_menu_for_sound_id(&widget, x, y, &sound_id);
        });

        widget.as_ref().add_controller(gesture);
    }

    pub(super) fn install_drag_source(self: &Arc<Self>, widget: &impl IsA<gtk4::Widget>) {
        let drag_source = DragSource::new();
        drag_source.set_actions(gtk4::gdk::DragAction::COPY);
        drag_source.set_button(gtk4::gdk::BUTTON_PRIMARY);
        drag_source.set_propagation_phase(gtk4::PropagationPhase::Capture);
        drag_source.set_propagation_limit(gtk4::PropagationLimit::SameNative);
        // Leave the sequence shared so normal row selection still works.
        drag_source.set_exclusive(false);

        let inner_weak = Arc::downgrade(self);
        drag_source.connect_prepare(move |source, _, _| {
            let inner = inner_weak.upgrade()?;
            let _ = source.set_state(gtk4::EventSequenceState::Claimed);
            let widget = source.widget()?;

            let sound_id = widget.widget_name().to_string();
            if sound_id.trim().is_empty() {
                return None;
            }
            log::info!("Drag prepare: sound_id={}", sound_id);

            let selected_ids = inner.selected_sound_ids();
            let sound_ids = if selected_ids.iter().any(|id| id == &sound_id) {
                selected_ids
            } else {
                vec![sound_id]
            };
            log::info!(
                "Drag prepare: dragging {} sound(s): {:?}",
                sound_ids.len(),
                sound_ids
            );

            let payload = tab_dnd::SoundTabDragPayload {
                source_tab_id: inner.active_tab_id.lock().clone(),
                sound_ids: sound_ids.clone(),
            }
            .normalized();

            if payload.is_none() {
                log::warn!("Drag prepare: payload normalization failed");
                return None;
            }
            let payload = payload?;
            log::info!("Drag prepare: payload created successfully");

            let count = sound_ids.len();
            let icon_text = if count == 1 {
                "1 sound".to_string()
            } else {
                format!("{} sounds", count)
            };
            let drag_label = gtk4::Label::new(Some(&icon_text));
            drag_label.add_css_class("drag-icon-label");
            let paintable = gtk4::WidgetPaintable::new(Some(&drag_label));
            source.set_icon(Some(&paintable), 0, 0);

            let bytes = tab_dnd::encode_drag_payload(&payload)?;
            let providers = [
                gtk4::gdk::ContentProvider::for_value(&bytes.to_value()),
                gtk4::gdk::ContentProvider::for_bytes(tab_dnd::SOUND_TAB_DND_MIME, &bytes),
            ];
            Some(gtk4::gdk::ContentProvider::new_union(&providers))
        });

        drag_source.connect_drag_begin(move |_, drag| {
            drag.set_actions(gtk4::gdk::DragAction::COPY);
            log::debug!(
                "Sound drag begin: actions={:?} selected={:?} formats={}",
                drag.actions(),
                drag.selected_action(),
                drag.formats()
            );

            drag.connect_selected_action_notify(|drag| {
                log::debug!(
                    "Sound drag selected-action changed: {:?}",
                    drag.selected_action()
                );
            });
            drag.connect_drop_performed(|drag| {
                log::debug!(
                    "Sound drag drop-performed: selected={:?}",
                    drag.selected_action()
                );
            });
            drag.connect_dnd_finished(|drag| {
                log::debug!("Sound drag finished: selected={:?}", drag.selected_action());
            });
            drag.connect_cancel(|drag, reason| {
                log::debug!(
                    "Sound drag cancelled: reason={:?} selected={:?}",
                    reason,
                    drag.selected_action()
                );
            });
        });

        widget.as_ref().add_controller(drag_source);
    }

    fn show_context_menu_for_sound_id(
        self: &Arc<Self>,
        widget: &Widget,
        x: f64,
        y: f64,
        sound_id: &str,
    ) {
        let sound = self.lookup_sound(sound_id);

        if let Some(sound) = sound {
            self.show_context_menu(widget, x, y, sound);
        }
    }

    fn show_context_menu(
        self: &Arc<Self>,
        widget: &Widget,
        x: f64,
        y: f64,
        sound: crate::config::Sound,
    ) {
        let active_tab = self.active_tab_id.lock().clone();
        let tabs = self.state.config.lock().tabs.clone();

        let menu_model = gio::Menu::new();
        let selected_ids = self.selected_sound_ids();
        let target_ids = if selected_ids.len() > 1 && selected_ids.iter().any(|id| id == &sound.id)
        {
            selected_ids
        } else {
            vec![sound.id.clone()]
        };
        let target_count = target_ids.len();
        let display_name = if sound.name.chars().count() > 30 {
            format!("{}…", sound.name.chars().take(29).collect::<String>())
        } else {
            sound.name.clone()
        };

        let section1 = gio::Menu::new();
        section1.append(Some("Rename"), Some("sound-ctx.rename"));
        section1.append(
            Some(if sound.hotkey.is_some() {
                "Update Hotkey"
            } else {
                "Set Hotkey"
            }),
            Some("sound-ctx.set-hotkey"),
        );
        section1.append(Some("Check file path"), Some("sound-ctx.check-path"));
        section1.append(
            Some(if target_count > 1 {
                "Refine Loudness (Selected)"
            } else {
                "Refine Loudness Now"
            }),
            Some("sound-ctx.refine-loudness"),
        );
        menu_model.append_section(Some(&display_name), &section1);

        let section2 = gio::Menu::new();
        if !tabs.is_empty() {
            let add_to_tab = gio::Menu::new();
            for tab in &tabs {
                add_to_tab.append(
                    Some(&tab.name),
                    Some(&format!("{SOUND_CONTEXT_NAMESPACE}.add-to-tab-{}", tab.id)),
                );
            }

            section2.append_submenu(Some("Add to Tab"), &add_to_tab);
        }
        if active_tab != GENERAL_TAB_ID {
            section2.append(Some("Remove from Tab"), Some("sound-ctx.remove-from-tab"));
        }
        if section2.n_items() > 0 {
            menu_model.append_section(None, &section2);
        }

        let destructive = gio::Menu::new();
        destructive.append(
            Some(if target_count > 1 {
                "Remove Selected"
            } else {
                "Remove"
            }),
            Some("sound-ctx.remove"),
        );
        menu_model.append_section(None, &destructive);

        let action_group = gio::SimpleActionGroup::new();

        {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let sound = sound.clone();
            let dialog_host = self.dialog_host.clone();
            let action = gio::SimpleAction::new("rename", None);
            action.connect_activate(move |_, _| {
                let inner_confirm = Arc::clone(&inner);
                let sound = sound.clone();
                let state_confirm = Arc::clone(&state);
                dialog_host.show_input(
                    "Rename Sound",
                    "Enter a new name:",
                    &sound.name,
                    "Rename",
                    move |new_name| match commands::rename_sound(
                        sound.id.clone(),
                        new_name,
                        Arc::clone(&state_confirm.config),
                    ) {
                        Ok(_) => inner_confirm.refresh_from_state_inner(),
                        Err(e) => log::warn!("Rename failed: {e}"),
                    },
                );
            });
            action_group.add_action(&action);
        }

        {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let sound = sound.clone();
            let dialog_host = self.dialog_host.clone();
            let action = gio::SimpleAction::new("set-hotkey", None);
            action.connect_activate(move |_, _| {
                let inner_confirm = Arc::clone(&inner);
                let sound = sound.clone();
                let state_confirm = Arc::clone(&state);
                let dialog_host_weak = dialog_host.downgrade();
                let config_for_capture = Arc::clone(&state.config);
                let hotkeys_for_capture = Arc::clone(&state.hotkeys);
                let sound_id_for_capture = sound.id.clone();
                dialog_host.show_hotkey_capture(
                    sound.hotkey.as_deref(),
                    move |hotkey| {
                        {
                            let cfg = config_for_capture.lock();
                            commands::validate_hotkey_available(
                                &cfg,
                                &sound_id_for_capture,
                                hotkey,
                            )
                            .map_err(|e| e.to_string())?;
                        }
                        hotkeys_for_capture
                            .lock()
                            .validate_hotkey_blocking(hotkey)
                            .map_err(|e| e.to_string())
                    },
                    move |hotkey| match commands::set_hotkey(
                        sound.id.clone(),
                        hotkey,
                        Arc::clone(&state_confirm.config),
                        Arc::clone(&state_confirm.hotkeys),
                    ) {
                        Ok(_) => inner_confirm.refresh_from_state_inner(),
                        Err(e) => {
                            log::warn!("Set hotkey failed: {e}");
                            let detail = e.to_string();
                            let message = crate::hotkeys::format_hotkey_error(&detail);
                            if let Some(dialog_host) = dialog_host_weak.upgrade() {
                                if crate::hotkeys::should_offer_swhkd_install(&detail) {
                                    dialog_host.show_hotkey_error_with_install_option(
                                        "Failed to Set Hotkey",
                                        &message,
                                        Arc::clone(&state_confirm.config),
                                        Arc::clone(&state_confirm.hotkeys),
                                    );
                                } else {
                                    dialog_host.show_error("Failed to Set Hotkey", &message);
                                }
                            }
                        }
                    },
                );
            });
            action_group.add_action(&action);
        }

        {
            let sound = sound.clone();
            let dialog_host = self.dialog_host.clone();
            let action = gio::SimpleAction::new("check-path", None);
            action.connect_activate(move |_, _| {
                let path = sound.source_path.as_deref().unwrap_or(&sound.path);
                dialog_host.show_path_info(&sound.name, path);
            });
            action_group.add_action(&action);
        }

        {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let target_ids = target_ids.clone();
            let dialog_host = self.dialog_host.clone();
            let action = gio::SimpleAction::new("refine-loudness", None);
            action.connect_activate(move |_, _| {
                for sound_id in &target_ids {
                    let inner_done = Arc::clone(&inner);
                    let dialog_host = dialog_host.downgrade();
                    let sound_id_for_err = sound_id.clone();
                    if let Err(e) = commands::analyze_sound_loudness_async(
                        sound_id.clone(),
                        Arc::clone(&state.config),
                        move |result| match result {
                            Ok(_) => inner_done.refresh_from_state_inner(),
                            Err(err) => {
                                log::warn!(
                                    "Loudness refine failed for '{}': {}",
                                    sound_id_for_err,
                                    err
                                );
                                if let Some(dialog_host) = dialog_host.upgrade() {
                                    dialog_host
                                        .show_error("Loudness Refinement Failed", &err.to_string());
                                }
                            }
                        },
                    ) {
                        log::warn!(
                            "Failed to dispatch loudness refinement for '{}': {}",
                            sound_id,
                            e
                        );
                    }
                }
            });
            action_group.add_action(&action);
        }

        for tab in &tabs {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let sound_ids = target_ids.clone();
            let tab_id = tab.id.clone();
            let action_name = format!("add-to-tab-{}", tab.id);
            let action = gio::SimpleAction::new(&action_name, None);
            action.connect_activate(move |_, _| {
                match commands::add_sounds_to_tab(
                    tab_id.clone(),
                    sound_ids.clone(),
                    Arc::clone(&state.config),
                ) {
                    Ok(_) => {
                        inner.refresh_from_state_inner();
                        inner.emit_library_changed();
                    }
                    Err(e) => log::warn!("Add to tab failed: {e}"),
                }
            });
            action_group.add_action(&action);
        }

        if active_tab != GENERAL_TAB_ID {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let tab_id = active_tab.clone();
            let sound_ids = target_ids.clone();
            let action = gio::SimpleAction::new("remove-from-tab", None);
            action.connect_activate(move |_, _| {
                let mut any_success = false;
                for sound_id in &sound_ids {
                    match commands::remove_sound_from_tab(
                        tab_id.clone(),
                        sound_id.clone(),
                        Arc::clone(&state.config),
                    ) {
                        Ok(_) => any_success = true,
                        Err(e) => log::warn!("Remove from tab failed for {}: {e}", sound_id),
                    }
                }
                if any_success {
                    inner.refresh_from_state_inner();
                    inner.emit_library_changed();
                }
            });
            action_group.add_action(&action);
        }

        {
            let inner = Arc::clone(self);
            let target_ids = target_ids.clone();
            let action = gio::SimpleAction::new("remove", None);
            action.connect_activate(move |_, _| {
                inner.request_sound_removal(target_ids.clone());
            });
            action_group.add_action(&action);
        }

        menu::show_popover_menu(
            widget,
            SOUND_CONTEXT_NAMESPACE,
            &menu_model,
            &action_group,
            x,
            y,
        );
    }

    pub(super) fn selected_sound_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        let count = self.selection.n_items();
        for idx in 0..count {
            if !self.selection.is_selected(idx) {
                continue;
            }
            let Some(obj) = self
                .selection
                .item(idx)
                .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
            else {
                continue;
            };
            let sound = obj.borrow::<SoundRowData>();
            ids.push(sound.id.clone());
        }
        ids
    }

    fn request_sound_removal(self: &Arc<Self>, ids: Vec<String>) -> bool {
        if self.removal_pending.get() {
            return false;
        }

        let ids = normalize_sound_ids(ids);
        if ids.is_empty() {
            return false;
        }

        let count = ids.len();
        let inner = Arc::clone(self);
        let remove = move || inner.remove_sounds_now(ids.clone());
        if self.state.config.lock().settings.skip_delete_confirm {
            remove();
        } else {
            let message = removal_confirmation(count);
            self.dialog_host
                .show_confirm("Remove Sound", &message, "Remove", remove);
        }
        true
    }

    fn remove_sounds_now(self: &Arc<Self>, ids: Vec<String>) {
        if self.removal_pending.replace(true) {
            return;
        }

        let count = ids.len();
        let inner_weak = Arc::downgrade(self);
        if let Err(err) = commands::remove_sounds_async(
            ids.clone(),
            Arc::clone(&self.state.config),
            Arc::clone(&self.state.hotkeys),
            move |result| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                inner.removal_pending.set(false);
                match result {
                    Ok(()) => {
                        let mut invalid_ids = inner.invalid_ids.lock();
                        for id in &ids {
                            invalid_ids.remove(id);
                        }
                        drop(invalid_ids);
                        inner.refresh_from_state_inner();
                        inner.emit_library_changed();
                    }
                    Err(err) => {
                        log::warn!("Remove failed for {count} sound(s): {err}");
                        inner
                            .dialog_host
                            .show_error("Failed to Remove Sound", &err.to_string());
                    }
                }
            },
        ) {
            self.removal_pending.set(false);
            log::warn!("Failed to dispatch removal for {count} sound(s): {err}");
            self.dialog_host
                .show_error("Failed to Remove Sound", &err.to_string());
        }
    }
}

fn normalize_sound_ids(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

fn removal_confirmation(count: usize) -> String {
    if count == 1 {
        "Remove this sound from the soundboard? The audio file will remain on disk.".to_string()
    } else {
        format!(
            "Remove {count} selected sounds from the soundboard? The audio files will remain on disk."
        )
    }
}

fn parse_uri_list(uri_list: &str) -> Vec<String> {
    uri_list
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }

            if let Some(path) = line.strip_prefix("file://") {
                let decoded = percent_decode(path);
                return Some(decoded);
            }

            Some(line.to_string())
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                    continue;
                }
            }
            result.push('%');
            result.push_str(&hex);
        } else {
            result.push(ch);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_helpers_normalize_ids_and_build_prompts() {
        assert!(normalize_sound_ids(Vec::new()).is_empty());
        assert_eq!(
            normalize_sound_ids(vec!["one".into(), "".into(), "one".into(), "two".into()]),
            ["one", "two"]
        );
        assert_eq!(
            removal_confirmation(1),
            "Remove this sound from the soundboard? The audio file will remain on disk."
        );
        assert_eq!(
            removal_confirmation(2),
            "Remove 2 selected sounds from the soundboard? The audio files will remain on disk."
        );
    }

    #[test]
    fn remove_shortcut_accepts_only_unmodified_delete_keys() {
        use gtk4::gdk::{Key, ModifierType};

        assert!(is_unmodified_delete_shortcut(
            Key::Delete,
            ModifierType::empty()
        ));
        assert!(is_unmodified_delete_shortcut(
            Key::KP_Delete,
            ModifierType::LOCK_MASK
        ));
        assert!(!is_unmodified_delete_shortcut(
            Key::BackSpace,
            ModifierType::empty()
        ));
        assert!(!is_unmodified_delete_shortcut(
            Key::Delete,
            ModifierType::CONTROL_MASK
        ));
        assert!(!is_unmodified_delete_shortcut(
            Key::KP_Delete,
            ModifierType::SHIFT_MASK
        ));
    }
}
