use std::cell::{Cell, RefCell};
use std::sync::Once;

use gio::resources_register_include;
use gtk4::gdk::Display;
use gtk4::CssProvider;
use libadwaita::StyleManager;

use crate::config::Theme;

const ICON_RESOURCE_PATH: &str = "/com/linuxsoundboard/icons";
const DARK_CSS: &str = include_str!("../../themes/dark.css");
const LIGHT_CSS: &str = include_str!("../../themes/light.css");
static RESOURCE_INIT: Once = Once::new();
thread_local! {
    static CURRENT_CSS_PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
    static ICON_RESOURCE_PATH_ADDED: Cell<bool> = const { Cell::new(false) };
}

pub fn apply_theme(theme: Theme) {
    ensure_app_resources();

    let manager = StyleManager::default();
    match theme {
        Theme::Dark => manager.set_color_scheme(libadwaita::ColorScheme::ForceDark),
        Theme::Light => manager.set_color_scheme(libadwaita::ColorScheme::ForceLight),
    }

    let css = match theme {
        Theme::Dark => DARK_CSS,
        Theme::Light => LIGHT_CSS,
    };

    let provider = CssProvider::new();
    provider.load_from_data(css);
    if let Some(display) = Display::default() {
        CURRENT_CSS_PROVIDER.with(|current| {
            if let Some(old_provider) = current.borrow_mut().take() {
                gtk4::style_context_remove_provider_for_display(&display, &old_provider);
            }
            gtk4::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            *current.borrow_mut() = Some(provider);
        });
    }
}

pub(crate) fn ensure_app_resources() {
    RESOURCE_INIT.call_once(|| {
        resources_register_include!("compiled.gresource")
            .expect("Failed to register bundled GTK resources");
    });

    ICON_RESOURCE_PATH_ADDED.with(|added| {
        if added.get() {
            return;
        }
        if let Some(display) = Display::default() {
            let theme = gtk4::IconTheme::for_display(&display);
            theme.add_resource_path(ICON_RESOURCE_PATH);
            added.set(true);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{DARK_CSS, LIGHT_CSS};

    /// Keyframe selector lists ("0%, 100% {") only parse on GTK 4.20 and newer.
    fn keyframe_selector_lists(css: &str) -> Vec<&str> {
        let mut offenders = Vec::new();
        let mut depth = 0usize;

        for line in css.lines() {
            let trimmed = line.trim();
            if depth == 0 && !trimmed.starts_with("@keyframes") {
                continue;
            }
            if depth > 0 && trimmed.ends_with(',') {
                offenders.push(trimmed);
            }
            depth += line.matches('{').count();
            depth = depth.saturating_sub(line.matches('}').count());
        }

        offenders
    }

    #[test]
    fn stylesheets_keep_one_selector_per_keyframe() {
        for (name, css) in [("dark.css", DARK_CSS), ("light.css", LIGHT_CSS)] {
            let offenders = keyframe_selector_lists(css);
            assert!(
                offenders.is_empty(),
                "{name} uses keyframe selector lists that GTK before 4.20 rejects: {offenders:?}"
            );
        }
    }

    #[test]
    fn keyframe_selector_lists_are_detected() {
        let css = "@keyframes pulse {\n  0%,\n  100% {\n    opacity: 1;\n  }\n}\n";
        assert_eq!(keyframe_selector_lists(css), vec!["0%,"]);
    }
}
