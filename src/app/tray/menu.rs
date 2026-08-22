use super::payload::MenuItem;
use crate::config::ControlHotkeyAction;

/// What clicking a row should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuAction {
    /// Show the window if it is hidden, hide it if it is showing.
    ToggleWindow,
    /// One of the actions a control hotkey can already trigger.
    Control(ControlHotkeyAction),
    /// Shut the application down for real.
    Quit,
}

const SHOW_HIDE: i32 = 1;
const PLAY_PAUSE: i32 = 3;
const STOP_ALL: i32 = 4;
const MUTE_REAL_MIC: i32 = 5;
const QUIT: i32 = 7;

pub(crate) fn build(window_visible: bool, real_mic_muted: bool) -> Vec<MenuItem> {
    vec![
        MenuItem::command(
            SHOW_HIDE,
            if window_visible {
                "Hide Linux Soundboard"
            } else {
                "Show Linux Soundboard"
            },
        ),
        MenuItem::separator(2),
        MenuItem::command(PLAY_PAUSE, "Play / Pause"),
        MenuItem::command(STOP_ALL, "Stop All"),
        MenuItem::checkmark(MUTE_REAL_MIC, "Mute Real Mic", real_mic_muted),
        MenuItem::separator(6),
        MenuItem::command(QUIT, "Quit"),
    ]
}

pub(crate) fn action_for(id: i32) -> Option<MenuAction> {
    match id {
        SHOW_HIDE => Some(MenuAction::ToggleWindow),
        PLAY_PAUSE => Some(MenuAction::Control(ControlHotkeyAction::PlayPause)),
        STOP_ALL => Some(MenuAction::Control(ControlHotkeyAction::StopAll)),
        MUTE_REAL_MIC => Some(MenuAction::Control(ControlHotkeyAction::MuteRealMic)),
        QUIT => Some(MenuAction::Quit),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tray::payload::ItemKind;

    #[test]
    fn the_first_row_offers_to_hide_a_window_that_is_showing() {
        let menu = build(true, false);
        assert_eq!(menu[0].label, "Hide Linux Soundboard");
    }

    #[test]
    fn the_first_row_offers_to_show_a_window_that_is_hidden() {
        let menu = build(false, false);
        assert_eq!(menu[0].label, "Show Linux Soundboard");
    }

    #[test]
    fn the_microphone_row_is_ticked_only_while_the_mic_is_muted() {
        let muted = build(true, true);
        let live = build(true, false);
        let row = |menu: &[MenuItem]| {
            menu.iter()
                .find(|item| item.id == MUTE_REAL_MIC)
                .expect("the menu has a microphone row")
                .kind
                .clone()
        };
        assert_eq!(row(&muted), ItemKind::Checkmark(true));
        assert_eq!(row(&live), ItemKind::Checkmark(false));
    }

    #[test]
    fn every_row_that_is_not_a_separator_leads_somewhere() {
        for item in build(true, false) {
            let handled = action_for(item.id).is_some();
            assert_eq!(
                handled,
                item.kind != ItemKind::Separator,
                "row {} ({:?}) is inconsistent: separators must have no action, \
                 and every other row must have one",
                item.id,
                item.label
            );
        }
    }

    #[test]
    fn a_row_id_we_do_not_use_is_ignored_rather_than_guessed_at() {
        assert_eq!(action_for(0), None);
        assert_eq!(action_for(99), None);
        assert_eq!(action_for(-1), None);
    }

    #[test]
    fn the_control_rows_reuse_the_actions_the_hotkeys_already_dispatch() {
        assert_eq!(
            action_for(PLAY_PAUSE),
            Some(MenuAction::Control(ControlHotkeyAction::PlayPause))
        );
        assert_eq!(
            action_for(STOP_ALL),
            Some(MenuAction::Control(ControlHotkeyAction::StopAll))
        );
        assert_eq!(
            action_for(MUTE_REAL_MIC),
            Some(MenuAction::Control(ControlHotkeyAction::MuteRealMic))
        );
    }
}
