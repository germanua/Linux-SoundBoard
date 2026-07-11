pub mod app_window;
pub mod dialogs;
mod dnd_import;
pub mod icons;
pub mod menu;
pub mod settings;
mod settings_folders;
mod settings_hotkeys;
mod settings_mic;
mod settings_playback;
pub mod sound_list;
mod tab_dnd;
pub mod tabs_sidebar;
pub mod theme;
pub mod transport;

pub(super) fn is_unmodified_delete_shortcut(
    keyval: gtk4::gdk::Key,
    modifiers: gtk4::gdk::ModifierType,
) -> bool {
    let shortcut_modifiers = gtk4::gdk::ModifierType::CONTROL_MASK
        | gtk4::gdk::ModifierType::ALT_MASK
        | gtk4::gdk::ModifierType::SHIFT_MASK
        | gtk4::gdk::ModifierType::SUPER_MASK
        | gtk4::gdk::ModifierType::HYPER_MASK
        | gtk4::gdk::ModifierType::META_MASK;
    matches!(keyval, gtk4::gdk::Key::Delete | gtk4::gdk::Key::KP_Delete)
        && !modifiers.intersects(shortcut_modifiers)
}
