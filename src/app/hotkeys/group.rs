use crate::config::GroupMode;

use crate::library_store::HotkeyGroupMember as GroupMember;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HotkeyToggles {
    pub tab_hotkeys: bool,
    pub multi_sound: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InertReason {
    NoMembers,

    OutOfScope,

    Ambiguous,

    MultiSoundDisabled,
}

impl InertReason {
    pub(crate) const fn message(self) -> &'static str {
        match self {
            Self::NoMembers => "That shortcut is not assigned to a sound.",
            Self::OutOfScope => "That shortcut belongs to another tab.",
            Self::Ambiguous => {
                "That shortcut is assigned in more than one tab. Open one of them to use it."
            }
            Self::MultiSoundDisabled => {
                "Several sounds share that shortcut. Turn on \"Multiple sounds per hotkey\" in Settings to use it."
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    Play(usize),

    AllPlaying,
    Inert(InertReason),
}

pub(crate) fn select_from_group(
    members: &[GroupMember],
    active_scope: &str,
    toggles: HotkeyToggles,
    mode: GroupMode,
    last_played: Option<&str>,
    entropy: u64,
    currently_playing: &HashSet<String>,
) -> Selection {
    if members.is_empty() {
        return Selection::Inert(InertReason::NoMembers);
    }

    let candidates: Vec<usize> = if toggles.tab_hotkeys {
        members
            .iter()
            .enumerate()
            .filter(|(_, member)| {
                member.tab_scope.is_none() || member.tab_scope.as_deref() == Some(active_scope)
            })
            .map(|(index, _)| index)
            .collect()
    } else {
        (0..members.len()).collect()
    };

    let Some(&first) = candidates.first() else {
        return Selection::Inert(InertReason::OutOfScope);
    };

    let scope = members[first].tab_scope.as_deref();
    if candidates
        .iter()
        .any(|&index| members[index].tab_scope.as_deref() != scope)
    {
        return Selection::Inert(InertReason::Ambiguous);
    }

    if candidates.len() > 1 && !toggles.multi_sound {
        return Selection::Inert(InertReason::MultiSoundDisabled);
    }

    let is_playing = |index: usize| currently_playing.contains(members[index].sound_id.as_str());
    let eligible: Vec<usize> = candidates
        .iter()
        .copied()
        .filter(|&index| !is_playing(index))
        .collect();
    if eligible.is_empty() {
        return Selection::AllPlaying;
    }
    if eligible.len() == 1 {
        return Selection::Play(eligible[0]);
    }

    if mode == GroupMode::Random {
        return Selection::Play(eligible[(entropy % eligible.len() as u64) as usize]);
    }

    let previous =
        last_played.and_then(|id| candidates.iter().position(|&i| members[i].sound_id == id));
    let start = match mode {
        GroupMode::Same => previous.unwrap_or(0),
        GroupMode::Next => previous.map_or(0, |position| (position + 1) % candidates.len()),

        GroupMode::Random => 0,
    };
    let chosen = (0..candidates.len())
        .map(|step| candidates[(start + step) % candidates.len()])
        .find(|&index| !is_playing(index))
        .unwrap_or(eligible[0]);

    Selection::Play(chosen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(sound: &str, scope: Option<&str>) -> GroupMember {
        GroupMember {
            binding_id: format!("binding-{sound}"),
            sound_id: sound.to_string(),
            tab_scope: scope.map(str::to_string),
        }
    }

    const OFF: HotkeyToggles = HotkeyToggles {
        tab_hotkeys: false,
        multi_sound: false,
    };
    const TABS: HotkeyToggles = HotkeyToggles {
        tab_hotkeys: true,
        multi_sound: false,
    };
    const MULTI: HotkeyToggles = HotkeyToggles {
        tab_hotkeys: false,
        multi_sound: true,
    };
    const BOTH: HotkeyToggles = HotkeyToggles {
        tab_hotkeys: true,
        multi_sound: true,
    };

    fn select(
        members: &[GroupMember],
        scope: &str,
        toggles: HotkeyToggles,
        mode: GroupMode,
        last: Option<&str>,
    ) -> Selection {
        select_with_playing(members, scope, toggles, mode, last, &HashSet::new())
    }

    fn select_with_playing(
        members: &[GroupMember],
        scope: &str,
        toggles: HotkeyToggles,
        mode: GroupMode,
        last: Option<&str>,
        playing: &HashSet<String>,
    ) -> Selection {
        select_from_group(members, scope, toggles, mode, last, 0, playing)
    }

    fn playing(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn single_unscoped_binding_plays_with_every_toggle_off() {
        let members = [member("a", None)];
        assert_eq!(
            select(&members, "general", OFF, GroupMode::Same, None),
            Selection::Play(0)
        );
    }

    #[test]
    fn unscoped_binding_still_plays_in_general_with_tab_scoping_on() {
        let members = [member("a", None)];
        assert_eq!(
            select(&members, "general", TABS, GroupMode::Same, None),
            Selection::Play(0)
        );
    }

    #[test]
    fn empty_group_is_inert() {
        assert_eq!(
            select(&[], "general", BOTH, GroupMode::Same, None),
            Selection::Inert(InertReason::NoMembers)
        );
    }

    #[test]
    fn binding_from_another_tab_does_not_fire() {
        let members = [member("a", Some("tab-a"))];
        assert_eq!(
            select(&members, "tab-b", TABS, GroupMode::Same, None),
            Selection::Inert(InertReason::OutOfScope)
        );
    }

    #[test]
    fn binding_from_the_active_tab_fires() {
        let members = [member("a", Some("tab-a"))];
        assert_eq!(
            select(&members, "tab-a", TABS, GroupMode::Same, None),
            Selection::Play(0)
        );
    }

    #[test]
    fn chord_reused_across_tabs_is_inert_in_general() {
        let members = [member("a", Some("tab-a")), member("b", Some("tab-b"))];
        assert_eq!(
            select(&members, "general", TABS, GroupMode::Same, None),
            Selection::Inert(InertReason::OutOfScope)
        );
    }

    #[test]
    fn chord_reused_across_tabs_resolves_inside_one_of_them() {
        let members = [member("a", Some("tab-a")), member("b", Some("tab-b"))];
        assert_eq!(
            select(&members, "tab-b", TABS, GroupMode::Same, None),
            Selection::Play(1)
        );
    }

    #[test]
    fn unscoped_and_scoped_binding_on_one_chord_is_ambiguous() {
        let members = [member("a", None), member("b", Some("tab-a"))];
        assert_eq!(
            select(&members, "tab-a", TABS, GroupMode::Same, None),
            Selection::Inert(InertReason::Ambiguous)
        );
    }

    #[test]
    fn scopes_left_over_from_tab_scoping_stay_inert_once_it_is_off() {
        let members = [member("a", Some("tab-a")), member("b", Some("tab-b"))];
        assert_eq!(
            select(&members, "general", MULTI, GroupMode::Same, None),
            Selection::Inert(InertReason::Ambiguous)
        );
    }

    #[test]
    fn group_needs_the_multi_sound_toggle() {
        let members = [member("a", None), member("b", None)];
        assert_eq!(
            select(&members, "general", OFF, GroupMode::Same, None),
            Selection::Inert(InertReason::MultiSoundDisabled)
        );
    }

    #[test]
    fn same_replays_the_last_member() {
        let members = [member("a", None), member("b", None), member("c", None)];
        assert_eq!(
            select(&members, "general", MULTI, GroupMode::Same, Some("c")),
            Selection::Play(2)
        );
    }

    #[test]
    fn same_falls_back_to_the_first_member() {
        let members = [member("a", None), member("b", None)];
        assert_eq!(
            select(&members, "general", MULTI, GroupMode::Same, None),
            Selection::Play(0)
        );
    }

    #[test]
    fn same_falls_back_when_the_last_member_left_the_group() {
        let members = [member("a", None), member("b", None)];
        assert_eq!(
            select(&members, "general", MULTI, GroupMode::Same, Some("gone")),
            Selection::Play(0)
        );
    }

    #[test]
    fn next_starts_at_the_first_member() {
        let members = [member("a", None), member("b", None)];
        assert_eq!(
            select(&members, "general", MULTI, GroupMode::Next, None),
            Selection::Play(0)
        );
    }

    #[test]
    fn next_advances_one_member_per_press() {
        let members = [member("a", None), member("b", None), member("c", None)];
        assert_eq!(
            select(&members, "general", MULTI, GroupMode::Next, Some("a")),
            Selection::Play(1)
        );
    }

    #[test]
    fn next_wraps_at_the_end() {
        let members = [member("a", None), member("b", None)];
        assert_eq!(
            select(&members, "general", MULTI, GroupMode::Next, Some("b")),
            Selection::Play(0)
        );
    }

    #[test]
    fn random_picks_within_the_group() {
        let members = [member("a", None), member("b", None), member("c", None)];
        assert_eq!(
            select_from_group(
                &members,
                "general",
                MULTI,
                GroupMode::Random,
                None,
                7,
                &HashSet::new()
            ),
            Selection::Play(1)
        );
        assert_eq!(
            select_from_group(
                &members,
                "general",
                MULTI,
                GroupMode::Random,
                None,
                9,
                &HashSet::new()
            ),
            Selection::Play(0)
        );
    }

    #[test]
    fn group_mode_is_ignored_for_a_single_member() {
        let members = [member("a", None)];
        for mode in [GroupMode::Same, GroupMode::Next, GroupMode::Random] {
            assert_eq!(
                select(&members, "general", MULTI, mode, Some("a")),
                Selection::Play(0)
            );
        }
    }

    #[test]
    fn returned_index_addresses_the_original_slice() {
        let members = [
            member("other", Some("tab-a")),
            member("wanted", Some("tab-b")),
        ];
        assert_eq!(
            select(&members, "tab-b", TABS, GroupMode::Same, None),
            Selection::Play(1)
        );
    }

    #[test]
    fn a_single_member_that_is_playing_selects_nothing() {
        let members = [member("a", None)];
        for mode in [GroupMode::Same, GroupMode::Next, GroupMode::Random] {
            assert_eq!(
                select_with_playing(
                    &members,
                    "general",
                    MULTI,
                    mode,
                    Some("a"),
                    &playing(&["a"])
                ),
                Selection::AllPlaying,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_single_member_becomes_eligible_again_once_it_is_free() {
        let members = [member("a", None)];
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Same,
                Some("a"),
                &playing(&[])
            ),
            Selection::Play(0)
        );
    }

    #[test]
    fn a_playing_member_is_skipped_and_the_other_member_is_chosen() {
        let members = [member("a", None), member("b", None)];
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Same,
                Some("a"),
                &playing(&["a"])
            ),
            Selection::Play(1)
        );
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Next,
                Some("a"),
                &playing(&["b"])
            ),
            Selection::Play(0)
        );
    }

    #[test]
    fn every_member_playing_selects_nothing() {
        let members = [member("a", None), member("b", None), member("c", None)];
        for mode in [GroupMode::Same, GroupMode::Next, GroupMode::Random] {
            assert_eq!(
                select_with_playing(
                    &members,
                    "general",
                    MULTI,
                    mode,
                    Some("b"),
                    &playing(&["a", "b", "c"])
                ),
                Selection::AllPlaying,
                "{mode:?}"
            );
        }
    }

    #[test]
    fn a_playing_member_cannot_resurrect_a_chord_that_is_otherwise_inert() {
        let members = [member("a", None), member("b", None)];
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                OFF,
                GroupMode::Same,
                None,
                &playing(&["a"])
            ),
            Selection::Inert(InertReason::MultiSoundDisabled)
        );
        let scoped = [member("a", Some("tab-a"))];
        assert_eq!(
            select_with_playing(
                &scoped,
                "general",
                TABS,
                GroupMode::Same,
                None,
                &playing(&["a"])
            ),
            Selection::Inert(InertReason::OutOfScope)
        );
    }

    #[test]
    fn next_skips_playing_members_and_wraps_once() {
        let members = [member("a", None), member("b", None), member("c", None)];
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Next,
                Some("a"),
                &playing(&["b"])
            ),
            Selection::Play(2)
        );
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Next,
                Some("a"),
                &playing(&["b", "c"])
            ),
            Selection::Play(0)
        );
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Next,
                Some("a"),
                &playing(&[])
            ),
            Selection::Play(1)
        );
    }

    #[test]
    fn a_skipped_member_becomes_eligible_again_after_it_finishes() {
        let members = [member("a", None), member("b", None), member("c", None)];
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Next,
                Some("c"),
                &playing(&["a"])
            ),
            Selection::Play(1),
            "the walk skips the playing member"
        );
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Next,
                Some("c"),
                &playing(&[])
            ),
            Selection::Play(0),
            "once nothing is playing the plain rotation is back"
        );
    }

    #[test]
    fn random_never_picks_a_playing_member() {
        let members = [member("a", None), member("b", None), member("c", None)];
        let b_playing = playing(&["b"]);
        for entropy in 0..12u64 {
            let selection = select_from_group(
                &members,
                "general",
                MULTI,
                GroupMode::Random,
                None,
                entropy,
                &b_playing,
            );
            assert!(
                matches!(selection, Selection::Play(0) | Selection::Play(2)),
                "entropy {entropy} must not pick the playing member: {selection:?}"
            );
        }
        let only_c = playing(&["a", "b"]);
        for entropy in 0..6u64 {
            assert_eq!(
                select_from_group(
                    &members,
                    "general",
                    MULTI,
                    GroupMode::Random,
                    None,
                    entropy,
                    &only_c
                ),
                Selection::Play(2)
            );
        }
    }

    #[test]
    fn same_keeps_its_preference_but_never_picks_a_playing_member() {
        let members = [member("a", None), member("b", None), member("c", None)];
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Same,
                Some("c"),
                &playing(&[])
            ),
            Selection::Play(2)
        );
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Same,
                Some("c"),
                &playing(&["c"])
            ),
            Selection::Play(0),
            "wrapping to the first eligible member"
        );
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Same,
                Some("a"),
                &playing(&["a"])
            ),
            Selection::Play(1)
        );
    }

    #[test]
    fn a_shared_chord_alternates_when_only_one_voice_can_be_live() {
        let members = [member("a", None), member("b", None)];
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Same,
                Some("a"),
                &playing(&["a"])
            ),
            Selection::Play(1),
            "A sounds, so B is chosen"
        );
        assert_eq!(
            select_with_playing(
                &members,
                "general",
                MULTI,
                GroupMode::Same,
                Some("b"),
                &playing(&["b"])
            ),
            Selection::Play(0),
            "B sounds, so A is chosen"
        );
    }
}
