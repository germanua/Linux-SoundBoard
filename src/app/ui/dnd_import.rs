use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use glib;
use gtk4::prelude::*;
use gtk4::{ApplicationWindow, Box as GtkBox, Orientation};
use libadwaita as adw;

use crate::app_meta::GENERAL_TAB_ID;
use crate::app_state::AppState;
use crate::audio::scanner;
use crate::commands;

use super::icons;
use super::sound_list::SoundList;

#[derive(Clone, Copy)]
enum DropState {
    Hidden,
    Ready,
    Importing,
    Rejected,
}

pub fn build_and_attach_drop_overlay(
    window: &ApplicationWindow,
    toast_overlay: &adw::ToastOverlay,
    sound_list: &SoundList,
    state: &Arc<AppState>,
) -> gtk4::Overlay {
    let drop_target_files = gtk4::DropTarget::new(
        gtk4::gdk::FileList::static_type(),
        gtk4::gdk::DragAction::COPY,
    );
    let drop_target_text = gtk4::DropTarget::new(glib::Type::STRING, gtk4::gdk::DragAction::COPY);

    let drop_overlay = gtk4::Overlay::new();
    drop_overlay.set_child(Some(toast_overlay));

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

    let state_drop_files = Arc::clone(state);
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

    let state_drop_text = Arc::clone(state);
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

    drop_overlay
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

fn show_toast(overlay: &adw::ToastOverlay, message: &str) {
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
