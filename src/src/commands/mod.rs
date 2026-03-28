//! Command handlers grouped by responsibility.

mod hotkeys;
mod library;
mod playback;
mod settings;
mod shared;
mod tabs;

#[cfg(test)]
mod tests;

pub use hotkeys::*;
pub use library::*;
pub use playback::*;
pub use settings::*;
pub(crate) use shared::probe_duration_ms;
pub use tabs::*;
