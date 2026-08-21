//! Publish what is playing to the desktop's media controls.
//!
//! This is the card the panel shows for a music player: sound name, transport
//! buttons, and on KDE a place on the lock screen. It is MPRIS2, a different
//! protocol from the tray, but served the same way — straight over GIO's GDBus,
//! with no extra dependency.
//!
//! A soundboard is not a music player, and a permanently registered one would
//! replace Spotify in the panel for the sake of a two-second airhorn and take
//! the media keys with it. So the bus name is held **only while a sound is
//! playing** and released the moment it stops. That, plus the setting being off
//! by default, is what keeps the feature from being a nuisance.

mod metadata;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use glib::prelude::ToVariant;
use glib::variant::{DictEntry, Variant};

pub(crate) use metadata::NowPlaying;

use crate::app_meta::{APP_BINARY, APP_ID, APP_TITLE};

const OBJECT_PATH: &str = "/org/mpris/MediaPlayer2";
const ROOT_INTERFACE: &str = "org.mpris.MediaPlayer2";
const PLAYER_INTERFACE: &str = "org.mpris.MediaPlayer2.Player";
const PROPERTIES_INTERFACE: &str = "org.freedesktop.DBus.Properties";

/// Something a media-control widget asked us to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MprisCommand {
    PlayPause,
    Stop,
    Next,
    Previous,
    /// Bring the window to the front. Every desktop offers this, so it is a
    /// second way back besides the tray icon.
    Raise,
    Quit,
}

/// The method a host called, or `None` for one we do not act on.
fn command_for(method: &str) -> Option<MprisCommand> {
    match method {
        // Play and Pause both toggle: the engine has one control, and a host
        // only offers the button that matches the status we reported.
        "PlayPause" | "Play" | "Pause" => Some(MprisCommand::PlayPause),
        "Stop" => Some(MprisCommand::Stop),
        "Next" => Some(MprisCommand::Next),
        "Previous" => Some(MprisCommand::Previous),
        "Raise" => Some(MprisCommand::Raise),
        "Quit" => Some(MprisCommand::Quit),
        _ => None,
    }
}

const INTERFACES_XML: &str = r#"
<node>
 <interface name="org.mpris.MediaPlayer2">
  <method name="Raise"/>
  <method name="Quit"/>
  <property name="CanQuit" type="b" access="read"/>
  <property name="CanRaise" type="b" access="read"/>
  <property name="HasTrackList" type="b" access="read"/>
  <property name="Identity" type="s" access="read"/>
  <property name="DesktopEntry" type="s" access="read"/>
  <property name="SupportedUriSchemes" type="as" access="read"/>
  <property name="SupportedMimeTypes" type="as" access="read"/>
 </interface>
 <interface name="org.mpris.MediaPlayer2.Player">
  <method name="Next"/>
  <method name="Previous"/>
  <method name="Pause"/>
  <method name="PlayPause"/>
  <method name="Stop"/>
  <method name="Play"/>
  <method name="Seek"><arg name="Offset" type="x" direction="in"/></method>
  <method name="SetPosition">
   <arg name="TrackId" type="o" direction="in"/>
   <arg name="Position" type="x" direction="in"/>
  </method>
  <method name="OpenUri"><arg name="Uri" type="s" direction="in"/></method>
  <property name="PlaybackStatus" type="s" access="read"/>
  <property name="LoopStatus" type="s" access="read"/>
  <property name="Rate" type="d" access="read"/>
  <property name="Shuffle" type="b" access="read"/>
  <property name="Metadata" type="a{sv}" access="read"/>
  <property name="Volume" type="d" access="read"/>
  <property name="Position" type="x" access="read"/>
  <property name="MinimumRate" type="d" access="read"/>
  <property name="MaximumRate" type="d" access="read"/>
  <property name="CanGoNext" type="b" access="read"/>
  <property name="CanGoPrevious" type="b" access="read"/>
  <property name="CanPlay" type="b" access="read"/>
  <property name="CanPause" type="b" access="read"/>
  <property name="CanSeek" type="b" access="read"/>
  <property name="CanControl" type="b" access="read"/>
  <signal name="Seeked"><arg name="Position" type="x"/></signal>
 </interface>
</node>"#;

/// A media-controls publisher. Exported for the process's lifetime; visible to
/// the desktop only while a sound is playing.
pub(crate) struct MprisService {
    connection: gio::DBusConnection,
    registrations: RefCell<Vec<gio::RegistrationId>>,
    owner: Cell<Option<gio::OwnerId>>,
    now: Rc<RefCell<Option<NowPlaying>>>,
}

impl MprisService {
    pub(crate) fn start(
        connection: &gio::DBusConnection,
        handler: impl Fn(MprisCommand) + 'static,
    ) -> Result<Self, glib::Error> {
        let node = gio::DBusNodeInfo::for_xml(INTERFACES_XML)?;
        let root_info = node
            .lookup_interface(ROOT_INTERFACE)
            .expect("the root interface is in the literal XML above");
        let player_info = node
            .lookup_interface(PLAYER_INTERFACE)
            .expect("the player interface is in the literal XML above");

        let now: Rc<RefCell<Option<NowPlaying>>> = Rc::new(RefCell::new(None));
        let handler: Rc<dyn Fn(MprisCommand)> = Rc::new(handler);

        let registrations = vec![
            register_root(connection, &root_info, &handler)?,
            register_player(connection, &player_info, &handler, &now)?,
        ];

        Ok(Self {
            connection: connection.clone(),
            registrations: RefCell::new(registrations),
            owner: Cell::new(None),
            now,
        })
    }

    /// Announce a sound, or clear the card when playback stops.
    ///
    /// Claims the bus name on the first sound and releases it when there is
    /// nothing playing, so the media keys go back to whatever the user was
    /// actually listening to.
    pub(crate) fn set_now_playing(&self, now: Option<NowPlaying>) {
        if *self.now.borrow() == now {
            return;
        }
        let stopping = now.is_none();
        *self.now.borrow_mut() = now;

        if stopping {
            self.release();
            return;
        }

        let owner = self.owner.take();
        if owner.is_none() {
            self.owner.set(Some(gio::bus_own_name_on_connection(
                &self.connection,
                &format!("org.mpris.MediaPlayer2.{APP_BINARY}"),
                gio::BusNameOwnerFlags::NONE,
                |_, name| log::debug!("Media controls: acquired {name}"),
                |_, name| log::warn!("Media controls: lost {name}"),
            )));
        } else {
            self.owner.set(owner);
        }
        self.announce();
    }

    /// Withdraw from the desktop's media controls.
    pub(crate) fn shutdown(&self) {
        self.release();
        for registration in self.registrations.borrow_mut().drain(..) {
            if let Err(error) = self.connection.unregister_object(registration) {
                log::warn!("Media controls: could not unexport an object: {error}");
            }
        }
    }

    fn release(&self) {
        if let Some(owner) = self.owner.take() {
            gio::bus_unown_name(owner);
            log::debug!("Media controls: released the player name");
        }
    }

    /// GDBus does not emit `PropertiesChanged` for objects registered by hand,
    /// so a player has to do it itself. `Position` is excluded by the spec —
    /// hosts poll it.
    fn announce(&self) {
        let now = self.now.borrow();
        let changed = [
            (
                "PlaybackStatus",
                metadata::playback_status(now.as_ref()).to_variant(),
            ),
            ("Metadata", metadata::build(now.as_ref())),
        ];
        let body = Variant::tuple_from_iter([
            PLAYER_INTERFACE.to_variant(),
            Variant::array_from_iter::<DictEntry<String, Variant>>(
                changed
                    .into_iter()
                    .map(|(name, value)| DictEntry::new(name.to_string(), value).to_variant()),
            ),
            Variant::array_from_iter::<String>(Vec::<Variant>::new()),
        ]);
        if let Err(error) = self.connection.emit_signal(
            None,
            OBJECT_PATH,
            PROPERTIES_INTERFACE,
            "PropertiesChanged",
            Some(&body),
        ) {
            log::warn!("Media controls: could not announce the current sound: {error}");
        }
    }
}

fn register_root(
    connection: &gio::DBusConnection,
    info: &gio::DBusInterfaceInfo,
    handler: &Rc<dyn Fn(MprisCommand)>,
) -> Result<gio::RegistrationId, glib::Error> {
    let handler = Rc::clone(handler);
    connection
        .register_object(OBJECT_PATH, info)
        .method_call(move |_, _, _, _, method, _, invocation| {
            if let Some(command) = command_for(method) {
                handler(command);
            }
            invocation.return_result(Ok(None));
        })
        .property(|_, _, _, _, property| match property {
            "CanQuit" | "CanRaise" => true.to_variant(),
            "HasTrackList" => false.to_variant(),
            "Identity" => APP_TITLE.to_variant(),
            "DesktopEntry" => APP_ID.to_variant(),
            "SupportedUriSchemes" | "SupportedMimeTypes" => {
                Variant::array_from_iter::<String>(Vec::<Variant>::new())
            }
            _ => String::new().to_variant(),
        })
        .build()
}

fn register_player(
    connection: &gio::DBusConnection,
    info: &gio::DBusInterfaceInfo,
    handler: &Rc<dyn Fn(MprisCommand)>,
    now: &Rc<RefCell<Option<NowPlaying>>>,
) -> Result<gio::RegistrationId, glib::Error> {
    let handler = Rc::clone(handler);
    let now = Rc::clone(now);
    connection
        .register_object(OBJECT_PATH, info)
        .method_call(move |_, _, _, _, method, _, invocation| {
            if let Some(command) = command_for(method) {
                handler(command);
            }
            invocation.return_result(Ok(None));
        })
        .property(move |_, _, _, _, property| {
            let now = now.borrow();
            match property {
                "PlaybackStatus" => metadata::playback_status(now.as_ref()).to_variant(),
                "Metadata" => metadata::build(now.as_ref()),
                "LoopStatus" => "None".to_variant(),
                "Rate" | "MinimumRate" | "MaximumRate" => 1.0f64.to_variant(),
                "Volume" => 1.0f64.to_variant(),
                "Shuffle" => false.to_variant(),
                // The engine reports a position but seeking a sound effect from
                // a panel widget is not something to promise, so it stays at
                // zero and CanSeek stays false.
                "Position" => 0i64.to_variant(),
                "CanGoNext" | "CanGoPrevious" | "CanPlay" | "CanPause" | "CanControl" => {
                    true.to_variant()
                }
                "CanSeek" => false.to_variant(),
                _ => String::new().to_variant(),
            }
        })
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transport_methods_map_to_commands() {
        assert_eq!(command_for("Stop"), Some(MprisCommand::Stop));
        assert_eq!(command_for("Next"), Some(MprisCommand::Next));
        assert_eq!(command_for("Previous"), Some(MprisCommand::Previous));
        assert_eq!(command_for("Raise"), Some(MprisCommand::Raise));
        assert_eq!(command_for("Quit"), Some(MprisCommand::Quit));
    }

    /// A host shows Play or Pause depending on the status we reported, and both
    /// have to reach the same one control the engine offers.
    #[test]
    fn play_and_pause_both_toggle() {
        assert_eq!(command_for("Play"), Some(MprisCommand::PlayPause));
        assert_eq!(command_for("Pause"), Some(MprisCommand::PlayPause));
        assert_eq!(command_for("PlayPause"), Some(MprisCommand::PlayPause));
    }

    /// Publishes a real player on the session bus and reads it back the way a
    /// panel does. Ignored by default because it needs a session bus; run it on
    /// a desktop with
    /// `cargo test -- --ignored publishes_a_real_player --nocapture`.
    #[test]
    #[ignore = "needs a session bus"]
    fn publishes_a_real_player() {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .expect("a session bus");
        let service = MprisService::start(&connection, |_| {}).expect("the objects export");

        service.set_now_playing(Some(NowPlaying {
            id: "3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string(),
            title: "airhorn.mp3".to_string(),
            duration_ms: Some(2_500),
            paused: false,
        }));

        let reply = Rc::new(RefCell::new(None));
        let main_loop = glib::MainLoop::new(None, false);
        connection.call(
            Some(&format!("org.mpris.MediaPlayer2.{APP_BINARY}")),
            OBJECT_PATH,
            PROPERTIES_INTERFACE,
            "GetAll",
            Some(&Variant::tuple_from_iter([PLAYER_INTERFACE.to_variant()])),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            {
                let reply = Rc::clone(&reply);
                let main_loop = main_loop.clone();
                move |result| {
                    *reply.borrow_mut() = Some(result);
                    main_loop.quit();
                }
            },
        );
        main_loop.run();

        let reply = reply.borrow_mut().take().expect("a reply arrived");
        service.shutdown();

        let rendered = reply
            .expect("GetAll answered without an error")
            .print(false);
        assert!(
            rendered.contains("'PlaybackStatus': <'Playing'>"),
            "{rendered}"
        );
        assert!(
            rendered.contains("'xesam:title': <'airhorn.mp3'>"),
            "{rendered}"
        );
    }

    #[test]
    fn a_method_we_do_not_implement_is_ignored_rather_than_guessed_at() {
        assert_eq!(command_for("Seek"), None);
        assert_eq!(command_for("SetPosition"), None);
        assert_eq!(command_for("OpenUri"), None);
    }
}
