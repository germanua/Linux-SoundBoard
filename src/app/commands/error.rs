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

    #[error("Source file unavailable: {0}")]
    SourceUnavailable(String),

    #[error("{0} not found")]
    NotFound(&'static str),

    #[error("{0}")]
    Invalid(String),

    /// A filesystem operation failed (e.g. creating the import directory).
    #[error("{0}")]
    Io(String),

    /// A bounded SQLite library operation failed.
    #[error("Library operation failed: {0}")]
    Library(String),

    /// Straight from the audio engine / IPC layer, still string-typed there.
    #[error("{0}")]
    Engine(String),

    /// An error surfaced by the hotkey subsystem.
    #[error("{0}")]
    Hotkey(String),

    #[error("Shortcut was saved but could not be activated: {0}")]
    HotkeyProjection(String),

    /// An error surfaced by loudness analysis.
    #[error("{0}")]
    Analysis(String),
}

impl CommandError {
    /// Wraps config-save errors.
    pub(crate) fn config_save<E: std::fmt::Display>(err: E) -> Self {
        Self::ConfigSave(err.to_string())
    }
}
