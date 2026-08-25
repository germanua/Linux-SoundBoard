use log::{debug, info, warn};
use parking_lot::Mutex;
use std::any::Any;
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::backend_runtime::{try_dispatch_hotkey, HotkeyBackend};
use super::error::{unsupported_key_for_backend, HotkeyError};
use super::parse_hotkey_spec;
use super::swhkd_config::SwhkdConfig;
use super::swhkd_install::{ensure_swhkd_binary_is_safe, missing_swhkd_message};
use super::swhkd_process::SwhkdProcesses;
use super::{
    SWHKD_PIPE_OPEN_RETRY_SECS, SWHKD_PIPE_REOPEN_DELAY_MS, SWHKD_RELOAD_POST_SIGNAL_WAIT_MS,
    SWHKD_RELOAD_PRE_SIGNAL_WAIT_MS,
};

const MAX_PIPE_LINE_BYTES: usize = 256;

#[derive(Debug, PartialEq, Eq)]
enum PipeRead {
    Eof,
    Binding(String),
    Rejected,
}

fn read_pipe_binding(reader: &mut impl BufRead) -> std::io::Result<PipeRead> {
    let mut value = Vec::with_capacity(MAX_PIPE_LINE_BYTES);
    let mut too_long = false;
    let mut saw_data = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            if !saw_data {
                return Ok(PipeRead::Eof);
            }
            break;
        }
        saw_data = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let data_len = newline.unwrap_or(available.len());
        if !too_long {
            let remaining = MAX_PIPE_LINE_BYTES.saturating_sub(value.len());
            value.extend_from_slice(&available[..data_len.min(remaining)]);
            too_long = data_len > remaining;
        }
        let consumed = data_len + usize::from(newline.is_some());
        reader.consume(consumed);
        if newline.is_some() {
            break;
        }
    }

    if too_long {
        return Ok(PipeRead::Rejected);
    }
    if value.last() == Some(&b'\r') {
        value.pop();
    }
    let Ok(binding_id) = std::str::from_utf8(&value) else {
        return Ok(PipeRead::Rejected);
    };
    if binding_id.is_empty()
        || !binding_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'-' | b'_'))
    {
        return Ok(PipeRead::Rejected);
    }
    Ok(PipeRead::Binding(binding_id.to_string()))
}

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
    pub fn new() -> Result<Self, HotkeyError> {
        info!("Initializing swhkd backend");

        let swhkd_path = which::which("swhkd")
            .map_err(|_| HotkeyError::BackendUnavailable(missing_swhkd_message("swhkd")))?;
        ensure_swhkd_binary_is_safe(&swhkd_path).map_err(HotkeyError::BackendUnavailable)?;

        if which::which("swhks").is_err() {
            return Err(HotkeyError::BackendUnavailable(missing_swhkd_message(
                "swhks",
            )));
        }

        let pipe_path = Self::create_hotkey_pipe()?;

        let mut config = SwhkdConfig::new(pipe_path.clone())?;

        if SwhkdProcesses::has_running_daemons() {
            warn!("Found pre-existing swhkd/swhks daemons; restarting them under app management");
            SwhkdProcesses::terminate_stale_daemons();
        }

        info!("Spawning swhkd/swhks processes");
        config.begin_projection()?;
        config.commit_projection()?;
        let processes = SwhkdProcesses::spawn_managed(&config.config_path)?;

        let processes_arc = Arc::new(Mutex::new(processes));

        processes_arc.lock().start_monitor();

        Ok(Self {
            processes: processes_arc,
            config: Arc::new(Mutex::new(config)),
            pipe_path,
            started: AtomicBool::new(false),
            listener_alive: Arc::new(AtomicBool::new(false)),
        })
    }

    fn create_hotkey_pipe() -> Result<PathBuf, HotkeyError> {
        let uid = nix::unistd::getuid();
        let runtime_dir = PathBuf::from(format!("/run/user/{}", uid));

        if !runtime_dir.exists() {
            return Err(HotkeyError::Io(format!(
                "Runtime directory does not exist: {}",
                runtime_dir.display()
            )));
        }

        let pipe_path = runtime_dir.join("lsb_hotkey.pipe");

        if pipe_path.exists() {
            fs::remove_file(&pipe_path)
                .map_err(|e| HotkeyError::Io(format!("Failed to remove old pipe: {}", e)))?;
        }

        // Let root-owned `swhkd` write to the pipe.
        nix::unistd::mkfifo(
            &pipe_path,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .map_err(|e| HotkeyError::Io(format!("Failed to create named pipe: {}", e)))?;

        info!("Created hotkey pipe at: {}", pipe_path.display());
        Ok(pipe_path)
    }

    fn reload_swhkd(&self) -> Result<(), HotkeyError> {
        let processes = self.processes.lock();

        thread::sleep(Duration::from_millis(SWHKD_RELOAD_PRE_SIGNAL_WAIT_MS));

        SwhkdConfig::reload_swhkd(processes.swhkd_pid)?;

        thread::sleep(Duration::from_millis(SWHKD_RELOAD_POST_SIGNAL_WAIT_MS));

        info!("swhkd config reload complete");
        Ok(())
    }

    fn reload_or_restore_last_good(&self) -> Result<(), HotkeyError> {
        let projection_result = self
            .reload_swhkd()
            .and_then(|_| self.verify_swhkd_running());
        let Err(projection_error) = projection_result else {
            return Ok(());
        };

        let restore_result = self
            .config
            .lock()
            .restore_last_good()
            .and_then(|_| self.reload_swhkd());
        match restore_result {
            Ok(()) => Err(HotkeyError::Process(format!(
                "swhkd rejected the new projection; restored the last-known-good config: {projection_error}"
            ))),
            Err(restore_error) => Err(HotkeyError::Process(format!(
                "swhkd projection failed ({projection_error}); restoring the last-known-good config also failed ({restore_error})"
            ))),
        }
    }

    fn validate_hotkey_binding(hotkey: &str) -> Result<(), HotkeyError> {
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
            .map_err(|detail| unsupported_key_for_backend("swhkd", detail.to_string()))
    }

    fn add_validated_hotkey_batch(
        config: &mut SwhkdConfig,
        bindings: &[(String, String)],
    ) -> Result<(), HotkeyError> {
        let mut failed = Vec::new();
        for (sound_id, hotkey) in bindings {
            match Self::validate_hotkey_binding(hotkey)
                .and_then(|_| config.stage_hotkey(sound_id, hotkey))
            {
                Ok(()) => {}
                Err(err) => failed.push(format!("{sound_id}={hotkey} ({err})")),
            }
        }

        if failed.is_empty() {
            Ok(())
        } else {
            Err(HotkeyError::Parse(format!(
                "Some hotkeys were skipped:\n{}",
                failed.join("\n")
            )))
        }
    }

    fn verify_swhkd_running(&self) -> Result<(), HotkeyError> {
        let processes = self.processes.lock();

        if SwhkdProcesses::pid_is_live(processes.swhkd_pid) {
            Ok(())
        } else {
            Err(HotkeyError::Process(format!(
                "swhkd process (PID {}) has crashed or exited.\n\
                 This usually happens due to:\n\
                 • Invalid hotkey configuration\n\
                 • Permission issues with /dev/input devices\n\
                 • Conflicting hotkey daemon already running\n\
                 Check logs: ~/.local/share/swhkd/*.log",
                processes.swhkd_pid
            )))
        }
    }

    pub fn is_healthy(&self) -> Result<(), HotkeyError> {
        if !self.listener_alive.load(Ordering::SeqCst) {
            return Err(HotkeyError::Process(
                "swhkd listener thread is not running".to_string(),
            ));
        }

        if let Err(e) = self.verify_swhkd_running() {
            return Err(HotkeyError::Process(format!(
                "swhkd process is not running: {}",
                e
            )));
        }

        Ok(())
    }
}

impl HotkeyBackend for SwhkdBackend {
    fn name(&self) -> &'static str {
        "swhkd"
    }

    fn validate_hotkey(&self, hotkey: &str) -> Result<(), HotkeyError> {
        Self::validate_hotkey_binding(hotkey)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn shutdown(&self) {
        info!("Shutting down swhkd backend");
        self.processes.lock().terminate();
    }

    fn register(&self, sound_id: &str, hotkey: &str) -> Result<(), HotkeyError> {
        let _ = (sound_id, hotkey);
        Err(HotkeyError::Process(
            "swhkd hotkeys must be updated through a complete projection".to_string(),
        ))
    }

    fn register_many(&self, bindings: &[(String, String)]) -> Result<(), HotkeyError> {
        self.begin_staged()?;
        let add_result = self.stage_many(bindings);
        self.commit_staged()?;
        add_result
    }

    fn begin_staged(&self) -> Result<(), HotkeyError> {
        self.config.lock().begin_projection()
    }

    fn stage_many(&self, bindings: &[(String, String)]) -> Result<(), HotkeyError> {
        let mut config = self.config.lock();
        if !config.projection_started() {
            config.begin_projection()?;
        }
        Self::add_validated_hotkey_batch(&mut config, bindings)
    }

    fn commit_staged(&self) -> Result<(), HotkeyError> {
        self.config.lock().commit_projection()?;
        self.reload_or_restore_last_good()
    }

    fn abort_staged(&self) {
        self.config.lock().abort_projection();
    }

    fn unregister(&self, sound_id: &str) -> Result<(), HotkeyError> {
        let _ = sound_id;
        Err(HotkeyError::Process(
            "swhkd hotkeys must be updated through a complete projection".to_string(),
        ))
    }

    fn unregister_many(&self, sound_ids: &[String]) -> Result<(), HotkeyError> {
        let _ = sound_ids;
        Err(HotkeyError::Process(
            "swhkd hotkeys must be updated through a complete projection".to_string(),
        ))
    }

    fn start_listener(&self, sender: SyncSender<String>) {
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
                let file = match File::options().read(true).write(true).open(&pipe_path) {
                    Ok(f) => f,
                    Err(e) => {
                        warn!("Failed to open hotkey pipe: {}", e);
                        thread::sleep(Duration::from_secs(SWHKD_PIPE_OPEN_RETRY_SECS));
                        continue;
                    }
                };

                let mut reader = BufReader::new(file);
                loop {
                    match read_pipe_binding(&mut reader) {
                        Ok(PipeRead::Binding(binding_id)) => {
                            debug!("swhkd hotkey triggered: {}", binding_id);
                            if !try_dispatch_hotkey(&sender, binding_id) {
                                debug!("Dropped swhkd hotkey repeat because the queue is full");
                            }
                        }
                        Ok(PipeRead::Rejected) => debug!("Rejected malformed swhkd pipe input"),
                        Ok(PipeRead::Eof) => break,
                        Err(error) => {
                            warn!("Error reading from hotkey pipe: {error}");
                            break;
                        }
                    }
                }

                debug!("Hotkey pipe closed, reopening...");
                thread::sleep(Duration::from_millis(SWHKD_PIPE_REOPEN_DELAY_MS));
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
    fn fifo_reader_caps_and_recovers_after_malformed_lines() {
        let payload = format!("{}\nvalid-id_2\n", "x".repeat(MAX_PIPE_LINE_BYTES + 500));
        let mut reader = BufReader::new(payload.as_bytes());

        assert_eq!(read_pipe_binding(&mut reader).unwrap(), PipeRead::Rejected);
        assert_eq!(
            read_pipe_binding(&mut reader).unwrap(),
            PipeRead::Binding("valid-id_2".to_string())
        );
        assert_eq!(read_pipe_binding(&mut reader).unwrap(), PipeRead::Eof);
    }

    #[test]
    fn fifo_reader_rejects_shell_and_path_characters() {
        for line in ["../sound\n", "sound;touch /tmp/x\n", "sound id\n"] {
            let mut reader = BufReader::new(line.as_bytes());
            assert_eq!(read_pipe_binding(&mut reader).unwrap(), PipeRead::Rejected);
        }
    }

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
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl+CapsLock").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("PrintScreen").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("MediaPlayPause").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("AudioVolumeUp").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("BrightnessDown").is_ok());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl+NumpadEqual").is_ok());
    }

    #[test]
    fn test_validate_hotkey_rejects_unsupported_swhkd_key() {
        let err = SwhkdBackend::validate_hotkey_binding("Ctrl+NumpadDivide").unwrap_err();
        assert_eq!(
            err.to_string(),
            "UNSUPPORTED_KEY_FOR_BACKEND:swhkd:Ctrl+NumpadDivide cannot be represented by swhkd."
        );
    }

    #[test]
    fn test_validate_hotkey_invalid() {
        assert!(SwhkdBackend::validate_hotkey_binding("").is_err());
        assert!(SwhkdBackend::validate_hotkey_binding("   ").is_err());
        assert!(SwhkdBackend::validate_hotkey_binding("Ctrl++KeyA").is_err());
    }

    #[test]
    fn register_many_batch_adds_all_bindings() {
        let dir = std::env::temp_dir().join(format!("lsb-swhkd-batch-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let mut config = SwhkdConfig::for_paths(dir.join("swhkdrc"), dir.join("test.pipe"));
        config.begin_projection().unwrap();
        let bindings = vec![
            ("sound-1".to_string(), "Ctrl+KeyA".to_string()),
            ("sound-2".to_string(), "Alt+KeyB".to_string()),
        ];

        SwhkdBackend::add_validated_hotkey_batch(&mut config, &bindings).unwrap();
        assert_eq!(config.commit_projection().unwrap(), 2);
        let rendered = fs::read_to_string(&config.config_path).unwrap();
        assert!(rendered.contains("ctrl + ~a"));
        assert!(rendered.contains("alt + ~b"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn register_many_batch_skips_invalid_without_dropping_valid_bindings() {
        let dir = std::env::temp_dir().join(format!("lsb-swhkd-partial-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let mut config = SwhkdConfig::for_paths(dir.join("swhkdrc"), dir.join("test.pipe"));
        config.begin_projection().unwrap();
        let bindings = vec![
            ("sound-1".to_string(), "Ctrl+KeyA".to_string()),
            ("sound-2".to_string(), "Ctrl+NumpadDivide".to_string()),
        ];

        let err = SwhkdBackend::add_validated_hotkey_batch(&mut config, &bindings).unwrap_err();

        assert!(err.to_string().contains("Some hotkeys were skipped"));
        assert!(err.to_string().contains("sound-2=Ctrl+NumpadDivide"));
        assert_eq!(config.commit_projection().unwrap(), 1);
        let rendered = fs::read_to_string(&config.config_path).unwrap();
        assert!(rendered.contains("ctrl + ~a"));
        assert!(!rendered.contains("sound-2"));
        fs::remove_dir_all(dir).unwrap();
    }
}
