//! Global hotkey runtime.

mod backend_runtime;
mod error;
mod manager_runtime;
mod model;
mod swhkd_backend;
mod swhkd_config;
mod swhkd_install;
mod swhkd_process;
mod x11_backend;

pub(super) const HOTKEYS_POLL_INTERVAL_MS: u64 = 10;
pub(super) const SWHKD_SOCKET_POLL_INTERVAL_MS: u64 = 100;
pub(super) const SWHKD_RELOAD_PRE_SIGNAL_WAIT_MS: u64 = 100;
pub(super) const SWHKD_RELOAD_POST_SIGNAL_WAIT_MS: u64 = 200;
pub(super) const SWHKD_STARTUP_VERIFY_WAIT_MS: u64 = 500;
pub(super) const SWHKD_MONITOR_INTERVAL_SECS: u64 = 30;
pub(super) const SWHKD_PIPE_OPEN_RETRY_SECS: u64 = 1;
pub(super) const SWHKD_PIPE_REOPEN_DELAY_MS: u64 = 100;

pub use error::format_hotkey_error;
pub use manager_runtime::HotkeyManager;
pub use model::{
    canonicalize_hotkey_string, normalize_capture_key, parse_hotkey_spec, HotkeyCode,
    HotkeyModifier, HotkeySpec,
};
pub use swhkd_install::{
    install_swhkd_native, install_swhkd_native_detailed, manual_swhkd_install_commands,
    should_offer_swhkd_install, SwhkdInstallError, SwhkdInstallErrorKind, SwhkdInstallReport,
    SwhkdInstallState, INSTALLED_SWHKD_HELPER_PATH, SWHKD_UPSTREAM_INSTALL_URL,
};
