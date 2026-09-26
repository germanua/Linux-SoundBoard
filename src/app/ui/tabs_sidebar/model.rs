#[derive(Clone)]
pub struct SidebarSelection {
    pub identity: String,
    pub scope: crate::library_store::LibraryScope,
}

pub type TabSelectedCallback = Box<dyn Fn(SidebarSelection) + 'static>;
pub type TabMembershipChangedCallback = Box<dyn Fn() + 'static>;

struct FolderNode {
    root_path: String,
    relative_path: Option<String>,
    name: Rc<RefCell<String>>,
    has_children: bool,
    children: RefCell<Option<gio::ListStore>>,
    children_requested: Rc<Cell<bool>>,
    expanded: Rc<Cell<bool>>,
    expanded_handler: RefCell<Option<(TreeListRow, glib::SignalHandlerId)>>,
    expansion_restored: Cell<bool>,
    disclosure_handlers: RefCell<Option<(Image, GestureClick, TreeListRow, glib::SignalHandlerId)>>,
    context_gesture: RefCell<Option<GestureClick>>,
    drop_target: RefCell<Option<gtk4::DropTargetAsync>>,
    drag_source: RefCell<Option<gtk4::DragSource>>,

    sibling_index: usize,
    sibling_pager: Option<std::rc::Weak<SiblingPager>>,
    children_pager: Rc<RefCell<Option<Rc<SiblingPager>>>>,
}

struct SiblingPager {
    library: crate::library_store::LibraryStore,
    children: gio::ListStore,
    root_path: String,
    parent_relative_path: Option<String>,
    loaded: Cell<usize>,
    next_page: Cell<usize>,
    has_more: Cell<bool>,
    in_flight: Cell<bool>,

    loaded_pages: RefCell<std::collections::BTreeSet<usize>>,

    focus_page: Cell<usize>,
    pending_pages: RefCell<std::collections::BTreeSet<usize>>,

    children_requested: Rc<Cell<bool>>,
}

const TAB_NAME_LABEL: &str = "tab-name-label";

impl SiblingPager {
    fn mark_reloadable(&self) {
        self.children_requested.set(false);
    }
}

const SIBLING_PREFETCH_MARGIN: usize = 32;

fn should_request_next_sibling_page(
    child_index: usize,
    loaded: usize,
    more: bool,
    in_flight: bool,
) -> bool {
    more && !in_flight && child_index + SIBLING_PREFETCH_MARGIN >= loaded
}

fn should_restore_expansion(node_expanded: bool, already_restored: bool) -> bool {
    node_expanded && !already_restored
}

const MAX_RETAINED_CHILD_ROWS: usize = 4_096;

fn count_loaded_child_rows(store: &gio::ListStore) -> usize {
    let mut total = 0usize;
    for item in store.iter::<BoxedAnyObject>().flatten() {
        total += 1;

        let Ok(node) = item.try_borrow::<FolderNode>() else {
            continue;
        };
        let children = node.loaded_children();
        drop(node);
        if let Some(children) = children {
            total += count_loaded_child_rows(&children);
        }
    }
    total
}

struct PlaceholderRow {
    sibling_index: usize,
    pager: std::rc::Weak<SiblingPager>,
}

const MAX_LOADED_SIBLING_PAGES: usize = 6;

fn page_to_evict(
    loaded_pages: &std::collections::BTreeSet<usize>,
    keep_near: usize,
    max_pages: usize,
) -> Option<usize> {
    if loaded_pages.len() <= max_pages {
        return None;
    }
    loaded_pages
        .iter()
        .copied()
        .max_by_key(|page| (page.abs_diff(keep_near), std::cmp::Reverse(*page)))
}

fn should_release_collapsed_children(total_retained_rows: usize, cap: usize) -> bool {
    total_retained_rows > cap
}

fn should_handle_expansion_change(previous: bool, next: bool) -> bool {
    previous != next
}

fn should_persist_expansion_change(changed: bool, rebuilding: bool) -> bool {
    changed && !rebuilding
}

fn folder_parent_relative_path(relative_path: &str) -> Option<String> {
    std::path::Path::new(relative_path)
        .parent()
        .filter(|parent| parent.components().next().is_some())
        .map(|parent| parent.to_string_lossy().into_owned())
}

fn folder_reorder_target_index(dragged_index: usize, target_index: usize, after: bool) -> usize {
    let raw = if after {
        target_index + 1
    } else {
        target_index
    };
    if dragged_index < raw {
        raw.saturating_sub(1)
    } else {
        raw
    }
}

fn update_disclosure_icon(image: &Image, expanded: bool) {
    icons::apply_image_icon(
        image,
        if expanded {
            icons::DISCLOSURE_OPEN
        } else {
            icons::DISCLOSURE_CLOSED
        },
    );
    image.set_tooltip_text(Some(if expanded {
        "Hide subfolders"
    } else {
        "Show subfolders"
    }));
}

impl FolderNode {
    fn children(&self) -> gio::ListStore {
        self.children
            .borrow_mut()
            .get_or_insert_with(gio::ListStore::new::<BoxedAnyObject>)
            .clone()
    }

    fn loaded_children(&self) -> Option<gio::ListStore> {
        self.children.borrow().clone()
    }

    fn root(path: String) -> Self {
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.clone());
        Self {
            root_path: path,
            relative_path: None,
            name: Rc::new(RefCell::new(name)),
            has_children: true,
            children: RefCell::new(Some(gio::ListStore::new::<BoxedAnyObject>())),
            children_requested: Rc::new(Cell::new(false)),
            expanded: Rc::new(Cell::new(true)),
            expanded_handler: RefCell::new(None),
            expansion_restored: Cell::new(false),
            disclosure_handlers: RefCell::new(None),
            context_gesture: RefCell::new(None),
            drop_target: RefCell::new(None),
            drag_source: RefCell::new(None),
            sibling_index: 0,
            sibling_pager: None,
            children_pager: Rc::new(RefCell::new(None)),
        }
    }

    #[cfg(test)]
    fn folder(root_path: String, item: crate::library_store::FolderItem) -> Self {
        Self::folder_at(root_path, item, 0, None)
    }

    fn folder_at(
        root_path: String,
        item: crate::library_store::FolderItem,
        sibling_index: usize,
        sibling_pager: Option<std::rc::Weak<SiblingPager>>,
    ) -> Self {
        Self {
            root_path,
            relative_path: Some(item.relative_path),
            name: Rc::new(RefCell::new(item.name)),
            has_children: item.has_children,
            children: RefCell::new(None),
            children_requested: Rc::new(Cell::new(false)),
            expanded: Rc::new(Cell::new(item.expanded)),
            expanded_handler: RefCell::new(None),
            expansion_restored: Cell::new(false),
            disclosure_handlers: RefCell::new(None),
            context_gesture: RefCell::new(None),
            drop_target: RefCell::new(None),
            drag_source: RefCell::new(None),
            sibling_index,
            sibling_pager,
            children_pager: Rc::new(RefCell::new(None)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SidebarDropIntent {
    Noop,
    AddToTarget,
    RemoveFromSource,
    MoveBetweenCustomTabs,
}

fn resolve_sidebar_drop_intent(source_tab_id: &str, target_tab_id: &str) -> SidebarDropIntent {
    if source_tab_id == target_tab_id {
        return SidebarDropIntent::Noop;
    }

    let source_is_general = source_tab_id == GENERAL_TAB_ID;
    let target_is_general = target_tab_id == GENERAL_TAB_ID;
    match (source_is_general, target_is_general) {
        (true, true) => SidebarDropIntent::Noop,
        (true, false) => SidebarDropIntent::AddToTarget,
        (false, true) => SidebarDropIntent::RemoveFromSource,
        (false, false) => SidebarDropIntent::MoveBetweenCustomTabs,
    }
}

fn drag_action_for_intent(intent: SidebarDropIntent) -> gtk4::gdk::DragAction {
    match intent {
        SidebarDropIntent::Noop => gtk4::gdk::DragAction::COPY,
        SidebarDropIntent::AddToTarget
        | SidebarDropIntent::RemoveFromSource
        | SidebarDropIntent::MoveBetweenCustomTabs => gtk4::gdk::DragAction::COPY,
    }
}

type FolderChangedCallback = Rc<RefCell<Option<Box<dyn Fn() + 'static>>>>;
type FolderMergeCallback = Rc<RefCell<Option<Box<dyn Fn(FolderMergeRequest) + 'static>>>>;

#[derive(Clone)]
struct FolderDropCallbacks {
    changed: FolderChangedCallback,

    reordered: FolderChangedCallback,

    merged: FolderMergeCallback,
}

fn tab_scope_key(tab_id: &str) -> String {
    crate::library_store::scope_key(&if tab_id == GENERAL_TAB_ID {
        crate::library_store::LibraryScope::General
    } else {
        crate::library_store::LibraryScope::ManualTab(tab_id.to_string())
    })
}

fn folder_drop_overrides(
    payload: &tab_dnd::SoundTabDragPayload,
    target: &tab_dnd::FolderDragContext,
) -> Vec<crate::library_store::FolderOverrideRecord> {
    if payload.source_folder.as_ref() == Some(target) {
        return Vec::new();
    }
    let mut overrides = Vec::with_capacity(payload.sound_ids.len().saturating_mul(
        if payload.source_folder.is_some() {
            2
        } else {
            1
        },
    ));
    for sound_id in &payload.sound_ids {
        overrides.push(crate::library_store::FolderOverrideRecord {
            root_path: target.root_path.clone(),
            folder_relative_path: target.relative_path.clone(),
            sound_public_id: sound_id.clone(),
            action: crate::library_store::FolderOverrideAction::Include,
        });
        if let Some(source) = &payload.source_folder {
            overrides.push(crate::library_store::FolderOverrideRecord {
                root_path: source.root_path.clone(),
                folder_relative_path: source.relative_path.clone(),
                sound_public_id: sound_id.clone(),
                action: crate::library_store::FolderOverrideAction::Exclude,
            });
        }
    }
    overrides
}

fn install_folder_drag_source(
    widget: &GtkBox,
    root_path: String,
    relative_path: String,
) -> gtk4::DragSource {
    let source = gtk4::DragSource::new();
    source.set_actions(gtk4::gdk::DragAction::COPY);
    source.connect_prepare(move |_, _, _| {
        let payload = tab_dnd::FolderDragPayload {
            root_path: root_path.clone(),
            relative_path: relative_path.clone(),
            parent_relative_path: folder_parent_relative_path(&relative_path),
        };
        let bytes = tab_dnd::encode_folder_drag(&payload)?;
        let providers = [
            gtk4::gdk::ContentProvider::for_value(&bytes.to_value()),
            gtk4::gdk::ContentProvider::for_bytes(tab_dnd::FOLDER_DND_MIME, &bytes),
        ];
        Some(gtk4::gdk::ContentProvider::new_union(&providers))
    });
    widget.add_controller(source.clone());
    source
}

#[derive(Debug, Clone)]
struct FolderMergeRequest {
    root_path: String,
    source_relative_path: String,
    destination_relative_path: String,
}

fn folder_merge_request(
    payload: &tab_dnd::FolderDragPayload,
    target_root_path: &str,
    target_relative_path: &str,
) -> Option<FolderMergeRequest> {
    if payload.root_path != target_root_path || payload.relative_path == target_relative_path {
        return None;
    }
    let inside_source = target_relative_path
        .strip_prefix(payload.relative_path.as_str())
        .is_some_and(|rest| rest.starts_with('/'));
    if inside_source {
        return None;
    }
    Some(FolderMergeRequest {
        root_path: payload.root_path.clone(),
        source_relative_path: payload.relative_path.clone(),
        destination_relative_path: target_relative_path.to_string(),
    })
}

fn folder_display_label(relative_path: &str) -> &str {
    relative_path
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .unwrap_or(relative_path)
}

fn collect_folder_sound_ids(
    library: &crate::library_store::LibraryStore,
    scope: crate::library_store::LibraryScope,
) -> Result<Vec<String>, crate::library_store::LibraryError> {
    let mut sound_ids = Vec::new();
    let mut page = 0;
    loop {
        let sounds = library.page(scope.clone(), "", page).recv()?.sounds;
        let is_last = sounds.len() < crate::library_store::PAGE_SIZE;
        sound_ids.extend(sounds.into_iter().map(|sound| sound.id));
        if is_last {
            return Ok(sound_ids);
        }
        page += 1;
    }
}

struct FolderDropTargetRow<'a> {
    root_path: &'a str,
    relative_path: &'a str,
    sibling_index: usize,
}

fn handle_folder_drop(
    bytes: &glib::Bytes,
    library: &crate::library_store::LibraryStore,
    row: FolderDropTargetRow<'_>,
    zone: tab_dnd::FolderDropZone,
    drop: &gtk4::gdk::Drop,
    callbacks: &FolderDropCallbacks,
) {
    let reject = |reason: &str| {
        log::debug!("Folder reorder drop refused: {reason}");
        drop.finish(gtk4::gdk::DragAction::empty());
    };
    let Some(payload) = tab_dnd::decode_folder_drag(bytes) else {
        reject("payload is not a folder drag");
        return;
    };
    let after = match zone {
        tab_dnd::FolderDropZone::Before => false,
        tab_dnd::FolderDropZone::After => true,
        tab_dnd::FolderDropZone::Into => {
            let Some(request) = folder_merge_request(&payload, row.root_path, row.relative_path)
            else {
                reject("a folder cannot be combined into itself or its own subtree");
                return;
            };
            drop.finish(gtk4::gdk::DragAction::COPY);
            if let Some(callback) = &*callbacks.merged.borrow() {
                callback(request);
            }
            return;
        }
    };
    if payload.root_path != row.root_path || payload.relative_path == row.relative_path {
        reject("different root, or dropped on itself");
        return;
    }
    if payload.parent_relative_path.as_deref()
        != folder_parent_relative_path(row.relative_path).as_deref()
    {
        reject("target is not a sibling");
        return;
    }
    let Some(dragged_index) = dragged_sibling_index(library, &payload) else {
        reject("dragged folder is no longer among its siblings");
        return;
    };
    let destination = folder_reorder_target_index(dragged_index, row.sibling_index, after);

    let response = library.reorder_folder(&payload.root_path, &payload.relative_path, destination);
    let drop_for_complete = drop.clone();
    let on_changed = Rc::clone(&callbacks.reordered);
    if let Err(error) = commands::dispatch_async_result(
        "reorder_sidebar_folder",
        move || response.recv(),
        move |result| match result {
            Ok(_) => {
                drop_for_complete.finish(gtk4::gdk::DragAction::COPY);
                if let Some(callback) = &*on_changed.borrow() {
                    callback();
                }
            }
            Err(error) => {
                log::warn!("Failed to reorder folder: {error}");
                drop_for_complete.finish(gtk4::gdk::DragAction::empty());
            }
        },
    ) {
        log::warn!("Failed to dispatch folder reorder: {error}");
        drop.finish(gtk4::gdk::DragAction::empty());
    }
}

fn dragged_sibling_index(
    library: &crate::library_store::LibraryStore,
    payload: &tab_dnd::FolderDragPayload,
) -> Option<usize> {
    let page = library
        .folder_children(
            &payload.root_path,
            payload.parent_relative_path.as_deref(),
            0,
        )
        .recv()
        .ok()?;
    page.folders
        .iter()
        .position(|folder| folder.relative_path == payload.relative_path)
}

const DROP_FEEDBACK_CLASSES: [&str; 3] = ["lsb-drop-before", "lsb-drop-into", "lsb-drop-after"];

fn set_folder_drop_feedback(widget: Option<gtk4::Widget>, zone: Option<tab_dnd::FolderDropZone>) {
    let Some(widget) = widget else {
        return;
    };
    let active = zone.map(|zone| match zone {
        tab_dnd::FolderDropZone::Before => "lsb-drop-before",
        tab_dnd::FolderDropZone::Into => "lsb-drop-into",
        tab_dnd::FolderDropZone::After => "lsb-drop-after",
    });
    for class in DROP_FEEDBACK_CLASSES {
        if Some(class) == active {
            widget.add_css_class(class);
        } else {
            widget.remove_css_class(class);
        }
    }
}

fn hovered_drop_zone(
    target: &gtk4::DropTargetAsync,
    drop: &gtk4::gdk::Drop,
    y: f64,
) -> tab_dnd::FolderDropZone {
    if !drop.formats().contain_mime_type(tab_dnd::FOLDER_DND_MIME) {
        return tab_dnd::FolderDropZone::Into;
    }
    let row_height = f64::from(target.widget().map(|widget| widget.height()).unwrap_or(0));
    tab_dnd::folder_drop_zone(y, row_height)
}

fn install_folder_drop_target(
    widget: &GtkBox,
    library: crate::library_store::LibraryStore,
    root_path: String,
    relative_path: String,
    sibling_index: usize,
    callbacks: FolderDropCallbacks,
) -> gtk4::DropTargetAsync {
    let formats = gtk4::gdk::ContentFormats::builder()
        .add_type(glib::Bytes::static_type())
        .add_mime_type(tab_dnd::SOUND_TAB_DND_MIME)
        .add_mime_type(tab_dnd::FOLDER_DND_MIME)
        .build();
    let target = gtk4::DropTargetAsync::new(Some(formats), gtk4::gdk::DragAction::COPY);
    target.connect_drag_motion(|target, drop, _, y| {
        set_folder_drop_feedback(target.widget(), Some(hovered_drop_zone(target, drop, y)));
        gtk4::gdk::DragAction::COPY
    });
    target.connect_drag_leave(|target, _| {
        set_folder_drop_feedback(target.widget(), None);
    });
    target.connect_drop(move |target, drop, _, y| {
        set_folder_drop_feedback(target.widget(), None);
        let row_height = f64::from(target.widget().map(|w| w.height()).unwrap_or(0));
        let zone = tab_dnd::folder_drop_zone(y, row_height);
        let is_folder_drag = drop.formats().contain_mime_type(tab_dnd::FOLDER_DND_MIME);
        let drop_for_read = drop.clone();
        let drop_for_finish = drop.clone();
        let library = library.clone();
        let root_path = root_path.clone();
        let relative_path = relative_path.clone();
        let callbacks = callbacks.clone();
        drop_for_read.read_value_async(
            glib::Bytes::static_type(),
            glib::Priority::DEFAULT,
            None::<&gio::Cancellable>,
            move |result| {
                let value = match result {
                    Ok(value) => value,
                    Err(error) => {
                        log::debug!("Drop payload could not be read: {error}");
                        drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                        return;
                    }
                };
                let Ok(bytes) = value.get::<glib::Bytes>() else {
                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                    return;
                };
                if is_folder_drag {
                    handle_folder_drop(
                        &bytes,
                        &library,
                        FolderDropTargetRow {
                            root_path: &root_path,
                            relative_path: &relative_path,
                            sibling_index,
                        },
                        zone,
                        &drop_for_finish,
                        &callbacks,
                    );
                    return;
                }
                let Some(payload) = tab_dnd::decode_drag_payload(&bytes) else {
                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                    return;
                };
                let target_folder = tab_dnd::FolderDragContext {
                    root_path: root_path.clone(),
                    relative_path: relative_path.clone(),
                };
                let overrides = folder_drop_overrides(&payload, &target_folder);
                if overrides.is_empty() {
                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                    return;
                }
                let drop_for_complete = drop_for_finish.clone();
                let on_changed = Rc::clone(&callbacks.changed);
                if let Err(error) = commands::dispatch_async_result(
                    "apply_folder_sound_drop",
                    move || {
                        for batch in overrides.chunks(crate::library_store::MAX_BATCH_ROWS) {
                            library
                                .apply_batch(crate::library_store::LibraryBatch::FolderOverrides(
                                    batch.to_vec(),
                                ))
                                .recv()?;
                        }
                        Ok::<(), crate::library_store::LibraryError>(())
                    },
                    move |result| match result {
                        Ok(()) => {
                            drop_for_complete.finish(gtk4::gdk::DragAction::COPY);
                            if let Some(callback) = &*on_changed.borrow() {
                                callback();
                            }
                        }
                        Err(error) => {
                            log::warn!("Folder drop failed: {error}");
                            drop_for_complete.finish(gtk4::gdk::DragAction::empty());
                        }
                    },
                ) {
                    log::warn!("Failed to dispatch folder drop: {error}");
                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                }
            },
        );
        true
    });
    widget.add_controller(target.clone());
    target
}
