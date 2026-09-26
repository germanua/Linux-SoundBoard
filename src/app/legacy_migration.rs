use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::{ControlHotkeyAction, FolderTabBinding, Settings, Sound};
use crate::library_store::{
    HotkeyBindingOwner, HotkeyBindingRecord, LegacyGeneratedMembershipRecord,
    LegacyGeneratedTabRecord, LibraryBatch, LibraryError, LibraryStore, ManualMembershipRecord,
    ManualTabRecord, RootRecord, SoundRecord, MAX_BATCH_ROWS,
};

include!("legacy_migration/model.rs");
include!("legacy_migration/io.rs");
include!("legacy_migration/migration.rs");
include!("legacy_migration/tests.rs");
