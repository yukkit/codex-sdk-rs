use codex_app_server_client::AppServerEvent;
use codex_app_server_protocol::{ServerNotification, ServerRequest};

pub(crate) fn event_matches(
    event: &AppServerEvent,
    thread_id: &str,
    turn_id: Option<&str>,
) -> bool {
    match event {
        AppServerEvent::ServerRequest(request) => {
            request_matches(request, thread_id, turn_id)
        }
        AppServerEvent::ServerNotification(notification) => {
            notification_matches(notification, thread_id, turn_id)
        }
        AppServerEvent::Lagged { .. } | AppServerEvent::Disconnected { .. } => true,
    }
}

fn request_matches(
    request: &ServerRequest,
    thread_id: &str,
    turn_id: Option<&str>,
) -> bool {
    match request {
        ServerRequest::CommandExecutionRequestApproval { params, .. } => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerRequest::FileChangeRequestApproval { params, .. } => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerRequest::ToolRequestUserInput { params, .. } => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerRequest::McpServerElicitationRequest { params, .. } => ids_match(
            Some(&params.thread_id),
            params.turn_id.as_deref(),
            thread_id,
            turn_id,
        ),
        ServerRequest::PermissionsRequestApproval { params, .. } => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerRequest::DynamicToolCall { params, .. } => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerRequest::ChatgptAuthTokensRefresh { .. }
        | ServerRequest::AttestationGenerate { .. } => {
            ids_match(None, None, thread_id, turn_id)
        }
        ServerRequest::CurrentTimeRead { params, .. } => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerRequest::ApplyPatchApproval { params, .. } => {
            let conversation_id = params.conversation_id.to_string();
            ids_match(Some(&conversation_id), None, thread_id, turn_id)
        }
        ServerRequest::ExecCommandApproval { params, .. } => {
            let conversation_id = params.conversation_id.to_string();
            ids_match(Some(&conversation_id), None, thread_id, turn_id)
        }
    }
}

fn ids_match(
    event_thread_id: Option<&str>,
    event_turn_id: Option<&str>,
    thread_id: &str,
    turn_id: Option<&str>,
) -> bool {
    event_thread_id.is_none_or(|event_thread| event_thread == thread_id)
        && turn_id.is_none_or(|turn_id| {
            event_turn_id.is_none_or(|event_turn| event_turn == turn_id)
        })
}

fn notification_matches(
    notification: &ServerNotification,
    thread_id: &str,
    turn_id: Option<&str>,
) -> bool {
    match notification {
        ServerNotification::Error(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ThreadStarted(params) => {
            ids_match(Some(&params.thread.id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadStatusChanged(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadArchived(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadDeleted(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadUnarchived(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadClosed(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::SkillsChanged(_) => ids_match(None, None, thread_id, turn_id),
        ServerNotification::ThreadNameUpdated(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadGoalUpdated(params) => ids_match(
            Some(&params.thread_id),
            params.turn_id.as_deref(),
            thread_id,
            turn_id,
        ),
        ServerNotification::ThreadGoalCleared(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadSettingsUpdated(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadTokenUsageUpdated(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::TurnStarted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn.id),
            thread_id,
            turn_id,
        ),
        ServerNotification::HookStarted(params) => ids_match(
            Some(&params.thread_id),
            params.turn_id.as_deref(),
            thread_id,
            turn_id,
        ),
        ServerNotification::TurnCompleted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn.id),
            thread_id,
            turn_id,
        ),
        ServerNotification::HookCompleted(params) => ids_match(
            Some(&params.thread_id),
            params.turn_id.as_deref(),
            thread_id,
            turn_id,
        ),
        ServerNotification::TurnDiffUpdated(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::TurnPlanUpdated(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ItemStarted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ItemGuardianApprovalReviewStarted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ItemGuardianApprovalReviewCompleted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ItemCompleted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::RawResponseItemCompleted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::AgentMessageDelta(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::PlanDelta(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::CommandExecOutputDelta(_)
        | ServerNotification::ProcessOutputDelta(_)
        | ServerNotification::ProcessExited(_) => {
            ids_match(None, None, thread_id, turn_id)
        }
        ServerNotification::CommandExecutionOutputDelta(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::TerminalInteraction(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::FileChangeOutputDelta(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::FileChangePatchUpdated(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ServerRequestResolved(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::McpToolCallProgress(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::McpServerOauthLoginCompleted(params) => {
            ids_match(params.thread_id.as_deref(), None, thread_id, turn_id)
        }
        ServerNotification::McpServerStatusUpdated(params) => {
            ids_match(params.thread_id.as_deref(), None, thread_id, turn_id)
        }
        ServerNotification::AccountUpdated(_)
        | ServerNotification::AccountRateLimitsUpdated(_)
        | ServerNotification::AppListUpdated(_)
        | ServerNotification::RemoteControlStatusChanged(_)
        | ServerNotification::ExternalAgentConfigImportProgress(_)
        | ServerNotification::ExternalAgentConfigImportCompleted(_)
        | ServerNotification::FsChanged(_) => ids_match(None, None, thread_id, turn_id),
        ServerNotification::ReasoningSummaryTextDelta(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ReasoningSummaryPartAdded(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ReasoningTextDelta(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ContextCompacted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ModelRerouted(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ModelVerification(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::TurnModerationMetadata(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::ModelSafetyBufferingUpdated(params) => ids_match(
            Some(&params.thread_id),
            Some(&params.turn_id),
            thread_id,
            turn_id,
        ),
        ServerNotification::Warning(params) => {
            ids_match(params.thread_id.as_deref(), None, thread_id, turn_id)
        }
        ServerNotification::GuardianWarning(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::DeprecationNotice(_)
        | ServerNotification::ConfigWarning(_)
        | ServerNotification::FuzzyFileSearchSessionUpdated(_)
        | ServerNotification::FuzzyFileSearchSessionCompleted(_) => {
            ids_match(None, None, thread_id, turn_id)
        }
        ServerNotification::ThreadRealtimeStarted(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadRealtimeItemAdded(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadRealtimeTranscriptDelta(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadRealtimeTranscriptDone(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadRealtimeOutputAudioDelta(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadRealtimeSdp(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadRealtimeError(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::ThreadRealtimeClosed(params) => {
            ids_match(Some(&params.thread_id), None, thread_id, turn_id)
        }
        ServerNotification::WindowsWorldWritableWarning(_)
        | ServerNotification::WindowsSandboxSetupCompleted(_)
        | ServerNotification::AccountLoginCompleted(_) => {
            ids_match(None, None, thread_id, turn_id)
        }
    }
}
