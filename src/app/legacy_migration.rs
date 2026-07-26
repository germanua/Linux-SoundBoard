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
    #[error("migration cancelled")]
    Cancelled,
    #[error("invalid legacy migration input: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct LegacyMigrationReport {
    pub sounds: usize,
    pub roots: usize,
    pub manual_tabs: usize,
    pub manual_memberships: usize,
    pub generated_tabs_deferred: usize,
    pub hotkeys: usize,
    pub source_sha256: String,
    pub library_id: String,
    pub settings: Settings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseIdentity {
    pub library_id: String,
    pub source_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyRestoreReport {
    pub archived_config: Option<PathBuf>,
    pub archived_database: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyMigrationProgress {
    BackingUp,
    Importing,
    Verifying,
    PublishingDatabase,
    PublishingSettings,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyMigrationBoundary {
    BeforeBackupWrite,
    BackupSynced,
    BackupPublished,
    BeforeDatabaseWrite,
    DatabaseBatchCommitted,
    DatabaseSynced,
    BeforeDatabasePublish,
    DatabasePublished,
    BeforeSettingsWrite,
    SettingsSynced,
    SettingsRenamed,
    SettingsPublished,
}

#[cfg(test)]
impl LegacyMigrationBoundary {
    const ALL: [Self; 8] = [
        Self::BackupSynced,
        Self::BackupPublished,
        Self::DatabaseBatchCommitted,
        Self::DatabaseSynced,
        Self::DatabasePublished,
        Self::SettingsSynced,
        Self::SettingsRenamed,
        Self::SettingsPublished,
    ];
    const WRITE_PHASES: [Self; 3] = [
        Self::BeforeBackupWrite,
        Self::BeforeDatabaseWrite,
        Self::BeforeSettingsWrite,
    ];
}

type MigrationObserver =
    Arc<dyn Fn(LegacyMigrationBoundary) -> Result<(), LegacyMigrationError> + Send + Sync>;

fn noop_observer() -> MigrationObserver {
    Arc::new(|_| Ok(()))
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
    observer: MigrationObserver,
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
    migrated_settings: Option<Settings>,
}

impl ImportState {
    fn new(store: LibraryStore, observer: MigrationObserver) -> Self {
        Self {
            store,
            observer,
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
            migrated_settings: None,
        }
    }

    fn push_roots(&mut self, roots: Vec<RootRecord>) -> Result<(), LegacyMigrationError> {
        self.root_count = self.root_count.saturating_add(roots.len());
        if !roots.is_empty() {
            self.store.apply_batch(LibraryBatch::Roots(roots)).recv()?;
            (self.observer)(LegacyMigrationBoundary::DatabaseBatchCommitted)?;
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
            (self.observer)(LegacyMigrationBoundary::DatabaseBatchCommitted)?;
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
            (self.observer)(LegacyMigrationBoundary::DatabaseBatchCommitted)?;
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
            (self.observer)(LegacyMigrationBoundary::DatabaseBatchCommitted)?;
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
            (self.observer)(LegacyMigrationBoundary::DatabaseBatchCommitted)?;
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
            (self.observer)(LegacyMigrationBoundary::DatabaseBatchCommitted)?;
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
            (self.observer)(LegacyMigrationBoundary::DatabaseBatchCommitted)?;
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
        if version > crate::config::LAST_LEGACY_SCHEMA_VERSION {
            return Err(LegacyMigrationError::Invalid(format!(
                "legacy schema {version} is newer than supported schema {}",
                crate::config::LAST_LEGACY_SCHEMA_VERSION
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
        let mut runtime_settings = settings;
        runtime_settings.control_hotkeys = Default::default();
        self.migrated_settings = Some(runtime_settings);
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
    sha256_observed(
        path,
        &noop_observer(),
        LegacyMigrationBoundary::BeforeBackupWrite,
    )
}

fn sha256_observed(
    path: &Path,
    observer: &MigrationObserver,
    boundary: LegacyMigrationBoundary,
) -> Result<String, LegacyMigrationError> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        observer(boundary)?;
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn remove_stale_candidates(parent: &Path, prefix: &str) -> Result<(), LegacyMigrationError> {
    let mut removed = false;
    for entry in fs::read_dir(parent)? {
        let entry = entry?;
        if entry.file_type()?.is_file() && entry.file_name().to_string_lossy().starts_with(prefix) {
            fs::remove_file(entry.path())?;
            removed = true;
        }
    }
    if removed {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn ensure_backup(
    source: &Path,
    expected_sha256: &str,
    observer: &MigrationObserver,
) -> Result<(), LegacyMigrationError> {
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

    let parent = source.parent().ok_or_else(|| {
        LegacyMigrationError::Invalid("legacy config has no parent directory".to_string())
    })?;
    remove_stale_candidates(parent, ".config.json.pre-v8-backup.")?;
    let candidate = source.with_file_name(format!(
        ".config.json.pre-v8-backup.{}",
        uuid::Uuid::new_v4()
    ));
    let result = (|| {
        observer(LegacyMigrationBoundary::BeforeBackupWrite)?;
        let mut reader = BufReader::new(fs::File::open(source)?);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            observer(LegacyMigrationBoundary::BeforeBackupWrite)?;
            let count = reader.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            file.write_all(&buffer[..count])?;
        }
        file.sync_all()?;
        observer(LegacyMigrationBoundary::BackupSynced)?;
        if sha256_observed(
            &candidate,
            observer,
            LegacyMigrationBoundary::BeforeBackupWrite,
        )? != expected_sha256
        {
            return Err(LegacyMigrationError::Invalid(
                "the legacy config changed while its migration backup was being created"
                    .to_string(),
            ));
        }
        fs::rename(&candidate, &backup)?;
        if let Some(parent) = backup.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        observer(LegacyMigrationBoundary::BackupPublished)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(candidate);
    }
    result
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

#[derive(Default)]
struct LegacySettingsState {
    schema_version: Option<u32>,
    settings: Option<serde_json::Value>,
}

struct LegacySettingsSeed<'a>(&'a mut LegacySettingsState);

impl<'de> DeserializeSeed<'de> for LegacySettingsSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct SettingsVisitor<'a>(&'a mut LegacySettingsState);
        impl<'de> Visitor<'de> for SettingsVisitor<'_> {
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
                        "settings" => self.0.settings = Some(map.next_value()?),
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(())
            }
        }
        deserializer.deserialize_map(SettingsVisitor(self.0))
    }
}

pub fn config_schema_version(path: &Path) -> Result<u32, LegacyMigrationError> {
    Ok(read_legacy_settings_state(path)?
        .schema_version
        .unwrap_or(0))
}

fn read_legacy_settings_state(path: &Path) -> Result<LegacySettingsState, LegacyMigrationError> {
    let reader = BufReader::new(fs::File::open(path)?);
    let mut deserializer = serde_json::Deserializer::from_reader(reader);
    let mut state = LegacySettingsState::default();
    LegacySettingsSeed(&mut state).deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(state)
}

pub(crate) fn read_legacy_runtime_settings(path: &Path) -> Result<Settings, LegacyMigrationError> {
    let mut state = read_legacy_settings_state(path)?;
    let version = state.schema_version.unwrap_or(0);
    if version > crate::config::LAST_LEGACY_SCHEMA_VERSION {
        return Err(LegacyMigrationError::Invalid(format!(
            "schema {version} is not a legacy configuration"
        )));
    }
    let migrated = crate::config::migration::run_migrations(
        serde_json::json!({
            "schema_version": version,
            "sound_folders": [],
            "sounds": [],
            "tabs": [],
            "settings": state.settings.take().unwrap_or_else(|| serde_json::json!({})),
        }),
        version,
    )?;
    let mut settings: Settings = serde_json::from_value(migrated["settings"].clone())?;
    settings.control_hotkeys = Default::default();
    Ok(settings)
}

fn finalize_database(
    path: &Path,
    source_sha256: &str,
    library_id: &str,
    expected_sounds: usize,
    expected_hotkeys: usize,
    observer: &MigrationObserver,
) -> Result<(), LegacyMigrationError> {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "INSERT OR IGNORE INTO manual_tabs(public_id, name, position)
         SELECT public_id, name, position FROM legacy_generated_tabs;
         INSERT OR IGNORE INTO manual_memberships(tab_id, sound_id, position)
         SELECT manual.id, legacy.sound_id, legacy.position
         FROM legacy_generated_memberships AS legacy
         JOIN legacy_generated_tabs AS generated ON generated.id = legacy.tab_id
         JOIN manual_tabs AS manual ON manual.public_id = generated.public_id;
         UPDATE hotkey_bindings AS binding
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
    observer(LegacyMigrationBoundary::DatabaseSynced)?;
    Ok(())
}

pub fn migrate_legacy_database(
    source: &Path,
    destination: &Path,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    migrate_legacy_database_observed(source, destination, noop_observer())
}

fn migrate_legacy_database_observed(
    source: &Path,
    destination: &Path,
    observer: MigrationObserver,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    if destination.exists() {
        return Err(LegacyMigrationError::Invalid(format!(
            "refusing to replace existing database '{}'",
            destination.display()
        )));
    }
    let source_sha256 = sha256_observed(
        source,
        &observer,
        LegacyMigrationBoundary::BeforeBackupWrite,
    )?;
    ensure_backup(source, &source_sha256, &observer)?;
    let parent = destination.parent().ok_or_else(|| {
        LegacyMigrationError::Invalid("library database has no parent directory".to_string())
    })?;
    remove_stale_candidates(parent, ".library.sqlite3.importing.")?;
    let candidate = destination.with_file_name(format!(
        ".library.sqlite3.importing.{}.{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let result = (|| -> Result<LegacyMigrationReport, LegacyMigrationError> {
        observer(LegacyMigrationBoundary::BeforeDatabaseWrite)?;
        let store = LibraryStore::open(candidate.clone())?;
        let mut state = ImportState::new(store, Arc::clone(&observer));
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
            settings: state.migrated_settings.take().ok_or_else(|| {
                LegacyMigrationError::Invalid("legacy settings were not imported".to_string())
            })?,
        };
        drop(state);
        finalize_database(
            &candidate,
            &report.source_sha256,
            &report.library_id,
            report.sounds,
            report.hotkeys,
            &observer,
        )?;
        if sha256_observed(source, &observer, LegacyMigrationBoundary::DatabaseSynced)?
            != source_sha256
        {
            return Err(LegacyMigrationError::Invalid(
                "legacy config changed while migration was running".to_string(),
            ));
        }
        observer(LegacyMigrationBoundary::BeforeDatabasePublish)?;
        fs::rename(&candidate, destination)?;
        if let Some(parent) = destination.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        observer(LegacyMigrationBoundary::DatabasePublished)?;
        Ok(report)
    })();
    if result.is_err() {
        let _ = fs::remove_file(candidate);
    }
    result
}

pub fn database_identity(path: &Path) -> Result<DatabaseIdentity, LegacyMigrationError> {
    if !path.is_file() {
        return Err(LegacyMigrationError::Invalid(format!(
            "library database '{}' is missing",
            path.display()
        )));
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let application_id: i64 =
        connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id != 0 && application_id != crate::library_store::DATABASE_APPLICATION_ID {
        return Err(LegacyMigrationError::Invalid(format!(
            "library database application id is {application_id}, expected {}",
            crate::library_store::DATABASE_APPLICATION_ID
        )));
    }
    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if schema_version != 3 {
        return Err(LegacyMigrationError::Invalid(format!(
            "library database schema is {schema_version}, expected 3"
        )));
    }
    let ready: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'database_ready'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if ready.as_deref() != Some("1") {
        return Err(LegacyMigrationError::Invalid(
            "library database is not marked ready".to_string(),
        ));
    }
    let flavor: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_flavor'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if flavor.as_deref() != Some("bounded-generation-v3") {
        return Err(LegacyMigrationError::Invalid(
            "library database metadata does not match the bounded schema".to_string(),
        ));
    }
    let library_id: Option<String> = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'library_id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let library_id = library_id
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            LegacyMigrationError::Invalid("library database has no identity".to_string())
        })?;
    let source_sha256 = connection
        .query_row(
            "SELECT value FROM meta WHERE key = 'source_sha256'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if application_id == 0 {
        connection.pragma_update(
            None,
            "application_id",
            crate::library_store::DATABASE_APPLICATION_ID,
        )?;
        fs::File::open(path)?.sync_all()?;
    }
    Ok(DatabaseIdentity {
        library_id,
        source_sha256,
    })
}

fn write_schema_8_config(path: &Path, settings: Settings) -> Result<(), LegacyMigrationError> {
    write_schema_8_config_observed(path, settings, &noop_observer())
}

fn write_schema_8_config_observed(
    path: &Path,
    settings: Settings,
    observer: &MigrationObserver,
) -> Result<(), LegacyMigrationError> {
    let mut config = crate::config::Config {
        settings,
        ..crate::config::Config::default()
    };
    let result = config.save_to_path_observed(path, |boundary| {
        observer(match boundary {
            crate::config::ConfigSaveBoundary::CandidateSynced => {
                LegacyMigrationBoundary::SettingsSynced
            }
            crate::config::ConfigSaveBoundary::Renamed => LegacyMigrationBoundary::SettingsRenamed,
            crate::config::ConfigSaveBoundary::DirectorySynced => {
                LegacyMigrationBoundary::SettingsPublished
            }
        })
    });
    match result {
        Ok(()) => Ok(()),
        Err(error) => match error.downcast::<LegacyMigrationError>() {
            Ok(error) => Err(*error),
            Err(error) => Err(LegacyMigrationError::Invalid(error.to_string())),
        },
    }
}

pub fn complete_legacy_settings_cutover(
    source: &Path,
    destination: &Path,
) -> Result<DatabaseIdentity, LegacyMigrationError> {
    let identity = database_identity(destination)?;
    let expected_source = identity.source_sha256.as_deref().ok_or_else(|| {
        LegacyMigrationError::Invalid("migrated database has no source checksum".to_string())
    })?;
    if sha256(source)? != expected_source {
        return Err(LegacyMigrationError::Invalid(
            "legacy config does not match the ready database".to_string(),
        ));
    }
    let settings = read_legacy_runtime_settings(source)?;
    write_schema_8_config(source, settings)?;
    let config = crate::config::Config::load_from_path(source)
        .map_err(|error| LegacyMigrationError::Invalid(error.to_string()))?;
    if config.schema_version != crate::config::CURRENT_SCHEMA_VERSION {
        return Err(LegacyMigrationError::Invalid(
            "settings schema verification failed".to_string(),
        ));
    }
    Ok(identity)
}

pub fn migrate_legacy_config(
    source: &Path,
    destination: &Path,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    migrate_legacy_config_observed(source, destination, noop_observer())
}

pub fn migrate_legacy_config_controlled(
    source: &Path,
    destination: &Path,
    cancelled: Arc<AtomicBool>,
    on_progress: Arc<dyn Fn(LegacyMigrationProgress) + Send + Sync>,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    let last_progress = AtomicU8::new(u8::MAX);
    migrate_legacy_config_observed(
        source,
        destination,
        Arc::new(move |boundary| {
            if cancelled.load(Ordering::Relaxed) {
                return Err(LegacyMigrationError::Cancelled);
            }
            let progress = match boundary {
                LegacyMigrationBoundary::BeforeBackupWrite
                | LegacyMigrationBoundary::BackupSynced
                | LegacyMigrationBoundary::BackupPublished => LegacyMigrationProgress::BackingUp,
                LegacyMigrationBoundary::BeforeDatabaseWrite
                | LegacyMigrationBoundary::DatabaseBatchCommitted => {
                    LegacyMigrationProgress::Importing
                }
                LegacyMigrationBoundary::DatabaseSynced => LegacyMigrationProgress::Verifying,
                LegacyMigrationBoundary::BeforeDatabasePublish
                | LegacyMigrationBoundary::DatabasePublished => {
                    LegacyMigrationProgress::PublishingDatabase
                }
                LegacyMigrationBoundary::BeforeSettingsWrite
                | LegacyMigrationBoundary::SettingsSynced
                | LegacyMigrationBoundary::SettingsRenamed => {
                    LegacyMigrationProgress::PublishingSettings
                }
                LegacyMigrationBoundary::SettingsPublished => LegacyMigrationProgress::Complete,
            };
            let progress_id = progress as u8;
            if last_progress.swap(progress_id, Ordering::Relaxed) != progress_id {
                on_progress(progress);
            }
            if cancelled.load(Ordering::Relaxed) {
                Err(LegacyMigrationError::Cancelled)
            } else {
                Ok(())
            }
        }),
    )
}

fn migrate_legacy_config_observed(
    source: &Path,
    destination: &Path,
    observer: MigrationObserver,
) -> Result<LegacyMigrationReport, LegacyMigrationError> {
    let report = migrate_legacy_database_observed(source, destination, Arc::clone(&observer))?;
    observer(LegacyMigrationBoundary::BeforeSettingsWrite)?;
    write_schema_8_config_observed(source, report.settings.clone(), &observer)?;
    let identity = database_identity(destination)?;
    if identity.library_id != report.library_id {
        return Err(LegacyMigrationError::Invalid(
            "published settings/database identity mismatch".to_string(),
        ));
    }
    Ok(report)
}

pub fn restore_legacy_backup(
    config_path: &Path,
    library_path: &Path,
) -> Result<LegacyRestoreReport, LegacyMigrationError> {
    let backup = config_path.with_file_name("config.json.pre-v8-backup");
    if !backup.is_file() {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy backup '{}' is missing",
            backup.display()
        )));
    }
    let version = config_schema_version(&backup)?;
    if version > crate::config::LAST_LEGACY_SCHEMA_VERSION {
        return Err(LegacyMigrationError::Invalid(format!(
            "legacy backup schema {version} is not restorable"
        )));
    }

    let suffix = uuid::Uuid::new_v4();
    let archived_config = config_path
        .exists()
        .then(|| config_path.with_file_name(format!("config.json.pre-restore-{suffix}")));
    let archived_database = library_path
        .exists()
        .then(|| library_path.with_file_name(format!("library.sqlite3.pre-restore-{suffix}")));
    let candidate = config_path.with_file_name(format!(".config.json.restoring-{suffix}"));

    let result = (|| -> Result<(), LegacyMigrationError> {
        let mut reader = BufReader::new(fs::File::open(&backup)?);
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&candidate)?;
        let mut writer = BufWriter::new(&file);
        std::io::copy(&mut reader, &mut writer)?;
        writer.flush()?;
        drop(writer);
        file.sync_all()?;

        if let Some(path) = &archived_database {
            fs::rename(library_path, path)?;
        }
        if let Some(path) = &archived_config {
            if let Err(error) = fs::rename(config_path, path) {
                if let Some(database) = &archived_database {
                    let _ = fs::rename(database, library_path);
                }
                return Err(error.into());
            }
        }
        if let Err(error) = fs::rename(&candidate, config_path) {
            if let Some(path) = &archived_config {
                let _ = fs::rename(path, config_path);
            }
            if let Some(path) = &archived_database {
                let _ = fs::rename(path, library_path);
            }
            return Err(error.into());
        }
        fs::set_permissions(config_path, fs::Permissions::from_mode(0o600))?;
        if let Some(parent) = config_path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(candidate);
    }
    result?;
    Ok(LegacyRestoreReport {
        archived_config,
        archived_database,
    })
}

pub fn initialize_empty_library(path: &Path, library_id: &str) -> Result<(), LegacyMigrationError> {
    if path.exists() {
        return Err(LegacyMigrationError::Invalid(format!(
            "refusing to replace existing database '{}'",
            path.display()
        )));
    }
    let store = LibraryStore::open(path.to_path_buf())?;
    drop(store);
    let connection = Connection::open(path)?;
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
    if let Some(parent) = path.parent() {
        fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
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
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
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
                .manual_tabs(0)
                .recv()
                .expect("load migrated tabs")
                .total,
            2
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
    fn pre_v8_backup_is_independent_from_in_place_source_writes() {
        let directory =
            std::env::temp_dir().join(format!("lsb-backup-copy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let original = br#"{"schema_version":7,"settings":{}}"#;
        fs::write(&source, original).expect("write source");

        ensure_backup(
            &source,
            &sha256(&source).expect("hash source"),
            &noop_observer(),
        )
        .expect("create backup");
        fs::write(
            &source,
            br#"{"schema_version":7,"settings":{"theme":"light"}}"#,
        )
        .expect("rewrite source in place");

        assert_eq!(
            fs::read(directory.join("config.json.pre-v8-backup")).expect("read backup"),
            original
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn restart_removes_stale_migration_candidates_before_retry() {
        let directory =
            std::env::temp_dir().join(format!("lsb-stale-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let stale_backup = directory.join(".config.json.pre-v8-backup.interrupted");
        let stale_database = directory.join(".library.sqlite3.importing.interrupted");
        let mut config = Config::default();
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        fs::write(&stale_backup, b"partial backup").expect("write stale backup candidate");
        fs::write(&stale_database, b"partial database").expect("write stale database candidate");

        migrate_legacy_database(&source, &destination).expect("retry migration");

        assert!(!stale_backup.exists());
        assert!(!stale_database.exists());
        assert!(destination.exists());
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn read_only_destination_preserves_legacy_source_and_publishes_nothing() {
        let directory =
            std::env::temp_dir().join(format!("lsb-read-only-migration-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        let source_before = fs::read(&source).expect("read legacy source");
        ensure_backup(
            &source,
            &sha256(&source).expect("hash source"),
            &noop_observer(),
        )
        .expect("create backup");
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o500))
            .expect("make destination read-only");

        let result = migrate_legacy_database(&source, &destination);

        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))
            .expect("restore destination permissions");
        assert!(result.is_err());
        assert_eq!(
            fs::read(&source).expect("read preserved source"),
            source_before
        );
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
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
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
        for version in 0..=crate::config::LAST_LEGACY_SCHEMA_VERSION {
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

    #[test]
    fn authentic_v2_schema_6_fixture_preserves_library_settings_and_hotkeys() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema6-fixture-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        fs::write(
            &source,
            include_bytes!("../tests/fixtures/config-v2.0-schema6.json"),
        )
        .expect("write historical fixture");

        let report = migrate_legacy_config(&source, &destination).expect("migrate fixture");

        assert_eq!(report.sounds, 1);
        assert_eq!(report.roots, 1);
        assert_eq!(report.manual_tabs, 1);
        assert_eq!(report.manual_memberships, 1);
        assert_eq!(report.hotkeys, 8);
        assert_eq!(report.settings.theme, crate::config::Theme::Light);
        assert_eq!(report.settings.local_volume, 41);
        let store = LibraryStore::open(destination).expect("open migrated database");
        let sound = store
            .sound_by_id("sound-distinctive")
            .recv()
            .expect("load fixture sound")
            .expect("fixture sound exists");
        assert_eq!(sound.name, "Upgrade fixture");
        assert_eq!(sound.duration_ms, Some(4_321));
        drop(store);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn authentic_v2_1_schema_7_fixture_preserves_generated_folder_state() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema7-fixture-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        fs::write(
            &source,
            include_bytes!("../tests/fixtures/config-v2.1-schema7.json"),
        )
        .expect("write historical fixture");

        let report = migrate_legacy_config(&source, &destination).expect("migrate fixture");

        assert_eq!(report.sounds, 1);
        assert_eq!(report.roots, 1);
        assert_eq!(report.manual_tabs, 1);
        assert_eq!(report.manual_memberships, 1);
        assert_eq!(report.generated_tabs_deferred, 1);
        assert_eq!(report.hotkeys, 2);
        let store = LibraryStore::open(destination).expect("open migrated database");
        assert_eq!(
            store
                .manual_tabs(0)
                .recv()
                .expect("load migrated tabs")
                .total,
            2
        );
        drop(store);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn full_cutover_publishes_small_schema_8_settings_with_matching_identity() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema8-cutover-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
        config.sound_folders.push("/music".to_string());
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        config.settings.control_hotkeys.stop_all = Some("Ctrl+KeyS".to_string());
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();

        let report = migrate_legacy_config(&source, &destination).expect("complete migration");
        let persisted: serde_json::Value =
            serde_json::from_reader(fs::File::open(&source).unwrap()).unwrap();
        assert_eq!(persisted["schema_version"], serde_json::json!(8));
        assert!(persisted.get("library_id").is_none());
        assert!(persisted.get("sounds").is_none());
        assert!(persisted.get("sound_folders").is_none());
        assert!(persisted["settings"].get("control_hotkeys").is_none());
        assert_eq!(
            database_identity(&destination).unwrap().library_id,
            report.library_id
        );
        assert_eq!(report.hotkeys, 1);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn restart_finishes_settings_switch_after_database_publication() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema8-resume-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();

        let report = migrate_legacy_database(&source, &destination).expect("publish database");
        assert_eq!(config_schema_version(&source).unwrap(), 7);
        let identity = complete_legacy_settings_cutover(&source, &destination)
            .expect("resume settings switch");
        assert_eq!(identity.library_id, report.library_id);
        assert_eq!(config_schema_version(&source).unwrap(), 8);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn controlled_migration_reports_ordered_progress() {
        let directory =
            std::env::temp_dir().join(format!("lsb-migration-progress-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        let progress = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = Arc::clone(&progress);

        migrate_legacy_config_controlled(
            &source,
            &destination,
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |update| captured.lock().unwrap().push(update)),
        )
        .expect("run controlled migration");

        assert_eq!(
            *progress.lock().unwrap(),
            [
                LegacyMigrationProgress::BackingUp,
                LegacyMigrationProgress::Importing,
                LegacyMigrationProgress::Verifying,
                LegacyMigrationProgress::PublishingDatabase,
                LegacyMigrationProgress::PublishingSettings,
                LegacyMigrationProgress::Complete,
            ]
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn controlled_migration_cancels_before_publication_and_retries_cleanly() {
        let directory =
            std::env::temp_dir().join(format!("lsb-migration-cancel-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        let source_before = fs::read(&source).expect("read source");
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_from_progress = Arc::clone(&cancelled);

        let result = migrate_legacy_config_controlled(
            &source,
            &destination,
            cancelled,
            Arc::new(move |progress| {
                if progress == LegacyMigrationProgress::Importing {
                    cancel_from_progress.store(true, Ordering::Relaxed);
                }
            }),
        );

        assert!(matches!(result, Err(LegacyMigrationError::Cancelled)));
        assert_eq!(fs::read(&source).unwrap(), source_before);
        assert!(!destination.exists());
        migrate_legacy_config(&source, &destination).expect("retry cancelled migration");
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn every_durable_migration_boundary_is_restart_safe() {
        for boundary in LegacyMigrationBoundary::ALL {
            let directory = std::env::temp_dir().join(format!(
                "lsb-migration-boundary-{boundary:?}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let source = directory.join("config.json");
            let destination = directory.join("library.sqlite3");
            let mut config = Config::default();
            config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
            config.sounds.push(Sound::new(
                "Tone".to_string(),
                "/music/tone.wav".to_string(),
            ));
            serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
            let source_before = fs::read(&source).expect("read legacy source");

            let result = migrate_legacy_config_observed(
                &source,
                &destination,
                Arc::new(move |current| {
                    if current == boundary {
                        return Err(LegacyMigrationError::Io(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            format!("interrupted at {current:?}"),
                        )));
                    }
                    Ok(())
                }),
            );

            assert!(result.is_err(), "{boundary:?} did not interrupt");
            assert!(
                !directory
                    .read_dir()
                    .expect("read migration directory")
                    .any(|entry| {
                        let name = entry
                            .expect("read migration entry")
                            .file_name()
                            .to_string_lossy()
                            .into_owned();
                        name.starts_with(".library.sqlite3.importing.")
                            || name.starts_with(".config.json.pre-v8-backup.")
                            || name.starts_with("config.json.tmp.")
                    }),
                "{boundary:?} left a disposable candidate"
            );

            if destination.exists() {
                database_identity(&destination).expect("published database remains valid");
                if config_schema_version(&source).expect("read config schema")
                    <= crate::config::LAST_LEGACY_SCHEMA_VERSION
                {
                    complete_legacy_settings_cutover(&source, &destination)
                        .expect("restart completes settings cutover");
                }
                assert_eq!(
                    config_schema_version(&source).expect("read recovered schema"),
                    crate::config::CURRENT_SCHEMA_VERSION
                );
            } else {
                assert_eq!(
                    fs::read(&source).expect("read preserved legacy source"),
                    source_before
                );
                migrate_legacy_config(&source, &destination)
                    .expect("restart retries unpublished migration");
            }

            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn abrupt_exit_at_every_durable_boundary_recovers_on_restart() {
        for boundary in LegacyMigrationBoundary::ALL {
            let directory = std::env::temp_dir().join(format!(
                "lsb-migration-crash-{boundary:?}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let source = directory.join("config.json");
            let destination = directory.join("library.sqlite3");
            let mut config = Config::default();
            config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
            config.sounds.push(Sound::new(
                "Tone".to_string(),
                "/music/tone.wav".to_string(),
            ));
            serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();

            let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = migrate_legacy_config_observed(
                    &source,
                    &destination,
                    Arc::new(move |current| {
                        if current == boundary {
                            panic!("simulated process exit at {current:?}");
                        }
                        Ok(())
                    }),
                );
            }));
            assert!(interrupted.is_err(), "{boundary:?} did not stop");

            if destination.exists() {
                database_identity(&destination).expect("published database remains valid");
                if config_schema_version(&source).unwrap()
                    <= crate::config::LAST_LEGACY_SCHEMA_VERSION
                {
                    complete_legacy_settings_cutover(&source, &destination)
                        .expect("restart completes settings cutover");
                }
            } else {
                migrate_legacy_config(&source, &destination)
                    .expect("restart retries unpublished migration");
            }
            assert_eq!(
                config_schema_version(&source).unwrap(),
                crate::config::CURRENT_SCHEMA_VERSION
            );
            database_identity(&destination).expect("recovered database remains valid");
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn disk_full_at_each_write_phase_preserves_a_restartable_source() {
        for boundary in LegacyMigrationBoundary::WRITE_PHASES {
            let directory = std::env::temp_dir().join(format!(
                "lsb-migration-disk-full-{boundary:?}-{}",
                uuid::Uuid::new_v4()
            ));
            fs::create_dir_all(&directory).expect("create test directory");
            let source = directory.join("config.json");
            let destination = directory.join("library.sqlite3");
            let mut config = Config::default();
            config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
            serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
            let source_before = fs::read(&source).expect("read legacy source");

            let result = migrate_legacy_config_observed(
                &source,
                &destination,
                Arc::new(move |current| {
                    if current == boundary {
                        return Err(LegacyMigrationError::Io(std::io::Error::from_raw_os_error(
                            28,
                        )));
                    }
                    Ok(())
                }),
            );

            let error = result.expect_err("disk-full injection must fail");
            let LegacyMigrationError::Io(error) = error else {
                panic!("expected disk-full I/O error, got {error}");
            };
            assert_eq!(error.raw_os_error(), Some(28));
            assert_eq!(fs::read(&source).unwrap(), source_before);
            if boundary == LegacyMigrationBoundary::BeforeSettingsWrite {
                database_identity(&destination).expect("published database remains valid");
                complete_legacy_settings_cutover(&source, &destination)
                    .expect("restart completes settings cutover");
            } else {
                assert!(!destination.exists());
                migrate_legacy_config(&source, &destination)
                    .expect("retry succeeds after storage is available");
            }
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn restart_refuses_a_ready_database_for_a_changed_legacy_source() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema8-mismatch-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        migrate_legacy_database(&source, &destination).expect("publish database");
        fs::write(&source, b"{\"schema_version\":7,\"settings\":{}}")
            .expect("replace legacy source");

        assert!(complete_legacy_settings_cutover(&source, &destination).is_err());
        assert_eq!(config_schema_version(&source).unwrap(), 7);
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn ready_database_with_wrong_application_id_is_rejected() {
        let directory =
            std::env::temp_dir().join(format!("lsb-wrong-app-id-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let destination = directory.join("library.sqlite3");
        initialize_empty_library(&destination, "library").expect("create database");
        let connection = Connection::open(&destination).expect("open database");
        connection
            .pragma_update(None, "application_id", 7_i64)
            .expect("write wrong application id");
        drop(connection);

        let error = database_identity(&destination).expect_err("wrong file format must fail");

        assert!(error.to_string().contains("application id"));
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn corrupt_and_wrong_format_databases_fail_closed_without_replacement() {
        for (name, bytes) in [
            ("wrong-format", b"this is not sqlite".as_slice()),
            (
                "corrupt",
                b"SQLite format 3\0deliberately truncated".as_slice(),
            ),
        ] {
            let directory =
                std::env::temp_dir().join(format!("lsb-{name}-database-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&directory).expect("create test directory");
            let destination = directory.join("library.sqlite3");
            fs::write(&destination, bytes).expect("write invalid database");
            let before = fs::read(&destination).expect("read invalid database");

            database_identity(&destination).expect_err("invalid database must fail");

            assert_eq!(
                fs::read(&destination).expect("read preserved invalid database"),
                before
            );
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn unsupported_and_non_ready_databases_fail_closed() {
        for (name, mutate) in [
            ("unsupported-schema", "PRAGMA user_version = 99;"),
            (
                "not-ready",
                "DELETE FROM meta WHERE key = 'database_ready';",
            ),
        ] {
            let directory =
                std::env::temp_dir().join(format!("lsb-{name}-database-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&directory).expect("create test directory");
            let destination = directory.join("library.sqlite3");
            initialize_empty_library(&destination, "library").expect("create database");
            let connection = Connection::open(&destination).expect("open database");
            connection.execute_batch(mutate).expect("mutate database");
            drop(connection);
            let before = fs::read(&destination).expect("read invalid database");

            database_identity(&destination).expect_err("invalid database must fail");

            assert_eq!(fs::read(&destination).unwrap(), before);
            fs::remove_dir_all(directory).expect("remove test directory");
        }
    }

    #[test]
    fn ready_unmarked_database_is_adopted_once() {
        let directory =
            std::env::temp_dir().join(format!("lsb-zero-app-id-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let destination = directory.join("library.sqlite3");
        initialize_empty_library(&destination, "library").expect("create database");
        let connection = Connection::open(&destination).expect("open database");
        connection
            .pragma_update(None, "application_id", 0_i64)
            .expect("clear application id");
        drop(connection);

        database_identity(&destination).expect("adopt valid interim database");

        let connection = Connection::open(&destination).expect("reopen database");
        let application_id: i64 = connection
            .query_row("PRAGMA application_id", [], |row| row.get(0))
            .expect("read application id");
        assert_eq!(
            application_id,
            crate::library_store::DATABASE_APPLICATION_ID
        );
        fs::remove_dir_all(directory).expect("remove test directory");
    }

    #[test]
    fn restore_keeps_current_files_and_reinstates_the_legacy_backup() {
        let directory =
            std::env::temp_dir().join(format!("lsb-schema8-restore-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).expect("create test directory");
        let source = directory.join("config.json");
        let destination = directory.join("library.sqlite3");
        let mut config = Config::default();
        config.schema_version = crate::config::LAST_LEGACY_SCHEMA_VERSION;
        config.sounds.push(Sound::new(
            "Tone".to_string(),
            "/music/tone.wav".to_string(),
        ));
        serde_json::to_writer(fs::File::create(&source).unwrap(), &config).unwrap();
        let legacy_bytes = fs::read(&source).unwrap();
        migrate_legacy_config(&source, &destination).expect("complete migration");

        let restored = restore_legacy_backup(&source, &destination).expect("restore backup");

        assert_eq!(fs::read(&source).unwrap(), legacy_bytes);
        assert!(!destination.exists());
        assert!(restored.archived_config.unwrap().is_file());
        assert!(restored.archived_database.unwrap().is_file());
        assert_eq!(config_schema_version(&source).unwrap(), 7);
        fs::remove_dir_all(directory).expect("remove test directory");
    }
}
