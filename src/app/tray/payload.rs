use glib::prelude::ToVariant;
use glib::variant::{DictEntry, Variant};

/// How a row is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ItemKind {
    /// A plain clickable row.
    Command,
    /// A horizontal rule. Carries no label.
    Separator,
    /// A row with a checkmark, ticked or not.
    Checkmark(bool),
}

/// One row of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuItem {
    /// Identifies the row in `Event` calls. Must be non-zero: the host reserves
    /// 0 for the root.
    pub id: i32,
    pub label: String,
    pub kind: ItemKind,
    pub enabled: bool,
}

impl MenuItem {
    pub(crate) fn command(id: i32, label: impl Into<String>) -> Self {
        Self {
            id,
            label: label.into(),
            kind: ItemKind::Command,
            enabled: true,
        }
    }

    pub(crate) fn checkmark(id: i32, label: impl Into<String>, checked: bool) -> Self {
        Self {
            id,
            label: label.into(),
            kind: ItemKind::Checkmark(checked),
            enabled: true,
        }
    }

    pub(crate) fn separator(id: i32) -> Self {
        Self {
            id,
            label: String::new(),
            kind: ItemKind::Separator,
            enabled: true,
        }
    }
}

fn properties_of(item: &MenuItem) -> Vec<(&'static str, Variant)> {
    match item.kind {
        ItemKind::Separator => vec![("type", "separator".to_variant())],
        ItemKind::Command => vec![
            ("label", item.label.to_variant()),
            ("enabled", item.enabled.to_variant()),
        ],
        ItemKind::Checkmark(checked) => vec![
            ("label", item.label.to_variant()),
            ("enabled", item.enabled.to_variant()),
            ("toggle-type", "checkmark".to_variant()),
            ("toggle-state", i32::from(checked).to_variant()),
        ],
    }
}

fn dict(pairs: impl IntoIterator<Item = (&'static str, Variant)>) -> Variant {
    Variant::array_from_iter::<DictEntry<String, Variant>>(
        pairs
            .into_iter()
            .map(|(name, value)| DictEntry::new(name.to_string(), value).to_variant()),
    )
}

pub(crate) fn item_properties(item: &MenuItem, filter: &[String]) -> Variant {
    dict(
        properties_of(item)
            .into_iter()
            .filter(|(name, _)| filter.is_empty() || filter.iter().any(|want| want == name)),
    )
}

/// The whole menu as the `(ia{sv}av)` tree `GetLayout` returns. The revision
/// that goes with it belongs to the service, not the shape.
pub(crate) fn layout(items: &[MenuItem], filter: &[String]) -> Variant {
    let children = items.iter().map(|item| {
        Variant::tuple_from_iter([
            item.id.to_variant(),
            item_properties(item, filter),
            empty_children(),
        ])
        .to_variant()
    });

    Variant::tuple_from_iter([
        0i32.to_variant(),
        dict([("children-display", "submenu".to_variant())]),
        Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, children),
    ])
}

/// Every row as the `a(ia{sv})` that `GetGroupProperties` returns.
pub(crate) fn group_properties(items: &[MenuItem], filter: &[String]) -> Variant {
    let rows = items.iter().map(|item| {
        Variant::tuple_from_iter([item.id.to_variant(), item_properties(item, filter)])
    });
    Variant::array_from_iter_with_type(
        glib::VariantTy::new("(ia{sv})").expect("literal type string is valid"),
        rows,
    )
}

fn empty_children() -> Variant {
    Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, Vec::<Variant>::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<String> {
        Vec::new()
    }

    fn menu() -> Vec<MenuItem> {
        vec![
            MenuItem::command(1, "Show Linux Soundboard"),
            MenuItem::separator(2),
            MenuItem::command(3, "Stop All Sounds"),
            MenuItem::checkmark(4, "Mute Microphone", true),
        ]
    }

    #[test]
    fn a_command_row_carries_its_label_and_stays_enabled() {
        let props = item_properties(&MenuItem::command(1, "Stop All Sounds"), &all());
        assert_eq!(
            props.print(false),
            "{'label': <'Stop All Sounds'>, 'enabled': <true>}"
        );
    }

    #[test]
    fn a_separator_carries_only_its_type() {
        let props = item_properties(&MenuItem::separator(2), &all());
        assert_eq!(props.print(false), "{'type': <'separator'>}");
    }

    #[test]
    fn a_checkmark_row_reports_which_way_it_is_set() {
        let ticked = item_properties(&MenuItem::checkmark(4, "Mute", true), &all());
        assert!(ticked.print(false).contains("'toggle-state': <1>"));
        assert!(ticked.print(false).contains("'toggle-type': <'checkmark'>"));

        let clear = item_properties(&MenuItem::checkmark(4, "Mute", false), &all());
        assert!(clear.print(false).contains("'toggle-state': <0>"));
    }

    /// A value in `a{sv}` is boxed exactly once. Boxing twice yields
    /// `<<'x'>>`, which a host is not obliged to understand.
    #[test]
    fn a_property_value_is_boxed_exactly_once() {
        let props = item_properties(&MenuItem::command(1, "Show"), &all());
        assert!(props.print(false).contains("<'Show'>"));
        assert!(!props.print(false).contains("<<'Show'>>"));
    }

    #[test]
    fn an_empty_filter_asks_for_every_property() {
        let props = item_properties(&MenuItem::checkmark(4, "Mute", false), &all());
        assert_eq!(props.n_children(), 4);
    }

    #[test]
    fn a_filter_keeps_only_the_properties_it_names() {
        let filter = vec!["label".to_string()];
        let props = item_properties(&MenuItem::checkmark(4, "Mute", false), &filter);
        assert_eq!(props.print(false), "{'label': <'Mute'>}");
    }

    #[test]
    fn a_filter_naming_nothing_the_row_has_yields_an_empty_map() {
        let filter = vec!["icon-name".to_string()];
        let props = item_properties(&MenuItem::command(1, "Show"), &filter);
        assert_eq!(props.n_children(), 0);
    }

    #[test]
    fn the_layout_matches_the_signature_the_spec_requires() {
        let layout = layout(&menu(), &all());
        assert_eq!(layout.type_().as_str(), "(ia{sv}av)");
    }

    #[test]
    fn the_layout_root_declares_itself_a_submenu() {
        let layout = layout(&menu(), &all());
        assert_eq!(
            layout.child_value(1).print(false),
            "{'children-display': <'submenu'>}"
        );
        assert_eq!(layout.child_value(0).get::<i32>(), Some(0));
    }

    #[test]
    fn every_row_becomes_a_child_of_the_root() {
        let layout = layout(&menu(), &all());
        assert_eq!(layout.child_value(2).n_children(), menu().len());
    }

    #[test]
    fn an_empty_menu_still_produces_a_usable_root() {
        let layout = layout(&[], &all());
        assert_eq!(layout.type_().as_str(), "(ia{sv}av)");
        assert_eq!(layout.child_value(2).n_children(), 0);
    }

    #[test]
    fn a_row_in_the_layout_has_no_children_of_its_own() {
        let layout = layout(&menu(), &all());
        let first = layout
            .child_value(2)
            .child_value(0)
            .as_variant()
            .expect("children are boxed variants");
        assert_eq!(first.child_value(0).get::<i32>(), Some(1));
        assert_eq!(first.child_value(2).n_children(), 0);
    }

    #[test]
    fn group_properties_matches_the_signature_the_spec_requires() {
        let props = group_properties(&menu(), &all());
        assert_eq!(props.type_().as_str(), "a(ia{sv})");
        assert_eq!(props.n_children(), menu().len());
    }

    #[test]
    fn group_properties_reports_each_row_against_its_own_id() {
        let props = group_properties(&menu(), &["label".to_string()]);
        let third = props.child_value(2);
        assert_eq!(third.child_value(0).get::<i32>(), Some(3));
        assert_eq!(
            third.child_value(1).print(false),
            "{'label': <'Stop All Sounds'>}"
        );
    }
}
