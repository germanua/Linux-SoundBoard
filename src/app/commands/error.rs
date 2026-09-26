#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("Failed to save config: {0}")]
    ConfigSave(String),

    #[error("Sound not found")]
    SoundNotFound,

    #[error("Sound is disabled")]
    SoundDisabled,

    #[error("Source file unavailable: {0}")]
    SourceUnavailable(String),

    #[error("{0} not found")]
    NotFound(&'static str),

    #[error("{0}")]
    Invalid(String),

    #[error("{0}")]
    Io(String),

    #[error("Library operation failed: {0}")]
    Library(String),

    #[error("{0}")]
    Engine(String),

    #[error("{0}")]
    Hotkey(String),

    #[error("Every sound on that shortcut is already playing")]
    HotkeyNoOp,

    #[error("Shortcut was saved but could not be activated: {0}")]
    HotkeyProjection(String),

    #[error("{0}")]
    Analysis(String),
}

impl CommandError {
    pub(crate) fn config_save<E: std::fmt::Display>(err: E) -> Self {
        Self::ConfigSave(err.to_string())
    }
}
