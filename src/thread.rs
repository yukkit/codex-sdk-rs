use std::fmt;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::{
    AskForApproval, ClientRequest, RequestId, SandboxMode, ThreadArchiveResponse,
    ThreadCompactStartParams, ThreadCompactStartResponse, ThreadForkParams,
    ThreadReadParams, ThreadReadResponse, ThreadSetNameParams, ThreadSetNameResponse,
    ThreadSource, ThreadStartParams, ThreadStartResponse, TurnStartParams,
};
use codex_protocol::config_types::Personality;
use tokio_stream::Stream;

use crate::client::Codex;
use crate::error::{Error, Result};
use crate::runtime::EventReceiver;
use crate::turn::{IntoTurnInput, TurnBuilder};
use crate::types::{ThreadId, cwd_to_string};

/// Reusable Codex conversation thread.
///
/// A thread keeps Codex context across turns. Callers must not start overlapping
/// turns for the same thread ID; the SDK does not serialize them.
#[derive(Clone)]
pub struct Thread {
    /// State shared by control handles and all clones of this thread.
    inner: Arc<ThreadInner>,
}

impl fmt::Debug for Thread {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Thread")
            .field("id", &self.inner.id)
            .finish_non_exhaustive()
    }
}

struct ThreadInner {
    /// Shared Codex runtime that owns this thread.
    codex: Codex,
    /// Codex-assigned thread id.
    id: ThreadId,
    /// The thread's receiver is unique even when its control handle is cloned.
    event_rx: Mutex<Option<EventReceiver>>,
}

impl Thread {
    pub(crate) fn from_id(codex: Codex, id: ThreadId, event_rx: EventReceiver) -> Self {
        Self {
            inner: Arc::new(ThreadInner {
                codex,
                id,
                event_rx: Mutex::new(Some(event_rx)),
            }),
        }
    }

    /// Codex-assigned thread id.
    pub fn id(&self) -> &str {
        &self.inner.id
    }

    /// Take this thread's long-lived event stream.
    ///
    /// The stream contains every event associated with this thread across all
    /// of its turns. `TurnCompleted` is an ordinary event and does not end the
    /// stream. A successful SDK archive delivers `ThreadArchived` as the final
    /// event. Observed `ThreadDeleted` and `ThreadClosed` notifications are also
    /// terminal; runtime disconnection is delivered before the stream ends.
    ///
    /// Poll the stream continuously while the thread is active. A full queue
    /// of reliable transcript, completion, or server-request events applies
    /// backpressure to the shared runtime and can delay unrelated threads.
    ///
    /// A thread has exactly one event stream, shared across all clones of its
    /// handle. Calling this method more than once returns
    /// [`Error::ThreadEventStreamTaken`].
    pub fn event_stream(&self) -> Result<ThreadEventStream> {
        let mut event_rx = self
            .inner
            .event_rx
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let event_rx = event_rx
            .take()
            .ok_or_else(|| Error::ThreadEventStreamTaken {
                thread_id: self.id().to_string(),
            })?;

        Ok(ThreadEventStream {
            thread: self.clone(),
            event_rx,
        })
    }

    /// Start building a turn from user input.
    ///
    /// Do not start it while another turn for this thread ID is active.
    pub fn turn(&self, input: impl IntoTurnInput) -> TurnBuilder {
        TurnBuilder::new(self.clone(), input)
    }

    /// Start building a turn from native Codex `turn/start` params.
    ///
    /// Do not start it while another turn for this thread ID is active.
    pub fn turn_params(&self, params: TurnStartParams) -> TurnBuilder {
        TurnBuilder::from_params(self.clone(), params)
    }

    /// Read this thread from Codex state.
    pub async fn read(&self, include_turns: bool) -> Result<ThreadReadResponse> {
        self.inner
            .codex
            .request_typed(ClientRequest::ThreadRead {
                request_id: self.inner.codex.next_request_id(),
                params: ThreadReadParams {
                    thread_id: self.inner.id.clone(),
                    include_turns,
                },
            })
            .await
    }

    /// Update this thread's display name.
    pub async fn set_name(
        &self,
        name: impl Into<String>,
    ) -> Result<ThreadSetNameResponse> {
        self.inner
            .codex
            .request_typed(ClientRequest::ThreadSetName {
                request_id: self.inner.codex.next_request_id(),
                params: ThreadSetNameParams {
                    thread_id: self.inner.id.clone(),
                    name: name.into(),
                },
            })
            .await
    }

    /// Start compaction for this thread.
    pub async fn compact(&self) -> Result<ThreadCompactStartResponse> {
        self.inner
            .codex
            .request_typed(ClientRequest::ThreadCompactStart {
                request_id: self.inner.codex.next_request_id(),
                params: ThreadCompactStartParams {
                    thread_id: self.inner.id.clone(),
                },
            })
            .await
    }

    /// Fork this thread into a new reusable thread.
    pub async fn fork(&self) -> Result<Thread> {
        self.inner.codex.fork_thread(self.inner.id.clone()).await
    }

    /// Fork this thread using native `thread/fork` params.
    pub async fn fork_params(&self, mut params: ThreadForkParams) -> Result<Thread> {
        params.thread_id = self.inner.id.clone();
        self.inner.codex.fork_thread_params(params).await
    }

    /// Archive this thread in persistent Codex state.
    ///
    /// A successful response establishes the local stream boundary before this
    /// method returns. `ThreadArchived` is delivered as the final event on this
    /// handle's stream, and [`Codex::unarchive_thread`](crate::Codex::unarchive_thread)
    /// creates a new attachment with a new event stream.
    pub async fn archive(&self) -> Result<ThreadArchiveResponse> {
        self.inner.codex.archive_thread(self.inner.id.clone()).await
    }

    pub(crate) fn codex(&self) -> &Codex {
        &self.inner.codex
    }
}

/// Long-lived stream of events for one [`Thread`].
///
/// This stream spans turns and yields only requests and notifications owned by
/// this thread. Runtime disconnection is also delivered before the stream ends.
/// Dropping it stops local event consumption but does not interrupt an active
/// turn.
pub struct ThreadEventStream {
    /// Owning thread, retained for its id, control API, and runtime lifetime.
    thread: Thread,
    /// Dedicated receiver populated by the runtime's thread-aware event router.
    event_rx: EventReceiver,
}

impl ThreadEventStream {
    /// Thread whose events this stream yields.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }

    /// Thread id this stream belongs to.
    pub fn thread_id(&self) -> &str {
        self.thread.id()
    }

    /// Resolve a server request with a method-specific result payload.
    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: impl serde::Serialize,
    ) -> Result<()> {
        self.thread
            .codex()
            .resolve_server_request(request_id, result)
            .await
    }

    /// Resolve a server request with an empty JSON object.
    pub async fn approve_server_request(&self, request_id: RequestId) -> Result<()> {
        self.thread.codex().approve_server_request(request_id).await
    }

    /// Reject a server request with a human-readable message.
    pub async fn reject_server_request(
        &self,
        request_id: RequestId,
        message: impl Into<String>,
    ) -> Result<()> {
        self.thread
            .codex()
            .reject_server_request(request_id, message)
            .await
    }
}

impl Stream for ThreadEventStream {
    type Item = AppServerEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.event_rx.poll_recv(cx)
    }
}

/// Builder for creating a [`Thread`].
pub struct ThreadBuilder {
    /// Runtime used to create the thread.
    codex: Codex,
    /// Native Codex params sent with `thread/start`.
    params: ThreadStartParams,
}

impl ThreadBuilder {
    pub(crate) fn new(codex: Codex) -> Self {
        Self {
            params: codex.default_thread_params().clone(),
            codex,
        }
    }

    /// Replace the native Codex `thread/start` params for this builder.
    ///
    /// Convenience setters such as [`model_provider`](Self::model_provider)
    /// continue to edit this native params object after it is installed.
    pub fn params(mut self, params: ThreadStartParams) -> Self {
        self.params = params;
        self
    }

    /// Set the thread working directory.
    pub fn cwd(mut self, cwd: impl Into<std::path::PathBuf>) -> Self {
        let cwd = cwd.into();
        self.params.cwd = Some(cwd_to_string(&cwd));
        self
    }

    /// Set the thread default model.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.params.model = Some(model.into());
        self
    }

    /// Set the thread default model provider.
    pub fn model_provider(mut self, model_provider: impl Into<String>) -> Self {
        self.params.model_provider = Some(model_provider.into());
        self
    }

    /// Set the thread default service tier.
    pub fn service_tier(mut self, service_tier: impl Into<String>) -> Self {
        self.params.service_tier = Some(Some(service_tier.into()));
        self
    }

    /// Clear the thread default service tier.
    pub fn clear_service_tier(mut self) -> Self {
        self.params.service_tier = Some(None);
        self
    }

    /// Set the thread default model personality.
    pub fn personality(mut self, personality: Personality) -> Self {
        self.params.personality = Some(personality);
        self
    }

    /// Replace Codex's native base instructions for this thread.
    ///
    /// Passing an empty string disables the built-in base instructions for this
    /// thread.
    pub fn base_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.params.base_instructions = Some(instructions.into());
        self
    }

    /// Add developer instructions for this thread.
    pub fn developer_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.params.developer_instructions = Some(instructions.into());
        self
    }

    /// Set the thread default approval policy.
    pub fn approval_policy(mut self, approval_policy: AskForApproval) -> Self {
        self.params.approval_policy = Some(approval_policy);
        self
    }

    /// Set the thread default sandbox.
    pub fn sandbox(mut self, sandbox: SandboxMode) -> Self {
        self.params.sandbox = Some(sandbox);
        self
    }

    /// Set whether Codex should store this thread as ephemeral state.
    pub fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.params.ephemeral = Some(ephemeral);
        self
    }

    /// Create the thread in the Codex runtime.
    pub async fn start(self) -> Result<Thread> {
        let mut params = self.params;
        if params.thread_source.is_none() {
            params.thread_source = Some(ThreadSource::User);
        }

        let response: ThreadStartResponse = self
            .codex
            .request_typed(ClientRequest::ThreadStart {
                request_id: self.codex.next_request_id(),
                params,
            })
            .await?;

        let thread_id = response.thread.id;
        tracing::info!(%thread_id, "started Codex thread");

        self.codex.attach_thread(thread_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_event_stream_implements_stream() {
        fn assert_stream<T: Stream<Item = AppServerEvent>>() {}

        assert_stream::<ThreadEventStream>();
    }
}
