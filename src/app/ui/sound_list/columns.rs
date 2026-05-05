use std::sync::Arc;

use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{Box as GtkBox, ColumnViewColumn, Label, Orientation, SignalListItemFactory};

use super::{SoundList, SoundListInner, SoundRowData};

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl SoundListInner {
    pub(super) fn configure_columns(self: &Arc<Self>) {
        self.col_view.append_column(&self.build_index_column());
        self.col_view.append_column(&self.build_name_column());
        self.col_view.append_column(&self.build_duration_column());
        self.col_view.append_column(&self.build_hotkey_column());
    }

    fn build_index_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Arc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
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
                inner.install_drag_source(&cell);
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
                let sound = obj.borrow::<SoundRowData>();
                let cell = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let label = cell.first_child().unwrap().downcast::<Label>().unwrap();
                label.set_text(&(list_item.position() + 1).to_string());
                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("playing_ids lock poisoned: {}", e);
                        false
                    });
                let is_active = active_sound_id
                    .lock()
                    .map(|id| id.as_deref() == Some(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("active_sound_id lock poisoned: {}", e);
                        false
                    });
                SoundList::sync_sound_state_classes(&cell, is_playing, is_active);
            });
        }

        let column = ColumnViewColumn::new(Some("#"), Some(factory));
        column.set_fixed_width(56);
        column
    }

    fn build_name_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Arc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
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
                inner.install_drag_source(&hbox);

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

                let sound = obj.borrow::<SoundRowData>();
                let hbox = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let dot = hbox.first_child().unwrap().downcast::<Label>().unwrap();
                let label = dot.next_sibling().unwrap().downcast::<Label>().unwrap();
                let warn = label.next_sibling().unwrap().downcast::<Label>().unwrap();
                let is_playing = playing_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("playing_ids lock poisoned: {}", e);
                        false
                    });
                let is_invalid = invalid_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("invalid_ids lock poisoned: {}", e);
                        false
                    });
                let is_active = active_sound_id
                    .lock()
                    .map(|id| id.as_deref() == Some(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("active_sound_id lock poisoned: {}", e);
                        false
                    });

                label.set_text(&sound.name);
                dot.set_visible(is_playing);
                warn.set_visible(is_invalid);
                hbox.set_widget_name(&sound.id);

                SoundList::sync_sound_state_classes(&hbox, is_playing, is_active);
            });
        }

        let column = ColumnViewColumn::new(Some("NAME"), Some(factory));
        column.set_expand(true);
        column
    }

    fn build_duration_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Arc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
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
                inner.install_drag_source(&cell);
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
                let sound = obj.borrow::<SoundRowData>();
                let cell = list_item.child().unwrap().downcast::<GtkBox>().unwrap();
                let label = cell.first_child().unwrap().downcast::<Label>().unwrap();
                label.set_text(
                    &sound
                        .duration_ms
                        .map(format_duration)
                        .unwrap_or_else(|| "\u{2014}".to_string()),
                );
                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("playing_ids lock poisoned: {}", e);
                        false
                    });
                let is_active = active_sound_id
                    .lock()
                    .map(|id| id.as_deref() == Some(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("active_sound_id lock poisoned: {}", e);
                        false
                    });
                SoundList::sync_sound_state_classes(&cell, is_playing, is_active);
            });
        }

        let column = ColumnViewColumn::new(Some("DURATION"), Some(factory));
        column.set_fixed_width(96);
        column
    }

    fn build_hotkey_column(self: &Arc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Arc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let cell = GtkBox::new(Orientation::Horizontal, 0);
                cell.set_hexpand(true);
                cell.set_halign(gtk4::Align::Fill);
                cell.add_css_class("sound-cell");
                let label = Label::builder().xalign(0.0).hexpand(true).build();
                cell.append(&label);
                inner.install_context_menu(&cell);
                inner.install_drag_source(&cell);
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
                let sound = obj.borrow::<SoundRowData>();
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
                let is_playing = playing_ids
                    .lock()
                    .map(|ids| ids.contains(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("playing_ids lock poisoned: {}", e);
                        false
                    });
                let is_active = active_sound_id
                    .lock()
                    .map(|id| id.as_deref() == Some(&sound.id))
                    .unwrap_or_else(|e| {
                        log::warn!("active_sound_id lock poisoned: {}", e);
                        false
                    });
                SoundList::sync_sound_state_classes(&cell, is_playing, is_active);
            });
        }

        let column = ColumnViewColumn::new(Some("HOTKEY"), Some(factory));
        column.set_fixed_width(160);
        column
    }
}
