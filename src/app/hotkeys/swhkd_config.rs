use chrono::Local;
use log::{debug, info};
use std::collections::BTreeMap;
use std::fs;
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::HotkeyError;
use super::parse_hotkey_spec;

pub struct SwhkdConfig {
    pub(crate) hotkeys: BTreeMap<String, String>,
    pub(crate) config_path: PathBuf,
    pub(crate) pipe_path: PathBuf,
}

static CONFIG_WRITE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn valid_binding_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl SwhkdConfig {
    fn last_good_path(&self) -> PathBuf {
        self.config_path.with_file_name("swhkdrc.last-good")
    }

    fn sync_parent(&self) -> Result<(), HotkeyError> {
        if let Some(parent) = self.config_path.parent() {
            fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
        }
        Ok(())
    }

    fn preserve_last_good(&self) -> Result<(), HotkeyError> {
        if !self.config_path.exists() {
            return Ok(());
        }
        let backup = self.last_good_path();
        let candidate = backup.with_file_name(format!(
            ".swhkdrc.last-good.tmp.{}.{}",
            std::process::id(),
            CONFIG_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<(), HotkeyError> {
            if fs::hard_link(&self.config_path, &candidate).is_err() {
                fs::copy(&self.config_path, &candidate)
                    .map_err(|error| HotkeyError::Io(error.to_string()))?;
            }
            fs::File::open(&candidate)
                .and_then(|file| file.sync_all())
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
            fs::rename(&candidate, backup).map_err(|error| HotkeyError::Io(error.to_string()))?;
            self.sync_parent()
        })();
        if result.is_err() {
            let _ = fs::remove_file(candidate);
        }
        result
    }

    pub(crate) fn restore_last_good(&self) -> Result<(), HotkeyError> {
        let backup = self.last_good_path();
        if !backup.exists() {
            return Err(HotkeyError::Io(
                "No last-known-good swhkd config is available".to_string(),
            ));
        }
        let candidate = self.config_path.with_file_name(format!(
            ".swhkdrc.restore.{}.{}",
            std::process::id(),
            CONFIG_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<(), HotkeyError> {
            fs::copy(&backup, &candidate).map_err(|error| HotkeyError::Io(error.to_string()))?;
            fs::File::open(&candidate)
                .and_then(|file| file.sync_all())
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
            fs::rename(&candidate, &self.config_path)
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
            self.sync_parent()
        })();
        if result.is_err() {
            let _ = fs::remove_file(candidate);
        }
        result
    }

    pub fn new(pipe_path: PathBuf) -> Result<Self, HotkeyError> {
        let config_path = Self::get_config_path()?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                HotkeyError::Io(format!("Failed to create config directory: {}", e))
            })?;
        }

        Ok(Self {
            hotkeys: BTreeMap::new(),
            config_path,
            pipe_path,
        })
    }

    fn get_config_path() -> Result<PathBuf, HotkeyError> {
        let config_dir = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            PathBuf::from(xdg)
        } else {
            let home = std::env::var("HOME")
                .map_err(|_| HotkeyError::Io("Could not determine home directory".to_string()))?;
            PathBuf::from(home).join(".config")
        };

        Ok(config_dir.join("linux-soundboard").join("swhkdrc"))
    }

    pub fn add_hotkey(&mut self, sound_id: &str, hotkey: &str) -> Result<(), HotkeyError> {
        if !valid_binding_id(sound_id) {
            return Err(HotkeyError::Io(
                "Hotkey binding ID contains unsupported characters".to_string(),
            ));
        }
        let swhkd_hotkey = Self::convert_to_swhkd_format(hotkey)?;
        debug!(
            "Adding hotkey: {} -> {} (swhkd format: {})",
            sound_id, hotkey, swhkd_hotkey
        );
        self.hotkeys.insert(sound_id.to_string(), swhkd_hotkey);
        Ok(())
    }

    pub fn remove_hotkeys(&mut self, sound_ids: &[String]) -> usize {
        let mut removed = 0;
        for sound_id in sound_ids {
            debug!("Removing hotkey: {}", sound_id);
            if self.hotkeys.remove(sound_id).is_some() {
                removed += 1;
            }
        }
        removed
    }

    /// Convert a canonical hotkey to `swhkd` syntax.
    ///
    /// `~` (don't-swallow / pass-through) must be placed immediately before the
    /// final key token, not before the modifiers.  swhkd 1.3.x rejects `~ctrl + l`
    /// but accepts `ctrl + ~l`.
    fn convert_to_swhkd_format(hotkey: &str) -> Result<String, HotkeyError> {
        let swhkd = parse_hotkey_spec(hotkey)?.swhkd_string()?;
        if let Some(last_plus) = swhkd.rfind(" + ") {
            let (mods, key) = swhkd.split_at(last_plus + 3);
            Ok(format!("{}~{}", mods, key))
        } else {
            Ok(format!("~{}", swhkd))
        }
    }

    pub fn write_to_file(&self) -> Result<(), HotkeyError> {
        info!("Writing swhkd config to: {}", self.config_path.display());

        let candidate = self.config_path.with_file_name(format!(
            ".swhkdrc.tmp.{}.{}",
            std::process::id(),
            CONFIG_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let write_result = (|| -> Result<(), HotkeyError> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&candidate)
                .map_err(|error| {
                    HotkeyError::Io(format!("Failed to create swhkd candidate: {error}"))
                })?;
            {
                let mut writer = BufWriter::new(&mut file);
                writeln!(writer, "# LinuxSoundBoard swhkd config")
                    .and_then(|_| {
                        writeln!(
                            writer,
                            "# Do not edit manually; changes will be overwritten"
                        )
                    })
                    .and_then(|_| {
                        writeln!(
                            writer,
                            "# Last updated: {}\n",
                            Local::now().format("%Y-%m-%d %H:%M:%S")
                        )
                    })
                    .map_err(|error| HotkeyError::Io(error.to_string()))?;

                let pipe = shell_quote(&self.pipe_path.to_string_lossy());
                for (binding_id, hotkey) in &self.hotkeys {
                    let kind = if binding_id.starts_with("control:") {
                        "Control"
                    } else {
                        "Sound"
                    };
                    writeln!(
                        writer,
                        "# {kind}: {binding_id}\n{hotkey}\n    printf '%s\\n' {} > {pipe}\n",
                        shell_quote(binding_id)
                    )
                    .map_err(|error| HotkeyError::Io(error.to_string()))?;
                }
                writer
                    .flush()
                    .map_err(|error| HotkeyError::Io(error.to_string()))?;
            }
            file.sync_all()
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
            self.preserve_last_good()?;
            fs::rename(&candidate, &self.config_path)
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
            self.sync_parent()
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&candidate);
        }
        write_result?;

        debug!("Config file written with {} hotkeys", self.hotkeys.len());
        Ok(())
    }

    /// Send `SIGHUP` to reload `swhkd`.
    pub fn reload_swhkd(swhkd_pid: i32) -> Result<(), HotkeyError> {
        info!("Sending SIGHUP to swhkd (PID: {})", swhkd_pid);

        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(swhkd_pid),
            nix::sys::signal::Signal::SIGHUP,
        )
        .map_err(|e| HotkeyError::Process(format!("Failed to send SIGHUP to swhkd: {}", e)))?;

        debug!("SIGHUP sent successfully");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_to_swhkd_format() {
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Alt+KeyP").unwrap(),
            "ctrl + alt + ~p"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Super+Shift+Digit1").unwrap(),
            "shift + super + ~1"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+ArrowUp").unwrap(),
            "ctrl + ~up"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadAdd").unwrap(),
            "alt + ~plus"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadSubtract").unwrap(),
            "alt + ~kpminus"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadMultiply").unwrap(),
            "alt + ~kpasterisk"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadEnter").unwrap(),
            "alt + ~kpenter"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadDecimal").unwrap(),
            "alt + ~kpdot"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Quote").unwrap(),
            "ctrl + ~apostrophe"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Backquote").unwrap(),
            "ctrl + ~grave"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Numpad0").unwrap(),
            "ctrl + ~kp0"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Numpad8").unwrap(),
            "ctrl + ~kp8"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Numpad1").unwrap(),
            "~kp1"
        );
    }

    #[test]
    fn test_add_remove_hotkey() {
        let pipe_path = PathBuf::from("/tmp/test.pipe");
        let mut config = SwhkdConfig {
            hotkeys: BTreeMap::new(),
            config_path: PathBuf::from("/tmp/test_swhkdrc"),
            pipe_path,
        };

        config.add_hotkey("sound1", "Ctrl+KeyA").unwrap();
        assert_eq!(config.hotkeys.len(), 1);

        config.add_hotkey("sound2", "Alt+KeyB").unwrap();
        assert_eq!(config.hotkeys.len(), 2);

        assert_eq!(config.remove_hotkeys(&["sound1".to_string()]), 1);
        assert_eq!(config.hotkeys.len(), 1);
    }

    #[test]
    fn test_rejects_unsupported_hotkey() {
        let pipe_path = PathBuf::from("/tmp/test.pipe");
        let mut config = SwhkdConfig {
            hotkeys: BTreeMap::new(),
            config_path: PathBuf::from("/tmp/test_swhkdrc"),
            pipe_path,
        };

        let err = config
            .add_hotkey("sound1", "Ctrl+NumpadDivide")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Ctrl+NumpadDivide cannot be represented by swhkd."
        );
        assert!(config.hotkeys.is_empty());
    }

    #[test]
    fn rejects_binding_ids_that_could_modify_the_generated_shell_command() {
        let mut config = SwhkdConfig {
            hotkeys: Default::default(),
            config_path: PathBuf::from("/tmp/test_swhkdrc"),
            pipe_path: PathBuf::from("/tmp/test.pipe"),
        };

        assert!(config
            .add_hotkey("../sound;touch /tmp/x", "Ctrl+KeyA")
            .is_err());
        assert!(config.hotkeys.is_empty());
    }

    #[test]
    fn failed_projection_can_restore_the_last_good_config() {
        let dir = std::env::temp_dir().join(format!("lsb-swhkd-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create test directory");
        let mut config = SwhkdConfig {
            hotkeys: BTreeMap::new(),
            config_path: dir.join("swhkdrc"),
            pipe_path: dir.join("hotkey.pipe"),
        };
        config
            .add_hotkey("first", "Ctrl+KeyA")
            .expect("add first binding");
        config.write_to_file().expect("write first config");
        config
            .add_hotkey("second", "Alt+KeyB")
            .expect("add second binding");
        config.write_to_file().expect("write candidate config");
        assert!(fs::read_to_string(&config.config_path)
            .expect("read candidate")
            .contains("second"));

        config.restore_last_good().expect("restore previous config");
        let restored = fs::read_to_string(&config.config_path).expect("read restored config");
        assert!(restored.contains("first"));
        assert!(!restored.contains("second"));

        fs::remove_dir_all(dir).expect("remove test directory");
    }
}
