use ashpd::WindowIdentifier;
use glib::translate::ToGlibPtr;
use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::*;

pub struct PortalParentWindow {
    identifier: WindowIdentifier,
}

impl PortalParentWindow {
    pub fn into_identifier(self) -> WindowIdentifier {
        self.identifier
    }
}

pub fn request_portal_parent_window<F>(window: &gtk4::Window, on_ready: F)
where
    F: FnOnce(Result<PortalParentWindow, String>) + 'static,
{
    let Some(surface) = window.surface() else {
        on_ready(Err(
            "The application window has not been realized yet, so the desktop shortcut dialog cannot be opened."
                .to_string(),
        ));
        return;
    };

    match surface.display().backend() {
        gtk4::gdk::Backend::Wayland => {
            let Ok(surface) = surface.downcast::<gdk4_wayland::WaylandSurface>() else {
                on_ready(Err(
                    "Failed to access the Wayland toplevel for the desktop shortcut dialog."
                        .to_string(),
                ));
                return;
            };
            let Ok(display) = surface.display().downcast::<gdk4_wayland::WaylandDisplay>() else {
                on_ready(Err(
                    "Failed to access the Wayland display for the desktop shortcut dialog."
                        .to_string(),
                ));
                return;
            };

            glib::MainContext::default().spawn_local(async move {
                // SAFETY: `surface` and `display` are moved into this async block and
                // outlive the `.await`; the raw wl_surface/wl_display pointers returned
                // by the gdk4-wayland FFI getters are valid for the lifetime of those
                // GDK objects, which satisfies WindowIdentifier::from_wayland_raw.
                let identifier = unsafe {
                    WindowIdentifier::from_wayland_raw(
                        gdk4_wayland::ffi::gdk_wayland_surface_get_wl_surface(
                            surface.to_glib_none().0,
                        ) as *mut _,
                        gdk4_wayland::ffi::gdk_wayland_display_get_wl_display(
                            display.to_glib_none().0,
                        ) as *mut _,
                    )
                    .await
                };

                match identifier {
                    Some(identifier) => on_ready(Ok(PortalParentWindow { identifier })),
                    None => on_ready(Err(
                        "Failed to export the Wayland application window for the desktop shortcut dialog. The compositor may not support xdg-foreign."
                            .to_string(),
                    )),
                }
            });
        }
        gtk4::gdk::Backend::X11 => {
            let Some(surface) = surface.downcast_ref::<gdk4_x11::X11Surface>() else {
                on_ready(Err(
                    "Failed to access the X11 toplevel for the desktop shortcut dialog."
                        .to_string(),
                ));
                return;
            };

            on_ready(Ok(PortalParentWindow {
                identifier: WindowIdentifier::from_xid(surface.xid()),
            }));
        }
        backend => on_ready(Err(format!(
            "The desktop shortcut dialog is unsupported for the current GTK backend: {backend:?}"
        ))),
    }
}
