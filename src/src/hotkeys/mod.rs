//! Global hotkey runtime.

mod backend_runtime;
mod manager_runtime;
mod model;
mod portal_backend;
mod portal_trigger;
mod x11_backend;

pub use manager_runtime::HotkeyManager;
pub use model::{
    canonicalize_hotkey_string, normalize_capture_key, parse_hotkey_spec, HotkeyCode,
    HotkeyModifier, HotkeySpec,
};
pub use portal_trigger::canonical_hotkey_to_portal_trigger;
