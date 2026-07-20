use std::collections::{HashMap, HashSet};

use gio::prelude::*;
use glib::BoxedAnyObject;

use crate::app_meta::GENERAL_TAB_ID;
use crate::config::Sound;

use super::{NavigationSound, SoundListInner, SoundRowData};

fn replace_store_rows(store: &gio::ListStore, rows: Vec<SoundRowData>) -> HashMap<String, u32> {
    let mut indices = HashMap::with_capacity(rows.len());
    let boxed_rows = rows
        .into_iter()
        .enumerate()
        .map(|(position, row)| {
            indices.insert(row.id.clone(), position as u32);
            BoxedAnyObject::new(row)
        })
        .collect::<Vec<_>>();

    store.splice(0, store.n_items(), &boxed_rows);
    indices
}

impl SoundListInner {
    pub(super) fn reload_store(&self) {
        let scroll_offsets = self.capture_scroll_offsets();
        let filtered_rows = self.filtered_row_data_from_state();
        *self.visible_row_indices.borrow_mut() = replace_store_rows(&self.store, filtered_rows);
        self.restore_scroll_offsets(scroll_offsets);
    }

    fn store_ids_match(&self, rows: &[SoundRowData]) -> bool {
        self.store.n_items() as usize == rows.len()
            && rows.iter().enumerate().all(|(position, row)| {
                self.store
                    .item(position as u32)
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                    .is_some_and(|obj| obj.borrow::<SoundRowData>().id == row.id)
            })
    }

    pub(super) fn replace_row_at(&self, position: u32, row: SoundRowData) {
        let replacements = [BoxedAnyObject::new(row)];
        self.store.splice(position, 1, &replacements);
    }

    pub(super) fn filtered_navigation_sounds_from_state(&self) -> Vec<NavigationSound> {
        let tab_id = self.current_tab_id();
        let search_query = self.current_search_query();
        let cfg = self.state.config.lock();

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

    pub(super) fn has_navigation_sounds_from_state(&self) -> bool {
        let tab_id = self.current_tab_id();
        let search_query = self.current_search_query();
        let cfg = self.state.config.lock();

        let tab_sound_ids = Self::tab_sound_ids(&cfg, &tab_id);
        cfg.sounds
            .iter()
            .any(|sound| Self::matches_filters(sound, &tab_sound_ids, &search_query))
    }

    pub(super) fn filtered_row_data_from_state(&self) -> Vec<SoundRowData> {
        let tab_id = self.current_tab_id();
        let search_query = self.current_search_query();
        let cfg = self.state.config.lock();

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

        if !self.store_ids_match(&filtered_rows) {
            let scroll_offsets = self.capture_scroll_offsets();
            *self.visible_row_indices.borrow_mut() = replace_store_rows(&self.store, filtered_rows);
            self.restore_scroll_offsets(scroll_offsets);
            return;
        }

        let scroll_offsets = self.capture_scroll_offsets();
        let mut changed = false;
        for (position, next) in filtered_rows.into_iter().enumerate() {
            let row_changed = self
                .store
                .item(position as u32)
                .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                .is_none_or(|obj| {
                    let current = obj.borrow::<SoundRowData>();
                    current.name != next.name
                        || current.duration_ms != next.duration_ms
                        || current.hotkey != next.hotkey
                });
            if row_changed {
                self.replace_row_at(position as u32, next);
                changed = true;
            }
        }

        if changed {
            self.restore_scroll_offsets(scroll_offsets);
        }
    }

    pub(super) fn emit_library_changed(&self) {
        if let Some(ref cb) = *self.on_library_changed.borrow() {
            cb();
        }
    }

    pub(super) fn lookup_sound(&self, sound_id: &str) -> Option<Sound> {
        self.state.config.lock().get_sound(sound_id).cloned()
    }

    pub(super) fn current_tab_id(&self) -> String {
        self.active_tab_id.lock().clone()
    }

    pub(super) fn current_search_query(&self) -> String {
        self.search_query.lock().to_lowercase()
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

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use super::*;

    fn row(id: &str) -> SoundRowData {
        SoundRowData {
            id: id.to_string(),
            name: id.to_string(),
            duration_ms: None,
            hotkey: None,
        }
    }

    #[test]
    fn bulk_replace_emits_one_items_changed_signal() {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        store.append(&BoxedAnyObject::new(row("old-a")));
        store.append(&BoxedAnyObject::new(row("old-b")));

        let notifications = Rc::new(Cell::new(0));
        let notifications_for_signal = Rc::clone(&notifications);
        store.connect_items_changed(move |_, _, _, _| {
            notifications_for_signal.set(notifications_for_signal.get() + 1);
        });

        replace_store_rows(&store, vec![row("new-a"), row("new-b"), row("new-c")]);

        assert_eq!(notifications.get(), 1);
    }

    #[test]
    fn bulk_replace_preserves_order_and_builds_indices() {
        let store = gio::ListStore::new::<BoxedAnyObject>();

        let indices = replace_store_rows(&store, vec![row("first"), row("second")]);
        let ids = (0..store.n_items())
            .map(|position| {
                store
                    .item(position)
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                    .expect("boxed row")
                    .borrow::<SoundRowData>()
                    .id
                    .clone()
            })
            .collect::<Vec<_>>();

        assert_eq!(ids, ["first", "second"]);
        assert_eq!(indices.get("first"), Some(&0));
        assert_eq!(indices.get("second"), Some(&1));
    }

    #[test]
    #[ignore = "release-scale gate for the 156,000-row General tab"]
    #[allow(clippy::print_stderr)]
    fn benchmark_large_bulk_replace() {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        let mut config = crate::config::Config::default();
        config.sounds.reserve(156_000);
        for index in 0..156_000 {
            let mut sound = Sound::new(
                format!("Звук {index:06} — Sound collection item"),
                format!(
                    "/home/test/Музика/Collection {:02}/Album {:03}/Disc {:02}/track-{index:06}.flac",
                    index % 24,
                    index % 512,
                    index % 4,
                ),
            );
            sound.id = format!("sound-{index:06}");
            sound.duration_ms = Some(180_000 + index as u64);
            config.sounds.push(sound);
        }
        let rows = config
            .sounds
            .iter()
            .map(|sound| SoundRowData {
                id: sound.id.clone(),
                name: sound.name.clone(),
                duration_ms: sound.duration_ms,
                hotkey: sound.hotkey.clone(),
            })
            .collect::<Vec<_>>();
        let notifications = Rc::new(Cell::new(0));
        let notifications_for_signal = Rc::clone(&notifications);
        store.connect_items_changed(move |_, _, _, _| {
            notifications_for_signal.set(notifications_for_signal.get() + 1);
        });

        let started = std::time::Instant::now();
        let indices = replace_store_rows(&store, rows);
        let elapsed = started.elapsed();
        let snapshot = crate::diagnostics::read_memory_snapshot()
            .expect("read process memory after replacing rows");

        eprintln!(
            "large-model rows={} replace_ms={} notifications={} pss_kib={}",
            store.n_items(),
            elapsed.as_millis(),
            notifications.get(),
            snapshot.pss_kb.unwrap_or_default(),
        );
        assert_eq!(store.n_items(), 156_000);
        assert_eq!(config.sounds.len(), 156_000);
        assert_eq!(indices.len(), 156_000);
        assert_eq!(notifications.get(), 1);
        assert!(elapsed <= std::time::Duration::from_millis(16));
        assert!(snapshot.pss_kb.is_some_and(|pss| pss < 102_400));
    }
}
