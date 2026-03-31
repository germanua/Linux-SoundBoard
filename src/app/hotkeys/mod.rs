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

pub use error::format_hotkey_error;
pub use manager_runtime::HotkeyManager;
pub use model::{
    canonicalize_hotkey_string, normalize_capture_key, parse_hotkey_spec, HotkeyCode,
    HotkeyModifier, HotkeySpec,
};
