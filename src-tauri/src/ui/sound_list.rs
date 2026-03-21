//! Sound list widget — GtkColumnView with gio::ListStore model.

use std::cell::RefCell;
use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use gio::prelude::*;
use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, ColumnView, ColumnViewColumn, GestureClick, Label, MultiSelection, Orientation,
    ScrolledWindow, SignalListItemFactory, Widget,
};

use crate::app_meta::GENERAL_TAB_ID;
use crate::app_state::AppState;
use crate::commands;
use crate::config::{ListStyle, Sound};

use super::menu;

const SOUND_CONTEXT_NAMESPACE: &str = "sound-ctx";

/// Format milliseconds as M:SS string.
fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// The sound list widget bundle.
#[derive(Clone)]
pub struct SoundList {
    inner: Arc<SoundListInner>,
}

struct SoundListInner {
    scroll: ScrolledWindow,
    col_view: ColumnView,
    selection: MultiSelection,
    store: gio::ListStore,
    active_tab_id: Mutex<String>,
    search_query: Mutex<String>,
    all_sounds: Mutex<Vec<Sound>>,
    playing_ids: Arc<Mutex<HashSet<String>>>,
    invalid_ids: Arc<Mutex<HashSet<String>>>,
    active_sound_id: Arc<Mutex<Option<String>>>,
    state: Arc<AppState>,
    on_library_changed: RefCell<Option<Box<dyn Fn() + 'static>>>,
}

// Safety: SoundListInner is only used on the GTK main thread.
unsafe impl Send for SoundListInner {}
unsafe impl Sync for SoundListInner {}

impl SoundList {
    pub fn new(state: Arc<AppState>) -> Self {
        let store = gio::ListStore::new::<BoxedAnyObject>();
        let selection = MultiSelection::new(Some(store.clone()));
        let col_view = ColumnView::new(Some(selection.clone()));
        col_view.set_vexpand(true);
        col_view.set_hexpand(true);
        col_view.set_reorderable(false);
        col_view.add_css_class("data-table");

        // Apply list style class based on config
        {
            let cfg = state.config.lock().unwrap();
            if cfg.settings.list_style == ListStyle::Card {
                col_view.add_css_class("list-style-card");
            }
        }

        let scroll = ScrolledWindow::builder()
            .child(&col_view)
            .vexpand(true)
            .hexpand(true)
            .build();

        let all_sounds = {
            let cfg = state.config.lock().unwrap();
            cfg.sounds.clone()
        };

        let inner = Arc::new(SoundListInner {
            scroll,
            col_view: col_view.clone(),
            selection,
            store: store.clone(),
            active_tab_id: Mutex::new(GENERAL_TAB_ID.to_string()),
            search_query: Mutex::new(String::new()),
            all_sounds: Mutex::new(all_sounds.clone()),
            playing_ids: Arc::new(Mutex::new(HashSet::new())),
            invalid_ids: Arc::new(Mutex::new(HashSet::new())),
            active_sound_id: Arc::new(Mutex::new(None)),
            state,
            on_library_changed: RefCell::new(None),
        });

        inner.configure_columns();
        inner.connect_activate();

        let sl = Self { inner };
        sl.reload_store(&all_sounds);
        sl
    }

    /// Return the GTK widget to embed in the window layout.
    pub fn widget(&self) -> &Widget {
        self.inner.scroll.upcast_ref()
    }

    /// Set the active tab — filters the visible sounds.
    pub fn set_active_tab(&self, tab_id: String) {
        *self.inner.active_tab_id.lock().unwrap() = tab_id;
        self.refresh_from_state();
    }

    /// Update which sound IDs are currently playing and refresh the list.
    pub fn set_playing_ids(&self, ids: HashSet<String>) {
        let changed = {
            let mut current = self.inner.playing_ids.lock().unwrap();
            if *current != ids {
                *current = ids;
                true
            } else {
                false
            }
        };
        if changed {
            self.refresh_visible_rows();
        }
    }

    /// Store which sound IDs have missing source files and refresh the list.
    pub fn set_invalid_ids(&self, ids: HashSet<String>) {
        *self.inner.invalid_ids.lock().unwrap() = ids;
        self.refresh_visible_rows();
    }

    /// Set the active sound ID (current transport track) and refresh if changed.
    pub fn set_active_sound_id(&self, id: Option<String>) {
        let changed = {
            let mut current = self.inner.active_sound_id.lock().unwrap();
            if *current != id {
                *current = id;
                true
            } else {
                false
            }
        };
        if changed {
            self.refresh_visible_rows();
        }
    }

    /// Set the search filter query and refresh the displayed list.
    pub fn set_search_filter(&self, query: String) {
        *self.inner.search_query.lock().unwrap() = query;
        self.refresh_visible_rows();
    }

    /// Reload sounds from AppState config.
    pub fn refresh_from_state(&self) {
        self.inner.refresh_from_state_inner();
    }

    /// Append newly imported sounds to the list.
    pub fn append_sounds(&self, new_sounds: Vec<Sound>) {
        let sounds = {
            let mut all = self.inner.all_sounds.lock().unwrap();
            all.extend(new_sounds);
            all.clone()
        };
        self.reload_store(&sounds);
        self.inner.emit_library_changed();
    }

    /// Return the currently visible (filtered) sounds.
    pub fn get_filtered_sounds(&self) -> Vec<Sound> {
        self.inner.filtered_sounds()
    }

    /// Replace store contents with the filtered sound list.
    fn reload_store(&self, sounds: &[Sound]) {
        self.inner.reload_store(sounds);
    }

    fn refresh_visible_rows(&self) {
        let sounds = self.inner.all_sounds.lock().unwrap().clone();
        self.reload_store(&sounds);
    }

    /// Register callback fired when sound membership or tab membership changes.
    pub fn connect_library_changed<F: Fn() + 'static>(&self, f: F) {
        *self.inner.on_library_changed.borrow_mut() = Some(Box::new(f));
    }

    /// Set the list style mode ("compact" or "card")
    pub fn set_list_style(&self, style: &str) {
        let cv = &self.inner.col_view;
        if ListStyle::from_str(style).unwrap_or_default() == ListStyle::Card {
            cv.remove_css_class("list-style-compact");
            cv.add_css_class("list-style-card");
        } else {
            cv.remove_css_class("list-style-card");
            cv.add_css_class("list-style-compact");
        }
    }
}

impl SoundListInner {
    fn configure_columns(self: &Arc<Self>) {
        self.col_view.append_column(&self.build_index_column());
        self.col_view.append_column(&self.build_name_column());
        self.col_view.append_column(&self.build_duration_column());
        self.col_view.append_column(&self.build_hotkey_column());
    }

    fn connect_activate(self: &Arc<Self>) {
        let inner = Arc::clone(self);
        let store = self.store.clone();
        let invalid_ids = Arc::clone(&self.invalid_ids);

        self.col_view.connect_activate(move |cv, pos| {
            let Some(obj) = store
                .item(pos)
                .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
            else {
                return;
            };
            let sound = obj.borrow::<Sound>().clone();
            let is_invalid = invalid_ids.lock().unwrap().contains(&sound.id);

            if !is_invalid {
                if let Err(e) = commands::play_sound(
                    sound.id.clone(),
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                ) {
                    log::warn!("Play failed for '{}': {}", sound.name, e);
                }
                return;
            }

            let Some(win) = cv
                .root()
                .and_then(|root| root.downcast::<gtk4::Window>().ok())
            else {
                return;
            };

            let sound_name = sound.name.clone();
            let sound_path = sound
                .source_path
                .clone()
                .unwrap_or_else(|| sound.path.clone());
            let state_locate = Arc::clone(&inner.state);
            let state_remove = Arc::clone(&inner.state);
            let invalid_locate = Arc::clone(&inner.invalid_ids);
            let invalid_remove = Arc::clone(&inner.invalid_ids);
            let inner_locate = Arc::clone(&inner);
            let inner_remove = Arc::clone(&inner);
            let sound_id_locate = sound.id.clone();
            let sound_id_remove = sound.id.clone();
            let win_locate = win.clone();

            crate::ui::dialogs::show_missing_file(
                &win,
                &sound_name,
                &sound_path,
                move || {
                    let file_dialog = gtk4::FileDialog::builder()
                        .title("Locate Audio File")
                        .build();
                    let state = Arc::clone(&state_locate);
                    let invalid_ids = Arc::clone(&invalid_locate);
                    let inner_refresh = Arc::clone(&inner_locate);
                    let sound_id = sound_id_locate.clone();
                    file_dialog.open(Some(&win_locate), gio::Cancellable::NONE, move |result| {
                        if let Ok(file) = result {
                            if let Some(path) = file.path() {
                                let new_path = path.to_string_lossy().to_string();
                                match commands::update_sound_source(
                                    sound_id.clone(),
                                    new_path,
                                    Arc::clone(&state.config),
                                ) {
                                    Ok(_) => {
                                        invalid_ids.lock().unwrap().remove(&sound_id);
                                        inner_refresh.refresh_from_state_inner();
                                    }
                                    Err(e) => log::warn!("Update source failed: {e}"),
                                }
                            }
                        }
                    });
                },
                move || match commands::remove_sound(
                    sound_id_remove.clone(),
                    Arc::clone(&state_remove.config),
                    Arc::clone(&state_remove.hotkeys),
                ) {
                    Ok(_) => {
                        invalid_remove.lock().unwrap().remove(&sound_id_remove);
                        inner_remove.refresh_from_state_inner();
                        inner_remove.emit_library_changed();
                    }
                    Err(e) => log::warn!("Remove sound failed: {e}"),
                },
            );
        });
    }

    fn build_index_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner = Arc::clone(self);
            factory.connect_setup(move |_, item| {
                let cell = GtkBox::new(Orientation::Horizontal, 0);
                cell.set_hexpand(true);
                cell.set_halign(gtk4::Align::Fill);
                cell.add_css_class("sound-cell");
                cell.add_css_class("sound-cell-first");
                let label = Label::builder()
                    .xalign(1.0)
                    .width_chars(3)
                    .css_classes(vec!["sound-index"])
                    .build();
                cell.append(&label);
                inner.install_context_menu(&cell);
                item.downcast_ref::<gtk4::ListItem>()
                    .unwrap()
                    .set_child(Some(&cell));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);
            factory.connect_bind(move |_, item| {
                let list_item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };
                let sound = obj.borrow::<Sound>();
                let cell = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let label = cell.first_child().unwrap().downcast::<Label>().unwrap();
                label.set_text(&(list_item.position() + 1).to_string());
                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids.lock().unwrap().contains(&sound.id);
                let is_active = active_sound_id.lock().unwrap().as_deref() == Some(&sound.id);
                if is_playing {
                    cell.add_css_class("sound-cell-playing");
                } else {
                    cell.remove_css_class("sound-cell-playing");
                }
                if is_active {
                    cell.add_css_class("sound-cell-active");
                } else {
                    cell.remove_css_class("sound-cell-active");
                }
            });
        }

        let column = ColumnViewColumn::new(Some("#"), Some(factory));
        column.set_fixed_width(56);
        column
    }

    fn build_name_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner = Arc::clone(self);
            factory.connect_setup(move |_, item| {
                let hbox = GtkBox::new(Orientation::Horizontal, 6);
                hbox.set_hexpand(true);
                hbox.add_css_class("sound-cell");
                let dot = Label::builder()
                    .label("●")
                    .css_classes(vec!["playing-dot"])
                    .visible(false)
                    .build();
                let label = Label::builder()
                    .xalign(0.0)
                    .css_classes(vec!["sound-name"])
                    .ellipsize(gtk4::pango::EllipsizeMode::End)
                    .hexpand(true)
                    .build();
                let warn = Label::builder()
                    .label("⚠")
                    .css_classes(vec!["warning-label"])
                    .visible(false)
                    .build();

                hbox.append(&dot);
                hbox.append(&label);
                hbox.append(&warn);
                inner.install_context_menu(&hbox);

                item.downcast_ref::<gtk4::ListItem>()
                    .unwrap()
                    .set_child(Some(&hbox));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let invalid_ids = Arc::clone(&self.invalid_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);

            factory.connect_bind(move |_, item| {
                let list_item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };

                let sound = obj.borrow::<Sound>();
                let hbox = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let dot = hbox.first_child().unwrap().downcast::<Label>().unwrap();
                let label = dot.next_sibling().unwrap().downcast::<Label>().unwrap();
                let warn = label.next_sibling().unwrap().downcast::<Label>().unwrap();
                let is_playing = playing_ids.lock().unwrap().contains(&sound.id);
                let is_invalid = invalid_ids.lock().unwrap().contains(&sound.id);
                let is_active = active_sound_id.lock().unwrap().as_deref() == Some(&sound.id);

                label.set_text(&sound.name);
                dot.set_visible(is_playing);
                warn.set_visible(is_invalid);
                hbox.set_widget_name(&sound.id);

                if is_playing {
                    hbox.add_css_class("sound-cell-playing");
                } else {
                    hbox.remove_css_class("sound-cell-playing");
                }

                if is_active {
                    hbox.add_css_class("sound-cell-active");
                } else {
                    hbox.remove_css_class("sound-cell-active");
                }
            });
        }

        let column = ColumnViewColumn::new(Some("NAME"), Some(factory));
        column.set_expand(true);
        column
    }

    fn build_duration_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner = Arc::clone(self);
            factory.connect_setup(move |_, item| {
                let cell = GtkBox::new(Orientation::Horizontal, 0);
                cell.set_hexpand(true);
                cell.set_halign(gtk4::Align::Fill);
                cell.add_css_class("sound-cell");
                let label = Label::builder()
                    .xalign(0.0)
                    .hexpand(true)
                    .css_classes(vec!["sound-duration"])
                    .build();
                cell.append(&label);
                inner.install_context_menu(&cell);
                item.downcast_ref::<gtk4::ListItem>()
                    .unwrap()
                    .set_child(Some(&cell));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);
            factory.connect_bind(move |_, item| {
                let list_item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };
                let sound = obj.borrow::<Sound>();
                let cell = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let label = cell.first_child().unwrap().downcast::<Label>().unwrap();
                label.set_text(
                    &sound
                        .duration_ms
                        .map(format_duration)
                        .unwrap_or_else(|| "\u{2014}".to_string()),
                );
                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids.lock().unwrap().contains(&sound.id);
                let is_active = active_sound_id.lock().unwrap().as_deref() == Some(&sound.id);
                if is_playing {
                    cell.add_css_class("sound-cell-playing");
                } else {
                    cell.remove_css_class("sound-cell-playing");
                }
                if is_active {
                    cell.add_css_class("sound-cell-active");
                } else {
                    cell.remove_css_class("sound-cell-active");
                }
            });
        }

        let column = ColumnViewColumn::new(Some("DURATION"), Some(factory));
        column.set_fixed_width(96);
        column
    }

    fn build_hotkey_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner = Arc::clone(self);
            factory.connect_setup(move |_, item| {
                let cell = GtkBox::new(Orientation::Horizontal, 0);
                cell.set_hexpand(true);
                cell.set_halign(gtk4::Align::Fill);
                cell.add_css_class("sound-cell");
                let label = Label::builder().xalign(0.0).hexpand(true).build();
                cell.append(&label);
                inner.install_context_menu(&cell);
                item.downcast_ref::<gtk4::ListItem>()
                    .unwrap()
                    .set_child(Some(&cell));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);
            factory.connect_bind(move |_, item| {
                let list_item = item.downcast_ref::<gtk4::ListItem>().unwrap();
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };
                let sound = obj.borrow::<Sound>();
                let cell = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let label = cell.first_child().unwrap().downcast::<Label>().unwrap();

                if let Some(hotkey) = &sound.hotkey {
                    label.set_text(hotkey);
                    label.add_css_class("hotkey-badge");
                    label.remove_css_class("dim-label");
                } else {
                    label.set_text("\u{2014}");
                    label.remove_css_class("hotkey-badge");
                    label.add_css_class("dim-label");
                }

                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids.lock().unwrap().contains(&sound.id);
                let is_active = active_sound_id.lock().unwrap().as_deref() == Some(&sound.id);
                if is_playing {
                    cell.add_css_class("sound-cell-playing");
                } else {
                    cell.remove_css_class("sound-cell-playing");
                }
                if is_active {
                    cell.add_css_class("sound-cell-active");
                } else {
                    cell.remove_css_class("sound-cell-active");
                }
            });
        }

        let column = ColumnViewColumn::new(Some("HOTKEY"), Some(factory));
        column.set_fixed_width(160);
        column
    }

    fn install_context_menu(self: &Arc<Self>, widget: &impl IsA<gtk4::Widget>) {
        let gesture = GestureClick::new();
        gesture.set_button(3);
        gesture.set_propagation_phase(gtk4::PropagationPhase::Capture);
        gesture.connect_pressed(|gesture, _, _, _| {
            // Keep existing multi-selection when opening context menu via right click.
            gesture.set_state(gtk4::EventSequenceState::Claimed);
        });

        let inner = Arc::clone(self);
        gesture.connect_released(move |gesture, _, x, y| {
            let Some(widget) = gesture.widget() else {
                return;
            };
            let sound_id = widget.widget_name().to_string();
            if sound_id.is_empty() {
                return;
            }
            inner.show_context_menu_for_sound_id(&widget, x, y, &sound_id);
        });

        widget.as_ref().add_controller(gesture);
    }

    fn show_context_menu_for_sound_id(
        self: &Arc<Self>,
        widget: &Widget,
        x: f64,
        y: f64,
        sound_id: &str,
    ) {
        let sound = {
            let cfg = self.state.config.lock().unwrap();
            cfg.get_sound(sound_id).cloned()
        };

        if let Some(sound) = sound {
            self.show_context_menu(widget, x, y, sound);
        }
    }

    fn show_context_menu(self: &Arc<Self>, widget: &Widget, x: f64, y: f64, sound: Sound) {
        let Some(win) = widget
            .root()
            .and_then(|root| root.downcast::<gtk4::Window>().ok())
        else {
            return;
        };

        let active_tab = self.active_tab_id.lock().unwrap().clone();
        let tabs = {
            let cfg = self.state.config.lock().unwrap();
            cfg.tabs.clone()
        };

        let menu_model = gio::Menu::new();
        let selected_ids = self.selected_sound_ids();
        let target_ids = if selected_ids.len() > 1 && selected_ids.iter().any(|id| id == &sound.id)
        {
            selected_ids
        } else {
            vec![sound.id.clone()]
        };
        let target_count = target_ids.len();
        let display_name = if sound.name.chars().count() > 30 {
            format!("{}…", sound.name.chars().take(29).collect::<String>())
        } else {
            sound.name.clone()
        };

        let section1 = gio::Menu::new();
        section1.append(Some("Rename"), Some("sound-ctx.rename"));
        section1.append(
            Some(if sound.hotkey.is_some() {
                "Update Hotkey"
            } else {
                "Set Hotkey"
            }),
            Some("sound-ctx.set-hotkey"),
        );
        section1.append(Some("Check file path"), Some("sound-ctx.check-path"));
        menu_model.append_section(Some(&display_name), &section1);

        let section2 = gio::Menu::new();
        if !tabs.is_empty() {
            let add_to_tab = gio::Menu::new();
            for tab in &tabs {
                add_to_tab.append(
                    Some(&tab.name),
                    Some(&format!("{SOUND_CONTEXT_NAMESPACE}.add-to-tab-{}", tab.id)),
                );
            }

            section2.append_submenu(Some("Add to Tab"), &add_to_tab);
        }
        if active_tab != GENERAL_TAB_ID {
            section2.append(Some("Remove from Tab"), Some("sound-ctx.remove-from-tab"));
        }
        if section2.n_items() > 0 {
            menu_model.append_section(None, &section2);
        }

        let destructive = gio::Menu::new();
        destructive.append(
            Some(if target_count > 1 {
                "Delete Selected"
            } else {
                "Delete"
            }),
            Some("sound-ctx.delete"),
        );
        menu_model.append_section(None, &destructive);

        let action_group = gio::SimpleActionGroup::new();

        {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let sound = sound.clone();
            let win = win.clone();
            let action = gio::SimpleAction::new("rename", None);
            action.connect_activate(move |_, _| {
                let inner_confirm = Arc::clone(&inner);
                let sound = sound.clone();
                let state_confirm = Arc::clone(&state);
                crate::ui::dialogs::show_input(
                    &win,
                    "Rename Sound",
                    "Enter a new name:",
                    &sound.name,
                    "Rename",
                    move |new_name| match commands::rename_sound(
                        sound.id.clone(),
                        new_name,
                        Arc::clone(&state_confirm.config),
                    ) {
                        Ok(_) => inner_confirm.refresh_from_state_inner(),
                        Err(e) => log::warn!("Rename failed: {e}"),
                    },
                );
            });
            action_group.add_action(&action);
        }

        {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let sound = sound.clone();
            let win = win.clone();
            let action = gio::SimpleAction::new("set-hotkey", None);
            action.connect_activate(move |_, _| {
                let inner_confirm = Arc::clone(&inner);
                let sound = sound.clone();
                let state_confirm = Arc::clone(&state);
                crate::ui::dialogs::show_hotkey_capture(
                    &win,
                    sound.hotkey.as_deref(),
                    move |hotkey| match commands::set_hotkey(
                        sound.id.clone(),
                        hotkey,
                        Arc::clone(&state_confirm.config),
                        Arc::clone(&state_confirm.hotkeys),
                    ) {
                        Ok(_) => inner_confirm.refresh_from_state_inner(),
                        Err(e) => log::warn!("Set hotkey failed: {e}"),
                    },
                );
            });
            action_group.add_action(&action);
        }

        {
            let sound = sound.clone();
            let win = win.clone();
            let action = gio::SimpleAction::new("check-path", None);
            action.connect_activate(move |_, _| {
                let path = sound.source_path.as_deref().unwrap_or(&sound.path);
                crate::ui::dialogs::show_path_info(&win, &sound.name, path);
            });
            action_group.add_action(&action);
        }

        for tab in &tabs {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let sound_ids = target_ids.clone();
            let tab_id = tab.id.clone();
            let action_name = format!("add-to-tab-{}", tab.id);
            let action = gio::SimpleAction::new(&action_name, None);
            action.connect_activate(move |_, _| {
                match commands::add_sounds_to_tab(
                    tab_id.clone(),
                    sound_ids.clone(),
                    Arc::clone(&state.config),
                ) {
                    Ok(_) => {
                        inner.refresh_from_state_inner();
                        inner.emit_library_changed();
                    }
                    Err(e) => log::warn!("Add to tab failed: {e}"),
                }
            });
            action_group.add_action(&action);
        }

        if active_tab != GENERAL_TAB_ID {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let tab_id = active_tab.clone();
            let sound_ids = target_ids.clone();
            let action = gio::SimpleAction::new("remove-from-tab", None);
            action.connect_activate(move |_, _| {
                let mut any_success = false;
                for sound_id in &sound_ids {
                    match commands::remove_sound_from_tab(
                        tab_id.clone(),
                        sound_id.clone(),
                        Arc::clone(&state.config),
                    ) {
                        Ok(_) => any_success = true,
                        Err(e) => log::warn!("Remove from tab failed for {}: {e}", sound_id),
                    }
                }
                if any_success {
                    inner.refresh_from_state_inner();
                    inner.emit_library_changed();
                }
            });
            action_group.add_action(&action);
        }

        {
            let inner = Arc::clone(self);
            let state = Arc::clone(&self.state);
            let sound = sound.clone();
            let target_ids = target_ids.clone();
            let win = win.clone();
            let action = gio::SimpleAction::new("delete", None);
            action.connect_activate(move |_, _| {
                let skip_confirm = state.config.lock().unwrap().settings.skip_delete_confirm;
                let inner_confirm = Arc::clone(&inner);
                let state_confirm = Arc::clone(&state);
                let sound_to_delete = sound.clone();
                let target_ids = target_ids.clone();
                let target_ids_for_delete = target_ids.clone();

                let delete_sound = move || {
                    let mut had_error = false;
                    for sound_id in &target_ids_for_delete {
                        if let Err(e) = commands::remove_sound(
                            sound_id.clone(),
                            Arc::clone(&state_confirm.config),
                            Arc::clone(&state_confirm.hotkeys),
                        ) {
                            had_error = true;
                            log::warn!("Delete failed for {}: {e}", sound_id);
                        }
                    }
                    if !had_error {
                        inner_confirm.refresh_from_state_inner();
                        inner_confirm.emit_library_changed();
                    }
                };

                if skip_confirm {
                    delete_sound();
                } else {
                    let message = if target_ids.len() > 1 {
                        format!(
                            "Delete {} selected sounds? This cannot be undone.",
                            target_ids.len()
                        )
                    } else {
                        format!("Delete '{}'? This cannot be undone.", sound_to_delete.name)
                    };
                    crate::ui::dialogs::show_confirm(
                        &win,
                        "Delete Sound",
                        &message,
                        "Delete",
                        delete_sound,
                    );
                }
            });
            action_group.add_action(&action);
        }

        menu::show_popover_menu(
            widget,
            SOUND_CONTEXT_NAMESPACE,
            &menu_model,
            &action_group,
            x,
            y,
        );
    }

    fn selected_sound_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        let count = self.selection.n_items();
        for idx in 0..count {
            if !self.selection.is_selected(idx) {
                continue;
            }
            let Some(obj) = self
                .selection
                .item(idx)
                .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
            else {
                continue;
            };
            let sound = obj.borrow::<Sound>();
            ids.push(sound.id.clone());
        }
        ids
    }

    fn filtered_sounds(&self) -> Vec<Sound> {
        let sounds = self.all_sounds.lock().unwrap().clone();
        let tab_id = self.active_tab_id.lock().unwrap().clone();
        let search_query = self.search_query.lock().unwrap().to_lowercase();
        let cfg = self.state.config.lock().unwrap();

        let sounds = if tab_id == GENERAL_TAB_ID {
            sounds
        } else if let Some(tab) = cfg.tabs.iter().find(|tab| tab.id == tab_id) {
            sounds
                .into_iter()
                .filter(|sound| tab.sound_ids.contains(&sound.id))
                .collect()
        } else {
            sounds
        };

        if search_query.is_empty() {
            sounds
        } else {
            sounds
                .into_iter()
                .filter(|sound| sound.name.to_lowercase().contains(&search_query))
                .collect()
        }
    }

    fn reload_store(&self, sounds: &[Sound]) {
        let filtered_ids = {
            let visible = self.filtered_sounds_from(sounds);
            visible
                .into_iter()
                .map(BoxedAnyObject::new)
                .collect::<Vec<_>>()
        };

        self.store.remove_all();
        for sound in filtered_ids {
            self.store.append(&sound);
        }
    }

    fn filtered_sounds_from(&self, sounds: &[Sound]) -> Vec<Sound> {
        let tab_id = self.active_tab_id.lock().unwrap().clone();
        let search_query = self.search_query.lock().unwrap().to_lowercase();
        let cfg = self.state.config.lock().unwrap();

        let sounds = if tab_id == GENERAL_TAB_ID {
            sounds.to_vec()
        } else if let Some(tab) = cfg.tabs.iter().find(|tab| tab.id == tab_id) {
            sounds
                .iter()
                .filter(|sound| tab.sound_ids.contains(&sound.id))
                .cloned()
                .collect()
        } else {
            sounds.to_vec()
        };

        if search_query.is_empty() {
            sounds
        } else {
            sounds
                .into_iter()
                .filter(|sound| sound.name.to_lowercase().contains(&search_query))
                .collect()
        }
    }

    /// Refresh the store from the current AppState config (called from action callbacks).
    fn refresh_from_state_inner(&self) {
        let sounds = {
            let cfg = self.state.config.lock().unwrap();
            cfg.sounds.clone()
        };
        *self.all_sounds.lock().unwrap() = sounds.clone();
        self.reload_store(&sounds);
    }

    fn emit_library_changed(&self) {
        if let Some(ref cb) = *self.on_library_changed.borrow() {
            cb();
        }
    }
}
