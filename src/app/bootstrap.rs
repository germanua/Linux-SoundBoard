use parking_lot::Mutex;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::{mpsc, Arc, OnceLock};
use std::thread;
use std::time::Duration;

use gtk4::gdk::prelude::DisplayExtManual;
use gtk4::prelude::*;
use gtk4::{Application, Window};
use libadwaita as adw;
use libadwaita::prelude::*;
use log::{info, warn};
use nix::fcntl::{Flock, FlockArg};

use crate::app_meta::{
    APP_BINARY, APP_ICON_NAME, APP_ID, APP_TITLE, APP_VERSION, BACKEND_ENV_VAR,
    ENGINE_SERVICE_NAME, ENGINE_TARGET_NAME, FALLBACK_RENDERER, FORCE_X11_ENV_VAR,
    RENDERER_ENV_VAR, WAYLAND_BACKEND, X11_BACKEND,
};
use crate::app_state::AppState;
use crate::config::Config;
use crate::timer_registry::TimerRegistry;

const ENGINE_SERVICE_UNIT: &str = ENGINE_SERVICE_NAME;
const ENGINE_TARGET_UNIT: &str = ENGINE_TARGET_NAME;
static STORAGE_LOCK: OnceLock<Mutex<Option<Flock<File>>>> = OnceLock::new();

include!("bootstrap/startup.rs");
include!("bootstrap/application.rs");
include!("bootstrap/engine.rs");
include!("bootstrap/tests.rs");
