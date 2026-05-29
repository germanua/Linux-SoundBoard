use std::cell::{Cell, RefCell};

use crate::audio::PlayerSnapshot;

type StringHandler = RefCell<Option<Box<dyn FnMut(String)>>>;
type SnapshotHandler = RefCell<Option<Box<dyn FnMut(PlayerSnapshot)>>>;

thread_local! {
    static HOTKEY_HANDLER: StringHandler = RefCell::new(None);
    static TOAST_HANDLER: StringHandler = RefCell::new(None);
    static LOUDNESS_STATUS_REFRESH_HANDLER: RefCell<Option<Box<dyn FnMut()>>> =
        RefCell::new(None);
    static SNAPSHOT_HANDLER: SnapshotHandler = RefCell::new(None);

    /// Set to true on the GTK main thread immediately before dispatching a
    /// user-initiated play request. Prevents Continue-mode auto-advance from
    /// firing on the transient "all stopped" snapshot that the engine emits
    /// between stop_all() and the subsequent play() IPC calls.
    static EXPLICIT_PLAY_PENDING: Cell<bool> = const { Cell::new(false) };
}

pub fn set_hotkey_handler(f: impl FnMut(String) + 'static) {
    HOTKEY_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_hotkey(id: String) {
    glib::MainContext::default().invoke(move || {
        HOTKEY_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(id);
            }
        });
    });
}

pub fn set_toast_handler(f: impl FnMut(String) + 'static) {
    TOAST_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_toast(message: String) {
    glib::MainContext::default().invoke(move || {
        TOAST_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(message);
            }
        });
    });
}

pub fn set_loudness_status_refresh_handler(f: impl FnMut() + 'static) {
    LOUDNESS_STATUS_REFRESH_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_loudness_status_refresh() {
    glib::MainContext::default().invoke(move || {
        LOUDNESS_STATUS_REFRESH_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler();
            }
        });
    });
}

/// Register the GTK-thread handler that receives snapshots from the audio engine.
/// Must be called on the GTK main thread.
pub fn set_snapshot_handler(f: impl FnMut(PlayerSnapshot) + 'static) {
    SNAPSHOT_HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

/// Called by the audio engine (via glib::MainContext::default().invoke()) on the GTK thread.
pub fn dispatch_snapshot(snapshot: PlayerSnapshot) {
    SNAPSHOT_HANDLER.with(|h| {
        if let Some(handler) = h.borrow_mut().as_mut() {
            handler(snapshot);
        }
    });
}

/// Mark that a user-initiated sound play has just been dispatched.
/// Must be called on the GTK main thread before `play_sound_async`.
pub fn mark_explicit_play_pending() {
    EXPLICIT_PLAY_PENDING.with(|p| p.set(true));
}

/// Clear the pending-play flag. Called when the new sound appears in a
/// snapshot (success) or when the play fails (error callback).
pub fn clear_explicit_play_pending() {
    EXPLICIT_PLAY_PENDING.with(|p| p.set(false));
}

/// Returns true if a user-initiated play has been dispatched but the resulting
/// playback has not yet appeared in a snapshot.
pub fn is_explicit_play_pending() -> bool {
    EXPLICIT_PLAY_PENDING.with(|p| p.get())
}
