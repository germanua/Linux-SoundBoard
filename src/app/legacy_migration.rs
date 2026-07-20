use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use rusqlite::Connection;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::config::{ControlHotkeyAction, FolderTabBinding, Settings, Sound};
use crate::library_store::{
    HotkeyBindingOwner, HotkeyBindingRecord, LegacyGeneratedMembershipRecord,
    LegacyGeneratedTabRecord, LibraryBatch, LibraryError, LibraryStore, ManualMembershipRecord,
    ManualTabRecord, RootRecord, SoundRecord, MAX_BATCH_ROWS,
};

#[derive(Debug, thiserror::Error)]
pub enum LegacyMigrationError {
    #[error("migration I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("legacy config parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("legacy config upgrade error: {0}")]
    ConfigMigration(#[from] crate::config::migration::MigrationError),
    #[error("library import error: {0}")]
    Library(#[from] LibraryError),
    #[error("migration database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("invalid legacy migration input: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyMigrationReport {
    pub sounds: usize,
    pub roots: usize,
    pub manual_tabs: usize,
    pub manual_memberships: usize,
    pub generated_tabs_deferred: usize,
    pub hotkeys: usize,
    pub source_sha256: String,
    pub library_id: String,
}

#[derive(Deserialize)]
struct TabMetadata {
    id: String,
    name: String,
    sound_ids: IgnoredAny,
    order: u32,
    #[serde(default)]
    folder_binding: Option<FolderTabBinding>,
}

#[derive(Clone)]
enum ImportedTab {
    Manual(String),
    Generated(String),
}

struct ImportState {
    store: LibraryStore,
    schema_version: Option<u32>,
    settings: Option<serde_json::Value>,
    sounds: Vec<SoundRecord>,
    hotkeys: Vec<HotkeyBindingRecord>,
    tabs: Vec<ManualTabRecord>,
    generated_tabs: Vec<LegacyGeneratedTabRecord>,
    tab_targets: Vec<ImportedTab>,
    memberships: Vec<ManualMembershipRecord>,
    generated_memberships: Vec<LegacyGeneratedMembershipRecord>,
    sound_count: usize,
    root_count: usize,
    manual_tab_count: usize,
    membership_count: usize,
    generated_tab_count: usize,
    hotkey_count: usize,
}

impl ImportState {
    fn new(store: LibraryStore) -> Self {
        Self {
            store,
            schema_version: None,
            settings: None,
            sounds: Vec::with_capacity(MAX_BATCH_ROWS),
            hotkeys: Vec::with_capacity(MAX_BATCH_ROWS),
            tabs: Vec::with_capacity(MAX_BATCH_ROWS),
            generated_tabs: Vec::with_capacity(MAX_BATCH_ROWS),
            tab_targets: Vec::new(),
            memberships: Vec::with_capacity(MAX_BATCH_ROWS),
            generated_memberships: Vec::with_capacity(MAX_BATCH_ROWS),
            sound_count: 0,
            root_count: 0,
            manual_tab_count: 0,
            membership_count: 0,
            generated_tab_count: 0,
            hotkey_count: 0,
        }
    }

    fn push_roots(&mut self, roots: Vec<RootRecord>) -> Result<(), LegacyMigrationError> {
        self.root_count = self.root_count.saturating_add(roots.len());
        if !roots.is_empty() {
            self.store.apply_batch(LibraryBatch::Roots(roots)).recv()?;
        }
        Ok(())
    }

    fn push_sound(&mut self, mut sound: Sound) -> Result<(), LegacyMigrationError> {
        if let Some(hotkey) = sound.hotkey.take() {
            self.hotkeys
                .push(crate::library_store::legacy_hotkey_binding(
                    sound.id.clone(),
                    HotkeyBindingOwner::Sound(sound.id.clone()),
                    &hotkey,
                ));
            self.hotkey_count = self.hotkey_count.saturating_add(1);
        }
        self.sounds.push(SoundRecord {
            sound,
            general_position: self.sound_count,
            locations: Vec::new(),
        });
        self.sound_count = self.sound_count.saturating_add(1);
        if self.sounds.len() >= MAX_BATCH_ROWS {
            self.flush_sounds()?;
        }
        if self.hotkeys.len() >= MAX_BATCH_ROWS {
            self.flush_hotkeys()?;
        }
        Ok(())
    }

    fn flush_sounds(&mut self) -> Result<(), LegacyMigrationError> {
        if !self.sounds.is_empty() {
            self.store
                .apply_batch(LibraryBatch::Sounds(std::mem::take(&mut self.sounds)))
                .recv()?;
        }
        Ok(())
    }

    fn flush_hotkeys(&mut self) -> Result<(), LegacyMigrationError> {
        if !self.hotkeys.is_empty() {
            self.store
                .apply_batch(LibraryBatch::HotkeyBindings(std::mem::take(
                    &mut self.hotkeys,
                )))
                .recv()?;
        }
        Ok(())
    }

    fn push_tab(&mut self, tab: TabMetadata) -> Result<(), LegacyMigrationError> {
        let _ = tab.sound_ids;
        if let Some(binding) = tab.folder_binding {
            self.generated_tab_count = self.generated_tab_count.saturating_add(1);
            self.tab_targets
                .push(ImportedTab::Generated(tab.id.clone()));
            self.generated_tabs.push(LegacyGeneratedTabRecord {
                public_id: tab.id,
                root_path: binding.root_folder,
                relative_path: binding.relative_subfolder,
                name: tab.name,
                position: tab.order as usize,
            });
            if self.generated_tabs.len() >= MAX_BATCH_ROWS {
                self.flush_generated_tabs()?;
            }
            return Ok(());
        }
        self.tab_targets.push(ImportedTab::Manual(tab.id.clone()));
        self.tabs.push(ManualTabRecord {
            public_id: tab.id,
            name: tab.name,
            position: tab.order as usize,
        });
        self.manual_tab_count = self.manual_tab_count.saturating_add(1);
        if self.tabs.len() >= MAX_BATCH_ROWS {
            self.flush_tabs()?;
        }
        Ok(())
    }

    fn flush_tabs(&mut self) -> Result<(), LegacyMigrationError> {
        if !self.tabs.is_empty() {
            self.store
                .apply_batch(LibraryBatch::ManualTabs(std::mem::take(&mut self.tabs)))
                .recv()?;
        }
        Ok(())
    }

    fn flush_generated_tabs(&mut self) -> Result<(), LegacyMigrationError> {
        if !self.generated_tabs.is_empty() {
            self.store
                .apply_batch(LibraryBatch::LegacyGeneratedTabs(std::mem::take(
                    &mut self.generated_tabs,
                )))
                .recv()?;
        }
        Ok(())
    }

    fn push_membership(
        &mut self,
        target: &ImportedTab,
        sound_id: String,
        position: usize,
    ) -> Result<(), LegacyMigrationError> {
        match target {
            ImportedTab::Manual(tab_id) => {
                self.memberships.push(ManualMembershipRecord {
                    tab_public_id: tab_id.clone(),
                    sound_public_id: sound_id,
                    position,
                });
                self.membership_count = self.membership_count.saturating_add(1);
            }
            ImportedTab::Generated(tab_id) => {
                self.generated_memberships
                    .push(LegacyGeneratedMembershipRecord {
                        tab_public_id: tab_id.clone(),
                        sound_public_id: sound_id,
                        position,
                    })
            }
        }
        if self.memberships.len() >= MAX_BATCH_ROWS {
            self.flush_memberships()?;
        }
        if self.generated_memberships.len() >= MAX_BATCH_ROWS {
            self.flush_generated_memberships()?;
        }
        Ok(())
    }

    fn flush_memberships(&mut self) -> Result<(), LegacyMigrationError> {
        if !self.memberships.is_empty() {
            self.store
                .apply_batch(LibraryBatch::ManualMemberships(std::mem::take(
                    &mut self.memberships,
                )))
                .recv()?;
        }
        Ok(())
    }

    fn flush_generated_memberships(&mut self) -> Result<(), LegacyMigrationError> {
        if !self.generated_memberships.is_empty() {
            self.store
                .apply_batch(LibraryBatch::LegacyGeneratedMemberships(std::mem::take(
                    &mut self.generated_memberships,
                )))
                .recv()?;
        }
        Ok(())
    }

    fn finish_first_pass(&mut self) -> Result<(), LegacyMigrationError> {
        self.flush_sounds()?;
        self.flush_hotkeys()?;
        self.flush_tabs()?;
        self.flush_generated_tabs()?;
        let version = self
            .schema_version
            .ok_or_else(|| LegacyMigrationError::Invalid("missing schema_version".to_string()))?;
        if version > crate::config::CURRENT_SCHEMA_VERSION {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy schema {version} is newer than supported schema {}",
                crate::config::CURRENT_SCHEMA_VERSION
            )));
        }
        let settings = self
            .settings
            .take()
            .ok_or_else(|| LegacyMigrationError::Invalid("missing settings".to_string()))?;
        let migrated = crate::config::migration::run_migrations(
            serde_json::json!({
                "schema_version": version,
                "sound_folders": [],
                "sounds": [],
                "tabs": [],
                "settings": settings,
            }),
            version,
        )?;
        let settings: Settings = serde_json::from_value(migrated["settings"].clone())?;
        for metadata in ControlHotkeyAction::all() {
            if let Some(hotkey) = settings.control_hotkeys.get_cloned(metadata.action) {
                self.hotkeys
                    .push(crate::library_store::legacy_hotkey_binding(
                        metadata.binding_id.to_string(),
                        HotkeyBindingOwner::Control(metadata.id.to_string()),
                        &hotkey,
                    ));
                self.hotkey_count = self.hotkey_count.saturating_add(1);
            }
        }
        self.flush_hotkeys()
    }
}

struct FirstPassSeed<'a>(&'a mut ImportState);

impl<'de> DeserializeSeed<'de> for FirstPassSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(FirstPassVisitor(self.0))
    }
}

struct FirstPassVisitor<'a>(&'a mut ImportState);

impl<'de> Visitor<'de> for FirstPassVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a legacy Linux SoundBoard configuration object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while let Some(field) = map.next_key::<String>()? {
            match field.as_str() {
                "schema_version" => self.0.schema_version = Some(map.next_value()?),
                "sound_folders" => map.next_value_seed(RootsSeed(self.0))?,
                "sounds" => map.next_value_seed(SoundsSeed(self.0))?,
                "tabs" => map.next_value_seed(TabsSeed(self.0))?,
                "settings" => self.0.settings = Some(map.next_value()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

struct RootsSeed<'a>(&'a mut ImportState);

impl<'de> DeserializeSeed<'de> for RootsSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct RootsVisitor<'a>(&'a mut ImportState);
        impl<'de> Visitor<'de> for RootsVisitor<'_> {
            type Value = ();
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of sound folder paths")
            }
            fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut roots = Vec::with_capacity(MAX_BATCH_ROWS);
                let mut position = self.0.root_count;
                while let Some(path) = sequence.next_element::<String>()? {
                    roots.push(RootRecord { path, position });
                    position = position.saturating_add(1);
                    if roots.len() == MAX_BATCH_ROWS {
                        self.0
                            .push_roots(std::mem::take(&mut roots))
                            .map_err(serde::de::Error::custom)?;
                    }
                }
                self.0.push_roots(roots).map_err(serde::de::Error::custom)
            }
        }
        deserializer.deserialize_seq(RootsVisitor(self.0))
    }
}

struct SoundsSeed<'a>(&'a mut ImportState);

impl<'de> DeserializeSeed<'de> for SoundsSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SoundsVisitor<'a>(&'a mut ImportState);
        impl<'de> Visitor<'de> for SoundsVisitor<'_> {
            type Value = ();
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of sounds")
            }
            fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
            where
                A: SeqAccess<'de>,
            {
                while let Some(sound) = sequence.next_element::<Sound>()? {
                    self.0.push_sound(sound).map_err(serde::de::Error::custom)?;
                }
                Ok(())
            }
        }
        deserializer.deserialize_seq(SoundsVisitor(self.0))
    }
}

struct TabsSeed<'a>(&'a mut ImportState);

impl<'de> DeserializeSeed<'de> for TabsSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TabsVisitor<'a>(&'a mut ImportState);
        impl<'de> Visitor<'de> for TabsVisitor<'_> {
            type Value = ();
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of sound tabs")
            }
            fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
            where
                A: SeqAccess<'de>,
            {
                while let Some(tab) = sequence.next_element::<TabMetadata>()? {
                    self.0.push_tab(tab).map_err(serde::de::Error::custom)?;
                }
                Ok(())
            }
        }
        deserializer.deserialize_seq(TabsVisitor(self.0))
    }
}

struct MembershipPassSeed<'a>(&'a mut ImportState);

impl<'de> DeserializeSeed<'de> for MembershipPassSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(MembershipTopVisitor(self.0))
    }
}

struct MembershipTopVisitor<'a>(&'a mut ImportState);

impl<'de> Visitor<'de> for MembershipTopVisitor<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a legacy Linux SoundBoard configuration object")
    }
    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        while let Some(field) = map.next_key::<String>()? {
            if field == "tabs" {
                map.next_value_seed(MembershipTabsSeed(self.0))?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct MembershipTabsSeed<'a>(&'a mut ImportState);

impl<'de> DeserializeSeed<'de> for MembershipTabsSeed<'_> {
    type Value = ();
    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct MembershipTabsVisitor<'a>(&'a mut ImportState);
        impl<'de> Visitor<'de> for MembershipTabsVisitor<'_> {
            type Value = ();
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of sound tabs")
            }
            fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut ordinal = 0_usize;
                while ordinal < self.0.tab_targets.len() {
                    let target = self.0.tab_targets[ordinal].clone();
                    if sequence
                        .next_element_seed(MembershipTabSeed {
                            state: self.0,
                            target,
                        })?
                        .is_none()
                    {
                        break;
                    }
                    ordinal = ordinal.saturating_add(1);
                }
                while sequence.next_element::<IgnoredAny>()?.is_some() {}
                Ok(())
            }
        }
        deserializer.deserialize_seq(MembershipTabsVisitor(self.0))
    }
}

struct MembershipTabSeed<'a> {
    state: &'a mut ImportState,
    target: ImportedTab,
}

impl<'de> DeserializeSeed<'de> for MembershipTabSeed<'_> {
    type Value = ();
    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(MembershipTabVisitor {
            state: self.state,
            target: self.target,
        })
    }
}

struct MembershipTabVisitor<'a> {
    state: &'a mut ImportState,
    target: ImportedTab,
}

impl<'de> Visitor<'de> for MembershipTabVisitor<'_> {
    type Value = ();
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a sound tab")
    }
    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let state = self.state;
        while let Some(field) = map.next_key::<String>()? {
            if field == "sound_ids" {
                map.next_value_seed(MembershipIdsSeed {
                    state,
                    target: &self.target,
                })?;
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(())
    }
}

struct MembershipIdsSeed<'a> {
    state: &'a mut ImportState,
    target: &'a ImportedTab,
}

impl<'de> DeserializeSeed<'de> for MembershipIdsSeed<'_> {
    type Value = ();
    fn deserialize<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct MembershipIdsVisitor<'a> {
            state: &'a mut ImportState,
            target: &'a ImportedTab,
        }
        impl<'de> Visitor<'de> for MembershipIdsVisitor<'_> {
            type Value = ();
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an array of sound IDs")
            }
            fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut position = 0_usize;
                while let Some(sound_id) = sequence.next_element::<String>()? {
                    self.state
                        .push_membership(self.target, sound_id, position)
                        .map_err(serde::de::Error::custom)?;
                    position = position.saturating_add(1);
                }
                Ok(())
            }
        }
        deserializer.deserialize_seq(MembershipIdsVisitor {
            state: self.state,
            target: self.target,
        })
    }
}

fn sha256(path: &Path) -> Result<String, LegacyMigrationError> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn ensure_backup(source: &Path, expected_sha256: &str) -> Result<(), LegacyMigrationError> {
    let backup = source.with_file_name("config.json.pre-v8-backup");
    if backup.exists() {
        if sha256(&backup)? == expected_sha256 {
            fs::set_permissions(backup, fs::Permissions::from_mode(0o600))?;
            return Ok(());
        }
        return Err(LegacyMigrationError::Invalid(
            "the existing pre-v8 backup does not match the migration source".to_string(),
        ));
    }
    if fs::hard_link(source, &backup).is_err() {
        let mut reader = BufReader::new(fs::File::open(source)?);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&backup)?;
        let mut writer = BufWriter::new(&file);
        std::io::copy(&mut reader, &mut writer)?;
        writer.flush()?;
        drop(writer);
        file.sync_all()?;
    }
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;
    if let Some(parent) = backup.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn parse_pass(
    source: &Path,
    state: &mut ImportState,
    memberships: bool,
) -> Result<(), LegacyMigrationError> {
    let reader = BufReader::new(fs::File::open(source)?);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    if memberships {
        MembershipPassSeed(state).deserialize(&mut deserializer)?;
    } else {
        FirstPassSeed(state).deserialize(&mut deserializer)?;
    }
    deserializer.end()?;
    Ok(())
}

fn finalize_database(
    path: &Path,
    source_sha256: &str,
    library_id: &str,
    expected_sounds: usize,
    expected_hotkeys: usize,
) -> Result<(), LegacyMigrationError> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "UPDATE hotkey_bindings AS binding
         SET issue = 'duplicate legacy binding'
         WHERE issue = 'valid legacy candidate'
           AND (SELECT COUNT(*) FROM hotkey_bindings AS other
                WHERE other.accelerator = binding.accelerator
                  AND other.issue = 'valid legacy candidate') > 1;
         UPDATE hotkey_bindings AS binding
         SET normalized = accelerator, state = 'active', issue = NULL
         WHERE issue = 'valid legacy candidate'
           AND (SELECT COUNT(*) FROM hotkey_bindings AS other
                WHERE other.accelerator = binding.accelerator
                  AND other.issue = 'valid legacy candidate') = 1;",
    )?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(LegacyMigrationError::Invalid(format!(
            "migrated database failed integrity_check: {integrity}"
        )));
    }
    let foreign_key_errors: usize = connection
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |_| Ok(()))?
        .count();
    if foreign_key_errors != 0 {
        return Err(LegacyMigrationError::Invalid(format!(
            "migrated database has {foreign_key_errors} foreign-key errors"
        )));
    }
    let sounds: i64 = connection.query_row("SELECT count(*) FROM sounds", [], |row| row.get(0))?;
    let hotkeys: i64 =
        connection.query_row("SELECT count(*) FROM hotkey_bindings", [], |row| row.get(0))?;
    let sounds = usize::try_from(sounds)
        .map_err(|_| LegacyMigrationError::Invalid("negative sound count".to_string()))?;
    let hotkeys = usize::try_from(hotkeys)
        .map_err(|_| LegacyMigrationError::Invalid("negative hotkey count".to_string()))?;
    if sounds != expected_sounds || hotkeys != expected_hotkeys {
        return Err(LegacyMigrationError::Invalid(format!(
            "migration verification mismatch: sounds {sounds}/{expected_sounds}, hotkeys {hotkeys}/{expected_hotkeys}"
        )));
    }
    connection.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('source_sha256', ?1)",
        [source_sha256],
    )?;
    connection.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('library_id', ?1)",
        [library_id],
    )?;
    connection.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES('database_ready', '1')",
        [],
    )?;
    drop(connection);
    fs::File::open(path)?.sync_all()?;
    Ok(())
}

pub fn migrate_legacy_database(
    source: &Path,
    destination: &Path,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    if destination.exists() {
        return Err(LegacyMigrationError::Invalid(format!(
            "refusing to replace existing database '{}'",
            destination.display()
        )));
    }
    let source_sha256 = sha256(source)?;
    ensure_backup(source, &source_sha256)?;
    let candidate = destination.with_file_name(format!(
        ".library.sqlite3.importing.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> Result<LegacyMigrationReport, LegacyMigrationError> {
        let store = LibraryStore::open(candidate.clone())?;
        let mut state = ImportState::new(store);
        parse_pass(source, &mut state, false)?;
        state.finish_first_pass()?;
        parse_pass(source, &mut state, true)?;
        state.flush_memberships()?;
        state.flush_generated_memberships()?;
        let report = LegacyMigrationReport {
            sounds: state.sound_count,
            roots: state.root_count,
            manual_tabs: state.manual_tab_count,
            manual_memberships: state.membership_count,
            generated_tabs_deferred: state.generated_tab_count,
            hotkeys: state.hotkey_count,
            source_sha256: source_sha256.clone(),
            library_id: uuid::Uuid::new_v4().to_string(),
        };
        drop(state);
        finalize_database(
            &candidate,
            &report.source_sha256,
            &report.library_id,
            report.sounds,
            report.hotkeys,
        )?;
        if sha256(source)? != source_sha256 {
            return Err(LegacyMigrationError::Invalid(
                "legacy config changed while migration was running".to_string(),
            ));
        }
        fs::rename(&candidate, destination)?;
        if let Some(parent) = destination.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(report)
    })();
    if result.is_err() {
        let _ = fs::remove_file(candidate);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, SoundTab};
    use crate::library_store::LibraryScope;

    #[test]
    fn legacy_import_streams_bounded_batches_and_preserves_manual_data() {
        let directory =
            std::env::temp_dir().join(format!("lsb-legacy-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        config.sound_folders.push("/music".to_string());
        for index in 0..1_025 {
            let mut sound = Sound::new(
                format!("Sound {index}"),
                format!("/music/sound-{index}.flac"),
            );
            sound.id = format!("sound-{index}");
            if index < 2 {
                sound.hotkey = Some(format!("Ctrl+Digit{}", index + 1));
            }
            config.sounds.push(sound);
        }
        let mut manual = SoundTab::new("Manual".to_string(), 0);
        manual.id = "manual".to_string();
        manual.sound_ids = config.sounds.iter().map(|sound| sound.id.clone()).collect();
        config.tabs.push(manual);
        let mut generated = SoundTab::new("Generated".to_string(), 1);
        generated.id = "generated".to_string();
        generated.sound_ids.push("sound-0".to_string());
        generated.folder_binding = Some(FolderTabBinding {
            root_folder: "/music".to_string(),
            relative_subfolder: "album".to_string(),
        });
        config.tabs.push(generated);
        serde_json::to_writer(fs::File::create(&source).expect("create config"), &config)
            .expect("write config");

        let report = migrate_legacy_database(&source, &destination).expect("migrate config");

        assert_eq!(report.sounds, 1_025);
        assert_eq!(report.manual_tabs, 1);
        assert_eq!(report.manual_memberships, 1_025);
        assert_eq!(report.generated_tabs_deferred, 1);
        assert_eq!(report.hotkeys, 2);
        assert!(directory.join("config.json.pre-v8-backup").exists());
        let store = LibraryStore::open(destination).expect("open migrated database");
        assert_eq!(
            store
                .count(LibraryScope::General, "")
                .recv()
                .expect("count sounds"),
            1_025
        );
        assert_eq!(
            store
                .count(LibraryScope::ManualTab("manual".to_string()), "")
                .recv()
                .expect("count manual membership"),
            1_025
        );
        drop(store);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn malformed_legacy_config_never_publishes_a_database() {
        let directory =
            std::env::temp_dir().join(format!("lsb-bad-legacy-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        fs::write(&source, br#"{"schema_version":7,"sounds":["#).expect("write malformed config");

        assert!(migrate_legacy_database(&source, &destination).is_err());
        assert!(!destination.exists());
        assert!(!directory
            .read_dir()
            .expect("read test directory")
            .any(|entry| entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .contains(".importing")));

        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn duplicate_and_invalid_hotkeys_survive_as_needs_attention() {
        let directory =
            std::env::temp_dir().join(format!("lsb-legacy-hotkeys-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        for (id, hotkey) in [
            ("first", "Ctrl+KeyA"),
            ("second", "Ctrl+KeyA"),
            ("invalid", "not a hotkey"),
        ] {
            let mut sound = Sound::new(id.to_string(), format!("/music/{id}.flac"));
            sound.id = id.to_string();
            sound.hotkey = Some(hotkey.to_string());
            config.sounds.push(sound);
        }
        serde_json::to_writer(fs::File::create(&source).expect("create config"), &config)
            .expect("write config");

        let report = migrate_legacy_database(&source, &destination).expect("migrate config");

        assert_eq!(report.hotkeys, 3);
        let connection = Connection::open(&destination).expect("open migrated database");
        let preserved: i64 = connection
            .query_row("SELECT count(*) FROM hotkey_bindings", [], |row| row.get(0))
            .expect("count preserved bindings");
        let attention: i64 = connection
            .query_row(
                "SELECT count(*) FROM hotkey_bindings WHERE state = 'needs_attention'",
                [],
                |row| row.get(0),
            )
            .expect("count attention bindings");
        assert_eq!(preserved, 3);
        assert_eq!(attention, 3);
        drop(connection);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn importer_accepts_every_legacy_schema_number_zero_through_seven() {
        for version in 0..=crate::config::CURRENT_SCHEMA_VERSION {
            let directory = std::env::temp_dir().join(format!(
                "lsb-legacy-schema-{version}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let source = directory.join("config.json");
            let destination = directory.join("library.sqlite3");
            let mut value = serde_json::to_value(Config::default()).expect("serialize config");
            value["schema_version"] = serde_json::json!(version);
            serde_json::to_writer(fs::File::create(&source).expect("create config"), &value)
                .expect("write config");

            migrate_legacy_database(&source, &destination)
                .unwrap_or_else(|error| panic!("schema {version} failed: {error}"));

            assert!(destination.exists());
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }
}
