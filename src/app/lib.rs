// Modules accessible from the binary (main.rs) or external test crates.
pub mod audio;
pub mod bootstrap;
pub mod commands;
pub mod config;
pub mod legacy_migration;
pub mod library_store;

// Implementation-only modules — accessible within this crate but not
// part of the public API.
pub(crate) mod app_meta;
pub(crate) mod app_state;
pub(crate) mod diagnostics;
pub(crate) mod hotkeys;
pub(crate) mod timer_registry;
pub(crate) mod tray;
pub(crate) mod ui;
pub(crate) mod ui_event_bridge;

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod library_store_tests;
