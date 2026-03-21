//! Tabs sidebar — GtkListBox showing General + user tabs.

use std::cell::RefCell;
use std::sync::{Arc, Mutex};

use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, Button, GestureClick, Label, ListBox, ListBoxRow, Orientation, ScrolledWindow,
    SelectionMode, Widget,
};

use crate::app_meta::GENERAL_TAB_ID;
use crate::app_state::AppState;
use crate::commands;

use super::dialogs;
use super::icons;
use super::menu;

/// Callback type for tab selection.
pub type TabSelectedCallback = Box<dyn Fn(String) + 'static>;

/// The tabs sidebar widget.
pub struct TabsSidebar {
    inner: Arc<TabsInner>,
}

struct TabsInner {
    scroll: ScrolledWindow,
    list_box: ListBox,
    state: Arc<AppState>,
    on_tab_selected: RefCell<Option<TabSelectedCallback>>,
    active_tab_id: Mutex<String>,
}

// Safety: TabsInner is only used on the GTK main thread.
unsafe impl Send for TabsInner {}
unsafe impl Sync for TabsInner {}

impl TabsSidebar {
    pub fn new(state: Arc<AppState>) -> Self {
        let vbox = GtkBox::new(Orientation::Vertical, 0);
        vbox.add_css_class("tabs-sidebar");
        vbox.set_width_request(196);

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

        let new_tab_btn = Button::builder()
            .tooltip_text("New Tab")
            .css_classes(vec!["flat", "circular"])
            .build();
        icons::apply_button_icon(&new_tab_btn, icons::ADD);

        header.append(&title_lbl);
        header.append(&new_tab_btn);
        vbox.append(&header);

        let list_box = ListBox::builder()
            .selection_mode(SelectionMode::Single)
            .css_classes(vec!["navigation-sidebar"])
            .build();
        vbox.append(&list_box);

        let scroll = ScrolledWindow::builder()
            .child(&vbox)
            .vexpand(true)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .build();

        let inner = Arc::new(TabsInner {
            scroll,
            list_box: list_box.clone(),
            state,
            on_tab_selected: RefCell::new(None),
            active_tab_id: Mutex::new(GENERAL_TAB_ID.to_string()),
        });

        {
            let inner_sel = Arc::clone(&inner);
            list_box.connect_row_selected(move |_, row| {
                if let Some(row) = row {
                    let id = row.widget_name().to_string();
                    *inner_sel.active_tab_id.lock().unwrap() = id.clone();
                    if let Some(ref cb) = *inner_sel.on_tab_selected.borrow() {
                        cb(id);
                    }
                }
            });
        }

        {
            let inner_btn = Arc::clone(&inner);
            new_tab_btn.connect_clicked(move |btn| {
                inner_btn.show_new_tab_dialog(btn);
            });
        }

        inner.reload_tabs_and_emit(None);

        Self { inner }
    }

    /// Return the root widget to embed in a parent container.
    pub fn widget(&self) -> &Widget {
        self.inner.scroll.upcast_ref()
    }

    /// Register a callback fired when the user selects a tab.
    pub fn connect_tab_selected<F: Fn(String) + Send + 'static>(&self, f: F) {
        *self.inner.on_tab_selected.borrow_mut() = Some(Box::new(f));
    }

    /// Reload the tab list from config, keeping the current selection.
    #[allow(dead_code)]
    pub fn reload_tabs(&self) {
        self.inner.reload_tabs_and_emit(None);
    }

    /// Reload and select a specific tab by ID.
    #[allow(dead_code)]
    pub fn reload_tabs_select(&self, tab_id: &str) {
        self.inner.reload_tabs_and_emit(Some(tab_id));
    }
}

impl TabsInner {
    fn show_new_tab_dialog(self: &Arc<Self>, button: &Button) {
        let Some(win) = button
            .root()
            .and_then(|root| root.downcast::<gtk4::Window>().ok())
        else {
            return;
        };

        let inner = Arc::clone(self);
        dialogs::show_input(
            &win,
            "New Tab",
            "Enter a name for the new tab:",
            "",
            "Create",
            move |name| match commands::create_tab(name, Arc::clone(&inner.state.config)) {
                Ok(tab) => {
                    *inner.active_tab_id.lock().unwrap() = tab.id.clone();
                    inner.reload_tabs_and_emit(Some(&tab.id));
                }
                Err(e) => log::warn!("Failed to create tab: {e}"),
            },
        );
    }

    fn reload_tabs_and_emit(self: &Arc<Self>, select_id: Option<&str>) {
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }

        let (tabs, active_id, total_sounds) = {
            let cfg = self.state.config.lock().unwrap();
            (
                cfg.tabs.clone(),
                self.active_tab_id.lock().unwrap().clone(),
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
        let mut child = self.list_box.first_child();
        while let Some(widget) = child {
            let next = widget.next_sibling();
            if let Ok(row) = widget.clone().downcast::<ListBoxRow>() {
                if row.widget_name() == tab_id {
                    self.list_box.select_row(Some(&row));
                    return true;
                }
            }
            child = next;
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

    fn show_tab_context_menu(
        self: &Arc<Self>,
        widget: &Widget,
        x: f64,
        y: f64,
        tab_id: &str,
        tab_name: &str,
    ) {
        let Some(win) = widget
            .root()
            .and_then(|root| root.downcast::<gtk4::Window>().ok())
        else {
            return;
        };

        let menu_model = gio::Menu::new();
        menu_model.append(Some("Rename Tab"), Some("tab-ctx.rename"));
        menu_model.append(Some("Delete Tab"), Some("tab-ctx.delete"));

        let action_group = gio::SimpleActionGroup::new();

        {
            let inner = Arc::clone(self);
            let win = win.clone();
            let tab_id = tab_id.to_string();
            let tab_name = tab_name.to_string();
            let action = gio::SimpleAction::new("rename", None);
            action.connect_activate(move |_, _| {
                let inner_confirm = Arc::clone(&inner);
                let tab_id = tab_id.clone();
                dialogs::show_input(
                    &win,
                    "Rename Tab",
                    "Enter a new name:",
                    &tab_name,
                    "Rename",
                    move |new_name| match commands::rename_tab(
                        tab_id.clone(),
                        new_name,
                        Arc::clone(&inner_confirm.state.config),
                    ) {
                        Ok(_) => inner_confirm.reload_tabs_and_emit(Some(&tab_id)),
                        Err(e) => log::warn!("Rename tab failed: {e}"),
                    },
                );
            });
            action_group.add_action(&action);
        }

        {
            let inner = Arc::clone(self);
            let win = win.clone();
            let tab_id = tab_id.to_string();
            let tab_name = tab_name.to_string();
            let action = gio::SimpleAction::new("delete", None);
            action.connect_activate(move |_, _| {
                let inner_confirm = Arc::clone(&inner);
                let tab_id = tab_id.clone();
                let message = format!("Delete tab '{tab_name}'? Sounds will not be removed.");
                dialogs::show_confirm(&win, "Delete Tab", &message, "Delete", move || {
                    match commands::delete_tab(
                        tab_id.clone(),
                        Arc::clone(&inner_confirm.state.config),
                    ) {
                        Ok(_) => {
                            *inner_confirm.active_tab_id.lock().unwrap() =
                                GENERAL_TAB_ID.to_string();
                            inner_confirm.reload_tabs_and_emit(Some(GENERAL_TAB_ID));
                        }
                        Err(e) => log::warn!("Delete tab failed: {e}"),
                    }
                });
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
