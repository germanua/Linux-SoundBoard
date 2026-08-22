//! MPRIS2 player for desktop media controls.

mod metadata;

use std::cell::RefCell;
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
    Raise,
    Quit,
}

/// The method a host called, or `None` for one we do not act on.
fn command_for(method: &str) -> Option<MprisCommand> {
    match method {
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

struct PlayerName {
    connection: gio::DBusConnection,
    owner: RefCell<Option<gio::OwnerId>>,
}

impl PlayerName {
    fn claim(&self) {
        if self.owner.borrow().is_some() {
            return;
        }
        *self.owner.borrow_mut() = Some(gio::bus_own_name_on_connection(
            &self.connection,
            &format!("org.mpris.MediaPlayer2.{APP_BINARY}"),
            gio::BusNameOwnerFlags::NONE,
            |_, name| log::debug!("Media controls: acquired {name}"),
            |_, name| log::warn!("Media controls: lost {name}"),
        ));
    }

    fn release(&self) {
        if let Some(owner) = self.owner.borrow_mut().take() {
            gio::bus_unown_name(owner);
            log::debug!("Media controls: released the player name");
        }
    }

    fn is_held(&self) -> bool {
        self.owner.borrow().is_some()
    }
}

pub(crate) struct MprisService {
    connection: gio::DBusConnection,
    registrations: RefCell<Vec<gio::RegistrationId>>,
    name: PlayerName,
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
            name: PlayerName {
                connection: connection.clone(),
                owner: RefCell::new(None),
            },
            now,
        })
    }

    pub(crate) fn set_enabled(&self, enabled: bool) {
        if enabled == self.name.is_held() {
            return;
        }
        if enabled {
            self.name.claim();
            self.announce();
        } else {
            self.name.release();
        }
    }

    /// Announce the sound that started, or that playback stopped.
    pub(crate) fn set_now_playing(&self, now: Option<NowPlaying>) {
        if *self.now.borrow() == now {
            return;
        }
        *self.now.borrow_mut() = now;
        self.announce();
    }

    /// Withdraw from the desktop's media controls.
    pub(crate) fn shutdown(&self) {
        self.name.release();
        for registration in self.registrations.borrow_mut().drain(..) {
            if let Err(error) = self.connection.unregister_object(registration) {
                log::warn!("Media controls: could not unexport an object: {error}");
            }
        }
    }

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
    use std::cell::Cell;

    #[test]
    fn the_transport_methods_map_to_commands() {
        assert_eq!(command_for("Stop"), Some(MprisCommand::Stop));
        assert_eq!(command_for("Next"), Some(MprisCommand::Next));
        assert_eq!(command_for("Previous"), Some(MprisCommand::Previous));
        assert_eq!(command_for("Raise"), Some(MprisCommand::Raise));
        assert_eq!(command_for("Quit"), Some(MprisCommand::Quit));
    }

    #[test]
    fn play_and_pause_both_toggle() {
        assert_eq!(command_for("Play"), Some(MprisCommand::PlayPause));
        assert_eq!(command_for("Pause"), Some(MprisCommand::PlayPause));
        assert_eq!(command_for("PlayPause"), Some(MprisCommand::PlayPause));
    }

    #[test]
    #[ignore = "needs a session bus"]
    fn publishes_a_real_player() {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .expect("a session bus");
        let service = MprisService::start(&connection, |_| {}).expect("the objects export");
        service.set_enabled(true);

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

        // Panels only discover the well-known bus name.
        let owned_while_playing = name_has_owner(&connection);
        service.set_enabled(false);
        settle();
        let owned_when_stopped = name_has_owner(&connection);

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
        assert!(
            owned_while_playing,
            "no player name was claimed while a sound was playing"
        );
        assert!(
            !owned_when_stopped,
            "switching the feature off must release the name and hand the media keys back"
        );
    }

    /// Let the main loop turn, so queued D-Bus traffic actually goes out.
    fn settle() {
        let main_loop = glib::MainLoop::new(None, false);
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), {
            let main_loop = main_loop.clone();
            move || main_loop.quit()
        });
        main_loop.run();
    }

    fn name_has_owner(connection: &gio::DBusConnection) -> bool {
        let answer = Rc::new(Cell::new(false));
        let main_loop = glib::MainLoop::new(None, false);
        connection.call(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&Variant::tuple_from_iter([format!(
                "org.mpris.MediaPlayer2.{APP_BINARY}"
            )
            .to_variant()])),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
            {
                let answer = Rc::clone(&answer);
                let main_loop = main_loop.clone();
                move |result| {
                    if let Ok(reply) = result {
                        answer.set(
                            reply
                                .try_child_get::<bool>(0)
                                .ok()
                                .flatten()
                                .unwrap_or(false),
                        );
                    }
                    main_loop.quit();
                }
            },
        );
        main_loop.run();
        answer.get()
    }

    #[test]
    #[ignore = "needs a session bus"]
    fn the_panel_controls_survive_a_second_sound() {
        let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)
            .expect("a session bus");
        let service = MprisService::start(&connection, |_| {}).expect("the objects export");

        let sound = |name: &str| {
            Some(NowPlaying {
                id: name.to_string(),
                title: name.to_string(),
                duration_ms: Some(2_000),
                paused: false,
            })
        };

        service.set_enabled(true);
        settle();
        let before_anything_played = name_has_owner(&connection);

        service.set_now_playing(sound("first.mp3"));
        settle();
        let during_first = name_has_owner(&connection);

        // The first sound ends, and a moment later the user starts another.
        service.set_now_playing(None);
        settle();
        let between_sounds = name_has_owner(&connection);

        service.set_now_playing(sound("second.mp3"));
        settle();
        let during_second = name_has_owner(&connection);

        service.set_now_playing(None);
        settle();
        let after_everything = name_has_owner(&connection);

        service.set_enabled(false);
        settle();
        let once_switched_off = name_has_owner(&connection);

        service.shutdown();

        assert!(
            before_anything_played,
            "the controls must be in the panel before the first sound, or there is nothing to press"
        );
        assert!(during_first, "the controls vanished during the first sound");
        assert!(
            between_sounds,
            "the controls vanished when a sound ended; a soundboard clip is over in a second"
        );
        assert!(
            during_second,
            "the controls did not come back for the second sound, which is the reported fault"
        );
        assert!(
            after_everything,
            "the controls vanished once playback stopped"
        );
        assert!(
            !once_switched_off,
            "switching the setting off must hand the media keys back"
        );
    }

    #[test]
    fn a_method_we_do_not_implement_is_ignored_rather_than_guessed_at() {
        assert_eq!(command_for("Seek"), None);
        assert_eq!(command_for("SetPosition"), None);
        assert_eq!(command_for("OpenUri"), None);
    }
}
