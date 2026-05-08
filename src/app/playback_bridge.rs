use std::cell::{Cell, RefCell};

use crate::audio::player::PlayerSnapshot;

thread_local! {
    static SNAPSHOT_HANDLER: RefCell<Option<Box<dyn FnMut(PlayerSnapshot)>>> =
        RefCell::new(None);

    /// Set to true on the GTK main thread immediately before dispatching a
    /// user-initiated play request.  Prevents Continue-mode auto-advance from
    /// firing on the transient "all stopped" snapshot that the engine emits
    /// between stop_all() and the subsequent play() IPC calls.
    static EXPLICIT_PLAY_PENDING: Cell<bool> = Cell::new(false);
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

/// Clear the pending-play flag.  Called when the new sound appears in a
/// snapshot (success) or when the play fails (error callback).
pub fn clear_explicit_play_pending() {
    EXPLICIT_PLAY_PENDING.with(|p| p.set(false));
}

/// Returns true if a user-initiated play has been dispatched but the resulting
/// playback has not yet appeared in a snapshot.
pub fn is_explicit_play_pending() -> bool {
    EXPLICIT_PLAY_PENDING.with(|p| p.get())
}
