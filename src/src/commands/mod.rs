//! Command handlers grouped by responsibility.

mod hotkeys;
mod library;
mod playback;
mod settings;
mod shared;
mod tabs;

pub use hotkeys::*;
pub use library::*;
pub use playback::*;
pub use settings::*;
pub use tabs::*;
