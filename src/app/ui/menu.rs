use gtk4::prelude::*;

pub fn show_popover_menu(
    widget: &impl IsA<gtk4::Widget>,
    namespace: &str,
    menu: &gio::Menu,
    action_group: &gio::SimpleActionGroup,
    x: f64,
    y: f64,
) {
    let widget = widget.as_ref();
    widget.insert_action_group(namespace, None::<&gio::SimpleActionGroup>);
    widget.insert_action_group(namespace, Some(action_group));

    let popover = gtk4::PopoverMenu::from_model(Some(menu));
    popover.add_css_class("lsb-context-menu");
    popover.insert_action_group(namespace, Some(action_group));
    popover.set_parent(widget);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

    let widget_weak = widget.downgrade();
    let namespace = namespace.to_string();
    popover.connect_closed(move |popover| {
        // Let activation finish before unparenting.
        let popover = popover.clone();
        let widget_weak = widget_weak.clone();
        let namespace = namespace.clone();
        glib::idle_add_local_once(move || {
            if let Some(widget) = widget_weak.upgrade() {
                widget.insert_action_group(&namespace, None::<&gio::SimpleActionGroup>);
            }
            popover.insert_action_group(&namespace, None::<&gio::SimpleActionGroup>);
            popover.unparent();
        });
    });

    popover.popup();
}
