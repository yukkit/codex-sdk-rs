//! Warmup helpers for the Codex app-server runtime connection.
//!
//! The warmup API sends idempotent inventory and config requests through an
//! already-started app-server. It does not create a Codex thread, which makes it
//! useful when an embedding application wants to hide cold-start cost before the
//! first user-visible session.
//!
//! This is a best-effort warmup over public app-server protocol methods. A
//! single warmup step failure is recorded in [`WarmupResult::failures`] instead
//! of failing the whole run, so callers can still benefit from the steps that
//! succeeded. Deeper session-init caches that are owned by Codex core may still
//! be built when the first thread is created until the protocol exposes a
//! dedicated session warmup request.

use std::future::{Future, IntoFuture};
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;

use codex_app_server_protocol::{
    AppsListParams, AppsListResponse, ClientRequest, ConfigRequirementsReadResponse,
    GetAccountParams, GetAccountResponse, ListMcpServerStatusParams,
    ListMcpServerStatusResponse, McpServerStatusDetail, ModelListParams,
    ModelListResponse, PermissionProfileListParams, PermissionProfileListResponse,
    SkillsListParams, SkillsListResponse, ThreadStartParams,
};
use tracing::Instrument;

use crate::client::Codex;
use crate::error::{Error, Result};
use crate::types::cwd_to_string;

const DEFAULT_STEP_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
struct WarmupTargets {
    thread_params: ThreadStartParams,
    models: bool,
    skills: bool,
    force_reload_skills: bool,
    permission_profiles: bool,
    config_requirements: bool,
    account: bool,
    mcp_status: bool,
    apps: bool,
    step_timeout: Duration,
}

impl Default for WarmupTargets {
    fn default() -> Self {
        Self {
            thread_params: ThreadStartParams::default(),
            models: true,
            skills: true,
            force_reload_skills: false,
            permission_profiles: true,
            config_requirements: true,
            account: true,
            mcp_status: true,
            apps: false,
            step_timeout: DEFAULT_STEP_TIMEOUT,
        }
    }
}

/// Summary of caches and inventories touched by a warmup run.
///
/// Each field is `None` when the matching warmup step was disabled, and `Some`
/// when that request completed successfully.
#[derive(Debug, Clone, Default)]
pub struct WarmupResult {
    /// Number of models returned by `model/list`.
    pub models: Option<usize>,
    /// Number of skills returned by `skills/list` across all warmed cwds.
    pub skills: Option<usize>,
    /// Number of permission profiles returned by `permissionProfile/list`.
    pub permission_profiles: Option<usize>,
    /// Whether `configRequirements/read` found configured requirements.
    pub config_requirements: Option<bool>,
    /// Whether `account/read` returned an account.
    pub account: Option<bool>,
    /// Number of MCP servers returned by `mcpServerStatus/list`.
    pub mcp_servers: Option<usize>,
    /// Number of apps returned by `app/list`.
    pub apps: Option<usize>,
    /// Best-effort warmup steps that failed.
    pub failures: Vec<WarmupFailure>,
}

impl WarmupResult {
    /// Whether every enabled warmup step completed successfully.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }
}

/// A non-fatal failure from one warmup step.
///
/// Warmup is intentionally best-effort: if a step such as `app/list` is blocked
/// by auth or network policy, the SDK records that failure and keeps warming
/// the remaining steps.
#[derive(Debug, Clone)]
pub struct WarmupFailure {
    /// App-server method or logical step that failed.
    pub step: &'static str,
    /// Human-readable error returned by the app-server protocol layer.
    pub error: String,
}

/// Builder for warming Codex app-server caches without creating a thread.
///
/// Create one with [`Codex::warmup`](crate::Codex::warmup). The default builder
/// warms the model catalog, skills, permission profiles, managed config
/// requirements, account lookup, and MCP server status. App/connector listing
/// can be enabled with [`apps`](Self::apps).
///
/// # Examples
///
/// ```no_run
/// # async fn run(codex: codex_sdk::Codex) -> codex_sdk::Result<()> {
/// let warmed = codex.warmup().send().await?;
/// println!("warmed {} skills", warmed.skills.unwrap_or_default());
/// for failure in &warmed.failures {
///     eprintln!("warmup step {} failed: {}", failure.step, failure.error);
/// }
/// # Ok(())
/// # }
/// ```
pub struct WarmupBuilder {
    codex: Codex,
    targets: WarmupTargets,
}

impl WarmupBuilder {
    pub(crate) fn new(codex: Codex) -> Self {
        Self {
            codex,
            targets: WarmupTargets::default(),
        }
    }

    /// Replace native thread/start params used to resolve cwd/config-sensitive requests.
    ///
    /// This is convenient when an application already builds the same params it
    /// will later pass to [`Codex::thread`](crate::Codex::thread).
    pub fn thread_params(mut self, params: ThreadStartParams) -> Self {
        self.targets.thread_params = params;
        self
    }

    /// Set the cwd used by cwd/config-sensitive warmup requests.
    ///
    /// Passing the same cwd that the first user session will use gives skills
    /// and permission-profile warmup the closest cache match that the public
    /// app-server protocol currently supports.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        self.targets.thread_params.cwd = Some(cwd_to_string(&cwd));
        self
    }

    /// Set the model associated with the warmup target.
    ///
    /// Current public warmup requests do not create a model-bound thread, but
    /// keeping the option here lets callers reuse the same option set they use
    /// for thread creation and leaves room for future protocol-level warmups.
    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.targets.thread_params.model = Some(model.into());
        self
    }

    /// Set the model provider associated with the warmup target.
    pub fn model_provider(mut self, model_provider: impl Into<String>) -> Self {
        self.targets.thread_params.model_provider = Some(model_provider.into());
        self
    }

    /// Enable or disable model catalog warmup.
    pub fn models(mut self, enabled: bool) -> Self {
        self.targets.models = enabled;
        self
    }

    /// Enable or disable skills warmup.
    pub fn skills(mut self, enabled: bool) -> Self {
        self.targets.skills = enabled;
        self
    }

    /// Force the skills warmup request to bypass cached skill state.
    pub fn force_reload_skills(mut self, enabled: bool) -> Self {
        self.targets.force_reload_skills = enabled;
        self
    }

    /// Enable or disable permission profile warmup.
    pub fn permission_profiles(mut self, enabled: bool) -> Self {
        self.targets.permission_profiles = enabled;
        self
    }

    /// Enable or disable config requirements warmup.
    pub fn config_requirements(mut self, enabled: bool) -> Self {
        self.targets.config_requirements = enabled;
        self
    }

    /// Enable or disable account/auth warmup.
    pub fn account(mut self, enabled: bool) -> Self {
        self.targets.account = enabled;
        self
    }

    /// Enable or disable MCP status/tool inventory warmup.
    pub fn mcp_status(mut self, enabled: bool) -> Self {
        self.targets.mcp_status = enabled;
        self
    }

    /// Enable or disable app/connector listing warmup.
    pub fn apps(mut self, enabled: bool) -> Self {
        self.targets.apps = enabled;
        self
    }

    /// Set the maximum duration of each individual warmup request.
    ///
    /// A timed-out step is recorded in [`WarmupResult::failures`] and later
    /// steps continue normally. The default is 30 seconds per request.
    pub fn step_timeout(mut self, timeout: Duration) -> Self {
        self.targets.step_timeout = timeout;
        self
    }

    /// Run the warmup requests.
    ///
    /// Requests are executed sequentially to keep app-server startup behavior
    /// predictable. Individual request failures are captured in
    /// [`WarmupResult::failures`] and do not stop later warmup steps.
    pub async fn send(mut self) -> Result<WarmupResult> {
        let cwd = self.take_effective_cwd();
        let span = tracing::info_span!(
            "codex_sdk.warmup",
            cwd = cwd
                .as_ref()
                .map(|cwd| cwd.display().to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
        Ok(self.send_inner(cwd).instrument(span).await)
    }

    fn take_effective_cwd(&mut self) -> Option<PathBuf> {
        self.targets
            .thread_params
            .cwd
            .take()
            .or_else(|| self.codex.default_thread_params().cwd.clone())
            .map(PathBuf::from)
    }

    async fn send_inner(self, cwd: Option<PathBuf>) -> WarmupResult {
        let cwd_string = cwd.as_ref().map(cwd_to_string);
        let mut result = WarmupResult::default();

        if self.targets.models {
            match self
                .request::<ModelListResponse>(ClientRequest::ModelList {
                    request_id: self.codex.next_request_id(),
                    params: ModelListParams {
                        include_hidden: Some(true),
                        ..Default::default()
                    },
                })
                .await
            {
                Ok(response) => result.models = Some(response.data.len()),
                Err(error) => result.push_failure("model/list", error),
            }
        }

        if self.targets.skills {
            match self
                .request::<SkillsListResponse>(ClientRequest::SkillsList {
                    request_id: self.codex.next_request_id(),
                    params: SkillsListParams {
                        cwds: cwd.into_iter().collect(),
                        force_reload: self.targets.force_reload_skills,
                    },
                })
                .await
            {
                Ok(response) => {
                    result.skills =
                        Some(response.data.iter().map(|entry| entry.skills.len()).sum());
                }
                Err(error) => result.push_failure("skills/list", error),
            }
        }

        if self.targets.permission_profiles {
            match self
                .request::<PermissionProfileListResponse>(
                    ClientRequest::PermissionProfileList {
                        request_id: self.codex.next_request_id(),
                        params: PermissionProfileListParams {
                            cursor: None,
                            limit: None,
                            cwd: cwd_string.clone(),
                        },
                    },
                )
                .await
            {
                Ok(response) => result.permission_profiles = Some(response.data.len()),
                Err(error) => result.push_failure("permissionProfile/list", error),
            }
        }

        if self.targets.config_requirements {
            match self
                .request::<ConfigRequirementsReadResponse>(
                    ClientRequest::ConfigRequirementsRead {
                        request_id: self.codex.next_request_id(),
                        params: None,
                    },
                )
                .await
            {
                Ok(response) => {
                    result.config_requirements = Some(response.requirements.is_some());
                }
                Err(error) => result.push_failure("configRequirements/read", error),
            }
        }

        if self.targets.account {
            match self
                .request::<GetAccountResponse>(ClientRequest::GetAccount {
                    request_id: self.codex.next_request_id(),
                    params: GetAccountParams {
                        refresh_token: false,
                    },
                })
                .await
            {
                Ok(response) => result.account = Some(response.account.is_some()),
                Err(error) => result.push_failure("account/read", error),
            }
        }

        if self.targets.mcp_status {
            match self
                .request::<ListMcpServerStatusResponse>(
                    ClientRequest::McpServerStatusList {
                        request_id: self.codex.next_request_id(),
                        params: ListMcpServerStatusParams {
                            cursor: None,
                            limit: None,
                            detail: Some(McpServerStatusDetail::ToolsAndAuthOnly),
                            thread_id: None,
                        },
                    },
                )
                .await
            {
                Ok(response) => result.mcp_servers = Some(response.data.len()),
                Err(error) => result.push_failure("mcpServerStatus/list", error),
            }
        }

        if self.targets.apps {
            match self
                .request::<AppsListResponse>(ClientRequest::AppsList {
                    request_id: self.codex.next_request_id(),
                    params: AppsListParams {
                        cursor: None,
                        limit: None,
                        thread_id: None,
                        force_refetch: false,
                    },
                })
                .await
            {
                Ok(response) => result.apps = Some(response.data.len()),
                Err(error) => result.push_failure("app/list", error),
            }
        }

        result
    }

    async fn request<T>(
        &self,
        request: ClientRequest,
    ) -> std::result::Result<T, WarmupStepError>
    where
        T: serde::de::DeserializeOwned,
    {
        match tokio::time::timeout(
            self.targets.step_timeout,
            self.codex.request_typed(request),
        )
        .await
        {
            Ok(result) => result.map_err(WarmupStepError::Request),
            Err(_) => Err(WarmupStepError::Timeout(self.targets.step_timeout)),
        }
    }
}

#[derive(Debug)]
enum WarmupStepError {
    Request(Error),
    Timeout(Duration),
}

impl std::fmt::Display for WarmupStepError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Request(error) => error.fmt(formatter),
            Self::Timeout(timeout) => {
                write!(formatter, "warmup request timed out after {timeout:?}")
            }
        }
    }
}

impl WarmupResult {
    fn push_failure(&mut self, step: &'static str, error: impl std::fmt::Display) {
        self.failures.push(WarmupFailure {
            step,
            error: error.to_string(),
        });
    }
}

impl IntoFuture for WarmupBuilder {
    type Output = Result<WarmupResult>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(async move { self.send().await })
    }
}
