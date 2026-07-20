#[cfg(test)]
use gio::prelude::*;
#[cfg(test)]
use glib::BoxedAnyObject;
#[cfg(test)]
use std::collections::HashMap;

use crate::app_meta::GENERAL_TAB_ID;
use crate::config::Sound;

use super::{SoundListInner, SoundRowData};

#[cfg(test)]
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
        let tab_id = self.current_tab_id();
        let scope = if tab_id == GENERAL_TAB_ID {
            crate::library_store::LibraryScope::General
        } else {
            crate::library_store::LibraryScope::ManualTab(tab_id)
        };
        self.store.reload(scope, self.current_search_query());
    }

    pub(super) fn replace_row_at(&self, position: u32, row: SoundRowData) {
        self.store.replace_at(position, row);
    }

    pub(super) fn refresh_from_state_inner(&self) {
        self.reload_store();
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
