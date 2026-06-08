use log::{debug, error, info, warn};
use nix::sys::signal::Signal;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::error::HotkeyError;
use super::swhkd_install::missing_swhkd_message;
use super::{
    SWHKD_MONITOR_INTERVAL_SECS, SWHKD_SOCKET_POLL_INTERVAL_MS, SWHKD_STALE_TERMINATE_TIMEOUT_MS,
    SWHKD_STARTUP_VERIFY_WAIT_MS,
};

pub struct SwhkdProcesses {
    pub swhks_child: Option<Child>,
    pub swhkd_child: Option<Child>,
    pub swhkd_pid: i32,
    pub managed: bool,
    pub monitor_running: Arc<AtomicBool>,
    pub swhkd_dead: Arc<AtomicBool>,
}

impl SwhkdProcesses {
    pub fn has_running_daemons() -> bool {
        Self::user_processes_are_running(&["swhkd", "swhks"])
    }

    pub fn spawn_swhks() -> Result<Child, HotkeyError> {
        info!("Spawning swhks process");

        let swhks_path = which::which("swhks")
            .map_err(|_| HotkeyError::Process(missing_swhkd_message("swhks")))?;

        Command::new(swhks_path)
            .spawn()
            .map_err(|e| HotkeyError::Process(format!("Failed to spawn swhks: {}", e)))
    }

    pub fn spawn_swhkd(config_path: &Path) -> Result<Child, HotkeyError> {
        info!("Spawning swhkd process");

        let swhkd_path = which::which("swhkd")
            .map_err(|_| HotkeyError::Process(missing_swhkd_message("swhkd")))?;

        if !Self::has_setuid_bit(&swhkd_path) {
            warn!("swhkd does not have setuid bit set");
            return Err(HotkeyError::Process(
                "swhkd requires setuid bit for proper operation.\n\
                 Run: sudo chmod u+s \"$(command -v swhkd)\"\n\
                 Or reinstall the package."
                    .to_string(),
            ));
        }

        Command::new(swhkd_path)
            .arg("--config")
            .arg(config_path)
            .spawn()
            .map_err(|e| HotkeyError::Process(format!("Failed to spawn swhkd: {}", e)))
    }

    fn has_setuid_bit(path: &Path) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(metadata) = std::fs::metadata(path) {
                let mode = metadata.permissions().mode();
                return (mode & 0o4000) != 0;
            }
        }
        false
    }

    pub fn wait_for_swhks_socket() -> Result<(), HotkeyError> {
        let uid = nix::unistd::getuid();
        let sock_path = PathBuf::from(format!("/run/user/{}/swhkd.sock", uid));

        debug!("Waiting for swhks socket at: {}", sock_path.display());

        for attempt in 1..=50 {
            if sock_path.exists() {
                info!("swhks socket ready after {} attempts", attempt);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(SWHKD_SOCKET_POLL_INTERVAL_MS));
        }

        Err(HotkeyError::Process(
            "Timeout waiting for swhks socket to be created".to_string(),
        ))
    }

    pub fn spawn_managed(config_path: &Path) -> Result<Self, HotkeyError> {
        let swhks_child = Self::spawn_swhks()?;

        Self::wait_for_swhks_socket()?;

        let swhkd_child = Self::spawn_swhkd(config_path)?;
        let swhkd_pid = swhkd_child.id() as i32;

        thread::sleep(Duration::from_millis(SWHKD_STARTUP_VERIFY_WAIT_MS));

        let pid = nix::unistd::Pid::from_raw(swhkd_pid);
        match nix::sys::signal::kill(pid, None) {
            Ok(_) => {
                info!("swhkd process verified running (PID: {})", swhkd_pid);
            }
            Err(_) => {
                return Err(HotkeyError::Process(format!(
                    "swhkd process (PID {}) crashed immediately after startup.\n\
                     This usually indicates:\n\
                     • Permission issues with /dev/input devices\n\
                     • Another hotkey daemon is already running\n\
                     • Invalid configuration file\n\
                     Check logs: ~/.local/share/swhkd/*.log",
                    swhkd_pid
                )));
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

    /// Forcefully stop any `swhkd`/`swhks` daemons owned by the current user
    /// before spawning a fresh managed pair.
    ///
    /// A `swhkd` started in a previous session can outlive the app: it is
    /// setuid-root, so a non-graceful app exit leaves it reparented to
    /// `systemd --user`. Attaching to such an orphan means relying on it
    /// honouring config reloads from an unknown state. Restarting it instead
    /// guarantees every session owns a clean daemon whose config is loaded at
    /// startup. Best-effort: failures are logged, not fatal.
    pub fn terminate_stale_daemons() {
        info!("Stopping pre-existing swhkd/swhks daemons before spawning a managed pair");

        // SIGTERM lets swhkd ungrab input devices and exit cleanly.
        Self::signal_user_processes("swhkd", Signal::SIGTERM);
        Self::signal_user_processes("swhks", Signal::SIGTERM);

        Self::wait_for_user_processes_to_exit(&["swhkd", "swhks"]);

        if Self::user_processes_are_running(&["swhkd", "swhks"]) {
            warn!("swhkd/swhks did not exit after SIGTERM; sending SIGKILL");
            Self::signal_user_processes("swhkd", Signal::SIGKILL);
            Self::signal_user_processes("swhks", Signal::SIGKILL);
            Self::wait_for_user_processes_to_exit(&["swhkd", "swhks"]);
        }

        Self::remove_stale_runtime_files();
    }

    /// PIDs of processes named `name` owned by the current user.
    fn user_pids(name: &str) -> Vec<i32> {
        let uid = nix::unistd::getuid().as_raw();
        Self::processes_for_real_uid(name, uid)
    }

    fn signal_user_processes(name: &str, signal: Signal) {
        for pid in Self::user_pids(name) {
            if let Err(err) = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal) {
                debug!(
                    "Could not send {:?} to {} PID {}: {}",
                    signal, name, pid, err
                );
            }
        }
    }

    fn user_processes_are_running(names: &[&str]) -> bool {
        names.iter().any(|name| !Self::user_pids(name).is_empty())
    }

    fn wait_for_user_processes_to_exit(names: &[&str]) {
        let mut waited = 0;
        while Self::user_processes_are_running(names) && waited < SWHKD_STALE_TERMINATE_TIMEOUT_MS {
            thread::sleep(Duration::from_millis(SWHKD_SOCKET_POLL_INTERVAL_MS));
            waited += SWHKD_SOCKET_POLL_INTERVAL_MS;
        }
    }

    fn processes_for_real_uid(name: &str, uid: u32) -> Vec<i32> {
        let Ok(entries) = fs::read_dir("/proc") else {
            return Vec::new();
        };

        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let pid = entry.file_name().to_string_lossy().parse::<i32>().ok()?;
                let proc_dir = entry.path();
                if !Self::proc_comm_matches(&proc_dir, name) {
                    return None;
                }
                if Self::proc_real_uid(&proc_dir) == Some(uid) {
                    Some(pid)
                } else {
                    None
                }
            })
            .collect()
    }

    fn proc_comm_matches(proc_dir: &Path, name: &str) -> bool {
        fs::read_to_string(proc_dir.join("comm"))
            .map(|comm| comm.trim() == name)
            .unwrap_or(false)
    }

    fn proc_real_uid(proc_dir: &Path) -> Option<u32> {
        let status = fs::read_to_string(proc_dir.join("status")).ok()?;
        Self::parse_real_uid_from_status(&status)
    }

    fn parse_real_uid_from_status(status: &str) -> Option<u32> {
        status.lines().find_map(|line| {
            let rest = line.strip_prefix("Uid:")?;
            rest.split_whitespace().next()?.parse::<u32>().ok()
        })
    }

    fn terminate_tracked_child(name: &str, child: &mut Child) {
        let pid = nix::unistd::Pid::from_raw(child.id() as i32);
        info!("Terminating tracked {} process", name);
        if let Err(err) = nix::sys::signal::kill(pid, Signal::SIGTERM) {
            debug!(
                "Could not send SIGTERM to tracked {} PID {}: {}",
                name, pid, err
            );
        }

        let mut waited = 0;
        while waited < SWHKD_STALE_TERMINATE_TIMEOUT_MS {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {
                    thread::sleep(Duration::from_millis(SWHKD_SOCKET_POLL_INTERVAL_MS));
                    waited += SWHKD_SOCKET_POLL_INTERVAL_MS;
                }
                Err(err) => {
                    debug!("Could not wait for tracked {} PID {}: {}", name, pid, err);
                    return;
                }
            }
        }

        warn!(
            "Tracked {} process did not exit after SIGTERM; sending SIGKILL",
            name
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Remove the swhks socket and swhkd pidfile a killed daemon leaves behind,
    /// so the freshly spawned swhks can bind its socket without `EADDRINUSE`.
    fn remove_stale_runtime_files() {
        let uid = nix::unistd::getuid();
        let runtime_dir = PathBuf::from(format!("/run/user/{}", uid));
        for name in [
            "swhkd.sock".to_string(),
            "swhks.pid".to_string(),
            format!("swhkd_{}.pid", uid),
            format!("swhks_{}.pid", uid),
        ] {
            let path = runtime_dir.join(name);
            if path.exists() {
                if let Err(e) = fs::remove_file(&path) {
                    debug!("Could not remove stale {}: {}", path.display(), e);
                }
            }
        }
    }

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
                thread::sleep(Duration::from_secs(SWHKD_MONITOR_INTERVAL_SECS));

                if !monitor_running.load(Ordering::SeqCst) {
                    break;
                }

                let check_pid = nix::unistd::Pid::from_raw(pid);
                if nix::sys::signal::kill(check_pid, None).is_err() {
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
                    break;
                }
            }
            info!("swhkd monitor thread stopped");
        });
    }

    pub fn terminate(&mut self) {
        if !self.managed {
            debug!("Not terminating unmanaged swhkd instance");
            return;
        }

        self.monitor_running.store(false, Ordering::SeqCst);

        if let Some(mut child) = self.swhkd_child.take() {
            Self::terminate_tracked_child("swhkd", &mut child);
        }

        if let Some(mut child) = self.swhks_child.take() {
            Self::terminate_tracked_child("swhks", &mut child);
        }

        Self::signal_user_processes("swhkd", Signal::SIGTERM);
        Self::signal_user_processes("swhks", Signal::SIGTERM);
        Self::wait_for_user_processes_to_exit(&["swhkd", "swhks"]);
        if Self::user_processes_are_running(&["swhkd", "swhks"]) {
            warn!("swhkd/swhks did not exit during shutdown; sending SIGKILL");
            Self::signal_user_processes("swhkd", Signal::SIGKILL);
            Self::signal_user_processes("swhks", Signal::SIGKILL);
            Self::wait_for_user_processes_to_exit(&["swhkd", "swhks"]);
        }
        Self::remove_stale_runtime_files();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_uid_from_proc_status() {
        let status = "Name:\tswhkd\nUid:\t1000\t0\t0\t0\nGid:\t1000\t1000\t1000\t1000\n";
        assert_eq!(
            SwhkdProcesses::parse_real_uid_from_status(status),
            Some(1000)
        );
    }

    #[test]
    fn ignores_status_without_uid_line() {
        assert_eq!(
            SwhkdProcesses::parse_real_uid_from_status("Name:\tswhkd\n"),
            None
        );
    }
}

impl Drop for SwhkdProcesses {
    fn drop(&mut self) {
        self.terminate();
    }
}
