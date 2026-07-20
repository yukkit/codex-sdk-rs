use std::collections::HashMap;
use std::fmt;

use codex_app_server_protocol::{
    AskForApproval, SandboxMode, ThreadStartParams, TurnEnvironmentParams,
    TurnStartParams,
};
use codex_core::config::Config;
use codex_features::Feature;
use codex_protocol::protocol::SandboxPolicy as CoreSandboxPolicy;

type ConfigMap = HashMap<String, serde_json::Value>;

/// Select how Codex resolves the execution environment for a thread or turn.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum EnvironmentAccess {
    /// Omit the protocol override.
    ///
    /// A new thread selects Codex's default environment. A turn inherits the
    /// thread's sticky environments.
    #[default]
    Inherit,
    /// Send an explicit empty environment list and disable environment access.
    Disabled,
    /// Select explicit environments. The first entry is used for the current turn.
    /// An empty list has the same protocol meaning as [`Self::Disabled`].
    Selected(Vec<TurnEnvironmentParams>),
}

impl EnvironmentAccess {
    pub(crate) fn apply_to_thread(self, params: &mut ThreadStartParams) {
        params.environments = self.into_protocol_value();
    }

    pub(crate) fn apply_to_turn(self, params: &mut TurnStartParams) {
        params.environments = self.into_protocol_value();
    }

    fn into_protocol_value(self) -> Option<Vec<TurnEnvironmentParams>> {
        match self {
            Self::Inherit => None,
            Self::Disabled => Some(Vec::new()),
            Self::Selected(environments) => Some(environments),
        }
    }
}

#[derive(Clone)]
enum ThreadDefaultsOperation {
    Replace(Box<ThreadStartParams>),
    MergeConfig(ConfigMap),
    SetEnvironment(EnvironmentAccess),
}

impl fmt::Debug for ThreadDefaultsOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Replace(_) => formatter.write_str("Replace(<redacted>)"),
            Self::MergeConfig(config) => formatter
                .debug_struct("MergeConfig")
                .field("count", &config.len())
                .finish(),
            Self::SetEnvironment(EnvironmentAccess::Inherit) => {
                formatter.write_str("SetEnvironment(Inherit)")
            }
            Self::SetEnvironment(EnvironmentAccess::Disabled) => {
                formatter.write_str("SetEnvironment(Disabled)")
            }
            Self::SetEnvironment(EnvironmentAccess::Selected(environments)) => formatter
                .debug_struct("SetEnvironment(Selected)")
                .field("count", &environments.len())
                .finish(),
        }
    }
}

/// Ordered changes applied to the native defaults after the startup config is resolved.
#[derive(Debug, Clone, Default)]
pub(super) struct ThreadDefaultsOverrides {
    operations: Vec<ThreadDefaultsOperation>,
}

impl ThreadDefaultsOverrides {
    pub(super) fn replace(&mut self, params: ThreadStartParams) {
        self.operations
            .push(ThreadDefaultsOperation::Replace(Box::new(params)));
    }

    pub(super) fn merge_config(&mut self, overrides: ConfigMap) {
        if !overrides.is_empty() {
            self.operations
                .push(ThreadDefaultsOperation::MergeConfig(overrides));
        }
    }

    pub(super) fn minimize_tools(&mut self) {
        self.merge_config(minimal_tool_config_overrides());
    }

    pub(super) fn minimize_prompt_context(&mut self) {
        self.merge_config(minimal_prompt_context_config_overrides());
    }

    pub(super) fn configure_pure_chat(&mut self) {
        let mut config = minimal_prompt_context_config_overrides();
        config.extend(minimal_tool_config_overrides());
        config.insert(
            PROJECT_DOC_MAX_BYTES_CONFIG_KEY.to_string(),
            serde_json::Value::from(0),
        );
        self.merge_config(config);
        self.set_environment_access(EnvironmentAccess::Disabled);
    }

    pub(super) fn set_prompt_context(&mut self, key: &'static str, enabled: bool) {
        self.merge_config(HashMap::from([(
            key.to_string(),
            serde_json::Value::Bool(enabled),
        )]));
    }

    pub(super) fn set_environment_access(&mut self, access: EnvironmentAccess) {
        self.operations
            .push(ThreadDefaultsOperation::SetEnvironment(access));
    }

    pub(super) fn apply(self, mut params: ThreadStartParams) -> ThreadStartParams {
        for operation in self.operations {
            match operation {
                ThreadDefaultsOperation::Replace(replacement) => params = *replacement,
                ThreadDefaultsOperation::MergeConfig(patch) => {
                    merge_thread_config_overrides(&mut params, patch);
                }
                ThreadDefaultsOperation::SetEnvironment(access) => {
                    access.apply_to_thread(&mut params);
                }
            }
        }
        params
    }
}

const MINIMAL_TOOL_DISABLED_FEATURES: &[Feature] = &[
    Feature::Apps,
    Feature::Plugins,
    Feature::ToolSuggest,
    Feature::ShellTool,
    Feature::CodeMode,
    Feature::CodeModeOnly,
    Feature::Collab,
    Feature::MultiAgentV2,
    Feature::SpawnCsv,
    Feature::ImageGeneration,
    Feature::MemoryTool,
    Feature::Goals,
    Feature::DeferredExecutor,
    Feature::RequestPermissionsTool,
    Feature::TokenBudget,
    Feature::CurrentTimeReminder,
    Feature::StandaloneWebSearch,
];

const MINIMAL_TOOL_DISABLED_BOOLEAN_CONFIG_KEYS: &[&str] = &[
    "orchestrator.skills.enabled",
    "orchestrator.mcp.enabled",
    "tools.experimental_request_user_input.enabled",
];

pub(super) const INCLUDE_PERMISSIONS_INSTRUCTIONS_CONFIG_KEY: &str =
    "include_permissions_instructions";
pub(super) const INCLUDE_APPS_INSTRUCTIONS_CONFIG_KEY: &str = "include_apps_instructions";
pub(super) const INCLUDE_COLLABORATION_MODE_INSTRUCTIONS_CONFIG_KEY: &str =
    "include_collaboration_mode_instructions";
pub(super) const INCLUDE_ENVIRONMENT_CONTEXT_CONFIG_KEY: &str =
    "include_environment_context";
pub(super) const INCLUDE_SKILL_INSTRUCTIONS_CONFIG_KEY: &str =
    "skills.include_instructions";

const PROJECT_DOC_MAX_BYTES_CONFIG_KEY: &str = "project_doc_max_bytes";

fn minimal_tool_config_overrides() -> ConfigMap {
    let feature_overrides = MINIMAL_TOOL_DISABLED_FEATURES.iter().map(|feature| {
        (
            format!("features.{}", feature.key()),
            serde_json::Value::Bool(false),
        )
    });
    let config_overrides = MINIMAL_TOOL_DISABLED_BOOLEAN_CONFIG_KEYS
        .iter()
        .map(|key| ((*key).to_string(), serde_json::Value::Bool(false)));
    let mut overrides = feature_overrides
        .chain(config_overrides)
        .collect::<HashMap<_, _>>();
    overrides.insert(
        "web_search".to_string(),
        serde_json::Value::String("disabled".to_string()),
    );
    overrides
}

fn minimal_prompt_context_config_overrides() -> ConfigMap {
    HashMap::from([
        (
            INCLUDE_PERMISSIONS_INSTRUCTIONS_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(false),
        ),
        (
            INCLUDE_APPS_INSTRUCTIONS_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(false),
        ),
        (
            INCLUDE_COLLABORATION_MODE_INSTRUCTIONS_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(false),
        ),
        (
            INCLUDE_ENVIRONMENT_CONTEXT_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(false),
        ),
        (
            INCLUDE_SKILL_INSTRUCTIONS_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(false),
        ),
    ])
}

pub(super) fn merge_thread_config_overrides(
    params: &mut ThreadStartParams,
    overrides: ConfigMap,
) {
    if !overrides.is_empty() {
        params.config.get_or_insert_default().extend(overrides);
    }
}

/// Take a documented snapshot of the resolved values that have native
/// `thread/start` representations.
///
/// A resolved [`Config`] no longer records the provenance of every field, so
/// this conversion is intentionally explicit rather than pretending to be a
/// lossless `From<Config>` implementation.
pub(super) fn thread_defaults_snapshot(config: &Config) -> ThreadStartParams {
    let (sandbox, permissions) = thread_permissions_from_config(config);
    let runtime_workspace_roots =
        (!config.permissions.user_visible_workspace_roots().is_empty())
            .then(|| config.permissions.user_visible_workspace_roots().to_vec());

    ThreadStartParams {
        cwd: Some(config.cwd.display().to_string()),
        model: config.model.clone(),
        model_provider: Some(config.model_provider_id.clone()),
        // This field is deliberately always present. `Some(None)` means
        // "explicitly clear" and must not collapse back to an omitted value.
        service_tier: Some(config.service_tier.clone()),
        runtime_workspace_roots,
        approval_policy: Some(AskForApproval::from(
            config.permissions.approval_policy.value(),
        )),
        approvals_reviewer: Some(config.approvals_reviewer.into()),
        sandbox,
        permissions,
        personality: config.personality,
        config: Some(thread_config_snapshot(config)),
        base_instructions: config.base_instructions.clone(),
        developer_instructions: config.developer_instructions.clone(),
        ephemeral: Some(config.ephemeral),
        thread_source: Some(codex_app_server_protocol::ThreadSource::User),
        ..Default::default()
    }
}

fn thread_config_snapshot(config: &Config) -> ConfigMap {
    let mut overrides = HashMap::from([
        (
            INCLUDE_PERMISSIONS_INSTRUCTIONS_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(config.include_permissions_instructions),
        ),
        (
            INCLUDE_APPS_INSTRUCTIONS_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(config.include_apps_instructions),
        ),
        (
            INCLUDE_COLLABORATION_MODE_INSTRUCTIONS_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(config.include_collaboration_mode_instructions),
        ),
        (
            INCLUDE_ENVIRONMENT_CONTEXT_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(config.include_environment_context),
        ),
        (
            INCLUDE_SKILL_INSTRUCTIONS_CONFIG_KEY.to_string(),
            serde_json::Value::Bool(config.include_skill_instructions),
        ),
    ]);

    if let Some(effort) = config.model_reasoning_effort.as_ref() {
        overrides.insert(
            "model_reasoning_effort".to_string(),
            serde_json::Value::String(effort.as_str().to_string()),
        );
    }
    if let Some(summary) = config.model_reasoning_summary {
        overrides.insert(
            "model_reasoning_summary".to_string(),
            serde_json::Value::String(summary.to_string()),
        );
    }

    overrides
}

fn thread_permissions_from_config(
    config: &Config,
) -> (Option<SandboxMode>, Option<String>) {
    if config.explicit_permission_profile_mode
        && let Some(profile) = config.permissions.active_permission_profile()
    {
        return (None, Some(profile.id));
    }

    let sandbox = match config.legacy_sandbox_policy() {
        CoreSandboxPolicy::ReadOnly { .. } => Some(SandboxMode::ReadOnly),
        CoreSandboxPolicy::WorkspaceWrite { .. } => Some(SandboxMode::WorkspaceWrite),
        CoreSandboxPolicy::DangerFullAccess => Some(SandboxMode::DangerFullAccess),
        CoreSandboxPolicy::ExternalSandbox { .. } => None,
    };
    (sandbox, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_access_preserves_protocol_tristate() {
        let mut params = ThreadStartParams::default();
        EnvironmentAccess::Disabled.apply_to_thread(&mut params);
        assert_eq!(params.environments, Some(Vec::new()));

        EnvironmentAccess::Inherit.apply_to_thread(&mut params);
        assert_eq!(params.environments, None);
    }

    #[test]
    fn thread_default_operations_follow_builder_order() {
        let mut defaults = ThreadDefaultsOverrides::default();
        defaults.set_environment_access(EnvironmentAccess::Disabled);
        defaults.replace(ThreadStartParams::default());
        assert_eq!(
            defaults.apply(ThreadStartParams::default()).environments,
            None
        );

        let mut defaults = ThreadDefaultsOverrides::default();
        defaults.replace(ThreadStartParams::default());
        defaults.set_environment_access(EnvironmentAccess::Disabled);
        assert_eq!(
            defaults.apply(ThreadStartParams::default()).environments,
            Some(Vec::new())
        );
    }

    #[test]
    fn debug_output_redacts_config_and_native_params() {
        let mut defaults = ThreadDefaultsOverrides::default();
        defaults.merge_config(HashMap::from([(
            "example.token".to_string(),
            serde_json::Value::String("thread-secret".to_string()),
        )]));
        defaults.replace(ThreadStartParams {
            developer_instructions: Some("instruction-secret".to_string()),
            ..Default::default()
        });

        let debug = format!("{defaults:?}");
        assert!(debug.contains("MergeConfig"));
        assert!(debug.contains("count: 1"));
        assert!(!debug.contains("thread-secret"));
        assert!(!debug.contains("instruction-secret"));
    }
}
