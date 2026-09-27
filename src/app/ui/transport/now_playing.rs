use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::{
    Align, Box as GtkBox, Button, FlowBox, Label, Orientation, Revealer, Scale, ScrolledWindow,
    SelectionMode,
};

use crate::app_state::AppState;
use crate::audio::{PlaybackPosition, PlayerSnapshot};
use crate::commands;
use crate::timer_registry::remove_source_id_safe;
use crate::ui::icons;

use super::helpers::format_duration;

#[derive(Clone)]
struct PlayingRow {
    root: GtkBox,
    sound_id: String,
    title: Label,
    progress: Scale,
    time: Label,
    pause_btn: Button,
    paused: Rc<Cell<bool>>,
    play_id: Rc<RefCell<String>>,
    duration_ms: Rc<Cell<u64>>,
    syncing_progress: Rc<Cell<bool>>,
    pending_seek: Rc<Cell<Option<(f64, i64)>>>,
}

type StopSoundHandler = Box<dyn Fn(String) + 'static>;

pub(super) struct NowPlayingPanel {
    root: GtkBox,
    title: Label,
    disclosure_btn: Button,
    revealer: Revealer,
    rows_box: FlowBox,
    rows: RefCell<HashMap<String, PlayingRow>>,
    order: RefCell<Vec<String>>,
    resolving_names: RefCell<HashSet<String>>,
    expanded: Cell<bool>,
    state: Arc<AppState>,
    on_stop_sound: RefCell<Option<StopSoundHandler>>,
}

impl NowPlayingPanel {
    pub(super) fn new(state: Arc<AppState>) -> Rc<Self> {
        let root = GtkBox::new(Orientation::Vertical, 1);
        root.add_css_class("now-playing-panel");
        root.set_halign(Align::Fill);
        root.set_hexpand(true);
        root.set_visible(false);

        let header = GtkBox::new(Orientation::Horizontal, 4);
        header.add_css_class("now-playing-header");

        let disclosure_btn = icons::button(icons::DISCLOSURE_OPEN, "Collapse Now Playing");
        disclosure_btn.add_css_class("now-playing-disclosure");
        disclosure_btn.set_size_request(20, 20);

        let title = Label::builder()
            .label("Now Playing")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(vec!["now-playing-title"])
            .build();

        header.append(&disclosure_btn);
        header.append(&title);

        let rows_box = FlowBox::new();
        rows_box.add_css_class("now-playing-list");
        rows_box.set_selection_mode(SelectionMode::None);
        rows_box.set_min_children_per_line(1);
        rows_box.set_max_children_per_line(3);
        rows_box.set_row_spacing(4);
        rows_box.set_column_spacing(8);
        rows_box.set_homogeneous(true);
        rows_box.set_halign(Align::Fill);
        rows_box.set_valign(Align::Start);
        rows_box.set_hexpand(true);
        rows_box.set_margin_end(6);

        let scroll = ScrolledWindow::builder()
            .child(&rows_box)
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .overlay_scrolling(false)
            .propagate_natural_height(true)
            .max_content_height(92)
            .build();
        scroll.add_css_class("now-playing-scroll");

        let revealer = Revealer::builder()
            .child(&scroll)
            .reveal_child(true)
            .transition_type(gtk4::RevealerTransitionType::SlideDown)
            .transition_duration(120)
            .build();

        root.append(&header);
        root.append(&revealer);

        let panel = Rc::new(Self {
            root,
            title,
            disclosure_btn,
            revealer,
            rows_box,
            rows: RefCell::new(HashMap::new()),
            order: RefCell::new(Vec::new()),
            resolving_names: RefCell::new(HashSet::new()),
            expanded: Cell::new(true),
            state,
            on_stop_sound: RefCell::new(None),
        });

        {
            let weak = Rc::downgrade(&panel);
            panel.disclosure_btn.connect_clicked(move |_| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                panel.set_expanded(!panel.expanded.get());
            });
        }
        panel
    }

    pub(super) fn widget(&self) -> &GtkBox {
        &self.root
    }

    pub(super) fn set_stop_sound_handler(&self, stop_sound: impl Fn(String) + 'static) {
        *self.on_stop_sound.borrow_mut() = Some(Box::new(stop_sound));
    }

    fn set_expanded(&self, expanded: bool) {
        self.expanded.set(expanded);
        self.revealer.set_reveal_child(expanded);
        icons::apply_button_icon(
            &self.disclosure_btn,
            if expanded {
                icons::DISCLOSURE_OPEN
            } else {
                icons::DISCLOSURE_CLOSED
            },
        );
        self.disclosure_btn.set_tooltip_text(Some(if expanded {
            "Collapse Now Playing"
        } else {
            "Expand Now Playing"
        }));
    }

    pub(super) fn handle_snapshot(self: &Rc<Self>, snapshot: &PlayerSnapshot) {
        let active = active_sound_positions(&snapshot.playback_positions);
        let active_ids: Vec<String> = active.iter().map(|p| p.play_id.clone()).collect();
        let active_set: HashSet<&str> = active_ids.iter().map(String::as_str).collect();

        let stale: Vec<String> = self
            .rows
            .borrow()
            .keys()
            .filter(|id| !active_set.contains(id.as_str()))
            .cloned()
            .collect();
        for id in stale {
            self.rows.borrow_mut().remove(&id);
        }

        for position in &active {
            self.ensure_row(position);
            self.update_row(position);
        }
        self.reorder_rows(&active_ids);

        let count = active_ids.len();
        self.title.set_label(&format!("Now Playing ({count})"));
        self.root.set_visible(should_show_panel(count));
    }

    fn ensure_row(self: &Rc<Self>, position: &PlaybackPosition) {
        if self.rows.borrow().contains_key(&position.play_id) {
            return;
        }

        let row_box = GtkBox::new(Orientation::Horizontal, 5);
        row_box.add_css_class("now-playing-row");
        row_box.set_valign(Align::Center);
        row_box.set_hexpand(true);
        row_box.set_size_request(330, 36);
        if position.paused {
            row_box.add_css_class("paused");
        }

        let title = Label::builder()
            .label("Loading…")
            .xalign(0.0)
            .hexpand(false)
            .width_request(96)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .max_width_chars(14)
            .css_classes(vec!["now-playing-track-name"])
            .build();

        let progress = Scale::with_range(Orientation::Horizontal, 0.0, 1.0, 0.001);
        progress.set_draw_value(false);
        progress.set_hexpand(true);
        progress.set_width_request(80);
        progress.set_height_request(18);
        progress.set_valign(Align::Center);
        progress.set_tooltip_text(Some("Seek this playback"));
        progress.update_property(&[gtk4::accessible::Property::Label("Seek playback")]);
        progress.add_css_class("now-playing-progress");

        let time = Label::builder()
            .label("0:00 / --:--")
            .width_chars(11)
            .xalign(1.0)
            .css_classes(vec!["monospace", "now-playing-time"])
            .build();

        let pause_btn = icons::button(
            pause_button_icon(position.paused),
            pause_button_tooltip(position.paused),
        );
        pause_btn.add_css_class("now-playing-row-btn");
        pause_btn.update_property(&[gtk4::accessible::Property::Label(
            pause_button_accessible_label(position.paused),
        )]);
        pause_btn.set_size_request(24, 24);
        pause_btn.set_valign(Align::Center);
        pause_btn.set_halign(Align::Center);

        let stop_btn = icons::button(icons::STOP, "Stop this playback");
        stop_btn.add_css_class("now-playing-row-btn");
        stop_btn.update_property(&[gtk4::accessible::Property::Label("Stop playback")]);
        stop_btn.set_size_request(24, 24);
        stop_btn.set_valign(Align::Center);
        stop_btn.set_halign(Align::Center);

        row_box.append(&title);
        row_box.append(&progress);
        row_box.append(&time);
        row_box.append(&pause_btn);
        row_box.append(&stop_btn);

        let paused = Rc::new(Cell::new(position.paused));
        let current_play_id = Rc::new(RefCell::new(position.play_id.clone()));
        let current_duration_ms = Rc::new(Cell::new(0));
        let syncing_progress = Rc::new(Cell::new(false));
        let pending_seek = Rc::new(Cell::new(None));
        {
            let weak = Rc::downgrade(self);
            let play_id = Rc::clone(&current_play_id);
            let duration_ms = Rc::clone(&current_duration_ms);
            let syncing = Rc::clone(&syncing_progress);
            let pending = Rc::clone(&pending_seek);
            let timeout: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
            progress.connect_value_changed(move |scale| {
                if syncing.get() || duration_ms.get() == 0 {
                    return;
                }
                let fraction = scale.value().clamp(0.0, 1.0);
                pending.set(Some((fraction, glib::monotonic_time() + 800_000)));
                if let Some(source) = timeout.borrow_mut().take() {
                    let _ = remove_source_id_safe(source);
                }
                let weak = weak.clone();
                let play_id = Rc::clone(&play_id);
                let duration_ms = Rc::clone(&duration_ms);
                let pending = Rc::clone(&pending);
                let timeout_done = Rc::clone(&timeout);
                let source = glib::timeout_add_local_once(Duration::from_millis(100), move || {
                    *timeout_done.borrow_mut() = None;
                    let Some(panel) = weak.upgrade() else {
                        return;
                    };
                    let Some((fraction, _)) = pending.get() else {
                        return;
                    };
                    panel.seek_playback(&play_id.borrow(), duration_ms.get(), fraction);
                });
                *timeout.borrow_mut() = Some(source);
            });
        }
        {
            let weak = Rc::downgrade(self);
            let play_id = Rc::clone(&current_play_id);
            let paused = Rc::clone(&paused);
            let row_root = row_box.clone();
            pause_btn.connect_clicked(move |button| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                let next_paused = !paused.get();
                if let Err(error) = commands::set_playback_paused(
                    play_id.borrow().clone(),
                    next_paused,
                    panel.state.player.clone(),
                ) {
                    log::warn!("Now Playing pause failed: {error}");
                } else {
                    paused.set(next_paused);
                    apply_pause_button_state(button, &row_root, next_paused);
                }
            });
        }
        {
            let weak = Rc::downgrade(self);
            let play_id = Rc::clone(&current_play_id);
            stop_btn.connect_clicked(move |_| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                {
                    let handler = panel.on_stop_sound.borrow();
                    if let Some(handler) = handler.as_ref() {
                        handler(play_id.borrow().clone());
                    }
                }
            });
        }

        self.rows.borrow_mut().insert(
            position.play_id.clone(),
            PlayingRow {
                root: row_box,
                sound_id: position.sound_id.clone(),
                title,
                progress,
                time,
                pause_btn,
                paused,
                play_id: current_play_id,
                duration_ms: current_duration_ms,
                syncing_progress,
                pending_seek,
            },
        );
        self.resolve_name_async(&position.sound_id);
    }

    fn update_row(&self, position: &PlaybackPosition) {
        let rows = self.rows.borrow();
        let Some(row) = rows.get(&position.play_id) else {
            return;
        };

        let pause_changed = row.paused.replace(position.paused) != position.paused;
        *row.play_id.borrow_mut() = position.play_id.clone();
        let duration_ms = position.duration_ms.unwrap_or(0);
        if row.duration_ms.replace(duration_ms) != duration_ms && duration_ms > 0 {
            let adjustment = row.progress.adjustment();
            adjustment.set_step_increment((5_000.0 / duration_ms as f64).min(1.0));
            adjustment.set_page_increment((30_000.0 / duration_ms as f64).min(1.0));
        }
        if pause_changed {
            apply_pause_button_state(&row.pause_btn, &row.root, position.paused);
        }

        let fraction = playback_progress(position.position_ms, position.duration_ms);
        let pending = row.pending_seek.get();
        if pending.is_none_or(|(target, deadline)| {
            glib::monotonic_time() >= deadline || (fraction - target).abs() < 0.02
        }) {
            row.pending_seek.set(None);
            row.syncing_progress.set(true);
            row.progress.set_value(fraction);
            row.syncing_progress.set(false);
        }
        row.progress
            .set_sensitive(position.duration_ms.is_some_and(|duration| duration > 0));

        let position_text = format_duration(position.position_ms);
        let duration_text = position
            .duration_ms
            .map(format_duration)
            .unwrap_or_else(|| "--:--".to_string());
        row.time
            .set_label(&format!("{position_text} / {duration_text}"));
    }

    fn reorder_rows(&self, ids: &[String]) {
        if self.order.borrow().as_slice() == ids {
            return;
        }

        let widgets: Vec<GtkBox> = {
            let rows = self.rows.borrow();
            ids.iter()
                .filter_map(|id| rows.get(id).map(|row| row.root.clone()))
                .collect()
        };

        while let Some(child) = self.rows_box.first_child() {
            self.rows_box.remove(&child);
        }
        for widget in widgets {
            self.rows_box.insert(&widget, -1);
        }
        *self.order.borrow_mut() = ids.to_vec();
    }

    fn resolve_name_async(self: &Rc<Self>, sound_id: &str) {
        if !self
            .resolving_names
            .borrow_mut()
            .insert(sound_id.to_string())
        {
            return;
        }

        let response = self.state.library.sound_by_id(sound_id);
        let weak = Rc::downgrade(self);
        let sound_id = sound_id.to_string();
        let callback_id = sound_id.clone();
        if let Err(error) = commands::dispatch_async_result(
            "resolve_now_playing_track_name",
            move || response.recv(),
            move |result| {
                let Some(panel) = weak.upgrade() else {
                    return;
                };
                panel.resolving_names.borrow_mut().remove(&callback_id);
                let Ok(Some(sound)) = result else {
                    return;
                };
                {
                    let rows = panel.rows.borrow();
                    for row in rows.values().filter(|row| row.sound_id == callback_id) {
                        row.title.set_label(&sound.name);
                        row.title.set_tooltip_text(Some(&sound.name));
                        row.progress
                            .update_property(&[gtk4::accessible::Property::Label(&format!(
                                "Seek {}",
                                sound.name
                            ))]);
                    }
                }
            },
        ) {
            self.resolving_names.borrow_mut().remove(&sound_id);
            log::warn!("Could not resolve Now Playing name for {sound_id}: {error}");
        }
    }

    fn seek_playback(&self, play_id: &str, duration_ms: u64, fraction: f64) {
        if duration_ms == 0 {
            return;
        }
        if let Err(error) = commands::seek_playback(
            play_id.to_string(),
            seek_position_ms(fraction, duration_ms),
            self.state.player.clone(),
        ) {
            log::warn!("Now Playing seek failed: {error}");
        }
    }
}

fn pause_button_icon(paused: bool) -> icons::IconPair {
    if paused {
        icons::PLAY
    } else {
        icons::PAUSE
    }
}

fn pause_button_tooltip(paused: bool) -> &'static str {
    if paused {
        "Resume this playback"
    } else {
        "Pause this playback"
    }
}

fn pause_button_accessible_label(paused: bool) -> &'static str {
    if paused {
        "Resume playback"
    } else {
        "Pause playback"
    }
}

fn apply_pause_button_state(button: &Button, root: &GtkBox, paused: bool) {
    icons::apply_button_icon(button, pause_button_icon(paused));
    button.set_tooltip_text(Some(pause_button_tooltip(paused)));
    button.update_property(&[gtk4::accessible::Property::Label(
        pause_button_accessible_label(paused),
    )]);
    if paused {
        root.add_css_class("paused");
    } else {
        root.remove_css_class("paused");
    }
}

fn playback_progress(position_ms: u64, duration_ms: Option<u64>) -> f64 {
    duration_ms
        .filter(|duration| *duration > 0)
        .map(|duration| position_ms as f64 / duration as f64)
        .unwrap_or(0.0)
        .clamp(0.0, 1.0)
}

fn seek_position_ms(progress: f64, duration_ms: u64) -> u64 {
    (progress.clamp(0.0, 1.0) * duration_ms as f64).round() as u64
}

fn active_sound_positions(positions: &[PlaybackPosition]) -> Vec<PlaybackPosition> {
    positions
        .iter()
        .filter(|position| !position.finished)
        .cloned()
        .collect()
}

fn should_show_panel(active_sound_count: usize) -> bool {
    active_sound_count >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn position(play_id: &str, sound_id: &str, finished: bool) -> PlaybackPosition {
        PlaybackPosition {
            play_id: play_id.to_string(),
            sound_id: sound_id.to_string(),
            position_ms: 100,
            paused: false,
            finished,
            duration_ms: Some(1_000),
        }
    }

    #[test]
    fn pause_button_icon_tracks_playback_state() {
        assert_eq!(pause_button_icon(false), icons::PAUSE);
        assert_eq!(pause_button_icon(true), icons::PLAY);
        assert_eq!(pause_button_tooltip(false), "Pause this playback");
        assert_eq!(pause_button_tooltip(true), "Resume this playback");
        assert_eq!(pause_button_accessible_label(false), "Pause playback");
        assert_eq!(pause_button_accessible_label(true), "Resume playback");
    }

    #[test]
    fn progress_uses_each_tracks_own_duration() {
        assert!((playback_progress(30_000, Some(60_000)) - 0.5).abs() < f64::EPSILON);
        assert!((playback_progress(30_000, Some(300_000)) - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn seek_fraction_maps_to_the_exact_track_duration() {
        assert_eq!(seek_position_ms(0.25, 80_000), 20_000);
        assert_eq!(seek_position_ms(0.75, 300_000), 225_000);
        assert_eq!(seek_position_ms(-1.0, 80_000), 0);
        assert_eq!(seek_position_ms(2.0, 80_000), 80_000);
    }

    #[test]
    fn panel_is_only_needed_for_multiple_playbacks() {
        assert!(!should_show_panel(0));
        assert!(!should_show_panel(1));
        assert!(should_show_panel(2));
        assert!(should_show_panel(8));
    }

    #[test]
    fn active_rows_ignore_finished_voices_and_keep_duplicate_sound_ids() {
        let positions = vec![
            position("new-a", "a", false),
            position("old-a", "a", false),
            position("done", "c", true),
            position("play-b", "b", false),
        ];
        let active = active_sound_positions(&positions);
        assert_eq!(active.len(), 3);
        assert_eq!(active[0].play_id, "new-a");
        assert_eq!(active[0].sound_id, "a");
        assert_eq!(active[1].play_id, "old-a");
        assert_eq!(active[2].sound_id, "b");
    }
}
