use std::future::{Future, IntoFuture};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use codex_app_server_client::{RemoteAppServerConnectArgs, RemoteAppServerEndpoint};
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

use crate::error::{Error, Result};
use crate::runtime::{DEFAULT_CHANNEL_CAPACITY, RuntimeHandle};
use crate::thread::{Thread, ThreadBuilder};
use crate::turn::{CodexTurnBuilder, IntoTurnInput};
use crate::types::ClientInfo;
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

#[derive(Debug, Clone)]
struct RuntimeOptions {
    /// Helper executable paths discovered by `codex_arg0`.
    arg0_paths: Option<Arg0DispatchPaths>,
    /// Client metadata reported to Codex.
    client_info: ClientInfo,
    /// Capacity for runtime event and command channels.
    channel_capacity: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct PromptContextOptions {
    include_permissions_instructions: Option<bool>,
    include_apps_instructions: Option<bool>,
    include_collaboration_mode_instructions: Option<bool>,
    include_environment_context: Option<bool>,
    include_skill_instructions: Option<bool>,
}

impl PromptContextOptions {
    fn minimal() -> Self {
        Self {
            include_permissions_instructions: Some(false),
            include_apps_instructions: Some(false),
            include_collaboration_mode_instructions: Some(false),
            include_environment_context: Some(false),
            include_skill_instructions: Some(false),
        }
    }

    fn apply_to(self, config: &mut Config) {
        if let Some(enabled) = self.include_permissions_instructions {
            config.include_permissions_instructions = enabled;
        }
        if let Some(enabled) = self.include_apps_instructions {
            config.include_apps_instructions = enabled;
        }
        if let Some(enabled) = self.include_collaboration_mode_instructions {
            config.include_collaboration_mode_instructions = enabled;
        }
        if let Some(enabled) = self.include_environment_context {
            config.include_environment_context = enabled;
        }
        if let Some(enabled) = self.include_skill_instructions {
            config.include_skill_instructions = enabled;
        }
    }
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            arg0_paths: None,
            client_info: ClientInfo::default(),
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
        }
    }
}

impl RuntimeOptions {
    fn take_arg0_paths(&mut self) -> Arg0DispatchPaths {
        self.arg0_paths.take().unwrap_or_else(fallback_arg0_paths)
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
        let event_rx = self.runtime().subscribe();
        let response: ThreadResumeResponse = self
            .request_typed(ClientRequest::ThreadResume {
                request_id: self.next_request_id(),
                params,
            })
            .await?;
        let thread_id = response.thread.id;
        tracing::info!(%thread_id, "resumed Codex thread");
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

        let event_rx = self.runtime().subscribe();
        let response: ThreadForkResponse = self
            .request_typed(ClientRequest::ThreadFork {
                request_id: self.next_request_id(),
                params,
            })
            .await?;
        let thread_id = response.thread.id;
        tracing::info!(%thread_id, "forked Codex thread");
        Ok(Thread::from_id(self.clone(), thread_id, event_rx))
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
        self.request_typed(ClientRequest::ThreadArchive {
            request_id: self.next_request_id(),
            params: ThreadArchiveParams {
                thread_id: thread_id.into(),
            },
        })
        .await
    }

    /// Restore an archived Codex thread and return a reusable handle.
    pub async fn unarchive_thread(&self, thread_id: impl Into<String>) -> Result<Thread> {
        let event_rx = self.runtime().subscribe();
        let response: ThreadUnarchiveResponse = self
            .request_typed(ClientRequest::ThreadUnarchive {
                request_id: self.next_request_id(),
                params: ThreadUnarchiveParams {
                    thread_id: thread_id.into(),
                },
            })
            .await?;
        let thread_id = response.thread.id;
        tracing::info!(%thread_id, "unarchived Codex thread");
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

    pub(crate) fn default_thread_params(&self) -> &ThreadStartParams {
        &self.inner.default_thread_params
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
    /// Optional model-visible prompt context controls.
    prompt_context: PromptContextOptions,
    /// Default approval policy for new threads and turns.
    approval_policy: AskForApproval,
    /// Default sandbox for new threads.
    sandbox: SandboxMode,
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
            prompt_context: PromptContextOptions::default(),
            approval_policy: AskForApproval::OnRequest,
            sandbox: SandboxMode::WorkspaceWrite,
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
        self.prompt_context = PromptContextOptions::minimal();
        self
    }

    /// Choose whether to include permission/sandbox guidance in model context.
    pub fn include_permissions_instructions(mut self, enabled: bool) -> Self {
        self.prompt_context.include_permissions_instructions = Some(enabled);
        self
    }

    /// Choose whether to include app/connector guidance in model context.
    pub fn include_apps_instructions(mut self, enabled: bool) -> Self {
        self.prompt_context.include_apps_instructions = Some(enabled);
        self
    }

    /// Choose whether to include collaboration-mode guidance in model context.
    pub fn include_collaboration_mode_instructions(mut self, enabled: bool) -> Self {
        self.prompt_context.include_collaboration_mode_instructions = Some(enabled);
        self
    }

    /// Choose whether to include environment metadata in model context.
    pub fn include_environment_context(mut self, enabled: bool) -> Self {
        self.prompt_context.include_environment_context = Some(enabled);
        self
    }

    /// Choose whether to include available skill instructions in model context.
    pub fn include_skill_instructions(mut self, enabled: bool) -> Self {
        self.prompt_context.include_skill_instructions = Some(enabled);
        self
    }

    /// Set the default approval policy for new threads and turns.
    pub fn default_approval_policy(mut self, approval_policy: AskForApproval) -> Self {
        self.approval_policy = approval_policy;
        self
    }

    /// Set the default sandbox for new threads.
    pub fn default_sandbox(mut self, sandbox: SandboxMode) -> Self {
        self.sandbox = sandbox;
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

    /// Set the capacity of the internal broadcast and command channels.
    ///
    /// Small values reduce buffering, while larger values tolerate slower event
    /// consumers before [`AppServerEvent::Lagged`](crate::AppServerEvent::Lagged)
    /// appears.
    pub fn channel_capacity(mut self, capacity: usize) -> Self {
        self.runtime.channel_capacity = capacity.max(1);
        self
    }

    /// Start the in-process Codex runtime.
    pub async fn start(self) -> Result<Codex> {
        let mut runtime = self.runtime;
        let arg0_paths = runtime.take_arg0_paths();
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
                prompt_context: self.prompt_context,
                approval_policy: self.approval_policy,
                sandbox: self.sandbox,
                ephemeral: self.ephemeral,
            },
        )
        .await?;

        start_with_config_and_paths(config, runtime, arg0_paths).await
    }
}

/// Builder for starting the SDK from a caller-supplied native Codex config.
///
/// This builder intentionally exposes only runtime options that are not already
/// represented by [`Config`].
#[derive(Debug, Clone)]
pub struct CodexWithConfigBuilder {
    /// Fully resolved native Codex configuration.
    config: Config,
    /// Runtime startup options that are not part of Codex config.
    runtime: RuntimeOptions,
}

impl CodexWithConfigBuilder {
    fn new(config: Config) -> Self {
        Self {
            config,
            runtime: RuntimeOptions::default(),
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

    /// Set the capacity of the internal broadcast and command channels.
    ///
    /// Small values reduce buffering, while larger values tolerate slower event
    /// consumers before [`AppServerEvent::Lagged`](crate::AppServerEvent::Lagged)
    /// appears.
    pub fn channel_capacity(mut self, capacity: usize) -> Self {
        self.runtime.channel_capacity = capacity.max(1);
        self
    }

    /// Start the in-process Codex runtime.
    pub async fn start(self) -> Result<Codex> {
        start_with_config(self.config, self.runtime).await
    }
}

#[derive(Debug, Clone)]
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
/// Remote mode intentionally exposes only transport and client-identity
/// options. Model, sandbox, prompt, and other Codex config defaults belong to
/// the remote app-server process; use thread/turn builders or native params for
/// per-request overrides.
#[derive(Debug, Clone)]
pub struct CodexRemoteBuilder {
    /// Remote app-server endpoint to connect to.
    endpoint: RemoteEndpointConfig,
    /// Client metadata reported during remote initialize.
    client_info: ClientInfo,
    /// Capacity for SDK broadcast and command channels.
    channel_capacity: usize,
    /// Native Codex defaults copied into new thread builders.
    default_thread_params: ThreadStartParams,
}

impl CodexRemoteBuilder {
    fn websocket(websocket_url: String) -> Self {
        Self {
            endpoint: RemoteEndpointConfig::WebSocket {
                websocket_url,
                auth_token: None,
            },
            client_info: ClientInfo::default(),
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            default_thread_params: remote_default_thread_params(),
        }
    }

    fn unix_socket(socket_path: PathBuf) -> Self {
        Self {
            endpoint: RemoteEndpointConfig::UnixSocket { socket_path },
            client_info: ClientInfo::default(),
            channel_capacity: DEFAULT_CHANNEL_CAPACITY,
            default_thread_params: remote_default_thread_params(),
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

    /// Set the capacity of the internal broadcast and command channels.
    pub fn channel_capacity(mut self, capacity: usize) -> Self {
        self.channel_capacity = capacity.max(1);
        self
    }

    /// Replace default params copied into new thread builders.
    ///
    /// These defaults are SDK-side conveniences only. The remote app-server
    /// remains the owner of its runtime config.
    pub fn default_thread_params(mut self, params: ThreadStartParams) -> Self {
        self.default_thread_params = params;
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

        let runtime = RuntimeHandle::connect_remote(RemoteAppServerConnectArgs {
            endpoint,
            client_name: self.client_info.name,
            client_version: self.client_info.version,
            experimental_api: true,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: self.channel_capacity,
        })
        .await?;

        Ok(Codex::from_runtime(runtime, self.default_thread_params))
    }
}

async fn start_with_config(config: Config, mut runtime: RuntimeOptions) -> Result<Codex> {
    let arg0_paths = runtime.take_arg0_paths();
    start_with_config_and_paths(config, runtime, arg0_paths).await
}

async fn start_with_config_and_paths(
    config: Config,
    runtime: RuntimeOptions,
    arg0_paths: Arg0DispatchPaths,
) -> Result<Codex> {
    let default_thread_params = default_thread_params_from_config(&config);
    let RuntimeOptions {
        client_info,
        channel_capacity,
        ..
    } = runtime;
    let runtime =
        RuntimeHandle::start(arg0_paths, config, client_info, channel_capacity).await?;

    Ok(Codex::from_runtime(runtime, default_thread_params))
}

fn remote_default_thread_params() -> ThreadStartParams {
    ThreadStartParams {
        ephemeral: Some(true),
        thread_source: Some(ThreadSource::User),
        ..Default::default()
    }
}

fn default_thread_params_from_config(config: &Config) -> ThreadStartParams {
    ThreadStartParams {
        cwd: Some(config.cwd.display().to_string()),
        model: config.model.clone(),
        model_provider: Some(config.model_provider_id.clone()),
        service_tier: config.service_tier.clone().map(Some),
        approval_policy: Some(AskForApproval::from(
            config.permissions.approval_policy.value(),
        )),
        personality: config.personality,
        ephemeral: Some(config.ephemeral),
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
    prompt_context: PromptContextOptions,
    approval_policy: AskForApproval,
    sandbox: SandboxMode,
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
            approval_policy: Some(options.approval_policy.to_core()),
            sandbox_mode: Some(options.sandbox.to_core()),
            codex_self_exe: arg0_paths.codex_self_exe.clone(),
            codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe.clone(),
            main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
            ephemeral: Some(options.ephemeral),
            ..Default::default()
        })
        .build()
        .await
        .map_err(Error::config)?;

    options.prompt_context.apply_to(&mut config);
    if let Some(effort) = options.reasoning_effort {
        config.model_reasoning_effort = Some(effort);
    }
    if let Some(summary) = options.reasoning_summary {
        config.model_reasoning_summary = Some(summary);
    }
    Ok(config)
}

fn fallback_arg0_paths() -> Arg0DispatchPaths {
    let current_exe = std::env::current_exe().ok();
    Arg0DispatchPaths {
        codex_self_exe: current_exe.clone(),
        codex_linux_sandbox_exe: if cfg!(target_os = "linux") {
            current_exe
        } else {
            None
        },
        main_execve_wrapper_exe: None,
    }
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
