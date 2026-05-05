use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use gio::prelude::*;
use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{ColumnView, MultiSelection, ScrolledWindow, Widget};

use crate::app_meta::GENERAL_TAB_ID;
use crate::app_state::AppState;
use crate::config::{ListStyle, Sound};

mod columns;
mod interaction;
mod model;
mod view_state;

pub(super) const SOUND_CONTEXT_NAMESPACE: &str = "sound-ctx";

#[derive(Debug, Clone)]
pub(super) struct SoundRowData {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) duration_ms: Option<u64>,
    pub(super) hotkey: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NavigationSound {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ScrollOffsets {
    pub(super) vertical: f64,
    pub(super) horizontal: f64,
}

#[derive(Clone)]
pub struct SoundList {
    pub(super) inner: Arc<SoundListInner>,
}

pub(super) struct SoundListInner {
    pub(super) scroll: ScrolledWindow,
    pub(super) col_view: ColumnView,
    pub(super) selection: MultiSelection,
    pub(super) store: gio::ListStore,
    pub(super) active_tab_id: Mutex<String>,
    pub(super) search_query: Mutex<String>,
    pub(super) playing_ids: Arc<Mutex<HashSet<String>>>,
    pub(super) invalid_ids: Arc<Mutex<HashSet<String>>>,
    pub(super) active_sound_id: Arc<Mutex<Option<String>>>,
    pub(super) state: Arc<AppState>,
    pub(super) on_library_changed: RefCell<Option<Box<dyn Fn() + 'static>>>,
    pub(super) visible_row_indices: RefCell<HashMap<String, u32>>,
}

// GTK main thread only.
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
        col_view.set_show_column_separators(false);
        col_view.set_show_row_separators(false);
        col_view.set_enable_rubberband(true);
        col_view.add_css_class("data-table");

        {
            if let Ok(cfg) = state.config.lock() {
                if cfg.settings.list_style == ListStyle::Card {
                    col_view.add_css_class("list-style-card");
                } else {
                    col_view.add_css_class("list-style-compact");
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

    pub fn widget(&self) -> &Widget {
        self.inner.scroll.upcast_ref()
    }

    fn sync_state_class(widget: &Widget, class_name: &str, enabled: bool) {
        if enabled {
            widget.add_css_class(class_name);
        } else {
            widget.remove_css_class(class_name);
        }
    }

    pub(super) fn sync_sound_state_classes(
        widget: &impl IsA<Widget>,
        is_playing: bool,
        is_active: bool,
    ) {
        let widget = widget.as_ref();

        Self::sync_state_class(widget, "sound-cell-playing", is_playing);
        Self::sync_state_class(widget, "sound-cell-active", is_active);

        // Mirror state onto the cell wrapper so CSS can paint full-width rows.
        if let Some(cell) = widget.parent() {
            Self::sync_state_class(&cell, "sound-cell-playing", is_playing);
            Self::sync_state_class(&cell, "sound-cell-active", is_active);
        }
    }

    pub fn set_active_tab(&self, tab_id: String) {
        if let Ok(mut id) = self.inner.active_tab_id.lock() {
            *id = tab_id;
        } else {
            log::warn!("active_tab_id lock poisoned in set_active_tab");
        }
        self.refresh_from_state();
    }

    pub fn set_playing_ids(&self, ids: HashSet<String>) {
        let changed = {
            if let Ok(mut current) = self.inner.playing_ids.lock() {
                if *current != ids {
                    *current = ids;
                    true
                } else {
                    false
                }
            } else {
                log::warn!("playing_ids lock poisoned in set_playing_ids");
                false
            }
        };
        if changed {
            self.inner.refresh_visible_sound_state();
        }
    }

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

    pub fn set_active_sound_id(&self, id: Option<String>) {
        let changed = {
            if let Ok(mut current) = self.inner.active_sound_id.lock() {
                if *current != id {
                    *current = id;
                    true
                } else {
                    false
                }
            } else {
                log::warn!("active_sound_id lock poisoned in set_active_sound_id");
                false
            }
        };
        if changed {
            self.inner.refresh_visible_sound_state();
        }
    }

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

    pub fn refresh_from_state(&self) {
        self.inner.refresh_from_state_inner();
    }

    pub fn append_sounds(&self, _new_sounds: Vec<Sound>) {
        self.refresh_from_state();
        self.inner.emit_library_changed();
    }

    pub fn get_navigation_sounds(&self) -> Vec<NavigationSound> {
        self.inner.filtered_navigation_sounds_from_state()
    }

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

    pub fn connect_library_changed<F: Fn() + 'static>(&self, f: F) {
        *self.inner.on_library_changed.borrow_mut() = Some(Box::new(f));
    }

    pub fn cleanup(&self) {
        *self.inner.on_library_changed.borrow_mut() = None;
        self.inner.store.remove_all();
        self.inner.visible_row_indices.borrow_mut().clear();
    }

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
