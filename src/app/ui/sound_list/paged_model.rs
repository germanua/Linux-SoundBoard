use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

use gio::prelude::*;
use glib::subclass::prelude::*;
use glib::BoxedAnyObject;

use crate::config::Sound;
use crate::library_store::{LibraryScope, LibraryStore, PAGE_SIZE};

use super::SoundRowData;

const MAX_CACHED_PAGES: usize = 4;

mod imp {
    use super::*;
    use gio::subclass::prelude::*;

    #[derive(Default)]
    pub struct PagedSoundModel {
        pub total: Cell<u32>,
        pub generation: Cell<u64>,
        pub library: RefCell<Option<LibraryStore>>,
        pub scope: RefCell<Option<LibraryScope>>,
        pub search: RefCell<String>,
        pub pages: RefCell<HashMap<u32, Vec<BoxedAnyObject>>>,
        pub pending: RefCell<HashSet<u32>>,
        pub lru: RefCell<VecDeque<u32>>,
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
        imp.pending.borrow_mut().clear();
        imp.lru.borrow_mut().clear();
        if old_total > 0 {
            self.items_changed(0, old_total, 0);
        }

        let Some(library) = imp.library.borrow().clone() else {
            return;
        };
        let response = library.count(scope, &search);
        let weak = self.downgrade();
        glib::timeout_add_local(Duration::from_millis(2), move || {
            let Some(model) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            match response.try_recv() {
                Ok(Some(total)) => {
                    if model.imp().generation.get() == generation {
                        let total = u32::try_from(total).unwrap_or(u32::MAX);
                        model.imp().total.set(total);
                        if total > 0 {
                            model.items_changed(0, 0, total);
                        }
                    }
                    glib::ControlFlow::Break
                }
                Ok(None) => glib::ControlFlow::Continue,
                Err(error) => {
                    log::warn!("Failed to count lazy sound rows: {error}");
                    glib::ControlFlow::Break
                }
            }
        });
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
        let placeholders = (0..len)
            .map(|_| {
                BoxedAnyObject::new(SoundRowData {
                    id: String::new(),
                    name: "Loading…".to_string(),
                    duration_ms: None,
                    hotkey: None,
                })
            })
            .collect();
        imp.pages.borrow_mut().insert(page, placeholders);
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
        let generation = imp.generation.get();
        let response = library.page(scope, &search, page as usize);
        let weak = self.downgrade();
        glib::timeout_add_local(Duration::from_millis(2), move || {
            let Some(model) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            match response.try_recv() {
                Ok(Some(result)) => {
                    model.install_page(page, generation, result.sounds);
                    glib::ControlFlow::Break
                }
                Ok(None) => glib::ControlFlow::Continue,
                Err(error) => {
                    model.imp().pending.borrow_mut().remove(&page);
                    log::warn!("Failed to load lazy sound page {page}: {error}");
                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn touch_page(&self, page: u32) {
        let imp = self.imp();
        let mut lru = imp.lru.borrow_mut();
        if let Some(position) = lru.iter().position(|cached| *cached == page) {
            lru.remove(position);
        }
        lru.push_back(page);
        while lru.len() > MAX_CACHED_PAGES {
            if let Some(evicted) = lru.pop_front() {
                imp.pages.borrow_mut().remove(&evicted);
                imp.pending.borrow_mut().remove(&evicted);
            }
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
        for (object, sound) in objects.iter().zip(sounds) {
            *object.borrow_mut::<SoundRowData>() = SoundRowData {
                id: sound.id,
                name: sound.name,
                duration_ms: sound.duration_ms,
                hotkey: sound.hotkey,
            };
        }
        drop(pages);
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
        if let Some(object) = self
            .imp()
            .pages
            .borrow()
            .get(&page)
            .and_then(|rows| rows.get(offset))
        {
            *object.borrow_mut::<SoundRowData>() = row;
            self.items_changed(position, 1, 1);
        }
    }

    pub(super) fn position_for_id(&self, id: &str) -> Option<u32> {
        self.imp().pages.borrow().iter().find_map(|(page, rows)| {
            rows.iter().enumerate().find_map(|(offset, object)| {
                (object.borrow::<SoundRowData>().id == id)
                    .then_some(page * PAGE_SIZE as u32 + offset as u32)
            })
        })
    }

    pub(super) fn clear(&self) {
        let old_total = self.imp().total.replace(0);
        self.imp().pages.borrow_mut().clear();
        self.imp().pending.borrow_mut().clear();
        self.imp().lru.borrow_mut().clear();
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
                .map(|_| {
                    BoxedAnyObject::new(SoundRowData {
                        id: String::new(),
                        name: "Loading…".to_string(),
                        duration_ms: None,
                        hotkey: None,
                    })
                })
                .collect(),
        );
        self.touch_page(page);
        true
    }

    #[cfg(test)]
    fn reset_for_test(&self, total: u32) {
        self.imp().generation.set(self.generation().wrapping_add(1));
        self.imp().total.set(total);
        self.imp().pages.borrow_mut().clear();
        self.imp().lru.borrow_mut().clear();
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
    use super::PagedSoundModel;

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
}
