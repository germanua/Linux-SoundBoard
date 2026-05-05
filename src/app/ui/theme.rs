use std::cell::RefCell;
use std::sync::Once;

use gio::resources_register_include;
use gtk4::gdk::Display;
use gtk4::CssProvider;
use libadwaita::StyleManager;

use crate::config::Theme;

const ICON_RESOURCE_PATH: &str = "/com/linuxsoundboard/icons";
static RESOURCE_INIT: Once = Once::new();
thread_local! {
    static CURRENT_CSS_PROVIDER: RefCell<Option<CssProvider>> = const { RefCell::new(None) };
}

pub fn apply_theme(theme: Theme) {
    ensure_app_resources();

    let manager = StyleManager::default();
    match theme {
        Theme::Dark => manager.set_color_scheme(libadwaita::ColorScheme::ForceDark),
        Theme::Light => manager.set_color_scheme(libadwaita::ColorScheme::ForceLight),
    }

    let css = match theme {
        Theme::Dark => include_str!("../../themes/dark.css"),
        Theme::Light => include_str!("../../themes/light.css"),
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

    if let Some(display) = Display::default() {
        let theme = gtk4::IconTheme::for_display(&display);
        theme.add_resource_path(ICON_RESOURCE_PATH);
    }
}
