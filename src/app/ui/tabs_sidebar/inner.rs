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

    fn load_folder_children_async(pager: Rc<SiblingPager>) {
        let page = pager.next_page.get();
        Self::load_sibling_page(pager, page);
    }

    fn load_sibling_page(pager: Rc<SiblingPager>, page: usize) {
        if pager.loaded_pages.borrow().contains(&page) {
            return;
        }
        if pager.in_flight.replace(true) {
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

    fn drain_pending_sibling_page(pager: &Rc<SiblingPager>) {
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

        label.set_widget_name(TAB_NAME_LABEL);

        hbox.append(&icon);
        hbox.append(&label);

        if sound_count > 0 {
            let badge = Label::builder()
                .label(sound_count.to_string())
                .css_classes(vec!["tab-count-badge"])
                .valign(gtk4::Align::Center)
                .yalign(0.5)
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
