use chrono::Local;
use log::{debug, info};
use std::fs;
use std::io::{BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use super::error::HotkeyError;
use super::parse_hotkey_spec;

pub struct SwhkdConfig {
    pub(crate) config_path: PathBuf,
    pub(crate) pipe_path: PathBuf,
    candidate: Option<ProjectionCandidate>,
}

struct ProjectionCandidate {
    path: PathBuf,
    writer: BufWriter<fs::File>,
    count: usize,
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
            config_path,
            pipe_path,
            candidate: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn for_paths(config_path: PathBuf, pipe_path: PathBuf) -> Self {
        Self {
            config_path,
            pipe_path,
            candidate: None,
        }
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

    pub fn begin_projection(&mut self) -> Result<(), HotkeyError> {
        self.abort_projection();
        info!("Streaming swhkd config to: {}", self.config_path.display());
        let path = self.config_path.with_file_name(format!(
            ".swhkdrc.tmp.{}.{}",
            std::process::id(),
            CONFIG_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .map_err(|error| {
                HotkeyError::Io(format!("Failed to create swhkd candidate: {error}"))
            })?;
        let mut writer = BufWriter::new(file);
        if let Err(error) = writeln!(writer, "# LinuxSoundBoard swhkd config")
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
        {
            let _ = fs::remove_file(&path);
            return Err(HotkeyError::Io(error.to_string()));
        }
        self.candidate = Some(ProjectionCandidate {
            path,
            writer,
            count: 0,
        });
        Ok(())
    }

    pub fn projection_started(&self) -> bool {
        self.candidate.is_some()
    }

    pub fn stage_hotkey(&mut self, sound_id: &str, hotkey: &str) -> Result<(), HotkeyError> {
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
        let pipe = shell_quote(&self.pipe_path.to_string_lossy());
        let candidate = self
            .candidate
            .as_mut()
            .ok_or_else(|| HotkeyError::Io("swhkd projection was not started".to_string()))?;
        let kind = if sound_id.starts_with("control:") {
            "Control"
        } else {
            "Sound"
        };
        writeln!(
            candidate.writer,
            "# {kind}: {sound_id}\n{swhkd_hotkey}\n    printf '%s\\n' {} > {pipe}\n",
            shell_quote(sound_id)
        )
        .map_err(|error| HotkeyError::Io(error.to_string()))?;
        candidate.count = candidate.count.saturating_add(1);
        Ok(())
    }

    pub fn abort_projection(&mut self) {
        if let Some(candidate) = self.candidate.take() {
            drop(candidate.writer);
            let _ = fs::remove_file(candidate.path);
        }
    }

    fn convert_to_swhkd_format(hotkey: &str) -> Result<String, HotkeyError> {
        let swhkd = parse_hotkey_spec(hotkey)?.swhkd_string()?;
        if let Some(last_plus) = swhkd.rfind(" + ") {
            let (mods, key) = swhkd.split_at(last_plus + 3);
            Ok(format!("{}~{}", mods, key))
        } else {
            Ok(format!("~{}", swhkd))
        }
    }

    pub fn commit_projection(&mut self) -> Result<usize, HotkeyError> {
        let mut candidate = self
            .candidate
            .take()
            .ok_or_else(|| HotkeyError::Io("swhkd projection was not started".to_string()))?;
        let result = (|| -> Result<usize, HotkeyError> {
            candidate
                .writer
                .flush()
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
            candidate
                .writer
                .get_ref()
                .sync_all()
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
            self.preserve_last_good()?;
            fs::rename(&candidate.path, &self.config_path)
                .map_err(|error| HotkeyError::Io(error.to_string()))?;
            self.sync_parent()?;
            Ok(candidate.count)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&candidate.path);
        }
        if let Ok(count) = result {
            debug!("Config file written with {count} hotkeys");
        }
        result
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
    fn projection_is_streamed_directly_to_the_candidate_file() {
        let dir = std::env::temp_dir().join(format!("lsb-swhkd-stream-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create test directory");
        let mut config = SwhkdConfig::for_paths(dir.join("swhkdrc"), dir.join("hotkey.pipe"));

        config.begin_projection().expect("begin projection");
        config
            .stage_hotkey("sound-1", "Ctrl+KeyA")
            .expect("stage first hotkey");
        config
            .stage_hotkey("control:stop_all", "Alt+KeyB")
            .expect("stage second hotkey");
        assert_eq!(config.commit_projection().expect("commit projection"), 2);

        let rendered = fs::read_to_string(&config.config_path).expect("read projection");
        assert!(rendered.contains("ctrl + ~a"));
        assert!(rendered.contains("alt + ~b"));
        assert!(rendered.contains("'sound-1'"));
        assert!(rendered.contains("'control:stop_all'"));
        fs::remove_dir_all(dir).expect("remove test directory");
    }

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
    fn abort_projection_removes_the_candidate() {
        let dir = std::env::temp_dir().join(format!("lsb-swhkd-abort-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create test directory");
        let mut config = SwhkdConfig::for_paths(dir.join("swhkdrc"), dir.join("hotkey.pipe"));
        config.begin_projection().expect("begin projection");
        config.stage_hotkey("sound1", "Ctrl+KeyA").unwrap();
        config.abort_projection();
        assert!(!config.projection_started());
        assert_eq!(fs::read_dir(&dir).expect("read test directory").count(), 0);
        fs::remove_dir_all(dir).expect("remove test directory");
    }

    #[test]
    fn test_rejects_unsupported_hotkey() {
        let mut config = SwhkdConfig::for_paths(
            PathBuf::from("/tmp/test_swhkdrc"),
            PathBuf::from("/tmp/test.pipe"),
        );
        config.begin_projection().unwrap();

        let err = config
            .stage_hotkey("sound1", "Ctrl+NumpadDivide")
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Ctrl+NumpadDivide cannot be represented by swhkd."
        );
        config.abort_projection();
    }

    #[test]
    fn rejects_binding_ids_that_could_modify_the_generated_shell_command() {
        let mut config = SwhkdConfig::for_paths(
            PathBuf::from("/tmp/test_swhkdrc"),
            PathBuf::from("/tmp/test.pipe"),
        );
        config.begin_projection().unwrap();

        assert!(config
            .stage_hotkey("../sound;touch /tmp/x", "Ctrl+KeyA")
            .is_err());
        config.abort_projection();
    }

    #[test]
    fn failed_projection_can_restore_the_last_good_config() {
        let dir = std::env::temp_dir().join(format!("lsb-swhkd-config-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create test directory");
        let mut config = SwhkdConfig::for_paths(dir.join("swhkdrc"), dir.join("hotkey.pipe"));
        config.begin_projection().expect("begin first projection");
        config
            .stage_hotkey("first", "Ctrl+KeyA")
            .expect("add first binding");
        config.commit_projection().expect("write first config");
        config.begin_projection().expect("begin second projection");
        config
            .stage_hotkey("first", "Ctrl+KeyA")
            .expect("retain first binding");
        config
            .stage_hotkey("second", "Alt+KeyB")
            .expect("add second binding");
        config.commit_projection().expect("write candidate config");
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
