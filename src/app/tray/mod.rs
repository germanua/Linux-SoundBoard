//! System tray integration: a StatusNotifierItem and its dbusmenu.

pub(crate) mod menu;
pub(crate) mod payload;
mod service;

pub(crate) use menu::MenuAction;
pub(crate) use payload::MenuItem;
pub(crate) use service::{TrayAction, TrayService};
