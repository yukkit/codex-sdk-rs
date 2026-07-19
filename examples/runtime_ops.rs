//! Production-style observability, warmup, inventory, and shutdown example.

mod common;

use std::time::Duration;

use codex_sdk::prelude::*;
use common::env_flag;
use tokio_stream::StreamExt;

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let observability = Observability::builder()
            .with_env()?
            .environment("example")
            .service_name("codex-sdk-rs-runtime-ops")
            .service_version(env!("CARGO_PKG_VERSION"))
            .runtime_metrics(true)
            .install()?;

        let cwd = std::env::current_dir()?;
        let codex = ctx
            .builder()
            .client_name("codex-sdk-rs-runtime-ops")
            .client_version(env!("CARGO_PKG_VERSION"))
            .cwd(&cwd)
            .default_sandbox(SandboxMode::ReadOnly)
            .default_approval_policy(AskForApproval::Never)
            .reasoning_effort(ReasoningEffort::Low)
            .reasoning_summary(ReasoningSummary::None)
            .personality(Personality::Pragmatic)
            .developer_instructions("Prefer concise operational responses.")
            .minimal_prompt_context()
            .channel_capacity(512)
            .app_server_channel_capacity(1024)
            .event_stream_capacity(512)
            .start()
            .await?;

        let mut runtime_events = codex.event_stream()?;
        let runtime_event_task = tokio::spawn(async move {
            while let Some(event) = runtime_events.next().await {
                eprintln!("runtime event: {event:?}");
                if matches!(event, AppServerEvent::Disconnected { .. }) {
                    break;
                }
            }
        });

        let operations = async {
            let mut warmup = codex
                .warmup()
                .cwd(&cwd)
                .models(true)
                .skills(true)
                .permission_profiles(true)
                .config_requirements(true)
                .account(true)
                .mcp_status(true)
                .apps(env_flag("CODEX_EXAMPLE_WARM_APPS"))
                .force_reload_skills(env_flag("CODEX_EXAMPLE_RELOAD_SKILLS"))
                .step_timeout(Duration::from_secs(15));
            if let Ok(model) = std::env::var("CODEX_MODEL") {
                warmup = warmup.model(model);
            }
            if let Ok(provider) = std::env::var("CODEX_MODEL_PROVIDER") {
                warmup = warmup.model_provider(provider);
            }
            let warmup = warmup.send().await?;
            println!("warmup complete: {}", warmup.is_complete());
            println!("warmup result: {warmup:#?}");

            let models = codex.models().await?;
            println!("visible models: {}", models.data.len());
            let account = codex.account().await?;
            println!(
                "authenticated: {}, auth required: {}",
                account.account.is_some(),
                account.requires_openai_auth
            );
            anyhow::Ok(())
        }
        .await;

        let shutdown = codex.shutdown().await;
        let runtime_events = runtime_event_task.await;
        observability.shutdown();

        operations?;
        shutdown?;
        runtime_events?;
        Ok(())
    })
}
