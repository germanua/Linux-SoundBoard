//! Typed error for engine-internal audio operations.

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Playback or audio decode operation failed.
    #[error("{0}")]
    Playback(String),
    /// Source routing or microphone configuration failed.
    #[error("{0}")]
    Routing(String),
    /// PipeWire, PulseAudio, or audio backend setup failed.
    #[error("{0}")]
    Setup(String),
}
