use codex_sdk::prelude::*;
use tokio_stream::StreamExt;

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let _observability = Observability::builder()
            .with_env()?
            .service_name("codex-sdk-rs-example")
            .install()?;

        let codex = ctx
            .builder()
            .client_name("codex-sdk-rs-example")
            .start()
            .await?;

        let thread = codex
            .thread()
            .sandbox(SandboxMode::ReadOnly)
            .approval_policy(AskForApproval::Never)
            .start()
            .await?;
        let mut events = thread.event_stream()?;
        let turn = thread
            .turn("Describe the current working directory in one sentence.")
            .start()
            .await?;
        let turn_id = turn.turn_id().to_string();

        while let Some(event) = events.next().await {
            match event {
                AppServerEvent::ServerNotification(
                    ServerNotification::AgentMessageDelta(delta),
                ) if delta.turn_id == turn_id => print!("{}", delta.delta),
                AppServerEvent::ServerNotification(
                    ServerNotification::TurnCompleted(done),
                ) if done.turn.id == turn_id => break,
                _ => {}
            }
        }
        println!();
        codex.shutdown().await?;
        Ok(())
    })
}
