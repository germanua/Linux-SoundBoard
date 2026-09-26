use parking_lot::Mutex;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use glib::BoxedAnyObject;
use gtk4::prelude::*;
use gtk4::{
    Box as GtkBox, GestureClick, Image, Label, ListBox, ListBoxRow, ListView, Orientation,
    ScrolledWindow, SelectionMode, SignalListItemFactory, SingleSelection, TreeExpander,
    TreeListModel, TreeListRow, Widget,
};

use crate::app_meta::GENERAL_TAB_ID;
use crate::app_state::AppState;
use crate::commands;

use super::dialogs::DialogHost;
use super::icons;
use super::is_unmodified_delete_shortcut;
use super::menu;
use super::tab_dnd;

include!("tabs_sidebar/model.rs");
include!("tabs_sidebar/api.rs");
include!("tabs_sidebar/inner.rs");
include!("tabs_sidebar/tests.rs");
