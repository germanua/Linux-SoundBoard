//! Configuration model types.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;
use uuid::Uuid;

use crate::config::defaults::{
    default_allow_multiple_playbacks, default_auto_gain_attack_ms, default_auto_gain_lookahead_ms,
    default_auto_gain_release_ms, default_auto_gain_target,
};

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

impl Serialize for Theme {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Theme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_str(&value).unwrap_or_default())
    }
}

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

impl Serialize for AutoGainMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AutoGainMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_str(&value).unwrap_or_default())
    }
}

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

impl Serialize for AutoGainApplyTo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AutoGainApplyTo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_str(&value).unwrap_or_default())
    }
}

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

impl Serialize for PlayMode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for PlayMode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_str(&value).unwrap_or_default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ListStyle {
    #[default]
    Compact,
    Card,
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

impl Serialize for ListStyle {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ListStyle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self::from_str(&value).unwrap_or_default())
    }
}

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

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::Dark,
            local_volume: 80,
            local_mute: false,
            mic_volume: 100,
            allow_multiple_playbacks: true,
            mic_passthrough: true,
            mic_source: None,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub sound_folders: Vec<String>,
    pub sounds: Vec<Sound>,
    #[serde(default)]
    pub tabs: Vec<SoundTab>,
    pub settings: Settings,
}
