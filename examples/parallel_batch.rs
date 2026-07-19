//! Run independent Codex threads concurrently for batch analysis.

mod common;

use codex_sdk::prelude::*;
use common::{ServerRequestPolicy, collect_turn};
use tokio::task::JoinSet;

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let codex = ctx
            .builder()
            .client_name("codex-sdk-rs-parallel-batch")
            .client_version(env!("CARGO_PKG_VERSION"))
            .default_sandbox(SandboxMode::ReadOnly)
            .default_approval_policy(AskForApproval::Never)
            .ephemeral(true)
            .event_stream_capacity(512)
            .start()
            .await?;

        let jobs = [
            (
                "architecture",
                "Summarize the repository architecture in two sentences.",
            ),
            (
                "testing",
                "Identify the most important test strategy used in this repository.",
            ),
            (
                "operations",
                "Identify one operational or maintenance risk in this repository.",
            ),
        ];
        let mut tasks = JoinSet::new();

        for (name, prompt) in jobs {
            let codex = codex.clone();
            tasks.spawn(async move {
                let thread = codex
                    .thread()
                    .sandbox(SandboxMode::ReadOnly)
                    .approval_policy(AskForApproval::Never)
                    .ephemeral(true)
                    .start()
                    .await?;
                let mut events = thread.event_stream()?;
                let turn = thread
                    .turn(prompt)
                    .effort(ReasoningEffort::Low)
                    .start()
                    .await?;
                let response = collect_turn(
                    &mut events,
                    turn.turn_id(),
                    ServerRequestPolicy::Reject,
                )
                .await?;
                anyhow::Ok((name, thread.id().to_owned(), response))
            });
        }

        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok((name, thread_id, response))) => {
                    println!("[{name}] thread={thread_id}\n{response}\n");
                }
                Ok(Err(error)) => eprintln!("batch job failed: {error:#}"),
                Err(error) => eprintln!("batch task panicked or was cancelled: {error}"),
            }
        }

        codex.shutdown().await?;
        Ok(())
    })
}
