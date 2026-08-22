use std::rc::Rc;
use std::sync::Arc;

use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{Box as GtkBox, ColumnViewColumn, Label, Orientation, SignalListItemFactory};

use super::paged_model::PagedSoundModel;
use super::{SoundList, SoundListInner, SoundRowData};

fn format_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

impl SoundListInner {
    pub(super) fn configure_columns(self: &Rc<Self>) {
        self.col_view.append_column(&self.build_index_column());
        self.col_view.append_column(&self.build_name_column());
        self.col_view.append_column(&self.build_duration_column());
        self.col_view.append_column(&self.build_hotkey_column());
    }

    fn build_label_column(
        self: &Rc<Self>,
        title: Option<&str>,
        fixed_width: i32,
        extra_cell_class: Option<&'static str>,
        pager: Option<PagedSoundModel>,
        new_label: fn() -> Label,
        bind_label: impl Fn(&Label, &SoundRowData, u32) + 'static,
    ) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Rc::downgrade(self);
            factory.connect_setup(move |_, item| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let cell = GtkBox::new(Orientation::Horizontal, 0);
                cell.set_hexpand(true);
                cell.set_halign(gtk4::Align::Fill);
                cell.add_css_class("sound-cell");
                if let Some(class) = extra_cell_class {
                    cell.add_css_class(class);
                }
                cell.append(&new_label());
                inner.install_context_menu(&cell);
                inner.install_drag_source(&cell);
                let Some(list_item) = item.downcast_ref::<gtk4::ListItem>() else {
                    return;
                };
                list_item.set_child(Some(&cell));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);
            factory.connect_bind(move |_, item| {
                let Some(list_item) = item.downcast_ref::<gtk4::ListItem>() else {
                    return;
                };
                if let Some(pager) = pager.as_ref() {
                    pager.load_position(list_item.position());
                }
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };
                let sound = obj.borrow::<SoundRowData>();
                let Some(cell) = list_item.child().and_then(|c| c.downcast::<GtkBox>().ok()) else {
                    return;
                };
                let Some(label) = cell.first_child().and_then(|w| w.downcast::<Label>().ok())
                else {
                    return;
                };
                bind_label(&label, &sound, list_item.position());
                cell.set_widget_name(&sound.id);
                let is_playing = playing_ids.lock().contains(&sound.id);
                let is_active = active_sound_id.lock().as_deref() == Some(&sound.id);
                SoundList::sync_sound_state_classes(&cell, is_playing, is_active);
            });
        }

        let column = ColumnViewColumn::new(title, Some(factory));
        column.set_fixed_width(fixed_width);
        column
    }

    fn build_index_column(self: &Rc<Self>) -> ColumnViewColumn {
        self.build_label_column(
            None,
            56,
            Some("sound-cell-first"),
            Some(self.store.clone()),
            || {
                Label::builder()
                    .xalign(1.0)
                    .width_chars(3)
                    .css_classes(vec!["sound-index"])
                    .build()
            },
            |label, _sound, position| label.set_text(&(position + 1).to_string()),
        )
    }

    fn build_name_column(self: &Rc<Self>) -> ColumnViewColumn {
        let factory = SignalListItemFactory::new();

        {
            let inner_weak = Rc::downgrade(self);
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

                let Some(list_item) = item.downcast_ref::<gtk4::ListItem>() else {
                    return;
                };
                list_item.set_child(Some(&hbox));
            });
        }

        {
            let playing_ids = Arc::clone(&self.playing_ids);
            let invalid_ids = Arc::clone(&self.invalid_ids);
            let active_sound_id = Arc::clone(&self.active_sound_id);

            factory.connect_bind(move |_, item| {
                let Some(list_item) = item.downcast_ref::<gtk4::ListItem>() else {
                    return;
                };
                let Some(obj) = list_item
                    .item()
                    .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                else {
                    return;
                };

                let sound = obj.borrow::<SoundRowData>();
                let Some(hbox) = list_item.child().and_then(|c| c.downcast::<GtkBox>().ok()) else {
                    return;
                };
                let Some(dot) = hbox.first_child().and_then(|w| w.downcast::<Label>().ok()) else {
                    return;
                };
                let Some(label) = dot.next_sibling().and_then(|w| w.downcast::<Label>().ok())
                else {
                    return;
                };
                let Some(warn) = label
                    .next_sibling()
                    .and_then(|w| w.downcast::<Label>().ok())
                else {
                    return;
                };
                let is_playing = playing_ids.lock().contains(&sound.id);
                let is_invalid = invalid_ids.lock().contains(&sound.id);
                let is_active = active_sound_id.lock().as_deref() == Some(&sound.id);

                label.set_text(&sound.name);
                dot.set_visible(is_playing);
                warn.set_visible(is_invalid);
                hbox.set_widget_name(&sound.id);

                SoundList::sync_sound_state_classes(&hbox, is_playing, is_active);
            });
        }

        let column = ColumnViewColumn::new(Some("NAME"), Some(factory));
        // Avoid forcing GTK to scan every lazy row.
        column.set_fixed_width(240);
        column.set_expand(true);
        column
    }

    fn build_duration_column(self: &Rc<Self>) -> ColumnViewColumn {
        self.build_label_column(
            Some("DURATION"),
            96,
            None,
            None,
            || {
                Label::builder()
                    .xalign(0.0)
                    .hexpand(true)
                    .css_classes(vec!["sound-duration"])
                    .build()
            },
            |label, sound, _position| {
                label.set_text(
                    &sound
                        .duration_ms
                        .map(format_duration)
                        .unwrap_or_else(|| "\u{2014}".to_string()),
                );
            },
        )
    }

    fn build_hotkey_column(self: &Rc<Self>) -> ColumnViewColumn {
        self.build_label_column(
            Some("HOTKEY"),
            160,
            None,
            None,
            || Label::builder().xalign(0.0).hexpand(true).build(),
            |label, sound, _position| {
                if let Some(hotkey) = &sound.hotkey {
                    label.set_text(hotkey);
                    label.add_css_class("hotkey-badge");
                    label.remove_css_class("dim-label");
                } else {
                    label.set_text("\u{2014}");
                    label.remove_css_class("hotkey-badge");
                    label.add_css_class("dim-label");
                }
            },
        )
    }
}
