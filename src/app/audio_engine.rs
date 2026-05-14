use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixStream;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;

use log::{info, warn};

use crate::app_meta::APP_VERSION;
use crate::audio::engine_ipc::{self, BindEngineSocket, EngineRequest, EngineResponse};
use crate::audio::player::AudioPlayer;
use crate::config::{Config, DefaultSourceMode, CURRENT_SCHEMA_VERSION};

pub fn run() -> i32 {
    match engine_ipc::bind_engine_socket() {
        Ok(BindEngineSocket::AlreadyRunning) => {
            info!("Linux Soundboard audio engine is already running");
            return 0;
        }
        Ok(BindEngineSocket::Listener(listener)) => {
            if let Err(err) = listener.set_nonblocking(true) {
                warn!("Failed to set audio engine socket nonblocking mode: {err}");
            }

            let config = load_config();
            let player = Arc::new(crate::init::init_player(&config));
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
            0
        }
        Err(err) => {
            eprintln!("{err}");
            1
        }
    }
}

fn load_config() -> Config {
    match Config::load() {
        Ok(config) => config,
        Err(err) => {
            warn!(
                "Failed to load config from '{}': {}. Starting audio engine in fail-closed routing mode.",
                Config::config_path().display(),
                err
            );
            let mut config = Config::default();
            config.settings.default_source_mode = DefaultSourceMode::Manual;
            config
        }
    }
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
            Err(message) => EngineResponse::Error { message },
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
            binary_path: std::env::current_exe()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
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
        } => match player.play(&sound_id, &path, base_volume, sound_lufs) {
            Ok(play_id) => EngineResponse::PlayId { play_id },
            Err(message) => EngineResponse::Error { message },
        },
        EngineRequest::PlayReplace {
            sound_id,
            path,
            base_volume,
            sound_lufs,
        } => {
            player.stop_all();
            match player.play(&sound_id, &path, base_volume, sound_lufs) {
                Ok(play_id) => EngineResponse::PlayId { play_id },
                Err(message) => EngineResponse::Error { message },
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
        EngineRequest::Shutdown => {
            stop.store(true, Ordering::Relaxed);
            EngineResponse::Ok
        }
    }
}

fn result_to_response(result: Result<(), String>) -> EngineResponse {
    match result {
        Ok(()) => EngineResponse::Ok,
        Err(message) => EngineResponse::Error { message },
    }
}
