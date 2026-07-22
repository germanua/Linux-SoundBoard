use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use log::{error, info, warn};

use crate::app_meta::APP_VERSION;
use crate::audio::engine_ipc::{self, BindEngineSocket, EngineRequest, EngineResponse};
use crate::audio::player::ShutdownPolicy;
use crate::audio::{AudioBackendKind, AudioPlayer, EngineError};
use crate::config::{Config, CURRENT_SCHEMA_VERSION};

pub fn run() -> i32 {
    match engine_ipc::bind_engine_socket() {
        Ok(BindEngineSocket::AlreadyRunning) => {
            info!("Linux Soundboard audio engine is already running");
            0
        }
        Ok(BindEngineSocket::Listener(listener)) => {
            if let Err(err) = listener.set_nonblocking(true) {
                warn!("Failed to set audio engine socket nonblocking mode: {err}");
            }

            let config = match Config::load_runtime_settings() {
                Ok(config) => config,
                Err(err) => {
                    error!(
                        "Refusing to start audio engine with unreadable config '{}': {err}",
                        Config::config_path().display()
                    );
                    drop(listener);
                    remove_engine_socket();
                    return 2;
                }
            };
            let player = Arc::new(init_player(&config));
            let stop = Arc::new(AtomicBool::new(false));

            info!(
                "Linux Soundboard audio engine listening at {}",
                engine_ipc::engine_socket_path().display()
            );

            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        let player = Arc::clone(&player);
                        let stop = Arc::clone(&stop);
                        let _ = thread::Builder::new()
                            .name("lsb-engine-client".to_string())
                            .spawn(move || handle_client(stream, player, stop));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(err) => {
                        warn!("Audio engine socket accept failed: {err}");
                        thread::sleep(Duration::from_millis(100));
                    }
                }
            }

            player.shutdown();
            drop(listener);
            remove_engine_socket();
            0
        }
        Err(err) => {
            error!("{err}");
            1
        }
    }
}

fn remove_engine_socket() {
    let path = engine_ipc::engine_socket_path();
    if let Err(err) = std::fs::remove_file(&path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            warn!(
                "Failed to remove audio engine socket '{}': {err}",
                path.display()
            );
        }
    }
}

fn init_player(config: &Config) -> AudioPlayer {
    super::legacy_cleanup::cleanup_legacy_audio_routing_artifacts();
    let backend = if crate::audio::pipewire_detection::check_pipewire().available {
        AudioBackendKind::PipeWire
    } else {
        AudioBackendKind::PulseAudio
    };
    AudioPlayer::new_with_config_audio_backend_and_shutdown_policy(
        config,
        backend,
        ShutdownPolicy::Persistent,
    )
}

fn handle_client(stream: UnixStream, player: Arc<AudioPlayer>, stop: Arc<AtomicBool>) {
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(err) => {
            warn!("Failed to clone engine client socket: {err}");
            return;
        }
    };
    let mut reader = BufReader::new(stream);

    loop {
        let mut line = String::new();
        let read = match reader.read_line(&mut line) {
            Ok(read) => read,
            Err(err) => {
                warn!("Failed to read engine client request: {err}");
                return;
            }
        };
        if read == 0 {
            return;
        }

        let response = match engine_ipc::parse_request(line.trim_end()) {
            Ok(request) => handle_request(request, &player, &stop),
            Err(e) => EngineResponse::Error {
                message: e.to_string(),
            },
        };
        if let Err(err) = engine_ipc::write_response(&mut writer, &response) {
            warn!("Failed to write engine client response: {err}");
            return;
        }
    }
}

fn handle_request(
    request: EngineRequest,
    player: &AudioPlayer,
    stop: &AtomicBool,
) -> EngineResponse {
    match request {
        EngineRequest::Info => EngineResponse::Info {
            engine_protocol_version: engine_ipc::ENGINE_PROTOCOL_VERSION,
            app_version: APP_VERSION.to_string(),
            config_schema_version: CURRENT_SCHEMA_VERSION,
            binary_path: std::env::var_os("APPIMAGE")
                .map(|path| std::path::PathBuf::from(path).display().to_string())
                .or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .map(|path| path.display().to_string())
                })
                .unwrap_or_else(|| "unknown".to_string()),
        },
        EngineRequest::Ping => EngineResponse::Pong,
        EngineRequest::Snapshot => EngineResponse::Snapshot {
            snapshot: player.snapshot(),
        },
        EngineRequest::Play {
            sound_id,
            path,
            base_volume,
            sound_lufs,
            sound_true_peak_dbtp,
        } => match player.play(
            &sound_id,
            &path,
            base_volume,
            sound_lufs,
            sound_true_peak_dbtp,
        ) {
            Ok(play_id) => EngineResponse::PlayId { play_id },
            Err(e) => EngineResponse::Error {
                message: e.to_string(),
            },
        },
        EngineRequest::PlayReplace {
            sound_id,
            path,
            base_volume,
            sound_lufs,
            sound_true_peak_dbtp,
        } => {
            player.stop_all();
            match player.play(
                &sound_id,
                &path,
                base_volume,
                sound_lufs,
                sound_true_peak_dbtp,
            ) {
                Ok(play_id) => EngineResponse::PlayId { play_id },
                Err(e) => EngineResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        EngineRequest::StopSound { sound_id } => result_to_response(player.stop_sound(&sound_id)),
        EngineRequest::StopAll => {
            player.stop_all();
            EngineResponse::Ok
        }
        EngineRequest::Seek {
            play_id,
            position_ms,
        } => {
            player.seek_playback(&play_id, position_ms);
            EngineResponse::Ok
        }
        EngineRequest::Pause { sound_id } => {
            player.pause(&sound_id);
            EngineResponse::Ok
        }
        EngineRequest::Resume { sound_id } => {
            player.resume(&sound_id);
            EngineResponse::Ok
        }
        EngineRequest::SetLocalVolume { volume } => {
            player.set_local_volume(volume);
            EngineResponse::Ok
        }
        EngineRequest::SetMicVolume { volume } => {
            player.set_mic_volume(volume);
            EngineResponse::Ok
        }
        EngineRequest::SetAutoGainEnabled { enabled } => {
            player.set_auto_gain_enabled(enabled);
            EngineResponse::Ok
        }
        EngineRequest::SetAutoGainTarget { target_lufs } => {
            player.set_auto_gain_target(target_lufs);
            EngineResponse::Ok
        }
        EngineRequest::SetAutoGainMode { mode } => {
            player.set_auto_gain_mode(mode);
            EngineResponse::Ok
        }
        EngineRequest::SetAutoGainApplyTo { apply_to } => {
            player.set_auto_gain_apply_to(apply_to);
            EngineResponse::Ok
        }
        EngineRequest::SetAutoGainDynamicSettings {
            lookahead_ms,
            attack_ms,
            release_ms,
        } => {
            player.set_auto_gain_dynamic_settings(lookahead_ms, attack_ms, release_ms);
            EngineResponse::Ok
        }
        EngineRequest::SetLooping { enabled } => {
            player.set_looping(enabled);
            EngineResponse::Ok
        }
        EngineRequest::SetMicPassthrough { enabled } => {
            result_to_response(player.set_mic_passthrough(enabled))
        }
        EngineRequest::SetMicSource { source } => result_to_response(player.set_mic_source(source)),
        EngineRequest::SetDefaultSourceMode { mode } => {
            result_to_response(player.set_default_source_mode(mode))
        }
        EngineRequest::SetMicLatencyProfile { profile } => {
            result_to_response(player.set_mic_latency_profile(profile))
        }
        EngineRequest::Shutdown {
            requester_version,
            expected_engine_version,
            expected_protocol_version,
            expected_config_schema_version,
        } => {
            if shutdown_request_is_authorized(
                requester_version.as_deref(),
                expected_engine_version.as_deref(),
                expected_protocol_version,
                expected_config_schema_version,
            ) {
                stop.store(true, Ordering::Relaxed);
                EngineResponse::Ok
            } else {
                EngineResponse::Error {
                    message: "Rejected an unscoped or stale engine shutdown request".to_string(),
                }
            }
        }
    }
}

fn shutdown_request_is_authorized(
    requester_version: Option<&str>,
    expected_engine_version: Option<&str>,
    expected_protocol_version: Option<u32>,
    expected_config_schema_version: Option<u32>,
) -> bool {
    requester_version.is_some_and(|requester| version_at_least(requester, APP_VERSION))
        && expected_engine_version == Some(APP_VERSION)
        && expected_protocol_version == Some(engine_ipc::ENGINE_PROTOCOL_VERSION)
        && expected_config_schema_version == Some(CURRENT_SCHEMA_VERSION)
}

fn version_at_least(candidate: &str, minimum: &str) -> bool {
    fn core(version: &str) -> Option<(u64, u64, u64)> {
        let mut parts = version.split('-').next()?.split('.');
        let parsed = (
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
            parts.next()?.parse().ok()?,
        );
        parts.next().is_none().then_some(parsed)
    }

    matches!((core(candidate), core(minimum)), (Some(candidate), Some(minimum)) if candidate >= minimum)
}

fn result_to_response(result: Result<(), EngineError>) -> EngineResponse {
    match result {
        Ok(()) => EngineResponse::Ok,
        Err(e) => EngineResponse::Error {
            message: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_unscoped_shutdown_cannot_stop_a_newer_engine() {
        let player = AudioPlayer::new_test_noop();
        let stop = AtomicBool::new(false);

        let response = handle_request(
            EngineRequest::Shutdown {
                requester_version: None,
                expected_engine_version: None,
                expected_protocol_version: None,
                expected_config_schema_version: None,
            },
            &player,
            &stop,
        );

        assert!(matches!(response, EngineResponse::Error { .. }));
        assert!(!stop.load(Ordering::Relaxed));
    }

    #[test]
    fn exact_scoped_shutdown_from_current_or_newer_version_is_accepted() {
        assert!(shutdown_request_is_authorized(
            Some(APP_VERSION),
            Some(APP_VERSION),
            Some(engine_ipc::ENGINE_PROTOCOL_VERSION),
            Some(CURRENT_SCHEMA_VERSION),
        ));
        assert!(shutdown_request_is_authorized(
            Some("99.0.0"),
            Some(APP_VERSION),
            Some(engine_ipc::ENGINE_PROTOCOL_VERSION),
            Some(CURRENT_SCHEMA_VERSION),
        ));
        assert!(!shutdown_request_is_authorized(
            Some("2.1.1"),
            Some(APP_VERSION),
            Some(engine_ipc::ENGINE_PROTOCOL_VERSION),
            Some(CURRENT_SCHEMA_VERSION),
        ));
    }
}
