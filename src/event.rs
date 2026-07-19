use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::{ServerNotification, ServerRequest};

pub(crate) fn event_matches(event: &AppServerEvent, thread_id: &str) -> bool {
    match event {
        AppServerEvent::ServerRequest(request) => request_matches(request, thread_id),
        AppServerEvent::ServerNotification(notification) => {
            notification_matches(notification, thread_id)
        }
        AppServerEvent::Lagged { .. } | AppServerEvent::Disconnected { .. } => true,
    }
}

fn request_matches(request: &ServerRequest, thread_id: &str) -> bool {
    match request {
        ServerRequest::CommandExecutionRequestApproval { params, .. } => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerRequest::FileChangeRequestApproval { params, .. } => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerRequest::ToolRequestUserInput { params, .. } => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerRequest::McpServerElicitationRequest { params, .. } => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerRequest::PermissionsRequestApproval { params, .. } => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerRequest::DynamicToolCall { params, .. } => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerRequest::ChatgptAuthTokensRefresh { .. }
        | ServerRequest::AttestationGenerate { .. } => thread_matches(None, thread_id),
        ServerRequest::CurrentTimeRead { params, .. } => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerRequest::ApplyPatchApproval { params, .. } => {
            let conversation_id = params.conversation_id.to_string();
            thread_matches(Some(&conversation_id), thread_id)
        }
        ServerRequest::ExecCommandApproval { params, .. } => {
            let conversation_id = params.conversation_id.to_string();
            thread_matches(Some(&conversation_id), thread_id)
        }
    }
}

fn notification_matches(notification: &ServerNotification, thread_id: &str) -> bool {
    match notification {
        ServerNotification::Error(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadStarted(params) => {
            thread_matches(Some(&params.thread.id), thread_id)
        }
        ServerNotification::ThreadStatusChanged(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadArchived(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadDeleted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadUnarchived(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadClosed(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::SkillsChanged(_) => thread_matches(None, thread_id),
        ServerNotification::ThreadNameUpdated(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadGoalUpdated(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadGoalCleared(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadSettingsUpdated(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadTokenUsageUpdated(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::TurnStarted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::HookStarted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::TurnCompleted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::HookCompleted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::TurnDiffUpdated(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::TurnPlanUpdated(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ItemStarted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ItemGuardianApprovalReviewStarted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ItemGuardianApprovalReviewCompleted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ItemCompleted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::RawResponseItemCompleted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::AgentMessageDelta(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::PlanDelta(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::CommandExecOutputDelta(_)
        | ServerNotification::ProcessOutputDelta(_)
        | ServerNotification::ProcessExited(_) => thread_matches(None, thread_id),
        ServerNotification::CommandExecutionOutputDelta(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::TerminalInteraction(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::FileChangeOutputDelta(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::FileChangePatchUpdated(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ServerRequestResolved(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::McpToolCallProgress(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::McpServerOauthLoginCompleted(params) => {
            thread_matches(params.thread_id.as_deref(), thread_id)
        }
        ServerNotification::McpServerStatusUpdated(params) => {
            thread_matches(params.thread_id.as_deref(), thread_id)
        }
        ServerNotification::AccountUpdated(_)
        | ServerNotification::AccountRateLimitsUpdated(_)
        | ServerNotification::AppListUpdated(_)
        | ServerNotification::RemoteControlStatusChanged(_)
        | ServerNotification::ExternalAgentConfigImportProgress(_)
        | ServerNotification::ExternalAgentConfigImportCompleted(_)
        | ServerNotification::FsChanged(_) => thread_matches(None, thread_id),
        ServerNotification::ReasoningSummaryTextDelta(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ReasoningSummaryPartAdded(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ReasoningTextDelta(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ContextCompacted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ModelRerouted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ModelVerification(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::TurnModerationMetadata(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ModelSafetyBufferingUpdated(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::Warning(params) => {
            thread_matches(params.thread_id.as_deref(), thread_id)
        }
        ServerNotification::GuardianWarning(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::DeprecationNotice(_)
        | ServerNotification::ConfigWarning(_)
        | ServerNotification::FuzzyFileSearchSessionUpdated(_)
        | ServerNotification::FuzzyFileSearchSessionCompleted(_) => {
            thread_matches(None, thread_id)
        }
        ServerNotification::ThreadRealtimeStarted(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadRealtimeItemAdded(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadRealtimeTranscriptDelta(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadRealtimeTranscriptDone(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadRealtimeOutputAudioDelta(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadRealtimeSdp(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadRealtimeError(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::ThreadRealtimeClosed(params) => {
            thread_matches(Some(&params.thread_id), thread_id)
        }
        ServerNotification::WindowsWorldWritableWarning(_)
        | ServerNotification::WindowsSandboxSetupCompleted(_)
        | ServerNotification::AccountLoginCompleted(_) => thread_matches(None, thread_id),
    }
}

fn thread_matches(event_thread_id: Option<&str>, thread_id: &str) -> bool {
    event_thread_id.is_none_or(|event_thread_id| event_thread_id == thread_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_scoped_events_only_match_their_thread() {
        assert!(thread_matches(Some("thread-1"), "thread-1"));
        assert!(!thread_matches(Some("thread-2"), "thread-1"));
    }

    #[test]
    fn global_events_match_every_thread() {
        assert!(thread_matches(None, "thread-1"));
        assert!(thread_matches(None, "thread-2"));
    }
}
