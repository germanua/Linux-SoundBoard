use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};

use gio::prelude::*;
use glib::subclass::prelude::*;
use glib::BoxedAnyObject;

use crate::config::Sound;
use crate::library_store::{LibraryScope, LibraryStore, PAGE_SIZE};

use super::SoundRowData;

const MAX_CACHED_PAGES: usize = 4;
const MAX_CACHED_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
static NEXT_SEARCH_OWNER: AtomicU64 = AtomicU64::new(1);

fn row_payload_bytes(row: &SoundRowData) -> usize {
    let sound_bytes = row.sound.as_ref().map_or(0, |sound| {
        sound.id.capacity()
            + sound.name.capacity()
            + sound.path.capacity()
            + sound.source_path.as_ref().map_or(0, String::capacity)
            + sound.hotkey.as_ref().map_or(0, String::capacity)
            + sound
                .loudness_source_fingerprint
                .as_ref()
                .map_or(0, String::capacity)
    });
    std::mem::size_of::<SoundRowData>()
        + row.id.capacity()
        + row.name.capacity()
        + row.hotkey.as_ref().map_or(0, String::capacity)
        + sound_bytes
}

mod imp {
    use super::*;
    use gio::subclass::prelude::*;

    #[derive(Default)]
    pub struct PagedSoundModel {
        pub total: Cell<u32>,
        pub generation: Cell<u64>,
        pub search_owner: Cell<u64>,
        pub library: RefCell<Option<LibraryStore>>,
        pub scope: RefCell<Option<LibraryScope>>,
        pub search: RefCell<String>,
        pub pages: RefCell<HashMap<u32, Vec<BoxedAnyObject>>>,
        pub identities: RefCell<HashMap<(u64, u32), glib::WeakRef<BoxedAnyObject>>>,
        pub pending: RefCell<HashSet<u32>>,
        pub lru: RefCell<VecDeque<u32>>,
        pub page_payload_bytes: RefCell<HashMap<u32, usize>>,
        pub cached_payload_bytes: Cell<usize>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PagedSoundModel {
        const NAME: &'static str = "LinuxSoundboardPagedSoundModel";
        type Type = super::PagedSoundModel;
        type ParentType = glib::Object;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for PagedSoundModel {}

    impl ListModelImpl for PagedSoundModel {
        fn item_type(&self) -> glib::Type {
            BoxedAnyObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.total.get()
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            if position >= self.total.get() {
                return None;
            }
            let page = position / PAGE_SIZE as u32;
            self.obj().ensure_page(page);
            self.pages
                .borrow()
                .get(&page)
                .and_then(|rows| rows.get(position as usize % PAGE_SIZE))
                .cloned()
                .map(Into::into)
        }
    }
}

glib::wrapper! {
    pub struct PagedSoundModel(ObjectSubclass<imp::PagedSoundModel>)
        @implements gio::ListModel;
}

impl PagedSoundModel {
    pub(super) fn new(library: LibraryStore) -> Self {
        let model: Self = glib::Object::builder().build();
        model
            .imp()
            .search_owner
            .set(NEXT_SEARCH_OWNER.fetch_add(1, Ordering::Relaxed));
        *model.imp().library.borrow_mut() = Some(library);
        model
    }

    pub(super) fn reload(&self, scope: LibraryScope, search: String) {
        let imp = self.imp();
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        *imp.scope.borrow_mut() = Some(scope.clone());
        *imp.search.borrow_mut() = search.clone();
        let old_total = imp.total.replace(0);
        imp.pages.borrow_mut().clear();
        imp.identities.borrow_mut().clear();
        imp.pending.borrow_mut().clear();
        imp.lru.borrow_mut().clear();
        imp.page_payload_bytes.borrow_mut().clear();
        imp.cached_payload_bytes.set(0);
        if old_total > 0 {
            self.items_changed(0, old_total, 0);
        }

        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let response = library.count_coalesced(imp.search_owner.get(), generation, scope, &search);
        let weak = self.downgrade();
        if let Err(error) = crate::commands::dispatch_async_result(
            "count_lazy_sound_rows",
            move || response.recv(),
            move |result| {
                let Some(model) = weak.upgrade() else {
                    return;
                };
                if model.imp().generation.get() != generation {
                    return;
                }
                match result {
                    Ok(total) => {
                        let total = u32::try_from(total).unwrap_or(u32::MAX);
                        model.imp().total.set(total);
                        if total > 0 {
                            model.items_changed(0, 0, total);
                        }
                    }
                    Err(error) => {
                        log::warn!("Failed to count lazy sound rows: {error}");
                    }
                }
            },
        ) {
            log::warn!("Failed to dispatch lazy sound row count: {error}");
        }
    }

    fn ensure_page(&self, page: u32) {
        let imp = self.imp();
        if imp.pages.borrow().contains_key(&page) {
            self.touch_page(page);
            return;
        }

        let start = page.saturating_mul(PAGE_SIZE as u32);
        let remaining = imp.total.get().saturating_sub(start) as usize;
        let len = remaining.min(PAGE_SIZE);
        if len == 0 {
            return;
        }
        imp.identities
            .borrow_mut()
            .retain(|_, weak| weak.upgrade().is_some());
        let generation = imp.generation.get();
        let placeholders = (0..len)
            .map(|offset| self.placeholder(generation, start.saturating_add(offset as u32)))
            .collect();
        imp.pages.borrow_mut().insert(page, placeholders);
        self.set_page_payload(page, 0);
        self.touch_page(page);

        if !imp.pending.borrow_mut().insert(page) {
            return;
        }
        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let Some(scope) = imp.scope.borrow().clone() else {
            return;
        };
        let search = imp.search.borrow().clone();
        let response = library.page_coalesced(
            imp.search_owner.get(),
            generation,
            scope,
            &search,
            page as usize,
        );
        let weak = self.downgrade();
        if let Err(error) = crate::commands::dispatch_async_result(
            "load_lazy_sound_page",
            move || response.recv(),
            move |result| {
                let Some(model) = weak.upgrade() else {
                    return;
                };
                if model.imp().generation.get() != generation {
                    return;
                }
                match result {
                    Ok(result) => {
                        model.install_page(page, generation, result.sounds);
                    }
                    Err(error) => {
                        model.imp().pending.borrow_mut().remove(&page);
                        log::warn!("Failed to load lazy sound page {page}: {error}");
                    }
                }
            },
        ) {
            self.imp().pending.borrow_mut().remove(&page);
            log::warn!("Failed to dispatch lazy sound page {page}: {error}");
        }
    }

    fn placeholder(&self, generation: u64, position: u32) -> BoxedAnyObject {
        let key = (generation, position);
        if let Some(row) = self
            .imp()
            .identities
            .borrow()
            .get(&key)
            .and_then(glib::WeakRef::upgrade)
        {
            return row;
        }
        let row = BoxedAnyObject::new(SoundRowData {
            id: String::new(),
            name: "Loading…".to_string(),
            duration_ms: None,
            hotkey: None,
            sound: None,
        });
        self.imp()
            .identities
            .borrow_mut()
            .insert(key, row.downgrade());
        row
    }

    fn touch_page(&self, page: u32) {
        let imp = self.imp();
        let mut lru = imp.lru.borrow_mut();
        if let Some(position) = lru.iter().position(|cached| *cached == page) {
            lru.remove(position);
        }
        lru.push_back(page);
        while lru.len() > MAX_CACHED_PAGES
            || self.imp().cached_payload_bytes.get() > MAX_CACHED_PAYLOAD_BYTES
        {
            if let Some(evicted) = lru.pop_front() {
                self.evict_page(evicted);
            }
        }
    }

    fn set_page_payload(&self, page: u32, payload_bytes: usize) {
        let previous = self
            .imp()
            .page_payload_bytes
            .borrow_mut()
            .insert(page, payload_bytes)
            .unwrap_or(0);
        self.imp().cached_payload_bytes.set(
            self.imp()
                .cached_payload_bytes
                .get()
                .saturating_sub(previous)
                .saturating_add(payload_bytes),
        );
    }

    fn evict_page(&self, page: u32) {
        self.imp().pages.borrow_mut().remove(&page);
        self.imp().pending.borrow_mut().remove(&page);
        if let Some(payload) = self.imp().page_payload_bytes.borrow_mut().remove(&page) {
            self.imp().cached_payload_bytes.set(
                self.imp()
                    .cached_payload_bytes
                    .get()
                    .saturating_sub(payload),
            );
        }
    }

    fn install_page(&self, page: u32, generation: u64, sounds: Vec<Sound>) -> bool {
        let imp = self.imp();
        if imp.generation.get() != generation {
            return false;
        }
        imp.pending.borrow_mut().remove(&page);
        let mut pages = imp.pages.borrow_mut();
        let Some(objects) = pages.get_mut(&page) else {
            return false;
        };
        let changed = objects.len().min(sounds.len());
        for (offset, sound) in sounds.into_iter().take(changed).enumerate() {
            let object = BoxedAnyObject::new(SoundRowData {
                id: sound.id.clone(),
                name: sound.name.clone(),
                duration_ms: sound.duration_ms,
                hotkey: sound.hotkey.clone(),
                sound: Some(sound),
            });
            imp.identities.borrow_mut().insert(
                (
                    generation,
                    page.saturating_mul(PAGE_SIZE as u32)
                        .saturating_add(offset as u32),
                ),
                object.downgrade(),
            );
            objects[offset] = object;
        }
        let payload_bytes = objects
            .iter()
            .map(|object| row_payload_bytes(&object.borrow::<SoundRowData>()))
            .fold(0_usize, usize::saturating_add);
        drop(pages);
        self.set_page_payload(page, payload_bytes);
        self.touch_page(page);
        if changed > 0 {
            self.items_changed(
                page.saturating_mul(PAGE_SIZE as u32),
                changed as u32,
                changed as u32,
            );
        }
        true
    }

    pub(super) fn replace_at(&self, position: u32, row: SoundRowData) {
        let page = position / PAGE_SIZE as u32;
        let offset = position as usize % PAGE_SIZE;
        let payload_bytes = {
            let mut pages = self.imp().pages.borrow_mut();
            let Some(rows) = pages.get_mut(&page) else {
                return;
            };
            let Some(slot) = rows.get_mut(offset) else {
                return;
            };
            let object = BoxedAnyObject::new(row);
            self.imp()
                .identities
                .borrow_mut()
                .insert((self.imp().generation.get(), position), object.downgrade());
            *slot = object;
            rows.iter()
                .map(|object| row_payload_bytes(&object.borrow::<SoundRowData>()))
                .fold(0_usize, usize::saturating_add)
        };
        self.set_page_payload(page, payload_bytes);
        self.touch_page(page);
        self.items_changed(position, 1, 1);
    }

    pub(super) fn update_loaded_sound(&self, sound: Sound) -> bool {
        let Some(position) = self.position_for_id(&sound.id) else {
            return false;
        };
        self.replace_at(
            position,
            SoundRowData {
                id: sound.id.clone(),
                name: sound.name.clone(),
                duration_ms: sound.duration_ms,
                hotkey: sound.hotkey.clone(),
                sound: Some(sound),
            },
        );
        true
    }

    pub(super) fn position_for_id(&self, id: &str) -> Option<u32> {
        self.imp().pages.borrow().iter().find_map(|(page, rows)| {
            rows.iter().enumerate().find_map(|(offset, object)| {
                (object.borrow::<SoundRowData>().id == id)
                    .then_some(page * PAGE_SIZE as u32 + offset as u32)
            })
        })
    }

    pub(super) fn sound_by_id(&self, id: &str) -> Option<Sound> {
        self.imp()
            .identities
            .borrow()
            .values()
            .filter_map(glib::WeakRef::upgrade)
            .find_map(|object| {
                let row = object.borrow::<SoundRowData>();
                (row.id == id).then(|| row.sound.clone()).flatten()
            })
    }

    pub(super) fn clear(&self) {
        let old_total = self.imp().total.replace(0);
        self.imp().pages.borrow_mut().clear();
        self.imp().identities.borrow_mut().clear();
        self.imp().pending.borrow_mut().clear();
        self.imp().lru.borrow_mut().clear();
        self.imp().page_payload_bytes.borrow_mut().clear();
        self.imp().cached_payload_bytes.set(0);
        if old_total > 0 {
            self.items_changed(0, old_total, 0);
        }
    }

    #[cfg(test)]
    fn new_for_test(total: u32) -> Self {
        let model: Self = glib::Object::builder().build();
        model.imp().total.set(total);
        model
    }

    #[cfg(test)]
    fn install_test_page(&self, page: u32) {
        let generation = self.generation();
        let _ = self.install_test_page_for_generation(page, generation);
    }

    #[cfg(test)]
    fn install_test_sound(&self, position: u32, sound: Sound) {
        let page = position / PAGE_SIZE as u32;
        self.install_test_page(page);
        assert!(self.install_page(page, self.generation(), vec![sound]));
    }

    #[cfg(test)]
    fn install_test_page_for_generation(&self, page: u32, generation: u64) -> bool {
        if self.generation() != generation {
            return false;
        }
        let start = page.saturating_mul(PAGE_SIZE as u32);
        let len = self
            .imp()
            .total
            .get()
            .saturating_sub(start)
            .min(PAGE_SIZE as u32) as usize;
        self.imp().pages.borrow_mut().insert(
            page,
            (0..len)
                .map(|offset| {
                    self.placeholder(
                        generation,
                        page.saturating_mul(PAGE_SIZE as u32)
                            .saturating_add(offset as u32),
                    )
                })
                .collect(),
        );
        self.set_page_payload(page, 0);
        self.touch_page(page);
        true
    }

    #[cfg(test)]
    fn reset_for_test(&self, total: u32) {
        self.imp().generation.set(self.generation().wrapping_add(1));
        self.imp().total.set(total);
        self.imp().pages.borrow_mut().clear();
        self.imp().identities.borrow_mut().clear();
        self.imp().lru.borrow_mut().clear();
        self.imp().page_payload_bytes.borrow_mut().clear();
        self.imp().cached_payload_bytes.set(0);
    }

    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.imp().generation.get()
    }

    #[cfg(test)]
    fn cached_page_count(&self) -> usize {
        self.imp().pages.borrow().len()
    }

    #[cfg(test)]
    fn cached_object_count(&self) -> usize {
        self.imp().pages.borrow().values().map(Vec::len).sum()
    }
}

#[cfg(test)]
mod tests {
    use gio::prelude::ListModelExt;

    use super::{PagedSoundModel, SoundRowData};

    #[test]
    fn cache_never_keeps_more_than_four_pages() {
        let model = PagedSoundModel::new_for_test(10_000);
        for page in 0..10 {
            model.install_test_page(page);
        }
        assert!(model.cached_page_count() <= 4);
        assert!(model.cached_object_count() <= 4 * crate::library_store::PAGE_SIZE);
    }

    #[test]
    fn stale_generation_cannot_replace_current_rows() {
        let model = PagedSoundModel::new_for_test(512);
        let first_generation = model.generation();
        model.reset_for_test(512);
        assert!(!model.install_test_page_for_generation(0, first_generation));
        assert_eq!(model.cached_page_count(), 0);
    }

    #[test]
    fn loaded_page_replaces_placeholder_identity_for_gtk_rebind() {
        use gio::prelude::*;
        use glib::BoxedAnyObject;

        let model = PagedSoundModel::new_for_test(1);
        let placeholder = model.item(0).expect("placeholder row");
        let mut sound =
            crate::config::Sound::new("Loaded".to_string(), "/music/loaded.flac".to_string());
        sound.id = "loaded".to_string();

        assert!(model.install_page(0, model.generation(), vec![sound]));
        let loaded = model.item(0).expect("loaded row");

        assert_ne!(placeholder, loaded);
        assert_eq!(
            loaded
                .downcast::<BoxedAnyObject>()
                .expect("boxed row")
                .borrow::<SoundRowData>()
                .name,
            "Loaded"
        );
    }

    #[test]
    fn externally_referenced_row_keeps_identity_after_page_eviction() {
        use gio::prelude::*;

        let model = PagedSoundModel::new_for_test(10_000);
        let original = model.item(0).expect("first row");
        for page in 1..=4 {
            model.install_test_page(page);
        }
        let restored = model.item(0).expect("restored first row");

        assert_eq!(original, restored);
    }

    #[test]
    fn externally_referenced_loaded_row_remains_actionable_after_page_eviction() {
        use gio::prelude::*;

        let model = PagedSoundModel::new_for_test(10_000);
        let mut sound =
            crate::config::Sound::new("Retained".to_string(), "/music/retained.flac".to_string());
        sound.id = "retained".to_string();
        model.install_test_sound(0, sound);
        let retained = model.item(0).expect("retained row");

        for page in 1..=4 {
            model.install_test_page(page);
        }

        assert!(model.sound_by_id("retained").is_some());
        drop(retained);
    }

    #[test]
    fn strong_page_cache_stays_under_two_mebibytes_of_row_payload() {
        let model = PagedSoundModel::new_for_test(10_000);
        let large_path = format!("/music/{}", "x".repeat(1_200_000));

        for (page, id) in [(0, "first"), (1, "second")] {
            let mut sound = crate::config::Sound::new(id.to_string(), large_path.clone());
            sound.id = id.to_string();
            model.install_test_sound(page * crate::library_store::PAGE_SIZE as u32, sound);
        }

        assert_eq!(model.cached_page_count(), 1);
    }

    #[test]
    fn replacing_a_row_reapplies_the_payload_limit() {
        let model = PagedSoundModel::new_for_test(10_000);
        for (page, id) in [(0, "first"), (1, "second")] {
            let mut sound = crate::config::Sound::new(id.to_string(), format!("/music/{id}.flac"));
            sound.id = id.to_string();
            model.install_test_sound(page * crate::library_store::PAGE_SIZE as u32, sound);
        }

        let mut large = crate::config::Sound::new(
            "Large".to_string(),
            format!("/music/{}", "x".repeat(2_100_000)),
        );
        large.id = "first".to_string();
        model.replace_at(
            0,
            SoundRowData {
                id: large.id.clone(),
                name: large.name.clone(),
                duration_ms: large.duration_ms,
                hotkey: large.hotkey.clone(),
                sound: Some(large),
            },
        );

        assert!(model.cached_page_count() < 2);
    }

    #[test]
    fn updating_a_loaded_sound_keeps_the_list_published() {
        let model = PagedSoundModel::new_for_test(1);
        let mut sound =
            crate::config::Sound::new("Sound".to_string(), "/music/sound.flac".to_string());
        sound.id = "sound".to_string();
        model.install_test_sound(0, sound.clone());

        sound.hotkey = Some("Ctrl+1".to_string());

        assert!(model.update_loaded_sound(sound));
        assert_eq!(model.n_items(), 1);
        assert_eq!(
            model.sound_by_id("sound").and_then(|sound| sound.hotkey),
            Some("Ctrl+1".to_string())
        );
    }
}
