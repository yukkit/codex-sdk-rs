//! Connect to a remote app-server over WebSocket or a Unix socket.

mod common;

use anyhow::bail;
use codex_sdk::prelude::*;
use common::{ServerRequestPolicy, consume_turn};
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut builder = if let Ok(url) = std::env::var("CODEX_WS_URL") {
        Codex::remote_websocket(url)
    } else if let Ok(path) = std::env::var("CODEX_UNIX_SOCKET") {
        Codex::remote_unix_socket(path)
    } else {
        bail!("set CODEX_WS_URL or CODEX_UNIX_SOCKET")
    };

    builder = builder
        .client_name("codex-sdk-rs-remote-client")
        .client_version(env!("CARGO_PKG_VERSION"))
        .channel_capacity(512)
        .app_server_channel_capacity(1024)
        .event_stream_capacity(512)
        .default_thread_params(ThreadStartParams {
            cwd: std::env::var("CODEX_REMOTE_CWD").ok(),
            ephemeral: Some(true),
            ..Default::default()
        });
    if let Ok(token) = std::env::var("CODEX_APP_SERVER_TOKEN") {
        builder = builder.auth_token(token);
    }
    let codex = builder.start().await?;
    let requested_model = std::env::var("CODEX_MODEL").ok();

    let mut runtime_events = codex.event_stream()?;
    let runtime_event_task = tokio::spawn(async move {
        while let Some(event) = runtime_events.next().await {
            eprintln!("remote runtime event: {event:?}");
            if matches!(event, AppServerEvent::Disconnected { .. }) {
                break;
            }
        }
    });

    let operations = async {
        let models = codex.models().await?;
        eprintln!(
            "remote models: {}",
            models
                .data
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );

        let mut thread_builder = codex
            .thread()
            .sandbox(SandboxMode::ReadOnly)
            .approval_policy(AskForApproval::Never)
            .ephemeral(true);
        if let Some(model) = requested_model {
            thread_builder = thread_builder.model(model);
        }
        let thread = thread_builder.start().await?;
        let mut events = thread.event_stream()?;
        let turn = thread
            .turn(
                "Reply with one short sentence describing the remote working directory.",
            )
            .start()
            .await?;
        consume_turn(&mut events, turn.turn_id(), ServerRequestPolicy::Reject).await?;
        anyhow::Ok(())
    }
    .await;
    let shutdown = codex.shutdown().await;
    let runtime_events = runtime_event_task.await;

    operations?;
    shutdown?;
    runtime_events?;
    Ok(())
}
