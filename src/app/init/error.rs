use std::fmt;

#[derive(Debug, Clone)]
pub enum InitError {
    Config(String),
    Audio(String),
    Hotkeys(String),
    Ui(String),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitPhase {
    Config,
    Audio,
    Hotkeys,
    Ui,
    Complete,
}
