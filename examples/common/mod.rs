//! Shared event-loop helpers for the scenario examples.

#![allow(dead_code)]

use anyhow::bail;
use codex_sdk::prelude::*;
use tokio_stream::StreamExt;

/// How a demo should handle approval-shaped server requests.
///
/// Rejecting is the safe default. Even in `ApproveKnown` mode, requests whose
/// typed response is not implemented here are rejected.
#[derive(Debug, Clone, Copy, Default)]
pub enum ServerRequestPolicy {
    ApproveKnown,
    #[default]
    Reject,
}

impl ServerRequestPolicy {
    pub fn from_env(name: &str) -> Self {
        if env_flag(name) {
            Self::ApproveKnown
        } else {
            Self::Reject
        }
    }
}

/// Consume one turn from a thread's long-lived event stream.
///
/// Callers keep the same stream for later turns. This helper never obtains a
/// second stream and never treats `TurnCompleted` as the end of the thread.
pub async fn consume_turn(
    stream: &mut ThreadEventStream,
    turn_id: &str,
    request_policy: ServerRequestPolicy,
) -> anyhow::Result<String> {
    consume_turn_inner(stream, turn_id, request_policy, true).await
}

/// Collect one turn without printing interleaved deltas.
///
/// This is useful for concurrent batch examples; completion and request
/// handling semantics are identical to [`consume_turn`].
pub async fn collect_turn(
    stream: &mut ThreadEventStream,
    turn_id: &str,
    request_policy: ServerRequestPolicy,
) -> anyhow::Result<String> {
    consume_turn_inner(stream, turn_id, request_policy, false).await
}

/// Wait until the server reports that a turn is active before steering or
/// interrupting it.
///
/// A successful `turn/start` response and active-turn registration are not the
/// same observable boundary, especially for remote transports.
pub async fn wait_for_turn_started(
    stream: &mut ThreadEventStream,
    turn_id: &str,
    request_policy: ServerRequestPolicy,
) -> anyhow::Result<()> {
    while let Some(event) = stream.next().await {
        match event {
            AppServerEvent::ServerNotification(ServerNotification::TurnStarted(
                started,
            )) if started.turn.id == turn_id => {
                return Ok(());
            }
            AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(
                completed,
            )) if completed.turn.id == turn_id => {
                bail!("turn {turn_id} completed before it could be controlled");
            }
            AppServerEvent::ServerNotification(ServerNotification::Error(error)) => {
                eprintln!("Codex error event: {error:?}");
            }
            AppServerEvent::ServerRequest(request) => {
                eprintln!("server request {}: {request:?}", request.id());
                resolve_server_request(stream, request, request_policy).await?;
            }
            AppServerEvent::Lagged { skipped } => {
                eprintln!("event stream lagged; skipped {skipped} best-effort events");
            }
            AppServerEvent::Disconnected { message } => {
                bail!("Codex disconnected while turn {turn_id} was starting: {message}");
            }
            _ => {}
        }
    }

    bail!("thread event stream ended before turn {turn_id} started")
}

async fn consume_turn_inner(
    stream: &mut ThreadEventStream,
    turn_id: &str,
    request_policy: ServerRequestPolicy,
    print_deltas: bool,
) -> anyhow::Result<String> {
    let mut response = String::new();

    while let Some(event) = stream.next().await {
        match event {
            AppServerEvent::ServerNotification(
                ServerNotification::AgentMessageDelta(delta),
            ) if delta.turn_id == turn_id => {
                if print_deltas {
                    print!("{}", delta.delta);
                }
                response.push_str(&delta.delta);
            }
            AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(
                done,
            )) if done.turn.id == turn_id => {
                if print_deltas {
                    println!();
                }
                let status = serde_json::to_value(&done.turn.status)?;
                eprintln!(
                    "turn {turn_id} completed with status {:?}",
                    done.turn.status
                );
                if status == "completed" {
                    return Ok(response);
                }
                let error = done
                    .turn
                    .error
                    .as_ref()
                    .map(|error| error.message.as_str())
                    .unwrap_or("no turn error was provided");
                bail!(
                    "turn {turn_id} ended with status {:?}: {error}",
                    done.turn.status
                );
            }
            AppServerEvent::ServerNotification(ServerNotification::Error(error)) => {
                eprintln!("Codex error event: {error:?}");
            }
            AppServerEvent::ServerRequest(request) => {
                eprintln!("server request {}: {request:?}", request.id());
                resolve_server_request(stream, request, request_policy).await?;
            }
            AppServerEvent::Lagged { skipped } => {
                eprintln!("event stream lagged; skipped {skipped} best-effort events");
            }
            AppServerEvent::Disconnected { message } => {
                bail!("Codex disconnected while turn {turn_id} was active: {message}");
            }
            _ => {}
        }
    }

    bail!("thread event stream ended before turn {turn_id} completed")
}

pub async fn wait_for_archive(
    stream: &mut ThreadEventStream,
    thread_id: &str,
) -> anyhow::Result<()> {
    while let Some(event) = stream.next().await {
        match event {
            AppServerEvent::ServerNotification(ServerNotification::ThreadArchived(
                archived,
            )) if archived.thread_id == thread_id => {
                return Ok(());
            }
            AppServerEvent::Lagged { skipped } => {
                eprintln!("archive stream lagged; skipped {skipped} events");
            }
            AppServerEvent::Disconnected { message } => {
                bail!("Codex disconnected while archiving {thread_id}: {message}");
            }
            _ => {}
        }
    }

    bail!("thread stream ended before archive confirmation for {thread_id}")
}

pub async fn wait_for_compaction(
    stream: &mut ThreadEventStream,
    thread_id: &str,
) -> anyhow::Result<()> {
    while let Some(event) = stream.next().await {
        match event {
            AppServerEvent::ServerNotification(ServerNotification::ItemCompleted(
                completed,
            )) if completed.thread_id == thread_id
                && serde_json::to_value(&completed.item)?["type"]
                    == "contextCompaction" =>
            {
                return Ok(());
            }
            AppServerEvent::ServerNotification(ServerNotification::Error(error)) => {
                eprintln!("Codex error event: {error:?}");
            }
            AppServerEvent::Lagged { skipped } => {
                eprintln!("compaction stream lagged; skipped {skipped} events");
            }
            AppServerEvent::Disconnected { message } => {
                bail!("Codex disconnected while compacting {thread_id}: {message}");
            }
            _ => {}
        }
    }

    bail!("thread stream ended before compaction confirmation for {thread_id}")
}

pub fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

async fn resolve_server_request(
    stream: &ThreadEventStream,
    request: ServerRequest,
    policy: ServerRequestPolicy,
) -> anyhow::Result<()> {
    let request_id = request.id().clone();
    match (policy, request) {
        (
            ServerRequestPolicy::ApproveKnown,
            ServerRequest::CommandExecutionRequestApproval { .. },
        ) => {
            stream
                .resolve_server_request(
                    request_id,
                    serde_json::json!({ "decision": "accept" }),
                )
                .await?;
        }
        (
            ServerRequestPolicy::ApproveKnown,
            ServerRequest::FileChangeRequestApproval { .. },
        ) => {
            stream
                .resolve_server_request(
                    request_id,
                    serde_json::json!({ "decision": "accept" }),
                )
                .await?;
        }
        (
            ServerRequestPolicy::ApproveKnown,
            ServerRequest::PermissionsRequestApproval { params, .. },
        ) => {
            stream
                .resolve_server_request(
                    request_id,
                    serde_json::json!({
                        "permissions": params.permissions,
                        "scope": "turn"
                    }),
                )
                .await?;
        }
        (ServerRequestPolicy::ApproveKnown, _) => {
            stream
                .reject_server_request(
                    request_id,
                    "example cannot construct this request's typed response",
                )
                .await?;
        }
        (ServerRequestPolicy::Reject, _) => {
            stream
                .reject_server_request(request_id, "rejected by safe example default")
                .await?;
        }
    }

    Ok(())
}
