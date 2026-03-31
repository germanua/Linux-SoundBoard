//! Initialization error types.

use std::fmt;

/// Errors that can occur during application initialization.
#[derive(Debug, Clone)]
pub enum InitError {
    /// Configuration loading or validation failed.
    Config(String),
    /// Audio system initialization failed.
    Audio(String),
    /// Hotkey system initialization failed.
    Hotkeys(String),
    /// UI window creation failed.
    Ui(String),
    /// Generic initialization failure.
    Other(String),
}

impl fmt::Display for InitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitError::Config(msg) => write!(f, "Config error: {}", msg),
            InitError::Audio(msg) => write!(f, "Audio error: {}", msg),
            InitError::Hotkeys(msg) => write!(f, "Hotkeys error: {}", msg),
            InitError::Ui(msg) => write!(f, "UI error: {}", msg),
            InitError::Other(msg) => write!(f, "Initialization error: {}", msg),
        }
    }
}

impl std::error::Error for InitError {}

/// Phases of initialization for progress tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitPhase {
    /// Configuration has been loaded
    Config,
    /// Audio player has been initialized
    Audio,
    /// Hotkey system has been initialized
    Hotkeys,
    /// UI window has been created
    Ui,
    /// Full initialization complete
    Complete,
}
