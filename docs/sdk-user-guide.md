# codex-sdk-rs User Guide

This guide is for application developers using `codex-sdk-rs`. You do not need
to work directly with the Codex app-server JSON-RPC protocol, `AppServerClient`,
`InProcessAppServerClient`, or Codex internal crates; the SDK provides a stable
Rust API:

- `Codex`: a shared Codex runtime.
- `Thread`: a Codex thread that can continue a conversation.
- `TurnBuilder` / `TurnHandle` / `TurnStream`: one user request, optional
  active-turn control, and its event stream.
- `ServerRequest`: native Codex requests, such as command approvals and MCP
  elicitations.
- `Observability`: the entry point for tracing / OpenTelemetry initialization.

## Minimal Example

```rust,no_run
use codex_sdk::prelude::*;

fn main() -> anyhow::Result<()> {
    codex_sdk::run_main(|ctx| async move {
        let _observability = Observability::builder()
            .with_env()?
            .service_name("my-codex-app")
            .install()?;

        let codex = ctx
            .builder()
            .client_name("my-codex-app")
            .codex_home("configs/codex")
            .default_sandbox(SandboxMode::ReadOnly)
            .default_approval_policy(AskForApproval::Never)
            .start()
            .await?;

        let result = codex
            .turn("Summarize this repository in one paragraph.")
            .cwd(std::env::current_dir()?)
            .send()
            .await?;

        println!("{}", result.final_response());
        codex.shutdown().await?;
        Ok(())
    })
}
```

In-process binary entry points must be wrapped with `codex_sdk::run_main`. It
initializes Codex helper argv0 dispatch before Tokio starts; shell,
`apply_patch`, the Linux sandbox, and related capabilities depend on this entry
point. Remote mode does not start Codex helpers in the current process and can
use a normal Tokio runtime.

## Runtime Backends

The SDK supports three app-server backends:

```rust,no_run
// 1. Default: embed Codex app-server in the current process.
let local = ctx.builder().start().await?;

// 2. Connect to an already-running remote WebSocket app-server.
let ws = Codex::remote_websocket("wss://codex.example.com/rpc")
    .auth_token("remote-bearer-token")
    .client_name("my-codex-app")
    .start()
    .await?;

// 3. Connect to a local or sidecar Unix-socket app-server.
let uds = Codex::remote_unix_socket("/tmp/codex-app-server.sock")
    .client_name("my-codex-app")
    .start()
    .await?;
```

Remote backends only connect to and consume the app-server protocol. Codex
config, auth, tooling, sandbox, working directory, and other runtime defaults
belong to the remote app-server process; they are not read from local
`CodexBuilder` settings. Use `codex.thread().xxx(...)`,
`thread.turn(...).xxx(...)`, or native `ThreadStartParams` / `TurnStartParams`
for per-request overrides.

`CodexRemoteBuilder::default_thread_params(...)` sets SDK-side defaults copied
into new thread builders. This mainly affects temporary-thread conveniences such
as `codex.turn(...)`.

## Cargo Dependency

Example path dependency inside this repository:

```toml
[dependencies]
codex-sdk-rs = { path = "../codex-sdk-rs" }
anyhow = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

The crate name is `codex-sdk-rs`, and the Rust import name is `codex_sdk`.

## Development Environment For This Repository

Before running checks in this repository for the first time, install the tools:

```sh
make setup
```

It detects and installs as needed:

- `cargo-deny`
- nightly `rustfmt`
- `cargo-udeps`
- `clippy`

Common checks:

```sh
make deny_check
make fmt_check
make clippy_check
make check
```

`deny.toml` contains a small number of explicit exceptions for the current Codex
tag, such as git source, license, advisory, and wrapper exceptions in the
upstream Codex dependency graph. When upgrading the Codex tag, do not carry
these forward mechanically; recheck them with the
[Codex upgrade checklist](upgrade-checklist.md).

## Configuration Sources

The in-process backend loads native Codex configuration in the current process.
In most applications, long-lived configuration belongs in
`CODEX_HOME/config.toml`, while one-off overrides belong on SDK builders. Remote
backend configuration belongs to the remote app-server process.

Common configuration directories:

- Default `CODEX_HOME`: `~/.codex`
- Application-specified: `.codex_home("configs/codex")`
- Environment variable: `CODEX_HOME=/path/to/codex-home`

Recommended convention:

```text
configs/
  .env              # Application env vars, OTEL, provider key names, etc.
  codex/
    config.toml     # Native Codex configuration
    auth.json       # If using the file credential store
```

Note: if you set `CODEX_HOME`, the directory must already exist.

## In-process Builder Configuration

`ctx.builder()` returns a `CodexBuilder`, which lets the SDK load native Codex
configuration and apply a small set of common code-layer defaults.

```rust,no_run
let codex = ctx
    .builder()
    .client_name("my-service")
    .client_version(env!("CARGO_PKG_VERSION"))
    .codex_home("configs/codex")
    .cwd("/workspace/my-repo")
    .model("gpt-5.x")
    .default_sandbox(SandboxMode::WorkspaceWrite)
    .default_approval_policy(AskForApproval::OnRequest)
    .ephemeral(true)
    .channel_capacity(1024)
    .start()
    .await?;
```

Field notes:

- `client_name`: your integration name, included in logs, traces, and upstream
  client metadata.
- `client_version`: your integration version.
- `codex_home`: Codex configuration and state directory; if omitted, Codex uses
  its default `~/.codex` or the environment.
- `cwd`: the runtime default working directory.
- `model`: default model override; if omitted, Codex config is used.
- `service_tier`: default service tier for new threads.
- `personality`: default model personality for new threads.
- `reasoning_effort`: default reasoning effort; an individual turn can still
  override it with `effort(...)`.
- `reasoning_summary`: default reasoning summary behavior, such as `auto`,
  `concise`, `detailed`, or `none`.
- `base_instructions`: replaces Codex's native base instructions, which become
  the large Responses API `instructions` payload; pass an empty string to disable
  the built-in base prompt.
- `developer_instructions`: adds application-owned developer instructions.
- `minimal_prompt_context`: disables optional permissions, apps, collaboration
  mode, environment, and skills instruction blocks for pure chat or low-token
  sessions. It does not remove base instructions or tool schemas.
- `default_sandbox`: default sandbox.
- `default_approval_policy`: default approval policy.
- `ephemeral`: whether sessions are ephemeral by default.
- `channel_capacity`: runtime event broadcast buffer size.

Replace the default prompt and reduce optional context:

```rust,no_run
let codex = ctx
    .builder()
    .reasoning_effort(ReasoningEffort::Low)
    .reasoning_summary(ReasoningSummary::None)
    .base_instructions("You are a concise assistant for product support.")
    .developer_instructions("Prefer short answers and ask one question at a time.")
    .minimal_prompt_context()
    .start()
    .await?;
```

For finer control, use `include_permissions_instructions(false)`,
`include_apps_instructions(false)`, `include_collaboration_mode_instructions(false)`,
`include_environment_context(false)`, or `include_skill_instructions(false)`. These
are code-layer overrides on `CodexBuilder`; when using
`builder_with_config(config)`, edit the supplied native `Config` directly.

If your application already resolved a native `codex_core::config::Config`
through Codex's own loader, pass it directly to the SDK with
`ctx.builder_with_config(config)` instead of mapping it through an SDK-specific
middle structure:

```rust,no_run
let config: Config = build_native_codex_config().await?;

let codex = ctx
    .builder_with_config(config)
    .client_name("my-service")
    .client_version(env!("CARGO_PKG_VERSION"))
    .channel_capacity(1024)
    .start()
    .await?;
```

`CodexWithConfigBuilder` only exposes runtime options that are not part of
`Config`, such as `client_name`, `client_version`, and `channel_capacity`.
Values such as `cwd`, `model`, `model_provider`, `service_tier`,
`personality`, `default_sandbox`, `default_approval_policy`, `ephemeral`,
base/developer instructions, and prompt context switches must come from the
supplied native `Config`.

## SandboxMode

The SDK uses native Codex `SandboxMode` directly:

```rust,no_run
SandboxMode::ReadOnly
SandboxMode::WorkspaceWrite
SandboxMode::DangerFullAccess
```

Recommendations:

- Read-only analysis and reviews: `ReadOnly`
- Allow edits to the current workspace: `WorkspaceWrite`
- Host environment is already isolated and you explicitly accept the risk:
  `DangerFullAccess`

If you need finer-grained policies such as writable roots or network access,
write them in native Codex `config.toml`, or pass a native `SandboxPolicy` to
`TurnBuilder::sandbox_policy(...)` / `CodexTurnBuilder::sandbox_policy(...)` for
one turn:

```toml
sandbox_mode = "workspace-write"

[sandbox_workspace_write]
network_access = false
writable_roots = ["/tmp/my-tool-cache"]
```

## AskForApproval

The SDK uses native Codex `AskForApproval` directly:

```rust,no_run
AskForApproval::UnlessTrusted
AskForApproval::OnRequest
AskForApproval::Never
```

Recommendations:

- User-facing Web/UI products: `OnRequest`, and show the `ServerRequest` plus
  request id to the user.
- Trusted automation that should fail closed: `Never` with synchronous
  `send()`; requests that require approval will be rejected by the SDK.
- More conservative interactive workflows: `UnlessTrusted`.

Because this is the native Codex type, granular policy can be passed directly
with `AskForApproval::Granular`; long-lived defaults can also remain in
`config.toml`.

## One-shot Turns

`codex.turn(input)` is a convenience path: it creates a temporary thread, starts
one turn, and collects the final text. `input` can be `&str` / `String` or native
`Vec<UserInput>`.

```rust,no_run
let result = codex
    .turn("Review the current repository and summarize the risks.")
    .cwd("/workspace/repo")
    .sandbox(SandboxMode::ReadOnly)
    .approval_policy(AskForApproval::Never)
    .effort(ReasoningEffort::High)
    .reasoning_summary(ReasoningSummary::Auto)
    .output_schema(serde_json::json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "risks": {
                "type": "array",
                "items": { "type": "string" }
            }
        },
        "required": ["summary", "risks"],
        "additionalProperties": false
    }))
    .timeout(std::time::Duration::from_secs(300))
    .send()
    .await?;

println!("{}", result.final_response());
```

For multimodal or lower-level input, pass native `UserInput` directly:

```rust,no_run
let result = codex
    .turn(vec![
        UserInput::Text {
            text: "Describe this image.".into(),
            text_elements: Vec::new(),
        },
        UserInput::LocalImage {
            path: "/workspace/repo/screenshot.png".into(),
            detail: None,
        },
    ])
    .send()
    .await?;
```

This path fits synchronous HTTP APIs, CLI wrappers, and CI tasks. It is not a
good fit for interactive UIs that need human approvals or complex MCP
elicitations.

## Reusing Threads

Create a `Thread` first when you need multi-turn context:

```rust,no_run
let thread = codex
    .thread()
    .cwd("/workspace/repo")
    .model("gpt-5.x")
    .sandbox(SandboxMode::WorkspaceWrite)
    .approval_policy(AskForApproval::OnRequest)
    .ephemeral(true)
    .start()
    .await?;

let _first = thread.turn("Draft a repair plan first.").send().await?;
let second = thread.run("Implement the first step from that plan.").await?;
```

By default, the same `Thread` allows only one active turn at a time. A second
`turn.stream().await` waits until the previous `TurnStream` completes or is
dropped. Different `Thread`s can run concurrently.

`Thread::run(input)` is the shortest path and uses the thread's current default
turn options. Use `thread.turn(input).xxx(...).send().await?` when you need to
set model, sandbox, reasoning, output schema, or other turn options.

Saved threads can be resumed, forked, listed, archived, and unarchived through
the shared `Codex` handle:

```rust,no_run
let resumed = codex.resume_thread("thread-id").await?;
let forked = resumed.fork().await?;
let _page = codex.list_threads().await?;

forked.set_name("investigation").await?;
forked.archive().await?;
let restored = codex.unarchive_thread(forked.id()).await?;
let _snapshot = restored.read(true).await?;
```

Use `resume_thread_params(...)`, `fork_thread_params(...)`, or
`list_threads_params(...)` when you need the full native app-server request
surface.

## Models And Account

`Codex` exposes two read-only runtime APIs for model pickers, health checks, and
auth status display:

```rust,no_run
let models = codex.models().await?;
let account = codex.account().await?;

if account.account.is_none() && account.requires_openai_auth {
    tracing::warn!("Codex runtime is not authenticated");
}

for model in models.data {
    println!("{} {}", model.id, model.display_name);
}
```

Use native params when you need hidden models, pagination, or a proactive token
refresh:

```rust,no_run
let models = codex
    .models_params(ModelListParams {
        include_hidden: Some(true),
        ..Default::default()
    })
    .await?;

let account = codex
    .account_params(GetAccountParams {
        refresh_token: true,
    })
    .await?;
```

The SDK intentionally does not expose high-level `login_*` / `logout` methods
yet. In embedded server-side use cases, authentication usually belongs to the
host system or to a preconfigured `codex_home`.

## Streaming And Events

Interactive UIs should use streaming:

```rust,no_run
let mut stream = thread
    .turn("Run the pg MCP smoke test.")
    .approval_policy(AskForApproval::OnRequest)
    .stream()
    .await?;

while let Some(event) = stream.next().await {
    match event {
        TurnEvent::ServerNotification(
            ServerNotification::AgentMessageDelta(delta),
        ) => {
            print!("{}", delta.delta);
        }
        TurnEvent::ServerNotification(
            ServerNotification::TurnCompleted(done),
        ) => {
            eprintln!("turn completed: {:?}", done.turn.status);
            break;
        }
        TurnEvent::ServerRequest(request) => {
            // Show this to the user, then resolve or reject it.
            stream
                .reject_server_request(request.id().clone(), "not approved")
                .await?;
        }
        other => {
            tracing::debug!(?other, "codex event");
        }
    }
}
```

If you need to steer or interrupt a turn before or while consuming the stream,
start it as a `TurnHandle`:

```rust,no_run
let turn = thread.turn("Count from 1 to 100.").start().await?;
turn.steer("Stop after 10 numbers.").await?;

let mut stream = turn.stream();
while let Some(event) = stream.next().await {
    if matches!(
        event,
        TurnEvent::ServerNotification(ServerNotification::TurnCompleted(_))
    ) {
        break;
    }
}
```

`TurnStream` also exposes `steer(...)` and `interrupt()` so a UI can keep one
object while consuming events and sending control requests.

Common events:

- `ServerNotification`: native Codex app-server notifications, such as
  `AgentMessageDelta`, `TurnCompleted`, `Error`, token usage, and so on.
- `ServerRequest`: requests that the application needs to answer.
- `Lagged`: the event buffer skipped messages.
- `RuntimeClosed`: the runtime is closed.

`TurnEvent` is only the SDK stream envelope. The actual protocol payloads are
native Codex `ServerNotification` / `ServerRequest` values, so Codex protocol
field and variant changes are exposed at compile time as much as possible.

Note: the underlying event distribution currently uses a broadcast channel.
`Lagged` means this consumer has missed some events; interactive UIs should mark
the current turn as needing refresh or retry. Synchronous `send()` collection
still relies on receiving `TurnCompleted` before returning a complete
`TurnResult`, so long-running or highly interactive scenarios should prefer
`stream()` directly.

## Handling ServerRequest

`ServerRequest` appears for command approvals, file change approvals, permission
requests, MCP elicitations, and similar cases. If the application does not
answer, the turn may wait.

```rust,no_run
match event {
    TurnEvent::ServerRequest(request) => {
        println!("request id = {}", request.id());
        println!("request = {:?}", request);

        if user_approved() {
            stream
                .approve_server_request(request.id().clone())
                .await?;
        } else {
            stream
                .reject_server_request(request.id().clone(), "declined by user")
                .await?;
        }
    }
    _ => {}
}
# fn user_approved() -> bool { false }
```

For simple approvals, `approve_server_request()` sends `{}`. For complex
requests, match the native `ServerRequest` variant and pass the matching typed
response; you can also pass `serde_json::json!` directly:

```rust,no_run
stream
    .resolve_server_request(
        request.id().clone(),
        serde_json::json!({
            "action": "accept",
            "content": {},
            "_meta": null
        }),
    )
    .await?;
```

For Web UIs, serialize `ServerRequest` and send it to the frontend. After the
user clicks a button, the frontend sends the `requestId` and decision back to
the backend; the backend can respond with the shared `Codex` handle and does not
need to cache SDK-internal objects:

```rust,no_run
codex
    .resolve_server_request(request_id, serde_json::json!({
        "action": "accept",
        "content": {},
        "_meta": null
    }))
    .await?;
```

Most `ServerRequest`s carry thread/turn ids, and the SDK delivers them only to
the matching `TurnStream`. A small number of global Codex requests do not carry
thread/turn ids and may be visible to multiple active streams. For those
requests, de-duplicate at the application layer by `RequestId` and resolve or
reject through the shared `Codex` handle.

## TurnStartParams

```rust,no_run
let params = TurnStartParams {
    input: vec![UserInput::Text {
        text: "Run the task.".into(),
        text_elements: Vec::new(),
    }],
    cwd: Some("/workspace/repo".into()),
    model: Some("gpt-5.x".into()),
    approval_policy: Some(AskForApproval::OnRequest),
    effort: Some(ReasoningEffort::High),
    summary: Some(ReasoningSummary::Auto),
    output_schema: Some(serde_json::json!({
        "type": "object",
        "properties": {
            "answer": { "type": "string" },
            "confidence": { "type": "number" }
        },
        "required": ["answer", "confidence"],
        "additionalProperties": false
    })),
    ..Default::default()
};

let result = thread
    .turn_params(params)
    .timeout(std::time::Duration::from_secs(300))
    .send()
    .await?;
```

Fields:

- `input`: user input for this turn. `Thread::turn(input)` / `Codex::turn(input)`
  accepts a string or `Vec<UserInput>` and writes it into `TurnStartParams.input`;
  `.params(...)` fully replaces the native params, including `input`. When
  constructing a complete `TurnStartParams`, prefer `thread.turn_params(params)`
  or `codex.turn_params(params)`.
- `cwd`: working directory override for this turn.
- `model`: model override for this turn.
- `service_tier`: service tier override for this turn and subsequent turns; use
  `clear_service_tier()` to explicitly clear it.
- `personality`: model personality override for this turn and subsequent turns.
- `approval_policy`: approval policy override for this turn.
- `sandbox_policy`: exact native sandbox policy override. The builder
  convenience method `sandbox(SandboxMode::...)` creates a simple policy with
  network disabled; use `sandbox_policy(...)` for writable roots or network
  access.
- `effort`: reasoning effort for this turn.
- `reasoning_summary` / `summary`: reasoning summary behavior for this turn; the
  builder method is `reasoning_summary(...)`, and the native `TurnStartParams`
  field is `summary`.
- `output_schema`: JSON Schema for the final assistant message; useful for
  structured results.
- `timeout(...)`: SDK collection behavior, not part of native Codex
  `TurnStartParams`; it only applies to `send()` collection paths, and streaming
  callers should manage timeout/cancellation themselves.

## Structured Output

Native Codex supports constraining the final answer through
`turn/start.outputSchema`. The SDK exposes this as `output_schema(...)` on
`TurnStartParams`, `CodexTurnBuilder`, and `TurnBuilder`.

```rust,no_run
#[derive(serde::Deserialize)]
struct ReviewSummary {
    summary: String,
    risks: Vec<String>,
}

let schema = serde_json::json!({
    "type": "object",
    "properties": {
        "summary": { "type": "string" },
        "risks": {
            "type": "array",
            "items": { "type": "string" }
        }
    },
    "required": ["summary", "risks"],
    "additionalProperties": false
});

let result = codex
    .turn("Summarize the risks in the current repository.")
    .approval_policy(AskForApproval::Never)
    .output_schema(schema)
    .send()
    .await?;

let summary: ReviewSummary = result.final_response_as()?;
```

`final_response_json()` parses the response into `serde_json::Value`;
`final_response_as<T>()` deserializes it directly into your type. The model
still returns final content as an assistant message. The SDK only sends the
schema and parses the final text.

## ThreadStartParams

```rust,no_run
let params = ThreadStartParams {
    cwd: Some("/workspace/repo".to_string()),
    model: Some("gpt-5.x".to_string()),
    model_provider: Some("proxy".to_string()),
    ..Default::default()
};

let thread = codex
    .thread()
    .params(params)
    .approval_policy(AskForApproval::OnRequest)
    .sandbox(SandboxMode::WorkspaceWrite)
    .ephemeral(true)
    .start()
    .await?;
```

`ThreadBuilder` directly uses native Codex `ThreadStartParams`, so upstream
thread/start fields can be passed through with `.params(...)`. Methods such as
`.cwd()`, `.model()`, `.model_provider()`, `.base_instructions()`,
`.developer_instructions()`, and `.approval_policy()` are only convenience
setters for common fields. Later turns can still override some turn-level fields
again.

## Observability

The SDK provides an `Observability` builder and re-exports Codex's official
`codex-otel` types.

Read from environment variables:

```rust,no_run
let _observability = Observability::builder()
    .with_env()?
    .service_name("my-codex-app")
    .service_version(env!("CARGO_PKG_VERSION"))
    .install()?;
```

Read from `.env`:

```rust,no_run
let _observability = Observability::builder()
    .with_dotenv("configs/.env")?
    .service_name("my-codex-app")
    .install()?;
```

Common supported environment variables:

- `RUST_LOG` / `CODEX_SDK_LOG`
- `OTEL_SDK_DISABLED`
- `OTEL_SERVICE_NAME`
- `OTEL_SERVICE_VERSION`
- `OTEL_RESOURCE_ATTRIBUTES`
- `OTEL_EXPORTER_OTLP_ENDPOINT`
- `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`
- `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`
- `OTEL_EXPORTER_OTLP_LOGS_ENDPOINT`
- `OTEL_EXPORTER_OTLP_PROTOCOL`
- `OTEL_EXPORTER_OTLP_HEADERS`
- `CODEX_SDK_OTEL_RUNTIME_METRICS`

If your application already installed a global `tracing_subscriber`:

```rust,no_run
let _observability = Observability::builder()
    .with_env()?
    .install_subscriber(false)
    .build_provider()?;
```

Manual exporter configuration:

```rust,no_run
let _observability = Observability::builder()
    .service_name("my-codex-app")
    .otlp_http("http://localhost:4318/v1/traces")
    .runtime_metrics(true)
    .install()?;
```

## MCP And Skills

The SDK does not directly install MCP servers or skills. It uses the results of
native Codex configuration loading.

Put MCP configuration in `CODEX_HOME/config.toml` or a trusted project
`.codex/config.toml`:

```toml
[mcp_servers.pg]
command = "python3.12"
args = ["/path/to/pg-mcp-server/main.py"]
env_vars = ["DATABASE_URL"]
default_tools_approval_mode = "prompt"
startup_timeout_sec = 20
tool_timeout_sec = 60
```

Put skills in Codex-supported locations, for example:

```text
~/.agents/skills/ppt-master/SKILL.md
repo/.agents/skills/ppt-master/SKILL.md
```

When calling Codex, explicitly mention `$skill-name` in the prompt or rely on
Codex selecting a skill based on its description.

## Web/HTTP Application Recommendations

Recommended structure:

- Create one shared `Codex` when the process starts.
- Create one `Thread` for each browser/business session.
- Create one `TurnStream` for each user input.
- Convert `TurnEvent` values into SSE/WebSocket events.
- When a `ServerRequest` arrives, send the request id and native request content
  to the frontend. After the frontend approves or rejects it, send the request
  id back to the backend, then call `Codex::resolve_server_request()` or
  `Codex::reject_server_request()`.
- On Ctrl-C or service shutdown, close SSE/WebSocket streams and call
  `codex.shutdown().await`.

Do not start a new `Codex` runtime for every user session unless you really need
to isolate `CODEX_HOME`, providers, MCP, or local state.

## Shutdown

```rust,no_run
codex.shutdown().await?;
```

`ObservabilityGuard` tries to flush on drop. If you need deterministic flushing:

```rust,no_run
observability.shutdown();
```

Long-lived connection services should wire process shutdown signals into
SSE/WebSocket handling; otherwise graceful shutdown may wait forever for
infinite streams to end naturally.

## Error Handling

The SDK error type is `codex_sdk::Error`:

- `Config`: Codex config failed to load.
- `RuntimeStart`: app-server runtime failed to start.
- `RuntimeClosed`: runtime/event stream closed.
- `RuntimeTask`: runtime background task failed during shutdown.
- `Protocol`: JSON-RPC/app-server protocol error.
- `Timeout`: synchronous turn collection timed out.
- `TurnFailed`: turn failed.
- `Approval`: server request resolve/reject failed.
- `Observability`: OTel/tracing initialization failed.

Server-side APIs should map these errors to structured error responses while
preserving `thread_id`, `turn_id`, and request id in logs.

## Configuration Layering Recommendations

Where to put configuration:

- SDK builder: overrides explicitly owned by this process, such as
  `client_name`, `cwd`, and default sandbox.
- SDK builder: also a good place for process-wide base/developer instructions
  and default reasoning/prompt-size policy such as `reasoning_effort`,
  `reasoning_summary`, and `minimal_prompt_context()`.
- `CODEX_HOME/config.toml`: long-lived user/service defaults, such as model
  provider, MCP, OTel, and auth store.
- Project `.codex/config.toml`: repo-scoped sandbox, MCP, hooks, and model
  instructions.
- `.env`: deployment environment variables, OTEL endpoint, and provider API key
  values.
- Prompt/thread options: local overrides for this request.

With this layering, long-lived configuration is not hard-coded into application
code when upgrading the SDK or the underlying Codex tag.
