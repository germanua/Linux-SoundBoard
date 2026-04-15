use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;
use uuid::Uuid;

use crate::config::defaults::{
    default_allow_multiple_playbacks, default_auto_gain_attack_ms, default_auto_gain_lookahead_ms,
    default_auto_gain_release_ms, default_auto_gain_target,
};

macro_rules! impl_string_serde_enum {
    ($type_name:ty) => {
        impl Serialize for $type_name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $type_name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Ok(<$type_name>::from_str(&value).unwrap_or_default())
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

impl FromStr for Theme {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dark" => Ok(Self::Dark),
            "light" => Ok(Self::Light),
            _ => Err(()),
        }
    }
}
impl_string_serde_enum!(Theme);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AutoGainMode {
    #[default]
    Static,
    Dynamic,
}

impl AutoGainMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dynamic => "dynamic",
        }
    }

    pub const fn player_value(self) -> u32 {
        match self {
            Self::Static => 0,
            Self::Dynamic => 1,
        }
    }
}

impl FromStr for AutoGainMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "static" => Ok(Self::Static),
            "dynamic" => Ok(Self::Dynamic),
            _ => Err(()),
        }
    }
}
impl_string_serde_enum!(AutoGainMode);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AutoGainApplyTo {
    Both,
    #[default]
    MicOnly,
}

impl AutoGainApplyTo {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::MicOnly => "mic_only",
        }
    }

    pub const fn player_value(self) -> u32 {
        match self {
            Self::Both => 0,
            Self::MicOnly => 1,
        }
    }
}

impl FromStr for AutoGainApplyTo {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "both" => Ok(Self::Both),
            "mic_only" => Ok(Self::MicOnly),
            _ => Err(()),
        }
    }
}
impl_string_serde_enum!(AutoGainApplyTo);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PlayMode {
    #[default]
    Default,
    Loop,
    Continue,
}

impl PlayMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Loop => "loop",
            Self::Continue => "continue",
        }
    }

    pub const fn should_loop(self) -> bool {
        matches!(self, Self::Loop)
    }

    pub const fn next(self) -> Self {
        match self {
            Self::Default => Self::Loop,
            Self::Loop => Self::Continue,
            Self::Continue => Self::Default,
        }
    }
}

impl FromStr for PlayMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "default" => Ok(Self::Default),
            "loop" => Ok(Self::Loop),
            "continue" => Ok(Self::Continue),
            _ => Err(()),
        }
    }
}
impl_string_serde_enum!(PlayMode);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ListStyle {
    #[default]
    Compact,
    Card,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DefaultSourceMode {
    #[default]
    Manual,
    AutoWhileRunning,
}

impl DefaultSourceMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::AutoWhileRunning => "auto_while_running",
        }
    }
}

impl FromStr for DefaultSourceMode {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "manual" => Ok(Self::Manual),
            "auto_while_running" => Ok(Self::AutoWhileRunning),
            _ => Err(()),
        }
    }
}
impl_string_serde_enum!(DefaultSourceMode);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MicLatencyProfile {
    #[default]
    Balanced,
    Low,
    Ultra,
}

impl MicLatencyProfile {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Balanced => "balanced",
            Self::Low => "low",
            Self::Ultra => "ultra",
        }
    }

    pub const fn player_value(self) -> u32 {
        match self {
            Self::Balanced => 0,
            Self::Low => 1,
            Self::Ultra => 2,
        }
    }

    pub const fn from_player_value(value: u32) -> Self {
        match value {
            1 => Self::Low,
            2 => Self::Ultra,
            _ => Self::Balanced,
        }
    }
}

impl FromStr for MicLatencyProfile {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "balanced" => Ok(Self::Balanced),
            "low" => Ok(Self::Low),
            "ultra" => Ok(Self::Ultra),
            _ => Err(()),
        }
    }
}
impl_string_serde_enum!(MicLatencyProfile);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LoudnessAnalysisState {
    #[default]
    Pending,
    Estimated,
    Refined,
    Unavailable,
}

impl LoudnessAnalysisState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Estimated => "estimated",
            Self::Refined => "refined",
            Self::Unavailable => "unavailable",
        }
    }
}

impl FromStr for LoudnessAnalysisState {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "estimated" => Ok(Self::Estimated),
            "refined" => Ok(Self::Refined),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(()),
        }
    }
}
impl_string_serde_enum!(LoudnessAnalysisState);

fn default_default_source_mode() -> DefaultSourceMode {
    DefaultSourceMode::Manual
}

impl ListStyle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Card => "card",
        }
    }
}

impl FromStr for ListStyle {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => Ok(Self::Compact),
            "card" => Ok(Self::Card),
            _ => Err(()),
        }
    }
}
impl_string_serde_enum!(ListStyle);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlHotkeyAction {
    PlayPause,
    StopAll,
    PreviousSound,
    NextSound,
    MuteHeadphones,
    MuteRealMic,
    CyclePlayMode,
}

pub const CONTROL_BINDING_PREFIX: &str = "control:";

#[derive(Debug, Clone, Copy)]
pub struct ControlHotkeyActionMeta {
    pub action: ControlHotkeyAction,
    pub id: &'static str,
    pub binding_id: &'static str,
    pub title: &'static str,
    pub subtitle: &'static str,
}

pub const CONTROL_HOTKEY_ACTIONS: &[ControlHotkeyActionMeta] = &[
    ControlHotkeyActionMeta {
        action: ControlHotkeyAction::PlayPause,
        id: "play_pause",
        binding_id: "control:play_pause",
        title: "Play / Pause",
        subtitle: "Toggle playback of the active sound",
    },
    ControlHotkeyActionMeta {
        action: ControlHotkeyAction::StopAll,
        id: "stop_all",
        binding_id: "control:stop_all",
        title: "Stop All",
        subtitle: "Stop all currently playing sounds",
    },
    ControlHotkeyActionMeta {
        action: ControlHotkeyAction::PreviousSound,
        id: "previous_sound",
        binding_id: "control:previous_sound",
        title: "Previous Sound",
        subtitle: "Play the previous sound in the list",
    },
    ControlHotkeyActionMeta {
        action: ControlHotkeyAction::NextSound,
        id: "next_sound",
        binding_id: "control:next_sound",
        title: "Next Sound",
        subtitle: "Play the next sound in the list",
    },
    ControlHotkeyActionMeta {
        action: ControlHotkeyAction::MuteHeadphones,
        id: "mute_headphones",
        binding_id: "control:mute_headphones",
        title: "Mute Headphones",
        subtitle: "Toggle headphone output",
    },
    ControlHotkeyActionMeta {
        action: ControlHotkeyAction::MuteRealMic,
        id: "mute_real_mic",
        binding_id: "control:mute_real_mic",
        title: "Mute Real Mic",
        subtitle: "Toggle real microphone passthrough",
    },
    ControlHotkeyActionMeta {
        action: ControlHotkeyAction::CyclePlayMode,
        id: "cycle_play_mode",
        binding_id: "control:cycle_play_mode",
        title: "Cycle Play Mode",
        subtitle: "Cycle between default / loop / continue",
    },
];

impl ControlHotkeyAction {
    pub fn all() -> &'static [ControlHotkeyActionMeta] {
        CONTROL_HOTKEY_ACTIONS
    }

    pub fn metadata(self) -> &'static ControlHotkeyActionMeta {
        CONTROL_HOTKEY_ACTIONS
            .iter()
            .find(|meta| meta.action == self)
            .expect("control hotkey metadata missing")
    }

    pub fn id(self) -> &'static str {
        self.metadata().id
    }

    pub fn binding_id(self) -> &'static str {
        self.metadata().binding_id
    }

    pub fn title(self) -> &'static str {
        self.metadata().title
    }

    pub fn subtitle(self) -> &'static str {
        self.metadata().subtitle
    }

    pub fn from_id(value: &str) -> Option<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        CONTROL_HOTKEY_ACTIONS
            .iter()
            .find(|meta| meta.id == normalized)
            .map(|meta| meta.action)
    }

    pub fn from_binding_id(value: &str) -> Option<Self> {
        value
            .strip_prefix(CONTROL_BINDING_PREFIX)
            .and_then(Self::from_id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ControlHotkeys {
    #[serde(default)]
    pub play_pause: Option<String>,
    #[serde(default)]
    pub stop_all: Option<String>,
    #[serde(default)]
    pub previous_sound: Option<String>,
    #[serde(default)]
    pub next_sound: Option<String>,
    #[serde(default)]
    pub mute_headphones: Option<String>,
    #[serde(default)]
    pub mute_real_mic: Option<String>,
    #[serde(default)]
    pub cycle_play_mode: Option<String>,
}

impl ControlHotkeys {
    pub fn get_cloned(&self, action: ControlHotkeyAction) -> Option<String> {
        match action {
            ControlHotkeyAction::PlayPause => self.play_pause.clone(),
            ControlHotkeyAction::StopAll => self.stop_all.clone(),
            ControlHotkeyAction::PreviousSound => self.previous_sound.clone(),
            ControlHotkeyAction::NextSound => self.next_sound.clone(),
            ControlHotkeyAction::MuteHeadphones => self.mute_headphones.clone(),
            ControlHotkeyAction::MuteRealMic => self.mute_real_mic.clone(),
            ControlHotkeyAction::CyclePlayMode => self.cycle_play_mode.clone(),
        }
    }

    pub fn set_action(&mut self, action: ControlHotkeyAction, hotkey: Option<String>) {
        match action {
            ControlHotkeyAction::PlayPause => self.play_pause = hotkey,
            ControlHotkeyAction::StopAll => self.stop_all = hotkey,
            ControlHotkeyAction::PreviousSound => self.previous_sound = hotkey,
            ControlHotkeyAction::NextSound => self.next_sound = hotkey,
            ControlHotkeyAction::MuteHeadphones => self.mute_headphones = hotkey,
            ControlHotkeyAction::MuteRealMic => self.mute_real_mic = hotkey,
            ControlHotkeyAction::CyclePlayMode => self.cycle_play_mode = hotkey,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SoundTab {
    pub id: String,
    pub name: String,
    pub sound_ids: Vec<String>,
    pub order: u32,
}

impl SoundTab {
    pub fn new(name: String, order: u32) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            sound_ids: vec![],
            order,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sound {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub source_path: Option<String>,
    pub hotkey: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    pub volume: u8,
    pub enabled: bool,
    #[serde(default)]
    pub loudness_lufs: Option<f64>,
    #[serde(default)]
    pub loudness_analysis_state: LoudnessAnalysisState,
    #[serde(default)]
    pub loudness_confidence: Option<f32>,
    #[serde(default)]
    pub loudness_source_fingerprint: Option<String>,
}

impl Sound {
    pub fn new(name: String, path: String) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name,
            path: path.clone(),
            source_path: Some(path),
            hotkey: None,
            duration_ms: None,
            volume: 100,
            enabled: true,
            loudness_lufs: None,
            loudness_analysis_state: LoudnessAnalysisState::Pending,
            loudness_confidence: None,
            loudness_source_fingerprint: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub theme: Theme,
    pub local_volume: u8,
    #[serde(default)]
    pub local_mute: bool,
    pub mic_volume: u8,
    #[serde(default = "default_allow_multiple_playbacks")]
    pub allow_multiple_playbacks: bool,
    pub mic_passthrough: bool,
    pub mic_source: Option<String>,
    #[serde(default = "default_default_source_mode")]
    pub default_source_mode: DefaultSourceMode,
    #[serde(default)]
    pub mic_latency_profile: MicLatencyProfile,
    #[serde(default)]
    pub skip_delete_confirm: bool,
    #[serde(default)]
    pub auto_gain: bool,
    #[serde(default)]
    pub auto_gain_mode: AutoGainMode,
    #[serde(default = "default_auto_gain_target")]
    pub auto_gain_target_lufs: f64,
    #[serde(default)]
    pub auto_gain_apply_to: AutoGainApplyTo,
    #[serde(default = "default_auto_gain_lookahead_ms")]
    pub auto_gain_lookahead_ms: u32,
    #[serde(default = "default_auto_gain_attack_ms")]
    pub auto_gain_attack_ms: u32,
    #[serde(default = "default_auto_gain_release_ms")]
    pub auto_gain_release_ms: u32,
    #[serde(default)]
    pub control_hotkeys: ControlHotkeys,
    #[serde(default)]
    pub play_mode: PlayMode,
    #[serde(default)]
    pub list_style: ListStyle,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumeSettingsDomain {
    pub local_volume: u8,
    pub local_mute: bool,
    pub mic_volume: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicRoutingSettingsDomain {
    pub mic_passthrough: bool,
    pub mic_source: Option<String>,
    pub default_source_mode: DefaultSourceMode,
    pub mic_latency_profile: MicLatencyProfile,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutoGainSettingsDomain {
    pub enabled: bool,
    pub mode: AutoGainMode,
    pub target_lufs: f64,
    pub apply_to: AutoGainApplyTo,
    pub lookahead_ms: u32,
    pub attack_ms: u32,
    pub release_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiSettingsDomain {
    pub theme: Theme,
    pub list_style: ListStyle,
    pub skip_delete_confirm: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackSettingsDomain {
    pub play_mode: PlayMode,
    pub allow_multiple_playbacks: bool,
}

impl Settings {
    pub fn volume_domain(&self) -> VolumeSettingsDomain {
        VolumeSettingsDomain {
            local_volume: self.local_volume,
            local_mute: self.local_mute,
            mic_volume: self.mic_volume,
        }
    }

    pub fn mic_routing_domain(&self) -> MicRoutingSettingsDomain {
        MicRoutingSettingsDomain {
            mic_passthrough: self.mic_passthrough,
            mic_source: self.mic_source.clone(),
            default_source_mode: self.default_source_mode,
            mic_latency_profile: self.mic_latency_profile,
        }
    }

    pub fn auto_gain_domain(&self) -> AutoGainSettingsDomain {
        AutoGainSettingsDomain {
            enabled: self.auto_gain,
            mode: self.auto_gain_mode,
            target_lufs: self.auto_gain_target_lufs,
            apply_to: self.auto_gain_apply_to,
            lookahead_ms: self.auto_gain_lookahead_ms,
            attack_ms: self.auto_gain_attack_ms,
            release_ms: self.auto_gain_release_ms,
        }
    }

    pub fn ui_domain(&self) -> UiSettingsDomain {
        UiSettingsDomain {
            theme: self.theme,
            list_style: self.list_style,
            skip_delete_confirm: self.skip_delete_confirm,
        }
    }

    pub fn playback_domain(&self) -> PlaybackSettingsDomain {
        PlaybackSettingsDomain {
            play_mode: self.play_mode,
            allow_multiple_playbacks: self.allow_multiple_playbacks,
        }
    }

    pub fn normalize_for_persistence(&mut self) {
        if !self.auto_gain_target_lufs.is_finite() {
            self.auto_gain_target_lufs = default_auto_gain_target();
        }
        self.auto_gain_target_lufs = self.auto_gain_target_lufs.clamp(-24.0, 0.0);
        self.allow_multiple_playbacks = false;
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            local_volume: 80,
            local_mute: false,
            mic_volume: 100,
            allow_multiple_playbacks: false,
            mic_passthrough: true,
            mic_source: None,
            default_source_mode: DefaultSourceMode::Manual,
            mic_latency_profile: MicLatencyProfile::Balanced,
            skip_delete_confirm: false,
            auto_gain: false,
            auto_gain_mode: AutoGainMode::Static,
            auto_gain_target_lufs: default_auto_gain_target(),
            auto_gain_apply_to: AutoGainApplyTo::MicOnly,
            auto_gain_lookahead_ms: default_auto_gain_lookahead_ms(),
            auto_gain_attack_ms: default_auto_gain_attack_ms(),
            auto_gain_release_ms: default_auto_gain_release_ms(),
            control_hotkeys: ControlHotkeys::default(),
            play_mode: PlayMode::Default,
            list_style: ListStyle::Compact,
        }
    }
}

fn default_schema_version() -> u32 {
    crate::config::migration::CURRENT_SCHEMA_VERSION
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: default_schema_version(),
            sound_folders: vec![],
            sounds: vec![],
            tabs: vec![],
            settings: Settings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub sound_folders: Vec<String>,
    pub sounds: Vec<Sound>,
    #[serde(default)]
    pub tabs: Vec<SoundTab>,
    pub settings: Settings,
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn settings_default_disables_multiple_playbacks() {
        assert!(!Settings::default().allow_multiple_playbacks);
    }
}
