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
