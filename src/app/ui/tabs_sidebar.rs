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
    /// Child rows, allocated only for folders that can actually have children.
    /// An empty `gio::ListStore` costs ~186 bytes, and in a wide library the
    /// overwhelming majority of folders are leaves that would never use one.
    children: RefCell<Option<gio::ListStore>>,
    children_requested: Rc<Cell<bool>>,
    expanded: Rc<Cell<bool>>,
    /// The row-expansion handler is attached to a `TreeListRow`, and those
    /// rows are created/destroyed as the virtualized tree scrolls -- a node
    /// gets rebound onto a different row each time. Storing `(row, handler)`
    /// here (mirroring `disclosure_handlers` below) lets `connect_unbind`
    /// disconnect the handler from the exact row it was attached to, so a
    /// stale handler never keeps mutating this node's state after rebind.
    expanded_handler: RefCell<Option<(TreeListRow, glib::SignalHandlerId)>>,
    /// Whether persisted expansion has already been applied to a
    /// `TreeListRow` for this node. Set on the node's first bind and never
    /// cleared. Writing `TreeListRow::set_expanded` from `connect_bind` on
    /// every bind (not just the first) re-expands rows during GTK's own
    /// collapse propagation: `gtk_tree_list_row_set_expanded()` emits
    /// `items-changed` on the tree model before it emits the `expanded`
    /// property notification, so a collapse click can cause GTK to rebind
    /// this node onto a visible row before `connect_expanded_notify` has run
    /// and updated `expanded` -- at which point re-applying the still-stale
    /// `true` value would immediately re-expand the row the user just
    /// collapsed.
    expansion_restored: Cell<bool>,
    disclosure_handlers: RefCell<Option<(Image, GestureClick, TreeListRow, glib::SignalHandlerId)>>,
    context_gesture: RefCell<Option<GestureClick>>,
    drop_target: RefCell<Option<gtk4::DropTargetAsync>>,
    /// Position of this node among its loaded siblings; used to decide when
    /// scrolling has gotten close enough to the end to prefetch more.
    sibling_index: usize,
    /// Pager that loads more of this node's siblings on demand. `None` for
    /// roots (there are few roots, so they are still loaded eagerly). Weak
    /// because the pager's `children` store holds this node: a strong ref
    /// here would create pager -> children -> node -> pager cycle that never
    /// frees. The only strong owner is the parent node's `children_pager`.
    sibling_pager: Option<std::rc::Weak<SiblingPager>>,
    /// Pager that loads this node's own children, one page at a time.
    /// Created lazily on first expand and cleared on collapse.
    children_pager: Rc<RefCell<Option<Rc<SiblingPager>>>>,
}

/// Shared, single-page-at-a-time loader for one folder's children. All
/// siblings under the same parent hold an `Rc` to the same pager so any of
/// them can trigger the next page as the user scrolls near the loaded end.
struct SiblingPager {
    library: crate::library_store::LibraryStore,
    children: gio::ListStore,
    root_path: String,
    parent_relative_path: Option<String>,
    loaded: Cell<usize>,
    next_page: Cell<usize>,
    has_more: Cell<bool>,
    in_flight: Cell<bool>,
    /// The owning node's "children already requested" latch. Held so a failed
    /// page load can clear it; see [`SiblingPager::mark_reloadable`].
    children_requested: Rc<Cell<bool>>,
}

impl SiblingPager {
    /// A failed page load must not poison the node. The `TreeListModel`
    /// create-closure only starts a pager when the node's request latch is
    /// clear, so leaving it set after a failure means the folder stays empty
    /// for the rest of the session no matter how often it is expanded.
    fn mark_reloadable(&self) {
        self.children_requested.set(false);
    }
}

/// How many unrendered rows are allowed to remain between the last bound
/// sibling and the end of what's loaded before the next page is requested.
const SIBLING_PREFETCH_MARGIN: usize = 32;

fn should_request_next_sibling_page(
    child_index: usize,
    loaded: usize,
    more: bool,
    in_flight: bool,
) -> bool {
    more && !in_flight && child_index + SIBLING_PREFETCH_MARGIN >= loaded
}

/// Persisted expansion is applied to a `TreeListRow` only on a node's first
/// bind. Re-applying it on later binds re-expands rows during GTK's collapse
/// propagation, because `items-changed` is emitted before the `expanded`
/// notification.
fn should_restore_expansion(node_expanded: bool, already_restored: bool) -> bool {
    node_expanded && !already_restored
}

/// Ceiling on folder child rows kept loaded across the whole sidebar.
/// Collapsing normally retains its children so re-expanding needs no query;
/// once the loaded tree exceeds this, collapsing releases them instead.
/// 4096 rows is ~3 MB at the measured ~750 bytes per row, in the same range as
/// the lazy sound model's 2 MiB payload bound.
const MAX_RETAINED_CHILD_ROWS: usize = 4_096;

/// Total loaded child rows under `store`, including nested expanded folders.
/// Walking is preferred over a running counter because rows leave in ways a
/// counter cannot observe: rebuilding the tree drops every node, and releasing
/// one node also drops its descendants' loaded rows. The walk is bounded by
/// `MAX_RETAINED_CHILD_ROWS` and only runs when the user collapses a folder.
fn count_loaded_child_rows(store: &gio::ListStore) -> usize {
    let mut total = 0usize;
    for item in store.iter::<BoxedAnyObject>().flatten() {
        total += 1;
        let children = item.borrow::<FolderNode>().loaded_children();
        if let Some(children) = children {
            total += count_loaded_child_rows(&children);
        }
    }
    total
}

fn should_release_collapsed_children(total_retained_rows: usize, cap: usize) -> bool {
    total_retained_rows > cap
}

/// Collapsing a folder makes GTK emit `expanded` notifications for every
/// loaded descendant row as it tears them down, even though those folders were
/// never expanded and their state does not change. Acting on them once cost a
/// database write and a full tree walk per row, which saturated the library
/// worker queue and made the collapsing folder's own reload fail.
fn should_handle_expansion_change(previous: bool, next: bool) -> bool {
    previous != next
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
    /// The child store, created on first use and retained from then on. Only
    /// reached for nodes that report `has_children`, so leaf folders never
    /// allocate one.
    fn children(&self) -> gio::ListStore {
        self.children
            .borrow_mut()
            .get_or_insert_with(gio::ListStore::new::<BoxedAnyObject>)
            .clone()
    }

    /// The child store only if it has already been created. Used by the
    /// retention walk, which must not allocate stores just to count them.
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
            sibling_index: 0,
            sibling_pager: None,
            children_pager: Rc::new(RefCell::new(None)),
        }
    }

    /// Only used by the retained-row memory measurement test; production
    /// code always goes through [`FolderNode::folder_at`] so it can carry
    /// sibling paging context.
    #[cfg(test)]
    fn folder(root_path: String, item: crate::library_store::FolderItem) -> Self {
        Self::folder_at(root_path, item, 0, None)
    }

    /// Like [`FolderNode::folder`], but also records this node's position
    /// among its siblings and the shared pager that can load more of them.
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

fn install_folder_drop_target(
    widget: &GtkBox,
    library: crate::library_store::LibraryStore,
    root_path: String,
    relative_path: String,
    on_changed: FolderChangedCallback,
) -> gtk4::DropTargetAsync {
    let formats = gtk4::gdk::ContentFormats::builder()
        .add_type(glib::Bytes::static_type())
        .add_mime_type(tab_dnd::SOUND_TAB_DND_MIME)
        .build();
    let target = gtk4::DropTargetAsync::new(Some(formats), gtk4::gdk::DragAction::COPY);
    target.connect_drop(move |_, drop, _, _| {
        let drop_for_read = drop.clone();
        let drop_for_finish = drop.clone();
        let library = library.clone();
        let root_path = root_path.clone();
        let relative_path = relative_path.clone();
        let on_changed = Rc::clone(&on_changed);
        drop_for_read.read_value_async(
            glib::Bytes::static_type(),
            glib::Priority::DEFAULT,
            None::<&gio::Cancellable>,
            move |result| {
                let Ok(value) = result else {
                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                    return;
                };
                let Ok(bytes) = value.get::<glib::Bytes>() else {
                    drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                    return;
                };
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
        let folder_changed: FolderChangedCallback = Rc::new(RefCell::new(None));
        let library_for_children = state.library.clone();
        let folder_tree = TreeListModel::new(folder_roots.clone(), false, false, move |item| {
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
            row_box.append(&disclosure);
            row_box.append(&expander);
            item.set_child(Some(&row_box));
        });
        let library_for_expansion = state.library.clone();
        let folder_roots_for_expansion = folder_roots.clone();
        let dialog_host_for_folders = dialog_host.clone();
        let folder_roots_for_actions = folder_roots.clone();
        let folder_generation_for_actions = Rc::clone(&folder_generation);
        let folder_changed_for_drop = Rc::clone(&folder_changed);
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
            let node = boxed.borrow::<FolderNode>();
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
                boxed
                    .borrow::<FolderNode>()
                    .disclosure_handlers
                    .replace(Some((
                        disclosure.clone(),
                        gesture,
                        row.clone(),
                        expansion_handler,
                    )));
            }
            if connect_expanded {
                let library = library_for_expansion.clone();
                let expanded_root_path = root_path.clone();
                let loaded_tree = folder_roots_for_expansion.clone();
                let expansion_handler = row.connect_expanded_notify(move |row| {
                    let is_expanded = row.is_expanded();
                    if !should_handle_expansion_change(expanded.replace(is_expanded), is_expanded) {
                        return;
                    }
                    // Collapsed children normally stay loaded: GtkTreeListModel hides
                    // them, and retaining them (~750 bytes per row, see
                    // `measure_retained_folder_child_row_cost`) is far cheaper than
                    // re-querying a page on every re-expand. Only once the loaded tree
                    // exceeds MAX_RETAINED_CHILD_ROWS does collapsing give rows back;
                    // the create-closure reloads them the next time it is expanded.
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
                boxed
                    .borrow::<FolderNode>()
                    .expanded_handler
                    .replace(Some((row.clone(), expansion_handler)));
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
                    let action_group = gio::SimpleActionGroup::new();
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
                        action.connect_activate(move |_, _| {
                            let response =
                                library.move_folder(&root_path, &relative_path, direction);
                            let library = library.clone();
                            let folder_roots = folder_roots.clone();
                            let folder_generation = Rc::clone(&folder_generation);
                            if let Err(error) = commands::dispatch_async_result(
                                "move_sidebar_folder",
                                move || response.recv(),
                                move |result| match result {
                                    Ok(true) => TabsInner::reload_folder_roots_model(
                                        library,
                                        folder_roots,
                                        folder_generation,
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
                boxed
                    .borrow::<FolderNode>()
                    .context_gesture
                    .replace(Some(gesture));
            }
            if install_drop_target {
                let Some(relative_path) = drop_relative_path else {
                    return;
                };
                let target = install_folder_drop_target(
                    &row_box,
                    library_for_expansion.clone(),
                    root_path,
                    relative_path,
                    Rc::clone(&folder_changed_for_drop),
                );
                boxed
                    .borrow::<FolderNode>()
                    .drop_target
                    .replace(Some(target));
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
            let (disclosure_handlers, expanded_handler, gesture, drop_target) = {
                let node = boxed.borrow::<FolderNode>();
                let disclosure_handlers = node.disclosure_handlers.borrow_mut().take();
                let expanded_handler = node.expanded_handler.borrow_mut().take();
                let gesture = node.context_gesture.borrow_mut().take();
                let drop_target = node.drop_target.borrow_mut().take();
                (disclosure_handlers, expanded_handler, gesture, drop_target)
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
                let node = boxed.borrow::<FolderNode>();
                let Some(relative_path) = node.relative_path.clone() else {
                    // Root rows are not a selectable scope. Do not toggle expansion here:
                    // the click that selects this row has already run the disclosure
                    // gesture's toggle, so toggling again flips the row straight back and
                    // discards the children the collapse branch just released.
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

    /// Creates a fresh [`SiblingPager`] for a folder node's children, stores
    /// it in `pager_slot`, and kicks off loading page 0. Called once, from
    /// the `TreeListModel` create-closure, the first time a node's children
    /// are requested.
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
            children_requested,
        });
        pager_slot.replace(Some(Rc::clone(&pager)));
        Self::load_folder_children_async(pager);
    }

    /// Loads exactly one page of a folder's children into `pager`'s shared
    /// list store. Call again (via [`should_request_next_sibling_page`]) to
    /// fetch subsequent pages as the user scrolls near the loaded end.
    fn load_folder_children_async(pager: Rc<SiblingPager>) {
        if pager.in_flight.replace(true) {
            log::warn!("Sidebar sibling page request already in flight; ignoring");
            return;
        }
        let page = pager.next_page.get();
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
                    let start_index = pager_for_result.loaded.get();
                    let count = result.folders.len();
                    for (offset, folder) in result.folders.into_iter().enumerate() {
                        pager_for_result.children.append(&BoxedAnyObject::new(
                            FolderNode::folder_at(
                                pager_for_result.root_path.clone(),
                                folder,
                                start_index + offset,
                                Some(Rc::downgrade(&pager_for_result)),
                            ),
                        ));
                    }
                    pager_for_result.loaded.set(start_index + count);
                    pager_for_result.next_page.set(page.saturating_add(1));
                    pager_for_result
                        .has_more
                        .set(count == crate::library_store::PAGE_SIZE);
                    pager_for_result.in_flight.set(false);
                }
                Err(error) => {
                    log::warn!("Failed to load sound folder children: {error}");
                    pager_for_result.has_more.set(false);
                    pager_for_result.in_flight.set(false);
                    // Transient store failures (a saturated worker queue, for
                    // one) must not leave this folder empty for good.
                    pager_for_result.mark_reloadable();
                }
            },
        ) {
            log::warn!("Failed to dispatch sound folder child load: {error}");
            pager.in_flight.set(false);
            pager.mark_reloadable();
        }
    }

    fn reload_folder_roots(&self) {
        Self::reload_folder_roots_model(
            self.state.library.clone(),
            self.folder_roots.clone(),
            Rc::clone(&self.folder_generation),
        );
    }

    fn reload_folder_roots_model(
        library: crate::library_store::LibraryStore,
        folder_roots: gio::ListStore,
        folder_generation: Rc<Cell<u64>>,
    ) {
        let next_generation = folder_generation.get().wrapping_add(1);
        folder_generation.set(next_generation);
        folder_roots.remove_all();
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
            let tab_name = {
                let config = inner.state.config.lock();
                config.get_tab(&tab_id).map(|tab| tab.name.clone())
            };
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

        if editable {
            self.attach_tab_context_menu(&row, id.to_string(), name.to_string());
        }

        row
    }

    fn attach_tab_context_menu(
        self: &Arc<Self>,
        row: &ListBoxRow,
        tab_id: String,
        tab_name: String,
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
            inner.show_tab_context_menu(&widget, x, y, &tab_id, &tab_name);
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

    fn show_tab_context_menu(
        self: &Arc<Self>,
        widget: &Widget,
        x: f64,
        y: f64,
        tab_id: &str,
        tab_name: &str,
    ) {
        let menu_model = gio::Menu::new();
        menu_model.append(Some("Rename Tab"), Some("tab-ctx.rename"));
        menu_model.append(Some("Delete Tab"), Some("tab-ctx.delete"));

        let action_group = gio::SimpleActionGroup::new();

        {
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

        {
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
        // 32 rows from the end of what's loaded, more pages exist, nothing in flight.
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
        // margin is 32: index 224 is exactly loaded(256) - margin, should trigger;
        // index 223 is one short of the margin, should not.
        assert!(should_request_next_sibling_page(224, 256, true, false));
        assert!(!should_request_next_sibling_page(223, 256, true, false));
    }

    /// Builds a node holding `child_count` loaded children, each of which may
    /// itself hold `grandchildren` loaded children.
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
        // This is the case a running counter gets wrong: releasing the parent
        // also drops the grandchildren, so they must be counted as retained.
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

    /// A transient store failure (the live one was "library worker queue is
    /// full") must not leave the folder permanently empty.
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
    fn leaf_folders_allocate_no_child_store() {
        // An empty gio::ListStore costs ~186 bytes. In a wide library almost
        // every folder is a leaf, so allocating one per node is pure waste.
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
        // GTK notifies every descendant row as it tears them down. Those
        // folders were never expanded, so false -> false must do nothing:
        // acting on them flooded the library queue and broke the real reload.
        assert!(!should_handle_expansion_change(false, false));
        assert!(!should_handle_expansion_change(true, true));
    }

    #[test]
    fn handles_a_real_expansion_change() {
        assert!(should_handle_expansion_change(true, false));
        assert!(should_handle_expansion_change(false, true));
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

    /// Drives the real GTK main context until `done` holds or the budget runs
    /// out, so async store replies land the way they do in the running app.
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

    /// Reproduces the sidebar's real expansion wiring: a `TreeListModel` whose
    /// create-closure starts a pager the first time a node's children are
    /// requested, exactly as `TabsInner::build` does.
    ///
    /// This must stay the only test that calls `gtk4::init`. The harness runs
    /// each test on its own thread even under `--test-threads=1`, and GTK
    /// panics if a second thread initializes it.
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
