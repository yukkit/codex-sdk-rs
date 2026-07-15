use codex_sdk::prelude::*;

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

        let result = codex
            .turn("Describe the current working directory in one sentence.")
            .sandbox(SandboxMode::ReadOnly)
            .approval_policy(AskForApproval::Never)
            .send()
            .await?;

        println!("{}", result.final_response());
        codex.shutdown().await?;
        Ok(())
    })
}
