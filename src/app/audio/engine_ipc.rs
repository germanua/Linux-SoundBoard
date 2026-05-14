use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::audio::player::PlayerSnapshot;
use crate::config::{DefaultSourceMode, MicLatencyProfile, CURRENT_SCHEMA_VERSION};

const IPC_TIMEOUT: Duration = Duration::from_secs(3);
const ENGINE_DIR_NAME: &str = "linux-soundboard";
const ENGINE_SOCKET_NAME: &str = "engine.sock";
pub const ENGINE_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineRequest {
    Info,
    Ping,
    Snapshot,
    Play {
        sound_id: String,
        path: String,
        base_volume: f32,
        sound_lufs: Option<f64>,
    },
    /// Stop all active sounds then start a new one as a single atomic operation.
    /// Prevents snapshot polls from observing an intermediate "all stopped" state.
    PlayReplace {
        sound_id: String,
        path: String,
        base_volume: f32,
        sound_lufs: Option<f64>,
    },
    StopSound {
        sound_id: String,
    },
    StopAll,
    Seek {
        play_id: String,
        position_ms: u64,
    },
    Pause {
        sound_id: String,
    },
    Resume {
        sound_id: String,
    },
    SetLocalVolume {
        volume: f32,
    },
    SetMicVolume {
        volume: f32,
    },
    SetAutoGainEnabled {
        enabled: bool,
    },
    SetAutoGainTarget {
        target_lufs: f64,
    },
    SetAutoGainMode {
        mode: u32,
    },
    SetAutoGainApplyTo {
        apply_to: u32,
    },
    SetAutoGainDynamicSettings {
        lookahead_ms: u32,
        attack_ms: u32,
        release_ms: u32,
    },
    SetLooping {
        enabled: bool,
    },
    SetMicPassthrough {
        enabled: bool,
    },
    SetMicSource {
        source: Option<String>,
    },
    SetDefaultSourceMode {
        mode: DefaultSourceMode,
    },
    SetMicLatencyProfile {
        profile: MicLatencyProfile,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineResponse {
    Ok,
    Info {
        engine_protocol_version: u32,
        app_version: String,
        config_schema_version: u32,
        binary_path: String,
    },
    Pong,
    PlayId {
        play_id: String,
    },
    Snapshot {
        snapshot: PlayerSnapshot,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineInfo {
    pub engine_protocol_version: u32,
    pub app_version: String,
    pub config_schema_version: u32,
    pub binary_path: String,
}

pub enum BindEngineSocket {
    Listener(UnixListener),
    AlreadyRunning,
}

pub fn engine_socket_path() -> PathBuf {
    engine_socket_path_for(std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
}

pub fn engine_socket_path_for(runtime_dir: Option<PathBuf>) -> PathBuf {
    let base = runtime_dir.unwrap_or_else(|| {
        let user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
        std::env::temp_dir().join(format!("{ENGINE_DIR_NAME}-{user}"))
    });
    base.join(ENGINE_DIR_NAME).join(ENGINE_SOCKET_NAME)
}

pub fn engine_running() -> bool {
    matches!(send_request(EngineRequest::Ping), Ok(EngineResponse::Pong))
}

pub fn engine_info() -> Result<EngineInfo, String> {
    engine_info_at(&engine_socket_path())
}

pub fn engine_info_at(path: &Path) -> Result<EngineInfo, String> {
    match send_request_to(path, EngineRequest::Info)? {
        EngineResponse::Info {
            engine_protocol_version,
            app_version,
            config_schema_version,
            binary_path,
        } => Ok(EngineInfo {
            engine_protocol_version,
            app_version,
            config_schema_version,
            binary_path,
        }),
        EngineResponse::Error { message } => Err(message),
        other => Err(format!("Unexpected engine info response: {other:?}")),
    }
}

pub fn engine_info_compatible(info: &EngineInfo) -> bool {
    info.engine_protocol_version == ENGINE_PROTOCOL_VERSION
        && info.config_schema_version == CURRENT_SCHEMA_VERSION
}

pub fn compatible_engine_running() -> bool {
    matches!(engine_info(), Ok(info) if engine_info_compatible(&info))
}

pub fn shutdown_incompatible_engine_if_running() -> bool {
    let path = engine_socket_path();
    shutdown_incompatible_engine_at(&path, Duration::from_secs(3))
}

pub fn shutdown_incompatible_engine_at(path: &Path, timeout: Duration) -> bool {
    if !path.exists() {
        return false;
    }

    match engine_info_at(path) {
        Ok(info) if engine_info_compatible(&info) => return false,
        Ok(info) => {
            log::warn!(
                "Stopping incompatible Linux Soundboard audio engine: protocol={} schema={} binary={}",
                info.engine_protocol_version,
                info.config_schema_version,
                info.binary_path
            );
        }
        Err(err) => {
            if !engine_responds_at(path) {
                return false;
            }
            log::warn!("Stopping old Linux Soundboard audio engine without compatible info: {err}");
        }
    }

    let _ = send_request_to(path, EngineRequest::Shutdown);
    wait_for_engine_stop(path, timeout)
}

pub fn bind_engine_socket() -> Result<BindEngineSocket, String> {
    bind_engine_socket_at(&engine_socket_path())
}

pub fn bind_engine_socket_at(path: &Path) -> Result<BindEngineSocket, String> {
    if path.exists() {
        if engine_responds_at(path) {
            return Ok(BindEngineSocket::AlreadyRunning);
        }
        fs::remove_file(path).map_err(|e| {
            format!(
                "Failed to remove stale engine socket {}: {e}",
                path.display()
            )
        })?;
    }

    let Some(dir) = path.parent() else {
        return Err(format!(
            "Engine socket path has no parent: {}",
            path.display()
        ));
    };
    fs::create_dir_all(dir)
        .map_err(|e| format!("Failed to create engine socket dir {}: {e}", dir.display()))?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("Failed to protect engine socket dir {}: {e}", dir.display()))?;

    let listener = UnixListener::bind(path)
        .map_err(|e| format!("Failed to bind engine socket {}: {e}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("Failed to protect engine socket {}: {e}", path.display()))?;
    Ok(BindEngineSocket::Listener(listener))
}

pub fn send_request(request: EngineRequest) -> Result<EngineResponse, String> {
    send_request_to(&engine_socket_path(), request)
}

pub fn send_request_to(path: &Path, request: EngineRequest) -> Result<EngineResponse, String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| format!("Failed to connect to engine socket {}: {e}", path.display()))?;
    stream
        .set_read_timeout(Some(IPC_TIMEOUT))
        .map_err(|e| format!("Failed to set engine socket read timeout: {e}"))?;
    stream
        .set_write_timeout(Some(IPC_TIMEOUT))
        .map_err(|e| format!("Failed to set engine socket write timeout: {e}"))?;

    write_message(&mut stream, &request)?;
    read_response(stream)
}

pub fn write_response(stream: &mut UnixStream, response: &EngineResponse) -> Result<(), String> {
    write_message(stream, response)
}

pub fn parse_request(line: &str) -> Result<EngineRequest, String> {
    serde_json::from_str(line).map_err(|e| format!("Invalid engine request: {e}"))
}

fn engine_responds_at(path: &Path) -> bool {
    matches!(
        send_request_to(path, EngineRequest::Ping),
        Ok(EngineResponse::Pong)
    )
}

fn wait_for_engine_stop(path: &Path, timeout: Duration) -> bool {
    let started_at = Instant::now();
    while started_at.elapsed() < timeout {
        if !path.exists() || !engine_responds_at(path) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn write_message<T: Serialize>(stream: &mut UnixStream, message: &T) -> Result<(), String> {
    serde_json::to_writer(&mut *stream, message)
        .map_err(|e| format!("Failed to encode engine message: {e}"))?;
    stream
        .write_all(b"\n")
        .map_err(|e| format!("Failed to write engine message: {e}"))?;
    stream
        .flush()
        .map_err(|e| format!("Failed to flush engine message: {e}"))
}

fn read_response(stream: UnixStream) -> Result<EngineResponse, String> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .map_err(|e| format!("Failed to read engine response: {e}"))?;
    if read == 0 {
        return Err("Engine closed the socket without a response".to_string());
    }
    serde_json::from_str(line.trim_end()).map_err(|e| format!("Invalid engine response: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_uses_runtime_dir() {
        let path = engine_socket_path_for(Some(PathBuf::from("/run/user/1000")));
        assert_eq!(
            path,
            PathBuf::from("/run/user/1000/linux-soundboard/engine.sock")
        );
    }

    #[test]
    fn parses_known_request_and_rejects_unknown() {
        let request = parse_request(r#"{"type":"set_mic_volume","volume":0.75}"#).unwrap();
        assert!(matches!(request, EngineRequest::SetMicVolume { .. }));

        let request = parse_request(r#"{"type":"info"}"#).unwrap();
        assert!(matches!(request, EngineRequest::Info));

        let err = parse_request(r#"{"type":"not_real"}"#).unwrap_err();
        assert!(err.contains("Invalid engine request"));
    }

    #[test]
    fn engine_info_compatibility_requires_current_protocol_and_schema() {
        let current = EngineInfo {
            engine_protocol_version: ENGINE_PROTOCOL_VERSION,
            app_version: "test".to_string(),
            config_schema_version: CURRENT_SCHEMA_VERSION,
            binary_path: "/tmp/linux-soundboard".to_string(),
        };
        assert!(engine_info_compatible(&current));

        let mut old_protocol = current.clone();
        old_protocol.engine_protocol_version = ENGINE_PROTOCOL_VERSION.saturating_sub(1);
        assert!(!engine_info_compatible(&old_protocol));

        let mut old_schema = current;
        old_schema.config_schema_version = CURRENT_SCHEMA_VERSION.saturating_sub(1);
        assert!(!engine_info_compatible(&old_schema));
    }

    #[test]
    fn bind_detects_already_running_engine() {
        let dir = std::env::temp_dir().join(format!("lsb-engine-ipc-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(ENGINE_SOCKET_NAME);
        let listener = UnixListener::bind(&path).unwrap();

        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            let request = parse_request(line.trim_end()).unwrap();
            assert!(matches!(request, EngineRequest::Ping));
            write_response(&mut stream, &EngineResponse::Pong).unwrap();
        });

        let result = bind_engine_socket_at(&path).unwrap();
        assert!(matches!(result, BindEngineSocket::AlreadyRunning));
        handle.join().unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
