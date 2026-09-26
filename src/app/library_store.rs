use std::collections::{HashMap, VecDeque};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};

use crate::config::{ControlHotkeyAction, LoudnessAnalysisState, Sound};

pub const PAGE_SIZE: usize = 256;
pub const MAX_BATCH_ROWS: usize = 512;
pub(crate) const DATABASE_SCHEMA_VERSION: i64 = 6;

pub(crate) const DATABASE_SCHEMA_FLAVOR: &str = "bounded-generation-v6";
pub(crate) const DATABASE_APPLICATION_ID: i64 = 0x4c53_4244;
const CONTROL_QUEUE_CAPACITY: usize = 16;
const VISIBLE_QUEUE_CAPACITY: usize = 64;
const MAINTENANCE_QUEUE_CAPACITY: usize = 2;

include!("library_store/model.rs");
include!("library_store/api.rs");
include!("library_store/worker.rs");
include!("library_store/schema.rs");
include!("library_store/mutations.rs");
include!("library_store/queries.rs");
include!("library_store/tests.rs");
