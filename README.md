# codex-sdk-rs

Rust SDK for using Codex app-server runtimes from server-side applications.

`codex-sdk-rs` provides an ergonomic Rust API for Codex runtime connections,
threads, turns, options, shutdown, and observability. It supports two runtime
backends:

- **In-process**: embed the Codex Rust app-server runtime in your process.
- **Remote app-server**: connect to an already-running Codex app-server over
  WebSocket or Unix socket.

The SDK exposes Codex's native `codex_app_server_client::AppServerEvent` through
a long-lived stream owned by each thread, including native server
notifications, requests, lag, and disconnection events.

## Status

This crate is an early implementation over Codex's app-server protocol.

- Crate version: `0.1.0`
- Minimum Rust version: `1.95.0`
- Upstream Codex Rust crates: `rust-v0.144.4`
- crates.io publishing: not enabled yet

When updating Codex, move all `codex-*` git dependencies together and follow the
[upgrade checklist](docs/upgrade-checklist.md).

## Install

Until a crates.io release is published, depend on the git repository directly:

```toml
[dependencies]
codex-sdk-rs = { git = "https://github.com/yukkit/codex-sdk-rs" }
anyhow = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tokio-stream = "0.1"
```

The Cargo package name is `codex-sdk-rs`; the Rust import name is `codex_sdk`.

## Quick Start

### In-Process Runtime

Use `run_main` for binaries that embed Codex. It initializes Codex helper entry
points such as shell, `apply_patch`, and the Linux sandbox before Tokio starts.

```rust,no_run
use codex_sdk::prelude::*;
use tokio_stream::StreamExt;

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let codex = ctx
            .builder()
            .client_name("my-codex-app")
            .codex_home("configs/codex")
            .start()
            .await?;

        let thread = codex
            .thread()
            .cwd(std::env::current_dir()?)
            .sandbox(SandboxMode::ReadOnly)
            .approval_policy(AskForApproval::Never)
            .start()
            .await?;
        let mut events = thread.event_stream()?;
        let turn = thread
            .turn("Summarize this repository in one paragraph.")
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
```

### Remote App-Server

Remote clients do not start Codex helpers in the local process, so they can use a
normal Tokio runtime.

```rust,no_run
use codex_sdk::prelude::*;
use tokio_stream::StreamExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let codex = Codex::remote_websocket("wss://codex.example.com/rpc")
        .auth_token(std::env::var("CODEX_APP_SERVER_TOKEN")?)
        .client_name("my-codex-app")
        .start()
        .await?;

    let thread = codex.thread().start().await?;
    let mut events = thread.event_stream()?;
    let turn = thread.turn("Reply with one short sentence.").start().await?;
    let turn_id = turn.turn_id().to_string();

    while let Some(event) = events.next().await {
        println!("{event:?}");
        if matches!(
            event,
            AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(ref done))
                if done.turn.id == turn_id
        ) {
            break;
        }
    }
    codex.shutdown().await?;
    Ok(())
}
```

Use `Codex::remote_unix_socket(path)` when the remote app-server is exposed over
a Unix socket.

## API Overview

| Surface | Purpose |
| --- | --- |
| `Codex::builder()` | Start an in-process app-server with common SDK options. |
| `Codex::builder_with_config(config)` | Start in-process with a native `codex_core::config::Config`. |
| `Codex::remote_websocket(url)` | Connect to a remote WebSocket app-server. |
| `Codex::remote_unix_socket(path)` | Connect to a remote Unix-socket app-server. |
| `codex.event_stream()?` | Take the runtime's stream of events without a thread target. |
| `codex.thread().start().await?` | Create a reusable thread. |
| `thread.event_stream()?` | Take the thread's long-lived native event stream. |
| `codex.request_typed::<T>(request).await?` | Send an arbitrary native `ClientRequest`. |
| `thread.turn(input).start().await?` | Start a turn and get a handle for steering or interruption. |
| `codex.turn(input).start().await?` | Start one turn on a temporary thread. Its handle exposes the owning thread. |
| `codex.models().await?` | List visible Codex models. |
| `codex.account().await?` | Read the current Codex account state. |

Turn input can be a string or `Vec<UserInput>`. Use
`codex.turn_params(params)` or `thread.turn_params(params)` when you need full
native `TurnStartParams` control.

Thread events are always consumed as a stream so applications can process
native status, error, output, and `ServerRequest` events across any number of
turns without a second batch-result model. `TurnCompleted` marks one turn's
completion; it does not end the thread stream.

Successful SDK archive calls deliver `ThreadArchived` before the old stream
ends, even if the upstream notification is delayed or dropped. Observed
`ThreadDeleted` and `ThreadClosed` notifications are terminal in the same way;
unarchive creates a new `Thread` attachment and stream.

Consume each active stream continuously. Live queues preserve transcript,
completion, and `ServerRequest` events with backpressure; progress-style events
may be skipped under pressure and reported through `Lagged`. Because the event
pump is shared, one full reliable queue can delay events for other threads.

Do not overlap turns for the same thread ID. Wait for its `TurnCompleted` event
before starting the next turn; different thread IDs can run concurrently.

Runtime-scoped notifications and threadless server requests are delivered once
through `CodexEventStream`, rather than being copied into every thread stream.

## Configuration Model

In-process mode loads and resolves Codex config in the local process.
`Codex::builder()` exposes the common SDK-facing options and constructs the
internal app-server start config for you. Use `Codex::builder_with_config(config)`
only when your application already owns native Codex config resolution.

Remote mode treats the app-server as the configuration owner. The Rust SDK
builder only configures the transport and client identity; use thread and turn
builders for per-request overrides.

For low-token or pure chat sessions, in-process builders can use
`minimal_prompt_context()` to disable optional Codex context instructions.
`base_instructions(...)`, `developer_instructions(...)`,
`reasoning_effort(...)`, and `reasoning_summary(...)` are available as runtime
defaults and can still be overridden per turn.

## Observability

The SDK re-exports Codex's official `codex-otel` types and provides a small
builder for common service setup:

```rust,no_run
let _observability = codex_sdk::Observability::builder()
    .with_env()?
    .service_name("my-codex-app")
    .service_version(env!("CARGO_PKG_VERSION"))
    .install()?;
```

Use `install_subscriber(false).build_provider()?` when your application already
owns the global `tracing_subscriber`.

## Documentation

- [SDK user guide](docs/sdk-user-guide.md): for application developers using
  `codex-sdk-rs`.
- [API parity notes](docs/api-parity.md): comparison with the official
  TypeScript and Python Codex SDK public APIs.
- [Native Codex developer guide](docs/native-codex-developer-guide.md): for
  SDK maintainers who need the original Codex app-server/config model.
- [Codex upgrade checklist](docs/upgrade-checklist.md): for bumping the
  upstream Codex git tag safely.

## Development

Set up local tooling once:

```sh
make setup
```

Useful checks:

```sh
make deny_check
make fmt_check
make clippy_check
make check
```

`deny.toml` intentionally keeps a few narrow exceptions for the current Codex
dependency graph. When bumping the Codex tag, revisit those exceptions with the
upgrade checklist instead of blindly carrying them forward.
