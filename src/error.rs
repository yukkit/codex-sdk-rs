use std::time::Duration;

/// SDK result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors surfaced by the SDK boundary.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Codex configuration could not be loaded or validated.
    #[error("failed to load Codex config: {0}")]
    Config(String),

    /// The Codex runtime failed to start or connect.
    #[error("failed to start Codex runtime: {0}")]
    RuntimeStart(String),

    /// The Codex runtime task failed while shutting down.
    #[error("Codex runtime task failed: {0}")]
    RuntimeTask(String),

    /// The runtime or event channel has closed.
    #[error("Codex runtime is closed")]
    RuntimeClosed,

    /// A Codex app-server protocol request failed.
    #[error("Codex protocol error: {0}")]
    Protocol(String),

    /// A `send()` operation exceeded its configured timeout.
    #[error("request timed out after {timeout:?}")]
    Timeout {
        /// Timeout duration that was exceeded.
        timeout: Duration,
    },

    /// Codex reported a non-retryable turn error.
    #[error("Codex turn failed: {message}")]
    TurnFailed {
        /// Thread id associated with the failed turn.
        thread_id: String,
        /// Turn id when Codex provided one.
        turn_id: Option<String>,
        /// Human-readable failure message from Codex.
        message: String,
    },

    /// Resolving or rejecting a server request failed.
    #[error("approval request failed: {0}")]
    Approval(String),

    /// Observability or OpenTelemetry setup failed.
    #[error("observability setup failed: {0}")]
    Observability(String),

    /// An SDK operation was cancelled before completion.
    #[error("operation was cancelled")]
    Cancelled,

    /// Filesystem or process I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization failed.
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

impl Error {
    pub(crate) fn config(error: impl std::fmt::Display) -> Self {
        Self::Config(error.to_string())
    }

    pub(crate) fn runtime_start(error: impl std::fmt::Display) -> Self {
        Self::RuntimeStart(error.to_string())
    }

    pub(crate) fn runtime_task(error: impl std::fmt::Display) -> Self {
        Self::RuntimeTask(error.to_string())
    }

    pub(crate) fn protocol(error: impl std::fmt::Display) -> Self {
        Self::Protocol(error.to_string())
    }

    pub(crate) fn approval(error: impl std::fmt::Display) -> Self {
        Self::Approval(error.to_string())
    }

    pub(crate) fn observability(error: impl std::fmt::Display) -> Self {
        Self::Observability(error.to_string())
    }
}
