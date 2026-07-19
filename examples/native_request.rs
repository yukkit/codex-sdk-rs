//! Compare high-level inventory methods with the typed native request escape hatch.

use codex_sdk::prelude::*;

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let codex = ctx
            .builder()
            .client_name("codex-sdk-rs-native-request")
            .client_version(env!("CARGO_PKG_VERSION"))
            .minimal_prompt_context()
            .start()
            .await?;

        let visible_models = codex.models().await?;
        println!("high-level visible models: {}", visible_models.data.len());

        let request_id = codex.next_request_id();
        let all_models: codex_sdk::ModelListResponse = codex
            .request_typed(ClientRequest::ModelList {
                request_id,
                params: ModelListParams {
                    include_hidden: Some(true),
                    ..Default::default()
                },
            })
            .await?;
        println!(
            "native model/list response (including hidden): {}",
            all_models.data.len()
        );

        let account = codex
            .account_params(GetAccountParams {
                refresh_token: false,
            })
            .await?;
        println!(
            "account present: {}, OpenAI auth required: {}",
            account.account.is_some(),
            account.requires_openai_auth
        );

        codex.shutdown().await?;
        Ok(())
    })
}
