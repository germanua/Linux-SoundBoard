//! Typed error for engine-internal audio operations.

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// Decode or playback blew up.
    #[error("{0}")]
    Playback(String),
    /// Source routing or mic configuration blew up.
    #[error("{0}")]
    Routing(String),
    /// Bringing the PipeWire/Pulse backend up blew up.
    #[error("{0}")]
    Setup(String),
}
