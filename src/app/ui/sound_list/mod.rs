use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::Arc;

use gio::prelude::*;
use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{ColumnView, MultiSelection, ScrolledWindow, Widget};

use crate::app_meta::GENERAL_TAB_ID;
use crate::app_state::AppState;
use crate::config::{ListStyle, Sound};

use super::dialogs::DialogHost;

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
    pub(super) dialog_host: DialogHost,
    pub(super) removal_pending: Cell<bool>,
    pub(super) on_library_changed: RefCell<Option<Box<dyn Fn() + 'static>>>,
    pub(super) visible_row_indices: RefCell<HashMap<String, u32>>,
}

// SAFETY: SoundListInner is only ever constructed, accessed, and dropped on
// the GTK main thread. The `RefCell` fields (`on_library_changed`,
// `visible_row_indices`) and all GTK widget fields are not thread-safe, but
// no code path ever sends this value to another thread. The `Arc<SoundListInner>`
// is held exclusively by GTK signal closures and the `SoundList` owner, all of
// which fire on the main thread. This invariant must never be violated — any
// future change that accesses `SoundListInner` from a background thread would
// produce UB.
unsafe impl Send for SoundListInner {}
// SAFETY: same as the Send impl above — all accesses are on the GTK main thread.
unsafe impl Sync for SoundListInner {}

impl SoundList {
    pub fn new(state: Arc<AppState>, dialog_host: DialogHost) -> Self {
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
            let cfg = state.config.lock();
            if cfg.settings.list_style == ListStyle::Card {
                col_view.add_css_class("list-style-card");
            } else {
                col_view.add_css_class("list-style-compact");
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
            dialog_host,
            removal_pending: Cell::new(false),
            on_library_changed: RefCell::new(None),
            visible_row_indices: RefCell::new(HashMap::new()),
        });

        inner.configure_columns();
        inner.connect_activate();
        inner.connect_remove_shortcut();
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
        *self.inner.active_tab_id.lock() = tab_id;
        self.refresh_from_state();
    }

    pub fn set_playing_ids(&self, ids: HashSet<String>) {
        let changed = {
            let mut current = self.inner.playing_ids.lock();
            if *current != ids {
                *current = ids;
                true
            } else {
                false
            }
        };
        if changed {
            self.inner.refresh_visible_sound_state();
        }
    }

    pub fn set_active_sound_id(&self, id: Option<String>) {
        let changed = {
            let mut current = self.inner.active_sound_id.lock();
            if *current != id {
                *current = id;
                true
            } else {
                false
            }
        };
        if changed {
            self.inner.refresh_visible_sound_state();
        }
    }

    pub fn set_search_filter(&self, query: String) {
        let changed = {
            let mut q = self.inner.search_query.lock();
            if *q != query {
                *q = query;
                true
            } else {
                false
            }
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

    pub fn has_navigation_sounds(&self) -> bool {
        self.inner.has_navigation_sounds_from_state()
    }

    pub fn active_tab_id(&self) -> String {
        self.inner.active_tab_id.lock().clone()
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
