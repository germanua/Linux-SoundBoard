//! The StatusNotifierItem and its dbusmenu, served over GIO's GDBus.
//!
//! GTK4 has no tray API — `GtkStatusIcon` was removed — so a panel icon means
//! speaking StatusNotifierItem over D-Bus. The two obvious crates do not fit:
//! `tray-icon` wraps libappindicator and would pull GTK 3 into a GTK 4
//! process, and `ksni` brings a Tokio runtime into a codebase that has no
//! async runtime at all. `gio` is already a dependency and serves both
//! interfaces directly, with every callback arriving on the GTK main thread —
//! which is where the menu actions have to run anyway.
//!
//! `ItemIsMenu` is left false: left-click toggles the window and right-click
//! opens the menu, the way Steam and Discord behave. `ContextMenu` is
//! deliberately unimplemented — it asks the application to draw a menu at
//! absolute screen coordinates, which GTK4 cannot do under Wayland.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glib::prelude::ToVariant;
use glib::variant::Variant;

use super::payload::{self, MenuItem};
// The icon that packaging actually installs into the icon theme is named after
// the application id, not `APP_ICON_NAME`; a tray host resolves the name in a
// separate process, so it has to be the installed one.
use crate::app_meta::{APP_ID, APP_TITLE};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
const MENU_PATH: &str = "/MenuBar";

/// Something the user did to the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrayAction {
    /// The icon was left-clicked.
    Activate,
    /// A menu row was clicked, identified by its `MenuItem::id`.
    MenuItem(i32),
}

const INTERFACES_XML: &str = r#"
<node>
 <interface name="org.kde.StatusNotifierItem">
  <property name="Category" type="s" access="read"/>
  <property name="Id" type="s" access="read"/>
  <property name="Title" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="WindowId" type="i" access="read"/>
  <property name="IconName" type="s" access="read"/>
  <property name="IconPixmap" type="a(iiay)" access="read"/>
  <property name="OverlayIconName" type="s" access="read"/>
  <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
  <property name="AttentionIconName" type="s" access="read"/>
  <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
  <property name="AttentionMovieName" type="s" access="read"/>
  <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
  <property name="IconThemePath" type="s" access="read"/>
  <property name="Menu" type="o" access="read"/>
  <property name="ItemIsMenu" type="b" access="read"/>
  <method name="Activate">
   <arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/>
  </method>
  <method name="SecondaryActivate">
   <arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/>
  </method>
  <method name="Scroll">
   <arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/>
  </method>
  <signal name="NewTitle"/>
  <signal name="NewIcon"/>
  <signal name="NewAttentionIcon"/>
  <signal name="NewOverlayIcon"/>
  <signal name="NewToolTip"/>
  <signal name="NewStatus"><arg name="status" type="s"/></signal>
 </interface>
 <interface name="com.canonical.dbusmenu">
  <property name="Version" type="u" access="read"/>
  <property name="TextDirection" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconThemePath" type="as" access="read"/>
  <method name="GetLayout">
   <arg name="parentId" type="i" direction="in"/>
   <arg name="recursionDepth" type="i" direction="in"/>
   <arg name="propertyNames" type="as" direction="in"/>
   <arg name="revision" type="u" direction="out"/>
   <arg name="layout" type="(ia{sv}av)" direction="out"/>
  </method>
  <method name="GetGroupProperties">
   <arg name="ids" type="ai" direction="in"/>
   <arg name="propertyNames" type="as" direction="in"/>
   <arg name="properties" type="a(ia{sv})" direction="out"/>
  </method>
  <method name="GetProperty">
   <arg name="id" type="i" direction="in"/>
   <arg name="name" type="s" direction="in"/>
   <arg name="value" type="v" direction="out"/>
  </method>
  <method name="Event">
   <arg name="id" type="i" direction="in"/>
   <arg name="eventId" type="s" direction="in"/>
   <arg name="data" type="v" direction="in"/>
   <arg name="timestamp" type="u" direction="in"/>
  </method>
  <method name="EventGroup">
   <arg name="events" type="a(isvu)" direction="in"/>
   <arg name="idErrors" type="ai" direction="out"/>
  </method>
  <method name="AboutToShow">
   <arg name="id" type="i" direction="in"/>
   <arg name="needUpdate" type="b" direction="out"/>
  </method>
  <method name="AboutToShowGroup">
   <arg name="ids" type="ai" direction="in"/>
   <arg name="updatesNeeded" type="ai" direction="out"/>
   <arg name="idErrors" type="ai" direction="out"/>
  </method>
  <signal name="ItemsPropertiesUpdated">
   <arg name="updatedProps" type="a(ia{sv})"/>
   <arg name="removedProps" type="a(ias)"/>
  </signal>
  <signal name="LayoutUpdated">
   <arg name="revision" type="u"/><arg name="parent" type="i"/>
  </signal>
  <signal name="ItemActivationRequested">
   <arg name="id" type="i"/><arg name="timestamp" type="u"/>
  </signal>
 </interface>
</node>"#;

struct TrayState {
    items: Vec<MenuItem>,
    /// Bumped whenever the menu changes; hosts use it to spot a stale layout.
    revision: u32,
    tooltip: String,
}

/// A live tray icon.
///
/// Dropping this does not remove the icon — none of the ids GIO hands back
/// release anything on drop — so the owning code calls [`Self::shutdown`]. They
/// are held behind `Cell`s because releasing consumes them while the service
/// itself is shared.
pub(crate) struct TrayService {
    connection: gio::DBusConnection,
    bus_name: String,
    owner: Cell<Option<gio::OwnerId>>,
    watcher: Cell<Option<gio::SignalSubscriptionId>>,
    registrations: RefCell<Vec<gio::RegistrationId>>,
    state: Rc<RefCell<TrayState>>,
    /// True once a watcher has appeared and accepted our registration. Until
    /// then there is no icon on screen, whatever we have exported.
    registered: Rc<Cell<bool>>,
}

impl TrayService {
    /// Export the item and ask the watcher to show it.
    ///
    /// Returns `Err` only when the objects cannot be exported. A missing
    /// watcher is not an error: several desktops have none until an extension
    /// or panel starts, so the item waits and registers when one appears.
    pub(crate) fn start(
        connection: &gio::DBusConnection,
        items: Vec<MenuItem>,
        handler: impl Fn(TrayAction) + 'static,
    ) -> Result<Self, glib::Error> {
        let node = gio::DBusNodeInfo::for_xml(INTERFACES_XML)?;
        let item_info = node
            .lookup_interface(ITEM_INTERFACE)
            .expect("the item interface is in the literal XML above");
        let menu_info = node
            .lookup_interface(MENU_INTERFACE)
            .expect("the menu interface is in the literal XML above");

        let state = Rc::new(RefCell::new(TrayState {
            items,
            revision: 1,
            tooltip: String::new(),
        }));
        let handler: Rc<dyn Fn(TrayAction)> = Rc::new(handler);

        let registrations = vec![
            register_item(connection, &item_info, &state, &handler)?,
            register_menu(connection, &menu_info, &state, &handler)?,
        ];

        let bus_name = item_bus_name(std::process::id());
        let owner = gio::bus_own_name_on_connection(
            connection,
            &bus_name,
            gio::BusNameOwnerFlags::NONE,
            |_, name| log::debug!("Tray: acquired {name}"),
            |_, name| log::warn!("Tray: lost {name}"),
        );

        // A watcher can arrive late or restart under us — a panel crash, or a
        // GNOME extension being toggled — so follow the name rather than
        // registering once at startup and hoping.
        //
        // `gio::bus_watch_name_on_connection` would be the obvious tool, but in
        // gio 0.20 it returns a `WatcherId` from a private module that a second
        // export of the same name shadows, leaving the type unnameable outside
        // the crate and the watch impossible to store and cancel. Following
        // NameOwnerChanged ourselves costs a few lines and hands back a
        // `SignalSubscriptionId` we can actually unsubscribe.
        let registered = Rc::new(Cell::new(false));
        let watcher = connection.signal_subscribe(
            Some("org.freedesktop.DBus"),
            Some("org.freedesktop.DBus"),
            Some("NameOwnerChanged"),
            Some("/org/freedesktop/DBus"),
            Some(WATCHER_NAME),
            gio::DBusSignalFlags::NONE,
            {
                let bus_name = bus_name.clone();
                let registered = Rc::clone(&registered);
                move |conn, _, _, _, _, params| {
                    if watcher_appeared(params) {
                        register_with_watcher(conn, &bus_name, &registered);
                    } else {
                        log::info!("Tray: the status notifier watcher went away");
                        registered.set(false);
                    }
                }
            },
        );

        // The watcher is usually already running by the time we start, in which
        // case no NameOwnerChanged is coming and this first attempt is the one
        // that counts. It failing is not an error: several desktops have no
        // watcher until a panel or extension provides one.
        register_with_watcher(connection, &bus_name, &registered);

        Ok(Self {
            connection: connection.clone(),
            bus_name,
            owner: Cell::new(Some(owner)),
            watcher: Cell::new(Some(watcher)),
            registrations: RefCell::new(registrations),
            state,
            registered,
        })
    }

    /// Whether a panel is actually showing the icon. The close button must not
    /// hide the window to a tray that is not there.
    pub(crate) fn is_live(&self) -> bool {
        self.registered.get()
    }

    /// Replace the menu. Hosts re-read the layout when the revision moves.
    pub(crate) fn set_menu(&self, items: Vec<MenuItem>) {
        {
            let mut state = self.state.borrow_mut();
            if state.items == items {
                return;
            }
            state.items = items;
            state.revision = state.revision.wrapping_add(1);
        }
        let revision = self.state.borrow().revision;
        self.emit(
            MENU_PATH,
            MENU_INTERFACE,
            "LayoutUpdated",
            Some(Variant::tuple_from_iter([
                revision.to_variant(),
                0i32.to_variant(),
            ])),
        );
    }

    /// Set the text shown when the pointer rests on the icon.
    ///
    /// This is where the playing sound belongs: hovering the icon is the first
    /// thing anyone tries, and unlike the media controls it works on every
    /// desktop that can show a tray icon at all.
    pub(crate) fn set_tooltip(&self, description: &str) {
        if self.state.borrow().tooltip == description {
            return;
        }
        self.state.borrow_mut().tooltip = description.to_string();
        self.emit(ITEM_PATH, ITEM_INTERFACE, "NewToolTip", None);
    }

    /// Remove the icon and release the bus name. Doing nothing on a second
    /// call, so quitting by more than one route stays safe.
    pub(crate) fn shutdown(&self) {
        if let Some(watcher) = self.watcher.take() {
            self.connection.signal_unsubscribe(watcher);
        }
        if let Some(owner) = self.owner.take() {
            gio::bus_unown_name(owner);
        }
        for registration in self.registrations.borrow_mut().drain(..) {
            if let Err(error) = self.connection.unregister_object(registration) {
                log::warn!("Tray: could not unexport an object: {error}");
            }
        }
        log::debug!("Tray: released {}", self.bus_name);
    }

    fn emit(&self, path: &str, interface: &str, signal: &str, body: Option<Variant>) {
        if let Err(error) =
            self.connection
                .emit_signal(None, path, interface, signal, body.as_ref())
        {
            log::warn!("Tray: could not emit {signal}: {error}");
        }
    }
}

/// Whether a `NameOwnerChanged` body reports the watcher arriving rather than
/// leaving. The body is `(name, old_owner, new_owner)`; an empty new owner
/// means the name was released.
fn watcher_appeared(params: &Variant) -> bool {
    params
        .try_child_get::<String>(2)
        .ok()
        .flatten()
        .is_some_and(|new_owner| !new_owner.is_empty())
}

/// The bus name convention every watcher expects.
fn item_bus_name(pid: u32) -> String {
    format!("org.kde.StatusNotifierItem-{pid}-1")
}

fn register_with_watcher(conn: &gio::DBusConnection, bus_name: &str, registered: &Rc<Cell<bool>>) {
    let registered = Rc::clone(registered);
    conn.call(
        Some(WATCHER_NAME),
        WATCHER_PATH,
        WATCHER_NAME,
        "RegisterStatusNotifierItem",
        Some(&Variant::tuple_from_iter([bus_name.to_variant()])),
        None,
        gio::DBusCallFlags::NONE,
        3000,
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(_) => {
                log::info!("Tray: registered with the status notifier watcher");
                registered.set(true);
            }
            Err(error) => {
                log::warn!("Tray: the watcher refused our icon: {error}");
                registered.set(false);
            }
        },
    );
}

fn register_item(
    connection: &gio::DBusConnection,
    info: &gio::DBusInterfaceInfo,
    state: &Rc<RefCell<TrayState>>,
    handler: &Rc<dyn Fn(TrayAction)>,
) -> Result<gio::RegistrationId, glib::Error> {
    let handler = Rc::clone(handler);
    let state = Rc::clone(state);
    connection
        .register_object(ITEM_PATH, info)
        .method_call(move |_, _, _, _, method, _, invocation| {
            // Scroll and SecondaryActivate are answered but ignored: a
            // soundboard has nothing sensible to do with a scroll wheel, and
            // returning an error for them makes some hosts log noise.
            if method == "Activate" {
                handler(TrayAction::Activate);
            }
            invocation.return_result(Ok(None));
        })
        .property(move |_, _, _, _, property| match property {
            "Category" => "ApplicationStatus".to_variant(),
            "Id" => APP_ID.to_variant(),
            "Title" => APP_TITLE.to_variant(),
            "Status" => "Active".to_variant(),
            "WindowId" => 0i32.to_variant(),
            "IconName" => APP_ID.to_variant(),
            "ToolTip" => Variant::tuple_from_iter([
                String::new().to_variant(),
                empty_pixmaps(),
                APP_TITLE.to_variant(),
                state.borrow().tooltip.to_variant(),
            ]),
            "Menu" => glib::variant::ObjectPath::try_from(MENU_PATH)
                .expect("the literal menu path is a valid object path")
                .to_variant(),
            // False, so a left-click reaches Activate and toggles the window
            // instead of opening the menu.
            "ItemIsMenu" => false.to_variant(),
            "IconPixmap" | "OverlayIconPixmap" | "AttentionIconPixmap" => empty_pixmaps(),
            _ => String::new().to_variant(),
        })
        .build()
}

fn register_menu(
    connection: &gio::DBusConnection,
    info: &gio::DBusInterfaceInfo,
    state: &Rc<RefCell<TrayState>>,
    handler: &Rc<dyn Fn(TrayAction)>,
) -> Result<gio::RegistrationId, glib::Error> {
    let handler = Rc::clone(handler);
    let call_state = Rc::clone(state);
    let property_state = Rc::clone(state);
    connection
        .register_object(MENU_PATH, info)
        .method_call(move |_, _, _, _, method, params, invocation| {
            let state = call_state.borrow();
            let result = match method {
                "GetLayout" => {
                    let filter = string_list(&params, 2);
                    Ok(Some(Variant::tuple_from_iter([
                        state.revision.to_variant(),
                        payload::layout(&state.items, &filter),
                    ])))
                }
                "GetGroupProperties" => {
                    let wanted = int_list(&params, 0);
                    let filter = string_list(&params, 1);
                    let items: Vec<MenuItem> = if wanted.is_empty() {
                        state.items.clone()
                    } else {
                        state
                            .items
                            .iter()
                            .filter(|item| wanted.contains(&item.id))
                            .cloned()
                            .collect()
                    };
                    Ok(Some(Variant::tuple_from_iter([payload::group_properties(
                        &items, &filter,
                    )])))
                }
                "GetProperty" => {
                    let id = params.try_child_get::<i32>(0).ok().flatten().unwrap_or(0);
                    let name = params
                        .try_child_get::<String>(1)
                        .ok()
                        .flatten()
                        .unwrap_or_default();
                    match state.items.iter().find(|item| item.id == id) {
                        Some(item) => {
                            Ok(Some(Variant::tuple_from_iter([payload::item_properties(
                                item,
                                std::slice::from_ref(&name),
                            )])))
                        }
                        None => Err(unknown_id(id)),
                    }
                }
                // Qt returns false here and announces changes by signal
                // instead; libdbusmenu ignores the return value either way.
                "AboutToShow" => Ok(Some(Variant::tuple_from_iter([false.to_variant()]))),
                "AboutToShowGroup" => Ok(Some(Variant::tuple_from_iter([
                    empty_int_list(),
                    empty_int_list(),
                ]))),
                "EventGroup" => Ok(Some(Variant::tuple_from_iter([empty_int_list()]))),
                "Event" => {
                    if let Some(id) = clicked_id(&params) {
                        // The handler may rebuild the menu, which borrows the
                        // state this closure is holding.
                        drop(state);
                        handler(TrayAction::MenuItem(id));
                    }
                    Ok(None)
                }
                other => Err(glib::Error::new(
                    gio::IOErrorEnum::NotSupported,
                    &format!("{MENU_INTERFACE}.{other} is not implemented"),
                )),
            };
            invocation.return_result(result);
        })
        .property(move |_, _, _, _, property| match property {
            "Version" => 3u32.to_variant(),
            "TextDirection" => "ltr".to_variant(),
            "Status" => "normal".to_variant(),
            "IconThemePath" => Variant::array_from_iter::<String>(Vec::<Variant>::new()),
            _ => {
                let _ = &property_state;
                String::new().to_variant()
            }
        })
        .build()
}

/// The id of a row the user clicked, or `None` for any other event.
///
/// Hosts also send `hovered`, `opened` and `closed` through this method; only
/// `clicked` should fire an action.
fn clicked_id(params: &Variant) -> Option<i32> {
    let event = params.try_child_get::<String>(1).ok().flatten()?;
    if event != "clicked" {
        return None;
    }
    params.try_child_get::<i32>(0).ok().flatten()
}

fn string_list(params: &Variant, index: usize) -> Vec<String> {
    params
        .try_child_get::<Vec<String>>(index)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn int_list(params: &Variant, index: usize) -> Vec<i32> {
    params
        .try_child_get::<Vec<i32>>(index)
        .ok()
        .flatten()
        .unwrap_or_default()
}

fn empty_int_list() -> Variant {
    Variant::array_from_iter::<i32>(Vec::<Variant>::new())
}

fn empty_pixmaps() -> Variant {
    Variant::array_from_iter_with_type(
        glib::VariantTy::new("(iiay)").expect("literal type string is valid"),
        Vec::<Variant>::new(),
    )
}

fn unknown_id(id: i32) -> glib::Error {
    glib::Error::new(
        gio::IOErrorEnum::InvalidArgument,
        &format!("no menu item with id {id}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(id: i32, name: &str) -> Variant {
        Variant::tuple_from_iter([
            id.to_variant(),
            name.to_variant(),
            0i32.to_variant().to_variant(),
            0u32.to_variant(),
        ])
    }

    #[test]
    fn the_bus_name_follows_the_convention_watchers_expect() {
        assert_eq!(
            item_bus_name(4321),
            "org.kde.StatusNotifierItem-4321-1".to_string()
        );
    }

    #[test]
    fn a_click_reports_the_row_it_landed_on() {
        assert_eq!(clicked_id(&event(7, "clicked")), Some(7));
    }

    /// Hosts send hover and open events down the same method. Acting on them
    /// would fire a menu item merely by moving the pointer over it.
    #[test]
    fn only_a_click_counts_as_a_click() {
        assert_eq!(clicked_id(&event(7, "hovered")), None);
        assert_eq!(clicked_id(&event(7, "opened")), None);
        assert_eq!(clicked_id(&event(7, "closed")), None);
    }

    #[test]
    fn a_malformed_event_is_ignored_rather_than_panicking() {
        assert_eq!(
            clicked_id(&Variant::tuple_from_iter([1i32.to_variant()])),
            None
        );
        assert_eq!(clicked_id(&"not a tuple".to_variant()), None);
    }

    fn name_owner_changed(old: &str, new: &str) -> Variant {
        Variant::tuple_from_iter([
            WATCHER_NAME.to_variant(),
            old.to_variant(),
            new.to_variant(),
        ])
    }

    #[test]
    fn a_watcher_taking_the_name_counts_as_arriving() {
        assert!(watcher_appeared(&name_owner_changed("", ":1.42")));
        // A panel restart hands the name straight from one owner to the next.
        assert!(watcher_appeared(&name_owner_changed(":1.41", ":1.42")));
    }

    #[test]
    fn a_watcher_releasing_the_name_counts_as_leaving() {
        assert!(!watcher_appeared(&name_owner_changed(":1.42", "")));
    }

    /// Exports a real item on the session bus and checks that the desktop's
    /// watcher takes it. Ignored by default because it needs a session bus with
    /// a running watcher, which CI does not have; run it on a desktop with
    /// `cargo test -- --ignored registers_with_a_real_watcher --nocapture`.
    #[test]
    #[ignore = "needs a session bus with a StatusNotifierWatcher"]
    fn registers_with_a_real_watcher() {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .expect("a session bus");
        let tray = TrayService::start(&connection, super::super::menu::build(true, false), |_| {})
            .expect("the objects export");

        // Registration is asynchronous, so let the main loop turn until the
        // reply lands or a second goes by.
        let main_loop = glib::MainLoop::new(None, false);
        glib::timeout_add_seconds_local(1, {
            let main_loop = main_loop.clone();
            move || {
                main_loop.quit();
                glib::ControlFlow::Break
            }
        });
        main_loop.run();

        let registered = tray.is_live();
        tray.set_tooltip("Playing: airhorn.mp3");

        // Ask our own dbusmenu for its layout the way a panel does. The call
        // has to be asynchronous: a blocking one would stop the main context
        // that has to dispatch it, and deadlock.
        let layout = Rc::new(RefCell::new(None));
        let main_loop = glib::MainLoop::new(None, false);
        connection.call(
            Some(&item_bus_name(std::process::id())),
            MENU_PATH,
            MENU_INTERFACE,
            "GetLayout",
            Some(&Variant::tuple_from_iter([
                0i32.to_variant(),
                (-1i32).to_variant(),
                Variant::array_from_iter::<String>(Vec::<Variant>::new()),
            ])),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            {
                let layout = Rc::clone(&layout);
                let main_loop = main_loop.clone();
                move |result| {
                    *layout.borrow_mut() = Some(result);
                    main_loop.quit();
                }
            },
        );
        main_loop.run();

        let layout = layout.borrow_mut().take().expect("a reply arrived");

        // And read the tooltip back the way a panel does when the pointer
        // rests on the icon.
        let tooltip = Rc::new(RefCell::new(None));
        let main_loop = glib::MainLoop::new(None, false);
        connection.call(
            Some(&item_bus_name(std::process::id())),
            ITEM_PATH,
            "org.freedesktop.DBus.Properties",
            "Get",
            Some(&Variant::tuple_from_iter([
                ITEM_INTERFACE.to_variant(),
                "ToolTip".to_variant(),
            ])),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            {
                let tooltip = Rc::clone(&tooltip);
                let main_loop = main_loop.clone();
                move |result| {
                    *tooltip.borrow_mut() = Some(result);
                    main_loop.quit();
                }
            },
        );
        main_loop.run();

        let tooltip = tooltip.borrow_mut().take().expect("a reply arrived");
        tray.shutdown();

        assert!(
            registered,
            "the watcher did not accept the item; is a panel running?"
        );
        assert_eq!(
            tooltip.expect("the tooltip was readable").print(false),
            "(<('', @a(iiay) [], 'Linux Soundboard', 'Playing: airhorn.mp3')>,)",
            "hovering the icon has to show what is playing"
        );

        let layout = layout.expect("GetLayout answered without an error");
        let rendered = layout.print(false);
        assert!(
            rendered.contains("Hide Linux Soundboard") && rendered.contains("Quit"),
            "the menu a host would see is missing rows: {rendered}"
        );
    }

    #[test]
    fn a_missing_property_list_reads_as_asking_for_everything() {
        let params = Variant::tuple_from_iter([0i32.to_variant(), (-1i32).to_variant()]);
        assert!(string_list(&params, 2).is_empty());
    }
}
