use std::collections::HashMap;
use std::fmt;
use std::future::{Future, IntoFuture};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use codex_app_server_client::{
    AppServerEvent, RemoteAppServerConnectArgs, RemoteAppServerEndpoint,
};
use codex_app_server_protocol::{
    AskForApproval, ClientRequest, GetAccountParams, GetAccountResponse, ModelListParams,
    ModelListResponse, RequestId, SandboxMode, ThreadArchiveParams,
    ThreadArchiveResponse, ThreadForkParams, ThreadForkResponse, ThreadListParams,
    ThreadListResponse, ThreadResumeParams, ThreadResumeResponse, ThreadSource,
    ThreadStartParams, ThreadUnarchiveParams, ThreadUnarchiveResponse, TurnStartParams,
};
use codex_arg0::Arg0DispatchPaths;
use codex_core::config::{Config, ConfigBuilder, ConfigOverrides, find_codex_home};
use codex_protocol::config_types::{Personality, ReasoningSummary};
use codex_protocol::openai_models::ReasoningEffort;
use codex_utils_absolute_path::AbsolutePathBuf;
use tokio_stream::Stream;

use crate::error::{Error, Result};
use crate::runtime::{
    DEFAULT_CHANNEL_CAPACITY, EventReceiver, RuntimeHandle, ThreadAttachmentKind,
};
use crate::thread::{Thread, ThreadBuilder};
use crate::thread_defaults::{
    EnvironmentAccess, INCLUDE_APPS_INSTRUCTIONS_CONFIG_KEY,
    INCLUDE_COLLABORATION_MODE_INSTRUCTIONS_CONFIG_KEY,
    INCLUDE_ENVIRONMENT_CONTEXT_CONFIG_KEY, INCLUDE_PERMISSIONS_INSTRUCTIONS_CONFIG_KEY,
    INCLUDE_SKILL_INSTRUCTIONS_CONFIG_KEY, ThreadDefaultsOverrides,
    thread_defaults_snapshot,
};
use crate::turn::{CodexTurnBuilder, IntoTurnInput};
use crate::types::{ClientInfo, ThreadId};
use crate::warmup::WarmupBuilder;

/// Handle to a shared Codex app-server runtime.
///
/// A single `Codex` value can create many [`Thread`]s. Clone it
/// freely; clones point at the same runtime and default thread options.
#[derive(Clone)]
pub struct Codex {
    /// Shared runtime state and default options.
    inner: Arc<CodexInner>,
}

struct CodexInner {
    /// Codex app-server runtime connection.
    runtime: Arc<RuntimeHandle>,
    /// Native Codex defaults copied into new thread builders.
    default_thread_params: ThreadStartParams,
}

#[derive(Debug, Clone, Default)]
struct RuntimeOptions {
    /// Helper executable paths discovered by `codex_arg0`.
    arg0_paths: Option<Arg0DispatchPaths>,
    /// Client metadata reported to Codex.
    client_info: ClientInfo,
    /// Independent capacities for the upstream connection and SDK streams.
    channels: ChannelCapacities,
}

#[derive(Debug, Clone, Copy)]
struct ChannelCapacities {
    app_server: usize,
    event_stream: usize,
}

impl Default for ChannelCapacities {
    fn default() -> Self {
        Self {
            app_server: DEFAULT_CHANNEL_CAPACITY,
            event_stream: DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

impl ChannelCapacities {
    fn set_all(&mut self, capacity: usize) {
        let capacity = capacity.max(1);
        self.app_server = capacity;
        self.event_stream = capacity;
    }
}

impl RuntimeOptions {
    fn take_arg0_paths(&mut self) -> Result<Arg0DispatchPaths> {
        self.arg0_paths.take().ok_or(Error::Arg0DispatchRequired)
    }
}

impl Codex {
    fn from_runtime(
        runtime: Arc<RuntimeHandle>,
        default_thread_params: ThreadStartParams,
    ) -> Self {
        Self {
            inner: Arc::new(CodexInner {
                runtime,
                default_thread_params,
            }),
        }
    }

    /// Start building a Codex runtime.
    pub fn builder() -> CodexBuilder {
        CodexBuilder::default()
    }

    /// Start building a Codex runtime from a fully resolved native config.
    pub fn builder_with_config(config: Config) -> CodexWithConfigBuilder {
        CodexWithConfigBuilder::new(config)
    }

    /// Start building a Codex client connected to a remote WebSocket app-server.
    pub fn remote_websocket(websocket_url: impl Into<String>) -> CodexRemoteBuilder {
        CodexRemoteBuilder::websocket(websocket_url.into())
    }

    /// Start building a Codex client connected to a remote Unix-socket app-server.
    pub fn remote_unix_socket(socket_path: impl Into<PathBuf>) -> CodexRemoteBuilder {
        CodexRemoteBuilder::unix_socket(socket_path.into())
    }

    /// Start building a reusable Codex thread.
    pub fn thread(&self) -> ThreadBuilder {
        ThreadBuilder::new(self.clone())
    }

    /// Take this runtime's stream of events that do not belong to one thread.
    ///
    /// Thread-scoped events are delivered through [`Thread::event_stream`].
    /// This stream yields runtime notifications, threadless server requests,
    /// upstream lag reports, and the final disconnection event. A runtime has
    /// exactly one such stream across all [`Codex`] clones.
    pub fn event_stream(&self) -> Result<CodexEventStream> {
        Ok(CodexEventStream {
            codex: self.clone(),
            event_rx: self.runtime().take_runtime_events()?,
        })
    }

    /// List available Codex models using server defaults.
    pub async fn models(&self) -> Result<ModelListResponse> {
        self.models_params(ModelListParams {
            include_hidden: Some(false),
            ..Default::default()
        })
        .await
    }

    /// List available Codex models using native `model/list` params.
    pub async fn models_params(
        &self,
        params: ModelListParams,
    ) -> Result<ModelListResponse> {
        self.request_typed(ClientRequest::ModelList {
            request_id: self.next_request_id(),
            params,
        })
        .await
    }

    /// Read the current Codex account state without forcing a token refresh.
    pub async fn account(&self) -> Result<GetAccountResponse> {
        self.account_params(GetAccountParams {
            refresh_token: false,
        })
        .await
    }

    /// Read the current Codex account state using native `account/read` params.
    pub async fn account_params(
        &self,
        params: GetAccountParams,
    ) -> Result<GetAccountResponse> {
        self.request_typed(ClientRequest::GetAccount {
            request_id: self.next_request_id(),
            params,
        })
        .await
    }

    /// Resume a saved Codex thread by id.
    pub async fn resume_thread(&self, thread_id: impl Into<String>) -> Result<Thread> {
        self.resume_thread_params(ThreadResumeParams {
            thread_id: thread_id.into(),
            ..Default::default()
        })
        .await
    }

    /// Resume a saved Codex thread using native `thread/resume` params.
    pub async fn resume_thread_params(
        &self,
        params: ThreadResumeParams,
    ) -> Result<Thread> {
        let requested_thread_id = params.thread_id.clone();
        let reservation = self
            .runtime()
            .prepare_thread_events(&requested_thread_id, ThreadAttachmentKind::Resume)?;
        let response: ThreadResumeResponse = self
            .request_typed(ClientRequest::ThreadResume {
                request_id: self.next_request_id(),
                params,
            })
            .await?;
        let thread_id = response.thread.id;
        if thread_id != requested_thread_id {
            drop(reservation);
            tracing::warn!(
                requested_thread_id,
                %thread_id,
                "thread/resume returned a different thread id"
            );
            return self.attach_thread(thread_id);
        }
        tracing::info!(%thread_id, "resumed Codex thread");
        let event_rx = reservation.claim()?;
        Ok(Thread::from_id(self.clone(), thread_id, event_rx))
    }

    /// Fork a saved Codex thread by id.
    pub async fn fork_thread(&self, thread_id: impl Into<String>) -> Result<Thread> {
        self.fork_thread_params(ThreadForkParams {
            thread_id: thread_id.into(),
            ..Default::default()
        })
        .await
    }

    /// Fork a saved Codex thread using native `thread/fork` params.
    pub async fn fork_thread_params(
        &self,
        mut params: ThreadForkParams,
    ) -> Result<Thread> {
        if params.thread_source.is_none() {
            params.thread_source = Some(ThreadSource::User);
        }

        let response: ThreadForkResponse = self
            .request_typed(ClientRequest::ThreadFork {
                request_id: self.next_request_id(),
                params,
            })
            .await?;
        let thread_id = response.thread.id;
        tracing::info!(%thread_id, "forked Codex thread");
        self.attach_thread(thread_id)
    }

    /// List saved Codex threads using server defaults.
    pub async fn list_threads(&self) -> Result<ThreadListResponse> {
        self.list_threads_params(default_thread_list_params()).await
    }

    /// List saved Codex threads using native `thread/list` params.
    pub async fn list_threads_params(
        &self,
        params: ThreadListParams,
    ) -> Result<ThreadListResponse> {
        self.request_typed(ClientRequest::ThreadList {
            request_id: self.next_request_id(),
            params,
        })
        .await
    }

    /// Archive a saved Codex thread.
    pub async fn archive_thread(
        &self,
        thread_id: impl Into<String>,
    ) -> Result<ThreadArchiveResponse> {
        let thread_id = thread_id.into();
        let response = self
            .request_typed(ClientRequest::ThreadArchive {
                request_id: self.next_request_id(),
                params: ThreadArchiveParams {
                    thread_id: thread_id.clone(),
                },
            })
            .await?;
        self.runtime().complete_thread_archive(&thread_id)?;
        Ok(response)
    }

    /// Restore an archived Codex thread and return a reusable handle.
    pub async fn unarchive_thread(&self, thread_id: impl Into<String>) -> Result<Thread> {
        let requested_thread_id = thread_id.into();
        let reservation = self.runtime().prepare_thread_events(
            &requested_thread_id,
            ThreadAttachmentKind::Unarchive,
        )?;
        let response: ThreadUnarchiveResponse = self
            .request_typed(ClientRequest::ThreadUnarchive {
                request_id: self.next_request_id(),
                params: ThreadUnarchiveParams {
                    thread_id: requested_thread_id.clone(),
                },
            })
            .await?;
        let thread_id = response.thread.id;
        if thread_id != requested_thread_id {
            drop(reservation);
            tracing::warn!(
                requested_thread_id,
                %thread_id,
                "thread/unarchive returned a different thread id"
            );
            return self.attach_thread(thread_id);
        }
        tracing::info!(%thread_id, "unarchived Codex thread");
        let event_rx = reservation.claim()?;
        Ok(Thread::from_id(self.clone(), thread_id, event_rx))
    }

    /// Start building a turn in a temporary thread.
    pub fn turn(&self, input: impl IntoTurnInput) -> CodexTurnBuilder {
        CodexTurnBuilder::new(self.clone(), input)
    }

    /// Start building a turn from native Codex `turn/start` params in a temporary thread.
    pub fn turn_params(&self, params: TurnStartParams) -> CodexTurnBuilder {
        CodexTurnBuilder::from_params(self.clone(), params)
    }

    /// Warm app-server caches and inventories without creating a Codex thread.
    pub fn warmup(&self) -> WarmupBuilder {
        WarmupBuilder::new(self.clone())
    }

    /// Allocate a request id for a native [`ClientRequest`].
    ///
    /// Use this instead of choosing ids independently so low-level requests do
    /// not collide with requests issued by the SDK's higher-level methods.
    pub fn next_request_id(&self) -> RequestId {
        self.inner.runtime.next_request_id()
    }

    /// Send a native Codex app-server request and deserialize its response.
    ///
    /// Construct the request with an id from [`next_request_id`](Self::next_request_id).
    /// The expected response type depends on the `ClientRequest` variant.
    pub async fn request_typed<T>(&self, request: ClientRequest) -> Result<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.inner.runtime.request_typed(request).await
    }

    /// Resolve a Codex server request with a method-specific result payload.
    ///
    /// `request_id` should come from [`ServerRequest::id`](crate::ServerRequest::id).
    /// The expected `result` shape depends on the concrete
    /// [`ServerRequest`](crate::ServerRequest) variant. Passing response types
    /// from `codex_app_server_protocol` is preferred, and `serde_json::json!`
    /// values are also accepted for simple cases.
    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: impl serde::Serialize,
    ) -> Result<()> {
        let result = serde_json::to_value(result)?;
        self.inner
            .runtime
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
        self.inner
            .runtime
            .reject_server_request(request_id, message)
            .await
    }

    /// Shut down the Codex app-server runtime connection and await cleanup.
    ///
    /// Dropping the final SDK handle also signals shutdown, but this method is
    /// preferred when the caller needs cleanup failures to be reported.
    pub async fn shutdown(&self) -> Result<()> {
        self.inner.runtime.shutdown().await
    }

    pub(crate) fn runtime(&self) -> &Arc<RuntimeHandle> {
        &self.inner.runtime
    }

    pub(crate) fn attach_thread(&self, thread_id: ThreadId) -> Result<Thread> {
        let event_rx = self.runtime().take_thread_events(&thread_id)?;
        Ok(Thread::from_id(self.clone(), thread_id, event_rx))
    }

    pub(crate) fn default_thread_params(&self) -> &ThreadStartParams {
        &self.inner.default_thread_params
    }
}

/// Long-lived stream of events owned by the shared Codex runtime.
///
/// Thread events are intentionally excluded; consume those from the matching
/// [`Thread`]. Dropping this stream does not shut down Codex.
pub struct CodexEventStream {
    codex: Codex,
    event_rx: EventReceiver,
}

impl CodexEventStream {
    /// Runtime whose global events this stream yields.
    pub fn codex(&self) -> &Codex {
        &self.codex
    }

    /// Resolve a server request with a method-specific result payload.
    pub async fn resolve_server_request(
        &self,
        request_id: RequestId,
        result: impl serde::Serialize,
    ) -> Result<()> {
        self.codex.resolve_server_request(request_id, result).await
    }

    /// Resolve a server request with an empty JSON object.
    pub async fn approve_server_request(&self, request_id: RequestId) -> Result<()> {
        self.codex.approve_server_request(request_id).await
    }

    /// Reject a server request with a human-readable message.
    pub async fn reject_server_request(
        &self,
        request_id: RequestId,
        message: impl Into<String>,
    ) -> Result<()> {
        self.codex.reject_server_request(request_id, message).await
    }
}

impl Stream for CodexEventStream {
    type Item = AppServerEvent;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.event_rx.poll_recv(cx)
    }
}

/// Builder for the shared Codex runtime and its default thread options.
///
/// Values configured here become the defaults for new threads. Per-thread and
/// per-turn builders can override most of them later.
#[derive(Debug, Clone)]
pub struct CodexBuilder {
    /// Runtime startup options that are not part of Codex config.
    runtime: RuntimeOptions,
    /// Codex home directory for config, auth, logs, and state.
    codex_home: Option<PathBuf>,
    /// Default working directory for the runtime and new threads.
    cwd: Option<PathBuf>,
    /// Default model for new threads.
    model: Option<String>,
    /// Default model provider for new threads.
    model_provider: Option<String>,
    /// Default service tier for new threads.
    service_tier: Option<Option<String>>,
    /// Default reasoning effort used by the model.
    reasoning_effort: Option<ReasoningEffort>,
    /// Default reasoning summary behavior used by the model.
    reasoning_summary: Option<ReasoningSummary>,
    /// Default model personality for new threads.
    personality: Option<Personality>,
    /// Native base instructions override for all threads created by this runtime.
    base_instructions: Option<String>,
    /// Native developer instructions added for all threads created by this runtime.
    developer_instructions: Option<String>,
    /// Ordered native changes applied to every new thread's defaults.
    thread_defaults: ThreadDefaultsOverrides,
    /// Explicit approval-policy override for new threads and turns.
    approval_policy: Option<AskForApproval>,
    /// Explicit sandbox override for new threads.
    sandbox: Option<SandboxMode>,
    /// Whether new threads are ephemeral by default.
    ephemeral: bool,
}

impl Default for CodexBuilder {
    fn default() -> Self {
        Self {
            runtime: RuntimeOptions::default(),
            codex_home: None,
            cwd: None,
            model: None,
            model_provider: None,
            service_tier: None,
            reasoning_effort: None,
            reasoning_summary: None,
            personality: None,
            base_instructions: None,
            developer_instructions: None,
            thread_defaults: ThreadDefaultsOverrides::default(),
            approval_policy: None,
            sandbox: None,
            ephemeral: true,
        }
    }
}

impl CodexBuilder {
    /// Provide Codex helper executable paths captured by [`run_main`](crate::run_main).
    pub fn arg0_paths(mut self, arg0_paths: Arg0DispatchPaths) -> Self {
        self.runtime.arg0_paths = Some(arg0_paths);
        self
    }

    /// Set the Codex home directory used for config, auth, logs, and state.
    pub fn codex_home(mut self, codex_home: impl Into<PathBuf>) -> Self {
        self.codex_home = Some(codex_home.into());
        self
    }

    /// Set the default working directory for new threads.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Set the default model for new threads.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Set the default model provider for new threads.
    pub fn model_provider(mut self, model_provider: impl Into<String>) -> Self {
        self.model_provider = Some(model_provider.into());
        self
    }

    /// Set the default service tier for new threads.
    pub fn service_tier(mut self, service_tier: impl Into<String>) -> Self {
        self.service_tier = Some(Some(service_tier.into()));
        self
    }

    /// Clear any configured default service tier for new threads.
    pub fn clear_service_tier(mut self) -> Self {
        self.service_tier = Some(None);
        self
    }

    /// Set the default reasoning effort for model requests.
    pub fn reasoning_effort(mut self, effort: ReasoningEffort) -> Self {
        self.reasoning_effort = Some(effort);
        self
    }

    /// Set the default reasoning summary behavior for model requests.
    pub fn reasoning_summary(mut self, summary: ReasoningSummary) -> Self {
        self.reasoning_summary = Some(summary);
        self
    }

    /// Set the default model personality for new threads.
    pub fn personality(mut self, personality: Personality) -> Self {
        self.personality = Some(personality);
        self
    }

    /// Replace Codex's native base instructions for new threads.
    ///
    /// This controls the large Responses API `instructions` payload. Passing an
    /// empty string disables the built-in base instructions.
    pub fn base_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.base_instructions = Some(instructions.into());
        self
    }

    /// Add developer instructions to new threads.
    ///
    /// These are appended as model-visible developer guidance alongside dynamic
    /// SDK context such as permissions, apps, and skills instructions.
    pub fn developer_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.developer_instructions = Some(instructions.into());
        self
    }

    /// Disable optional model-visible context blocks for small-token sessions.
    ///
    /// This keeps the base instructions and tool schemas, but disables permissions,
    /// apps, collaboration mode, environment context, and skill instruction blocks.
    pub fn minimal_prompt_context(mut self) -> Self {
        self.thread_defaults.minimize_prompt_context();
        self
    }

    /// Disable the configurable tool families used by a normal Codex agent.
    ///
    /// This applies native per-thread overrides for:
    ///
    /// - apps, plugins, install recommendations, and orchestrator-owned skills/MCP;
    /// - shell/code-mode, multi-agent/fanout, image generation, memories, and goals;
    /// - deferred-environment, permission-request, token-budget, time, user-input,
    ///   and web-search tools.
    ///
    /// This is a best-effort minimal tool profile, not an all-tools-off policy.
    /// Core tools such as `update_plan` can remain model-visible. User-configured
    /// MCP servers, dynamic tools, environment-dependent tools, and future tool
    /// families are also unaffected.
    /// Call [`Self::default_thread_config_overrides`] afterwards to re-enable or
    /// further customize individual native settings.
    pub fn minimal_tools(mut self) -> Self {
        self.thread_defaults.minimize_tools();
        self
    }

    /// Configure new threads for a small-token, chat-oriented session.
    ///
    /// This disables optional prompt context, project-document discovery, known
    /// configurable tool families, and environment access. It remains a
    /// best-effort profile: core, user MCP, dynamic, and future tools can still
    /// be model-visible.
    pub fn pure_chat_profile(mut self) -> Self {
        self.thread_defaults.configure_pure_chat();
        self
    }

    /// Select the default execution-environment behavior for new threads.
    pub fn default_environment_access(mut self, access: EnvironmentAccess) -> Self {
        self.thread_defaults.set_environment_access(access);
        self
    }

    /// Choose whether to include permission/sandbox guidance in model context.
    pub fn include_permissions_instructions(mut self, enabled: bool) -> Self {
        self.thread_defaults
            .set_prompt_context(INCLUDE_PERMISSIONS_INSTRUCTIONS_CONFIG_KEY, enabled);
        self
    }

    /// Choose whether to include app/connector guidance in model context.
    pub fn include_apps_instructions(mut self, enabled: bool) -> Self {
        self.thread_defaults
            .set_prompt_context(INCLUDE_APPS_INSTRUCTIONS_CONFIG_KEY, enabled);
        self
    }

    /// Choose whether to include collaboration-mode guidance in model context.
    pub fn include_collaboration_mode_instructions(mut self, enabled: bool) -> Self {
        self.thread_defaults.set_prompt_context(
            INCLUDE_COLLABORATION_MODE_INSTRUCTIONS_CONFIG_KEY,
            enabled,
        );
        self
    }

    /// Choose whether to include environment metadata in model context.
    pub fn include_environment_context(mut self, enabled: bool) -> Self {
        self.thread_defaults
            .set_prompt_context(INCLUDE_ENVIRONMENT_CONTEXT_CONFIG_KEY, enabled);
        self
    }

    /// Choose whether to include available skill instructions in model context.
    pub fn include_skill_instructions(mut self, enabled: bool) -> Self {
        self.thread_defaults
            .set_prompt_context(INCLUDE_SKILL_INSTRUCTIONS_CONFIG_KEY, enabled);
        self
    }

    /// Merge native config overrides into every new thread's defaults.
    ///
    /// Keys use the dotted paths accepted by `thread/start.config`. Prefer the
    /// typed setters above for common values; use this for current Codex fields
    /// that intentionally have no high-level SDK setter.
    pub fn default_thread_config_overrides(
        mut self,
        overrides: HashMap<String, serde_json::Value>,
    ) -> Self {
        self.thread_defaults.merge_config(overrides);
        self
    }

    /// Explicitly override the approval policy for new threads and turns.
    ///
    /// Without this call, native Codex config and trust defaults are preserved.
    pub fn default_approval_policy(mut self, approval_policy: AskForApproval) -> Self {
        self.approval_policy = Some(approval_policy);
        self
    }

    /// Explicitly override the sandbox for new threads.
    ///
    /// Without this call, native Codex config and trust defaults are preserved.
    pub fn default_sandbox(mut self, sandbox: SandboxMode) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Set whether new threads are ephemeral by default.
    pub fn ephemeral(mut self, ephemeral: bool) -> Self {
        self.ephemeral = ephemeral;
        self
    }

    /// Set the client name reported to Codex.
    pub fn client_name(mut self, name: impl Into<String>) -> Self {
        self.runtime.client_info.name = name.into();
        self
    }

    /// Set the client version reported to Codex.
    pub fn client_version(mut self, version: impl Into<String>) -> Self {
        self.runtime.client_info.version = version.into();
        self
    }

    /// Set both app-server and SDK event-stream queue capacities.
    ///
    /// Transcript, completion, and server-request events apply backpressure
    /// when full and can pause the shared event pump. Best-effort progress
    /// events may be replaced by `Lagged`. This does not change the fixed
    /// pre-attachment event and inactive-route limits.
    pub fn channel_capacity(mut self, capacity: usize) -> Self {
        self.runtime.channels.set_all(capacity);
        self
    }

    /// Set the upstream app-server client's event and command queue capacity.
    ///
    /// Increase this independently when transport bursts are larger than each
    /// application's desired per-stream buffering.
    pub fn app_server_channel_capacity(mut self, capacity: usize) -> Self {
        self.runtime.channels.app_server = capacity.max(1);
        self
    }

    /// Set the capacity of each SDK runtime or thread event stream queue.
    ///
    /// Reliable events apply backpressure when this queue is full; best-effort
    /// events may be summarized by a later `Lagged` marker.
    pub fn event_stream_capacity(mut self, capacity: usize) -> Self {
        self.runtime.channels.event_stream = capacity.max(1);
        self
    }

    /// Start the in-process Codex runtime.
    pub async fn start(self) -> Result<Codex> {
        let mut runtime = self.runtime;
        let arg0_paths = runtime.take_arg0_paths()?;
        let thread_defaults = self.thread_defaults;
        let cwd = match self.cwd {
            Some(cwd) => cwd,
            None => std::env::current_dir()?,
        };
        let config = build_config(
            &arg0_paths,
            ConfigBuildOptions {
                codex_home: self.codex_home,
                cwd,
                model: self.model,
                model_provider: self.model_provider,
                service_tier: self.service_tier,
                reasoning_effort: self.reasoning_effort,
                reasoning_summary: self.reasoning_summary,
                personality: self.personality,
                base_instructions: self.base_instructions,
                developer_instructions: self.developer_instructions,
                approval_policy: self.approval_policy,
                sandbox: self.sandbox,
                ephemeral: self.ephemeral,
            },
        )
        .await?;

        start_with_config_and_paths(config, runtime, arg0_paths, thread_defaults).await
    }
}

/// Builder for starting the SDK from a caller-supplied native Codex config.
///
/// Common effective thread defaults are projected from [`Config`]. The embedded
/// app-server still reloads file and project layers for each thread, so this
/// builder also exposes native default thread params/config overrides for values
/// that exist only as in-memory mutations on a resolved config.
#[derive(Debug, Clone)]
pub struct CodexWithConfigBuilder {
    /// Fully resolved native Codex configuration.
    config: Config,
    /// Runtime startup options that are not part of Codex config.
    runtime: RuntimeOptions,
    /// Ordered native changes applied to the resolved config snapshot.
    thread_defaults: ThreadDefaultsOverrides,
}

impl CodexWithConfigBuilder {
    fn new(config: Config) -> Self {
        Self {
            config,
            runtime: RuntimeOptions::default(),
            thread_defaults: ThreadDefaultsOverrides::default(),
        }
    }

    /// Provide Codex helper executable paths captured by [`run_main`](crate::run_main).
    pub fn arg0_paths(mut self, arg0_paths: Arg0DispatchPaths) -> Self {
        self.runtime.arg0_paths = Some(arg0_paths);
        self
    }

    /// Set the client name reported to Codex.
    pub fn client_name(mut self, name: impl Into<String>) -> Self {
        self.runtime.client_info.name = name.into();
        self
    }

    /// Set the client version reported to Codex.
    pub fn client_version(mut self, version: impl Into<String>) -> Self {
        self.runtime.client_info.version = version.into();
        self
    }

    /// Set both app-server and SDK event-stream queue capacities.
    ///
    /// Transcript, completion, and server-request events apply backpressure
    /// when full and can pause the shared event pump. Best-effort progress
    /// events may be replaced by `Lagged`. This does not change the fixed
    /// pre-attachment event and inactive-route limits.
    pub fn channel_capacity(mut self, capacity: usize) -> Self {
        self.runtime.channels.set_all(capacity);
        self
    }

    /// Set the upstream app-server client's event and command queue capacity.
    ///
    /// Increase this independently when transport bursts are larger than each
    /// application's desired per-stream buffering.
    pub fn app_server_channel_capacity(mut self, capacity: usize) -> Self {
        self.runtime.channels.app_server = capacity.max(1);
        self
    }

    /// Set the capacity of each SDK runtime or thread event stream queue.
    ///
    /// Reliable events apply backpressure when this queue is full; best-effort
    /// events may be summarized by a later `Lagged` marker.
    pub fn event_stream_capacity(mut self, capacity: usize) -> Self {
        self.runtime.channels.event_stream = capacity.max(1);
        self
    }

    /// Replace the native defaults copied into new thread builders.
    ///
    /// The embedded app-server reloads file and project configuration for each
    /// thread. Common resolved values are projected from [`Config`] automatically,
    /// but callers that mutate other thread-scoped config fields in memory should
    /// carry them explicitly through [`ThreadStartParams::config`] here.
    pub fn default_thread_params(mut self, params: ThreadStartParams) -> Self {
        self.thread_defaults.replace(params);
        self
    }

    /// Disable optional model-visible context blocks for small-token sessions.
    pub fn minimal_prompt_context(mut self) -> Self {
        self.thread_defaults.minimize_prompt_context();
        self
    }

    /// Disable the configurable tool families used by a normal Codex agent.
    ///
    /// This applies the same best-effort native per-thread tool profile as
    /// [`CodexBuilder::minimal_tools`]. It does not guarantee an empty tool list;
    /// core, user MCP, dynamic, environment-dependent, and future tools can
    /// remain model-visible.
    pub fn minimal_tools(mut self) -> Self {
        self.thread_defaults.minimize_tools();
        self
    }

    /// Configure new threads for a small-token, chat-oriented session.
    ///
    /// This applies the same prompt, project-document, tool, and environment
    /// defaults as [`CodexBuilder::pure_chat_profile`].
    pub fn pure_chat_profile(mut self) -> Self {
        self.thread_defaults.configure_pure_chat();
        self
    }

    /// Select the default execution-environment behavior for new threads.
    pub fn default_environment_access(mut self, access: EnvironmentAccess) -> Self {
        self.thread_defaults.set_environment_access(access);
        self
    }

    /// Merge native config overrides into every new thread's defaults.
    ///
    /// Keys use the same dotted paths accepted by `thread/start.config`, for
    /// example `features.plugins` or `web_search`. This is the preferred
    /// escape hatch for thread-scoped values that cannot be reconstructed from
    /// a fully resolved [`Config`].
    pub fn default_thread_config_overrides(
        mut self,
        overrides: HashMap<String, serde_json::Value>,
    ) -> Self {
        self.thread_defaults.merge_config(overrides);
        self
    }

    /// Start the in-process Codex runtime.
    pub async fn start(self) -> Result<Codex> {
        start_with_config(self.config, self.runtime, self.thread_defaults).await
    }
}

#[derive(Clone)]
enum RemoteEndpointConfig {
    WebSocket {
        websocket_url: String,
        auth_token: Option<String>,
    },
    UnixSocket {
        socket_path: PathBuf,
    },
}

/// Builder for connecting to an already-running remote Codex app-server.
///
/// Runtime config belongs to the remote app-server process. This builder only
/// owns transport/client identity and SDK-side defaults sent with `thread/start`.
#[derive(Clone)]
pub struct CodexRemoteBuilder {
    /// Remote app-server endpoint to connect to.
    endpoint: RemoteEndpointConfig,
    /// Client metadata reported during remote initialize.
    client_info: ClientInfo,
    /// Independent capacities for the upstream connection and SDK streams.
    channels: ChannelCapacities,
    /// Ordered SDK-side changes applied to new thread defaults.
    thread_defaults: ThreadDefaultsOverrides,
}

impl fmt::Debug for CodexRemoteBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (transport, auth_configured) = match &self.endpoint {
            RemoteEndpointConfig::WebSocket { auth_token, .. } => {
                ("websocket", auth_token.is_some())
            }
            RemoteEndpointConfig::UnixSocket { .. } => ("unix_socket", false),
        };

        formatter
            .debug_struct("CodexRemoteBuilder")
            .field("transport", &transport)
            .field("auth_configured", &auth_configured)
            .field("client_info", &self.client_info)
            .field("thread_defaults", &self.thread_defaults)
            .field("app_server_channel_capacity", &self.channels.app_server)
            .field("event_stream_capacity", &self.channels.event_stream)
            .finish_non_exhaustive()
    }
}

impl CodexRemoteBuilder {
    fn websocket(websocket_url: String) -> Self {
        Self {
            endpoint: RemoteEndpointConfig::WebSocket {
                websocket_url,
                auth_token: None,
            },
            client_info: ClientInfo::default(),
            channels: ChannelCapacities::default(),
            thread_defaults: ThreadDefaultsOverrides::default(),
        }
    }

    fn unix_socket(socket_path: PathBuf) -> Self {
        Self {
            endpoint: RemoteEndpointConfig::UnixSocket { socket_path },
            client_info: ClientInfo::default(),
            channels: ChannelCapacities::default(),
            thread_defaults: ThreadDefaultsOverrides::default(),
        }
    }

    /// Set a bearer auth token for a remote WebSocket app-server.
    ///
    /// This only applies to builders created by
    /// [`Codex::remote_websocket`](crate::Codex::remote_websocket).
    /// Remote auth tokens are only valid for `wss://` URLs or loopback `ws://`
    /// URLs, matching upstream Codex transport policy.
    pub fn auth_token(mut self, auth_token: impl Into<String>) -> Self {
        if let RemoteEndpointConfig::WebSocket {
            auth_token: token, ..
        } = &mut self.endpoint
        {
            *token = Some(auth_token.into());
        }
        self
    }

    /// Set the client name reported to the remote app-server.
    pub fn client_name(mut self, name: impl Into<String>) -> Self {
        self.client_info.name = name.into();
        self
    }

    /// Set the client version reported to the remote app-server.
    pub fn client_version(mut self, version: impl Into<String>) -> Self {
        self.client_info.version = version.into();
        self
    }

    /// Set both app-server and SDK event-stream queue capacities.
    ///
    /// Transcript, completion, and server-request events apply backpressure
    /// when full and can pause the shared event pump. Best-effort progress
    /// events may be replaced by `Lagged`. This does not change the fixed
    /// pre-attachment event and inactive-route limits.
    pub fn channel_capacity(mut self, capacity: usize) -> Self {
        self.channels.set_all(capacity);
        self
    }

    /// Set the upstream app-server client's event and command queue capacity.
    ///
    /// Increase this independently when transport bursts are larger than each
    /// application's desired per-stream buffering.
    pub fn app_server_channel_capacity(mut self, capacity: usize) -> Self {
        self.channels.app_server = capacity.max(1);
        self
    }

    /// Set the capacity of each SDK runtime or thread event stream queue.
    ///
    /// Reliable events apply backpressure when this queue is full; best-effort
    /// events may be summarized by a later `Lagged` marker.
    pub fn event_stream_capacity(mut self, capacity: usize) -> Self {
        self.channels.event_stream = capacity.max(1);
        self
    }

    /// Replace default params copied into new thread builders.
    ///
    /// These defaults are SDK-side conveniences only. The remote app-server
    /// remains the owner of its runtime config.
    pub fn default_thread_params(mut self, params: ThreadStartParams) -> Self {
        self.thread_defaults.replace(params);
        self
    }

    /// Connect to the remote app-server.
    pub async fn start(self) -> Result<Codex> {
        let endpoint = match self.endpoint {
            RemoteEndpointConfig::WebSocket {
                websocket_url,
                auth_token,
            } => RemoteAppServerEndpoint::WebSocket {
                websocket_url,
                auth_token,
            },
            RemoteEndpointConfig::UnixSocket { socket_path } => {
                RemoteAppServerEndpoint::UnixSocket {
                    socket_path: AbsolutePathBuf::from_absolute_path(socket_path)
                        .map_err(Error::config)?,
                }
            }
        };

        let runtime = RuntimeHandle::connect_remote(
            RemoteAppServerConnectArgs {
                endpoint,
                client_name: self.client_info.name,
                client_version: self.client_info.version,
                experimental_api: true,
                mcp_server_openai_form_elicitation: false,
                opt_out_notification_methods: Vec::new(),
                channel_capacity: self.channels.app_server,
            },
            self.channels.event_stream,
        )
        .await?;

        let default_thread_params =
            self.thread_defaults.apply(remote_default_thread_params());
        Ok(Codex::from_runtime(runtime, default_thread_params))
    }
}

async fn start_with_config(
    config: Config,
    mut runtime: RuntimeOptions,
    thread_defaults: ThreadDefaultsOverrides,
) -> Result<Codex> {
    let arg0_paths = runtime.take_arg0_paths()?;
    start_with_config_and_paths(config, runtime, arg0_paths, thread_defaults).await
}

async fn start_with_config_and_paths(
    config: Config,
    runtime: RuntimeOptions,
    arg0_paths: Arg0DispatchPaths,
    thread_defaults: ThreadDefaultsOverrides,
) -> Result<Codex> {
    let default_thread_params = thread_defaults.apply(thread_defaults_snapshot(&config));
    let RuntimeOptions {
        client_info,
        channels,
        ..
    } = runtime;
    let runtime = RuntimeHandle::start(
        arg0_paths,
        config,
        client_info,
        channels.app_server,
        channels.event_stream,
    )
    .await?;

    Ok(Codex::from_runtime(runtime, default_thread_params))
}

fn remote_default_thread_params() -> ThreadStartParams {
    ThreadStartParams {
        ephemeral: Some(true),
        thread_source: Some(ThreadSource::User),
        ..Default::default()
    }
}

fn default_thread_list_params() -> ThreadListParams {
    ThreadListParams {
        cursor: None,
        limit: None,
        sort_key: None,
        sort_direction: None,
        model_providers: None,
        source_kinds: None,
        archived: None,
        cwd: None,
        use_state_db_only: false,
        search_term: None,
        parent_thread_id: None,
        ancestor_thread_id: None,
    }
}

struct ConfigBuildOptions {
    codex_home: Option<PathBuf>,
    cwd: PathBuf,
    model: Option<String>,
    model_provider: Option<String>,
    service_tier: Option<Option<String>>,
    reasoning_effort: Option<ReasoningEffort>,
    reasoning_summary: Option<ReasoningSummary>,
    personality: Option<Personality>,
    base_instructions: Option<String>,
    developer_instructions: Option<String>,
    approval_policy: Option<AskForApproval>,
    sandbox: Option<SandboxMode>,
    ephemeral: bool,
}

async fn build_config(
    arg0_paths: &Arg0DispatchPaths,
    options: ConfigBuildOptions,
) -> Result<Config> {
    let codex_home = match options.codex_home {
        Some(path) => path,
        None => find_codex_home().map_err(Error::config)?.to_path_buf(),
    };

    let mut config = ConfigBuilder::default()
        .codex_home(codex_home)
        .harness_overrides(ConfigOverrides {
            cwd: Some(options.cwd),
            model: options.model,
            model_provider: options.model_provider,
            service_tier: options.service_tier,
            base_instructions: options.base_instructions,
            developer_instructions: options.developer_instructions,
            personality: options.personality,
            approval_policy: options.approval_policy.map(AskForApproval::to_core),
            sandbox_mode: options.sandbox.map(SandboxMode::to_core),
            codex_self_exe: arg0_paths.codex_self_exe.clone(),
            codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe.clone(),
            main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
            ephemeral: Some(options.ephemeral),
            ..Default::default()
        })
        .build()
        .await
        .map_err(Error::config)?;

    if let Some(effort) = options.reasoning_effort {
        config.model_reasoning_effort = Some(effort);
    }
    if let Some(summary) = options.reasoning_summary {
        config.model_reasoning_summary = Some(summary);
    }
    Ok(config)
}

impl IntoFuture for CodexBuilder {
    type Output = Result<Codex>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.start().await })
    }
}

impl IntoFuture for CodexWithConfigBuilder {
    type Output = Result<Codex>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.start().await })
    }
}

impl IntoFuture for CodexRemoteBuilder {
    type Output = Result<Codex>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.start().await })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread_defaults::merge_thread_config_overrides;

    fn test_config_build_options(
        codex_home: &std::path::Path,
        cwd: &std::path::Path,
    ) -> ConfigBuildOptions {
        ConfigBuildOptions {
            codex_home: Some(codex_home.to_path_buf()),
            cwd: cwd.to_path_buf(),
            model: None,
            model_provider: None,
            service_tier: None,
            reasoning_effort: None,
            reasoning_summary: None,
            personality: None,
            base_instructions: None,
            developer_instructions: None,
            approval_policy: None,
            sandbox: None,
            ephemeral: true,
        }
    }

    #[test]
    fn remote_builder_debug_redacts_endpoint_and_auth_token() {
        let builder = Codex::remote_websocket(
            "wss://example.test/app-server?credential=url-secret",
        )
        .auth_token("bearer-secret");

        let debug = format!("{builder:?}");
        assert!(debug.contains("auth_configured: true"));
        assert!(!debug.contains("url-secret"));
        assert!(!debug.contains("bearer-secret"));
    }

    #[test]
    fn builder_debug_redacts_native_thread_config_values() {
        let builder =
            Codex::builder().default_thread_config_overrides(HashMap::from([(
                "example.token".to_string(),
                serde_json::Value::String("thread-secret".to_string()),
            )]));

        let debug = format!("{builder:?}");
        assert!(debug.contains("MergeConfig"));
        assert!(debug.contains("count: 1"));
        assert!(!debug.contains("thread-secret"));
    }

    #[test]
    fn minimal_tools_sets_documented_native_overrides() {
        let builder = Codex::builder().minimal_tools();
        let params = builder.thread_defaults.apply(ThreadStartParams::default());
        let overrides = params.config.expect("minimal tool config");

        for key in [
            "features.apps",
            "features.plugins",
            "features.shell_tool",
            "features.multi_agent",
            "features.image_generation",
            "features.memories",
            "features.goals",
            "orchestrator.skills.enabled",
            "orchestrator.mcp.enabled",
            "tools.experimental_request_user_input.enabled",
        ] {
            assert_eq!(
                overrides.get(key),
                Some(&serde_json::Value::Bool(false)),
                "unexpected override for {key}"
            );
        }
        assert_eq!(
            overrides.get("web_search"),
            Some(&serde_json::Value::String("disabled".to_string()))
        );
        assert_eq!(params.environments, None);
    }

    #[test]
    fn later_native_overrides_can_customize_minimal_tools() {
        let builder = Codex::builder()
            .minimal_tools()
            .default_thread_config_overrides(HashMap::from([(
                "features.image_generation".to_string(),
                serde_json::Value::Bool(true),
            )]));

        let params = builder.thread_defaults.apply(ThreadStartParams::default());
        assert_eq!(
            params
                .config
                .as_ref()
                .and_then(|config| config.get("features.image_generation")),
            Some(&serde_json::Value::Bool(true))
        );
    }

    #[test]
    fn pure_chat_profile_disables_prompt_context_project_docs_tools_and_environment() {
        let builder = Codex::builder().pure_chat_profile();
        let params = builder.thread_defaults.apply(ThreadStartParams::default());

        assert_eq!(params.environments, Some(Vec::new()));
        let config = params.config.expect("pure chat config");
        assert_eq!(
            config.get("include_apps_instructions"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(
            config.get("features.plugins"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(
            config.get("project_doc_max_bytes"),
            Some(&serde_json::Value::from(0))
        );
    }

    #[test]
    fn later_builder_calls_can_customize_the_pure_chat_profile() {
        let builder = Codex::builder()
            .pure_chat_profile()
            .include_apps_instructions(true)
            .default_thread_config_overrides(HashMap::from([(
                "project_doc_max_bytes".to_string(),
                serde_json::Value::from(1024),
            )]))
            .default_environment_access(EnvironmentAccess::Inherit);
        let params = builder.thread_defaults.apply(ThreadStartParams::default());

        assert_eq!(params.environments, None);
        assert_eq!(
            params
                .config
                .as_ref()
                .and_then(|config| config.get("include_apps_instructions")),
            Some(&serde_json::Value::Bool(true))
        );
        assert_eq!(
            params
                .config
                .as_ref()
                .and_then(|config| config.get("project_doc_max_bytes")),
            Some(&serde_json::Value::from(1024))
        );
    }

    #[test]
    fn in_process_runtime_requires_arg0_dispatch_paths() {
        let mut options = RuntimeOptions::default();
        assert!(matches!(
            options.take_arg0_paths(),
            Err(Error::Arg0DispatchRequired)
        ));
    }

    #[test]
    fn channel_capacities_can_be_tuned_together_or_independently() {
        let builder = Codex::builder()
            .channel_capacity(64)
            .app_server_channel_capacity(128)
            .event_stream_capacity(16);

        assert_eq!(builder.runtime.channels.app_server, 128);
        assert_eq!(builder.runtime.channels.event_stream, 16);
    }

    #[test]
    fn explicit_thread_config_overrides_replace_derived_values() {
        let mut params = ThreadStartParams {
            config: Some(HashMap::from([(
                "features.plugins".to_string(),
                serde_json::Value::Bool(true),
            )])),
            ..Default::default()
        };

        merge_thread_config_overrides(
            &mut params,
            HashMap::from([
                (
                    "features.plugins".to_string(),
                    serde_json::Value::Bool(false),
                ),
                (
                    "project_doc_max_bytes".to_string(),
                    serde_json::Value::from(0),
                ),
            ]),
        );

        let config = params.config.expect("merged config");
        assert_eq!(
            config.get("features.plugins"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(
            config.get("project_doc_max_bytes"),
            Some(&serde_json::Value::from(0))
        );
    }

    #[tokio::test]
    async fn native_sandbox_and_approval_are_inherited_without_builder_overrides() {
        let codex_home = tempfile::tempdir().expect("create temp CODEX_HOME");
        let cwd = tempfile::tempdir().expect("create temp cwd");
        std::fs::write(
            codex_home.path().join("config.toml"),
            "approval_policy = \"never\"\nsandbox_mode = \"read-only\"\n",
        )
        .expect("write native config");

        let config = build_config(
            &Arg0DispatchPaths::default(),
            test_config_build_options(codex_home.path(), cwd.path()),
        )
        .await
        .expect("build test config");
        let params = thread_defaults_snapshot(&config);

        assert_eq!(params.approval_policy, Some(AskForApproval::Never));
        assert_eq!(params.sandbox, Some(SandboxMode::ReadOnly));
    }

    #[tokio::test]
    async fn runtime_defaults_are_forwarded_to_native_thread_params() {
        let codex_home = tempfile::tempdir().expect("create temp CODEX_HOME");
        let cwd = tempfile::tempdir().expect("create temp cwd");
        std::fs::write(
            codex_home.path().join("config.toml"),
            "approval_policy = \"on-request\"\nsandbox_mode = \"workspace-write\"\n",
        )
        .expect("write native config");
        let mut options = test_config_build_options(codex_home.path(), cwd.path());
        options.service_tier = Some(None);
        options.reasoning_effort = Some(ReasoningEffort::High);
        options.reasoning_summary = Some(ReasoningSummary::Detailed);
        options.base_instructions = Some("test base".to_string());
        options.developer_instructions = Some("test developer".to_string());
        options.approval_policy = Some(AskForApproval::Never);
        options.sandbox = Some(SandboxMode::ReadOnly);
        let mut config = build_config(&Arg0DispatchPaths::default(), options)
            .await
            .expect("build test config");

        config.include_permissions_instructions = false;
        config.include_apps_instructions = false;
        config.include_collaboration_mode_instructions = false;
        config.include_environment_context = false;
        config.include_skill_instructions = false;

        let params = thread_defaults_snapshot(&config);
        // Codex normalizes an explicit clear to its wire-level `default` tier.
        assert_eq!(params.service_tier, Some(Some("default".to_string())));
        assert_eq!(params.sandbox, Some(SandboxMode::ReadOnly));
        assert_eq!(params.approval_policy, Some(AskForApproval::Never));
        assert_eq!(params.base_instructions.as_deref(), Some("test base"));
        assert_eq!(
            params.developer_instructions.as_deref(),
            Some("test developer")
        );

        let overrides = params.config.expect("thread config overrides");
        assert_eq!(overrides.len(), 7);
        for key in [
            "include_permissions_instructions",
            "include_apps_instructions",
            "include_collaboration_mode_instructions",
            "include_environment_context",
            "skills.include_instructions",
        ] {
            assert_eq!(overrides.get(key), Some(&serde_json::Value::Bool(false)));
        }
        assert_eq!(
            overrides.get("model_reasoning_effort"),
            Some(&serde_json::Value::String("high".to_string()))
        );
        assert_eq!(
            overrides.get("model_reasoning_summary"),
            Some(&serde_json::Value::String("detailed".to_string()))
        );

        // A caller-supplied resolved Config can also represent a direct clear;
        // keep that distinct from an omitted thread override.
        config.service_tier = None;
        let cleared = thread_defaults_snapshot(&config);
        assert_eq!(cleared.service_tier, Some(None));
    }
}
