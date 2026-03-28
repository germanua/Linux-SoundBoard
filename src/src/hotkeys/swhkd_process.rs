use log::{debug, error, info, warn};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::swhkd_install::missing_swhkd_message;

pub struct SwhkdProcesses {
    pub swhks_child: Option<Child>,
    pub swhkd_child: Option<Child>,
    pub swhkd_pid: i32,
    pub managed: bool,
    /// Flag set to false when monitor should stop
    pub monitor_running: Arc<AtomicBool>,
    /// Flag set to true when swhkd has died
    pub swhkd_dead: Arc<AtomicBool>,
}

impl SwhkdProcesses {
    /// Check if swhkd is already running (e.g., via systemd service)
    pub fn is_swhkd_running() -> bool {
        Command::new("pgrep")
            .arg("-x")
            .arg("swhkd")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    /// Get PID of running swhkd process
    pub fn get_swhkd_pid() -> Option<i32> {
        let output = Command::new("pgrep").arg("-x").arg("swhkd").output().ok()?;

        if output.status.success() {
            let pid_str = String::from_utf8_lossy(&output.stdout);
            pid_str.trim().parse::<i32>().ok()
        } else {
            None
        }
    }

    /// Spawn swhks process (non-privileged)
    pub fn spawn_swhks() -> Result<Child, String> {
        info!("Spawning swhks process");

        let swhks_path = which::which("swhks")
            .map_err(|_| missing_swhkd_message("swhks"))?;

        Command::new(swhks_path)
            .spawn()
            .map_err(|e| format!("Failed to spawn swhks: {}", e))
    }

    /// Spawn swhkd process (requires setuid bit or root privileges)
    pub fn spawn_swhkd() -> Result<Child, String> {
        info!("Spawning swhkd process");

        let swhkd_path = which::which("swhkd")
            .map_err(|_| missing_swhkd_message("swhkd"))?;

        // Check if swhkd has setuid bit
        if !Self::has_setuid_bit(&swhkd_path) {
            warn!("swhkd does not have setuid bit set");
            return Err("swhkd requires setuid bit for proper operation.\n\
                 Run: sudo chmod u+s \"$(command -v swhkd)\"\n\
                 Or reinstall the package."
                .to_string());
        }

        Command::new(swhkd_path)
            .spawn()
            .map_err(|e| format!("Failed to spawn swhkd: {}", e))
    }

    /// Check if a binary has the setuid bit set
    fn has_setuid_bit(path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(path) {
                let mode = metadata.permissions().mode();
                return (mode & 0o4000) != 0; // Check setuid bit
            }
        }
        false
    }

    /// Wait for swhks socket to be ready
    pub fn wait_for_swhks_socket() -> Result<(), String> {
        let uid = nix::unistd::getuid();
        let sock_path = PathBuf::from(format!("/run/user/{}/swhkd.sock", uid));

        debug!("Waiting for swhks socket at: {}", sock_path.display());

        for attempt in 1..=50 {
            if sock_path.exists() {
                info!("swhks socket ready after {} attempts", attempt);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }

        Err("Timeout waiting for swhks socket to be created".to_string())
    }

    /// Create new managed processes
    pub fn spawn_managed() -> Result<Self, String> {
        // Spawn swhks first
        let swhks_child = Self::spawn_swhks()?;

        // Wait for socket to be ready
        Self::wait_for_swhks_socket()?;

        // Spawn swhkd
        let swhkd_child = Self::spawn_swhkd()?;
        let swhkd_pid = swhkd_child.id() as i32;

        // Give swhkd a moment to initialize
        thread::sleep(Duration::from_millis(500));

        // Verify swhkd is still running (didn't crash on startup)
        let pid = nix::unistd::Pid::from_raw(swhkd_pid);
        match nix::sys::signal::kill(pid, None) {
            Ok(_) => {
                info!("swhkd process verified running (PID: {})", swhkd_pid);
            }
            Err(_) => {
                return Err(format!(
                    "swhkd process (PID {}) crashed immediately after startup.\n\
                     This usually indicates:\n\
                     • Permission issues with /dev/input devices\n\
                     • Another hotkey daemon is already running\n\
                     • Invalid configuration file\n\
                     Check logs: ~/.local/share/swhkd/*.log",
                    swhkd_pid
                ));
            }
        }

        Ok(Self {
            swhks_child: Some(swhks_child),
            swhkd_child: Some(swhkd_child),
            swhkd_pid,
            managed: true,
            monitor_running: Arc::new(AtomicBool::new(false)),
            swhkd_dead: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Attach to existing swhkd instance
    pub fn attach_existing() -> Result<Self, String> {
        let swhkd_pid =
            Self::get_swhkd_pid().ok_or("swhkd is running but PID could not be determined")?;

        info!("Attaching to existing swhkd instance (PID: {})", swhkd_pid);

        Ok(Self {
            swhks_child: None,
            swhkd_child: None,
            swhkd_pid,
            managed: false,
            monitor_running: Arc::new(AtomicBool::new(false)),
            swhkd_dead: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Start monitoring swhkd in a background thread
    /// This will detect if swhkd dies and log an error
    pub fn start_monitor(&self) {
        if !self.managed {
            debug!("Not starting monitor for unmanaged swhkd instance");
            return;
        }

        let monitor_running = self.monitor_running.clone();
        let swhkd_dead = self.swhkd_dead.clone();
        let pid = self.swhkd_pid;

        monitor_running.store(true, Ordering::SeqCst);
        swhkd_dead.store(false, Ordering::SeqCst);

        thread::spawn(move || {
            info!("swhkd monitor thread started for PID {}", pid);
            while monitor_running.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_secs(30));

                if !monitor_running.load(Ordering::SeqCst) {
                    break;
                }

                let check_pid = nix::unistd::Pid::from_raw(pid);
                if nix::sys::signal::kill(check_pid, None).is_err() {
                    // swhkd has died
                    error!(
                        "CRITICAL: swhkd process (PID {}) has died!\n\
                         Hotkeys will stop working until the application is restarted.\n\
                         Possible causes:\n\
                         • Invalid hotkey configuration\n\
                         • Permission issues with /dev/input devices\n\
                         • swhkd crashed (check ~/.local/share/swhkd/*.log)\n\
                         • Another hotkey daemon is already running",
                        pid
                    );
                    swhkd_dead.store(true, Ordering::SeqCst);
                    // Stop monitoring since swhkd is dead
                    break;
                }
            }
            info!("swhkd monitor thread stopped");
        });
    }

    /// Terminate managed processes
    pub fn terminate(&mut self) {
        if !self.managed {
            debug!("Not terminating unmanaged swhkd instance");
            return;
        }

        // Stop monitor first
        self.monitor_running.store(false, Ordering::SeqCst);

        // Kill swhkd first
        if let Some(mut child) = self.swhkd_child.take() {
            info!("Terminating swhkd process");
            let _ = child.kill();
            let _ = child.wait();
        }

        // Then kill swhks
        if let Some(mut child) = self.swhks_child.take() {
            info!("Terminating swhks process");
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for SwhkdProcesses {
    fn drop(&mut self) {
        self.terminate();
    }
}
