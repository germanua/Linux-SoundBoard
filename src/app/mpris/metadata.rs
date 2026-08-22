//! The `Metadata` map a media-control widget reads. Pure `glib::Variant`
//! building so unit tests can pin the shapes; D-Bus lives in the parent.

use glib::prelude::ToVariant;
use glib::variant::{DictEntry, Variant};

use crate::app_meta::{APP_ID, APP_TITLE};

/// What is playing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NowPlaying {
    /// The sound's public id, used to build a track path.
    pub id: String,
    /// The name shown in the transport bar.
    pub title: String,
    pub duration_ms: Option<u64>,
    pub paused: bool,
}

/// What `PlaybackStatus` should report.
pub(crate) fn playback_status(now: Option<&NowPlaying>) -> &'static str {
    match now {
        Some(now) if now.paused => "Paused",
        Some(_) => "Playing",
        None => "Stopped",
    }
}

pub(crate) fn track_id(sound_id: &str) -> glib::variant::ObjectPath {
    let mut path = String::with_capacity(sound_id.len() + 24);
    path.push_str("/com/linuxsoundboard/track/");
    if sound_id.is_empty() {
        path.push_str("none");
    }
    for character in sound_id.chars() {
        if character.is_ascii_alphanumeric() {
            path.push(character);
        } else {
            path.push('_');
        }
    }
    glib::variant::ObjectPath::try_from(path)
        .expect("every character is sanitised to an object-path character")
}

/// The `a{sv}` a host reads to draw the now-playing card. Empty when nothing is
/// playing — that's how a player says "no track" instead of leaving stale text.
pub(crate) fn build(now: Option<&NowPlaying>) -> Variant {
    let Some(now) = now else {
        return Variant::array_from_iter::<DictEntry<String, Variant>>(Vec::<Variant>::new());
    };

    let mut entries = vec![
        ("mpris:trackid", track_id(&now.id).to_variant()),
        ("xesam:title", now.title.to_variant()),
        (
            "xesam:artist",
            Variant::array_from_iter::<String>([APP_TITLE.to_variant()]),
        ),
        // Hosts that show a placeholder image use this; the installed app icon
        // is the only artwork a soundboard has.
        (
            "mpris:artUrl",
            format!("image://theme/{APP_ID}").to_variant(),
        ),
    ];
    if let Some(duration_ms) = now.duration_ms {
        // MPRIS counts in microseconds.
        entries.push((
            "mpris:length",
            ((duration_ms as i64).saturating_mul(1_000)).to_variant(),
        ));
    }

    Variant::array_from_iter::<DictEntry<String, Variant>>(
        entries
            .into_iter()
            .map(|(name, value)| DictEntry::new(name.to_string(), value).to_variant()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing() -> NowPlaying {
        NowPlaying {
            id: "3f2504e0-4f89-41d3-9a0c-0305e82c3301".to_string(),
            title: "airhorn.mp3".to_string(),
            duration_ms: Some(2_500),
            paused: false,
        }
    }

    #[test]
    fn the_metadata_matches_the_signature_hosts_expect() {
        assert_eq!(build(Some(&playing())).type_().as_str(), "a{sv}");
        assert_eq!(build(None).type_().as_str(), "a{sv}");
    }

    #[test]
    fn nothing_playing_reports_an_empty_map() {
        assert_eq!(build(None).n_children(), 0);
    }

    #[test]
    fn the_title_is_the_sound_name() {
        let rendered = build(Some(&playing())).print(false);
        assert!(rendered.contains("'xesam:title': <'airhorn.mp3'>"));
    }

    #[test]
    fn the_length_is_reported_in_microseconds() {
        let rendered = build(Some(&playing())).print(false);
        assert!(
            rendered.contains("'mpris:length': <int64 2500000>"),
            "{rendered}"
        );
    }

    #[test]
    fn a_sound_of_unknown_length_reports_no_length_at_all() {
        let unknown = NowPlaying {
            duration_ms: None,
            ..playing()
        };
        assert!(!build(Some(&unknown)).print(false).contains("mpris:length"));
    }

    /// A uuid's hyphens are not legal in a D-Bus object path, so a track id
    /// built straight from a sound id would fail to serialise.
    #[test]
    fn a_track_path_survives_a_uuid_sound_id() {
        assert_eq!(
            track_id("3f2504e0-4f89-41d3-9a0c-0305e82c3301").as_str(),
            "/com/linuxsoundboard/track/3f2504e0_4f89_41d3_9a0c_0305e82c3301"
        );
    }

    #[test]
    fn a_track_path_survives_an_id_that_is_not_a_uuid() {
        assert_eq!(
            track_id("sound/../weird id").as_str(),
            "/com/linuxsoundboard/track/sound____weird_id"
        );
        assert_eq!(track_id("").as_str(), "/com/linuxsoundboard/track/none");
    }

    #[test]
    fn playback_status_follows_what_is_playing() {
        assert_eq!(playback_status(None), "Stopped");
        assert_eq!(playback_status(Some(&playing())), "Playing");
        let paused = NowPlaying {
            paused: true,
            ..playing()
        };
        assert_eq!(playback_status(Some(&paused)), "Paused");
    }
}
