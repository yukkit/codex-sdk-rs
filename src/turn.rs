use codex_app_server_protocol::{
    AskForApproval, ClientRequest, SandboxMode, SandboxPolicy, ThreadStartParams,
    TurnInterruptParams, TurnInterruptResponse, TurnStartParams, TurnStartResponse,
    TurnSteerParams, TurnSteerResponse, UserInput,
};
use codex_protocol::config_types::{Personality, ReasoningSummary};
use codex_protocol::openai_models::ReasoningEffort;

use crate::client::Codex;
use crate::error::Result;
use crate::runtime::RuntimeHandle;
use crate::thread::Thread;
use crate::types::{TurnId, cwd_to_string};

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

/// Builder for [`Codex::turn`](crate::Codex::turn).
///
/// This convenience path creates a temporary thread and starts one turn.
pub struct CodexTurnBuilder {
    /// Runtime used for the ephemeral thread.
    codex: Codex,
    /// Native Codex params used when creating the temporary thread.
    thread_params: ThreadStartParams,
    /// Native Codex params used when starting the turn.
    turn_params: TurnStartParams,
}

impl CodexTurnBuilder {
    pub(crate) fn new(codex: Codex, input: impl IntoTurnInput) -> Self {
        Self::from_params(codex, turn_params_with_input(input))
    }

    pub(crate) fn from_params(codex: Codex, turn_params: TurnStartParams) -> Self {
        let thread_params = codex.default_thread_params().clone();
        Self {
            codex,
            thread_params,
            turn_params,
        }
    }

    /// Set the working directory for both the temporary thread and turn.
    pub fn cwd(mut self, cwd: impl Into<std::path::PathBuf>) -> Self {
        let cwd = cwd.into();
        self.thread_params.cwd = Some(cwd_to_string(&cwd));
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

    /// Create the temporary thread, start the turn, and return its handle.
    pub async fn start(self) -> Result<TurnHandle> {
        let thread = self
            .codex
            .thread()
            .params(self.thread_params)
            .start()
            .await?;
        TurnBuilder::from_params(thread, self.turn_params)
            .start()
            .await
    }
}

/// Handle for a started Codex turn.
///
/// Use this when the application needs to identify, steer, or interrupt an
/// active turn. Events are consumed from the owning [`Thread`].
pub struct TurnHandle {
    /// Thread that owns this turn and its long-lived event stream.
    thread: Thread,
    /// Active turn id.
    turn_id: TurnId,
}

impl TurnHandle {
    /// Thread that owns this turn.
    pub fn thread(&self) -> &Thread {
        &self.thread
    }

    /// Thread id this handle controls.
    pub fn thread_id(&self) -> &str {
        self.thread.id()
    }

    /// Turn id this handle controls.
    pub fn turn_id(&self) -> &str {
        &self.turn_id
    }

    /// Send additional input to this active turn.
    pub async fn steer(&self, input: impl IntoTurnInput) -> Result<TurnSteerResponse> {
        steer_turn(
            self.thread.codex().runtime(),
            self.thread.id(),
            &self.turn_id,
            input,
        )
        .await
    }

    /// Request interruption of this active turn.
    pub async fn interrupt(&self) -> Result<TurnInterruptResponse> {
        interrupt_turn(
            self.thread.codex().runtime(),
            self.thread.id(),
            &self.turn_id,
        )
        .await
    }
}

/// Builder for starting a turn on an existing [`Thread`].
pub struct TurnBuilder {
    /// Existing thread that will receive the turn.
    thread: Thread,
    /// Native Codex params sent with `turn/start`.
    params: TurnStartParams,
}

impl TurnBuilder {
    pub(crate) fn new(thread: Thread, input: impl IntoTurnInput) -> Self {
        Self::from_params(thread, turn_params_with_input(input))
    }

    pub(crate) fn from_params(thread: Thread, params: TurnStartParams) -> Self {
        Self { thread, params }
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

    /// Start the turn and return its control handle.
    ///
    /// Consume events from [`Thread::event_stream`], which spans every turn in
    /// this thread.
    pub async fn start(self) -> Result<TurnHandle> {
        let thread_id = self.thread.id().to_string();
        let mut params = self.params;
        params.thread_id = thread_id.clone();
        let response: TurnStartResponse = self
            .thread
            .codex()
            .request_typed(ClientRequest::TurnStart {
                request_id: self.thread.codex().next_request_id(),
                params,
            })
            .await?;

        let turn_id = response.turn.id;
        tracing::info!(%thread_id, %turn_id, "started Codex turn");

        Ok(TurnHandle {
            thread: self.thread,
            turn_id,
        })
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

#[cfg(test)]
mod tests {
    use super::*;

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
