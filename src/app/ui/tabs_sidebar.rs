use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, GestureClick, Label, ListBox, ListBoxRow, ListView, Orientation, ScrolledWindow,
    SelectionMode, SignalListItemFactory, SingleSelection, TreeExpander, TreeListModel,
    TreeListRow, Widget,
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
    name: String,
    has_children: bool,
    children: gio::ListStore,
    children_requested: Cell<bool>,
}

impl FolderNode {
    fn root(path: String) -> Self {
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| path.clone());
        Self {
            root_path: path,
            relative_path: None,
            name,
            has_children: true,
            children: gio::ListStore::new::<BoxedAnyObject>(),
            children_requested: Cell::new(false),
        }
    }

    fn folder(root_path: String, item: crate::library_store::FolderItem) -> Self {
        Self {
            root_path,
            relative_path: Some(item.relative_path),
            name: item.name,
            has_children: item.has_children,
            children: gio::ListStore::new::<BoxedAnyObject>(),
            children_requested: Cell::new(false),
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

pub struct TabsSidebar {
    inner: Arc<TabsInner>,
}

struct TabsInner {
    scroll: ScrolledWindow,
    list_box: ListBox,
    folder_roots: gio::ListStore,
    folder_generation: Rc<Cell<u64>>,
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
        vbox.append(&list_box);

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
        let library_for_children = state.library.clone();
        let folder_tree = TreeListModel::new(folder_roots.clone(), false, false, move |item| {
            let boxed = item.downcast_ref::<BoxedAnyObject>()?;
            let node = boxed.borrow::<FolderNode>();
            if !node.has_children {
                return None;
            }
            if !node.children_requested.replace(true) {
                TabsInner::load_folder_children_async(
                    library_for_children.clone(),
                    node.children.clone(),
                    node.root_path.clone(),
                    node.relative_path.clone(),
                    0,
                );
            }
            Some(node.children.clone().upcast())
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
            let expander = TreeExpander::new();
            expander.set_child(Some(&label));
            item.set_child(Some(&expander));
        });
        folder_factory.connect_bind(|_, item| {
            let Some(item) = item.downcast_ref::<gtk4::ListItem>() else {
                return;
            };
            let Some(row) = item.item().and_downcast::<TreeListRow>() else {
                return;
            };
            let Some(expander) = item.child().and_downcast::<TreeExpander>() else {
                return;
            };
            let Some(label) = expander.child().and_downcast::<Label>() else {
                return;
            };
            let Some(boxed) = row.item().and_downcast::<BoxedAnyObject>() else {
                return;
            };
            label.set_label(&boxed.borrow::<FolderNode>().name);
            expander.set_list_row(Some(&row));
        });
        let folder_view = ListView::new(Some(folder_selection.clone()), Some(folder_factory));
        folder_view.add_css_class("navigation-sidebar");
        vbox.append(&folder_view);

        let scroll = ScrolledWindow::builder()
            .child(&vbox)
            .vexpand(true)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .build();

        let inner = Arc::new(TabsInner {
            scroll,
            list_box: list_box.clone(),
            folder_roots,
            folder_generation,
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

        inner.reload_tabs_now(None);
        inner.connect_delete_shortcut();
        inner.attach_sidebar_drop_target(&list_box);

        Self { inner }
    }

    pub fn widget(&self) -> &Widget {
        self.inner.scroll.upcast_ref()
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
        glib::timeout_add_local(Duration::from_millis(2), move || {
            if generation.get() != expected_generation {
                return glib::ControlFlow::Break;
            }
            match response.try_recv() {
                Ok(Some(result)) => {
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
                    glib::ControlFlow::Break
                }
                Ok(None) => glib::ControlFlow::Continue,
                Err(error) => {
                    log::warn!("Failed to load sound folder roots: {error}");
                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn load_folder_children_async(
        library: crate::library_store::LibraryStore,
        model: gio::ListStore,
        root_path: String,
        parent_relative_path: Option<String>,
        page: usize,
    ) {
        let response = library.folder_children(&root_path, parent_relative_path.as_deref(), page);
        glib::timeout_add_local(Duration::from_millis(2), move || {
            match response.try_recv() {
                Ok(Some(result)) => {
                    let count = result.folders.len();
                    for folder in result.folders {
                        model.append(&BoxedAnyObject::new(FolderNode::folder(
                            root_path.clone(),
                            folder,
                        )));
                    }
                    if count == crate::library_store::PAGE_SIZE {
                        Self::load_folder_children_async(
                            library.clone(),
                            model.clone(),
                            root_path.clone(),
                            parent_relative_path.clone(),
                            page.saturating_add(1),
                        );
                    }
                    glib::ControlFlow::Break
                }
                Ok(None) => glib::ControlFlow::Continue,
                Err(error) => {
                    log::warn!("Failed to load sound folder children: {error}");
                    glib::ControlFlow::Break
                }
            }
        });
    }

    fn reload_folder_roots(&self) {
        let next_generation = self.folder_generation.get().wrapping_add(1);
        self.folder_generation.set(next_generation);
        self.folder_roots.remove_all();
        Self::load_roots_async(
            self.state.library.clone(),
            self.folder_roots.clone(),
            0,
            Rc::clone(&self.folder_generation),
            next_generation,
        );
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
            inner.reload_tabs_now(select_id.as_deref());
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
                match commands::create_tab_with_store(
                    name,
                    Arc::clone(&inner.state.config),
                    inner.state.library.clone(),
                ) {
                    Ok(tab) => {
                        *inner.active_tab_id.lock() = tab.id.clone();
                        inner.queue_reload_tabs_and_emit(Some(tab.id));
                    }
                    Err(e) => log::warn!("Failed to create tab: {e}"),
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
            inner.reload_tabs_now(select_id.as_deref());
        });
    }

    fn reload_tabs_now(self: &Arc<Self>, select_id: Option<&str>) {
        self.reload_folder_roots();
        self.list_box.select_row(None::<&ListBoxRow>);
        while let Some(row) = self.list_box.row_at_index(0) {
            self.list_box.remove(&row);
        }

        let (tabs, active_id, total_sounds) = {
            let cfg = self.state.config.lock();
            (
                cfg.tabs.clone(),
                self.active_tab_id.lock().clone(),
                cfg.sounds.len(),
            )
        };

        self.list_box.append(&self.make_tab_row(
            GENERAL_TAB_ID,
            "General",
            icons::FOLDER_OPEN,
            total_sounds,
            false,
        ));

        let mut sorted_tabs = tabs;
        sorted_tabs.sort_by_key(|tab| tab.order);
        for tab in &sorted_tabs {
            self.list_box.append(&self.make_tab_row(
                &tab.id,
                &tab.name,
                icons::FOLDER,
                tab.sound_ids.len(),
                true,
            ));
        }

        let target_id = select_id.unwrap_or(&active_id).to_string();
        if !self.select_row_by_id(&target_id) {
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

        let cfg = self.state.config.lock();
        cfg.tabs
            .iter()
            .find(|tab| tab.id == tab_id)
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

                                match commands::apply_sound_tab_drop_with_store(
                                    payload.source_tab_id.clone(),
                                    target_tab_id_for_read.clone(),
                                    payload.sound_ids.clone(),
                                    Arc::clone(&inner.state.config),
                                    inner.state.library.clone(),
                                ) {
                                    Ok(changed) => {
                                        if !changed {
                                            log::info!(
                                                "Tab drop produced no membership changes (source={}, target={}, sounds={})",
                                                payload.source_tab_id,
                                                target_tab_id_for_read,
                                                payload.sound_ids.len()
                                            );
                                            drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                                            return;
                                        }

                                        let finish_action = drag_action_for_intent(intent);
                                        drop_for_finish.finish(finish_action);

                                        let inner_weak_refresh = Arc::downgrade(&inner);
                                        let source_tab_id = payload.source_tab_id.clone();
                                        let target_tab_id = target_tab_id_for_read.clone();
                                        let moved_count = payload.sound_ids.len();
                                        glib::idle_add_local_once(move || {
                                            let Some(inner) = inner_weak_refresh.upgrade() else {
                                                return;
                                            };
                                            inner.reload_tabs_and_emit(None);
                                            inner.emit_tab_membership_changed();
                                            inner.send_drop_toast(
                                                intent,
                                                &source_tab_id,
                                                &target_tab_id,
                                                moved_count,
                                            );
                                        });
                                    }
                                    Err(e) => {
                                        log::warn!("Tab drop failed: {e}");
                                        drop_for_finish.finish(gtk4::gdk::DragAction::empty());
                                    }
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
                    Arc::clone(&inner.state.config),
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
                        match commands::rename_tab_with_store(
                            tab_id.clone(),
                            new_name,
                            Arc::clone(&inner_confirm.state.config),
                            inner_confirm.state.library.clone(),
                        ) {
                            Ok(_) => inner_confirm.queue_reload_tabs_and_emit(Some(tab_id.clone())),
                            Err(e) => log::warn!("Rename tab failed: {e}"),
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
