//! Persistent thread creation, inspection, branching, compaction, and archival.

mod common;

use anyhow::bail;
use codex_sdk::prelude::*;
use common::{ServerRequestPolicy, consume_turn, wait_for_archive, wait_for_compaction};

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let args: Vec<_> = std::env::args().skip(1).collect();
        let command = args.first().cloned().unwrap_or_else(|| "list".to_owned());
        let thread_id = args.get(1).cloned();

        let codex = ctx
            .builder()
            .client_name("codex-sdk-rs-thread-lifecycle")
            .client_version(env!("CARGO_PKG_VERSION"))
            .default_sandbox(SandboxMode::ReadOnly)
            .default_approval_policy(AskForApproval::Never)
            .ephemeral(false)
            .start()
            .await?;

        match command.as_str() {
            "create" => create_and_branch(&codex).await?,
            "resume" => resume_and_continue(&codex, required_id(thread_id)?).await?,
            "inspect" => inspect(&codex, required_id(thread_id)?).await?,
            "compact" => compact(&codex, required_id(thread_id)?).await?,
            "archive" => {
                codex.archive_thread(required_id(thread_id)?).await?;
                println!("thread archived");
            }
            "unarchive" => {
                let thread = codex.unarchive_thread(required_id(thread_id)?).await?;
                println!("restored thread {}", thread.id());
                let snapshot = thread.read(true).await?;
                println!("restored turns: {}", snapshot.thread.turns.len());
            }
            "list" => print_thread_page(&codex).await?,
            _ => bail!(
                "unknown command {command:?}; expected create, resume, inspect, compact, archive, unarchive, or list"
            ),
        }

        codex.shutdown().await?;
        Ok(())
    })
}

async fn create_and_branch(codex: &Codex) -> anyhow::Result<()> {
    let params = ThreadStartParams {
        cwd: Some(std::env::current_dir()?.to_string_lossy().into_owned()),
        ephemeral: Some(false),
        ..Default::default()
    };
    let thread = codex
        .thread()
        .params(params)
        .base_instructions("You are a persistent repository assistant.")
        .developer_instructions("Keep answers concise and repository-specific.")
        .sandbox(SandboxMode::ReadOnly)
        .approval_policy(AskForApproval::Never)
        .start()
        .await?;
    thread.set_name("example-main-investigation").await?;
    let mut events = thread.event_stream()?;
    let turn = thread
        .turn("Summarize the repository in three bullets so this thread has useful history.")
        .start()
        .await?;
    consume_turn(&mut events, turn.turn_id(), ServerRequestPolicy::Reject).await?;
    println!("created thread {}", thread.id());
    let snapshot = thread.read(true).await?;
    println!("persisted turns: {}", snapshot.thread.turns.len());

    let fork = thread.fork().await?;
    fork.set_name("example-archived-branch").await?;
    let fork_id = fork.id().to_owned();
    let mut fork_events = fork.event_stream()?;
    fork.archive().await?;
    wait_for_archive(&mut fork_events, &fork_id).await?;
    let restored = codex.unarchive_thread(&fork_id).await?;
    println!("forked, archived, and restored thread {}", restored.id());

    print_thread_page(codex).await?;
    Ok(())
}

async fn resume_and_continue(codex: &Codex, thread_id: String) -> anyhow::Result<()> {
    let thread = codex.resume_thread(&thread_id).await?;
    let mut events = thread.event_stream()?;
    let turn = thread
        .turn("Continue by identifying the single most important maintenance task.")
        .start()
        .await?;
    consume_turn(&mut events, turn.turn_id(), ServerRequestPolicy::Reject).await?;
    Ok(())
}

async fn inspect(codex: &Codex, thread_id: String) -> anyhow::Result<()> {
    let thread = codex.resume_thread(&thread_id).await?;
    let snapshot = thread.read(true).await?;
    println!("thread: {}", snapshot.thread.id);
    println!("name: {:?}", snapshot.thread.name);
    println!("status: {:?}", snapshot.thread.status);
    println!("turns: {}", snapshot.thread.turns.len());
    Ok(())
}

async fn compact(codex: &Codex, thread_id: String) -> anyhow::Result<()> {
    let thread = codex.resume_thread(&thread_id).await?;
    let mut events = thread.event_stream()?;
    thread.compact().await?;
    wait_for_compaction(&mut events, thread.id()).await?;
    println!("compacted thread {}", thread.id());
    Ok(())
}

fn required_id(thread_id: Option<String>) -> anyhow::Result<String> {
    match thread_id {
        Some(thread_id) => Ok(thread_id),
        None => bail!("this command requires a thread id"),
    }
}

async fn print_thread_page(codex: &Codex) -> anyhow::Result<()> {
    let page = codex
        .list_threads_params(ThreadListParams {
            cursor: None,
            limit: Some(10),
            sort_key: None,
            sort_direction: None,
            model_providers: None,
            source_kinds: None,
            archived: None,
            cwd: None,
            use_state_db_only: false,
            search_term: None,
            parent_thread_id: None,
            ancestor_thread_id: None,
        })
        .await?;
    println!("threads in first page: {}", page.data.len());
    for thread in &page.data {
        println!(
            "thread: id={}, name={:?}, status={:?}",
            thread.id, thread.name, thread.status
        );
    }
    Ok(())
}
