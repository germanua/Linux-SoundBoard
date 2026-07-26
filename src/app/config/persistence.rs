use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::defaults::{config_dir_name, CONFIG_FILE_NAME};
use crate::config::{Config, LoudnessAnalysisState, SoundTab};

static SAVE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const SCHEMA_6_BACKUP_FILE_NAME: &str = "config.json.pre-v6-backup";

fn save_temp_path(path: &Path) -> PathBuf {
    let sequence = SAVE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    path.with_file_name(format!(
        "{}.tmp.{}.{}",
        CONFIG_FILE_NAME,
        std::process::id(),
        sequence
    ))
}

fn schema_6_backup_path(path: &Path) -> PathBuf {
    path.with_file_name(SCHEMA_6_BACKUP_FILE_NAME)
}

fn ensure_schema_6_backup(path: &Path, original: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let backup_path = schema_6_backup_path(path);
    if backup_path.exists() {
        if fs::read(&backup_path)? == original {
            fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600))?;
            return Ok(());
        }
        return Err(format!(
            "Refusing to replace conflicting pre-v6 backup '{}'",
            backup_path.display()
        )
        .into());
    }

    let tmp_path = backup_path.with_file_name(format!(
        "{SCHEMA_6_BACKUP_FILE_NAME}.tmp.{}.{}",
        std::process::id(),
        SAVE_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let mut tmp = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp_path)?;
        tmp.write_all(original)?;
        tmp.sync_all()?;

        match fs::hard_link(&tmp_path, &backup_path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::read(&backup_path)? != original {
                    return Err(format!(
                        "Refusing to replace conflicting pre-v6 backup '{}'",
                        backup_path.display()
                    )
                    .into());
                }
            }
            Err(err) => return Err(Box::new(err)),
        }
        fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600))?;
        if let Some(parent) = backup_path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(tmp_path);
    result
}

impl Config {
    pub fn config_path() -> PathBuf {
        let config_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(config_dir_name());

        let _ = fs::create_dir_all(&config_dir);
        config_dir.join(CONFIG_FILE_NAME)
    }

    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        Self::load_from_path(&Self::config_path())
    }

    pub fn load_runtime_settings() -> Result<Self, Box<dyn std::error::Error>> {
        let path = Self::config_path();
        Self::load_runtime_settings_from_path(&path)
    }

    pub(crate) fn load_runtime_settings_from_path(
        path: &Path,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let version = crate::legacy_migration::config_schema_version(path)?;
        if version <= crate::config::LAST_LEGACY_SCHEMA_VERSION {
            let config = Self {
                settings: crate::legacy_migration::read_legacy_runtime_settings(path)?,
                ..Self::default()
            };
            return Ok(config);
        }
        Self::load_from_path(path)
    }

    pub(crate) fn load_from_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        if path.exists() {
            let content = fs::read(path)?;
            let raw: serde_json::Value = serde_json::from_slice(&content)?;

            let version = raw
                .get("schema_version")
                .and_then(|v| v.as_u64())
                .map(u32::try_from)
                .transpose()
                .map_err(|_| "configuration schema version exceeds the supported integer range")?
                .unwrap_or(0);

            let config_value = if version == crate::config::migration::CURRENT_SCHEMA_VERSION {
                raw
            } else {
                crate::config::migration::run_migrations(raw, version)?
            };

            let mut config: Config = serde_json::from_value(config_value)?;
            config.sanitize_for_persistence();
            config.persistence_path = Some(path.to_path_buf());
            if version == 6 {
                ensure_schema_6_backup(path, &content)?;
            }
            Ok(config)
        } else {
            Ok(Self {
                persistence_path: Some(path.to_path_buf()),
                ..Self::default()
            })
        }
    }

    pub fn save(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let path = self.persistence_path.clone().ok_or_else(|| {
            std::io::Error::other(
                "configuration has no persistence path; load it or save to an explicit path first",
            )
        })?;
        self.save_to_path(&path)
    }

    pub(crate) fn save_to_path(&mut self, path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.sanitize_for_persistence();
        let tmp_path = save_temp_path(path);

        let write_result = (|| -> Result<(), Box<dyn std::error::Error>> {
            let tmp_file = fs::File::create(&tmp_path)?;
            {
                let mut writer = std::io::BufWriter::new(&tmp_file);
                if self.schema_version >= crate::config::CURRENT_SCHEMA_VERSION {
                    let mut persisted = serde_json::to_value(&*self)?;
                    let object = persisted.as_object_mut().ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "configuration did not serialize as an object",
                        )
                    })?;
                    object.remove("library_id");
                    object.remove("sound_folders");
                    object.remove("sounds");
                    object.remove("tabs");
                    if let Some(settings) = object
                        .get_mut("settings")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        settings.remove("control_hotkeys");
                    }
                    serde_json::to_writer_pretty(&mut writer, &persisted)?;
                } else {
                    serde_json::to_writer_pretty(&mut writer, &*self)?;
                }
                writer.flush()?;
            }
            tmp_file.sync_all()?;
            Ok(())
        })();

        if let Err(err) = write_result {
            let _ = fs::remove_file(&tmp_path);
            return Err(err);
        }

        if let Err(err) = fs::rename(&tmp_path, path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(Box::new(err));
        }
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::File::open(parent).and_then(|dir| dir.sync_all()) {
                return Err(Box::new(std::io::Error::other(format!(
                    "Configuration was written but its directory could not be synced: {err}"
                ))));
            }
        }
        self.persistence_path = Some(path.to_path_buf());
        Ok(())
    }

    pub fn add_sound_folder(&mut self, folder: String) {
        if !self.sound_folders.contains(&folder) {
            log::info!("Config: Adding folder: {}", folder);
            self.sound_folders.push(folder);
            log::info!("Config: Total folders now: {}", self.sound_folders.len());
        } else {
            log::info!("Config: Folder already exists: {}", folder);
        }
    }

    pub fn remove_sound_folder(&mut self, folder: &str) {
        log::info!("Config: Removing folder: {}", folder);
        let before = self.sound_folders.len();
        self.sound_folders.retain(|f| f != folder);
        let after = self.sound_folders.len();
        log::info!("Config: Folders before: {}, after: {}", before, after);
    }

    pub fn add_sound(&mut self, sound: crate::config::Sound) {
        if !self.sounds.iter().any(|s| s.path == sound.path) {
            self.sounds.push(sound);
        }
    }

    pub fn remove_sound(&mut self, id: &str) {
        self.remove_sounds(&[id.to_string()]);
    }

    pub fn remove_sounds(&mut self, ids: &[String]) {
        if ids.is_empty() {
            return;
        }

        let remove_set: HashSet<&str> = ids.iter().map(String::as_str).collect();
        self.sounds
            .retain(|sound| !remove_set.contains(sound.id.as_str()));
        for tab in &mut self.tabs {
            tab.sound_ids
                .retain(|sound_id| !remove_set.contains(sound_id.as_str()));
        }
    }

    pub fn get_sound(&self, id: &str) -> Option<&crate::config::Sound> {
        self.sounds.iter().find(|s| s.id == id)
    }

    pub fn get_sound_mut(&mut self, id: &str) -> Option<&mut crate::config::Sound> {
        self.sounds.iter_mut().find(|s| s.id == id)
    }

    pub fn set_hotkey(&mut self, id: &str, hotkey: Option<String>) {
        if let Some(sound) = self.get_sound_mut(id) {
            sound.hotkey = hotkey;
        }
    }

    pub fn set_sound_name(&mut self, id: &str, name: String) {
        if let Some(sound) = self.get_sound_mut(id) {
            sound.name = name;
        }
    }

    pub fn sanitize_for_persistence(&mut self) {
        for sound in &mut self.sounds {
            if sound.source_path.as_deref() == Some(sound.path.as_str()) {
                sound.source_path = None;
            }
            if sound
                .loudness_source_fingerprint
                .as_ref()
                .is_some_and(|fingerprint| fingerprint.trim().is_empty())
            {
                sound.loudness_source_fingerprint = None;
            }
            if matches!(sound.loudness_lufs, Some(v) if !v.is_finite()) {
                log::warn!(
                    "Dropping non-finite loudness for sound '{}' [{}]",
                    sound.name,
                    sound.path
                );
                sound.loudness_lufs = None;
                sound.loudness_analysis_state = LoudnessAnalysisState::Unavailable;
            }

            match sound.loudness_confidence {
                Some(confidence) if !confidence.is_finite() => {
                    sound.loudness_confidence = None;
                }
                Some(confidence) => {
                    sound.loudness_confidence = Some(confidence.clamp(0.0, 1.0));
                }
                None => {}
            }

            if sound.loudness_lufs.is_some() {
                if matches!(
                    sound.loudness_analysis_state,
                    LoudnessAnalysisState::Pending | LoudnessAnalysisState::Unavailable
                ) {
                    // Backward compatibility: old configs did not store loudness state.
                    sound.loudness_analysis_state = LoudnessAnalysisState::Refined;
                }
                if sound.loudness_confidence.is_none() {
                    sound.loudness_confidence = Some(1.0);
                }
            } else if matches!(
                sound.loudness_analysis_state,
                LoudnessAnalysisState::Estimated | LoudnessAnalysisState::Refined
            ) {
                sound.loudness_analysis_state = LoudnessAnalysisState::Pending;
                sound.loudness_confidence = None;
            }

            if sound.loudness_analysis_state == LoudnessAnalysisState::Unavailable {
                sound.loudness_confidence = None;
            }
        }

        self.settings.normalize_for_persistence();
    }

    pub fn create_tab(&mut self, name: String) -> SoundTab {
        let order = self.tabs.iter().map(|t| t.order).max().unwrap_or(0) + 1;
        let tab = SoundTab::new(name, order);
        self.tabs.push(tab.clone());
        tab
    }

    pub fn rename_tab(&mut self, id: &str, name: String) -> bool {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.name = name;
            true
        } else {
            false
        }
    }

    pub fn delete_tab(&mut self, id: &str) -> bool {
        let len_before = self.tabs.len();
        self.tabs.retain(|t| t.id != id);
        self.tabs.len() < len_before
    }

    pub fn get_tab(&self, id: &str) -> Option<&SoundTab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn get_tab_mut(&mut self, id: &str) -> Option<&mut SoundTab> {
        self.tabs.iter_mut().find(|t| t.id == id)
    }

    pub fn add_sounds_to_tab(&mut self, tab_id: &str, sound_ids: Vec<String>) -> bool {
        if let Some(tab) = self.get_tab_mut(tab_id) {
            let mut existing_ids = tab.sound_ids.iter().cloned().collect::<HashSet<_>>();
            for sound_id in sound_ids {
                if existing_ids.insert(sound_id.clone()) {
                    tab.sound_ids.push(sound_id);
                }
            }
            true
        } else {
            false
        }
    }

    pub fn remove_sound_from_tab(&mut self, tab_id: &str, sound_id: &str) -> bool {
        if let Some(tab) = self.get_tab_mut(tab_id) {
            let len_before = tab.sound_ids.len();
            tab.sound_ids.retain(|id| id != sound_id);
            tab.sound_ids.len() < len_before
        } else {
            false
        }
    }

    pub fn remove_sounds_from_tab(&mut self, tab_id: &str, sound_ids: &[String]) -> bool {
        let Some(tab) = self.get_tab_mut(tab_id) else {
            return false;
        };

        if sound_ids.is_empty() {
            return true;
        }

        let remove_set: HashSet<&str> = sound_ids.iter().map(String::as_str).collect();
        tab.sound_ids
            .retain(|sound_id| !remove_set.contains(sound_id.as_str()));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Sound;
    const SCHEMA_6_FIXTURE: &[u8] = include_bytes!("../../tests/fixtures/config-v2.0-schema6.json");

    fn test_dir() -> PathBuf {
        std::env::temp_dir().join(format!("lsb-config-test-{}", uuid::Uuid::new_v4()))
    }

    fn backup_path(path: &Path) -> PathBuf {
        schema_6_backup_path(path)
    }

    #[test]
    #[ignore = "release-scale gate for large manual-tab imports"]
    #[allow(clippy::print_stderr)]
    fn benchmark_large_tab_batch_membership() {
        let mut config = Config::default();
        let mut tab = SoundTab::new("Large manual tab".to_string(), 0);
        tab.id = "large-manual".to_string();
        config.tabs.push(tab);
        let ids = (0..20_000)
            .map(|index| format!("sound-{index:06}"))
            .collect::<Vec<_>>();

        let started = std::time::Instant::now();
        assert!(config.add_sounds_to_tab("large-manual", ids));
        let elapsed = started.elapsed();

        eprintln!("large-tab memberships=20000 add_ms={}", elapsed.as_millis());
        assert_eq!(config.tabs[0].sound_ids.len(), 20_000);
        assert!(elapsed <= std::time::Duration::from_millis(100));
    }

    #[test]
    fn schema_6_load_creates_exact_private_backup_and_preserves_user_fields() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create config directory");
        let path = dir.join("config.json");
        fs::write(&path, SCHEMA_6_FIXTURE).expect("write schema 6 fixture");

        let config = Config::load_from_path(&path).expect("migrate schema 6 fixture");

        assert_eq!(config.schema_version, crate::config::CURRENT_SCHEMA_VERSION);
        assert_eq!(config.sound_folders, ["/home/test/Sound Library"]);
        assert_eq!(config.sounds[0].id, "sound-distinctive");
        assert_eq!(config.sounds[0].name, "Upgrade fixture");
        assert_eq!(
            config.sounds[0].source_path.as_deref(),
            Some("/home/test/source/upgrade.flac")
        );
        assert_eq!(config.sounds[0].hotkey.as_deref(), Some("Ctrl+Alt+9"));
        assert_eq!(config.sounds[0].duration_ms, Some(4321));
        assert_eq!(config.sounds[0].volume, 37);
        assert!(!config.sounds[0].enabled);
        assert_eq!(config.tabs[0].id, "tab-distinctive");
        assert_eq!(config.tabs[0].name, "Upgrade tab");
        assert_eq!(config.tabs[0].order, 7);
        assert_eq!(config.tabs[0].folder_binding, None);
        assert_eq!(config.tabs[0].sound_ids, ["sound-distinctive"]);
        assert_eq!(
            config.settings.mic_source.as_deref(),
            Some("easyeffects_source")
        );
        assert_eq!(config.settings.local_volume, 41);
        assert!(config.settings.local_mute);
        assert_eq!(config.settings.mic_volume, 63);
        assert!(config.settings.mic_passthrough);
        assert_eq!(config.settings.default_source_mode.as_str(), "manual");
        assert_eq!(config.settings.mic_latency_profile.as_str(), "low");
        assert_eq!(config.settings.excluded_apps, ["OBS"]);
        assert!(config.settings.skip_delete_confirm);
        assert_eq!(
            config.settings.control_hotkeys.play_pause.as_deref(),
            Some("Ctrl+Shift+P")
        );
        assert_eq!(
            config.settings.control_hotkeys.stop_all.as_deref(),
            Some("Ctrl+Shift+S")
        );
        assert_eq!(
            config.settings.control_hotkeys.previous_sound.as_deref(),
            Some("Ctrl+Shift+Left")
        );
        assert_eq!(
            config.settings.control_hotkeys.next_sound.as_deref(),
            Some("Ctrl+Shift+Right")
        );
        assert_eq!(
            config.settings.control_hotkeys.mute_headphones.as_deref(),
            Some("Ctrl+Shift+H")
        );
        assert_eq!(
            config.settings.control_hotkeys.mute_real_mic.as_deref(),
            Some("Ctrl+Shift+R")
        );
        assert_eq!(
            config.settings.control_hotkeys.cycle_play_mode.as_deref(),
            Some("Ctrl+Shift+C")
        );
        assert!(config.settings.auto_gain);
        assert_eq!(config.settings.auto_gain_mode.as_str(), "dynamic");
        assert_eq!(config.settings.auto_gain_target_lufs, -16.0);
        assert_eq!(config.settings.auto_gain_apply_to.as_str(), "mic_only");
        assert_eq!(config.settings.auto_gain_lookahead_ms, 44);
        assert_eq!(config.settings.auto_gain_attack_ms, 8);
        assert_eq!(config.settings.auto_gain_release_ms, 222);
        assert_eq!(config.settings.play_mode.as_str(), "continue");
        assert_eq!(config.settings.list_style.as_str(), "card");
        assert_eq!(
            fs::read(backup_path(&path)).expect("read backup"),
            SCHEMA_6_FIXTURE
        );
        assert_eq!(
            fs::metadata(backup_path(&path))
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        fs::remove_dir_all(dir).expect("cleanup config directory");
    }

    #[test]
    fn runtime_settings_loader_ignores_legacy_library_arrays() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create config directory");
        let path = dir.join("config.json");
        let mut legacy = Config {
            schema_version: crate::config::LAST_LEGACY_SCHEMA_VERSION,
            ..Config::default()
        };
        legacy.settings.local_volume = 37;
        legacy.sounds = (0..2_048)
            .map(|index| {
                Sound::new(
                    format!("Sound {index}"),
                    format!("/music/sound-{index}.wav"),
                )
            })
            .collect();
        serde_json::to_writer(fs::File::create(&path).unwrap(), &legacy).unwrap();

        let runtime =
            Config::load_runtime_settings_from_path(&path).expect("load bounded runtime settings");

        assert_eq!(runtime.settings.local_volume, 37);
        assert!(runtime.sounds.is_empty());
        assert!(runtime.tabs.is_empty());
        assert!(runtime.sound_folders.is_empty());
        fs::remove_dir_all(dir).expect("cleanup config directory");
    }

    #[test]
    fn identical_schema_6_backup_is_idempotent() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create config directory");
        let path = dir.join("config.json");
        let backup = backup_path(&path);
        fs::write(&path, SCHEMA_6_FIXTURE).expect("write schema 6 fixture");
        fs::write(&backup, SCHEMA_6_FIXTURE).expect("write existing backup");

        Config::load_from_path(&path).expect("load with identical backup");

        assert_eq!(fs::read(&path).expect("read config"), SCHEMA_6_FIXTURE);
        assert_eq!(fs::read(&backup).expect("read backup"), SCHEMA_6_FIXTURE);
        fs::remove_dir_all(dir).expect("cleanup config directory");
    }

    #[test]
    fn conflicting_schema_6_backup_fails_without_modifying_either_file() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create config directory");
        let path = dir.join("config.json");
        let backup = backup_path(&path);
        let conflicting = b"existing backup from another configuration";
        fs::write(&path, SCHEMA_6_FIXTURE).expect("write schema 6 fixture");
        fs::write(&backup, conflicting).expect("write conflicting backup");

        let error = Config::load_from_path(&path).expect_err("conflicting backup must fail");

        assert!(error.to_string().contains("pre-v6 backup"));
        assert_eq!(fs::read(&path).expect("read config"), SCHEMA_6_FIXTURE);
        assert_eq!(fs::read(&backup).expect("read backup"), conflicting);
        fs::remove_dir_all(dir).expect("cleanup config directory");
    }

    #[test]
    fn malformed_and_future_configs_remain_byte_for_byte_unchanged() {
        for bytes in [
            b"{ definitely not json".as_slice(),
            br#"{"schema_version":99,"sound_folders":[],"sounds":[],"tabs":[],"settings":{}}"#,
        ] {
            let dir = test_dir();
            fs::create_dir_all(&dir).expect("create config directory");
            let path = dir.join("config.json");
            fs::write(&path, bytes).expect("write invalid config");

            assert!(Config::load_from_path(&path).is_err());
            assert_eq!(fs::read(&path).expect("read unchanged config"), bytes);
            assert!(!backup_path(&path).exists());
            fs::remove_dir_all(dir).expect("cleanup config directory");
        }
    }

    #[test]
    fn oversized_future_schema_remains_byte_for_byte_unchanged() {
        let bytes = std::str::from_utf8(SCHEMA_6_FIXTURE)
            .expect("schema 6 fixture is UTF-8")
            .replace("\"schema_version\": 6", "\"schema_version\": 4294967303");
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create config directory");
        let path = dir.join("config.json");
        fs::write(&path, &bytes).expect("write future config");

        assert!(Config::load_from_path(&path).is_err());
        assert_eq!(
            fs::read(&path).expect("read future config"),
            bytes.as_bytes()
        );
        assert!(!backup_path(&path).exists());
    }

    #[test]
    fn save_temp_path_is_unique_per_call() {
        let path = PathBuf::from("/tmp/linux-soundboard/config.json");

        let first = save_temp_path(&path);
        let second = save_temp_path(&path);

        assert_ne!(first, second);
        assert_eq!(first.parent(), path.parent());
        assert_eq!(second.parent(), path.parent());
        assert!(first
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("config.json.tmp."));
    }

    #[test]
    fn default_config_cannot_save_to_an_implicit_user_path() {
        let mut config = Config::default();

        let error = config
            .save()
            .expect_err("an unbound config must not discover a user path");

        assert!(error.to_string().contains("persistence path"));
    }

    #[test]
    fn schema_8_save_contains_only_settings() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create config directory");
        let path = dir.join("config.json");
        let mut config = Config::default();
        config.sound_folders.push("/tmp/sounds".to_string());
        config.settings.control_hotkeys.set_action(
            crate::config::ControlHotkeyAction::StopAll,
            Some("F8".to_string()),
        );

        config.save_to_path(&path).expect("save config");

        let loaded = Config::load_from_path(&path).expect("load saved config");
        assert!(loaded.sound_folders.is_empty());
        assert!(loaded.sounds.is_empty());
        assert!(loaded.tabs.is_empty());
        assert!(loaded.settings.control_hotkeys.stop_all.is_none());
        let persisted: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read config")).expect("parse config");
        assert!(persisted.get("library_id").is_none());
        assert!(persisted.get("sound_folders").is_none());
        assert!(persisted.get("sounds").is_none());
        assert!(persisted.get("tabs").is_none());
        assert!(persisted["settings"].get("control_hotkeys").is_none());
        assert!(fs::read_dir(&dir)
            .expect("read config directory")
            .all(|entry| !entry
                .expect("read config entry")
                .file_name()
                .to_string_lossy()
                .contains(".tmp.")));

        fs::remove_dir_all(dir).expect("cleanup config directory");
    }

    #[test]
    fn save_to_path_cleans_temp_file_when_replacement_fails() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create config directory");
        let mut config = Config::default();

        assert!(config.save_to_path(&dir).is_err());
        assert!(fs::read_dir(dir.parent().expect("temp parent"))
            .expect("read temp parent")
            .all(|entry| !entry
                .expect("read temp entry")
                .file_name()
                .to_string_lossy()
                .starts_with(&format!(
                    "{}.tmp.",
                    dir.file_name().unwrap().to_string_lossy()
                ))));

        fs::remove_dir_all(dir).expect("cleanup config directory");
    }
}
