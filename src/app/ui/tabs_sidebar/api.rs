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
