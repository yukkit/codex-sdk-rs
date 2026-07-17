use std::future::{Future, IntoFuture};
use std::pin::Pin;
use std::time::Duration;

use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::{
    AskForApproval, ClientRequest, RequestId, SandboxMode, SandboxPolicy,
    ServerNotification, ThreadStartParams, TurnInterruptParams, TurnInterruptResponse,
    TurnStartParams, TurnStartResponse, TurnSteerParams, TurnSteerResponse, UserInput,
};
use codex_protocol::config_types::{Personality, ReasoningSummary};
use codex_protocol::openai_models::ReasoningEffort;
use tokio::sync::{OwnedSemaphorePermit, broadcast};

use crate::client::Codex;
use crate::error::{Error, Result};
use crate::event::event_matches;
use crate::runtime::RuntimeHandle;
use crate::thread::Thread;
use crate::types::{ThreadId, TurnId, cwd_to_string};

/// Converts common SDK input shapes into native Codex turn input items.
pub trait IntoTurnInput {
    /// Convert this value into `TurnStartParams.input`.
    fn into_turn_input(self) -> Vec<UserInput>;
}

impl IntoTurnInput for String {
    fn into_turn_input(self) -> Vec<UserInput> {
        text_input(self)
    }
}

impl IntoTurnInput for &str {
    fn into_turn_input(self) -> Vec<UserInput> {
        text_input(self)
    }
}

impl IntoTurnInput for &String {
    fn into_turn_input(self) -> Vec<UserInput> {
        text_input(self.as_str())
    }
}

impl IntoTurnInput for UserInput {
    fn into_turn_input(self) -> Vec<UserInput> {
        vec![self]
    }
}

impl IntoTurnInput for Vec<UserInput> {
    fn into_turn_input(self) -> Vec<UserInput> {
        self
    }
}

impl<const N: usize> IntoTurnInput for [UserInput; N] {
    fn into_turn_input(self) -> Vec<UserInput> {
        Vec::from(self)
    }
}

/// Result collected from a completed turn.
///
/// This is returned by `send()` helpers, which consume the event stream until
/// `ServerNotification::TurnCompleted` and concatenate assistant message
/// deltas into [`final_response`](Self::final_response).
#[derive(Debug, Clone)]
pub struct TurnResult {
    /// Thread id associated with the completed turn.
    thread_id: ThreadId,
    /// Turn id associated with the completed turn.
    turn_id: TurnId,
    /// Concatenated assistant message deltas.
    final_response: String,
    /// Events observed before completion.
    events: Vec<AppServerEvent>,
}

impl TurnResult {
    /// Thread id associated with the completed turn.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Turn id associated with the completed turn.
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    /// Concatenated final assistant response text.
    pub fn final_response(&self) -> &str {
        &self.final_response
    }

    /// Parse the final assistant response as JSON.
    pub fn final_response_json(&self) -> Result<serde_json::Value> {
        Ok(serde_json::from_str(&self.final_response)?)
    }

    /// Deserialize the final assistant response as a typed JSON value.
    ///
    /// Pair this with native [`TurnStartParams`] or a builder
    /// `output_schema` method when you want Codex to produce a
    /// schema-constrained result.
    pub fn final_response_as<T>(&self) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        Ok(serde_json::from_str(&self.final_response)?)
    }

    /// Events observed while collecting the turn.
    pub fn events(&self) -> &[AppServerEvent] {
        &self.events
    }
}

/// Builder for [`Codex::turn`](crate::Codex::turn).
///
/// This convenience path creates a temporary thread, starts one turn, and
/// collects the final response.
pub struct CodexTurnBuilder {
    /// Runtime used for the ephemeral thread.
    pub(crate) codex: Codex,
    /// Native Codex params used when creating the temporary thread.
    pub(crate) thread_params: ThreadStartParams,
    /// Native Codex params used when starting the turn.
    pub(crate) turn_params: TurnStartParams,
    /// Maximum wall-clock time to wait when collecting this turn.
    pub(crate) timeout: Option<Duration>,
}

impl CodexTurnBuilder {
    pub(crate) fn new(codex: Codex, input: impl IntoTurnInput) -> Self {
        Self::from_params(codex, turn_params_with_input(input))
    }

    pub(crate) fn from_params(codex: Codex, turn_params: TurnStartParams) -> Self {
        let thread_params = codex.inner.default_thread_params.clone();
        Self {
            codex,
            thread_params,
            turn_params,
            timeout: None,
        }
    }

    /// Set the working directory for both the temporary thread and turn.
    pub fn cwd(mut self, cwd: impl Into<std::path::PathBuf>) -> Self {
        let cwd = cwd.into();
        self.thread_params.cwd = Some(cwd_to_string(cwd.clone()));
        self.turn_params.cwd = Some(cwd);
        self
    }

    /// Set the model for both the temporary thread and turn.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        let model = model.into();
        self.thread_params.model = Some(model.clone());
        self.turn_params.model = Some(model);
        self
    }

    /// Set the model provider for the temporary thread.
    pub fn model_provider(mut self, model_provider: impl Into<String>) -> Self {
        self.thread_params.model_provider = Some(model_provider.into());
        self
    }

    /// Set the service tier for both the temporary thread and turn.
    pub fn service_tier(mut self, service_tier: impl Into<String>) -> Self {
        let service_tier = service_tier.into();
        self.thread_params.service_tier = Some(Some(service_tier.clone()));
        self.turn_params.service_tier = Some(Some(service_tier));
        self
    }

    /// Clear the service tier for both the temporary thread and turn.
    pub fn clear_service_tier(mut self) -> Self {
        self.thread_params.service_tier = Some(None);
        self.turn_params.service_tier = Some(None);
        self
    }

    /// Set the model personality for both the temporary thread and turn.
    pub fn personality(mut self, personality: Personality) -> Self {
        self.thread_params.personality = Some(personality);
        self.turn_params.personality = Some(personality);
        self
    }

    /// Replace Codex's native base instructions for the temporary thread.
    ///
    /// Passing an empty string disables the built-in base instructions for this
    /// turn's temporary thread.
    pub fn base_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.thread_params.base_instructions = Some(instructions.into());
        self
    }

    /// Add developer instructions for the temporary thread.
    pub fn developer_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.thread_params.developer_instructions = Some(instructions.into());
        self
    }

    /// Set the sandbox mode for the temporary thread and a simple turn policy.
    ///
    /// The turn policy uses no extra writable roots and disables network access
    /// for read-only or workspace-write modes. Use
    /// [`sandbox_policy`](Self::sandbox_policy) for exact native policy control.
    pub fn sandbox(mut self, sandbox: SandboxMode) -> Self {
        self.thread_params.sandbox = Some(sandbox);
        self.turn_params.sandbox_policy = Some(sandbox_policy_from_mode(sandbox));
        self
    }

    /// Set the exact sandbox policy for the turn.
    ///
    /// This is the escape hatch for writable roots, network access, and other
    /// policy details that are not represented by [`SandboxMode`].
    pub fn sandbox_policy(mut self, sandbox_policy: SandboxPolicy) -> Self {
        self.turn_params.sandbox_policy = Some(sandbox_policy);
        self
    }

    /// Set the approval policy for both the temporary thread and turn.
    pub fn approval_policy(mut self, approval_policy: AskForApproval) -> Self {
        self.thread_params.approval_policy = Some(approval_policy);
        self.turn_params.approval_policy = Some(approval_policy);
        self
    }

    /// Set the reasoning effort for the turn.
    pub fn effort(mut self, effort: ReasoningEffort) -> Self {
        self.turn_params.effort = Some(effort);
        self
    }

    /// Set the reasoning summary behavior for the turn.
    pub fn reasoning_summary(mut self, summary: ReasoningSummary) -> Self {
        self.turn_params.summary = Some(summary);
        self
    }

    /// Set the reasoning summary behavior for the turn.
    pub fn summary(self, summary: ReasoningSummary) -> Self {
        self.reasoning_summary(summary)
    }

    /// Replace the user input for the turn.
    pub fn input(mut self, input: impl IntoTurnInput) -> Self {
        self.turn_params.input = input.into_turn_input();
        self
    }

    /// Replace the user input with a single text prompt.
    pub fn input_text(mut self, prompt: impl Into<String>) -> Self {
        self.turn_params.input = prompt.into().into_turn_input();
        self
    }

    /// Set a JSON Schema for the final assistant message.
    pub fn output_schema(mut self, schema: impl Into<serde_json::Value>) -> Self {
        self.turn_params.output_schema = Some(schema.into());
        self
    }

    /// Replace the native Codex `thread/start` params for the temporary thread.
    pub fn thread_params(mut self, params: ThreadStartParams) -> Self {
        self.thread_params = params;
        self
    }

    /// Replace the native Codex `turn/start` params for this turn, including input.
    ///
    /// If you want to keep the input passed to [`Codex::turn`](crate::Codex::turn),
    /// prefer the convenience setters or call [`input_text`](Self::input_text)
    /// after this method.
    pub fn params(mut self, params: TurnStartParams) -> Self {
        self.turn_params = params;
        self
    }

    /// Set the timeout used while collecting this turn.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Create the temporary thread, start the turn, and collect the result.
    pub async fn send(self) -> Result<TurnResult> {
        self.start_ephemeral_thread().await
    }
}

/// Handle for a started Codex turn.
///
/// Use this when the application needs to steer or interrupt an active turn
/// before consuming its event stream.
pub struct TurnHandle {
    /// Permit that keeps this thread from starting another active turn.
    _permit: OwnedSemaphorePermit,
    /// Runtime used for control requests and event subscription.
    runtime: std::sync::Arc<RuntimeHandle>,
    /// Thread id associated with the active turn.
    thread_id: ThreadId,
    /// Active turn id.
    turn_id: TurnId,
    /// Receiver subscribed before `turn/start`, so early events are not missed.
    rx: broadcast::Receiver<AppServerEvent>,
}

impl TurnHandle {
    /// Thread id this handle controls.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Turn id this handle controls.
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    /// Send additional input to this active turn.
    pub async fn steer(&self, input: impl IntoTurnInput) -> Result<TurnSteerResponse> {
        steer_turn(&self.runtime, &self.thread_id, &self.turn_id, input).await
    }

    /// Request interruption of this active turn.
    pub async fn interrupt(&self) -> Result<TurnInterruptResponse> {
        interrupt_turn(&self.runtime, &self.thread_id, &self.turn_id).await
    }

    /// Consume this handle and return a filtered event stream.
    pub fn stream(self) -> TurnStream {
        TurnStream {
            _permit: self._permit,
            runtime: self.runtime,
            thread_id: self.thread_id,
            turn_id: self.turn_id,
            rx: self.rx,
        }
    }

    /// Consume the event stream until the turn completes.
    pub async fn send(self) -> Result<TurnResult> {
        let mut stream = self.stream();
        stream.collect().await
    }

    /// Consume the event stream until the turn completes.
    pub async fn run(self) -> Result<TurnResult> {
        self.send().await
    }
}

/// Builder for starting a turn on an existing [`Thread`].
pub struct TurnBuilder {
    /// Existing thread that will receive the turn.
    thread: Thread,
    /// Native Codex params sent with `turn/start`.
    params: TurnStartParams,
    /// Maximum wall-clock time to wait when collecting this turn.
    timeout: Option<Duration>,
}

impl TurnBuilder {
    pub(crate) fn new(thread: Thread, input: impl IntoTurnInput) -> Self {
        Self::from_params(thread, turn_params_with_input(input))
    }

    pub(crate) fn from_params(thread: Thread, params: TurnStartParams) -> Self {
        Self {
            thread,
            params,
            timeout: None,
        }
    }

    /// Replace the native Codex `turn/start` params for this builder, including input.
    ///
    /// `thread_id` is always filled from the owning [`Thread`]. If you want to
    /// keep the input passed to [`Thread::turn`](crate::Thread::turn), prefer the
    /// convenience setters or call [`input_text`](Self::input_text) after this method.
    pub fn params(mut self, params: TurnStartParams) -> Self {
        self.params = params;
        self
    }

    /// Set the working directory for this turn.
    pub fn cwd(mut self, cwd: impl Into<std::path::PathBuf>) -> Self {
        self.params.cwd = Some(cwd.into());
        self
    }

    /// Set the model for this turn.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.params.model = Some(model.into());
        self
    }

    /// Set the service tier for this turn and subsequent turns.
    pub fn service_tier(mut self, service_tier: impl Into<String>) -> Self {
        self.params.service_tier = Some(Some(service_tier.into()));
        self
    }

    /// Clear the service tier for this turn and subsequent turns.
    pub fn clear_service_tier(mut self) -> Self {
        self.params.service_tier = Some(None);
        self
    }

    /// Set the model personality for this turn and subsequent turns.
    pub fn personality(mut self, personality: Personality) -> Self {
        self.params.personality = Some(personality);
        self
    }

    /// Set the approval policy for this turn.
    pub fn approval_policy(mut self, approval_policy: AskForApproval) -> Self {
        self.params.approval_policy = Some(approval_policy);
        self
    }

    /// Set a simple sandbox policy for this turn and subsequent turns.
    ///
    /// The policy uses no extra writable roots and disables network access for
    /// read-only or workspace-write modes. Use
    /// [`sandbox_policy`](Self::sandbox_policy) for exact native policy control.
    pub fn sandbox(mut self, sandbox: SandboxMode) -> Self {
        self.params.sandbox_policy = Some(sandbox_policy_from_mode(sandbox));
        self
    }

    /// Set the exact sandbox policy for this turn and subsequent turns.
    ///
    /// This is the escape hatch for writable roots, network access, and other
    /// policy details that are not represented by [`SandboxMode`].
    pub fn sandbox_policy(mut self, sandbox_policy: SandboxPolicy) -> Self {
        self.params.sandbox_policy = Some(sandbox_policy);
        self
    }

    /// Set the reasoning effort for this turn.
    pub fn effort(mut self, effort: ReasoningEffort) -> Self {
        self.params.effort = Some(effort);
        self
    }

    /// Set the reasoning summary behavior for this turn.
    pub fn reasoning_summary(mut self, summary: ReasoningSummary) -> Self {
        self.params.summary = Some(summary);
        self
    }

    /// Set the reasoning summary behavior for this turn.
    pub fn summary(self, summary: ReasoningSummary) -> Self {
        self.reasoning_summary(summary)
    }

    /// Replace the user input for this turn.
    pub fn input(mut self, input: impl IntoTurnInput) -> Self {
        self.params.input = input.into_turn_input();
        self
    }

    /// Replace the user input with a single text prompt.
    pub fn input_text(mut self, prompt: impl Into<String>) -> Self {
        self.params.input = prompt.into().into_turn_input();
        self
    }

    /// Set a JSON Schema for the final assistant message.
    pub fn output_schema(mut self, schema: impl Into<serde_json::Value>) -> Self {
        self.params.output_schema = Some(schema.into());
        self
    }

    /// Set the timeout used by [`send`](Self::send).
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub(crate) fn maybe_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.timeout = timeout;
        self
    }

    /// Start the turn and return a handle for streaming or control.
    pub async fn start(self) -> Result<TurnHandle> {
        let permit = self.thread.acquire_turn_permit().await?;
        let runtime = self.thread.codex().inner.runtime.clone();
        let rx = runtime.subscribe();
        let mut params = self.params;
        params.thread_id = self.thread.id().to_string();
        let response: TurnStartResponse = runtime
            .request_typed(ClientRequest::TurnStart {
                request_id: runtime.next_request_id(),
                params,
            })
            .await?;

        let thread_id = self.thread.id().to_string();
        let turn_id = response.turn.id;
        tracing::info!(%thread_id, %turn_id, "started Codex turn");

        Ok(TurnHandle {
            _permit: permit,
            runtime,
            thread_id,
            turn_id,
            rx,
        })
    }

    /// Start the turn and return a filtered event stream.
    ///
    /// The returned stream only yields events matching this thread and turn,
    /// plus runtime-level events such as lag and shutdown.
    pub async fn stream(self) -> Result<TurnStream> {
        Ok(self.start().await?.stream())
    }

    /// Start the turn and collect it until completion.
    ///
    /// Server requests are rejected automatically because this method has no UI
    /// hook for approvals. Use [`stream`](Self::stream) when the application
    /// needs to handle approvals or elicitation.
    pub async fn send(self) -> Result<TurnResult> {
        let timeout = self.timeout;
        let run = async move { self.start().await?.send().await };

        match timeout {
            Some(timeout) => tokio::time::timeout(timeout, run)
                .await
                .map_err(|_| Error::Timeout { timeout })?,
            None => run.await,
        }
    }
}

impl IntoFuture for TurnBuilder {
    type Output = Result<TurnResult>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.send().await })
    }
}

fn turn_params_with_input(input: impl IntoTurnInput) -> TurnStartParams {
    TurnStartParams {
        input: input.into_turn_input(),
        ..Default::default()
    }
}

fn text_input(prompt: impl Into<String>) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: prompt.into(),
        text_elements: Vec::new(),
    }]
}

fn sandbox_policy_from_mode(sandbox: SandboxMode) -> SandboxPolicy {
    match sandbox {
        SandboxMode::ReadOnly => SandboxPolicy::ReadOnly {
            network_access: false,
        },
        SandboxMode::WorkspaceWrite => SandboxPolicy::WorkspaceWrite {
            writable_roots: Vec::new(),
            network_access: false,
            exclude_tmpdir_env_var: false,
            exclude_slash_tmp: false,
        },
        SandboxMode::DangerFullAccess => SandboxPolicy::DangerFullAccess,
    }
}

async fn steer_turn(
    runtime: &RuntimeHandle,
    thread_id: &str,
    turn_id: &str,
    input: impl IntoTurnInput,
) -> Result<TurnSteerResponse> {
    runtime
        .request_typed(ClientRequest::TurnSteer {
            request_id: runtime.next_request_id(),
            params: TurnSteerParams {
                thread_id: thread_id.to_string(),
                input: input.into_turn_input(),
                expected_turn_id: turn_id.to_string(),
                ..Default::default()
            },
        })
        .await
}

async fn interrupt_turn(
    runtime: &RuntimeHandle,
    thread_id: &str,
    turn_id: &str,
) -> Result<TurnInterruptResponse> {
    runtime
        .request_typed(ClientRequest::TurnInterrupt {
            request_id: runtime.next_request_id(),
            params: TurnInterruptParams {
                thread_id: thread_id.to_string(),
                turn_id: turn_id.to_string(),
            },
        })
        .await
}

/// Stream of events for one active turn.
///
/// Dropping the stream releases the thread's active-turn permit. Keep polling
/// it until `ServerNotification::TurnCompleted` when you need the turn to
/// finish normally.
pub struct TurnStream {
    /// Permit that keeps this thread from starting another active turn.
    _permit: OwnedSemaphorePermit,
    /// Runtime used to resolve server requests and keep Codex alive.
    runtime: std::sync::Arc<RuntimeHandle>,
    /// Thread id used to filter broadcast runtime events.
    thread_id: ThreadId,
    /// Turn id used to filter broadcast runtime events.
    turn_id: TurnId,
    /// Subscription to the shared runtime event broadcast channel.
    rx: broadcast::Receiver<AppServerEvent>,
}

impl TurnStream {
    /// Thread id this stream is filtered to.
    pub fn thread_id(&self) -> &str {
        &self.thread_id
    }

    /// Turn id this stream is filtered to.
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    /// Send additional input to this active turn.
    pub async fn steer(&self, input: impl IntoTurnInput) -> Result<TurnSteerResponse> {
        steer_turn(&self.runtime, &self.thread_id, &self.turn_id, input).await
    }

    /// Request interruption of this active turn.
    pub async fn interrupt(&self) -> Result<TurnInterruptResponse> {
        interrupt_turn(&self.runtime, &self.thread_id, &self.turn_id).await
    }

    /// Resolve a Codex server request with a method-specific result payload.
    ///
    /// `request_id` should come from [`ServerRequest::id`](crate::ServerRequest::id).
    /// The expected `result` shape depends on the concrete
    /// [`ServerRequest`](crate::ServerRequest) variant. This method is a
    /// stream-scoped convenience for applications that answer requests while
    /// consuming turn events.
    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: impl serde::Serialize,
    ) -> Result<()> {
        let result = serde_json::to_value(result)?;
        self.runtime
            .resolve_server_request(request_id, result)
            .await
    }

    /// Resolve a Codex server request with an empty JSON object.
    ///
    /// This is a convenience for approval flows whose successful response
    /// payload is `{}`.
    pub async fn approve_server_request(&self, request_id: RequestId) -> Result<()> {
        self.resolve_server_request(request_id, serde_json::json!({}))
            .await
    }

    /// Reject a Codex server request with a human-readable message.
    pub async fn reject_server_request(
        &self,
        request_id: RequestId,
        message: impl Into<String>,
    ) -> Result<()> {
        self.runtime
            .reject_server_request(request_id, message)
            .await
    }

    /// Wait for the next event matching this turn.
    pub async fn next(&mut self) -> Option<AppServerEvent> {
        loop {
            let event = match self.rx.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    return Some(AppServerEvent::Lagged {
                        skipped: skipped.try_into().unwrap_or(usize::MAX),
                    });
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return None;
                }
            };

            if event_matches(&event, &self.thread_id, Some(&self.turn_id)) {
                return Some(event);
            }
        }
    }

    /// Collect this stream until the turn completes.
    ///
    /// This is the implementation behind [`TurnBuilder::send`]. It appends
    /// `ServerNotification::AgentMessageDelta` payloads into the final response
    /// and returns when `ServerNotification::TurnCompleted` arrives.
    pub async fn collect(&mut self) -> Result<TurnResult> {
        let mut final_response = String::new();
        let mut events = Vec::new();

        while let Some(event) = self.next().await {
            match &event {
                AppServerEvent::ServerNotification(
                    ServerNotification::AgentMessageDelta(delta),
                ) => {
                    final_response.push_str(&delta.delta);
                }
                AppServerEvent::ServerRequest(request) => {
                    let request_id = request.id().clone();
                    self.reject_server_request(
                        request_id,
                        "codex-sdk-rs TurnBuilder::send does not handle approvals",
                    )
                    .await?;
                }
                AppServerEvent::ServerNotification(ServerNotification::Error(error))
                    if !error.will_retry =>
                {
                    return Err(Error::TurnFailed {
                        thread_id: error.thread_id.clone(),
                        turn_id: Some(error.turn_id.clone()),
                        message: error.error.message.clone(),
                    });
                }
                AppServerEvent::ServerNotification(
                    ServerNotification::TurnCompleted(_),
                ) => {
                    events.push(event);
                    return Ok(TurnResult {
                        thread_id: self.thread_id.clone(),
                        turn_id: self.turn_id.clone(),
                        final_response,
                        events,
                    });
                }
                AppServerEvent::Disconnected { .. } => return Err(Error::RuntimeClosed),
                _ => {}
            }
            events.push(event);
        }

        Err(Error::RuntimeClosed)
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize, PartialEq)]
    struct StructuredAnswer {
        title: String,
        score: u8,
    }

    #[test]
    fn final_response_as_deserializes_structured_json() {
        let result = TurnResult {
            thread_id: "thread".to_string(),
            turn_id: "turn".to_string(),
            final_response: r#"{"title":"ok","score":7}"#.to_string(),
            events: Vec::new(),
        };

        let parsed: StructuredAnswer = result.final_response_as().unwrap();

        assert_eq!(
            parsed,
            StructuredAnswer {
                title: "ok".to_string(),
                score: 7,
            }
        );
    }

    #[test]
    fn turn_params_with_input_stores_string_as_native_text_input() {
        let params = turn_params_with_input("hello");

        assert_eq!(
            params.input,
            vec![UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            }]
        );
    }

    #[test]
    fn turn_params_with_input_accepts_native_user_input_vec() {
        let input = vec![
            UserInput::Text {
                text: "hello".to_string(),
                text_elements: Vec::new(),
            },
            UserInput::LocalImage {
                detail: None,
                path: "image.png".into(),
            },
        ];

        let params = turn_params_with_input(input.clone());

        assert_eq!(params.input, input);
    }

    #[test]
    fn sandbox_mode_maps_to_simple_turn_policy() {
        assert_eq!(
            sandbox_policy_from_mode(SandboxMode::ReadOnly),
            SandboxPolicy::ReadOnly {
                network_access: false
            }
        );
        assert_eq!(
            sandbox_policy_from_mode(SandboxMode::WorkspaceWrite),
            SandboxPolicy::WorkspaceWrite {
                writable_roots: Vec::new(),
                network_access: false,
                exclude_tmpdir_env_var: false,
                exclude_slash_tmp: false,
            }
        );
        assert_eq!(
            sandbox_policy_from_mode(SandboxMode::DangerFullAccess),
            SandboxPolicy::DangerFullAccess
        );
    }
}
