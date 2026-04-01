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
    widget.insert_action_group(namespace, Some(action_group));

    let popover = gtk4::PopoverMenu::from_model(Some(menu));
    popover.insert_action_group(namespace, Some(action_group));
    popover.set_parent(widget);
    popover.set_has_arrow(false);
    popover.set_pointing_to(Some(&gtk4::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));

    popover.connect_closed(move |popover| {
        // Let activation finish before unparenting.
        let popover = popover.clone();
        glib::idle_add_local_once(move || popover.unparent());
    });

    popover.popup();
}
