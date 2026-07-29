#[cfg(test)]
use gio::prelude::*;
#[cfg(test)]
use glib::BoxedAnyObject;
#[cfg(test)]
use std::collections::HashMap;

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
        let scope = self.current_scope();
        self.store.reload(scope, self.current_search_query());
    }

    pub(super) fn replace_row_at(&self, position: u32, row: SoundRowData) {
        self.store.replace_at(position, row);
    }

    pub(super) fn update_loaded_sound(&self, sound: Sound) {
        self.store.update_loaded_sound(sound);
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
        self.store.sound_by_id(sound_id)
    }

    pub(super) fn current_scope(&self) -> crate::library_store::LibraryScope {
        self.active_scope.lock().clone()
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
            sound: None,
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
}
