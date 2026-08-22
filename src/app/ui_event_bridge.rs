use std::cell::{Cell, RefCell};

use crate::audio::PlayerSnapshot;
use crate::config::GroupMode;
use crate::mpris::{MprisCommand, NowPlaying};
use crate::tray::{MenuItem, TrayAction};

type StringHandler = RefCell<Option<Box<dyn FnMut(String)>>>;
type GroupModeHandler = RefCell<Option<Box<dyn FnMut(GroupMode)>>>;
type SnapshotHandler = RefCell<Option<Box<dyn FnMut(PlayerSnapshot)>>>;
type TrayActionHandler = RefCell<Option<Box<dyn FnMut(TrayAction)>>>;
type TrayMenuHandler = RefCell<Option<Box<dyn FnMut(Vec<MenuItem>)>>>;
type TrayEnabledHandler = RefCell<Option<Box<dyn FnMut(bool)>>>;
type NowPlayingHandler = RefCell<Option<Box<dyn FnMut(Option<NowPlaying>)>>>;
type MprisCommandHandler = RefCell<Option<Box<dyn FnMut(MprisCommand)>>>;

thread_local! {
    static HOTKEY_HANDLER: StringHandler = RefCell::new(None);
    static TOAST_HANDLER: StringHandler = RefCell::new(None);
    static LOUDNESS_STATUS_REFRESH_HANDLER: RefCell<Option<Box<dyn FnMut()>>> =
        RefCell::new(None);
    static SNAPSHOT_HANDLER: SnapshotHandler = RefCell::new(None);
    static GROUP_MODE_HANDLER: GroupModeHandler = RefCell::new(None);

    static TRAY_ACTION_HANDLER: TrayActionHandler = RefCell::new(None);
    static TRAY_MENU_HANDLER: TrayMenuHandler = RefCell::new(None);
    static TRAY_ENABLED_HANDLER: TrayEnabledHandler = RefCell::new(None);

    static NOW_PLAYING_HANDLER: NowPlayingHandler = RefCell::new(None);
    static MPRIS_COMMAND_HANDLER: MprisCommandHandler = RefCell::new(None);

    static CLOSE_TO_TRAY_POLICY: RefCell<Option<Box<dyn Fn() -> bool>>> = RefCell::new(None);

    static QUIT_REQUESTED: Cell<bool> = const { Cell::new(false) };

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

pub fn set_tray_enabled_handler(f: impl FnMut(bool) + 'static) {
    TRAY_ENABLED_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

/// Show or withdraw the tray icon after the setting changed.
pub fn post_tray_enabled(enabled: bool) {
    glib::MainContext::default().invoke(move || {
        TRAY_ENABLED_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(enabled);
            }
        });
    });
}

pub fn set_now_playing_handler(f: impl FnMut(Option<NowPlaying>) + 'static) {
    NOW_PLAYING_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

/// Announce the sound that started, or `None` when playback stopped.
pub fn post_now_playing(now: Option<NowPlaying>) {
    glib::MainContext::default().invoke(move || {
        NOW_PLAYING_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(now.clone());
            }
        });
    });
}

pub fn set_mpris_command_handler(f: impl FnMut(MprisCommand) + 'static) {
    MPRIS_COMMAND_HANDLER.with(|handler| *handler.borrow_mut() = Some(Box::new(f)));
}

pub fn post_mpris_command(command: MprisCommand) {
    glib::MainContext::default().invoke(move || {
        MPRIS_COMMAND_HANDLER.with(|handler| {
            if let Some(handler) = handler.borrow_mut().as_mut() {
                handler(command);
            }
        });
    });
}

pub fn set_close_to_tray_policy(f: impl Fn() -> bool + 'static) {
    CLOSE_TO_TRAY_POLICY.with(|policy| *policy.borrow_mut() = Some(Box::new(f)));
}

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

/// GTK-thread handler for engine snapshots. Main thread only.
pub fn set_snapshot_handler(f: impl FnMut(PlayerSnapshot) + 'static) {
    SNAPSHOT_HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(f)));
}

pub fn dispatch_snapshot(snapshot: PlayerSnapshot) {
    SNAPSHOT_HANDLER.with(|h| {
        if let Some(handler) = h.borrow_mut().as_mut() {
            handler(snapshot);
        }
    });
}

/// Flag a user-initiated play. Main thread, before `play_sound_async`.
pub fn mark_explicit_play_pending() {
    EXPLICIT_PLAY_PENDING.with(|p| p.set(true));
}

pub fn clear_explicit_play_pending() {
    EXPLICIT_PLAY_PENDING.with(|p| p.set(false));
}

/// True while a user-initiated play is out but hasn't shown up in a snapshot.
pub fn is_explicit_play_pending() -> bool {
    EXPLICIT_PLAY_PENDING.with(|p| p.get())
}
