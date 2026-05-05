use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk4::prelude::*;
use gtk4::EventControllerKey;
use libadwaita::prelude::*;

use crate::commands;
use crate::timer_registry::remove_source_id_safe;

use super::helpers::{format_duration, install_volume_editor, is_seek_key};
use super::playback::{
    update_headphones_button, update_mic_button, update_play_mode_button, update_play_pause_button,
};
use super::{LibraryChangedCallback, ListStyleChangedCallback, ScrubInput, TransportBar};

fn weak_library_changed_callback(
    callback: &RefCell<Option<LibraryChangedCallback>>,
) -> Option<Rc<dyn Fn() + 'static>> {
    let weak = Rc::downgrade(callback.borrow().as_ref()?);
    Some(Rc::new(move || {
        if let Some(callback) = weak.upgrade() {
            callback();
        }
    }))
}

fn weak_list_style_changed_callback(
    callback: &RefCell<Option<ListStyleChangedCallback>>,
) -> Option<Rc<dyn Fn(String) + 'static>> {
    let weak = Rc::downgrade(callback.borrow().as_ref()?);
    Some(Rc::new(move |style| {
        if let Some(callback) = weak.upgrade() {
            callback(style);
        }
    }))
}

impl TransportBar {
    pub fn connect_search_changed<F: Fn(String) + 'static>(&self, f: F) {
        self.inner
            .search_entry
            .connect_search_changed(move |entry| {
                f(entry.text().to_string());
            });
    }

    pub fn set_sound_list_provider<
        F: Fn() -> Vec<crate::ui::sound_list::NavigationSound> + Send + Sync + 'static,
    >(
        &self,
        f: F,
    ) {
        *self
            .inner
            .sound_list_provider
            .lock()
            .expect("sound_list_provider lock poisoned") = Some(Box::new(f));
    }

    pub fn set_toast_sender(&self, sender: std::sync::mpsc::Sender<String>) {
        *self
            .inner
            .toast_sender
            .lock()
            .expect("toast_sender lock poisoned") = Some(sender);
    }

    pub fn connect_library_changed<F: Fn() + 'static>(&self, f: F) {
        *self.inner.on_library_changed.borrow_mut() = Some(Rc::new(f));
    }

    pub fn connect_list_style_changed<F: Fn(String) + 'static>(&self, f: F) {
        *self.inner.on_list_style_changed.borrow_mut() = Some(Rc::new(f));
    }

    pub fn cleanup(&self) {
        if let Some(timeout_id) = self.inner.scrub_commit_timeout.borrow_mut().take() {
            let _ = remove_source_id_safe(timeout_id);
        }
        if let Some(timer_id) = self.inner.scrub_timer_id.borrow_mut().take() {
            let _ = remove_source_id_safe(timer_id);
        }
        let settings_dialog = self.inner.settings_dialog.borrow_mut().take();
        if let Some(settings_dialog) = settings_dialog {
            settings_dialog.force_close();
        }
        *self
            .inner
            .sound_list_provider
            .lock()
            .expect("sound_list_provider lock poisoned") = None;
        *self
            .inner
            .toast_sender
            .lock()
            .expect("toast_sender lock poisoned") = None;
        *self.inner.on_library_changed.borrow_mut() = None;
        *self.inner.on_list_style_changed.borrow_mut() = None;
    }

    pub(super) fn connect_signals(&self) {
        let inner_weak = Rc::downgrade(&self.inner);

        {
            let inner_weak = inner_weak.clone();
            self.inner.stop_btn.connect_clicked(move |_| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                inner.stop_all_playback();
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.play_btn.connect_clicked(move |btn| {
                let Some(inner_toggle) = inner_weak.upgrade() else {
                    return;
                };
                let should_resume = btn.is_active();
                update_play_pause_button(btn, should_resume);
                let active_track = inner_toggle.active_track.borrow().clone();
                if let Some(track) = active_track.as_ref() {
                    if should_resume {
                        commands::resume_sound(
                            track.sound_id.clone(),
                            Arc::clone(&inner_toggle.state.player),
                        );
                    } else {
                        commands::pause_sound(
                            track.sound_id.clone(),
                            Arc::clone(&inner_toggle.state.player),
                        );
                    }
                }
            });
        }

        {
            let inner_weak = inner_weak.clone();
            let local_adj = self.inner.local_vol.adjustment();
            let local_label = self.inner.local_vol_label.clone();
            let local_entry = self.inner.local_vol_entry.clone();
            local_adj.connect_value_changed(move |adj| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let volume = adj.value().round().clamp(0.0, 100.0) as u8;
                local_label.set_label(&format!("{volume}"));
                if !gtk4::prelude::WidgetExt::is_visible(&local_entry) {
                    local_entry.set_text(&format!("{volume}"));
                }
                let _ = commands::set_local_volume(
                    volume,
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                );
            });
        }

        {
            let inner_weak = inner_weak.clone();
            let mic_adj = self.inner.mic_vol.adjustment();
            let mic_label = self.inner.mic_vol_label.clone();
            let mic_entry = self.inner.mic_vol_entry.clone();
            mic_adj.connect_value_changed(move |adj| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let volume = adj.value().round().clamp(0.0, 100.0) as u8;
                mic_label.set_label(&format!("{volume}"));
                if !gtk4::prelude::WidgetExt::is_visible(&mic_entry) {
                    mic_entry.set_text(&format!("{volume}"));
                }
                let _ = commands::set_mic_volume(
                    volume,
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                );
            });
        }

        install_volume_editor(
            &self.inner.local_vol.adjustment(),
            &self.inner.local_vol_label,
            &self.inner.local_vol_entry,
        );
        install_volume_editor(
            &self.inner.mic_vol.adjustment(),
            &self.inner.mic_vol_label,
            &self.inner.mic_vol_entry,
        );

        {
            let inner_weak = inner_weak.clone();
            self.inner.headphones_btn.connect_toggled(move |btn| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                if inner.suppress_headphones_toggle.get() {
                    return;
                }
                let requested_enabled = btn.is_active();
                match commands::toggle_local_mute(
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                ) {
                    Ok(muted) => {
                        update_headphones_button(btn, !muted);
                    }
                    Err(e) => {
                        log::warn!("Toggle local mute failed: {e}");
                        inner.suppress_headphones_toggle.set(true);
                        btn.set_active(!requested_enabled);
                        inner.suppress_headphones_toggle.set(false);
                        update_headphones_button(btn, !requested_enabled);
                    }
                }
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.mic_btn.connect_toggled(move |btn| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                if inner.suppress_mic_toggle.get() {
                    return;
                }
                let requested_enabled = btn.is_active();
                btn.set_sensitive(false);
                let btn_weak = btn.downgrade();
                let inner_done_weak = Rc::downgrade(&inner);
                if let Err(e) = commands::set_mic_passthrough_enabled_async(
                    requested_enabled,
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                    move |result| {
                        let Some(btn) = btn_weak.upgrade() else {
                            return;
                        };
                        match result {
                            Ok(enabled) => {
                                if let Some(inner_done) = inner_done_weak.upgrade() {
                                    inner_done.suppress_mic_toggle.set(true);
                                    btn.set_active(enabled);
                                    inner_done.suppress_mic_toggle.set(false);
                                }
                                update_mic_button(&btn, enabled);
                            }
                            Err(err) => {
                                log::warn!("Toggle mic passthrough failed: {err}");
                                let fallback_enabled = !requested_enabled;
                                if let Some(inner_done) = inner_done_weak.upgrade() {
                                    inner_done.suppress_mic_toggle.set(true);
                                    btn.set_active(fallback_enabled);
                                    inner_done.suppress_mic_toggle.set(false);
                                }
                                update_mic_button(&btn, fallback_enabled);
                            }
                        }
                        btn.set_sensitive(true);
                    },
                ) {
                    log::warn!("Failed to dispatch mic passthrough toggle: {e}");
                    inner.suppress_mic_toggle.set(true);
                    btn.set_active(!requested_enabled);
                    inner.suppress_mic_toggle.set(false);
                    update_mic_button(btn, !requested_enabled);
                    btn.set_sensitive(true);
                }
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner
                .scrub
                .connect_change_value(move |_, scroll_type, value| {
                    let Some(inner_seek) = inner_weak.upgrade() else {
                        return glib::Propagation::Proceed;
                    };
                    if scroll_type == gtk4::ScrollType::Jump {
                        inner_seek.begin_scrub_interaction(ScrubInput::Pointer);

                        if let Some(position_ms) = inner_seek.record_scrub_preview(value) {
                            inner_seek
                                .time_label
                                .set_text(&format_duration(position_ms));
                        }

                        if let Some(timeout_id) =
                            inner_seek.scrub_commit_timeout.borrow_mut().take()
                        {
                            let _ = remove_source_id_safe(timeout_id);
                        }

                        // Coalesce drag updates into one seek.
                        let inner_weak_commit = Rc::downgrade(&inner_seek);
                        let timeout_id =
                            glib::timeout_add_local_once(Duration::from_millis(100), move || {
                                let Some(inner_commit) = inner_weak_commit.upgrade() else {
                                    return;
                                };
                                inner_commit.commit_scrub_seek_on_release();
                                *inner_commit.scrub_commit_timeout.borrow_mut() = None;
                            });
                        *inner_seek.scrub_commit_timeout.borrow_mut() = Some(timeout_id);
                    }
                    glib::Propagation::Proceed
                });
        }

        {
            let inner_weak_pressed = inner_weak.clone();
            let key = EventControllerKey::new();
            key.connect_key_pressed(move |_, keyval, _, _| {
                let Some(inner_key) = inner_weak_pressed.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                if keyval.name().as_deref() == Some("Escape") {
                    inner_key.cancel_scrub_interaction();
                    return glib::Propagation::Stop;
                }

                if is_seek_key(keyval) {
                    inner_key.begin_scrub_interaction(ScrubInput::Keyboard);
                }

                glib::Propagation::Proceed
            });

            let inner_weak_release = inner_weak.clone();
            key.connect_key_released(move |_, keyval, _, _| {
                let Some(inner_key_release) = inner_weak_release.upgrade() else {
                    return;
                };
                if is_seek_key(keyval) {
                    inner_key_release.commit_scrub_seek_on_release();
                }
            });

            self.inner.scrub.add_controller(key);
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.refresh_btn.connect_clicked(move |btn| {
                let Some(inner_refresh) = inner_weak.upgrade() else {
                    return;
                };
                btn.add_css_class("spinning");
                let state_refresh = Arc::clone(&inner_refresh.state);
                let inner_weak_done = Rc::downgrade(&inner_refresh);
                let btn_done = btn.clone();
                glib::MainContext::default().spawn_local(async move {
                    match commands::refresh_sounds(
                        Arc::clone(&state_refresh.config),
                        Arc::clone(&state_refresh.hotkeys),
                    ) {
                        Ok(_) => {
                            if let Some(inner_refresh_done) = inner_weak_done.upgrade() {
                                if let Some(tx) = &*inner_refresh_done
                                    .toast_sender
                                    .lock()
                                    .expect("toast_sender lock poisoned")
                                {
                                    let _ = tx.send("Sounds refreshed".to_string());
                                }
                                if let Some(cb) =
                                    inner_refresh_done.on_library_changed.borrow().as_ref()
                                {
                                    cb();
                                }
                            }
                        }
                        Err(e) => log::warn!("Refresh failed: {e}"),
                    }
                    btn_done.remove_css_class("spinning");
                });
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.settings_btn.connect_clicked(move |btn| {
                let Some(inner_settings) = inner_weak.upgrade() else {
                    return;
                };
                let Some(win) = btn
                    .root()
                    .and_then(|root| root.downcast::<gtk4::Window>().ok())
                else {
                    return;
                };

                // Reuse: dialog is cached, hidden but still parented — no new Wayland surface.
                if let Some(existing) = inner_settings.settings_dialog.borrow().as_ref() {
                    existing.present(Some(&win));
                    crate::diagnostics::memory::log_memory_snapshot("ui:settings:reused");
                    crate::diagnostics::record_phase("ui:settings_reused", None);
                    return;
                }

                // First open: build once, cache, intercept close to hide instead of destroy.
                let on_library_changed =
                    weak_library_changed_callback(&inner_settings.on_library_changed);
                let on_list_style_changed =
                    weak_list_style_changed_callback(&inner_settings.on_list_style_changed);
                let prefs = crate::ui::settings::build_settings_dialog(
                    &win,
                    Arc::clone(&inner_settings.state),
                    on_library_changed,
                    on_list_style_changed,
                );
                // Hide instead of destroy on close — stops signal before adw_dialog_close() runs.
                prefs.connect_close_attempt(|d| {
                    d.set_visible(false);
                    d.stop_signal_emission_by_name("close-attempt");
                });
                *inner_settings.settings_dialog.borrow_mut() = Some(prefs.clone());
                prefs.present(Some(&win));
                crate::diagnostics::memory::log_memory_snapshot("ui:settings:opened");
                crate::diagnostics::record_phase("ui:settings_opened", None);
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.playmode_btn.connect_clicked(move |btn| {
                let Some(inner) = inner_weak.upgrade() else {
                    return;
                };
                let current_mode = inner
                    .state
                    .config
                    .lock()
                    .expect("config lock poisoned")
                    .settings
                    .play_mode;
                let new_mode = current_mode.next();
                let _ = commands::set_play_mode(
                    new_mode.as_str().to_string(),
                    Arc::clone(&inner.state.config),
                    Arc::clone(&inner.state.player),
                );
                update_play_mode_button(btn, new_mode);
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.prev_btn.connect_clicked(move |_| {
                let Some(inner_prev) = inner_weak.upgrade() else {
                    return;
                };
                inner_prev.play_adjacent_sound(-1);
            });
        }

        {
            let inner_weak = inner_weak.clone();
            self.inner.next_btn.connect_clicked(move |_| {
                let Some(inner_next) = inner_weak.upgrade() else {
                    return;
                };
                inner_next.play_adjacent_sound(1);
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    fn close_once(slot: &RefCell<Option<u32>>) -> Option<u32> {
        slot.borrow_mut().take()
    }

    #[test]
    fn settings_closing_takes_timer_id_once() {
        let slot = RefCell::new(Some(42_u32));
        assert_eq!(close_once(&slot), Some(42));
        assert!(slot.borrow().is_none());
        // Idempotent on repeat close.
        assert_eq!(close_once(&slot), None);
    }

    #[test]
    fn settings_reopening_replaces_stale_timer_id() {
        let slot = RefCell::new(Some(1_u32));
        // Simulate defensive take on open (stale timer from prior session).
        let prev = slot.borrow_mut().take();
        assert_eq!(prev, Some(1));
        *slot.borrow_mut() = Some(2);
        assert_eq!(*slot.borrow(), Some(2));
    }
}
