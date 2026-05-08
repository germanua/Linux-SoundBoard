use std::cell::RefCell;
use std::collections::HashMap;

use gtk4::prelude::*;
use gtk4::{Button, Image, ToggleButton};

use super::theme::ensure_app_resources;

#[derive(Clone, Copy)]
pub struct IconPair {
    name: &'static str,
    fallbacks: &'static [&'static str],
}

impl IconPair {
    pub const fn new(name: &'static str) -> Self {
        Self {
            name,
            fallbacks: &[],
        }
    }

    pub const fn with_fallbacks(name: &'static str, fallbacks: &'static [&'static str]) -> Self {
        Self { name, fallbacks }
    }
}

pub const ADD: IconPair =
    IconPair::with_fallbacks("lsb-add-symbolic", &["list-add-symbolic", "add-symbolic"]);
pub const DELETE: IconPair = IconPair::with_fallbacks(
    "lsb-delete-symbolic",
    &["edit-delete-symbolic", "user-trash-symbolic"],
);
pub const REMOVE: IconPair = IconPair::with_fallbacks(
    "lsb-remove-symbolic",
    &["list-remove-symbolic", "remove-symbolic"],
);
pub const FOLDER_OPEN: IconPair =
    IconPair::with_fallbacks("lsb-folder-open-symbolic", &["folder-open-symbolic"]);
pub const FOLDER: IconPair = IconPair::with_fallbacks("lsb-folder-symbolic", &["folder-symbolic"]);
pub const PLAY: IconPair =
    IconPair::with_fallbacks("lsb-play-symbolic", &["media-playback-start-symbolic"]);
pub const PAUSE: IconPair =
    IconPair::with_fallbacks("lsb-pause-symbolic", &["media-playback-pause-symbolic"]);
pub const STOP: IconPair =
    IconPair::with_fallbacks("lsb-stop-symbolic", &["media-playback-stop-symbolic"]);
pub const PREVIOUS: IconPair =
    IconPair::with_fallbacks("lsb-previous-symbolic", &["media-skip-backward-symbolic"]);
pub const NEXT: IconPair =
    IconPair::with_fallbacks("lsb-next-symbolic", &["media-skip-forward-symbolic"]);
pub const LOCAL_AUDIO: IconPair = IconPair::with_fallbacks(
    "lsb-speaker-symbolic",
    &["audio-speakers-symbolic", "audio-volume-high-symbolic"],
);
pub const LOCAL_AUDIO_MUTED: IconPair =
    IconPair::with_fallbacks("lsb-speaker-off-symbolic", &["audio-volume-muted-symbolic"]);
pub const HEADPHONES: IconPair =
    IconPair::with_fallbacks("lsb-headphones-symbolic", &["audio-headphones-symbolic"]);
pub const HEADPHONES_MUTED: IconPair = IconPair::with_fallbacks(
    "lsb-headphone-off-symbolic",
    &["audio-volume-muted-symbolic"],
);
pub const MICROPHONE: IconPair =
    IconPair::with_fallbacks("lsb-mic-symbolic", &["audio-input-microphone-symbolic"]);
pub const MICROPHONE_DISABLED: IconPair = IconPair::with_fallbacks(
    "lsb-mic-off-symbolic",
    &[
        "microphone-sensitivity-muted-symbolic",
        "audio-input-microphone-symbolic",
    ],
);
pub const PLAYMODE_DEFAULT: IconPair =
    IconPair::with_fallbacks("lsb-play-once-symbolic", &["media-playback-start-symbolic"]);
pub const PLAYMODE_LOOP: IconPair =
    IconPair::with_fallbacks("lsb-repeat-symbolic", &["media-playlist-repeat-symbolic"]);
pub const PLAYMODE_CONTINUE: IconPair =
    IconPair::with_fallbacks("lsb-list-end-symbolic", &["go-last-symbolic"]);
pub const REFRESH: IconPair =
    IconPair::with_fallbacks("lsb-refresh-symbolic", &["view-refresh-symbolic"]);
pub const SETTINGS: IconPair =
    IconPair::with_fallbacks("lsb-settings-symbolic", &["preferences-system-symbolic"]);
pub const KEYBOARD: IconPair =
    IconPair::with_fallbacks("lsb-keyboard-symbolic", &["input-keyboard-symbolic"]);
pub const DROP_ZONE: IconPair =
    IconPair::with_fallbacks("lsb-drop-zone-symbolic", &["folder-download-symbolic"]);

#[cfg(test)]
const ALL_ICONS: &[IconPair] = &[
    ADD,
    DELETE,
    REMOVE,
    FOLDER_OPEN,
    FOLDER,
    PLAY,
    PAUSE,
    STOP,
    PREVIOUS,
    NEXT,
    LOCAL_AUDIO,
    LOCAL_AUDIO_MUTED,
    HEADPHONES,
    HEADPHONES_MUTED,
    MICROPHONE,
    MICROPHONE_DISABLED,
    PLAYMODE_DEFAULT,
    PLAYMODE_LOOP,
    PLAYMODE_CONTINUE,
    REFRESH,
    SETTINGS,
    KEYBOARD,
    DROP_ZONE,
];

thread_local! {
    static RESOLVED_NAMES: RefCell<HashMap<&'static str, &'static str>> =
        RefCell::new(HashMap::new());
}

fn resolved_name(icon: IconPair) -> &'static str {
    ensure_app_resources();

    let Some(display) = gtk4::gdk::Display::default() else {
        return icon.name;
    };

    if let Some(name) = RESOLVED_NAMES.with(|cache| cache.borrow().get(icon.name).copied()) {
        return name;
    }

    let theme = gtk4::IconTheme::for_display(&display);
    let resolved = if theme.has_icon(icon.name) {
        icon.name
    } else {
        icon.fallbacks
            .iter()
            .copied()
            .find(|fallback| theme.has_icon(fallback))
            .unwrap_or(icon.name)
    };

    RESOLVED_NAMES.with(|cache| {
        cache.borrow_mut().insert(icon.name, resolved);
    });
    resolved
}

pub fn image(icon: IconPair) -> Image {
    Image::from_icon_name(resolved_name(icon))
}

pub fn name(icon: IconPair) -> &'static str {
    resolved_name(icon)
}

pub fn apply_button_icon(button: &impl IsA<gtk4::Button>, icon: IconPair) {
    let resolved = resolved_name(icon);
    if button.as_ref().icon_name().as_deref() != Some(resolved) {
        button.set_icon_name(resolved);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn collect_svg_files(dir: &Path, files: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("read icon directory") {
            let entry = entry.expect("read icon entry");
            let path = entry.path();
            if path.is_dir() {
                collect_svg_files(&path, files);
            } else if path.extension().is_some_and(|ext| ext == "svg") {
                files.push(path);
            }
        }
    }

    #[test]
    fn bundled_icons_are_registered_as_resources() {
        let manifest = include_str!("../../resources/resources.gresource.xml");

        for icon in ALL_ICONS {
            if icon.name.starts_with("lsb-") {
                let filename = format!("{}.svg", icon.name);
                assert!(
                    manifest.contains(&filename),
                    "missing bundled icon resource entry for {}",
                    icon.name
                );
            }
        }
    }

    #[test]
    fn bundled_icons_use_gtk_symbolic_classes() {
        let mut files = Vec::new();
        collect_svg_files(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/icons/scalable"),
            &mut files,
        );

        for file in files {
            let svg = std::fs::read_to_string(&file).expect("read svg icon");
            assert!(
                !svg.contains("<line ") && !svg.contains("<polyline "),
                "{} uses SVG primitives GTK symbolic icons ignore",
                file.display()
            );

            for primitive in ["<path", "<rect", "<circle"] {
                let mut offset = 0;
                while let Some(start) = svg[offset..].find(primitive) {
                    let start = offset + start;
                    let end = svg[start..]
                        .find('>')
                        .map(|end| start + end)
                        .expect("svg primitive has closing bracket");
                    let element = &svg[start..end];
                    assert!(
                        element.contains("class=\"foreground-fill\"")
                            || element.contains("class=\"foreground-stroke\""),
                        "{} has a {} without a GTK symbolic foreground class",
                        file.display(),
                        primitive
                    );
                    offset = end + 1;
                }
            }
        }
    }
}
