use std::sync::Arc;

use codex_app_server_protocol::{
    AskForApproval, ClientRequest, SandboxMode, ThreadArchiveResponse,
    ThreadCompactStartParams, ThreadCompactStartResponse, ThreadForkParams,
    ThreadReadParams, ThreadReadResponse, ThreadSetNameParams, ThreadSetNameResponse,
    ThreadSource, ThreadStartParams, ThreadStartResponse, TurnStartParams,
};
use codex_protocol::config_types::Personality;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::client::Codex;
use crate::error::{Error, Result};
use crate::turn::{IntoTurnInput, TurnBuilder, TurnResult};
use crate::types::{ThreadId, cwd_to_string};

/// Reusable Codex conversation thread.
///
/// A thread keeps Codex context across turns. This SDK serializes active turns
/// on the same thread so event routing, approvals, and context updates stay
/// ordered.
#[derive(Clone)]
pub struct Thread {
    /// Shared Codex runtime that owns this thread.
    codex: Codex,
    /// Codex-assigned thread id.
    id: ThreadId,
    /// Single permit used to serialize active turns on this thread.
    turn_permits: Arc<Semaphore>,
}

impl Thread {
    pub(crate) fn from_id(codex: Codex, id: ThreadId) -> Self {
        Self {
            codex,
            id,
            turn_permits: Arc::new(Semaphore::new(1)),
        }
    }

    /// Codex-assigned thread id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Start building a turn from user input.
    pub fn turn(&self, input: impl IntoTurnInput) -> TurnBuilder {
        TurnBuilder::new(self.clone(), input)
    }

    /// Start building a turn from native Codex `turn/start` params.
    pub fn turn_params(&self, params: TurnStartParams) -> TurnBuilder {
        TurnBuilder::from_params(self.clone(), params)
    }

    /// Start a turn with default options, wait for completion, and collect its
    /// final result.
    ///
    /// Use [`turn`](Self::turn) when you need to configure model, sandbox,
    /// reasoning, output schema, or other turn options before collecting with
    /// [`TurnBuilder::send`](crate::TurnBuilder::send).
    pub async fn run(&self, input: impl IntoTurnInput) -> Result<TurnResult> {
        self.turn(input).send().await
    }

    /// Read this thread from Codex state.
    pub async fn read(&self, include_turns: bool) -> Result<ThreadReadResponse> {
        self.codex
            .inner
            .runtime
            .request_typed(ClientRequest::ThreadRead {
                request_id: self.codex.inner.runtime.next_request_id(),
                params: ThreadReadParams {
                    thread_id: self.id.clone(),
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
        self.codex
            .inner
            .runtime
            .request_typed(ClientRequest::ThreadSetName {
                request_id: self.codex.inner.runtime.next_request_id(),
                params: ThreadSetNameParams {
                    thread_id: self.id.clone(),
                    name: name.into(),
                },
            })
            .await
    }

    /// Start compaction for this thread.
    pub async fn compact(&self) -> Result<ThreadCompactStartResponse> {
        self.codex
            .inner
            .runtime
            .request_typed(ClientRequest::ThreadCompactStart {
                request_id: self.codex.inner.runtime.next_request_id(),
                params: ThreadCompactStartParams {
                    thread_id: self.id.clone(),
                },
            })
            .await
    }

    /// Fork this thread into a new reusable thread.
    pub async fn fork(&self) -> Result<Thread> {
        self.codex.fork_thread(self.id.clone()).await
    }

    /// Fork this thread using native `thread/fork` params.
    pub async fn fork_params(&self, mut params: ThreadForkParams) -> Result<Thread> {
        params.thread_id = self.id.clone();
        self.codex.fork_thread_params(params).await
    }

    /// Archive this thread.
    pub async fn archive(&self) -> Result<ThreadArchiveResponse> {
        self.codex.archive_thread(self.id.clone()).await
    }

    pub(crate) fn codex(&self) -> &Codex {
        &self.codex
    }

    pub(crate) async fn acquire_turn_permit(&self) -> Result<OwnedSemaphorePermit> {
        self.turn_permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Cancelled)
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
            params: codex.inner.default_thread_params.clone(),
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
        self.params.cwd = Some(cwd_to_string(cwd));
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
            .inner
            .runtime
            .request_typed(ClientRequest::ThreadStart {
                request_id: self.codex.inner.runtime.next_request_id(),
                params,
            })
            .await?;

        let thread_id = response.thread.id;
        tracing::info!(%thread_id, "started Codex thread");

        Ok(Thread::from_id(self.codex, thread_id))
    }
}
