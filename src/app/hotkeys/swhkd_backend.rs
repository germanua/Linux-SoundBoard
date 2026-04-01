use log::{debug, info, warn};
use std::any::Any;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::backend_runtime::HotkeyBackend;
use super::error::unsupported_key_for_backend;
use super::parse_hotkey_spec;
use super::swhkd_config::SwhkdConfig;
use super::swhkd_install::missing_swhkd_message;
use super::swhkd_process::SwhkdProcesses;

/// Clears the flag when dropped.
struct DropFlag {
    flag: Arc<AtomicBool>,
}

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
        info!("swhkd listener thread exited");
    }
}

pub struct SwhkdBackend {
    processes: Arc<Mutex<SwhkdProcesses>>,
    config: Arc<Mutex<SwhkdConfig>>,
    pipe_path: PathBuf,
    started: AtomicBool,
    listener_alive: Arc<AtomicBool>,
}

impl SwhkdBackend {
    pub fn new() -> Result<Self, String> {
        info!("Initializing swhkd backend");

        if which::which("swhkd").is_err() {
            return Err(missing_swhkd_message("swhkd"));
        }

        if which::which("swhks").is_err() {
            return Err(missing_swhkd_message("swhks"));
        }

        let pipe_path = Self::create_hotkey_pipe()?;

        let config = SwhkdConfig::new(pipe_path.clone())?;

        let processes = if SwhkdProcesses::is_swhkd_running() {
            info!("Using existing swhkd instance");
            SwhkdProcesses::attach_existing()?
        } else {
            info!("Spawning swhkd/swhks processes");

            config.write_to_file()?;

            SwhkdProcesses::spawn_managed()?
        };

        let processes_arc = Arc::new(Mutex::new(processes));

        {
            let processes_guard = processes_arc
                .lock()
                .map_err(|e| format!("Failed to lock processes: {}", e))?;
            processes_guard.start_monitor();
        }

        Ok(Self {
            processes: processes_arc,
            config: Arc::new(Mutex::new(config)),
            pipe_path,
            started: AtomicBool::new(false),
            listener_alive: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Create the hotkey pipe.
    fn create_hotkey_pipe() -> Result<PathBuf, String> {
        let uid = nix::unistd::getuid();
        let runtime_dir = PathBuf::from(format!("/run/user/{}", uid));

        if !runtime_dir.exists() {
            return Err(format!(
                "Runtime directory does not exist: {}",
                runtime_dir.display()
            ));
        }

        let pipe_path = runtime_dir.join("lsb_hotkey.pipe");

        if pipe_path.exists() {
            fs::remove_file(&pipe_path).map_err(|e| format!("Failed to remove old pipe: {}", e))?;
        }

        // Let root-owned `swhkd` write to the pipe.
        nix::unistd::mkfifo(
            &pipe_path,
            nix::sys::stat::Mode::S_IRUSR
                | nix::sys::stat::Mode::S_IWUSR
                | nix::sys::stat::Mode::S_IWGRP
                | nix::sys::stat::Mode::S_IWOTH,
        )
        .map_err(|e| format!("Failed to create named pipe: {}", e))?;

        info!("Created hotkey pipe at: {}", pipe_path.display());
        Ok(pipe_path)
    }

    /// Reload `swhkd` after a config change.
    fn reload_swhkd(&self) -> Result<(), String> {
        let processes = self
            .processes
            .lock()
            .map_err(|e| format!("Failed to acquire processes lock (poisoned): {}", e))?;

        thread::sleep(Duration::from_millis(100));

        SwhkdConfig::reload_swhkd(processes.swhkd_pid)?;

        thread::sleep(Duration::from_millis(200));

        info!("swhkd config reload complete");
        Ok(())
    }

    fn reload_swhkd_async(&self) {
        let processes = Arc::clone(&self.processes);
        thread::spawn(move || {
            let swhkd_pid = match processes
                .lock()
                .map_err(|e| format!("Failed to acquire processes lock (poisoned): {}", e))
            {
                Ok(processes) => processes.swhkd_pid,
                Err(e) => {
                    warn!("Failed to queue swhkd reload: {}", e);
                    return;
                }
            };

            thread::sleep(Duration::from_millis(100));
            if let Err(e) = SwhkdConfig::reload_swhkd(swhkd_pid) {
                warn!(
                    "Failed to reload swhkd config: {}. Hotkeys will be unregistered on next app restart.",
                    e
                );
                return;
            }

            thread::sleep(Duration::from_millis(200));
            info!("swhkd config reload complete");
        });
    }

    fn unregister_many_inner(&self, sound_ids: &[String]) -> Result<(), String> {
        if sound_ids.is_empty() {
            return Ok(());
        }

        let mut config = self
            .config
            .lock()
            .map_err(|e| format!("Failed to acquire config lock (poisoned): {}", e))?;
        let removed = config.remove_hotkeys(sound_ids);
        if removed == 0 {
            return Ok(());
        }

        config.write_to_file()?;
        drop(config);

        // Keep delete-path unregisters off the GTK thread.
        self.reload_swhkd_async();

        Ok(())
    }

    fn validate_hotkey_binding(hotkey: &str) -> Result<(), String> {
        let trimmed = hotkey.trim();
        if trimmed.is_empty() {
            return Err(unsupported_key_for_backend(
                "swhkd",
                "Hotkey cannot be empty.",
            ));
        }

        let spec = parse_hotkey_spec(trimmed).map_err(|e| {
            unsupported_key_for_backend("swhkd", format!("{trimmed} is invalid. {e}"))
        })?;

        spec.swhkd_string()
            .map(|_| ())
            .map_err(|detail| unsupported_key_for_backend("swhkd", detail))
    }

    /// Check that `swhkd` is still running.
    fn verify_swhkd_running(&self) -> Result<(), String> {
        let processes = self
            .processes
            .lock()
            .map_err(|e| format!("Failed to acquire processes lock (poisoned): {}", e))?;

        let pid = nix::unistd::Pid::from_raw(processes.swhkd_pid);
        match nix::sys::signal::kill(pid, None) {
            Ok(_) => Ok(()),
            Err(_) => Err(format!(
                "swhkd process (PID {}) has crashed or exited.\n\
                 This usually happens due to:\n\
                 • Invalid hotkey configuration\n\
                 • Permission issues with /dev/input devices\n\
                 • Conflicting hotkey daemon already running\n\
                 Check logs: ~/.local/share/swhkd/*.log",
                processes.swhkd_pid
            )),
        }
    }

    /// Check that the backend is healthy.
    pub fn is_healthy(&self) -> Result<(), String> {
        if !self.listener_alive.load(Ordering::SeqCst) {
            return Err("swhkd listener thread is not running".to_string());
        }

        if let Err(e) = self.verify_swhkd_running() {
            return Err(format!("swhkd process is not running: {}", e));
        }

        Ok(())
    }
}

impl HotkeyBackend for SwhkdBackend {
    fn name(&self) -> &'static str {
        "swhkd"
    }

    fn validate_hotkey(&self, hotkey: &str) -> Result<(), String> {
        Self::validate_hotkey_binding(hotkey)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), String> {
        debug!("Registering hotkey: {} -> {}", sound_id, hotkey);

        Self::validate_hotkey_binding(hotkey)?;

        let mut config = self
            .config
            .lock()
            .map_err(|e| format!("Failed to acquire config lock (poisoned): {}", e))?;
        config.add_hotkey(sound_id, hotkey)?;

        config.write_to_file()?;

        drop(config);

        if let Err(e) = self.reload_swhkd() {
            warn!(
                "Failed to reload swhkd config: {}. Hotkey will be registered on next app restart.",
                e
            );
        }

        if let Err(e) = self.verify_swhkd_running() {
            warn!("swhkd verification warning: {}", e);
        }

        Ok(())
    }

    fn unregister(&self, sound_id: &str) -> Result<(), String> {
        debug!("Unregistering hotkey: {}", sound_id);
        self.unregister_many_inner(&[sound_id.to_string()])
    }

    fn unregister_many(&self, sound_ids: &[String]) -> Result<(), String> {
        if !sound_ids.is_empty() {
            debug!("Unregistering {} hotkeys", sound_ids.len());
        }
        self.unregister_many_inner(sound_ids)
    }

    fn start_listener(&self, sender: Sender<String>) {
        if self.started.swap(true, Ordering::SeqCst) {
            warn!("swhkd listener already started");
            return;
        }

        let pipe_path = self.pipe_path.clone();
        let listener_alive = self.listener_alive.clone();

        info!(
            "Starting swhkd hotkey listener on pipe: {}",
            pipe_path.display()
        );

        listener_alive.store(true, Ordering::SeqCst);
        info!("swhkd listener thread started");

        thread::spawn(move || {
            let flag = listener_alive.clone();
            let _guard = DropFlag { flag };

            loop {
                let file = match File::open(&pipe_path) {
                    Ok(f) => f,
                    Err(e) => {
                        warn!("Failed to open hotkey pipe: {}", e);
                        thread::sleep(Duration::from_secs(1));
                        continue;
                    }
                };

                let reader = BufReader::new(file);
                for line in reader.lines() {
                    match line {
                        Ok(sound_id) => {
                            let sound_id = sound_id.trim().to_string();
                            if !sound_id.is_empty() {
                                debug!("swhkd hotkey triggered: {}", sound_id);
                                if sender.send(sound_id).is_err() {
                                    warn!("Failed to send hotkey event (receiver dropped)");
                                    return;
                                }
                            }
                        }
                        Err(e) => {
                            warn!("Error reading from hotkey pipe: {}", e);
                            break;
                        }
                    }
                }

                debug!("Hotkey pipe closed, reopening...");
                thread::sleep(Duration::from_millis(100));
            }
        });
    }
}

impl Drop for SwhkdBackend {
    fn drop(&mut self) {
        info!("Cleaning up swhkd backend");

        if self.pipe_path.exists() {
            if let Err(e) = fs::remove_file(&self.pipe_path) {
                warn!("Failed to remove hotkey pipe: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_name() {
        assert_eq!("swhkd", "swhkd");
    }

    #[test]
    fn test_validate_hotkey_canonical_values() {
        assert!(SwhkdBackend::validate_hotkey_binding("F1").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("KeyA").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Digit1").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Slash").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Shift+Slash").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Space").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Tab").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Enter").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Backspace").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Delete").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl+Alt+KeyP").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl+Numpad1").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Numpad1").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl+NumpadAdd").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl+KeyA").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Alt+Slash").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Super+Digit1").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl+Enter").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl+Backspace").is_ok());
    }

    #[test]
    fn test_validate_hotkey_rejects_unsupported_swhkd_key() {
        let err = SwhkdBackend::validate_hotkey_binding("Ctrl+NumpadDivide").unwrap_err();
        assert_eq!(
            err,
            "UNSUPPORTED_KEY_FOR_BACKEND:swhkd:Ctrl+NumpadDivide cannot be represented by swhkd."
        );
    }

    #[test]
    fn test_validate_hotkey_invalid() {
        assert!(SwhkdBackend::validate_hotkey_binding("").is_err());
        assert!(SwhkdBackend::validate_hotkey_binding("   ").is_err());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl++KeyA").is_err());
    }
}
