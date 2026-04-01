use gtk4::prelude::*;
use gtk4::{Button, Image, ToggleButton};

#[derive(Clone, Copy)]
pub struct IconPair {
    name: &'static str,
}

impl IconPair {
    pub const fn new(name: &'static str) -> Self {
        Self { name }
    }
}

pub const ADD: IconPair = IconPair::new("lsb-add-symbolic");
pub const REMOVE: IconPair = IconPair::new("lsb-remove-symbolic");
pub const FOLDER_OPEN: IconPair = IconPair::new("lsb-folder-open-symbolic");
pub const FOLDER: IconPair = IconPair::new("lsb-folder-symbolic");
pub const PLAY: IconPair = IconPair::new("lsb-play-symbolic");
pub const PAUSE: IconPair = IconPair::new("lsb-pause-symbolic");
pub const STOP: IconPair = IconPair::new("lsb-stop-symbolic");
pub const PREVIOUS: IconPair = IconPair::new("lsb-previous-symbolic");
pub const NEXT: IconPair = IconPair::new("lsb-next-symbolic");
pub const HEADPHONES: IconPair = IconPair::new("lsb-headphones-symbolic");
pub const HEADPHONES_MUTED: IconPair = IconPair::new("lsb-headphone-off-symbolic");
pub const MICROPHONE: IconPair = IconPair::new("lsb-mic-symbolic");
pub const MICROPHONE_DISABLED: IconPair = IconPair::new("lsb-mic-off-symbolic");
pub const PLAYMODE_DEFAULT: IconPair = IconPair::new("lsb-play-once-symbolic");
pub const PLAYMODE_LOOP: IconPair = IconPair::new("lsb-repeat-symbolic");
pub const PLAYMODE_CONTINUE: IconPair = IconPair::new("lsb-list-end-symbolic");
pub const REFRESH: IconPair = IconPair::new("lsb-refresh-symbolic");
pub const SETTINGS: IconPair = IconPair::new("lsb-settings-symbolic");
pub const KEYBOARD: IconPair = IconPair::new("lsb-keyboard-symbolic");
pub const DROP_ZONE: IconPair = IconPair::new("lsb-drop-zone-symbolic");

pub fn image(icon: IconPair) -> Image {
    Image::from_icon_name(icon.name)
}

pub fn name(icon: IconPair) -> &'static str {
    icon.name
}

pub fn apply_button_icon(button: &impl IsA<gtk4::Button>, icon: IconPair) {
    button.set_icon_name(icon.name);
}

pub fn button(icon: IconPair, tooltip: &str) -> Button {
    let button = Button::builder().tooltip_text(tooltip).build();
    apply_button_icon(&button, icon);
    button
}

pub fn toggle_button(icon: IconPair, tooltip: &str) -> ToggleButton {
    let button = ToggleButton::builder().tooltip_text(tooltip).build();
    apply_button_icon(&button, icon);
    button
}
