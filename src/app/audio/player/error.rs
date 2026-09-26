#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("{0}")]
    Playback(String),

    #[error("{0}")]
    Routing(String),

    #[error("{0}")]
    Setup(String),
}
