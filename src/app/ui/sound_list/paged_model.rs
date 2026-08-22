use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gio::prelude::*;
use glib::subclass::prelude::*;
use glib::BoxedAnyObject;

use crate::config::Sound;
use crate::library_store::{LibraryError, LibraryScope, LibraryStore, PAGE_SIZE};

use super::SoundRowData;

const MAX_CACHED_PAGES: usize = 4;
const MAX_CACHED_PAYLOAD_BYTES: usize = 2 * 1024 * 1024;
const MAX_PENDING_PAGE_LOADS: usize = 2;
const PAGE_LOAD_DEBOUNCE: Duration = Duration::from_millis(40);
const ROW_CHANGE_CHUNK: u32 = 64;
static NEXT_SEARCH_OWNER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageLoadPriority {
    Visible,
    Prefetch,
}

fn replacement_chunks(start: u32, len: u32) -> Vec<(u32, u32)> {
    (0..len)
        .step_by(ROW_CHANGE_CHUNK as usize)
        .map(|offset| {
            (
                start.saturating_add(offset),
                len.saturating_sub(offset).min(ROW_CHANGE_CHUNK),
            )
        })
        .collect()
}

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
        pub deferred_visible: RefCell<VecDeque<u32>>,
        pub load_source: RefCell<Option<glib::SourceId>>,
        pub failed: RefCell<HashSet<u32>>,
        pub lru: RefCell<VecDeque<u32>>,
        pub page_payload_bytes: RefCell<HashMap<u32, usize>>,
        pub cached_payload_bytes: Cell<usize>,
        pub reload_started: Cell<Option<Instant>>,
        pub first_page_generation: Cell<Option<u64>>,
        pub initial_page: RefCell<Option<(u32, Vec<Sound>)>>,
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
            self.pages
                .borrow()
                .get(&page)
                .and_then(|rows| rows.get(position as usize % PAGE_SIZE))
                .cloned()
                .or_else(|| Some(self.obj().placeholder(self.generation.get(), position)))
                .map(Into::into)
        }
    }
}

glib::wrapper! {
    pub struct PagedSoundModel(ObjectSubclass<imp::PagedSoundModel>)
        @implements gio::ListModel;
}

impl PagedSoundModel {
    pub(super) fn new(
        library: LibraryStore,
        initial_sound_count: usize,
        initial_sounds: Vec<Sound>,
    ) -> Self {
        let model: Self = glib::Object::builder().build();
        model
            .imp()
            .search_owner
            .set(NEXT_SEARCH_OWNER.fetch_add(1, Ordering::Relaxed));
        *model.imp().library.borrow_mut() = Some(library);
        model.imp().initial_page.replace(Some((
            u32::try_from(initial_sound_count).unwrap_or(u32::MAX),
            initial_sounds,
        )));
        model
    }

    pub(super) fn reload(&self, scope: LibraryScope, search: String) {
        let imp = self.imp();
        let generation = imp.generation.get().wrapping_add(1);
        imp.generation.set(generation);
        imp.reload_started.set(Some(Instant::now()));
        imp.first_page_generation.set(None);
        *imp.scope.borrow_mut() = Some(scope.clone());
        *imp.search.borrow_mut() = search.clone();
        let old_total = imp.total.replace(0);
        imp.pages.borrow_mut().clear();
        imp.identities.borrow_mut().clear();
        imp.pending.borrow_mut().clear();
        imp.deferred_visible.borrow_mut().clear();
        if let Some(source) = imp.load_source.borrow_mut().take() {
            source.remove();
        }
        imp.failed.borrow_mut().clear();
        imp.lru.borrow_mut().clear();
        imp.page_payload_bytes.borrow_mut().clear();
        imp.cached_payload_bytes.set(0);
        self.publish_cache_diagnostics();
        if old_total > 0 {
            self.items_changed(0, old_total, 0);
        }
        if matches!(scope, LibraryScope::General) && search.is_empty() {
            if let Some((total, sounds)) = imp.initial_page.borrow_mut().take() {
                let weak = self.downgrade();
                glib::idle_add_local_once(move || {
                    let Some(model) = weak.upgrade() else {
                        return;
                    };
                    if model.publish_initial_page(generation, total, sounds)
                        && total > PAGE_SIZE as u32
                    {
                        model.prefetch_adjacent(0);
                    }
                });
                return;
            }
        }

        let Some(library) = imp.library.borrow().clone() else {
            return;
        };

        // Reserve page 0 so the first-page fetch and the count fetch can race:
        // whoever lands first publishes readable rows, and the scroll path sees
        // page 0 as in flight instead of requesting it again.
        let placeholders = (0..PAGE_SIZE as u32)
            .map(|offset| self.placeholder(generation, offset))
            .collect();
        imp.pages.borrow_mut().insert(0, placeholders);
        self.set_page_payload(0, 0);
        self.touch_page(0);
        imp.pending.borrow_mut().insert(0);

        let page_response = library.page_coalesced(
            imp.search_owner.get(),
            generation,
            scope.clone(),
            &search,
            0,
        );
        let weak_page = self.downgrade();
        if let Err(error) = crate::commands::dispatch_async_result(
            "load_first_lazy_sound_page",
            move || page_response.recv(),
            move |result| {
                let Some(model) = weak_page.upgrade() else {
                    return;
                };
                if model.imp().generation.get() != generation {
                    return;
                }
                match result {
                    Ok(result) => {
                        if model.imp().total.get() == 0 {
                            model.imp().pending.borrow_mut().remove(&0);
                            let provisional_total =
                                u32::try_from(result.sounds.len()).unwrap_or(u32::MAX);
                            if model.publish_initial_page(
                                generation,
                                provisional_total,
                                result.sounds,
                            ) && provisional_total > PAGE_SIZE as u32
                            {
                                model.prefetch_adjacent(0);
                            }
                        } else {
                            let loaded = model.install_page(0, generation, result.sounds);
                            model.drain_deferred_pages();
                            if loaded && model.imp().deferred_visible.borrow().is_empty() {
                                model.prefetch_adjacent(0);
                            }
                        }
                    }
                    Err(LibraryError::QueueFull) => {
                        model.evict_page(0);
                    }
                    Err(error) => {
                        model.evict_page(0);
                        log::warn!("Failed to load first lazy sound page: {error}");
                    }
                }
            },
        ) {
            self.evict_page(0);
            log::warn!("Failed to dispatch first lazy sound page: {error}");
        }

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
                        if let Some(started) = model.imp().reload_started.get() {
                            log::debug!(
                                "Library latency: generation={} phase=count elapsed_us={} rows={}",
                                generation,
                                started.elapsed().as_micros(),
                                total
                            );
                        }
                        model.apply_exact_total(generation, total);
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

    fn ensure_page(&self, page: u32, priority: PageLoadPriority) {
        let imp = self.imp();
        if imp.pages.borrow().contains_key(&page) {
            self.touch_page(page);
            return;
        }
        if imp.pending.borrow().len() >= MAX_PENDING_PAGE_LOADS {
            if priority == PageLoadPriority::Visible {
                self.defer_visible_page(page);
            }
            return;
        }
        log::debug!(
            "Lazy page requested: generation={} page={} priority={priority:?}",
            imp.generation.get(),
            page
        );

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
        let response = match priority {
            PageLoadPriority::Visible => library.page_coalesced(
                imp.search_owner.get(),
                generation,
                scope,
                &search,
                page as usize,
            ),
            PageLoadPriority::Prefetch => library.prefetch_page_coalesced(
                imp.search_owner.get(),
                generation,
                scope,
                &search,
                page as usize,
            ),
        };
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
                        let loaded = model.install_page(page, generation, result.sounds);
                        model.drain_deferred_pages();
                        if loaded
                            && priority == PageLoadPriority::Visible
                            && model.imp().deferred_visible.borrow().is_empty()
                        {
                            model.prefetch_adjacent(page);
                        }
                    }
                    Err(LibraryError::QueueFull) => {
                        model.evict_page(page);
                        if priority == PageLoadPriority::Visible {
                            model.defer_visible_page(page);
                            model.schedule_deferred_pages();
                        }
                    }
                    Err(error) => {
                        model.handle_page_failure(page, generation, priority);
                        model.drain_deferred_pages();
                        log::warn!("Failed to load lazy sound page {page}: {error}");
                    }
                }
            },
        ) {
            self.handle_page_failure(page, generation, priority);
            log::warn!("Failed to dispatch lazy sound page {page}: {error}");
        }
    }

    pub(super) fn load_position(&self, position: u32) {
        let page = position / PAGE_SIZE as u32;
        if self.imp().pages.borrow().contains_key(&page)
            || self.imp().pending.borrow().contains(&page)
            || self.imp().deferred_visible.borrow().contains(&page)
        {
            return;
        }
        self.defer_visible_page(page);
        self.schedule_deferred_pages();
    }

    fn defer_visible_page(&self, page: u32) {
        let mut deferred = self.imp().deferred_visible.borrow_mut();
        if let Some(index) = deferred.iter().position(|queued| *queued == page) {
            deferred.remove(index);
        }
        deferred.push_back(page);
        while deferred.len() > MAX_PENDING_PAGE_LOADS {
            deferred.pop_front();
        }
    }

    fn drain_deferred_pages(&self) {
        if self.imp().load_source.borrow().is_some() {
            return;
        }
        while self.imp().pending.borrow().len() < MAX_PENDING_PAGE_LOADS {
            let Some(page) = self.imp().deferred_visible.borrow_mut().pop_front() else {
                break;
            };
            self.ensure_page(page, PageLoadPriority::Visible);
        }
    }

    fn schedule_deferred_pages(&self) {
        if let Some(source) = self.imp().load_source.borrow_mut().take() {
            source.remove();
        }
        let weak = self.downgrade();
        let source = glib::timeout_add_local_once(PAGE_LOAD_DEBOUNCE, move || {
            let Some(model) = weak.upgrade() else {
                return;
            };
            model.imp().load_source.borrow_mut().take();
            model.drain_deferred_pages();
        });
        *self.imp().load_source.borrow_mut() = Some(source);
    }

    fn prefetch_adjacent(&self, page: u32) {
        let page_count = self.imp().total.get().div_ceil(PAGE_SIZE as u32);
        if page + 1 < page_count {
            self.ensure_page(page + 1, PageLoadPriority::Prefetch);
        }
        if page > 0 {
            self.ensure_page(page - 1, PageLoadPriority::Prefetch);
        }
    }

    fn handle_page_failure(&self, page: u32, generation: u64, priority: PageLoadPriority) {
        if self.imp().generation.get() != generation {
            return;
        }
        match priority {
            PageLoadPriority::Visible => {
                self.fail_page(page, generation);
            }
            PageLoadPriority::Prefetch => {
                let start = page.saturating_mul(PAGE_SIZE as u32);
                let len = self
                    .imp()
                    .total
                    .get()
                    .saturating_sub(start)
                    .min(PAGE_SIZE as u32);
                self.evict_page(page);
                if len > 0 {
                    self.notify_replacements(generation, start, len, false);
                }
            }
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
        self.publish_cache_diagnostics();
    }

    fn evict_page(&self, page: u32) {
        self.imp().pages.borrow_mut().remove(&page);
        self.imp().pending.borrow_mut().remove(&page);
        self.imp().failed.borrow_mut().remove(&page);
        if let Some(payload) = self.imp().page_payload_bytes.borrow_mut().remove(&page) {
            self.imp().cached_payload_bytes.set(
                self.imp()
                    .cached_payload_bytes
                    .get()
                    .saturating_sub(payload),
            );
        }
        self.publish_cache_diagnostics();
    }

    /// Diagnostics runs on worker threads too, so the cache size is copied out
    /// rather than read back off this main-thread object.
    fn publish_cache_diagnostics(&self) {
        let imp = self.imp();
        let pages = imp.pages.borrow();
        let row_count = pages.values().map(|rows| rows.len()).sum();
        crate::diagnostics::memory::set_ui_row_cache(
            pages.len(),
            imp.cached_payload_bytes.get(),
            row_count,
        );
    }

    fn publish_initial_page(&self, generation: u64, total: u32, sounds: Vec<Sound>) -> bool {
        let imp = self.imp();
        if imp.generation.get() != generation || imp.total.get() != 0 {
            return false;
        }
        imp.total.set(total);
        let mut objects = Vec::with_capacity(sounds.len());
        for (position, sound) in sounds.into_iter().enumerate() {
            let object = BoxedAnyObject::new(SoundRowData {
                id: sound.id.clone(),
                name: sound.name.clone(),
                duration_ms: sound.duration_ms,
                hotkey: sound.hotkey.clone(),
                sound: Some(sound),
            });
            imp.identities
                .borrow_mut()
                .insert((generation, position as u32), object.downgrade());
            objects.push(object);
        }
        let payload_bytes = objects
            .iter()
            .map(|object| row_payload_bytes(&object.borrow::<SoundRowData>()))
            .fold(0_usize, usize::saturating_add);
        if !objects.is_empty() {
            imp.pages.borrow_mut().insert(0, objects);
            self.set_page_payload(0, payload_bytes);
            self.touch_page(0);
        }
        if total > 0 {
            self.items_changed(0, 0, total);
            imp.first_page_generation.set(Some(generation));
            if let Some(started) = imp.reload_started.get() {
                log::debug!(
                    "Library latency: generation={} phase=first_rows elapsed_us={} rows={}",
                    generation,
                    started.elapsed().as_micros(),
                    total.min(PAGE_SIZE as u32)
                );
            }
        }
        true
    }

    /// Reconcile a provisional total — from a first page that beat the exact
    /// count — with the real count once it lands.
    ///
    /// `total` is always exactly what GTK has been told, since every site that
    /// changes it emits the matching `items_changed` alongside. So the delta is
    /// precisely what GTK still needs, and announcing only the size change
    /// leaves the first page's row identities alone. Previous total 0 makes
    /// this a plain count announcement.
    fn apply_exact_total(&self, generation: u64, exact_total: u32) -> bool {
        let imp = self.imp();
        if imp.generation.get() != generation {
            return false;
        }
        let previous = imp.total.replace(exact_total);
        match exact_total.cmp(&previous) {
            std::cmp::Ordering::Greater => {
                self.items_changed(previous, 0, exact_total - previous);
            }
            std::cmp::Ordering::Less => {
                self.items_changed(exact_total, previous - exact_total, 0);
            }
            std::cmp::Ordering::Equal => {}
        }
        true
    }

    fn notify_replacements(&self, generation: u64, start: u32, len: u32, record_first_rows: bool) {
        // Never report past `total` — past what GTK has been told exists.
        // `reload` reserves page 0 with a full PAGE_SIZE before any count is
        // known, so unlike `ensure_page`'s pages that vector isn't bounded by
        // `total`. Exact count smaller than what the page query then returns
        // (independent queries, store can grow between them) and
        // `install_page`/`fail_page` would announce rows beyond it and trip the
        // list-item-manager assertion. Clamping here covers every caller.
        let len = len.min(self.imp().total.get().saturating_sub(start));
        if len == 0 {
            return;
        }
        let weak = self.downgrade();
        let mut chunks: VecDeque<_> = replacement_chunks(start, len).into();
        glib::idle_add_local(move || {
            let Some(model) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if model.imp().generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let Some((chunk_start, chunk_len)) = chunks.pop_front() else {
                return glib::ControlFlow::Break;
            };
            let notify_started = Instant::now();
            model.items_changed(chunk_start, chunk_len, chunk_len);
            log::debug!(
                "GTK row notification latency: start={} rows={} elapsed_us={}",
                chunk_start,
                chunk_len,
                notify_started.elapsed().as_micros()
            );
            if record_first_rows && model.imp().first_page_generation.get() != Some(generation) {
                model.imp().first_page_generation.set(Some(generation));
                if let Some(started) = model.imp().reload_started.get() {
                    log::debug!(
                        "Library latency: generation={} phase=first_rows elapsed_us={} rows={}",
                        generation,
                        started.elapsed().as_micros(),
                        chunk_len
                    );
                }
            }
            if chunks.is_empty() {
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
    }

    fn install_page(&self, page: u32, generation: u64, sounds: Vec<Sound>) -> bool {
        let imp = self.imp();
        if imp.generation.get() != generation {
            return false;
        }
        imp.pending.borrow_mut().remove(&page);
        imp.failed.borrow_mut().remove(&page);
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
            self.notify_replacements(
                generation,
                page.saturating_mul(PAGE_SIZE as u32),
                changed as u32,
                imp.first_page_generation.get() != Some(generation),
            );
        }
        true
    }

    fn fail_page(&self, page: u32, generation: u64) -> bool {
        let imp = self.imp();
        if imp.generation.get() != generation {
            return false;
        }
        imp.pending.borrow_mut().remove(&page);
        let start = page.saturating_mul(PAGE_SIZE as u32);
        let len = {
            let mut pages = imp.pages.borrow_mut();
            let Some(rows) = pages.get_mut(&page) else {
                return false;
            };
            for (offset, row) in rows.iter_mut().enumerate() {
                let object = BoxedAnyObject::new(SoundRowData {
                    id: String::new(),
                    name: "Load failed — activate to retry".to_string(),
                    duration_ms: None,
                    hotkey: None,
                    sound: None,
                });
                imp.identities.borrow_mut().insert(
                    (generation, start.saturating_add(offset as u32)),
                    object.downgrade(),
                );
                *row = object;
            }
            rows.len() as u32
        };
        imp.failed.borrow_mut().insert(page);
        self.set_page_payload(page, 0);
        self.touch_page(page);
        if len > 0 {
            self.notify_replacements(generation, start, len, false);
        }
        true
    }

    pub(super) fn retry_position(&self, position: u32) -> bool {
        let page = position / PAGE_SIZE as u32;
        if !self.imp().failed.borrow_mut().remove(&page) {
            return false;
        }
        let generation = self.imp().generation.get();
        let start = page.saturating_mul(PAGE_SIZE as u32);
        let len = self
            .imp()
            .total
            .get()
            .saturating_sub(start)
            .min(PAGE_SIZE as u32);
        self.evict_page(page);
        self.imp()
            .identities
            .borrow_mut()
            .retain(|(row_generation, position), _| {
                *row_generation != generation
                    || *position < start
                    || *position >= start.saturating_add(len)
            });
        self.ensure_page(page, PageLoadPriority::Visible);
        if len > 0 {
            self.notify_replacements(generation, start, len, false);
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
        self.imp().deferred_visible.borrow_mut().clear();
        if let Some(source) = self.imp().load_source.borrow_mut().take() {
            source.remove();
        }
        self.imp().failed.borrow_mut().clear();
        self.imp().lru.borrow_mut().clear();
        self.imp().page_payload_bytes.borrow_mut().clear();
        self.imp().cached_payload_bytes.set(0);
        self.publish_cache_diagnostics();
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
        self.imp().pending.borrow_mut().clear();
        self.imp().deferred_visible.borrow_mut().clear();
        if let Some(source) = self.imp().load_source.borrow_mut().take() {
            source.remove();
        }
        self.imp().failed.borrow_mut().clear();
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

    #[cfg(test)]
    fn deferred_visible_pages(&self) -> Vec<u32> {
        self.imp()
            .deferred_visible
            .borrow()
            .iter()
            .copied()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use gio::prelude::ListModelExt;

    use super::{replacement_chunks, PagedSoundModel, SoundRowData};

    #[test]
    fn loaded_page_notifications_are_split_into_frame_sized_chunks() {
        assert_eq!(
            replacement_chunks(256, 130),
            [(256, 64), (320, 64), (384, 2)]
        );
    }

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
    fn arbitrary_item_lookup_does_not_start_a_page_load() {
        use gio::prelude::*;

        let model = PagedSoundModel::new_for_test(10_000);
        assert!(model.item(9_999).is_some());
        assert_eq!(model.cached_page_count(), 0);
    }

    #[test]
    fn rapid_scroll_keeps_only_the_latest_two_deferred_pages() {
        let model = PagedSoundModel::new_for_test(156_000);
        model.defer_visible_page(2);
        model.defer_visible_page(300);
        model.defer_visible_page(609);

        assert_eq!(model.deferred_visible_pages(), [300, 609]);
        assert_eq!(model.cached_page_count(), 0);
    }

    #[test]
    fn unloaded_positions_have_distinct_gtk_identities() {
        use gio::prelude::*;

        let model = PagedSoundModel::new_for_test(2);
        assert_ne!(model.item(0), model.item(1));
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
        model.install_test_page(0);
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
    fn prepared_first_page_is_published_without_a_second_store_request() {
        use gio::prelude::*;
        use glib::BoxedAnyObject;

        let model = PagedSoundModel::new_for_test(0);
        let mut sound =
            crate::config::Sound::new("Prepared".to_string(), "/music/prepared.flac".to_string());
        sound.id = "prepared".to_string();

        assert!(model.publish_initial_page(model.generation(), 2, vec![sound]));
        assert_eq!(model.n_items(), 2);
        assert_eq!(
            model
                .item(0)
                .expect("prepared row")
                .downcast::<BoxedAnyObject>()
                .expect("boxed row")
                .borrow::<SoundRowData>()
                .name,
            "Prepared"
        );
    }

    // Do NOT pump `glib::MainContext::default()` from a test in this module.
    //
    // `idle_add_local` hardcodes the global default context in glib 0.20.12, so
    // a test can't redirect it to a private one. Tests here leave real non-Send
    // closures on that shared context, each pinned to whichever OS thread ran
    // it, and the first test to pump picks up someone else's and SIGABRTs the
    // binary: "Value accessed from different thread than where it was created".
    // Passes alone, kills the full run in order. Assert synchronously instead.

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

    #[test]
    fn provisional_first_page_is_readable_before_exact_count_arrives() {
        use gio::prelude::*;
        use glib::BoxedAnyObject;

        let model = PagedSoundModel::new_for_test(0);
        let mut sound = crate::config::Sound::new(
            "Provisional".to_string(),
            "/music/provisional.flac".to_string(),
        );
        sound.id = "provisional".to_string();

        assert!(model.publish_initial_page(model.generation(), 1, vec![sound]));
        assert_eq!(model.n_items(), 1);
        assert_eq!(
            model
                .item(0)
                .expect("provisional row")
                .downcast::<BoxedAnyObject>()
                .expect("boxed row")
                .borrow::<SoundRowData>()
                .name,
            "Provisional"
        );
    }

    #[test]
    fn exact_total_growing_after_provisional_page_preserves_row_identity() {
        use gio::prelude::*;

        let model = PagedSoundModel::new_for_test(0);
        let mut sound = crate::config::Sound::new(
            "Provisional".to_string(),
            "/music/provisional.flac".to_string(),
        );
        sound.id = "provisional".to_string();
        let generation = model.generation();

        assert!(model.publish_initial_page(generation, 1, vec![sound]));
        let before = model.item(0).expect("row before reconciliation");

        assert!(model.apply_exact_total(generation, 5));
        assert_eq!(model.n_items(), 5);

        let after = model.item(0).expect("row after reconciliation");
        assert_eq!(before, after);
    }

    #[test]
    fn exact_total_shrinking_after_provisional_page_shrinks_item_count() {
        let model = PagedSoundModel::new_for_test(0);
        let mut sound = crate::config::Sound::new(
            "Provisional".to_string(),
            "/music/provisional.flac".to_string(),
        );
        sound.id = "provisional".to_string();
        let generation = model.generation();

        assert!(model.publish_initial_page(generation, 5, vec![sound]));
        assert!(model.apply_exact_total(generation, 1));
        assert_eq!(model.n_items(), 1);
    }

    #[test]
    fn exact_total_matching_provisional_total_sends_no_notification() {
        let model = PagedSoundModel::new_for_test(0);
        let mut sound = crate::config::Sound::new(
            "Provisional".to_string(),
            "/music/provisional.flac".to_string(),
        );
        sound.id = "provisional".to_string();
        let generation = model.generation();

        assert!(model.publish_initial_page(generation, 1, vec![sound]));
        assert!(model.apply_exact_total(generation, 1));
        assert_eq!(model.n_items(), 1);
    }

    #[test]
    fn count_arriving_first_then_page_installs_without_duplicating_rows() {
        let model = PagedSoundModel::new_for_test(0);
        let generation = model.generation();

        assert!(model.apply_exact_total(generation, 1));
        assert_eq!(model.n_items(), 1);

        model.install_test_page(0);
        let mut sound =
            crate::config::Sound::new("Loaded".to_string(), "/music/loaded.flac".to_string());
        sound.id = "loaded".to_string();
        assert!(model.install_page(0, generation, vec![sound]));

        assert_eq!(model.n_items(), 1);
        assert_eq!(model.cached_object_count(), 1);
    }

    #[test]
    fn stale_generation_cannot_apply_exact_total() {
        let model = PagedSoundModel::new_for_test(0);
        let first_generation = model.generation();
        model.reset_for_test(3);

        assert!(!model.apply_exact_total(first_generation, 3));
        assert_eq!(model.n_items(), 3);
    }

    #[test]
    fn failed_page_can_be_retried_without_unpublishing_the_list() {
        use gio::prelude::*;
        use glib::BoxedAnyObject;

        let model = PagedSoundModel::new_for_test(1);
        let _ = model.item(0).expect("loading row");
        model.install_test_page(0);

        assert!(model.fail_page(0, model.generation()));
        assert_eq!(model.n_items(), 1);
        assert_eq!(
            model
                .item(0)
                .expect("failed row")
                .downcast::<BoxedAnyObject>()
                .expect("boxed row")
                .borrow::<SoundRowData>()
                .name,
            "Load failed — activate to retry"
        );

        assert!(model.retry_position(0));
        assert_eq!(model.n_items(), 1);
        assert_eq!(
            model
                .item(0)
                .expect("retry loading row")
                .downcast::<BoxedAnyObject>()
                .expect("boxed row")
                .borrow::<SoundRowData>()
                .name,
            "Loading…"
        );
    }
}
