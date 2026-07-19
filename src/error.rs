use std::time::Duration;

use codex_app_server_client::TypedRequestError;

use crate::thread::Thread;

type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// SDK result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors surfaced by the SDK boundary.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// Codex configuration could not be loaded or validated.
    #[error("failed to load Codex config: {source}")]
    Config {
        /// Underlying configuration error.
        #[source]
        source: BoxError,
    },

    /// The Codex runtime failed to start or connect.
    #[error("failed to start Codex runtime: {source}")]
    RuntimeStart {
        /// Underlying runtime startup error.
        #[source]
        source: BoxError,
    },

    /// In-process startup was attempted without Codex helper dispatch paths.
    #[error(
        "in-process Codex startup requires helper paths from run_main; use CodexMain::builder or provide arg0_paths explicitly"
    )]
    Arg0DispatchRequired,

    /// The Codex runtime task failed while shutting down.
    #[error("Codex runtime task failed: {0}")]
    RuntimeTask(#[source] tokio::task::JoinError),

    /// The runtime or event channel has closed.
    #[error("Codex runtime is closed")]
    RuntimeClosed,

    /// The single event stream owned by a thread has already been taken.
    #[error("event stream for thread {thread_id} has already been taken")]
    ThreadEventStreamTaken {
        /// Thread whose event stream was already taken.
        thread_id: String,
    },

    /// Another resume or unarchive request already owns this thread lifecycle.
    #[error("a lifecycle request for thread {thread_id} is already in progress")]
    ThreadLifecycleInProgress {
        /// Thread whose lifecycle route is already reserved.
        thread_id: String,
    },

    /// A temporary thread was created, but its first turn did not start.
    #[error(
        "failed to start a turn after temporary thread {thread_id} was created: {source}"
    )]
    TemporaryTurnStart {
        /// Created thread id, retained so the caller can inspect or clean it up.
        thread_id: String,
        /// Created thread and its still-attached event stream.
        thread: Thread,
        /// Turn-start failure.
        #[source]
        source: Box<Error>,
    },

    /// The single runtime-level event stream has already been taken.
    #[error("Codex runtime event stream has already been taken")]
    CodexEventStreamTaken,

    /// A thread lifecycle request or response used an invalid Codex thread id.
    #[error("invalid Codex thread id: {thread_id}")]
    InvalidThreadId {
        /// Invalid thread id supplied by the caller or returned by the app-server.
        thread_id: String,
    },

    /// The app-server client did not finish shutting down in time.
    #[error("Codex runtime shutdown timed out after {timeout:?}")]
    RuntimeShutdownTimeout {
        /// Maximum shutdown duration.
        timeout: Duration,
    },

    /// The app-server client failed while shutting down.
    #[error("failed to shut down Codex runtime: {0}")]
    RuntimeShutdown(#[source] std::io::Error),

    /// A previous shutdown attempt completed with an error.
    #[error("Codex runtime shutdown previously failed: {message}")]
    RuntimeShutdownFailed {
        /// Stable copy of the first shutdown failure.
        message: String,
    },

    /// A Codex app-server protocol request failed.
    #[error("Codex protocol error: {0}")]
    Protocol(#[source] TypedRequestError),

    /// Resolving or rejecting a server request failed.
    #[error("approval request failed: {0}")]
    Approval(#[source] std::io::Error),

    /// Resolving or rejecting a server request exceeded its bounded wait.
    #[error("server request response timed out after {timeout:?}")]
    ServerRequestResponseTimeout {
        /// Maximum response duration.
        timeout: Duration,
    },

    /// Observability or OpenTelemetry setup failed.
    #[error("observability setup failed: {source}")]
    Observability {
        /// Underlying observability setup error.
        #[source]
        source: BoxError,
    },

    /// Filesystem or process I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// JSON serialization or deserialization failed.
    #[error(transparent)]
    Serde(#[from] serde_json::Error),
}

impl Error {
    /// Return the temporary thread retained after a partial turn-start failure.
    pub fn temporary_thread(&self) -> Option<&Thread> {
        match self {
            Self::TemporaryTurnStart { thread, .. } => Some(thread),
            _ => None,
        }
    }

    /// Consume a partial turn-start failure into its thread and root error.
    pub fn into_temporary_turn_failure(self) -> Option<(Thread, Error)> {
        match self {
            Self::TemporaryTurnStart { thread, source, .. } => Some((thread, *source)),
            _ => None,
        }
    }

    pub(crate) fn config(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Config {
            source: Box::new(error),
        }
    }

    pub(crate) fn runtime_start(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::RuntimeStart {
            source: Box::new(error),
        }
    }

    pub(crate) fn runtime_task(error: tokio::task::JoinError) -> Self {
        Self::RuntimeTask(error)
    }

    pub(crate) fn runtime_shutdown(error: std::io::Error) -> Self {
        Self::RuntimeShutdown(error)
    }

    pub(crate) fn protocol(error: TypedRequestError) -> Self {
        Self::Protocol(error)
    }

    pub(crate) fn approval(error: std::io::Error) -> Self {
        Self::Approval(error)
    }

    pub(crate) fn observability(
        error: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Observability {
            source: Box::new(error),
        }
    }

    pub(crate) fn observability_message(message: impl Into<String>) -> Self {
        Self::Observability {
            source: Box::new(MessageError(message.into())),
        }
    }
}

#[derive(Debug)]
struct MessageError(String);

impl std::fmt::Display for MessageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MessageError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_error_remains_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<Error>();
    }
}
