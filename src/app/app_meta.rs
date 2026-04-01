pub const APP_ID: &str = "com.linuxsoundboard.app";
pub const APP_ICON_NAME: &str = APP_ID;
pub const APP_TITLE: &str = "Linux Soundboard";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_BINARY: &str = env!("CARGO_PKG_NAME");
pub const APP_COMMENT: &str = "A Linux soundboard with PipeWire virtual mic support";

pub const CONFIG_DIR_NAME: &str = "linux-soundboard";
pub const DEFAULT_IMPORT_DIR_NAME: &str = "linux-soundboard";
pub const FALLBACK_IMPORT_DIR: &str = "./sounds";

pub const GENERAL_TAB_ID: &str = "general";

pub const BACKEND_ENV_VAR: &str = "GDK_BACKEND";
pub const FORCE_X11_ENV_VAR: &str = "LSB_FORCE_X11";
pub const RENDERER_ENV_VAR: &str = "GSK_RENDERER";
pub const FALLBACK_RENDERER: &str = "cairo";
pub const WAYLAND_BACKEND: &str = "wayland";
pub const X11_BACKEND: &str = "x11";
pub const STARTUP_VIRTUAL_MIC_DELAY_MS: u64 = 200;
pub const HOTKEY_POLL_INTERVAL_MS: u64 = 50;

pub const VIRTUAL_SINK_NAME: &str = "LinuxSoundboard_Sink";
pub const VIRTUAL_SOURCE_NAME: &str = "LinuxSoundboard_Mic";
pub const VIRTUAL_OUTPUT_DESCRIPTION: &str = "Linux_Soundboard_Output";
pub const VIRTUAL_MIC_DESCRIPTION: &str = "Linux_Soundboard_Mic";
pub const LOOPBACK_LATENCY_MS: u32 = 30;
