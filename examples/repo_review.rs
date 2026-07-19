//! One-off, structured, optionally multimodal repository review.

mod common;

use std::path::PathBuf;

use codex_sdk::prelude::*;
use common::{ServerRequestPolicy, consume_turn};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ReviewReport {
    summary: String,
    risks: Vec<String>,
    recommendations: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let cwd = std::env::current_dir()?;
        let codex = ctx
            .builder()
            .client_name("codex-sdk-rs-repo-review")
            .client_version(env!("CARGO_PKG_VERSION"))
            .cwd(&cwd)
            .default_sandbox(SandboxMode::ReadOnly)
            .default_approval_policy(AskForApproval::Never)
            .reasoning_effort(ReasoningEffort::High)
            .reasoning_summary(ReasoningSummary::Concise)
            .personality(Personality::Pragmatic)
            .channel_capacity(512)
            .start()
            .await?;

        let schema = serde_json::json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string" },
                "risks": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "recommendations": {
                    "type": "array",
                    "items": { "type": "string" }
                }
            },
            "required": ["summary", "risks", "recommendations"],
            "additionalProperties": false
        });

        let mut input = vec![UserInput::Text {
            text: "Review this repository. Focus on correctness, maintainability, and operational risks. Return only the requested JSON object.".into(),
            text_elements: Vec::new(),
        }];
        if let Some(image) = std::env::var_os("CODEX_EXAMPLE_IMAGE") {
            input.push(UserInput::LocalImage {
                path: PathBuf::from(image),
                detail: None,
            });
        }

        let thread_defaults = ThreadStartParams {
            cwd: Some(cwd.to_string_lossy().into_owned()),
            ephemeral: Some(true),
            ..Default::default()
        };
        let mut turn_builder = codex
            .turn(input)
            .thread_params(thread_defaults)
            .base_instructions("You are a senior software reviewer.")
            .developer_instructions("Be concrete and do not modify the workspace.")
            .sandbox(SandboxMode::ReadOnly)
            .approval_policy(AskForApproval::Never)
            .effort(ReasoningEffort::High)
            .reasoning_summary(ReasoningSummary::Concise)
            .personality(Personality::Pragmatic)
            .output_schema(schema);

        if let Ok(model) = std::env::var("CODEX_MODEL") {
            turn_builder = turn_builder.model(model);
        }
        if let Ok(provider) = std::env::var("CODEX_MODEL_PROVIDER") {
            turn_builder = turn_builder.model_provider(provider);
        }
        if let Ok(tier) = std::env::var("CODEX_SERVICE_TIER") {
            turn_builder = turn_builder.service_tier(tier);
        }

        let turn = turn_builder.start().await?;
        eprintln!(
            "temporary thread {}, turn {}",
            turn.thread_id(),
            turn.turn_id()
        );
        let mut events = turn.thread().event_stream()?;
        let response =
            consume_turn(&mut events, turn.turn_id(), ServerRequestPolicy::Reject)
                .await?;
        let report: ReviewReport = serde_json::from_str(&response)?;

        println!("summary: {}", report.summary);
        for risk in report.risks {
            println!("risk: {risk}");
        }
        for recommendation in report.recommendations {
            println!("recommendation: {recommendation}");
        }

        codex.shutdown().await?;
        Ok(())
    })
}
