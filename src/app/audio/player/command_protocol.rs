//! Commands sent from AudioPlayer to the PipeWire loop.

use super::*;

pub(super) enum AudioCommand {
    Play {
        sound_id: String,
        path: String,
        base_volume: f32,
        sound_lufs: Option<f64>,
        sound_true_peak_dbtp: Option<f32>,
        response: Sender<Result<String, EngineError>>,
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
    SetLoudnessBoostEnabled {
        enabled: bool,
    },
    SetLoudnessBoostDb {
        boost_db: f64,
    },
    SetLooping {
        enabled: bool,
    },
    SetMicPassthrough {
        enabled: bool,
        response: Sender<Result<(), EngineError>>,
    },
    SetMicSource {
        source: Option<String>,
        response: Sender<Result<(), EngineError>>,
    },
    SetDefaultSourceMode {
        mode: DefaultSourceMode,
        response: Sender<Result<(), EngineError>>,
    },
    SetMicLatencyProfile {
        profile: MicLatencyProfile,
        response: Sender<Result<(), EngineError>>,
    },
    Shutdown {
        policy: ShutdownPolicy,
        response: Sender<()>,
    },
}
