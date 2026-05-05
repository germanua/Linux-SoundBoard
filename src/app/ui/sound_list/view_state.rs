use std::collections::HashSet;

use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{Box as GtkBox, Label, Widget};

use super::{ScrollOffsets, SoundList, SoundListInner, SoundRowData};

fn clamp_adjustment_value(adjustment: &gtk4::Adjustment, value: f64) -> f64 {
    let lower = adjustment.lower();
    let upper = (adjustment.upper() - adjustment.page_size()).max(lower);
    value.clamp(lower, upper)
}

impl SoundListInner {
    pub(super) fn capture_scroll_offsets(&self) -> ScrollOffsets {
        ScrollOffsets {
            vertical: self.scroll.vadjustment().value(),
            horizontal: self.scroll.hadjustment().value(),
        }
    }

    pub(super) fn restore_scroll_offsets(&self, offsets: ScrollOffsets) {
        let vadjustment = self.scroll.vadjustment();
        let hadjustment = self.scroll.hadjustment();

        glib::idle_add_local_once(move || {
            vadjustment.set_value(clamp_adjustment_value(&vadjustment, offsets.vertical));
            hadjustment.set_value(clamp_adjustment_value(&hadjustment, offsets.horizontal));
        });
    }

    pub(super) fn refresh_visible_sound_state(&self) {
        let mut child = self.col_view.first_child();
        while let Some(current) = child {
            self.refresh_visible_sound_state_widget(&current);
            child = current.next_sibling();
        }
    }

    fn refresh_visible_sound_state_widget(&self, widget: &Widget) {
        if widget.has_css_class("sound-cell") {
            self.refresh_sound_cell_widget(widget);
        }

        let mut child = widget.first_child();
        while let Some(current) = child {
            self.refresh_visible_sound_state_widget(&current);
            child = current.next_sibling();
        }
    }

    fn refresh_sound_cell_widget(&self, widget: &Widget) {
        let sound_id = widget.widget_name();
        if sound_id.is_empty() {
            return;
        }

        let is_playing = self
            .playing_ids
            .lock()
            .map(|ids| ids.contains(sound_id.as_str()))
            .unwrap_or_else(|e| {
                log::warn!("playing_ids lock poisoned: {}", e);
                false
            });
        let is_active = self
            .active_sound_id
            .lock()
            .map(|id| id.as_deref() == Some(sound_id.as_str()))
            .unwrap_or_else(|e| {
                log::warn!("active_sound_id lock poisoned: {}", e);
                false
            });

        SoundList::sync_sound_state_classes(widget, is_playing, is_active);

        if let Ok(container) = widget.clone().downcast::<GtkBox>() {
            if let Some(dot) = container
                .first_child()
                .and_then(|child| child.downcast::<Label>().ok())
            {
                if dot.has_css_class("playing-dot") {
                    dot.set_visible(is_playing);
                }
            }
        }
    }

    pub(super) fn rebind_rows_for_ids(&self, sound_ids: &HashSet<String>) {
        if sound_ids.is_empty() {
            return;
        }

        let scroll_offsets = self.capture_scroll_offsets();
        let mut replacements = Vec::new();
        {
            let indices = self.visible_row_indices.borrow();
            for sound_id in sound_ids {
                if let Some(position) = indices.get(sound_id) {
                    let Some(obj) = self
                        .store
                        .item(*position)
                        .and_then(|obj| obj.downcast::<BoxedAnyObject>().ok())
                    else {
                        continue;
                    };
                    replacements.push((*position, obj.borrow::<SoundRowData>().clone()));
                }
            }
        }

        replacements.sort_unstable_by_key(|(position, _)| *position);
        replacements.dedup_by_key(|(position, _)| *position);

        // Replace the row so GtkColumnView rebinds transient playback state.
        for (position, row) in replacements {
            self.replace_row_at(position, row);
        }
        self.restore_scroll_offsets(scroll_offsets);
    }
}
