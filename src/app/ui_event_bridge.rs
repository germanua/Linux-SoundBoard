use std::cell::{Cell, RefCell};

use crate::audio::PlayerSnapshot;
use crate::config::GroupMode;
use crate::tray::{MenuItem, TrayAction};

type StringHandler = RefCell<Option<Box<dyn FnMut(String)>>>;
type GroupModeHandler = RefCell<Option<Box<dyn FnMut(GroupMode)>>>;
type SnapshotHandler = RefCell<Option<Box<dyn FnMut(PlayerSnapshot)>>>;
type TrayActionHandler = RefCell<Option<Box<dyn FnMut(TrayAction)>>>;
type TrayMenuHandler = RefCell<Option<Box<dyn FnMut(Vec<MenuItem>)>>>;

thread_local! {
    static HOTKEY_HANDLER: StringHandler = RefCell::new(None);
    static TOAST_HANDLER: StringHandler = RefCell::new(None);
    static LOUDNESS_STATUS_REFRESH_HANDLER: RefCell<Option<Box<dyn FnMut()>>> =
        RefCell::new(None);
    static SNAPSHOT_HANDLER: SnapshotHandler = RefCell::new(None);
    /// The settings panel is built once and kept, so a mode changed by hotkey
    /// would otherwise still read the old value the next time it is opened.
    static GROUP_MODE_HANDLER: GroupModeHandler = RefCell::new(None);

    /// The tray lives in `bootstrap`, which has no transport or window, while
    /// the code that can act on a click lives in the window. These two carry
    /// clicks one way and the rebuilt menu back the other.
    static TRAY_ACTION_HANDLER: TrayActionHandler = RefCell::new(None);
    static TRAY_MENU_HANDLER: TrayMenuHandler = RefCell::new(None);

    /// Answers "should the close button hide the window instead of quitting?".
    /// Owned by `bootstrap`, which knows both the setting and whether a panel
    /// is really showing the icon, but consulted from the window's own
    /// close-request handler — the earliest one to run, and so the only place
    /// that can stop the teardown before it starts.
    static CLOSE_TO_TRAY_POLICY: RefCell<Option<Box<dyn Fn() -> bool>>> = RefCell::new(None);

    /// Set once when the user asks to quit from the tray. Without it, closing
    /// the window would consult the policy above and hide it again.
    static QUIT_REQUESTED: Cell<bool> = const { Cell::new(false) };

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

pub fn set_group_mode_handler(f: impl FnMut(GroupMode) + 'static) {
    GROUP_MODE_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_group_mode_changed(mode: GroupMode) {
    glib::MainContext::default().invoke(move || {
        GROUP_MODE_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(mode);
            }
        });
    });
}

pub fn set_tray_action_handler(f: impl FnMut(TrayAction) + 'static) {
    TRAY_ACTION_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_tray_action(action: TrayAction) {
    glib::MainContext::default().invoke(move || {
        TRAY_ACTION_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(action);
            }
        });
    });
}

pub fn set_tray_menu_handler(f: impl FnMut(Vec<MenuItem>) + 'static) {
    TRAY_MENU_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_tray_menu(items: Vec<MenuItem>) {
    glib::MainContext::default().invoke(move || {
        TRAY_MENU_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(items);
            }
        });
    });
}

pub fn set_close_to_tray_policy(f: impl Fn() -> bool + 'static) {
    CLOSE_TO_TRAY_POLICY.with(|policy| *policy.borrow_mut() = Some(Box::new(f)));
}

/// Whether closing the window should hide it. False unless a policy has been
/// installed and agrees, so the close button keeps quitting when there is no
/// tray to hide into.
pub fn close_should_hide_to_tray() -> bool {
    if QUIT_REQUESTED.with(|requested| requested.get()) {
        return false;
    }
    CLOSE_TO_TRAY_POLICY.with(|policy| {
        policy
            .borrow()
            .as_ref()
            .is_some_and(|should_hide| should_hide())
    })
}

/// Record that the next window close is a real quit. One-shot: the process is
/// on its way out, so it is never cleared.
pub fn mark_quit_requested() {
    QUIT_REQUESTED.with(|requested| requested.set(true));
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
