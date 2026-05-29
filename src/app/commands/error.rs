//! Typed error for the command layer.
//!
//! [`CommandError::SourceUnavailable`] replaces the old
//! `SOURCE_UNAVAILABLE_ERROR_PREFIX` + `str::starts_with` protocol the sound
//! list used to detect a missing backing file.
//!
//! Errors from the audio engine / IPC layer, loudness analysis, and the hotkey
//! backend are still string-typed at their source. Rather than let a bare
//! `String` leak through, we wrap them in named variants at this boundary
//! ([`Engine`](CommandError::Engine), [`Hotkey`](CommandError::Hotkey),
//! [`Analysis`](CommandError::Analysis)).
//!
//! `Display` deliberately reproduces the previous wording so UI toasts and log
//! lines are unchanged.

/// An error returned by a command-layer function.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    /// Persisting the config to disk failed.
    #[error("Failed to save config: {0}")]
    ConfigSave(String),

    /// No sound matches the requested id.
    #[error("Sound not found")]
    SoundNotFound,

    /// The sound exists but is disabled, so it cannot be played.
    #[error("Sound is disabled")]
    SoundDisabled,

    /// The backing audio file for a sound is missing or unreadable. Carries the
    /// path so the UI can mark the row invalid.
    #[error("Source file unavailable: {0}")]
    SourceUnavailable(String),

    /// A requested tab (or tab/sound pair) does not exist. The field names which
    /// (e.g. `"Tab"`, `"Source tab"`, `"Target tab"`, `"Tab or sound"`).
    #[error("{0} not found")]
    NotFound(&'static str),

    /// Caller-supplied input failed validation: an unknown mode string, an empty
    /// name, an out-of-range value, an unsupported file, a missing path, etc.
    /// The message is user-facing.
    #[error("{0}")]
    Invalid(String),

    /// A filesystem operation failed (e.g. creating the import directory).
    #[error("{0}")]
    Io(String),

    /// An error surfaced by the audio engine / IPC layer (still string-typed;
    /// that layer is migrated separately).
    #[error("{0}")]
    Engine(String),

    /// An error surfaced by the hotkey subsystem.
    #[error("{0}")]
    Hotkey(String),

    /// An error surfaced by loudness analysis.
    #[error("{0}")]
    Analysis(String),
}

impl CommandError {
    /// Wrap a config-persistence failure (`Config::save` returns a boxed error).
    pub(crate) fn config_save<E: std::fmt::Display>(err: E) -> Self {
        Self::ConfigSave(err.to_string())
    }
}
