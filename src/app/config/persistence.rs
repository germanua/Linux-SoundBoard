use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::defaults::{config_dir_name, CONFIG_FILE_NAME};
use crate::config::Config;

static SAVE_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
const SCHEMA_6_BACKUP_FILE_NAME: &str = "config.json.pre-v6-backup";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigSaveBoundary {
    CandidateSynced,
    Renamed,
    DirectorySynced,
}

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
        self.save_to_path_observed(path, |_| Ok::<(), std::convert::Infallible>(()))
    }

    pub(crate) fn save_to_path_observed<E>(
        &mut self,
        path: &Path,
        mut observer: impl FnMut(ConfigSaveBoundary) -> Result<(), E>,
    ) -> Result<(), Box<dyn std::error::Error>>
    where
        E: std::error::Error + 'static,
    {
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
            observer(ConfigSaveBoundary::CandidateSynced)
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)?;
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
        observer(ConfigSaveBoundary::Renamed)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)?;
        if let Some(parent) = path.parent() {
            if let Err(err) = fs::File::open(parent).and_then(|dir| dir.sync_all()) {
                return Err(Box::new(std::io::Error::other(format!(
                    "Configuration was written but its directory could not be synced: {err}"
                ))));
            }
        }
        observer(ConfigSaveBoundary::DirectorySynced)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)?;
        self.persistence_path = Some(path.to_path_buf());
        Ok(())
    }

    pub fn sanitize_for_persistence(&mut self) {
        self.settings.normalize_for_persistence();
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
    fn schema_6_load_creates_exact_private_backup_and_preserves_user_fields() {
        let dir = test_dir();
        fs::create_dir_all(&dir).expect("create config directory");
        let path = dir.join("config.json");
        fs::write(&path, SCHEMA_6_FIXTURE).expect("write schema 6 fixture");

        let config = Config::load_from_path(&path).expect("migrate schema 6 fixture");

        assert_eq!(config.schema_version, crate::config::CURRENT_SCHEMA_VERSION);
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
        let mut legacy = crate::test_support::legacy_config::LegacyConfigFixture::default();
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
        assert_eq!(
            runtime.schema_version,
            crate::config::CURRENT_SCHEMA_VERSION
        );
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
        config.settings.control_hotkeys.set_action(
            crate::config::ControlHotkeyAction::StopAll,
            Some("F8".to_string()),
        );

        config.save_to_path(&path).expect("save config");

        let loaded = Config::load_from_path(&path).expect("load saved config");
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
