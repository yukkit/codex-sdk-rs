use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::{ServerNotification, ServerRequest};
use codex_protocol::ThreadId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EventTarget {
    Thread(ThreadId),
    InvalidThread,
    Runtime,
}

pub(crate) fn event_target(event: &AppServerEvent) -> EventTarget {
    match event {
        AppServerEvent::ServerRequest(request) => request_target(request),
        AppServerEvent::ServerNotification(notification) => {
            notification_target(notification)
        }
        AppServerEvent::Lagged { .. } | AppServerEvent::Disconnected { .. } => {
            EventTarget::Runtime
        }
    }
}

fn request_target(request: &ServerRequest) -> EventTarget {
    let thread_id = match request {
        ServerRequest::CommandExecutionRequestApproval { params, .. } => {
            Some(params.thread_id.as_str())
        }
        ServerRequest::FileChangeRequestApproval { params, .. } => {
            Some(params.thread_id.as_str())
        }
        ServerRequest::ToolRequestUserInput { params, .. } => {
            Some(params.thread_id.as_str())
        }
        ServerRequest::McpServerElicitationRequest { params, .. } => {
            Some(params.thread_id.as_str())
        }
        ServerRequest::PermissionsRequestApproval { params, .. } => {
            Some(params.thread_id.as_str())
        }
        ServerRequest::DynamicToolCall { params, .. } => Some(params.thread_id.as_str()),
        ServerRequest::CurrentTimeRead { params, .. } => Some(params.thread_id.as_str()),
        ServerRequest::ChatgptAuthTokensRefresh { .. }
        | ServerRequest::AttestationGenerate { .. }
        | ServerRequest::ApplyPatchApproval { .. }
        | ServerRequest::ExecCommandApproval { .. } => None,
    };

    target_from_thread_id(thread_id)
}

fn notification_target(notification: &ServerNotification) -> EventTarget {
    let thread_id = match notification {
        ServerNotification::Error(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadStarted(params) => Some(params.thread.id.as_str()),
        ServerNotification::ThreadStatusChanged(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ThreadArchived(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadDeleted(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadUnarchived(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadClosed(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadNameUpdated(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadGoalUpdated(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadGoalCleared(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadSettingsUpdated(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ThreadTokenUsageUpdated(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::TurnStarted(params) => Some(params.thread_id.as_str()),
        ServerNotification::HookStarted(params) => Some(params.thread_id.as_str()),
        ServerNotification::TurnCompleted(params) => Some(params.thread_id.as_str()),
        ServerNotification::HookCompleted(params) => Some(params.thread_id.as_str()),
        ServerNotification::TurnDiffUpdated(params) => Some(params.thread_id.as_str()),
        ServerNotification::TurnPlanUpdated(params) => Some(params.thread_id.as_str()),
        ServerNotification::ItemStarted(params) => Some(params.thread_id.as_str()),
        ServerNotification::ItemGuardianApprovalReviewStarted(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ItemGuardianApprovalReviewCompleted(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ItemCompleted(params) => Some(params.thread_id.as_str()),
        ServerNotification::RawResponseItemCompleted(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::AgentMessageDelta(params) => Some(params.thread_id.as_str()),
        ServerNotification::PlanDelta(params) => Some(params.thread_id.as_str()),
        ServerNotification::CommandExecutionOutputDelta(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::TerminalInteraction(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::FileChangeOutputDelta(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::FileChangePatchUpdated(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ServerRequestResolved(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::McpToolCallProgress(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ReasoningSummaryTextDelta(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ReasoningSummaryPartAdded(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ReasoningTextDelta(params) => Some(params.thread_id.as_str()),
        ServerNotification::ContextCompacted(params) => Some(params.thread_id.as_str()),
        ServerNotification::ModelRerouted(params) => Some(params.thread_id.as_str()),
        ServerNotification::ModelVerification(params) => Some(params.thread_id.as_str()),
        ServerNotification::TurnModerationMetadata(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ModelSafetyBufferingUpdated(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::Warning(params) => params.thread_id.as_deref(),
        ServerNotification::GuardianWarning(params) => Some(params.thread_id.as_str()),
        ServerNotification::McpServerStatusUpdated(params) => params.thread_id.as_deref(),
        ServerNotification::ThreadRealtimeStarted(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ThreadRealtimeItemAdded(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ThreadRealtimeTranscriptDelta(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ThreadRealtimeTranscriptDone(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ThreadRealtimeOutputAudioDelta(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ThreadRealtimeSdp(params) => Some(params.thread_id.as_str()),
        ServerNotification::ThreadRealtimeError(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::ThreadRealtimeClosed(params) => {
            Some(params.thread_id.as_str())
        }
        ServerNotification::SkillsChanged(_)
        | ServerNotification::McpServerOauthLoginCompleted(_)
        | ServerNotification::AccountUpdated(_)
        | ServerNotification::AccountRateLimitsUpdated(_)
        | ServerNotification::AppListUpdated(_)
        | ServerNotification::RemoteControlStatusChanged(_)
        | ServerNotification::ExternalAgentConfigImportProgress(_)
        | ServerNotification::ExternalAgentConfigImportCompleted(_)
        | ServerNotification::DeprecationNotice(_)
        | ServerNotification::ConfigWarning(_)
        | ServerNotification::FuzzyFileSearchSessionUpdated(_)
        | ServerNotification::FuzzyFileSearchSessionCompleted(_)
        | ServerNotification::CommandExecOutputDelta(_)
        | ServerNotification::ProcessOutputDelta(_)
        | ServerNotification::ProcessExited(_)
        | ServerNotification::FsChanged(_)
        | ServerNotification::WindowsWorldWritableWarning(_)
        | ServerNotification::WindowsSandboxSetupCompleted(_)
        | ServerNotification::AccountLoginCompleted(_) => None,
    };

    target_from_thread_id(thread_id)
}

fn target_from_thread_id(thread_id: Option<&str>) -> EventTarget {
    match thread_id {
        Some(thread_id) => ThreadId::from_string(thread_id)
            .map(EventTarget::Thread)
            .unwrap_or(EventTarget::InvalidThread),
        None => EventTarget::Runtime,
    }
}
