use chrono::Local;
use log::{debug, info};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use super::parse_hotkey_spec;

pub struct SwhkdConfig {
    pub(crate) hotkeys: HashMap<String, String>,
    pub(crate) config_path: PathBuf,
    pub(crate) pipe_path: PathBuf,
}

impl SwhkdConfig {
    /// Create new config manager
    pub fn new(pipe_path: PathBuf) -> Result<Self, String> {
        let config_path = Self::get_config_path()?;

        // Ensure config directory exists
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create config directory: {}", e))?;
        }

        Ok(Self {
            hotkeys: HashMap::new(),
            config_path,
            pipe_path,
        })
    }

    /// Get the swhkd config file path
    fn get_config_path() -> Result<PathBuf, String> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("XDG_CONFIG_HOME"))
            .map_err(|_| "Could not determine home directory")?;

        let config_dir = if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
            PathBuf::from(xdg)
        } else {
            PathBuf::from(home).join(".config")
        };

        Ok(config_dir.join("swhkd").join("swhkdrc"))
    }

    /// Add a hotkey binding
    pub fn add_hotkey(&mut self, sound_id: &str, hotkey: &str) -> Result<(), String> {
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

    /// Convert our canonical hotkey format to the exact swhkd key syntax.
    fn convert_to_swhkd_format(hotkey: &str) -> Result<String, String> {
        Ok(format!("~{}", parse_hotkey_spec(hotkey)?.swhkd_string()?))
    }

    /// Write the config file
    pub fn write_to_file(&self) -> Result<(), String> {
        info!("Writing swhkd config to: {}", self.config_path.display());

        let timestamp = Local::now().format("%Y-%m-%d %H:%M:%S");
        let mut content = format!(
            "# LinuxSoundBoard - Auto-generated configuration\n\
             # DO NOT EDIT MANUALLY - Changes will be overwritten\n\
             # Last updated: {}\n\
             \n",
            timestamp
        );

        // Sort hotkeys for consistent output
        let mut sorted_hotkeys: Vec<_> = self.hotkeys.iter().collect();
        sorted_hotkeys.sort_by_key(|(id, _)| *id);

        for (sound_id, hotkey) in sorted_hotkeys {
            // Determine if this is a control hotkey or sound hotkey
            let comment = if sound_id.starts_with("control:") {
                format!("# Control: {}", sound_id.strip_prefix("control:").unwrap())
            } else {
                format!("# Sound: {}", sound_id)
            };

            content.push_str(&format!(
                "{}\n{}\n    echo \"{}\" > {}\n\n",
                comment,
                hotkey,
                sound_id,
                self.pipe_path.display()
            ));
        }

        // Write to file
        let mut file = fs::File::create(&self.config_path)
            .map_err(|e| format!("Failed to create config file: {}", e))?;

        file.write_all(content.as_bytes())
            .map_err(|e| format!("Failed to write config file: {}", e))?;

        debug!("Config file written with {} hotkeys", self.hotkeys.len());
        Ok(())
    }

    /// Send SIGHUP to swhkd to reload config
    pub fn reload_swhkd(swhkd_pid: i32) -> Result<(), String> {
        info!("Sending SIGHUP to swhkd (PID: {})", swhkd_pid);

        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(swhkd_pid),
            nix::sys::signal::Signal::SIGHUP,
        )
        .map_err(|e| format!("Failed to send SIGHUP to swhkd: {}", e))?;

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
            "~ctrl + alt + p"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Super+Shift+Digit1").unwrap(),
            "~shift + super + 1"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+ArrowUp").unwrap(),
            "~ctrl + up"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadAdd").unwrap(),
            "~alt + plus"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadSubtract").unwrap(),
            "~alt + kpminus"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadMultiply").unwrap(),
            "~alt + kpasterisk"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadEnter").unwrap(),
            "~alt + kpenter"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Alt+NumpadDecimal").unwrap(),
            "~alt + kpdot"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Quote").unwrap(),
            "~ctrl + apostrophe"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Backquote").unwrap(),
            "~ctrl + grave"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Numpad0").unwrap(),
            "~ctrl + kp0"
        );
        assert_eq!(
            SwhkdConfig::convert_to_swhkd_format("Ctrl+Numpad8").unwrap(),
            "~ctrl + kp8"
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
            hotkeys: HashMap::new(),
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
            hotkeys: HashMap::new(),
            config_path: PathBuf::from("/tmp/test_swhkdrc"),
            pipe_path,
        };

        let err = config
            .add_hotkey("sound1", "Ctrl+NumpadDivide")
            .unwrap_err();
        assert_eq!(err, "Ctrl+NumpadDivide cannot be represented by swhkd.");
        assert!(config.hotkeys.is_empty());
    }
}
