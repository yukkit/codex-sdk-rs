//! Multi-turn repair workflow with safe-by-default approval handling.

mod common;

use codex_sdk::prelude::*;
use common::{ServerRequestPolicy, consume_turn, env_flag, wait_for_turn_started};

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let cwd = std::env::current_dir()?;
        let allow_writes = env_flag("CODEX_EXAMPLE_ALLOW_WRITES");
        let request_policy =
            ServerRequestPolicy::from_env("CODEX_EXAMPLE_APPROVE_REQUESTS");
        let thread_sandbox = if allow_writes {
            SandboxMode::WorkspaceWrite
        } else {
            SandboxMode::ReadOnly
        };

        let codex = ctx
            .builder()
            .client_name("codex-sdk-rs-interactive-repair")
            .client_version(env!("CARGO_PKG_VERSION"))
            .cwd(&cwd)
            .default_sandbox(thread_sandbox)
            .default_approval_policy(AskForApproval::OnRequest)
            .developer_instructions(
                "Work incrementally. Explain the diagnosis before making changes.",
            )
            .start()
            .await?;

        let thread = codex
            .thread()
            .cwd(&cwd)
            .personality(Personality::Pragmatic)
            .sandbox(thread_sandbox)
            .approval_policy(AskForApproval::OnRequest)
            .ephemeral(true)
            .start()
            .await?;
        let mut events = thread.event_stream()?;

        let diagnosis = thread
            .turn("Inspect the repository and propose a short repair plan. Do not modify files yet.")
            .sandbox(SandboxMode::ReadOnly)
            .effort(ReasoningEffort::Medium)
            .start()
            .await?;
        consume_turn(&mut events, diagnosis.turn_id(), request_policy).await?;

        let implementation_prompt = if allow_writes {
            "Implement the smallest safe improvement from the plan, then run the narrowest useful validation."
        } else {
            "Describe the exact patch you would make, but keep this read-only demonstration from modifying files."
        };
        let mut implementation = thread
            .turn(implementation_prompt)
            .approval_policy(AskForApproval::OnRequest)
            .effort(ReasoningEffort::High);
        implementation = if allow_writes {
            implementation.sandbox_policy(SandboxPolicy::WorkspaceWrite {
                writable_roots: Vec::new(),
                network_access: false,
                exclude_tmpdir_env_var: false,
                exclude_slash_tmp: false,
            })
        } else {
            implementation.sandbox(SandboxMode::ReadOnly)
        };
        let implementation = implementation.start().await?;

        let steering = std::env::var("CODEX_EXAMPLE_STEER").ok();
        let should_interrupt = env_flag("CODEX_EXAMPLE_INTERRUPT");
        if steering.is_some() || should_interrupt {
            wait_for_turn_started(&mut events, implementation.turn_id(), request_policy)
                .await?;
            if let Some(steering) = steering {
                implementation.steer(steering).await?;
            }
            if should_interrupt {
                implementation.interrupt().await?;
            }
        }

        consume_turn(&mut events, implementation.turn_id(), request_policy).await?;
        let snapshot = thread.read(false).await?;
        eprintln!("thread {} snapshot: {snapshot:?}", thread.id());

        codex.shutdown().await?;
        Ok(())
    })
}
