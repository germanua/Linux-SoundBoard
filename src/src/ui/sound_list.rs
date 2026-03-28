//! Sound list widget — GtkColumnView with gio::ListStore model.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use gio::prelude::*;
use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, ColumnView, ColumnViewColumn, DragSource, GestureClick, Label, MultiSelection,
    Orientation, ScrolledWindow, SignalListItemFactory, Widget,
};

use crate::app_meta::GENERAL_TAB_ID;
use crate::app_state::AppState;
use crate::commands;
use crate::config::{ListStyle, Sound};

use super::menu;
use super::tab_dnd::{self, SoundTabDragPayload};

const SOUND_CONTEXT_NAMESPACE: &str = "sound-ctx";

#[derive(Debug, Clone)]
struct SoundRowData {
    id: String,
    name: String,
    duration_ms: Option<u64>,
    hotkey: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NavigationSound {
    pub id: String,
    pub name: String,
}

/// Format milliseconds as M:SS string.
fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The sound list widget bundle.
#[derive(Clone)]
pub struct SoundList {
    inner: Arc<SoundListInner>,
}

struct SoundListInner {
    scroll: ScrolledWindow,
    col_view: ColumnView,
    selection: MultiSelection,
    store: gio::ListStore,
    active_tab_id: Mutex<String>,
    search_query: Mutex<String>,
    playing_ids: Arc<Mutex<HashSet<String>>>,
    invalid_ids: Arc<Mutex<HashSet<String>>>,
    active_sound_id: Arc<Mutex<Option<String>>>,
    state: Arc<AppState>,
    on_library_changed: RefCell<Option<Box<dyn Fn() + 'static>>>,
    visible_row_indices: RefCell<HashMap<String, u32>>,
}

// Safety: SoundListInner is only used on the GTK main thread.
unsafe impl Send for SoundListInner {}
unsafe impl Sync for SoundListInner {}

impl SoundList {
    pub fn new(state: Arc<AppState>) -> Self {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        let selection = MultiSelection::new(Some(store.clone()));
        let col_view = ColumnView::new(Some(selection.clone()));
        col_view.set_vexpand(true);
        col_view.set_hexpand(true);
        col_view.set_reorderable(false);
        // Enable rubberband selection (click and drag to select multiple rows).
        col_view.set_enable_rubberband(true);
        col_view.add_css_class("data-table");

        // Apply list style class based on config
        {
            if let Ok(cfg) = state.config.lock() {
                if cfg.settings.list_style == ListStyle::Card {
                    col_view.add_css_class("list-style-card");
                }
            } else {
                log::warn!("Config lock poisoned in SoundList::new");
            }
        }

        let scroll = ScrolledWindow::builder()
            .child(&col_view)
            .vexpand(true)
            .hexpand(true)
            .build();

        let inner = Arc::new(SoundListInner {
            scroll,
            col_view: col_view.clone(),
            selection,
            store: store.clone(),
            active_tab_id: Mutex::new(GENERAL_TAB_ID.to_string()),
            search_query: Mutex::new(String::new()),
            playing_ids: Arc::new(Mutex::new(HashSet::new())),
            invalid_ids: Arc::new(Mutex::new(HashSet::new())),
            active_sound_id: Arc::new(Mutex::new(None)),
            state,
            on_library_changed: RefCell::new(None),
            visible_row_indices: RefCell::new(HashMap::new()),
        });

        inner.configure_columns();
        inner.connect_activate();
        inner.setup_drag_drop();

        let sl = Self { inner };
        sl.refresh_from_state();
        sl
    }

    /// Return the GTK widget to embed in the window layout.
    pub fn widget(&self) -> &Widget {
        self.inner.scroll.upcast_ref()
    }

    /// Set the active tab — filters the visible sounds.
    pub fn set_active_tab(&self, tab_id: String) {
        if let Ok(mut id) = self.inner.active_tab_id.lock() {
            *id = tab_id;
        } else {
            log::warn!("active_tab_id lock poisoned in set_active_tab");
        }
        self.refresh_from_state();
    }

    /// Update which sound IDs are currently playing and refresh the list.
    pub fn set_playing_ids(&self, ids: HashSet<String>) {
        let changed_ids = {
            if let Ok(mut current) = self.inner.playing_ids.lock() {
                if *current != ids {
                    let changed_ids = current.symmetric_difference(&ids).cloned().collect();
                    *current = ids;
                    Some(changed_ids)
                } else {
                    None
                }
            } else {
                log::warn!("playing_ids lock poisoned in set_playing_ids");
                None
            }
        };
        if let Some(changed_ids) = changed_ids {
            self.inner.rebind_rows_for_ids(&changed_ids);
        }
    }

    /// Store which sound IDs have missing source files and refresh the list.
    pub fn set_invalid_ids(&self, ids: HashSet<String>) {
        let changed_ids = if let Ok(mut ids_set) = self.inner.invalid_ids.lock() {
            if *ids_set != ids {
                let changed_ids = ids_set.symmetric_difference(&ids).cloned().collect();
                *ids_set = ids;
                Some(changed_ids)
            } else {
                None
            }
        } else {
            log::warn!("invalid_ids lock poisoned in set_invalid_ids");
            None
        };
        if let Some(changed_ids) = changed_ids {
            self.inner.rebind_rows_for_ids(&changed_ids);
        }
    }

    /// Set the active sound ID (current transport track) and refresh if changed.
    pub fn set_active_sound_id(&self, id: Option<String>) {
        let changed_ids = {
            if let Ok(mut current) = self.inner.active_sound_id.lock() {
                if *current != id {
                    let mut changed_ids = HashSet::new();
                    if let Some(previous) = current.clone() {
                        changed_ids.insert(previous);
                    }
                    if let Some(next) = id.as_ref() {
                        changed_ids.insert(next.clone());
                    }
                    *current = id;
                    Some(changed_ids)
                } else {
                    None
                }
            } else {
                log::warn!("active_sound_id lock poisoned in set_active_sound_id");
                None
            }
        };
        if let Some(changed_ids) = changed_ids {
            self.inner.rebind_rows_for_ids(&changed_ids);
        }
    }

    /// Set the search filter query and refresh the displayed list.
    pub fn set_search_filter(&self, query: String) {
        let changed = if let Ok(mut q) = self.inner.search_query.lock() {
            if *q != query {
                *q = query;
                true
            } else {
                false
            }
        } else {
            log::warn!("search_query lock poisoned in set_search_filter");
            false
        };
        if changed {
            self.refresh_visible_rows();
        }
    }

    /// Reload sounds from AppState config.
    pub fn refresh_from_state(&self) {
        self.inner.refresh_from_state_inner();
    }

    /// Append newly imported sounds to the list.
    pub fn append_sounds(&self, _new_sounds: Vec<Sound>) {
        // New sounds are already in Config, just refresh the view
        self.refresh_from_state();
        self.inner.emit_library_changed();
    }

    /// Return the currently visible sounds in lightweight form for transport navigation.
    pub fn get_navigation_sounds(&self) -> Vec<NavigationSound> {
        self.inner.filtered_navigation_sounds_from_state()
    }

    /// Return the currently active tab id.
    pub fn active_tab_id(&self) -> String {
        self.inner
            .active_tab_id
            .lock()
            .map(|id| id.clone())
            .unwrap_or_else(|e| {
                log::warn!("active_tab_id lock poisoned in active_tab_id: {}", e);
                crate::app_meta::GENERAL_TAB_ID.to_string()
            })
    }

    fn refresh_visible_rows(&self) {
        self.inner.reload_store();
    }

    /// Register callback fired when sound membership or tab membership changes.
    pub fn connect_library_changed<F: Fn() + 'static>(&self, f: F) {
        *self.inner.on_library_changed.borrow_mut() = Some(Box::new(f));
    }

    pub fn cleanup(&self) {
        *self.inner.on_library_changed.borrow_mut() = None;
        self.inner.store.remove_all();
        self.inner.visible_row_indices.borrow_mut().clear();
    }

    /// Set the list style mode ("compact" or "card")
    pub fn set_list_style(&self, style: &str) {
        let cv = &self.inner.col_view;
        if ListStyle::from_str(style).unwrap_or_default() == ListStyle::Card {
            cv.remove_css_class("list-style-compact");
            cv.add_css_class("list-style-card");
        } else {
            cv.remove_css_class("list-style-card");
            cv.add_css_class("list-style-compact");
        }
    }
}

impl SoundListInner {
    fn configure_columns(self: &Arc<Self>) {
        self.col_view.append_column(&self.build_index_column());
        self.col_view.append_column(&self.build_name_column());
        self.col_view.append_column(&self.build_duration_column());
        self.col_view.append_column(&self.build_hotkey_column());
    }

    fn connect_activate(self: &Arc<Self>) {
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
            let is_invalid = invalid_ids
                .lock()
                .map(|ids| ids.contains(&sound.id))
                .unwrap_or_else(|e| {
                    log::warn!("invalid_ids lock poisoned: {}", e);
                    false
                });
            let is_missing_on_demand = if is_invalid {
                true
            } else {
                match commands::validate_single_source(
                    sound.id.clone(),
                    Arc::clone(&inner.state.config),
                ) {
                    Ok(is_available) => !is_available,
                    Err(e) => {
                        log::warn!(
                            "On-demand source validation failed for '{}': {}",
                            sound.id,
                            e
                        );
                        false
                    }
                }
            };

            if !is_missing_on_demand {
                if let Err(e) = commands::play_sound(
                    sound.id.clone(),
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                ) {
                    log::warn!("Play failed for '{}': {}", sound.name, e);
                }
                return;
            }

            if !is_invalid {
                if let Ok(mut ids) = invalid_ids.lock() {
                    ids.insert(sound.id.clone());
                }
                let changed_ids = HashSet::from([sound.id.clone()]);
                inner.rebind_rows_for_ids(&changed_ids);
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
            let state_remove = Arc::clone(&inner.state);
            let invalid_locate = Arc::clone(&inner.invalid_ids);
            let invalid_remove = Arc::clone(&inner.invalid_ids);
            let inner_weak_locate = Arc::downgrade(&inner);
            let inner_weak_remove = Arc::downgrade(&inner);
            let sound_id_locate = sound.id.clone();
            let sound_id_remove = sound.id.clone();
            let win_locate = win.clone();

            crate::ui::dialogs::show_missing_file(
                &win,
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
                                match commands::update_sound_source(
                                    sound_id.clone(),
                                    new_path,
                                    Arc::clone(&state.config),
                                ) {
                                    Ok(_) => {
                                        if let Ok(mut ids) = invalid_ids.lock() {
                                            ids.remove(&sound_id);
                                        }
                                        if let Some(inner_refresh) = inner_weak_refresh.upgrade() {
                                            inner_refresh.refresh_from_state_inner();
                                        }
                                    }
                                    Err(e) => log::warn!("Update source failed: {e}"),
                                }
                            }
                        }
                    });
                },
                move || match commands::remove_sound(
                    sound_id_remove.clone(),
                    Arc::clone(&state_remove.config),
                    Arc::clone(&state_remove.hotkeys),
                ) {
                    Ok(_) => {
                        if let Ok(mut ids) = invalid_remove.lock() {
                            ids.remove(&sound_id_remove);
                        }
                        if let Some(inner_remove) = inner_weak_remove.upgrade() {
                            inner_remove.refresh_from_state_inner();
                            inner_remove.emit_library_changed();
                        }
                    }
                    Err(e) => log::warn!("Remove sound failed: {e}"),
                },
            );
        });
    }

    fn setup_drag_drop(self: &Arc<Self>) {
        // Setup drop target for files
        let drop_target_files = gtk4::DropTarget::new(
            gtk4::gdk::FileList::static_type(),
            gtk4::gdk::DragAction::COPY,
        );

        let drop_target_text =
            gtk4::DropTarget::new(glib::Type::STRING, gtk4::gdk::DragAction::COPY);

        // Handle file drops
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

        // Handle text/URI drops
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

    fn handle_dropped_files(&self, paths: Vec<String>) -> bool {
        if paths.is_empty() {
            log::warn!("No paths to import");
            return false;
        }

        // Get the current active tab
        let tab_id = self
            .active_tab_id
            .lock()
            .map(|id| id.clone())
            .unwrap_or_else(|e| {
                log::warn!("active_tab_id lock poisoned: {}", e);
                crate::app_meta::GENERAL_TAB_ID.to_string()
            });
        log::info!("Importing to tab: {}", tab_id);

        let tab_id_opt = if tab_id == crate::app_meta::GENERAL_TAB_ID {
            None
        } else {
            Some(tab_id.clone())
        };

        // Import files to the current tab
        match commands::import_files_to_tab(paths, tab_id_opt, Arc::clone(&self.state.config)) {
            Ok(new_sounds) => {
                log::info!("Successfully imported {} sounds", new_sounds.len());
                if !new_sounds.is_empty() {
                    self.refresh_from_state_inner();
                    self.emit_library_changed();
                }
                true
            }
            Err(e) => {
                log::warn!("Drop import failed: {e}");
                false
            }
        }
    }

    fn build_index_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Arc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let cell = GtkBox::new(Orientation::Horizontal, 0);
                cell.set_hexpand(true);
                cell.set_halign(gtk4::Align::Fill);
                cell.add_css_class("sound-cell");
                cell.add_css_class("sound-cell-first");
                let label = Label::builder()
                    .xalign(1.0)
                    .width_chars(3)
                    .css_classes(vec!["sound-index"])
                    .build();
                cell.append(&label);
                inner.install_context_menu(&cell);
                inner.install_drag_source(&cell);
                item.downcast_ref::<gtk4::ListItem>()
                    .unwrap()
                    .set_child(Some(&cell));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);
            factory.connect_bind(move |_, item| {
                let list_item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };
                let sound = obj.borrow::<SoundRowData>();
                let cell = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let label = cell.first_child().unwrap().downcast::<Label>().unwrap();
                label.set_text(&(list_item.position() + 1).to_string());
                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("playing_ids lock poisoned: {}", e);
                        false
                    });
                let is_active = active_sound_id
                    .lock()
                    .map(|id| id.as_deref() == Some(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("active_sound_id lock poisoned: {}", e);
                        false
                    });
                if is_playing {
                    cell.add_css_class("sound-cell-playing");
                } else {
                    cell.remove_css_class("sound-cell-playing");
                }
                if is_active {
                    cell.add_css_class("sound-cell-active");
                } else {
                    cell.remove_css_class("sound-cell-active");
                }
            });
        }

        let column = ColumnViewColumn::new(Some("#"), Some(factory));
        column.set_fixed_width(56);
        column
    }

    fn build_name_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Arc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let hbox = GtkBox::new(Orientation::Horizontal, 6);
                hbox.set_hexpand(true);
                hbox.add_css_class("sound-cell");
                let dot = Label::builder()
                    .label("●")
                    .css_classes(vec!["playing-dot"])
                    .visible(false)
                    .build();
                let label = Label::builder()
                    .xalign(0.0)
                    .css_classes(vec!["sound-name"])
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .hexpand(true)
                    .build();
                let warn = Label::builder()
                    .label("⚠")
                    .css_classes(vec!["warning-label"])
                    .visible(false)
                    .build();

                hbox.append(&dot);
                hbox.append(&label);
                hbox.append(&warn);
                inner.install_context_menu(&hbox);
                inner.install_drag_source(&hbox);

                item.downcast_ref::<gtk4::ListItem>()
                    .unwrap()
                    .set_child(Some(&hbox));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let invalid_ids = Arc::clone(&self.invalid_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);

            factory.connect_bind(move |_, item| {
                let list_item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };

                let sound = obj.borrow::<SoundRowData>();
                let hbox = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let dot = hbox.first_child().unwrap().downcast::<Label>().unwrap();
                let label = dot.next_sibling().unwrap().downcast::<Label>().unwrap();
                let warn = label.next_sibling().unwrap().downcast::<Label>().unwrap();
                let is_playing = playing_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("playing_ids lock poisoned: {}", e);
                        false
                    });
                let is_invalid = invalid_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("invalid_ids lock poisoned: {}", e);
                        false
                    });
                let is_active = active_sound_id
                    .lock()
                    .map(|id| id.as_deref() == Some(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("active_sound_id lock poisoned: {}", e);
                        false
                    });

                label.set_text(&sound.name);
                dot.set_visible(is_playing);
                warn.set_visible(is_invalid);
                hbox.set_widget_name(&sound.id);

                if is_playing {
                    hbox.add_css_class("sound-cell-playing");
                } else {
                    hbox.remove_css_class("sound-cell-playing");
                }

                if is_active {
                    hbox.add_css_class("sound-cell-active");
                } else {
                    hbox.remove_css_class("sound-cell-active");
                }
            });
        }

        let column = ColumnViewColumn::new(Some("NAME"), Some(factory));
        column.set_expand(true);
        column
    }

    fn build_duration_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Arc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let cell = GtkBox::new(Orientation::Horizontal, 0);
                cell.set_hexpand(true);
                cell.set_halign(gtk4::Align::Fill);
                cell.add_css_class("sound-cell");
                let label = Label::builder()
                    .xalign(0.0)
                    .hexpand(true)
                    .css_classes(vec!["sound-duration"])
                    .build();
                cell.append(&label);
                inner.install_context_menu(&cell);
                inner.install_drag_source(&cell);
                item.downcast_ref::<gtk4::ListItem>()
                    .unwrap()
                    .set_child(Some(&cell));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);
            factory.connect_bind(move |_, item| {
                let list_item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };
                let sound = obj.borrow::<SoundRowData>();
                let cell = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let label = cell.first_child().unwrap().downcast::<Label>().unwrap();
                label.set_text(
                    &sound
                        .duration_ms
                        .map(format_duration)
                        .unwrap_or_else(|| "\u{2014}".to_string()),
                );
                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("playing_ids lock poisoned: {}", e);
                        false
                    });
                let is_active = active_sound_id
                    .lock()
                    .map(|id| id.as_deref() == Some(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("active_sound_id lock poisoned: {}", e);
                        false
                    });
                if is_playing {
                    cell.add_css_class("sound-cell-playing");
                } else {
                    cell.remove_css_class("sound-cell-playing");
                }
                if is_active {
                    cell.add_css_class("sound-cell-active");
                } else {
                    cell.remove_css_class("sound-cell-active");
                }
            });
        }

        let column = ColumnViewColumn::new(Some("DURATION"), Some(factory));
        column.set_fixed_width(96);
        column
    }

    fn build_hotkey_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Arc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let cell = GtkBox::new(Orientation::Horizontal, 0);
                cell.set_hexpand(true);
                cell.set_halign(gtk4::Align::Fill);
                cell.add_css_class("sound-cell");
                let label = Label::builder().xalign(0.0).hexpand(true).build();
                cell.append(&label);
                inner.install_context_menu(&cell);
                inner.install_drag_source(&cell);
                item.downcast_ref::<gtk4::ListItem>()
                    .unwrap()
                    .set_child(Some(&cell));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);
            factory.connect_bind(move |_, item| {
                let list_item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };
                let sound = obj.borrow::<SoundRowData>();
                let cell = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let label = cell.first_child().unwrap().downcast::<Label>().unwrap();

                if let Some(hotkey) = &sound.hotkey {
                    label.set_text(hotkey);
                    label.add_css_class("hotkey-badge");
                    label.remove_css_class("dim-label");
                } else {
                    label.set_text("\u{2014}");
                    label.remove_css_class("hotkey-badge");
                    label.add_css_class("dim-label");
                }

                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("playing_ids lock poisoned: {}", e);
                        false
                    });
                let is_active = active_sound_id
                    .lock()
                    .map(|id| id.as_deref() == Some(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("active_sound_id lock poisoned: {}", e);
                        false
                    });
                if is_playing {
                    cell.add_css_class("sound-cell-playing");
                } else {
                    cell.remove_css_class("sound-cell-playing");
                }
                if is_active {
                    cell.add_css_class("sound-cell-active");
                } else {
                    cell.remove_css_class("sound-cell-active");
                }
            });
        }

        let column = ColumnViewColumn::new(Some("HOTKEY"), Some(factory));
        column.set_fixed_width(160);
        column
    }

    fn install_context_menu(self: &Arc<Self>, widget: &impl IsA<gtk4::Widget>) {
        let gesture = GestureClick::new();
        gesture.set_button(3);
        gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
        gesture.connect_pressed(|gesture, _, _, _| {
            // Keep existing multi-selection when opening context menu via right click.
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

    fn install_drag_source(self: &Arc<Self>, widget: &impl IsA<gtk4::Widget>) {
        let drag_source = DragSource::new();
        drag_source.set_actions(gtk4::gdk::DragAction::COPY);
        drag_source.set_button(gtk4::gdk::BUTTON_PRIMARY);
        drag_source.set_propagation_phase(gtk4::PropagationPhase::Capture);
        drag_source.set_propagation_limit(gtk4::PropagationLimit::SameNative);
        // Don't claim the event sequence exclusively so that single-clicks still
        // reach the ColumnView's MultiSelection gesture for normal row selection.
        drag_source.set_exclusive(false);

        let inner_weak = Arc::downgrade(self);
        drag_source.connect_prepare(move |source, _, _| {
            let Some(inner) = inner_weak.upgrade() else {
                return None;
            };
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

            let payload = SoundTabDragPayload {
                source_tab_id: inner
                    .active_tab_id
                    .lock()
                    .map(|id| id.clone())
                    .unwrap_or_else(|e| {
                        log::warn!("active_tab_id lock poisoned: {}", e);
                        crate::app_meta::GENERAL_TAB_ID.to_string()
                    }),
                sound_ids: sound_ids.clone(),
            }
            .normalized();

            if payload.is_none() {
                log::warn!("Drag prepare: payload normalization failed");
                return None;
            }
            let payload = payload?;
            log::info!("Drag prepare: payload created successfully");

            // Create drag icon showing sound count
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

    fn show_context_menu(self: &Arc<Self>, widget: &Widget, x: f64, y: f64, sound: Sound) {
        let Some(win) = widget
            .root()
            .and_then(|root| root.downcast::<gtk4::Window>().ok())
        else {
            return;
        };

        let active_tab = self
            .active_tab_id
            .lock()
            .map(|id| id.clone())
            .unwrap_or_else(|e| {
                log::warn!("active_tab_id lock poisoned: {}", e);
                crate::app_meta::GENERAL_TAB_ID.to_string()
            });
        let tabs = {
            if let Ok(cfg) = self.state.config.lock() {
                cfg.tabs.clone()
            } else {
                log::warn!("Config lock poisoned in show_context_menu");
                Vec::new()
            }
        };

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
                "Delete Selected"
            } else {
                "Delete"
            }),
            Some("sound-ctx.delete"),
        );
        menu_model.append_section(None, &destructive);

        let action_group = gio::SimpleActionGroup::new();

        {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let sound = sound.clone();
            let win = win.clone();
            let action = gio::SimpleAction::new("rename", None);
            action.connect_activate(move |_, _| {
                let inner_confirm = Arc::clone(&inner);
                let sound = sound.clone();
                let state_confirm = Arc::clone(&state);
                crate::ui::dialogs::show_input(
                    &win,
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
            let win = win.clone();
            let action = gio::SimpleAction::new("set-hotkey", None);
            action.connect_activate(move |_, _| {
                let inner_confirm = Arc::clone(&inner);
                let sound = sound.clone();
                let state_confirm = Arc::clone(&state);
                let error_window = win.clone();
                let hotkeys_for_capture = Arc::clone(&state.hotkeys);
                crate::ui::dialogs::show_hotkey_capture(
                    &win,
                    sound.hotkey.as_deref(),
                    move |hotkey| {
                        hotkeys_for_capture
                            .lock()
                            .map_err(|e| format!("Hotkeys lock poisoned: {}", e))?
                            .validate_hotkey_blocking(hotkey)
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
                            let message = crate::hotkeys::format_hotkey_error(&e);
                            crate::ui::dialogs::show_error(
                                &error_window,
                                "Failed to Set Hotkey",
                                &message,
                            );
                        }
                    },
                );
            });
            action_group.add_action(&action);
        }

        {
            let sound = sound.clone();
            let win = win.clone();
            let action = gio::SimpleAction::new("check-path", None);
            action.connect_activate(move |_, _| {
                let path = sound.source_path.as_deref().unwrap_or(&sound.path);
                crate::ui::dialogs::show_path_info(&win, &sound.name, path);
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
            let state = Arc::clone(&self.state);
            let sound = sound.clone();
            let target_ids = target_ids.clone();
            let win = win.clone();
            let action = gio::SimpleAction::new("delete", None);
            action.connect_activate(move |_, _| {
                let skip_confirm = state
                    .config
                    .lock()
                    .map(|cfg| cfg.settings.skip_delete_confirm)
                    .unwrap_or_else(|e| {
                        log::warn!("Config lock poisoned: {}", e);
                        false
                    });
                let inner_confirm = Arc::clone(&inner);
                let state_confirm = Arc::clone(&state);
                let sound_to_delete = sound.clone();
                let target_ids = target_ids.clone();
                let selection_count = target_ids.len();
                let target_ids_for_delete = target_ids.clone();

                let delete_sound = move || match commands::remove_sounds(
                    target_ids_for_delete.clone(),
                    Arc::clone(&state_confirm.config),
                    Arc::clone(&state_confirm.hotkeys),
                ) {
                    Ok(_) => {
                        inner_confirm.refresh_from_state_inner();
                        inner_confirm.emit_library_changed();
                    }
                    Err(e) => {
                        log::warn!("Delete failed for {} sound(s): {e}", selection_count);
                    }
                };

                if skip_confirm {
                    delete_sound();
                } else {
                    let message = if target_ids.len() > 1 {
                        format!(
                            "Delete {} selected sounds? This cannot be undone.",
                            target_ids.len()
                        )
                    } else {
                        format!("Delete '{}'? This cannot be undone.", sound_to_delete.name)
                    };
                    crate::ui::dialogs::show_confirm(
                        &win,
                        "Delete Sound",
                        &message,
                        "Delete",
                        delete_sound,
                    );
                }
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

    fn selected_sound_ids(&self) -> Vec<String> {
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

    fn reload_store(&self) {
        let filtered_rows = self.filtered_row_data_from_state();
        let boxed_rows = filtered_rows
            .into_iter()
            .map(BoxedAnyObject::new)
            .collect::<Vec<_>>();

        self.store.remove_all();
        for row in boxed_rows {
            self.store.append(&row);
        }
        self.rebuild_visible_row_indices();
    }

    fn current_store_rows(&self) -> Vec<SoundRowData> {
        (0..self.store.n_items())
            .filter_map(|position| {
                self.store
                    .item(position)
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                    .map(|obj| obj.borrow::<SoundRowData>().clone())
            })
            .collect()
    }

    fn replace_row_at(&self, position: u32, row: SoundRowData) {
        let replacements = [BoxedAnyObject::new(row)];
        self.store.splice(position, 1, &replacements);
    }

    fn rebuild_visible_row_indices(&self) {
        let mut indices = self.visible_row_indices.borrow_mut();
        indices.clear();

        for position in 0..self.store.n_items() {
            let Some(obj) = self
                .store
                .item(position)
                .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
            else {
                continue;
            };
            indices.insert(obj.borrow::<SoundRowData>().id.clone(), position);
        }
    }

    /// Rebind only rows whose transient state changed, leaving the store intact.
    fn rebind_rows_for_ids(&self, sound_ids: &HashSet<String>) {
        if sound_ids.is_empty() {
            return;
        }

        let mut changed_positions = Vec::new();
        {
            let indices = self.visible_row_indices.borrow();
            for sound_id in sound_ids {
                if let Some(position) = indices.get(sound_id) {
                    changed_positions.push(*position);
                }
            }
        }

        changed_positions.sort_unstable();
        changed_positions.dedup();
        for position in changed_positions {
            self.store.items_changed(position, 0, 0);
        }
    }

    fn filtered_navigation_sounds_from_state(&self) -> Vec<NavigationSound> {
        let tab_id = self.current_tab_id();
        let search_query = self.current_search_query();
        let cfg = match self.state.config.lock() {
            Ok(cfg) => cfg,
            Err(e) => {
                log::warn!("Config lock poisoned: {}", e);
                return Vec::new();
            }
        };

        let tab_sound_ids = Self::tab_sound_ids(&cfg, &tab_id);
        cfg.sounds
            .iter()
            .filter(|sound| Self::matches_filters(sound, &tab_sound_ids, &search_query))
            .map(|sound| NavigationSound {
                id: sound.id.clone(),
                name: sound.name.clone(),
            })
            .collect()
    }

    fn filtered_row_data_from_state(&self) -> Vec<SoundRowData> {
        let tab_id = self.current_tab_id();
        let search_query = self.current_search_query();
        let cfg = match self.state.config.lock() {
            Ok(cfg) => cfg,
            Err(e) => {
                log::warn!("Config lock poisoned: {}", e);
                return Vec::new();
            }
        };

        let tab_sound_ids = Self::tab_sound_ids(&cfg, &tab_id);
        cfg.sounds
            .iter()
            .filter(|sound| Self::matches_filters(sound, &tab_sound_ids, &search_query))
            .map(|sound| SoundRowData {
                id: sound.id.clone(),
                name: sound.name.clone(),
                duration_ms: sound.duration_ms,
                hotkey: sound.hotkey.clone(),
            })
            .collect()
    }

    /// Refresh the store from the current AppState config (called from action callbacks).
    fn refresh_from_state_inner(&self) {
        let filtered_rows = self.filtered_row_data_from_state();
        let current_rows = self.current_store_rows();

        if current_rows.len() != filtered_rows.len() {
            self.reload_store();
            return;
        }

        if current_rows
            .iter()
            .zip(filtered_rows.iter())
            .any(|(current, next)| current.id != next.id)
        {
            self.reload_store();
            return;
        }

        let mut changed = false;
        for (position, (current, next)) in current_rows.iter().zip(filtered_rows.iter()).enumerate()
        {
            if current.name != next.name
                || current.duration_ms != next.duration_ms
                || current.hotkey != next.hotkey
            {
                self.replace_row_at(position as u32, next.clone());
                changed = true;
            }
        }

        if changed || !current_rows.is_empty() || filtered_rows.is_empty() {
            self.rebuild_visible_row_indices();
        }
    }

    fn emit_library_changed(&self) {
        if let Some(ref cb) = *self.on_library_changed.borrow() {
            cb();
        }
    }

    fn lookup_sound(&self, sound_id: &str) -> Option<Sound> {
        match self.state.config.lock() {
            Ok(cfg) => cfg.get_sound(sound_id).cloned(),
            Err(e) => {
                log::warn!("Config lock poisoned in lookup_sound: {}", e);
                None
            }
        }
    }

    fn current_tab_id(&self) -> String {
        self.active_tab_id
            .lock()
            .map(|id| id.clone())
            .unwrap_or_else(|e| {
                log::warn!("active_tab_id lock poisoned: {}", e);
                GENERAL_TAB_ID.to_string()
            })
    }

    fn current_search_query(&self) -> String {
        self.search_query
            .lock()
            .map(|q| q.to_lowercase())
            .unwrap_or_else(|e| {
                log::warn!("search_query lock poisoned: {}", e);
                String::new()
            })
    }

    fn tab_sound_ids<'a>(cfg: &'a crate::config::Config, tab_id: &str) -> Option<HashSet<&'a str>> {
        if tab_id == GENERAL_TAB_ID {
            None
        } else {
            cfg.tabs
                .iter()
                .find(|tab| tab.id == tab_id)
                .map(|tab| tab.sound_ids.iter().map(String::as_str).collect())
        }
    }

    fn matches_filters(
        sound: &Sound,
        tab_sound_ids: &Option<HashSet<&str>>,
        search_query: &str,
    ) -> bool {
        let tab_match = tab_sound_ids
            .as_ref()
            .is_none_or(|sound_ids| sound_ids.contains(sound.id.as_str()));
        if !tab_match {
            return false;
        }

        search_query.is_empty() || sound.name.to_lowercase().contains(search_query)
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

            // Handle file:// URIs
            if let Some(path) = line.strip_prefix("file://") {
                // Simple URL decode for common cases
                let decoded = percent_decode(path);
                return Some(decoded);
            }

            // Return as-is if not a file:// URI
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
