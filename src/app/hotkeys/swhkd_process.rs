use log::{debug, error, info, warn};
use nix::sys::signal::Signal;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

struct SpawnedSwhkd {
    child: Child,
    log_path: PathBuf,
}

impl SwhkdProcesses {
    pub fn has_running_daemons() -> bool {
        Self::user_processes_are_running(&["swhkd", "swhks"])
    }

    pub fn spawn_swhks() -> Result<Child, HotkeyError> {
        info!("Spawning swhks process");

        let swhks_path = which::which("swhks")
            .map_err(|_| HotkeyError::Process(missing_swhkd_message("swhks")))?;

        let mut command = Command::new(swhks_path);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
            .spawn()
            .map_err(|e| HotkeyError::Process(format!("Failed to spawn swhks: {}", e)))
    }

    fn spawn_swhkd(config_path: &Path) -> Result<SpawnedSwhkd, HotkeyError> {
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

        let mut command = Command::new(swhkd_path);
        command.arg("--config").arg(config_path);
        Self::spawn_swhkd_command(command, "direct")
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

        Self::wait_for_swhks_socket_at(&sock_path, 50, SWHKD_SOCKET_POLL_INTERVAL_MS)
    }

    fn wait_for_swhks_socket_at(
        sock_path: &Path,
        attempts: usize,
        poll_interval_ms: u64,
    ) -> Result<(), HotkeyError> {
        debug!("Waiting for swhks socket at: {}", sock_path.display());

        for attempt in 1..=attempts.max(1) {
            if UnixStream::connect(sock_path).is_ok() {
                info!("swhks socket ready after {} attempts", attempt);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(poll_interval_ms));
        }

        Err(HotkeyError::Process(
            "Timeout waiting for swhks socket to be created".to_string(),
        ))
    }

    pub fn spawn_managed(config_path: &Path) -> Result<Self, HotkeyError> {
        let mut swhks_child = Self::spawn_swhks()?;

        if let Err(error) = Self::wait_for_swhks_socket() {
            Self::terminate_tracked_child("swhks", &mut swhks_child);
            return Err(error);
        }

        let mut swhkd = match Self::spawn_swhkd(config_path) {
            Ok(process) => process,
            Err(error) => {
                Self::terminate_tracked_child("swhks", &mut swhks_child);
                return Err(error);
            }
        };
        let swhkd_pid = swhkd.child.id() as i32;

        thread::sleep(Duration::from_millis(SWHKD_STARTUP_VERIFY_WAIT_MS));

        match swhkd.child.try_wait() {
            Ok(Some(status)) => {
                Self::terminate_tracked_child("swhks", &mut swhks_child);
                return Err(HotkeyError::Process(Self::format_startup_exit_message(
                    swhkd_pid,
                    status,
                    &swhkd.log_path,
                )));
            }
            Ok(None) => {}
            Err(err) => {
                Self::terminate_tracked_child("swhkd", &mut swhkd.child);
                Self::terminate_tracked_child("swhks", &mut swhks_child);
                return Err(HotkeyError::Process(format!(
                    "Could not verify swhkd startup state for PID {}: {}",
                    swhkd_pid, err
                )));
            }
        }

        if !Self::pid_is_live(swhkd_pid) {
            Self::terminate_tracked_child("swhkd", &mut swhkd.child);
            Self::terminate_tracked_child("swhks", &mut swhks_child);
            return Err(HotkeyError::Process(format!(
                "swhkd process (PID {}) is not running after startup.\n\
                 This usually indicates:\n\
                 • Permission issues with /dev/input devices\n\
                 • Another hotkey daemon is already running\n\
                 • Invalid configuration file\n\
                 Check logs: ~/.local/share/swhkd/*.log",
                swhkd_pid
            )));
        }

        info!("swhkd process verified running (PID: {})", swhkd_pid);

        Ok(Self {
            swhks_child: Some(swhks_child),
            swhkd_child: Some(swhkd.child),
            swhkd_pid,
            managed: true,
            monitor_running: Arc::new(AtomicBool::new(false)),
            swhkd_dead: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Kill any `swhkd`/`swhks` already running, then spawn our own pair.
    ///
    /// swhkd is setuid-root, so a hard exit leaves it under `systemd --user`
    /// still grabbing keys. Adopting an orphan means trusting it to reload
    /// config from an unknown state, so restart instead. Best-effort.
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

    fn spawn_swhkd_command(
        mut command: Command,
        launch_label: &str,
    ) -> Result<SpawnedSwhkd, HotkeyError> {
        let (log_file, log_path) = Self::create_spawn_log(launch_label)?;
        let stdout = log_file
            .try_clone()
            .map_err(|e| HotkeyError::Io(format!("Failed to prepare swhkd log: {e}")))?;

        command
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(log_file));
        let child = command
            .spawn()
            .map_err(|e| HotkeyError::Process(format!("Failed to spawn swhkd: {e}")))?;

        Ok(SpawnedSwhkd { child, log_path })
    }

    fn create_spawn_log(launch_label: &str) -> Result<(File, PathBuf), HotkeyError> {
        let uid = nix::unistd::getuid();
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("/run/user/{uid}")));
        let log_dir = if runtime_dir.is_dir() {
            runtime_dir.join("linux-soundboard")
        } else {
            std::env::temp_dir().join("linux-soundboard")
        };
        fs::create_dir_all(&log_dir)
            .map_err(|e| HotkeyError::Io(format!("Failed to create swhkd log dir: {e}")))?;

        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        let log_path = log_dir.join(format!(
            "swhkd-startup-{}-{}-{stamp}.log",
            std::process::id(),
            launch_label
        ));

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| HotkeyError::Io(format!("Failed to open swhkd log: {e}")))?;
        writeln!(file, "linux-soundboard: launching swhkd ({launch_label})").ok();

        Ok((file, log_path))
    }

    fn format_startup_exit_message(pid: i32, status: ExitStatus, log_path: &Path) -> String {
        let startup_log = Self::read_startup_log_tail(log_path);
        let lowercase_log = startup_log.to_ascii_lowercase();
        let remediation = if lowercase_log.contains("uinput") {
            // The /dev/uinput node exists even when the module is missing, so the
            // open fails with ENODEV rather than looking like a missing device.
            "swhkd could not open /dev/uinput, which it needs to read input devices.\n\
             If your kernel was updated since you last booted, reboot first — the running \
             kernel can no longer load any module.\n\
             Otherwise load it now and at every boot:\n\
             sudo modprobe uinput\n\
             echo uinput | sudo tee /etc/modules-load.d/uinput.conf"
        } else if lowercase_log.contains("launch the binary with pkexec") {
            "The installed swhkd build refuses direct launch. Use the Install swhkd button to rebuild the daemon with the Linux Soundboard helper."
        } else {
            "Run: sudo chown root:root \"$(command -v swhkd)\" && sudo chmod u+s \"$(command -v swhkd)\"\n\
             Or use the Install swhkd button to rebuild swhkd automatically."
        };

        format!(
            "swhkd exited immediately after startup (PID {}, status: {}).\n\
             Linux Soundboard needs a working swhkd daemon so it can read input devices.\n\
             swhkd startup log: {}\n\
             {}\n\
             {}",
            pid,
            status,
            log_path.display(),
            startup_log,
            remediation
        )
    }

    fn read_startup_log_tail(path: &Path) -> String {
        let Ok(text) = fs::read_to_string(path) else {
            return "No startup output was captured.".to_string();
        };
        let lines: Vec<&str> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect();
        if lines.is_empty() {
            return "No startup output was captured.".to_string();
        }
        let start = lines.len().saturating_sub(12);
        format!("Last swhkd output:\n{}", lines[start..].join("\n"))
    }

    pub fn pid_is_live(pid: i32) -> bool {
        if let Some(state) = Self::proc_stat_state(pid) {
            return state != 'Z';
        }

        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok()
    }

    fn proc_stat_state(pid: i32) -> Option<char> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        Self::parse_proc_stat_state(&stat)
    }

    fn parse_proc_stat_state(stat: &str) -> Option<char> {
        let after_name = stat.rsplit_once(") ")?;
        after_name.1.chars().next()
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

    /// Clear the swhks socket and swhkd pidfile a killed daemon leaves behind,
    /// or the new swhks dies binding its socket with `EADDRINUSE`.
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

                if !Self::pid_is_live(pid) {
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

impl Drop for SwhkdProcesses {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

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

    #[test]
    fn parses_proc_stat_state() {
        assert_eq!(
            SwhkdProcesses::parse_proc_stat_state("123 (swhkd) S 1 2 3"),
            Some('S')
        );
        assert_eq!(
            SwhkdProcesses::parse_proc_stat_state("123 (name with spaces) Z 1 2 3"),
            Some('Z')
        );
    }

    #[test]
    fn reads_startup_log_tail() {
        let path = std::env::temp_dir().join(format!(
            "lsb-swhkd-test-{}-{}.log",
            std::process::id(),
            "tail"
        ));
        fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let tail = SwhkdProcesses::read_startup_log_tail(&path);
        fs::remove_file(&path).ok();

        assert!(tail.contains("Last swhkd output:"));
        assert!(tail.contains("three"));
    }

    #[test]
    fn socket_readiness_requires_a_connectable_swhks_listener() {
        let directory =
            std::env::temp_dir().join(format!("lsb-swhks-readiness-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("swhkd.sock");
        fs::write(&socket, b"stale").unwrap();
        assert!(SwhkdProcesses::wait_for_swhks_socket_at(&socket, 1, 0).is_err());

        fs::remove_file(&socket).unwrap();
        let listener = UnixListener::bind(&socket).unwrap();
        assert!(SwhkdProcesses::wait_for_swhks_socket_at(&socket, 1, 0).is_ok());

        drop(listener);
        fs::remove_dir_all(directory).unwrap();
    }
}
