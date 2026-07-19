use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll, ready};

use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::{
    AskForApproval, ClientRequest, RequestId, SandboxMode, ServerNotification,
    ThreadArchiveResponse, ThreadCompactStartParams, ThreadCompactStartResponse,
    ThreadForkParams, ThreadReadParams, ThreadReadResponse, ThreadSetNameParams,
    ThreadSetNameResponse, ThreadSource, ThreadStartParams, ThreadStartResponse,
    TurnStartParams,
};
use codex_protocol::config_types::Personality;
use tokio::sync::broadcast;
use tokio_stream::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use crate::client::Codex;
use crate::error::{Error, Result};
use crate::event::event_matches;
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

struct ThreadInner {
    /// Shared Codex runtime that owns this thread.
    codex: Codex,
    /// Codex-assigned thread id.
    id: ThreadId,
    /// The thread's receiver is unique even when its control handle is cloned.
    event_rx: Mutex<Option<broadcast::Receiver<AppServerEvent>>>,
}

impl Thread {
    pub(crate) fn from_id(
        codex: Codex,
        id: ThreadId,
        event_rx: broadcast::Receiver<AppServerEvent>,
    ) -> Self {
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
    /// stream. The stream ends when the thread or runtime closes.
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
            event_rx: BroadcastStream::new(event_rx),
            terminated: false,
        })
    }

    /// Start building a turn from user input.
    pub fn turn(&self, input: impl IntoTurnInput) -> TurnBuilder {
        TurnBuilder::new(self.clone(), input)
    }

    /// Start building a turn from native Codex `turn/start` params.
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

    /// Archive this thread.
    pub async fn archive(&self) -> Result<ThreadArchiveResponse> {
        self.inner.codex.archive_thread(self.inner.id.clone()).await
    }

    pub(crate) fn codex(&self) -> &Codex {
        &self.inner.codex
    }
}

/// Long-lived stream of events for one [`Thread`].
///
/// This stream spans turns and yields thread-scoped requests and notifications,
/// plus runtime-level lag and disconnection events. Dropping it stops local
/// event consumption but does not interrupt an active turn.
pub struct ThreadEventStream {
    /// Owning thread, retained for its id, control API, and runtime lifetime.
    thread: Thread,
    /// Subscription created before the thread request, so early events survive.
    event_rx: BroadcastStream<AppServerEvent>,
    /// Set after the thread/runtime closes or the source channel ends.
    terminated: bool,
}

impl ThreadEventStream {
    /// Thread whose events this stream yields.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }

    /// Thread id this stream is filtered to.
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

    fn poll_next_event(&mut self, cx: &mut Context<'_>) -> Poll<Option<AppServerEvent>> {
        if self.terminated {
            return Poll::Ready(None);
        }

        loop {
            let event = match ready!(Pin::new(&mut self.event_rx).poll_next(cx)) {
                Some(Ok(event)) => event,
                Some(Err(BroadcastStreamRecvError::Lagged(skipped))) => {
                    return Poll::Ready(Some(AppServerEvent::Lagged {
                        skipped: skipped.try_into().unwrap_or(usize::MAX),
                    }));
                }
                None => {
                    self.terminated = true;
                    return Poll::Ready(None);
                }
            };

            if event_matches(&event, self.thread.id()) {
                if is_terminal_thread_event(&event) {
                    self.terminated = true;
                }
                return Poll::Ready(Some(event));
            }
        }
    }
}

impl Stream for ThreadEventStream {
    type Item = AppServerEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.get_mut().poll_next_event(cx)
    }
}

fn is_terminal_thread_event(event: &AppServerEvent) -> bool {
    matches!(
        event,
        AppServerEvent::ServerNotification(ServerNotification::ThreadClosed(_))
            | AppServerEvent::Disconnected { .. }
    )
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

        // Subscribe before `thread/start`; app-server may emit `thread/started`
        // before the request future resolves.
        let event_rx = self.codex.runtime().subscribe();
        let response: ThreadStartResponse = self
            .codex
            .request_typed(ClientRequest::ThreadStart {
                request_id: self.codex.next_request_id(),
                params,
            })
            .await?;

        let thread_id = response.thread.id;
        tracing::info!(%thread_id, "started Codex thread");

        Ok(Thread::from_id(self.codex, thread_id, event_rx))
    }
}

#[cfg(test)]
mod tests {
    use codex_app_server_protocol::{
        Turn, TurnCompletedNotification, TurnItemsView, TurnStatus,
    };

    use super::*;

    #[test]
    fn thread_event_stream_implements_stream() {
        fn assert_stream<T: Stream<Item = AppServerEvent>>() {}

        assert_stream::<ThreadEventStream>();
    }

    #[test]
    fn turn_completion_does_not_terminate_thread_stream() {
        let completed = AppServerEvent::ServerNotification(
            ServerNotification::TurnCompleted(TurnCompletedNotification {
                thread_id: "thread-1".to_string(),
                turn: Turn {
                    id: "turn-1".to_string(),
                    items: Vec::new(),
                    items_view: TurnItemsView::Full,
                    status: TurnStatus::Completed,
                    error: None,
                    started_at: Some(1),
                    completed_at: Some(2),
                    duration_ms: Some(1_000),
                },
            }),
        );
        let closed =
            AppServerEvent::ServerNotification(ServerNotification::ThreadClosed(
                codex_app_server_protocol::ThreadClosedNotification {
                    thread_id: "thread-1".to_string(),
                },
            ));
        let disconnected = AppServerEvent::Disconnected {
            message: "runtime closed".to_string(),
        };

        assert!(!is_terminal_thread_event(&completed));
        assert!(is_terminal_thread_event(&closed));
        assert!(is_terminal_thread_event(&disconnected));
        assert!(!is_terminal_thread_event(&AppServerEvent::Lagged {
            skipped: 1,
        }));
    }
}
