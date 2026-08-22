use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, GestureClick, Image, Label, ListBox, ListBoxRow, ListView, Orientation,
    ScrolledWindow, SelectionMode, SignalListItemFactory, SingleSelection, TreeExpander,
    TreeListModel, TreeListRow, Widget,
};

use crate::app_meta::GENERAL_TAB_ID;
use crate::app_state::AppState;
use crate::commands;

use super::dialogs::DialogHost;
use super::icons;
use super::is_unmodified_delete_shortcut;
use super::menu;
use super::tab_dnd;

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
    /// Loaded sibling index used for prefetch.
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
    /// Materialized pages; the rest are placeholders.
    loaded_pages: RefCell<std::collections::BTreeSet<usize>>,
    /// Page most recently brought into view; eviction keeps its neighbours.
    focus_page: Cell<usize>,
    pending_pages: RefCell<std::collections::BTreeSet<usize>>,
    /// Cleared after a failed page load.
    children_requested: Rc<Cell<bool>>,
}

/// Widget name on the tab row's title label, so the row can be read back.
const TAB_NAME_LABEL: &str = "tab-name-label";

impl SiblingPager {
    fn mark_reloadable(&self) {
        self.children_requested.set(false);
    }
}

/// Prefetch distance from the loaded end.
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
        // Placeholder rows stand in for evicted pages and have no children.
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
        // GTK wants a concrete action even for no-op drops.
        SidebarDropIntent::Noop => gtk4::gdk::DragAction::COPY,
        SidebarDropIntent::AddToTarget
        | SidebarDropIntent::RemoveFromSource
        | SidebarDropIntent::MoveBetweenCustomTabs => gtk4::gdk::DragAction::COPY,
    }
}

type FolderChangedCallback = Rc<RefCell<Option<Box<dyn Fn() + 'static>>>>;
type FolderMergeCallback = Rc<RefCell<Option<Box<dyn Fn(FolderMergeRequest) + 'static>>>>;

/// What a folder row's drop target reports back to the sidebar.
#[derive(Clone)]
struct FolderDropCallbacks {
    /// Sounds moved into this folder, so tab membership changed.
    changed: FolderChangedCallback,
    /// Sibling order changed, so the tree needs rebuilding.
    reordered: FolderChangedCallback,
    /// A folder was dropped onto this one; the sidebar asks the user first.
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

/// A request to move every sound in one folder into another.
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

/// Last path component — what the sidebar shows for a folder.
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

/// The row a folder was dropped against.
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

/// CSS classes that show where a folder drag will land.
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

pub struct TabsSidebar {
    inner: Arc<TabsInner>,
}

struct TabsInner {
    root: GtkBox,
    list_box: ListBox,
    folder_roots: gio::ListStore,
    folder_generation: Rc<Cell<u64>>,
    folder_rebuilding: Rc<Cell<bool>>,
    tab_generation: Cell<u64>,
    state: Arc<AppState>,
    on_tab_selected: RefCell<Option<TabSelectedCallback>>,
    on_tab_membership_changed: RefCell<Option<TabMembershipChangedCallback>>,
    active_tab_id: Mutex<String>,
    tab_deletion_pending: Cell<bool>,
    toast_sender: Mutex<Option<std::sync::mpsc::Sender<String>>>,
    dialog_host: DialogHost,
}

impl TabsSidebar {
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn new(state: Arc<AppState>, dialog_host: DialogHost) -> Self {
        let vbox = GtkBox::new(Orientation::Vertical, 0);
        vbox.add_css_class("tabs-sidebar");

        let header = GtkBox::new(Orientation::Horizontal, 4);
        header.set_margin_start(8);
        header.set_margin_end(8);
        header.set_margin_top(8);
        header.set_margin_bottom(4);

        let title_lbl = Label::builder()
            .label("TABS")
            .css_classes(vec!["dim-label", "caption"])
            .hexpand(true)
            .xalign(0.0)
            .build();

        let new_tab_btn = icons::button(icons::ADD, "New Tab");
        new_tab_btn.add_css_class("sidebar-new-tab-btn");
        new_tab_btn.set_size_request(28, 28);

        header.append(&title_lbl);
        header.append(&new_tab_btn);
        vbox.append(&header);

        let list_box = ListBox::builder()
            .selection_mode(SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        let tabs_scroll = ScrolledWindow::builder()
            .child(&list_box)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(320)
            .build();
        vbox.append(&tabs_scroll);

        let folders_title = Label::builder()
            .label("FOLDERS")
            .css_classes(vec!["dim-label", "caption"])
            .xalign(0.0)
            .margin_start(8)
            .margin_end(8)
            .margin_top(12)
            .margin_bottom(4)
            .build();
        vbox.append(&folders_title);

        let folder_roots = gio::ListStore::new::<BoxedAnyObject>();
        let folder_generation = Rc::new(Cell::new(0));
        let folder_rebuilding = Rc::new(Cell::new(false));
        let folder_changed: FolderChangedCallback = Rc::new(RefCell::new(None));
        let folder_reordered: FolderChangedCallback = Rc::new(RefCell::new(None));
        let folder_merged: FolderMergeCallback = Rc::new(RefCell::new(None));
        let folder_removed: FolderChangedCallback = Rc::new(RefCell::new(None));
        let library_for_children = state.library.clone();
        let folder_tree = TreeListModel::new(folder_roots.clone(), false, false, move |item| {
            let boxed = item.downcast_ref::<BoxedAnyObject>()?;
            // Evicted pages leave non-expandable placeholders.
            let node = boxed.try_borrow::<FolderNode>().ok()?;
            if !node.has_children {
                return None;
            }
            if !node.children_requested.replace(true) {
                TabsInner::start_children_pager(
                    library_for_children.clone(),
                    node.children(),
                    node.root_path.clone(),
                    node.relative_path.clone(),
                    &node.children_pager,
                    Rc::clone(&node.children_requested),
                );
            }
            Some(node.children().upcast())
        });
        let folder_selection = SingleSelection::new(Some(folder_tree.clone()));
        folder_selection.set_autoselect(false);
        folder_selection.set_can_unselect(true);
        let folder_factory = SignalListItemFactory::new();
        folder_factory.connect_setup(|_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            let label = Label::builder()
                .xalign(0.0)
                .hexpand(true)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .build();
            let disclosure = icons::image(icons::DISCLOSURE_CLOSED);
            disclosure.set_pixel_size(16);
            disclosure.set_size_request(20, 20);
            disclosure.set_tooltip_text(Some("Show subfolders"));
            let expander = TreeExpander::new();
            expander.set_indent_for_depth(false);
            expander.set_hide_expander(true);
            expander.set_child(Some(&label));
            let row_box = GtkBox::new(Orientation::Horizontal, 2);
            row_box.add_css_class("lsb-folder-row");
            row_box.append(&disclosure);
            row_box.append(&expander);
            item.set_child(Some(&row_box));
        });
        let library_for_expansion = state.library.clone();
        let folder_roots_for_expansion = folder_roots.clone();
        let dialog_host_for_folders = dialog_host.clone();
        let folder_roots_for_actions = folder_roots.clone();
        let folder_generation_for_actions = Rc::clone(&folder_generation);
        let folder_rebuilding_for_expansion = Rc::clone(&folder_rebuilding);
        let folder_rebuilding_for_actions = Rc::clone(&folder_rebuilding);
        let folder_removed_for_menu = Rc::clone(&folder_removed);
        let folder_drop_callbacks = FolderDropCallbacks {
            changed: Rc::clone(&folder_changed),
            reordered: Rc::clone(&folder_reordered),
            merged: Rc::clone(&folder_merged),
        };
        folder_factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            let Some(row) = item.item().and_downcast::<TreeListRow>() else {
                return;
            };
            let Some(row_box) = item.child().and_downcast::<GtkBox>() else {
                return;
            };
            let Some(disclosure) = row_box.first_child().and_downcast::<Image>() else {
                return;
            };
            let Some(expander) = disclosure.next_sibling().and_downcast::<TreeExpander>() else {
                return;
            };
            let Some(label) = expander.child().and_downcast::<Label>() else {
                return;
            };
            let Some(boxed) = row.item().and_downcast::<BoxedAnyObject>() else {
                return;
            };
            if let Ok(placeholder) = boxed.try_borrow::<PlaceholderRow>() {
                label.set_label("");
                disclosure.set_opacity(0.0);
                disclosure.set_sensitive(false);
                expander.set_list_row(Some(&row));
                let page = placeholder.sibling_index / crate::library_store::PAGE_SIZE;
                let pager = placeholder.pager.upgrade();
                drop(placeholder);
                if let Some(pager) = pager {
                    pager.focus_page.set(page);
                    TabsInner::load_sibling_page(pager, page);
                }
                return;
            }
            let Ok(node) = boxed.try_borrow::<FolderNode>() else {
                return;
            };
            label.set_label(&node.name.borrow());
            let expanded = Rc::clone(&node.expanded);
            let restore_expanded = expanded.get();
            let restore_expansion =
                should_restore_expansion(restore_expanded, node.expansion_restored.replace(true));
            let connect_expanded = node.expanded_handler.borrow().is_none();
            let root_path = node.root_path.clone();
            let relative_path = node.relative_path.clone();
            let name = Rc::clone(&node.name);
            let has_children = node.has_children;
            let install_disclosure = has_children && node.disclosure_handlers.borrow().is_none();
            let install_context_menu =
                node.relative_path.is_some() && node.context_gesture.borrow().is_none();
            let install_drop_target =
                node.relative_path.is_some() && node.drop_target.borrow().is_none();
            let context_relative_path = node.relative_path.clone();
            let drop_relative_path = node.relative_path.clone();
            let sibling_index = node.sibling_index;
            let sibling_pager = node.sibling_pager.clone();
            let children = node.children();
            let children_pager = Rc::clone(&node.children_pager);
            let children_requested = Rc::clone(&node.children_requested);
            drop(node);
            if let Some(pager) = sibling_pager.as_ref().and_then(std::rc::Weak::upgrade) {
                // Evict pages farthest from the viewport.
                pager
                    .focus_page
                    .set(sibling_index / crate::library_store::PAGE_SIZE);
                if should_request_next_sibling_page(
                    sibling_index,
                    pager.loaded.get(),
                    pager.has_more.get(),
                    pager.in_flight.get(),
                ) {
                    TabsInner::load_folder_children_async(pager);
                }
            }
            expander.set_list_row(Some(&row));
            if restore_expansion {
                row.set_expanded(true);
            }
            disclosure.set_opacity(if has_children { 1.0 } else { 0.0 });
            disclosure.set_sensitive(has_children);
            if has_children {
                update_disclosure_icon(&disclosure, restore_expanded);
            } else {
                disclosure.set_tooltip_text(None);
            }
            if install_disclosure {
                let gesture = GestureClick::new();
                gesture.set_button(1);
                let row_for_click = row.clone();
                gesture.connect_released(move |_, _, _, _| {
                    row_for_click.set_expanded(!row_for_click.is_expanded());
                });
                disclosure.add_controller(gesture.clone());
                let disclosure_for_expansion = disclosure.clone();
                let expansion_handler = row.connect_expanded_notify(move |row| {
                    update_disclosure_icon(&disclosure_for_expansion, row.is_expanded());
                });
                if let Ok(node) = boxed.try_borrow::<FolderNode>() {
                    node.disclosure_handlers.replace(Some((
                        disclosure.clone(),
                        gesture,
                        row.clone(),
                        expansion_handler,
                    )));
                }
            }
            if connect_expanded {
                let library = library_for_expansion.clone();
                let expanded_root_path = root_path.clone();
                let loaded_tree = folder_roots_for_expansion.clone();
                let rebuilding = Rc::clone(&folder_rebuilding_for_expansion);
                let expansion_handler = row.connect_expanded_notify(move |row| {
                    let is_expanded = row.is_expanded();
                    if !should_handle_expansion_change(expanded.replace(is_expanded), is_expanded) {
                        return;
                    }
                    if !is_expanded
                        && should_release_collapsed_children(
                            count_loaded_child_rows(&loaded_tree),
                            MAX_RETAINED_CHILD_ROWS,
                        )
                    {
                        children.remove_all();
                        children_pager.replace(None);
                        children_requested.set(false);
                    }
                    let Some(relative_path) = relative_path.as_deref() else {
                        return;
                    };
                    if !should_persist_expansion_change(true, rebuilding.get()) {
                        return;
                    }
                    let response = library.set_folder_expanded(
                        &expanded_root_path,
                        relative_path,
                        is_expanded,
                    );
                    if let Err(error) = commands::dispatch_async_result(
                        "save_sidebar_folder_expansion",
                        move || response.recv(),
                        move |result| {
                            if let Err(error) = result {
                                log::warn!("Failed to save folder expansion: {error}");
                            }
                        },
                    ) {
                        log::warn!("Failed to dispatch folder expansion save: {error}");
                    }
                });
                if let Ok(node) = boxed.try_borrow::<FolderNode>() {
                    node.expanded_handler
                        .replace(Some((row.clone(), expansion_handler)));
                }
            }
            if install_context_menu {
                let gesture = GestureClick::new();
                gesture.set_button(3);
                let library = library_for_expansion.clone();
                let dialog_host = dialog_host_for_folders.clone();
                let label = label.clone();
                let context_root_path = root_path.clone();
                let folder_roots = folder_roots_for_actions.clone();
                let folder_generation = Rc::clone(&folder_generation_for_actions);
                let folder_rebuilding_ctx = Rc::clone(&folder_rebuilding_for_actions);
                let folder_removed_ctx = Rc::clone(&folder_removed_for_menu);
                gesture.connect_pressed(move |gesture, _, x, y| {
                    let Some(widget) = gesture.widget() else {
                        return;
                    };
                    let Some(relative_path) = context_relative_path.as_deref() else {
                        return;
                    };
                    let menu_model = gio::Menu::new();
                    menu_model.append(Some("Rename Folder"), Some("folder-ctx.rename"));
                    menu_model.append(Some("Move Up"), Some("folder-ctx.move-up"));
                    menu_model.append(Some("Move Down"), Some("folder-ctx.move-down"));
                    menu_model.append(Some("Remove Folder"), Some("folder-ctx.remove"));
                    let action_group = gio::SimpleActionGroup::new();
                    let name_for_remove = Rc::clone(&name);
                    let dialog_host_for_remove = dialog_host.clone();
                    let action = gio::SimpleAction::new("rename", None);
                    let rename_library = library.clone();
                    let rename_root_path = context_root_path.clone();
                    let rename_relative_path = relative_path.to_string();
                    let name = Rc::clone(&name);
                    let label = label.clone();
                    let dialog_host = dialog_host.clone();
                    action.connect_activate(move |_, _| {
                        let library = rename_library.clone();
                        let root_path = rename_root_path.clone();
                        let relative_path = rename_relative_path.clone();
                        let name = Rc::clone(&name);
                        let label = label.clone();
                        let initial_name = name.borrow().clone();
                        dialog_host.show_input(
                            "Rename Folder",
                            "Enter a display name:",
                            &initial_name,
                            "Rename",
                            move |new_name| {
                                let response = library.set_folder_display_name(
                                    &root_path,
                                    &relative_path,
                                    Some(&new_name),
                                );
                                let name = Rc::clone(&name);
                                let label = label.clone();
                                if let Err(error) = commands::dispatch_async_result(
                                    "rename_sidebar_folder",
                                    move || response.recv(),
                                    move |result| match result {
                                        Ok(true) => {
                                            *name.borrow_mut() = new_name.clone();
                                            label.set_label(&new_name);
                                        }
                                        Ok(false) => {
                                            log::warn!("Folder rename target no longer exists");
                                        }
                                        Err(error) => {
                                            log::warn!("Failed to rename folder: {error}");
                                        }
                                    },
                                ) {
                                    log::warn!("Failed to dispatch folder rename: {error}");
                                }
                            },
                        );
                    });
                    action_group.add_action(&action);
                    for (action_name, direction) in [("move-up", -1), ("move-down", 1)] {
                        let action = gio::SimpleAction::new(action_name, None);
                        let library = library.clone();
                        let root_path = context_root_path.clone();
                        let relative_path = relative_path.to_string();
                        let folder_roots = folder_roots.clone();
                        let folder_generation = Rc::clone(&folder_generation);
                        let folder_rebuilding = Rc::clone(&folder_rebuilding_ctx);
                        action.connect_activate(move |_, _| {
                            let response =
                                library.move_folder(&root_path, &relative_path, direction);
                            let library = library.clone();
                            let folder_roots = folder_roots.clone();
                            let folder_generation = Rc::clone(&folder_generation);
                            let folder_rebuilding = Rc::clone(&folder_rebuilding);
                            if let Err(error) = commands::dispatch_async_result(
                                "move_sidebar_folder",
                                move || response.recv(),
                                move |result| match result {
                                    Ok(true) => TabsInner::reload_folder_roots_model(
                                        library,
                                        folder_roots,
                                        folder_generation,
                                        folder_rebuilding,
                                    ),
                                    Ok(false) => {}
                                    Err(error) => {
                                        log::warn!("Failed to move folder: {error}");
                                    }
                                },
                            ) {
                                log::warn!("Failed to dispatch folder move: {error}");
                            }
                        });
                        action_group.add_action(&action);
                    }
                    {
                        let action = gio::SimpleAction::new("remove", None);
                        let library = library.clone();
                        let root_path = context_root_path.clone();
                        let relative_path = relative_path.to_string();
                        let dialog_host = dialog_host_for_remove.clone();
                        let name = Rc::clone(&name_for_remove);
                        let on_removed = Rc::clone(&folder_removed_ctx);
                        action.connect_activate(move |_, _| {
                            let scope = crate::library_store::LibraryScope::Folder {
                                root_path: root_path.clone(),
                                relative_path: relative_path.clone(),
                            };
                            let response = library.count(scope, "");
                            let library = library.clone();
                            let root_path = root_path.clone();
                            let relative_path = relative_path.clone();
                            let dialog_host = dialog_host.clone();
                            let display_name = name.borrow().clone();
                            let on_removed = Rc::clone(&on_removed);
                            if let Err(error) = commands::dispatch_async_result(
                                "count_folder_before_remove",
                                move || response.recv(),
                                move |result| {
                                    let count = match result {
                                        Ok(count) => count,
                                        Err(error) => {
                                            log::warn!("Failed to count folder sounds: {error}");
                                            dialog_host.show_error(
                                                "Failed to Remove Folder",
                                                &error.to_string(),
                                            );
                                            return;
                                        }
                                    };
                                    let plural = if count == 1 { "sound" } else { "sounds" };
                                    let message = format!(
                                        "Remove '{display_name}'? Its {count} {plural} stop appearing. Nothing is deleted from disk; restore it from Settings."
                                    );
                                    let library = library.clone();
                                    let root_path = root_path.clone();
                                    let relative_path = relative_path.clone();
                                    let on_removed = Rc::clone(&on_removed);
                                    dialog_host.show_confirm(
                                        "Remove Folder",
                                        &message,
                                        "Remove",
                                        move || {
                                            let response = library.set_folder_hidden(
                                                &root_path,
                                                &relative_path,
                                                true,
                                            );
                                            let on_removed = Rc::clone(&on_removed);
                                            if let Err(error) = commands::dispatch_async_result(
                                                "hide_sidebar_folder",
                                                move || response.recv(),
                                                move |result| match result {
                                                    Ok(_) => {
                                                        if let Some(callback) =
                                                            &*on_removed.borrow()
                                                        {
                                                            callback();
                                                        }
                                                    }
                                                    Err(error) => {
                                                        log::warn!(
                                                            "Failed to remove folder: {error}"
                                                        );
                                                    }
                                                },
                                            ) {
                                                log::warn!(
                                                    "Failed to dispatch folder removal: {error}"
                                                );
                                            }
                                        },
                                    );
                                },
                            ) {
                                log::warn!("Failed to dispatch folder count: {error}");
                            }
                        });
                        action_group.add_action(&action);
                    }
                    menu::show_popover_menu(
                        &widget,
                        "folder-ctx",
                        &menu_model,
                        &action_group,
                        x,
                        y,
                    );
                });
                expander.add_controller(gesture.clone());
                if let Ok(node) = boxed.try_borrow::<FolderNode>() {
                    node.context_gesture.replace(Some(gesture));
                }
            }
            if install_drop_target {
                let Some(relative_path) = drop_relative_path else {
                    return;
                };
                let target = install_folder_drop_target(
                    &row_box,
                    library_for_expansion.clone(),
                    root_path.clone(),
                    relative_path.clone(),
                    sibling_index,
                    folder_drop_callbacks.clone(),
                );
                if let Ok(node) = boxed.try_borrow::<FolderNode>() {
                    node.drop_target.replace(Some(target));
                }
                if let Ok(node) = boxed.try_borrow::<FolderNode>() {
                    if node.drag_source.borrow().is_none() {
                        let source = install_folder_drag_source(
                            &row_box,
                            root_path.clone(),
                            relative_path.clone(),
                        );
                        node.drag_source.replace(Some(source));
                    }
                }
            }
        });
        folder_factory.connect_unbind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            let Some(row) = item.item().and_downcast::<TreeListRow>() else {
                return;
            };
            let Some(row_box) = item.child().and_downcast::<GtkBox>() else {
                return;
            };
            let Some(disclosure) = row_box.first_child().and_downcast::<Image>() else {
                return;
            };
            let Some(expander) = disclosure.next_sibling().and_downcast::<TreeExpander>() else {
                return;
            };
            let Some(boxed) = row.item().and_downcast::<BoxedAnyObject>() else {
                return;
            };
            let (disclosure_handlers, expanded_handler, gesture, drop_target, drag_source) = {
                let Ok(node) = boxed.try_borrow::<FolderNode>() else {
                    return;
                };
                let disclosure_handlers = node.disclosure_handlers.borrow_mut().take();
                let expanded_handler = node.expanded_handler.borrow_mut().take();
                let gesture = node.context_gesture.borrow_mut().take();
                let drop_target = node.drop_target.borrow_mut().take();
                let drag_source = node.drag_source.borrow_mut().take();
                (
                    disclosure_handlers,
                    expanded_handler,
                    gesture,
                    drop_target,
                    drag_source,
                )
            };
            if let Some((image, click_gesture, row, expansion_handler)) = disclosure_handlers {
                image.remove_controller(&click_gesture);
                row.disconnect(expansion_handler);
            }
            if let Some((row, expansion_handler)) = expanded_handler {
                row.disconnect(expansion_handler);
            }
            if let Some(gesture) = gesture {
                expander.remove_controller(&gesture);
            }
            if let Some(target) = drop_target {
                row_box.remove_controller(&target);
            }
            if let Some(source) = drag_source {
                row_box.remove_controller(&source);
            }
        });
        let folder_view = ListView::new(Some(folder_selection.clone()), Some(folder_factory));
        folder_view.add_css_class("navigation-sidebar");
        folder_view.add_css_class("folder-tree");

        let folders_scroll = ScrolledWindow::builder()
            .child(&folder_view)
            .vexpand(true)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .build();
        vbox.append(&folders_scroll);

        let inner = Arc::new(TabsInner {
            root: vbox,
            list_box: list_box.clone(),
            folder_roots,
            folder_generation,
            folder_rebuilding,
            tab_generation: Cell::new(0),
            state,
            on_tab_selected: RefCell::new(None),
            on_tab_membership_changed: RefCell::new(None),
            active_tab_id: Mutex::new(GENERAL_TAB_ID.to_string()),
            tab_deletion_pending: Cell::new(false),
            toast_sender: Mutex::new(None),
            dialog_host,
        });
        {
            let inner_weak = Arc::downgrade(&inner);
            folder_changed.borrow_mut().replace(Box::new(move || {
                if let Some(inner) = inner_weak.upgrade() {
                    inner.emit_tab_membership_changed();
                }
            }));
        }
        {
            let inner_weak = Arc::downgrade(&inner);
            folder_reordered.borrow_mut().replace(Box::new(move || {
                if let Some(inner) = inner_weak.upgrade() {
                    inner.reload_folder_roots();
                }
            }));
        }
        {
            let inner_weak = Arc::downgrade(&inner);
            folder_removed.borrow_mut().replace(Box::new(move || {
                if let Some(inner) = inner_weak.upgrade() {
                    inner.reload_folder_roots();
                    let selected = inner.active_tab_id.lock().clone();
                    inner.queue_reload_tabs_and_emit(Some(selected));
                    inner.emit_tab_membership_changed();
                }
            }));
        }
        {
            let inner_weak = Arc::downgrade(&inner);
            folder_merged.borrow_mut().replace(Box::new(move |request| {
                if let Some(inner) = inner_weak.upgrade() {
                    inner.request_folder_merge(request);
                }
            }));
        }

        {
            let inner_weak = Arc::downgrade(&inner);
            folder_selection.connect_selected_item_notify(move |selection| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let Some(row) = selection.selected_item().and_downcast::<TreeListRow>() else {
                    return;
                };
                let Some(boxed) = row.item().and_downcast::<BoxedAnyObject>() else {
                    return;
                };
                let Ok(node) = boxed.try_borrow::<FolderNode>() else {
                    return;
                };
                let Some(relative_path) = node.relative_path.clone() else {
                    return;
                };
                let root_path = node.root_path.clone();
                drop(node);
                inner.list_box.select_row(None::<&ListBoxRow>);
                let identity = format!("folder:{root_path}/{relative_path}");
                *inner.active_tab_id.lock() = identity.clone();
                if let Some(ref callback) = *inner.on_tab_selected.borrow() {
                    callback(SidebarSelection {
                        identity,
                        scope: crate::library_store::LibraryScope::Folder {
                            root_path,
                            relative_path,
                        },
                    });
                };
            });
        }

        {
            let inner_weak = Arc::downgrade(&inner);
            list_box.connect_row_selected(move |_, row| {
                let Some(inner_sel) = inner_weak.upgrade() else {
                    return;
                };
                if let Some(row) = row {
                    let id = row.widget_name().to_string();
                    *inner_sel.active_tab_id.lock() = id.clone();
                    if let Some(ref cb) = *inner_sel.on_tab_selected.borrow() {
                        let scope = if id == GENERAL_TAB_ID {
                            crate::library_store::LibraryScope::General
                        } else {
                            crate::library_store::LibraryScope::ManualTab(id.clone())
                        };
                        cb(SidebarSelection {
                            identity: id,
                            scope,
                        });
                    }
                }
            });
        }

        {
            let inner_weak = Arc::downgrade(&inner);
            new_tab_btn.connect_clicked(move |_| {
                let Some(inner_btn) = inner_weak.upgrade() else {
                    return;
                };
                inner_btn.show_new_tab_dialog();
            });
        }

        inner.reload_tabs_async(None);
        inner.connect_delete_shortcut();
        inner.attach_sidebar_drop_target(&list_box);

        Self { inner }
    }

    pub fn activate_tab(&self, scope_key: &str) -> bool {
        let identity = match scope_key {
            GENERAL_TAB_ID => GENERAL_TAB_ID,
            other => match other.strip_prefix("tab:") {
                Some(public_id) => public_id,
                None => return false,
            },
        };

        let mut row = self.inner.list_box.first_child();
        while let Some(child) = row {
            if let Some(list_row) = child.downcast_ref::<ListBoxRow>() {
                if list_row.widget_name() == identity {
                    self.inner.list_box.select_row(Some(list_row));
                    return true;
                }
            }
            row = child.next_sibling();
        }
        false
    }

    pub fn widget(&self) -> &Widget {
        self.inner.root.upcast_ref()
    }

    pub fn connect_tab_selected<F: Fn(SidebarSelection) + 'static>(&self, f: F) {
        *self.inner.on_tab_selected.borrow_mut() = Some(Box::new(f));
    }

    pub fn connect_tab_membership_changed<F: Fn() + 'static>(&self, f: F) {
        *self.inner.on_tab_membership_changed.borrow_mut() = Some(Box::new(f));
    }

    pub fn reload_tabs(&self) {
        self.inner.reload_tabs_and_emit(None);
    }

    pub fn set_toast_sender(&self, sender: std::sync::mpsc::Sender<String>) {
        *self.inner.toast_sender.lock() = Some(sender);
    }

    pub fn cleanup(&self) {
        *self.inner.on_tab_selected.borrow_mut() = None;
        *self.inner.on_tab_membership_changed.borrow_mut() = None;
        *self.inner.toast_sender.lock() = None;
    }
}

impl TabsInner {
    fn load_roots_async(
        library: crate::library_store::LibraryStore,
        model: gio::ListStore,
        page: usize,
        generation: Rc<Cell<u64>>,
        expected_generation: u64,
    ) {
        let response = library.roots(page);
        if let Err(error) = commands::dispatch_async_result(
            "load_sidebar_roots",
            move || response.recv(),
            move |result| {
                if generation.get() != expected_generation {
                    return;
                }
                match result {
                    Ok(result) => {
                        let count = result.roots.len();
                        for root in result.roots {
                            model.append(&BoxedAnyObject::new(FolderNode::root(root.path)));
                        }
                        if count == crate::library_store::PAGE_SIZE {
                            Self::load_roots_async(
                                library.clone(),
                                model.clone(),
                                page.saturating_add(1),
                                Rc::clone(&generation),
                                expected_generation,
                            );
                        }
                    }
                    Err(error) => {
                        log::warn!("Failed to load sound folder roots: {error}");
                    }
                }
            },
        ) {
            log::warn!("Failed to dispatch sound folder root load: {error}");
        }
    }

    fn start_children_pager(
        library: crate::library_store::LibraryStore,
        children: gio::ListStore,
        root_path: String,
        relative_path: Option<String>,
        pager_slot: &Rc<RefCell<Option<Rc<SiblingPager>>>>,
        children_requested: Rc<Cell<bool>>,
    ) {
        let pager = Rc::new(SiblingPager {
            library,
            children,
            root_path,
            parent_relative_path: relative_path,
            loaded: Cell::new(0),
            next_page: Cell::new(0),
            has_more: Cell::new(true),
            in_flight: Cell::new(false),
            loaded_pages: RefCell::new(std::collections::BTreeSet::new()),
            focus_page: Cell::new(0),
            pending_pages: RefCell::new(std::collections::BTreeSet::new()),
            children_requested,
        });
        pager_slot.replace(Some(Rc::clone(&pager)));
        Self::load_folder_children_async(pager);
    }

    /// Loads the next page onto the end of a folder's children.
    fn load_folder_children_async(pager: Rc<SiblingPager>) {
        let page = pager.next_page.get();
        Self::load_sibling_page(pager, page);
    }

    fn load_sibling_page(pager: Rc<SiblingPager>, page: usize) {
        if pager.loaded_pages.borrow().contains(&page) {
            return;
        }
        if pager.in_flight.replace(true) {
            // Keep the active request queued.
            pager.pending_pages.borrow_mut().insert(page);
            return;
        }
        let response = pager.library.folder_children(
            &pager.root_path,
            pager.parent_relative_path.as_deref(),
            page,
        );
        let pager_for_result = Rc::clone(&pager);
        if let Err(error) = commands::dispatch_async_result(
            "load_sidebar_folder_children",
            move || response.recv(),
            move |result| match result {
                Ok(result) => {
                    let start_index = page * crate::library_store::PAGE_SIZE;
                    let count = result.folders.len();
                    let nodes: Vec<BoxedAnyObject> = result
                        .folders
                        .into_iter()
                        .enumerate()
                        .map(|(offset, folder)| {
                            BoxedAnyObject::new(FolderNode::folder_at(
                                pager_for_result.root_path.clone(),
                                folder,
                                start_index + offset,
                                Some(Rc::downgrade(&pager_for_result)),
                            ))
                        })
                        .collect();
                    let existing = pager_for_result.children.n_items() as usize;
                    if start_index >= existing {
                        for node in &nodes {
                            pager_for_result.children.append(node);
                        }
                        pager_for_result.loaded.set(start_index + count);
                        pager_for_result.next_page.set(page.saturating_add(1));
                        pager_for_result
                            .has_more
                            .set(count == crate::library_store::PAGE_SIZE);
                    } else {
                        // Preserve indices while swapping placeholders.
                        let replaced = count.min(existing - start_index);
                        pager_for_result.children.splice(
                            start_index as u32,
                            replaced as u32,
                            &nodes[..replaced],
                        );
                    }
                    pager_for_result.loaded_pages.borrow_mut().insert(page);
                    pager_for_result.in_flight.set(false);
                    Self::evict_distant_sibling_pages(&pager_for_result);
                    Self::drain_pending_sibling_page(&pager_for_result);
                }
                Err(error) => {
                    log::warn!("Failed to load sound folder children: {error}");
                    pager_for_result.has_more.set(false);
                    pager_for_result.in_flight.set(false);
                    // Let transient failures retry.
                    pager_for_result.mark_reloadable();
                    Self::drain_pending_sibling_page(&pager_for_result);
                }
            },
        ) {
            log::warn!("Failed to dispatch sound folder child load: {error}");
            pager.in_flight.set(false);
            pager.mark_reloadable();
        }
    }

    /// Starts the page that was asked for while the pager was busy.
    fn drain_pending_sibling_page(pager: &Rc<SiblingPager>) {
        // Load nearest to the viewport first.
        let focus = pager.focus_page.get();
        let next = pager
            .pending_pages
            .borrow()
            .iter()
            .copied()
            .filter(|page| !pager.loaded_pages.borrow().contains(page))
            .min_by_key(|page| page.abs_diff(focus));
        let Some(page) = next else {
            pager.pending_pages.borrow_mut().clear();
            return;
        };
        pager.pending_pages.borrow_mut().remove(&page);
        Self::load_sibling_page(Rc::clone(pager), page);
    }

    fn evict_distant_sibling_pages(pager: &Rc<SiblingPager>) {
        loop {
            let victim = {
                let pages = pager.loaded_pages.borrow();
                page_to_evict(&pages, pager.focus_page.get(), MAX_LOADED_SIBLING_PAGES)
            };
            let Some(page) = victim else {
                return;
            };
            let start = page * crate::library_store::PAGE_SIZE;
            let total = pager.children.n_items() as usize;
            if start >= total {
                pager.loaded_pages.borrow_mut().remove(&page);
                continue;
            }
            let count = crate::library_store::PAGE_SIZE.min(total - start);
            let blanks: Vec<BoxedAnyObject> = (0..count)
                .map(|offset| {
                    BoxedAnyObject::new(PlaceholderRow {
                        sibling_index: start + offset,
                        pager: Rc::downgrade(pager),
                    })
                })
                .collect();
            pager.children.splice(start as u32, count as u32, &blanks);
            pager.loaded_pages.borrow_mut().remove(&page);
        }
    }

    fn reload_folder_roots(&self) {
        Self::reload_folder_roots_model(
            self.state.library.clone(),
            self.folder_roots.clone(),
            Rc::clone(&self.folder_generation),
            Rc::clone(&self.folder_rebuilding),
        );
    }

    fn reload_folder_roots_model(
        library: crate::library_store::LibraryStore,
        folder_roots: gio::ListStore,
        folder_generation: Rc<Cell<u64>>,
        folder_rebuilding: Rc<Cell<bool>>,
    ) {
        let next_generation = folder_generation.get().wrapping_add(1);
        folder_generation.set(next_generation);
        folder_rebuilding.set(true);
        folder_roots.remove_all();
        folder_rebuilding.set(false);
        Self::load_roots_async(library, folder_roots, 0, folder_generation, next_generation);
    }

    fn connect_delete_shortcut(self: &Arc<Self>) {
        let key = gtk4::EventControllerKey::new();
        let inner_weak = Arc::downgrade(self);
        key.connect_key_pressed(move |_, keyval, _, modifiers| {
            let Some(inner) = inner_weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if !is_unmodified_delete_shortcut(keyval, modifiers) {
                return glib::Propagation::Proceed;
            }
            let Some(row) = inner.list_box.selected_row() else {
                return glib::Propagation::Proceed;
            };
            let tab_id = row.widget_name().to_string();
            let tab_name = Self::tab_row_name(&row);
            let Some(tab_name) = tab_name else {
                return glib::Propagation::Proceed;
            };

            inner.request_tab_deletion(tab_id, tab_name);
            glib::Propagation::Stop
        });
        self.list_box.add_controller(key);
    }

    fn queue_reload_tabs_and_emit(self: &Arc<Self>, select_id: Option<String>) {
        let inner_weak = Arc::downgrade(self);
        glib::idle_add_local_once(move || {
            let Some(inner) = inner_weak.upgrade() else {
                return;
            };
            inner.reload_tabs_async(select_id.as_deref());
        });
    }

    fn show_new_tab_dialog(self: &Arc<Self>) {
        let inner_weak = Arc::downgrade(self);
        self.dialog_host.show_input(
            "New Tab",
            "Enter a name for the new tab:",
            "",
            "Create",
            move |name| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let inner_done = Arc::downgrade(&inner);
                let dispatch = commands::create_tab_with_store_async(
                    name,
                    inner.state.library.clone(),
                    move |result| match result {
                        Ok(tab) => {
                            if let Some(inner) = inner_done.upgrade() {
                                *inner.active_tab_id.lock() = tab.id.clone();
                                inner.queue_reload_tabs_and_emit(Some(tab.id));
                            }
                        }
                        Err(e) => {
                            if let Some(inner) = inner_done.upgrade() {
                                inner.queue_reload_tabs_and_emit(None);
                            }
                            log::warn!("Failed to create tab: {e}");
                        }
                    },
                );
                if let Err(error) = dispatch {
                    log::warn!("Failed to dispatch tab creation: {error}");
                }
            },
        );
    }

    fn reload_tabs_and_emit(self: &Arc<Self>, select_id: Option<&str>) {
        let inner_weak = Arc::downgrade(self);
        let select_id = select_id.map(str::to_string);
        glib::idle_add_local_once(move || {
            let Some(inner) = inner_weak.upgrade() else {
                return;
            };
            inner.reload_tabs_async(select_id.as_deref());
        });
    }

    fn reload_tabs_async(self: &Arc<Self>, select_id: Option<&str>) {
        self.reload_folder_roots();
        self.state.manual_tabs.lock().clear();
        let generation = self.tab_generation.get().wrapping_add(1);
        self.tab_generation.set(generation);
        self.list_box.select_row(None::<&ListBoxRow>);
        while let Some(row) = self.list_box.row_at_index(0) {
            self.list_box.remove(&row);
        }

        let target_id = select_id
            .map(str::to_string)
            .unwrap_or_else(|| self.active_tab_id.lock().clone());
        let response = self
            .state
            .library
            .count(crate::library_store::LibraryScope::General, "");
        let inner_weak = Arc::downgrade(self);
        let target_for_completion = target_id.clone();
        if let Err(error) = commands::dispatch_async_result(
            "load_sidebar_general_count",
            move || response.recv(),
            move |result| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                if inner.tab_generation.get() != generation {
                    return;
                }
                let total_sounds = match result {
                    Ok(total) => total,
                    Err(error) => {
                        log::warn!("Failed to count General sidebar sounds: {error}");
                        0
                    }
                };
                inner.list_box.append(&inner.make_tab_row(
                    GENERAL_TAB_ID,
                    "General",
                    icons::FOLDER_OPEN,
                    total_sounds,
                    false,
                ));
                inner.load_manual_tabs_async(0, generation, target_for_completion);
            },
        ) {
            log::warn!("Failed to dispatch General sidebar count: {error}");
            self.list_box.append(&self.make_tab_row(
                GENERAL_TAB_ID,
                "General",
                icons::FOLDER_OPEN,
                0,
                false,
            ));
            self.load_manual_tabs_async(0, generation, target_id);
        }
    }

    fn load_manual_tabs_async(self: &Arc<Self>, page: usize, generation: u64, target_id: String) {
        let response = self.state.library.manual_tabs(page);
        let inner_weak = Arc::downgrade(self);
        let target_for_completion = target_id.clone();
        if let Err(error) = commands::dispatch_async_result(
            "load_sidebar_manual_tabs",
            move || response.recv(),
            move |result| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                if inner.tab_generation.get() != generation {
                    return;
                }
                match result {
                    Ok(result) => {
                        let has_more = page
                            .saturating_add(1)
                            .saturating_mul(crate::library_store::PAGE_SIZE)
                            < result.total;
                        for tab in result.tabs {
                            inner.state.manual_tabs.lock().push(tab.clone());
                            inner.list_box.append(&inner.make_tab_row(
                                &tab.public_id,
                                &tab.name,
                                icons::FOLDER,
                                tab.sound_count,
                                true,
                            ));
                        }
                        if has_more {
                            inner.load_manual_tabs_async(
                                page.saturating_add(1),
                                generation,
                                target_for_completion,
                            );
                        } else {
                            inner.finish_tab_reload(&target_for_completion);
                        }
                    }
                    Err(error) => {
                        log::warn!("Failed to load manual sidebar tabs: {error}");
                        inner.finish_tab_reload(&target_for_completion);
                    }
                }
            },
        ) {
            log::warn!("Failed to dispatch manual sidebar tab load: {error}");
            if self.tab_generation.get() == generation {
                self.finish_tab_reload(&target_id);
            }
        }
    }

    fn finish_tab_reload(&self, target_id: &str) {
        if !self.select_row_by_id(target_id) {
            self.select_row_by_id(GENERAL_TAB_ID);
        }
    }

    fn select_row_by_id(&self, tab_id: &str) -> bool {
        let mut index = 0;
        while let Some(row) = self.list_box.row_at_index(index) {
            if row.widget_name() == tab_id {
                self.list_box.select_row(Some(&row));
                return true;
            }
            index += 1;
        }
        false
    }

    fn tab_row_name(row: &ListBoxRow) -> Option<String> {
        let mut child = row.child()?.first_child();
        while let Some(widget) = child {
            if widget.widget_name() == TAB_NAME_LABEL {
                return widget
                    .downcast::<Label>()
                    .ok()
                    .map(|l| l.label().to_string());
            }
            child = widget.next_sibling();
        }
        None
    }

    fn make_tab_row(
        self: &Arc<Self>,
        id: &str,
        name: &str,
        icon: icons::IconPair,
        sound_count: usize,
        editable: bool,
    ) -> ListBoxRow {
        let hbox = GtkBox::new(Orientation::Horizontal, 8);
        hbox.set_margin_start(8);
        hbox.set_margin_end(8);
        hbox.set_margin_top(5);
        hbox.set_margin_bottom(5);

        let icon = icons::image(icon);
        let label = Label::builder()
            .label(name)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .build();
        // Shortcuts must not query the store on the GTK thread.
        label.set_widget_name(TAB_NAME_LABEL);

        hbox.append(&icon);
        hbox.append(&label);

        if sound_count > 0 {
            let badge = Label::builder()
                .label(sound_count.to_string())
                .css_classes(vec!["tab-count-badge"])
                .build();
            hbox.append(&badge);
        }

        let row = ListBoxRow::builder().child(&hbox).build();
        row.set_widget_name(id);
        row.add_css_class("tab-row");

        self.attach_tab_context_menu(&row, id.to_string(), name.to_string(), editable);

        row
    }

    fn attach_tab_context_menu(
        self: &Arc<Self>,
        row: &ListBoxRow,
        tab_id: String,
        tab_name: String,
        editable: bool,
    ) {
        let gesture = GestureClick::new();
        gesture.set_button(3);

        {
            let list_box = self.list_box.clone();
            let row = row.clone();
            gesture.connect_pressed(move |_, _, _, _| {
                list_box.select_row(Some(&row));
            });
        }

        let inner = Arc::clone(self);
        gesture.connect_released(move |gesture, _, x, y| {
            let Some(widget) = gesture.widget() else {
                return;
            };
            inner.show_tab_context_menu(&widget, x, y, &tab_id, &tab_name, editable);
        });

        row.add_controller(gesture);
    }

    fn clear_hovered_drop_row(hovered_row: &Rc<RefCell<Option<ListBoxRow>>>) {
        if let Some(row) = hovered_row.borrow_mut().take() {
            row.remove_css_class("tab-row-drop-hover");
        }
    }

    fn update_hovered_drop_row(
        list_box: &ListBox,
        hovered_row: &Rc<RefCell<Option<ListBoxRow>>>,
        y: f64,
    ) -> Option<ListBoxRow> {
        let next_row = list_box.row_at_y(y as i32);
        let next_id = next_row.as_ref().map(|row| row.widget_name().to_string());
        let current_id = hovered_row
            .borrow()
            .as_ref()
            .map(|row| row.widget_name().to_string());

        if next_id == current_id {
            return next_row;
        }

        Self::clear_hovered_drop_row(hovered_row);
        if let Some(row) = &next_row {
            row.add_css_class("tab-row-drop-hover");
        }
        *hovered_row.borrow_mut() = next_row.clone();
        next_row
    }

    fn action_for_hovered_row(&self, hovered_row: Option<&ListBoxRow>) -> gtk4::gdk::DragAction {
        let Some(row) = hovered_row else {
            return gtk4::gdk::DragAction::empty();
        };

        let target_tab_id = row.widget_name().to_string();
        if target_tab_id.trim().is_empty() {
            return gtk4::gdk::DragAction::empty();
        }

        let source_tab_id = self.active_tab_id.lock().clone();
        let intent = resolve_sidebar_drop_intent(&source_tab_id, &target_tab_id);
        drag_action_for_intent(intent)
    }

    fn tab_display_name(&self, tab_id: &str) -> String {
        if tab_id == GENERAL_TAB_ID {
            return "General".to_string();
        }

        self.state
            .manual_tabs
            .lock()
            .iter()
            .find(|tab| tab.public_id == tab_id)
            .map(|tab| tab.name.clone())
            .unwrap_or_else(|| tab_id.to_string())
    }

    fn send_drop_toast(
        &self,
        intent: SidebarDropIntent,
        source_tab_id: &str,
        target_tab_id: &str,
        count: usize,
    ) {
        if count == 0 {
            return;
        }

        let message = match intent {
            SidebarDropIntent::Noop => return,
            SidebarDropIntent::AddToTarget => {
                let target_name = self.tab_display_name(target_tab_id);
                if count == 1 {
                    format!("1 sound added to {target_name}")
                } else {
                    format!("{count} sounds added to {target_name}")
                }
            }
            SidebarDropIntent::RemoveFromSource => {
                let source_name = self.tab_display_name(source_tab_id);
                if count == 1 {
                    format!("1 sound removed from {source_name}")
                } else {
                    format!("{count} sounds removed from {source_name}")
                }
            }
            SidebarDropIntent::MoveBetweenCustomTabs => {
                let target_name = self.tab_display_name(target_tab_id);
                if count == 1 {
                    format!("1 sound moved to {target_name}")
                } else {
                    format!("{count} sounds moved to {target_name}")
                }
            }
        };

        if let Some(tx) = &*self.toast_sender.lock() {
            let _ = tx.send(message);
        }
    }

    fn attach_sidebar_drop_target(self: &Arc<Self>, list_box: &ListBox) {
        let drop_formats = gtk4::gdk::ContentFormats::builder()
            .add_type(glib::Bytes::static_type())
            .add_mime_type(tab_dnd::SOUND_TAB_DND_MIME)
            .build();
        let drop_target =
            gtk4::DropTargetAsync::new(Some(drop_formats), gtk4::gdk::DragAction::COPY);
        drop_target.set_propagation_phase(gtk4::PropagationPhase::Capture);

        let hovered_row = Rc::new(RefCell::new(None::<ListBoxRow>));

        drop_target.connect_accept(|_, drop| {
            let formats = drop.formats();
            let accepts = formats.contain_mime_type(tab_dnd::SOUND_TAB_DND_MIME)
                || formats.contains_type(glib::Bytes::static_type());
            log::debug!(
                "Sidebar drop accept: formats={} mime={} bytes_type={} accepted={}",
                formats,
                tab_dnd::SOUND_TAB_DND_MIME,
                formats.contains_type(glib::Bytes::static_type()),
                accepts
            );
            accepts
        });

        {
            let inner_weak = Arc::downgrade(self);
            let list_box = list_box.clone();
            let hovered_row = Rc::clone(&hovered_row);
            drop_target.connect_drag_enter(move |_, drop, _, y| {
                let Some(inner) = inner_weak.upgrade() else {
                    return gtk4::gdk::DragAction::empty();
                };
                let hovered = Self::update_hovered_drop_row(&list_box, &hovered_row, y);
                let action = inner.action_for_hovered_row(hovered.as_ref());
                let hovered_id = hovered
                    .as_ref()
                    .map(|row| row.widget_name().to_string())
                    .unwrap_or_else(|| "<none>".to_string());
                drop.status(action, action);
                log::debug!(
                    "Sidebar drop enter: y={y:.1} target={} action={:?} source_selected={:?}",
                    hovered_id,
                    action,
                    drop.drag().map(|drag| drag.selected_action())
                );
                action
            });
        }

        {
            let inner_weak = Arc::downgrade(self);
            let list_box = list_box.clone();
            let hovered_row = Rc::clone(&hovered_row);
            drop_target.connect_drag_motion(move |_, drop, _, y| {
                let Some(inner) = inner_weak.upgrade() else {
                    return gtk4::gdk::DragAction::empty();
                };
                let hovered = Self::update_hovered_drop_row(&list_box, &hovered_row, y);
                let action = inner.action_for_hovered_row(hovered.as_ref());
                let hovered_id = hovered
                    .as_ref()
                    .map(|row| row.widget_name().to_string())
                    .unwrap_or_else(|| "<none>".to_string());
                drop.status(action, action);
                log::debug!(
                    "Sidebar drop motion: y={y:.1} target={} action={:?} source_selected={:?}",
                    hovered_id,
                    action,
                    drop.drag().map(|drag| drag.selected_action())
                );
                action
            });
        }

        {
            let hovered_row = Rc::clone(&hovered_row);
            drop_target.connect_drag_leave(move |_, _| {
                Self::clear_hovered_drop_row(&hovered_row);
            });
        }

        {
            let inner_weak = Arc::downgrade(self);
            let list_box = list_box.clone();
            let hovered_row = Rc::clone(&hovered_row);
            drop_target.connect_drop(move |_, drop, _, y| {
                let Some(inner) = inner_weak.upgrade() else { return false };
                let hovered = Self::update_hovered_drop_row(&list_box, &hovered_row, y);
                Self::clear_hovered_drop_row(&hovered_row);

                let Some(target_row) = hovered else {
                    log::debug!("Tab drop ignored: pointer not over a tab row");
                    return false;
                };

                let target_tab_id = target_row.widget_name().to_string();
                if target_tab_id.trim().is_empty() {
                    log::warn!("Tab drop ignored: missing target tab ID");
                    return false;
                }

                let drop_for_read = drop.clone();
                let drop_for_finish = drop.clone();
                let inner_weak_async = Arc::downgrade(&inner);
                let target_tab_id_for_read = target_tab_id.clone();
                drop_for_read.read_value_async(
                    glib::Bytes::static_type(),
                    glib::Priority::DEFAULT,
                    None::<&gio::Cancellable>,
                    move |result| {
                        let Some(inner) = inner_weak_async.upgrade() else {
                            drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                            return;
                        };
                        match result {
                            Ok(value) => {
                                let Ok(bytes) = value.get::<glib::Bytes>() else {
                                    log::warn!("Tab drop failed: could not extract bytes from drop");
                                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                                    return;
                                };

                                let Some(payload) = tab_dnd::decode_drag_payload(&bytes) else {
                                    log::warn!("Tab drop failed: could not decode payload");
                                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                                    return;
                                };

                                let intent = resolve_sidebar_drop_intent(
                                    &payload.source_tab_id,
                                    &target_tab_id_for_read,
                                );
                                if intent == SidebarDropIntent::Noop {
                                    log::info!(
                                        "Tab drop ignored as no-op (source={}, target={})",
                                        payload.source_tab_id,
                                        target_tab_id_for_read
                                    );
                                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                                    return;
                                }

                                let inner_done = Arc::downgrade(&inner);
                                let drop_done = drop_for_finish.clone();
                                let source_tab_id = payload.source_tab_id.clone();
                                let target_tab_id = target_tab_id_for_read.clone();
                                let moved_count = payload.sound_ids.len();
                                let dispatch = commands::apply_sound_tab_drop_with_store_async(
                                    payload.source_tab_id.clone(),
                                    target_tab_id_for_read.clone(),
                                    payload.sound_ids.clone(),
                                    inner.state.library.clone(),
                                    move |result| match result {
                                        Ok(true) => {
                                            drop_done.finish(drag_action_for_intent(intent));
                                            if let Some(inner) = inner_done.upgrade() {
                                                inner.reload_tabs_and_emit(None);
                                                inner.emit_tab_membership_changed();
                                                inner.send_drop_toast(
                                                    intent,
                                                    &source_tab_id,
                                                    &target_tab_id,
                                                    moved_count,
                                                );
                                            }
                                        }
                                        Ok(false) => {
                                            log::info!(
                                                "Tab drop produced no membership changes (source={}, target={}, sounds={})",
                                                source_tab_id,
                                                target_tab_id,
                                                moved_count
                                            );
                                            drop_done.finish(gtk4::gdk::DragAction::empty());
                                        }
                                        Err(e) => {
                                            log::warn!("Tab drop failed: {e}");
                                            drop_done.finish(gtk4::gdk::DragAction::empty());
                                            if let Some(inner) = inner_done.upgrade() {
                                                inner.reload_tabs_and_emit(None);
                                                inner.emit_tab_membership_changed();
                                            }
                                        }
                                    },
                                );
                                if let Err(error) = dispatch {
                                    log::warn!("Failed to dispatch tab drop: {error}");
                                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                                }
                            }
                            Err(e) => {
                                log::warn!("Tab drop failed while reading payload: {e}");
                                drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                            }
                        }
                    },
                );

                true
            });
        }

        list_box.add_controller(drop_target);
    }

    fn emit_tab_membership_changed(&self) {
        if let Some(ref cb) = *self.on_tab_membership_changed.borrow() {
            cb();
        }
    }

    fn request_folder_merge(self: &Arc<Self>, request: FolderMergeRequest) {
        let library = self.state.library.clone();
        let scope = crate::library_store::LibraryScope::Folder {
            root_path: request.root_path.clone(),
            relative_path: request.source_relative_path.clone(),
        };
        let inner_weak = Arc::downgrade(self);
        // Move exactly the sounds counted in the prompt.
        if let Err(error) = commands::dispatch_async_result(
            "collect_folder_merge_sounds",
            move || collect_folder_sound_ids(&library, scope),
            move |result| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let sound_ids = match result {
                    Ok(sound_ids) => sound_ids,
                    Err(error) => {
                        log::warn!("Failed to read folder contents for combine: {error}");
                        inner
                            .dialog_host
                            .show_error("Failed to Combine Folders", &error.to_string());
                        return;
                    }
                };
                inner.confirm_folder_merge(request, sound_ids);
            },
        ) {
            log::warn!("Failed to dispatch folder combine: {error}");
        }
    }

    fn confirm_folder_merge(self: &Arc<Self>, request: FolderMergeRequest, sound_ids: Vec<String>) {
        let source_name = folder_display_label(&request.source_relative_path);
        let destination_name = folder_display_label(&request.destination_relative_path);
        if sound_ids.is_empty() {
            self.dialog_host.show_error(
                "Nothing to Combine",
                &format!("'{source_name}' has no sounds to move."),
            );
            return;
        }
        let count = sound_ids.len();
        let plural = if count == 1 { "sound" } else { "sounds" };
        let message = format!(
            "Move {count} {plural} from '{source_name}' into '{destination_name}'? \
             Files are not moved on disk."
        );
        let inner_weak = Arc::downgrade(self);
        let sound_ids = Rc::new(sound_ids);
        self.dialog_host
            .show_confirm("Combine Folders", &message, "Move", move || {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                inner.apply_folder_merge(request.clone(), sound_ids.as_ref().clone());
            });
    }

    fn apply_folder_merge(self: &Arc<Self>, request: FolderMergeRequest, sound_ids: Vec<String>) {
        let payload = tab_dnd::SoundTabDragPayload {
            source_tab_id: String::new(),
            source_folder: Some(tab_dnd::FolderDragContext {
                root_path: request.root_path.clone(),
                relative_path: request.source_relative_path.clone(),
            }),
            sound_ids,
        };
        let target = tab_dnd::FolderDragContext {
            root_path: request.root_path.clone(),
            relative_path: request.destination_relative_path.clone(),
        };
        let overrides = folder_drop_overrides(&payload, &target);
        if overrides.is_empty() {
            return;
        }
        let library = self.state.library.clone();
        let inner_weak = Arc::downgrade(self);
        if let Err(error) = commands::dispatch_async_result(
            "apply_folder_merge",
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
            move |result| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                match result {
                    Ok(()) => inner.emit_tab_membership_changed(),
                    Err(error) => {
                        log::warn!("Failed to combine folders: {error}");
                        inner
                            .dialog_host
                            .show_error("Failed to Combine Folders", &error.to_string());
                    }
                }
            },
        ) {
            log::warn!("Failed to dispatch folder combine: {error}");
        }
    }

    fn request_tab_deletion(self: &Arc<Self>, tab_id: String, tab_name: String) {
        if self.tab_deletion_pending.get() {
            return;
        }

        let inner_weak = Arc::downgrade(self);
        let message = format!("Delete tab '{tab_name}'? Sounds will not be removed.");
        self.dialog_host
            .show_confirm("Delete Tab", &message, "Delete", move || {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                if inner.tab_deletion_pending.replace(true) {
                    return;
                }

                let inner_weak_complete = Arc::downgrade(&inner);
                if let Err(err) = commands::delete_tab_with_store_async(
                    tab_id.clone(),
                    inner.state.library.clone(),
                    move |result| {
                        let Some(inner) = inner_weak_complete.upgrade() else {
                            return;
                        };
                        inner.tab_deletion_pending.set(false);
                        match result {
                            Ok(()) => {
                                *inner.active_tab_id.lock() = GENERAL_TAB_ID.to_string();
                                inner.queue_reload_tabs_and_emit(Some(GENERAL_TAB_ID.to_string()));
                            }
                            Err(err) => {
                                log::warn!("Delete tab failed: {err}");
                                inner
                                    .dialog_host
                                    .show_error("Failed to Delete Tab", &err.to_string());
                            }
                        }
                    },
                ) {
                    inner.tab_deletion_pending.set(false);
                    log::warn!("Failed to dispatch tab deletion: {err}");
                    inner
                        .dialog_host
                        .show_error("Failed to Delete Tab", &err.to_string());
                }
            });
    }

    /// Ask for a tab's hotkey, opening on whatever is already bound.
    fn prompt_tab_hotkey(self: &Arc<Self>, scope_key: String, tab_name: String) {
        let inner_weak = Arc::downgrade(self);
        let dialog_weak = self.dialog_host.downgrade();
        let binding_id = commands::tab_binding_id(&scope_key);

        let read =
            commands::hotkey_binding_async(binding_id, self.state.library.clone(), move |result| {
                let current = match result {
                    Ok(binding) => binding.map(|binding| binding.accelerator),
                    Err(error) => {
                        log::warn!("Could not read the tab's hotkey: {error}");
                        None
                    }
                };
                let (Some(inner), Some(dialog_host)) =
                    (inner_weak.upgrade(), dialog_weak.upgrade())
                else {
                    return;
                };
                let dialog_report = dialog_weak.clone();
                dialog_host.show_hotkey_capture(
                    current.as_deref(),
                    // A tab's own hotkey is live in every tab by definition.
                    None,
                    move |hotkey| {
                        crate::hotkeys::canonicalize_hotkey_string(hotkey)
                            .map(|_| ())
                            .map_err(|error| error.to_string())
                    },
                    move |hotkey, _scoped| {
                        let tab_name = tab_name.clone();
                        let dialog_done = dialog_report.clone();
                        let dispatch = commands::set_tab_hotkey_async(
                            scope_key.clone(),
                            hotkey.clone(),
                            inner.state.library.clone(),
                            inner.state.hotkey_projection.clone(),
                            move |result| match result {
                                Ok(()) => crate::ui_event_bridge::post_toast(match hotkey {
                                    Some(hotkey) => format!("{tab_name} opens with {hotkey}"),
                                    None => format!("{tab_name} has no hotkey"),
                                }),
                                Err(error) => {
                                    log::warn!("Set tab hotkey failed: {error}");
                                    if let Some(dialog_host) = dialog_done.upgrade() {
                                        dialog_host.show_error(
                                            "Failed to Set Tab Hotkey",
                                            &crate::hotkeys::format_hotkey_error(
                                                &error.to_string(),
                                            ),
                                        );
                                    }
                                }
                            },
                        );
                        if let Err(error) = dispatch {
                            log::warn!("Failed to dispatch the tab hotkey update: {error}");
                        }
                    },
                );
            });
        if let Err(error) = read {
            log::warn!("Failed to read the tab's hotkey: {error}");
        }
    }

    fn show_tab_context_menu(
        self: &Arc<Self>,
        widget: &Widget,
        x: f64,
        y: f64,
        tab_id: &str,
        tab_name: &str,
        editable: bool,
    ) {
        let tab_hotkeys = self.state.config.lock().settings.tab_hotkeys;

        let menu_model = gio::Menu::new();
        if editable {
            menu_model.append(Some("Rename Tab"), Some("tab-ctx.rename"));
            menu_model.append(Some("Delete Tab"), Some("tab-ctx.delete"));
        }
        if tab_hotkeys {
            menu_model.append(Some("Set Tab Hotkey"), Some("tab-ctx.hotkey"));
        }
        if menu_model.n_items() == 0 {
            return;
        }

        let action_group = gio::SimpleActionGroup::new();

        if tab_hotkeys {
            let inner_weak = Arc::downgrade(self);
            let scope = tab_scope_key(tab_id);
            let tab_name = tab_name.to_string();
            let action = gio::SimpleAction::new("hotkey", None);
            action.connect_activate(move |_, _| {
                let Some(inner_menu) = inner_weak.upgrade() else {
                    return;
                };
                inner_menu.prompt_tab_hotkey(scope.clone(), tab_name.clone());
            });
            action_group.add_action(&action);
        }

        if editable {
            let inner_weak = Arc::downgrade(self);
            let tab_id = tab_id.to_string();
            let tab_name = tab_name.to_string();
            let dialog_host = self.dialog_host.clone();
            let action = gio::SimpleAction::new("rename", None);
            action.connect_activate(move |_, _| {
                let Some(inner_menu) = inner_weak.upgrade() else {
                    return;
                };
                let inner_confirm_weak = Arc::downgrade(&inner_menu);
                let tab_id = tab_id.clone();
                dialog_host.show_input(
                    "Rename Tab",
                    "Enter a new name:",
                    &tab_name,
                    "Rename",
                    move |new_name| {
                        let Some(inner_confirm) = inner_confirm_weak.upgrade() else {
                            return;
                        };
                        let inner_done = Arc::downgrade(&inner_confirm);
                        let tab_id_done = tab_id.clone();
                        let dispatch = commands::rename_tab_with_store_async(
                            tab_id.clone(),
                            new_name,
                            inner_confirm.state.library.clone(),
                            move |result| match result {
                                Ok(_) => {
                                    if let Some(inner) = inner_done.upgrade() {
                                        inner.queue_reload_tabs_and_emit(Some(tab_id_done));
                                    }
                                }
                                Err(e) => {
                                    if let Some(inner) = inner_done.upgrade() {
                                        inner.queue_reload_tabs_and_emit(None);
                                    }
                                    log::warn!("Rename tab failed: {e}");
                                }
                            },
                        );
                        if let Err(error) = dispatch {
                            log::warn!("Failed to dispatch tab rename: {error}");
                        }
                    },
                );
            });
            action_group.add_action(&action);
        }

        if editable {
            let inner_weak = Arc::downgrade(self);
            let tab_id = tab_id.to_string();
            let tab_name = tab_name.to_string();
            let action = gio::SimpleAction::new("delete", None);
            action.connect_activate(move |_, _| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                inner.request_tab_deletion(tab_id.clone(), tab_name.clone());
            });
            action_group.add_action(&action);
        }

        menu::show_popover_menu(widget, "tab-ctx", &menu_model, &action_group, x, y);
    }
}

impl Clone for TabsSidebar {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_folder_drop_builds_sparse_move_overrides() {
        let payload = tab_dnd::SoundTabDragPayload {
            source_tab_id: "folder:source".to_string(),
            source_folder: Some(tab_dnd::FolderDragContext {
                root_path: "/music".to_string(),
                relative_path: "source".to_string(),
            }),
            sound_ids: vec!["one".to_string(), "two".to_string()],
        };
        let target = tab_dnd::FolderDragContext {
            root_path: "/music".to_string(),
            relative_path: "target".to_string(),
        };

        let overrides = folder_drop_overrides(&payload, &target);
        assert_eq!(overrides.len(), 4);
        assert_eq!(
            overrides
                .iter()
                .map(|record| (
                    record.folder_relative_path.as_str(),
                    record.sound_public_id.as_str(),
                    record.action
                ))
                .collect::<Vec<_>>(),
            [
                (
                    "target",
                    "one",
                    crate::library_store::FolderOverrideAction::Include
                ),
                (
                    "source",
                    "one",
                    crate::library_store::FolderOverrideAction::Exclude
                ),
                (
                    "target",
                    "two",
                    crate::library_store::FolderOverrideAction::Include
                ),
                (
                    "source",
                    "two",
                    crate::library_store::FolderOverrideAction::Exclude
                ),
            ]
        );
        assert!(folder_drop_overrides(
            &payload,
            payload.source_folder.as_ref().expect("source folder")
        )
        .is_empty());
    }

    #[test]
    fn should_request_next_sibling_page_triggers_near_loaded_end() {
        assert!(should_request_next_sibling_page(224, 256, true, false));
    }

    #[test]
    fn should_request_next_sibling_page_does_not_trigger_far_from_end() {
        assert!(!should_request_next_sibling_page(0, 256, true, false));
    }

    #[test]
    fn should_request_next_sibling_page_never_triggers_without_more_pages() {
        assert!(!should_request_next_sibling_page(255, 256, false, false));
    }

    #[test]
    fn should_request_next_sibling_page_never_triggers_while_in_flight() {
        assert!(!should_request_next_sibling_page(224, 256, true, true));
    }

    #[test]
    fn should_request_next_sibling_page_boundary_exactly_at_margin() {
        assert!(should_request_next_sibling_page(224, 256, true, false));
        assert!(!should_request_next_sibling_page(223, 256, true, false));
    }

    fn node_with_loaded_children(child_count: usize, grandchildren: usize) -> BoxedAnyObject {
        let parent = FolderNode::folder_at(
            "/music".to_string(),
            crate::library_store::FolderItem {
                id: 0,
                relative_path: "albums".to_string(),
                name: "albums".to_string(),
                expanded: true,
                has_children: true,
            },
            0,
            None,
        );
        for index in 0..child_count {
            let child = FolderNode::folder_at(
                "/music".to_string(),
                crate::library_store::FolderItem {
                    id: index as i64 + 1,
                    relative_path: format!("albums/Album {index}"),
                    name: format!("Album {index}"),
                    expanded: false,
                    has_children: grandchildren > 0,
                },
                index,
                None,
            );
            for deep in 0..grandchildren {
                child
                    .children()
                    .append(&BoxedAnyObject::new(FolderNode::folder_at(
                        "/music".to_string(),
                        crate::library_store::FolderItem {
                            id: 1_000 + deep as i64,
                            relative_path: format!("albums/Album {index}/Disc {deep}"),
                            name: format!("Disc {deep}"),
                            expanded: false,
                            has_children: false,
                        },
                        deep,
                        None,
                    )));
            }
            parent.children().append(&BoxedAnyObject::new(child));
        }
        BoxedAnyObject::new(parent)
    }

    #[test]
    fn counts_directly_loaded_child_rows() {
        let node = node_with_loaded_children(5, 0);
        let store = gio::ListStore::new::<BoxedAnyObject>();
        store.append(&node);
        // 1 parent row + its 5 loaded children.
        assert_eq!(count_loaded_child_rows(&store), 6);
    }

    #[test]
    fn counts_rows_loaded_under_nested_folders() {
        let node = node_with_loaded_children(3, 4);
        let store = gio::ListStore::new::<BoxedAnyObject>();
        store.append(&node);
        // 1 parent + 3 children + 3*4 grandchildren.
        assert_eq!(count_loaded_child_rows(&store), 16);
    }

    #[test]
    fn counts_nothing_for_an_empty_tree() {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        assert_eq!(count_loaded_child_rows(&store), 0);
    }

    #[test]
    fn a_failed_page_load_leaves_the_node_reloadable() {
        let temp_dir =
            std::env::temp_dir().join(format!("lsb-reloadable-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");

        let children_requested = Rc::new(Cell::new(true));
        let pager = Rc::new(SiblingPager {
            library,
            children: gio::ListStore::new::<BoxedAnyObject>(),
            root_path: "/music".to_string(),
            parent_relative_path: Some("albums".to_string()),
            loaded: Cell::new(0),
            next_page: Cell::new(0),
            has_more: Cell::new(false),
            in_flight: Cell::new(false),
            loaded_pages: RefCell::new(std::collections::BTreeSet::new()),
            focus_page: Cell::new(0),
            pending_pages: RefCell::new(std::collections::BTreeSet::new()),
            children_requested: Rc::clone(&children_requested),
        });

        pager.mark_reloadable();

        assert!(
            !children_requested.get(),
            "a failed load left the request latch set, so the create-closure \
             will never start a new pager and the folder stays empty"
        );
    }

    #[test]
    fn a_page_requested_while_busy_is_remembered() {
        let temp_dir =
            std::env::temp_dir().join(format!("lsb-pending-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");
        let pager = Rc::new(SiblingPager {
            library,
            children: gio::ListStore::new::<BoxedAnyObject>(),
            root_path: "/music".to_string(),
            parent_relative_path: None,
            loaded: Cell::new(0),
            next_page: Cell::new(0),
            has_more: Cell::new(true),
            in_flight: Cell::new(true),
            loaded_pages: RefCell::new(std::collections::BTreeSet::new()),
            focus_page: Cell::new(0),
            pending_pages: RefCell::new(std::collections::BTreeSet::new()),
            children_requested: Rc::new(Cell::new(true)),
        });

        TabsInner::load_sibling_page(Rc::clone(&pager), 5);

        assert!(
            pager.pending_pages.borrow().contains(&5),
            "a page requested while busy was dropped, so its rows stay blank"
        );
    }

    #[test]
    fn placeholder_rows_do_not_break_the_retention_walk() {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        store.append(&node_with_loaded_children(2, 0));
        store.append(&BoxedAnyObject::new(PlaceholderRow {
            sibling_index: 512,
            pager: std::rc::Weak::new(),
        }));
        // 1 parent + its 2 children + 1 placeholder.
        assert_eq!(count_loaded_child_rows(&store), 4);
    }

    #[test]
    fn keeps_every_page_while_within_the_window() {
        let pages: std::collections::BTreeSet<usize> = (0..4).collect();
        assert_eq!(page_to_evict(&pages, 2, 6), None);
    }

    #[test]
    fn drops_the_page_farthest_from_the_viewport() {
        let pages: std::collections::BTreeSet<usize> = [0, 5, 6, 7, 8, 9, 10].into_iter().collect();
        assert_eq!(page_to_evict(&pages, 7, 6), Some(0));
    }

    #[test]
    fn drops_pages_ahead_when_they_are_farther_than_pages_behind() {
        let pages: std::collections::BTreeSet<usize> = [0, 1, 2, 3, 4, 5, 20].into_iter().collect();
        assert_eq!(page_to_evict(&pages, 1, 6), Some(20));
    }

    #[test]
    fn breaks_distance_ties_by_dropping_the_lower_page() {
        let pages: std::collections::BTreeSet<usize> = [3, 4, 5, 6, 7].into_iter().collect();
        assert_eq!(page_to_evict(&pages, 5, 4), Some(3));
    }

    #[test]
    fn leaf_folders_allocate_no_child_store() {
        let leaf = FolderNode::folder_at(
            "/music".to_string(),
            crate::library_store::FolderItem {
                id: 1,
                relative_path: "albums/Album 1".to_string(),
                name: "Album 1".to_string(),
                expanded: false,
                has_children: false,
            },
            0,
            None,
        );
        assert!(
            leaf.loaded_children().is_none(),
            "leaf folder allocated a child store it can never use"
        );
    }

    #[test]
    fn folders_with_children_still_get_a_store_on_demand() {
        let parent = FolderNode::folder_at(
            "/music".to_string(),
            crate::library_store::FolderItem {
                id: 1,
                relative_path: "albums".to_string(),
                name: "albums".to_string(),
                expanded: false,
                has_children: true,
            },
            0,
            None,
        );
        let store = parent.children();
        store.append(&BoxedAnyObject::new(1u8));
        assert_eq!(
            parent.children().n_items(),
            1,
            "the lazily created store must be retained, not rebuilt per call"
        );
    }

    #[test]
    fn ignores_the_expansion_notification_storm_from_a_collapsing_parent() {
        assert!(!should_handle_expansion_change(false, false));
        assert!(!should_handle_expansion_change(true, true));
    }

    #[test]
    fn handles_a_real_expansion_change() {
        assert!(should_handle_expansion_change(true, false));
        assert!(should_handle_expansion_change(false, true));
    }

    #[test]
    fn dropping_below_a_later_row_does_not_overshoot() {
        assert_eq!(folder_reorder_target_index(1, 3, true), 3);
        assert_eq!(folder_reorder_target_index(1, 3, false), 2);
    }

    #[test]
    fn dropping_above_an_earlier_row_keeps_the_slot() {
        assert_eq!(folder_reorder_target_index(3, 1, false), 1);
        assert_eq!(folder_reorder_target_index(3, 1, true), 2);
    }

    #[test]
    fn a_folders_parent_comes_from_its_relative_path() {
        assert_eq!(folder_parent_relative_path("albumA"), None);
        assert_eq!(
            folder_parent_relative_path("albumA/disc1").as_deref(),
            Some("albumA")
        );
        assert_eq!(folder_parent_relative_path("a/b/c").as_deref(), Some("a/b"));
    }

    fn merge_payload(root: &str, relative: &str) -> tab_dnd::FolderDragPayload {
        tab_dnd::FolderDragPayload {
            root_path: root.to_string(),
            relative_path: relative.to_string(),
            parent_relative_path: folder_parent_relative_path(relative),
        }
    }

    #[test]
    fn folders_can_be_combined_across_different_parents() {
        let request =
            folder_merge_request(&merge_payload("/music", "albumA/disc1"), "/music", "albumB")
                .expect("a folder should combine into an unrelated folder");
        assert_eq!(request.source_relative_path, "albumA/disc1");
        assert_eq!(request.destination_relative_path, "albumB");
    }

    #[test]
    fn a_folder_can_be_combined_into_its_own_ancestor() {
        assert!(
            folder_merge_request(&merge_payload("/music", "albumA/disc1"), "/music", "albumA")
                .is_some()
        );
    }

    #[test]
    fn a_folder_cannot_be_combined_into_itself_or_its_own_subtree() {
        assert!(
            folder_merge_request(&merge_payload("/music", "albumA"), "/music", "albumA").is_none()
        );
        assert!(
            folder_merge_request(&merge_payload("/music", "albumA"), "/music", "albumA/disc1")
                .is_none()
        );
        // A prefix match that is not a path boundary is a different folder.
        assert!(
            folder_merge_request(&merge_payload("/music", "album"), "/music", "albumA").is_some()
        );
    }

    #[test]
    fn folders_from_different_roots_cannot_be_combined() {
        assert!(
            folder_merge_request(&merge_payload("/music", "albumA"), "/other", "albumB").is_none()
        );
    }

    #[test]
    fn tearing_the_tree_down_does_not_persist_collapse() {
        assert!(
            !should_persist_expansion_change(true, true),
            "a collapse caused by rebuilding the tree is not user intent"
        );
        assert!(
            should_persist_expansion_change(true, false),
            "a real user collapse must still be saved"
        );
        assert!(
            !should_persist_expansion_change(false, false),
            "an unchanged row writes nothing"
        );
    }

    #[test]
    fn collapsing_retains_children_while_under_the_row_cap() {
        // The common case: collapsing is free and re-expanding needs no query.
        assert!(!should_release_collapsed_children(1_000, 4_096));
    }

    #[test]
    fn collapsing_releases_children_once_over_the_row_cap() {
        assert!(should_release_collapsed_children(4_097, 4_096));
    }

    #[test]
    fn row_cap_boundary_retains_at_exactly_the_cap() {
        assert!(!should_release_collapsed_children(4_096, 4_096));
        assert!(should_release_collapsed_children(4_097, 4_096));
    }

    fn pump_until(done: impl Fn() -> bool) -> bool {
        let context = glib::MainContext::default();
        for _ in 0..2_000 {
            while context.iteration(false) {}
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        done()
    }

    #[test]
    #[ignore = "drives the shared GTK main context: needs a display and must \
                run alone, e.g. cargo test --lib -- --ignored --exact \
                ui::tabs_sidebar::tests::releasing_a_collapsed_node_reloads_its_children_on_reexpand"]
    #[allow(clippy::print_stderr)]
    fn releasing_a_collapsed_node_reloads_its_children_on_reexpand() {
        if gtk4::init().is_err() {
            eprintln!("skipped: no display available");
            return;
        }
        let temp_dir =
            std::env::temp_dir().join(format!("lsb-reexpand-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");
        let root_path = temp_dir.to_string_lossy().into_owned();

        library
            .apply_batch(crate::library_store::LibraryBatch::Roots(vec![
                crate::library_store::RootRecord {
                    path: root_path.clone(),
                    position: 0,
                },
            ]))
            .recv()
            .expect("insert root");
        let folders = (0..8usize)
            .map(|index| crate::library_store::FolderRecord {
                root_path: root_path.clone(),
                relative_path: format!("Album {index}"),
                parent_relative_path: None,
                name: format!("Album {index}"),
                position: index,
            })
            .collect();
        library
            .apply_batch(crate::library_store::LibraryBatch::Folders(folders))
            .recv()
            .expect("insert folders");

        let roots = gio::ListStore::new::<BoxedAnyObject>();
        let node = FolderNode::root(root_path.clone());
        let children = node.children();
        let children_pager = Rc::clone(&node.children_pager);
        let children_requested = Rc::clone(&node.children_requested);
        roots.append(&BoxedAnyObject::new(node));

        let library_for_children = library.clone();
        let tree = TreeListModel::new(roots.clone(), false, false, move |item| {
            let boxed = item.downcast_ref::<BoxedAnyObject>()?;
            let node = boxed.borrow::<FolderNode>();
            if !node.has_children {
                return None;
            }
            if !node.children_requested.replace(true) {
                TabsInner::start_children_pager(
                    library_for_children.clone(),
                    node.children(),
                    node.root_path.clone(),
                    node.relative_path.clone(),
                    &node.children_pager,
                    Rc::clone(&node.children_requested),
                );
            }
            Some(node.children().upcast())
        });

        let row = tree.row(0).expect("root row");
        row.set_expanded(true);
        assert!(
            pump_until(|| children.n_items() == 8),
            "first expand never loaded children, got {}",
            children.n_items()
        );

        // Exactly what the cap does when the loaded tree is over budget.
        row.set_expanded(false);
        children.remove_all();
        children_pager.replace(None);
        children_requested.set(false);

        row.set_expanded(true);
        assert!(
            pump_until(|| children.n_items() == 8),
            "re-expanding a released node did not reload its children, got {}",
            children.n_items()
        );
    }

    #[test]
    #[ignore = "drives the shared GTK main context: needs a display and must \
                run alone, e.g. cargo test --lib -- --ignored --exact \
                ui::tabs_sidebar::tests::scrolling_a_wide_folder_bounds_loaded_pages"]
    #[allow(clippy::print_stderr)]
    fn scrolling_a_wide_folder_bounds_loaded_pages() {
        if gtk4::init().is_err() {
            eprintln!("skipped: no display available");
            return;
        }
        const PAGE: usize = crate::library_store::PAGE_SIZE;
        const FOLDERS: usize = PAGE * (MAX_LOADED_SIBLING_PAGES + 4);

        let temp_dir =
            std::env::temp_dir().join(format!("lsb-window-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");
        let root_path = temp_dir.to_string_lossy().into_owned();
        library
            .apply_batch(crate::library_store::LibraryBatch::Roots(vec![
                crate::library_store::RootRecord {
                    path: root_path.clone(),
                    position: 0,
                },
            ]))
            .recv()
            .expect("insert root");
        for chunk_start in (0..FOLDERS).step_by(500) {
            let chunk = (chunk_start..(chunk_start + 500).min(FOLDERS))
                .map(|index| crate::library_store::FolderRecord {
                    root_path: root_path.clone(),
                    relative_path: format!("Album {index:05}"),
                    parent_relative_path: None,
                    name: format!("Album {index:05}"),
                    position: index,
                })
                .collect();
            library
                .apply_batch(crate::library_store::LibraryBatch::Folders(chunk))
                .recv()
                .expect("insert folders");
        }

        let node = FolderNode::root(root_path.clone());
        let children = node.children();
        let pager_slot = Rc::clone(&node.children_pager);
        let requested = Rc::clone(&node.children_requested);
        TabsInner::start_children_pager(
            library,
            children.clone(),
            root_path,
            None,
            &pager_slot,
            requested,
        );
        assert!(
            pump_until(|| children.n_items() as usize == PAGE),
            "first page never arrived"
        );
        let pager = pager_slot.borrow().clone().expect("pager installed");

        // Walk forward the way binding rows does, requesting each next page.
        for page in 1..(FOLDERS / PAGE) {
            pager.focus_page.set(page);
            TabsInner::load_folder_children_async(Rc::clone(&pager));
            assert!(
                pump_until(|| !pager.in_flight.get()),
                "page {page} never settled"
            );
        }

        assert_eq!(
            children.n_items() as usize,
            FOLDERS,
            "row count must stay at the full folder count so indices are stable"
        );
        assert!(
            pager.loaded_pages.borrow().len() <= MAX_LOADED_SIBLING_PAGES,
            "loaded pages {} exceeded the window of {}",
            pager.loaded_pages.borrow().len(),
            MAX_LOADED_SIBLING_PAGES
        );

        // Page 0 is farthest from the end: it must have become placeholders.
        let first = children
            .item(0)
            .and_downcast::<BoxedAnyObject>()
            .expect("row 0");
        assert!(
            first.try_borrow::<PlaceholderRow>().is_ok(),
            "row 0 should have been evicted to a placeholder"
        );
    }

    #[test]
    fn restores_persisted_expansion_only_on_first_bind() {
        assert!(should_restore_expansion(true, false));
        assert!(!should_restore_expansion(true, true));
    }

    #[test]
    fn never_forces_collapse_from_bind() {
        assert!(!should_restore_expansion(false, false));
        assert!(!should_restore_expansion(false, true));
    }

    #[test]
    fn expansion_latch_only_restores_on_first_bind_of_a_node() {
        let already_restored = Cell::new(false);
        let first_bind = should_restore_expansion(true, already_restored.replace(true));
        let second_bind = should_restore_expansion(true, already_restored.replace(true));
        assert_eq!([first_bind, second_bind], [true, false]);
    }

    #[test]
    fn sibling_pager_is_released_when_parent_drops() {
        let temp_dir =
            std::env::temp_dir().join(format!("lsb-tabs-sidebar-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&temp_dir).expect("create test dir");
        let library = crate::library_store::LibraryStore::open(temp_dir.join("library.sqlite3"))
            .expect("open disposable library store");

        let children = gio::ListStore::new::<BoxedAnyObject>();
        let pager = Rc::new(SiblingPager {
            library,
            children: children.clone(),
            root_path: "/music".to_string(),
            parent_relative_path: None,
            loaded: Cell::new(3),
            next_page: Cell::new(1),
            has_more: Cell::new(false),
            in_flight: Cell::new(false),
            loaded_pages: RefCell::new(std::collections::BTreeSet::new()),
            focus_page: Cell::new(0),
            pending_pages: RefCell::new(std::collections::BTreeSet::new()),
            children_requested: Rc::new(Cell::new(true)),
        });

        for index in 0..3usize {
            let item = crate::library_store::FolderItem {
                id: index as i64,
                relative_path: format!("albums/Album {index:05}"),
                name: format!("Album {index:05}"),
                expanded: false,
                has_children: false,
            };
            children.append(&BoxedAnyObject::new(FolderNode::folder_at(
                "/music".to_string(),
                item,
                index,
                Some(Rc::downgrade(&pager)),
            )));
        }

        let weak = Rc::downgrade(&pager);
        drop(pager);
        drop(children);

        assert!(
            weak.upgrade().is_none(),
            "SiblingPager was still reachable after all outside owners dropped it; \
             pager<->children<->FolderNode reference cycle is leaking"
        );

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    fn read_pss_kib() -> usize {
        let smaps = std::fs::read_to_string("/proc/self/smaps_rollup").expect("read smaps_rollup");
        smaps
            .lines()
            .find_map(|line| line.strip_prefix("Pss:"))
            .and_then(|value| value.split_whitespace().next())
            .and_then(|value| value.parse::<usize>().ok())
            .expect("parse PSS")
    }

    #[test]
    #[ignore = "manual measurement: needs a display, run under xvfb-run"]
    #[allow(clippy::print_stdout)]
    fn measure_retained_folder_child_row_cost() {
        if gtk4::init().is_err() {
            println!(
                "retained folder child row cost: skipped, gtk4::init() failed (no display available)"
            );
            return;
        }

        let root_path = "/home/flinux/Музика".to_string();

        for count in [1_000usize, 10_000, 50_000] {
            let before_kib = read_pss_kib();

            let store = gio::ListStore::new::<BoxedAnyObject>();
            for index in 0..count {
                let item = crate::library_store::FolderItem {
                    id: index as i64,
                    relative_path: format!("albums/Album {index:05}"),
                    name: format!("Album {index:05}"),
                    expanded: false,
                    has_children: false,
                };
                store.append(&BoxedAnyObject::new(FolderNode::folder(
                    root_path.clone(),
                    item,
                )));
            }

            let after_kib = read_pss_kib();
            drop(store);

            let delta_kib = after_kib.saturating_sub(before_kib);
            let bytes_per_row = (delta_kib * 1024) as f64 / count as f64;
            println!(
                "retained folder child row cost: n={count} delta_kib={delta_kib} bytes_per_row={bytes_per_row:.1}"
            );
        }
    }
}

#[cfg(test)]
mod tab_hotkey_tests {
    use super::tab_scope_key;
    use crate::app_meta::GENERAL_TAB_ID;

    #[test]
    fn general_and_manual_tabs_use_separate_scope_namespaces() {
        // Manual tabs carry a uuid, so the two can never produce the same key.
        assert_eq!(tab_scope_key(GENERAL_TAB_ID), "general");
        assert_eq!(
            tab_scope_key("41d0f0a4-6a1e-4a0e-9d2e-0b0f8a1d2c3e"),
            "tab:41d0f0a4-6a1e-4a0e-9d2e-0b0f8a1d2c3e"
        );
    }
}
