use crate::config::GroupMode;
// The store's row type is the input to these rules; a second copy of the same
// three fields would only need converting back and forth.
use crate::library_store::HotkeyGroupMember as GroupMember;

/// The two independent Settings toggles that govern resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct HotkeyToggles {
    pub tab_hotkeys: bool,
    pub multi_sound: bool,
}

/// Why a press produced no sound. Each variant maps to a distinct user-facing
/// explanation, so "nothing happened" is never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InertReason {
    /// The chord has no active bindings at all.
    NoMembers,
    /// Every binding for this chord belongs to some other tab.
    OutOfScope,
    /// The chord resolves to bindings from more than one scope, so there is no
    /// single right answer. Deliberately does nothing rather than guessing.
    Ambiguous,
    /// Several sounds share the chord but "Multiple sounds per hotkey" is off.
    MultiSoundDisabled,
}

impl InertReason {
    /// What the user is told when a press does nothing. Silence with no
    /// explanation reads as a broken hotkey.
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

/// The index into the `members` slice that was passed in, or the reason none
/// was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Selection {
    Play(usize),
    Inert(InertReason),
}

pub(crate) fn select_from_group(
    members: &[GroupMember],
    active_scope: &str,
    toggles: HotkeyToggles,
    mode: GroupMode,
    last_played: Option<&str>,
    entropy: u64,
) -> Selection {
    if members.is_empty() {
        return Selection::Inert(InertReason::NoMembers);
    }

    // Indices into `members`, so the answer addresses the caller's slice.
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

    if candidates.len() == 1 {
        return Selection::Play(first);
    }
    if !toggles.multi_sound {
        return Selection::Inert(InertReason::MultiSoundDisabled);
    }

    let previous =
        last_played.and_then(|id| candidates.iter().position(|&i| members[i].sound_id == id));
    let position = match mode {
        GroupMode::Same => previous.unwrap_or(0),
        GroupMode::Next => previous.map_or(0, |p| (p + 1) % candidates.len()),
        GroupMode::Random => (entropy % candidates.len() as u64) as usize,
    };

    Selection::Play(candidates[position])
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
        select_from_group(members, scope, toggles, mode, last, 0)
    }

    // Today's behavior must survive unchanged

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

    // Toggle A: tab scoping

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

    // Toggle B: several sounds on one chord

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
            select_from_group(&members, "general", MULTI, GroupMode::Random, None, 7),
            Selection::Play(1)
        );
        assert_eq!(
            select_from_group(&members, "general", MULTI, GroupMode::Random, None, 9),
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

    // Indices refer to the caller's slice, not a filtered copy

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
}
