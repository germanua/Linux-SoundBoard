use std::collections::HashSet;

use gio::prelude::*;
use glib::BoxedAnyObject;

use crate::app_meta::GENERAL_TAB_ID;
use crate::config::Sound;

use super::{NavigationSound, SoundListInner, SoundRowData};

impl SoundListInner {
    pub(super) fn reload_store(&self) {
        let scroll_offsets = self.capture_scroll_offsets();
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
        self.restore_scroll_offsets(scroll_offsets);
    }

    pub(super) fn current_store_rows(&self) -> Vec<SoundRowData> {
        (0..self.store.n_items())
            .filter_map(|position| {
                self.store
                    .item(position)
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                    .map(|obj| obj.borrow::<SoundRowData>().clone())
            })
            .collect()
    }

    pub(super) fn replace_row_at(&self, position: u32, row: SoundRowData) {
        let replacements = [BoxedAnyObject::new(row)];
        self.store.splice(position, 1, &replacements);
    }

    pub(super) fn rebuild_visible_row_indices(&self) {
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

    pub(super) fn filtered_navigation_sounds_from_state(&self) -> Vec<NavigationSound> {
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

    pub(super) fn filtered_row_data_from_state(&self) -> Vec<SoundRowData> {
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

    pub(super) fn refresh_from_state_inner(&self) {
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

        let scroll_offsets = self.capture_scroll_offsets();
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
            if changed {
                self.restore_scroll_offsets(scroll_offsets);
            }
        }
    }

    pub(super) fn emit_library_changed(&self) {
        if let Some(ref cb) = *self.on_library_changed.borrow() {
            cb();
        }
    }

    pub(super) fn lookup_sound(&self, sound_id: &str) -> Option<Sound> {
        match self.state.config.lock() {
            Ok(cfg) => cfg.get_sound(sound_id).cloned(),
            Err(e) => {
                log::warn!("Config lock poisoned in lookup_sound: {}", e);
                None
            }
        }
    }

    pub(super) fn current_tab_id(&self) -> String {
        self.active_tab_id
            .lock()
            .map(|id| id.clone())
            .unwrap_or_else(|e| {
                log::warn!("active_tab_id lock poisoned: {}", e);
                GENERAL_TAB_ID.to_string()
            })
    }

    pub(super) fn current_search_query(&self) -> String {
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
