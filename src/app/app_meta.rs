pub const APP_ID: &str = "com.linuxsoundboard.app";
pub const APP_ICON_NAME: &str = "linux-soundboard";
pub const APP_TITLE: &str = "Linux Soundboard";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const APP_BINARY: &str = env!("CARGO_PKG_NAME");
pub const CONFIG_DIR_NAME: &str = "linux-soundboard";

pub const GENERAL_TAB_ID: &str = "general";

pub const BACKEND_ENV_VAR: &str = "GDK_BACKEND";
pub const FORCE_X11_ENV_VAR: &str = "LSB_FORCE_X11";
pub const RENDERER_ENV_VAR: &str = "GSK_RENDERER";
pub const FALLBACK_RENDERER: &str = "cairo";
pub const WAYLAND_BACKEND: &str = "wayland";
pub const X11_BACKEND: &str = "x11";
pub const LOCAL_PLAYBACK_NODE_NAME: &str = "linuxsoundboard.local_playback";
pub const MIC_CAPTURE_NODE_NAME: &str = "linuxsoundboard.mic_capture";
pub const VIRTUAL_SOURCE_NAME: &str = "linuxsoundboard.virtual_mic";
// Name of our feeder Playback stream that pushes audio into the null-sink.
// The null-sink itself uses VIRTUAL_SOURCE_NAME; the feeder is a separate
// PipeWire client stream that targets it.
pub const VIRTUAL_MIC_FEEDER_NODE_NAME: &str = "linuxsoundboard.virtual_mic_feeder";
pub const VIRTUAL_OUTPUT_DESCRIPTION: &str = "Linux_Soundboard_Output";
// Underscores instead of spaces: `pactl load-module sink_properties=…` strips
// whitespace from the value (pipewire-pulse compat limitation, no quoting
// escape works), so any space-containing description gets truncated to the
// first word. Users see this verbatim in mic dropdowns; the underscores are
// the standard Linux convention for that case.
pub const VIRTUAL_MIC_DESCRIPTION: &str = "Linux_Soundboard_Mic";
