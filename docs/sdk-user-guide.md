# codex-sdk-rs User Guide

This guide is for application developers using `codex-sdk-rs`. You do not need
to work directly with the Codex app-server JSON-RPC protocol, `AppServerClient`,
`InProcessAppServerClient`, or Codex internal crates; the SDK provides a stable
Rust API:

- `Codex` / `CodexEventStream`: a shared Codex runtime and its single stream of
  runtime-scoped events.
- `Thread` / `ThreadEventStream`: a conversation and its single long-lived
  stream of events across all turns.
- `TurnBuilder` / `TurnHandle`: one user request and optional active-turn
  control.
- `ServerRequest`: native Codex requests, such as command approvals and MCP
  elicitations.
- `Observability`: the entry point for tracing / OpenTelemetry initialization.

## Minimal Example

```rust,no_run
use codex_sdk::prelude::*;
use tokio_stream::StreamExt;

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

        let thread = codex
            .thread()
            .cwd(std::env::current_dir()?)
            .start()
            .await?;
        let mut events = thread.event_stream()?;
        let turn = thread
            .turn("Summarize this repository in one paragraph.")
            .start()
            .await?;
        let turn_id = turn.turn_id().to_string();

        while let Some(event) = events.next().await {
            println!("{event:?}");
            if matches!(
                event,
                AppServerEvent::ServerNotification(
                    ServerNotification::TurnCompleted(ref done)
                ) if done.turn.id == turn_id
            ) {
                break;
            }
        }
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

`CodexRemoteBuilder::default_thread_params(...)` can copy one complete native
request into new thread builders. Higher-level configuration profiles stay on
the in-process builders, where their relationship to the locally resolved
Codex configuration is explicit.

## Cargo Dependency

Example path dependency inside this repository:

```toml
[dependencies]
codex-sdk-rs = { path = "../codex-sdk-rs" }
anyhow = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
tokio-stream = "0.1"
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

## Typical Native `config.toml` Recipes

Native Codex configuration is useful for durable runtime policy that should
also apply outside this SDK. Put personal or service-wide defaults in
`CODEX_HOME/config.toml`; put trusted repository overrides in
`.codex/config.toml`. Project configuration is ignored for untrusted projects.

The examples below are intentionally small. Copy one as a starting point, then
add only the settings the deployment owns. TOML root keys must appear before
tables such as `[sandbox_workspace_write]`, and secrets should come from the
environment rather than being written into the file. See the official
[configuration basics](https://learn.chatgpt.com/docs/config-file/config-basic)
and [configuration reference](https://learn.chatgpt.com/docs/config-file/config-reference)
for the complete schema.

### Interactive Coding Agent

A balanced default for a developer working interactively in a repository:

```toml
model_reasoning_effort = "high"
model_reasoning_summary = "auto"
personality = "pragmatic"

approval_policy = "on-request"
approvals_reviewer = "user"
sandbox_mode = "workspace-write"
web_search = "cached"

[sandbox_workspace_write]
network_access = false
```

This lets Codex edit the workspace while asking before it needs to cross the
sandbox boundary. Cached web search does not grant shell commands outbound
network access; enable `sandbox_workspace_write.network_access` separately only
when command-line tools need it.

### Read-only Review Agent

For source inspection, design review, or audit jobs that must not modify the
workspace:

```toml
model_reasoning_effort = "high"
model_reasoning_summary = "auto"

approval_policy = "never"
sandbox_mode = "read-only"
web_search = "disabled"
```

With `approval_policy = "never"`, work that requires broader permission fails
instead of producing an approval prompt. This is useful for unattended review
jobs where a write or network fallback would be surprising.

### Unattended Agent In An Isolated Worker

For CI or a disposable worker where writes inside the checked-out workspace are
expected, but outbound network and most inherited credentials are not:

```toml
model_reasoning_effort = "medium"
model_reasoning_summary = "none"

approval_policy = "never"
sandbox_mode = "workspace-write"
web_search = "disabled"

[sandbox_workspace_write]
network_access = false

[shell_environment_policy]
inherit = "core"
exclude = ["AWS_*", "AZURE_*", "GITHUB_TOKEN"]
```

Keep the outer worker or container isolated as well. `workspace-write` limits
Codex-launched work; it is not a replacement for deployment-level isolation.

### Chat-oriented Runtime Baseline

For an application using Codex as a conversational model rather than a coding
agent:

```toml
model_reasoning_effort = "low"
model_reasoning_summary = "none"
model_verbosity = "low"

approval_policy = "never"
sandbox_mode = "read-only"
web_search = "disabled"
project_doc_max_bytes = 0
```

This native baseline disables project instructions and web search, makes
permission escalation fail without prompting, and prevents local workspace
writes. It does not remove shell or other tool schemas, so `config.toml` alone
does not guarantee an empty prompt or tool list. In an in-process SDK
integration, also call `pure_chat_profile()`; it applies the prompt-context,
tool-family, and environment overrides that match the Codex revision pinned by
this crate. Supply the chatbot role with `base_instructions(...)` as shown in
[Pure Chatbot Configuration](#pure-chatbot-configuration).

### Runtime With An MCP Knowledge Server

For a read-oriented agent backed by a remote knowledge service:

```toml
approval_policy = "on-request"
sandbox_mode = "read-only"

[mcp_servers.knowledge]
url = "https://mcp.example.com/mcp"
bearer_token_env_var = "KNOWLEDGE_MCP_TOKEN"
enabled_tools = ["search", "fetch"]
default_tools_approval_mode = "prompt"
startup_timeout_sec = 20
tool_timeout_sec = 60
```

The bearer token value stays in `KNOWLEDGE_MCP_TOKEN`; the TOML contains only
the environment-variable name. Replace the URL and tool names with those
advertised by the actual server. `sandbox_mode = "read-only"` constrains local
commands and files, not operations performed by a remote MCP server. The MCP
boundary in this example comes from `enabled_tools` plus
`default_tools_approval_mode = "prompt"`.

Explicit SDK builder calls remain the process-owned override layer. For example,
calling `.default_sandbox(...)` or `.reasoning_effort(...)` takes precedence for
new threads created by the in-process runtime. If these setters are omitted,
`ctx.builder()` preserves the resolved native Codex values instead of replacing
them with SDK defaults. A remote `CodexRemoteBuilder` does not load local
`config.toml`; place these settings beside the remote app-server instead.

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
- `minimal_tools`: disables the configurable Codex tool families and discovery
  surfaces for each new thread. Core, environment-dependent, user-MCP, and
  dynamic tools can remain, so this is not an all-tools-off guarantee.
- `default_environment_access`: chooses whether new threads inherit Codex's
  default environment, disable environment access, or select explicit environments.
- `pure_chat_profile`: disables optional prompt context, project-document
  loading, known configurable tools, and environment access for a chat-oriented
  default.
- `default_thread_config_overrides`: merges native dotted config keys into every
  `thread/start`; use this escape hatch for less common current Codex settings.
- `default_sandbox`: explicit sandbox override; if omitted, native Codex config
  and trust defaults are preserved.
- `default_approval_policy`: explicit approval-policy override; if omitted,
  native Codex config and trust defaults are preserved.
- `ephemeral`: whether sessions are ephemeral by default.
- `channel_capacity`: convenience setting for both the upstream app-server
  queues and each SDK event-stream queue.
- `app_server_channel_capacity`: independently sizes upstream transport event
  and command queues.
- `event_stream_capacity`: independently sizes each live SDK event queue.
  Transcript, completion, and `ServerRequest` events apply backpressure;
  best-effort progress events may be replaced by a `Lagged` marker. These
  settings do not configure the fixed pre-attachment limits.

The embedded app-server resolves config again for every new thread. Runtime
defaults are snapshotted from the startup `Config` into `thread/start`, while
prompt-context, tool-profile, and other thread-only setters are recorded directly
as ordered `thread/start` overrides. Changing the working directory therefore
cannot silently restore a file-config value over an SDK builder default.

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
are code-layer overrides on `CodexBuilder`. With `builder_with_config(config)`,
common resolved values are projected automatically; use
`default_thread_config_overrides(...)` for other direct thread-scoped mutations.

### Pure Chatbot Configuration

Codex can also host a conversational assistant that is not intended to work on
code. Replace the native Codex base instructions with the chatbot's role and
disable the optional coding-oriented context blocks:

```rust,no_run
let codex = ctx
    .builder()
    // Keep user configuration and project instructions out of this runtime.
    .codex_home("/srv/my-chatbot/codex-home")
    .cwd("/srv/my-chatbot/workdir")
    .base_instructions(
        "You are a friendly customer-support assistant. \
         Answer from the conversation and do not perform software-development tasks.",
    )
    .developer_instructions(
        "Keep answers concise. Ask one clarifying question when the request is ambiguous.",
    )
    .pure_chat_profile()
    .default_sandbox(SandboxMode::ReadOnly)
    .ephemeral(true)
    .start()
    .await?;

let thread = codex.thread().start().await?;
let mut events = thread.event_stream()?;

let _turn = thread.turn("How do I change my delivery address?").start().await?;
// Consume `events` until this turn emits `TurnCompleted` before starting the
// customer's next turn. Keep the same thread to preserve chat history.
```

`base_instructions(...)` replaces, rather than appends to, the model-specific
Codex base prompt. Passing an empty string removes that base prompt entirely,
but a chatbot normally supplies its own role as shown above. Base and developer
instructions belong to the thread configuration: set them on `CodexBuilder` as
defaults, on `ThreadBuilder` before creating a reusable thread, or on
`CodexTurnBuilder` when creating a one-turn temporary thread. A later
`turn/start` on an existing thread cannot replace its base instructions.

`pure_chat_profile()` bundles the settings needed for a chat-oriented thread:

- `minimal_prompt_context()` disables permission, app, collaboration-mode,
  environment, and skill instruction blocks;
- `minimal_tools()` disables the configurable tool families and plugin/app
  discovery described below;
- `project_doc_max_bytes = 0` disables `AGENTS.md` and fallback project-document
  loading;
- `default_environment_access(EnvironmentAccess::Disabled)` sends
  `thread/start.environments = []`, preventing selection of the default local
  environment and removing environment-dependent tools such as `apply_patch`
  and `view_image`.

These changes are recorded as ordered thread defaults, so a later builder call
can deliberately re-enable an individual setting. `minimal_prompt_context()` by
itself deliberately does not remove:

- conversation history or the current user message;
- tool schemas exposed by Codex;
- `AGENTS.md` discovered from the configured working directory;
- enabled MCP servers, plugins, memories, or other extension-provided context.

`minimal_tools()` applies these native settings. They are grouped here by the
capability they remove:

| Setting | Effect when disabled |
| --- | --- |
| `features.apps` | Removes the ChatGPT app/connector surface and prevents app connectors from becoming model tools. |
| `features.plugins` | Stops loading plugin-provided skills, MCP servers, and tools; this also removes generic `plugins_instructions` when no selected plugin remains. |
| `features.tool_suggest` | Removes plugin/app discovery recommendations, the `recommended_plugins` context block, and `request_plugin_install`. It normally requires both apps and plugins. |
| `orchestrator.skills.enabled` | Hides skills supplied by the host/orchestrator. It does not disable ordinary filesystem skills by itself. |
| `orchestrator.mcp.enabled` | Hides the orchestrator-owned `codex_apps` MCP server. User-configured MCP servers are unaffected. |
| `features.shell_tool`, `features.code_mode`, `features.code_mode_only` | Removes shell execution and JavaScript code-mode entry points such as `exec_command`, `write_stdin`, or code-mode wrappers. |
| `features.multi_agent`, `features.multi_agent_v2`, `features.enable_fanout` | Removes sub-agent collaboration and fanout/job tools. |
| `features.image_generation` | Removes the model-visible image-generation namespace. |
| `features.memories` | Disables the memory extension, including memory context and any dedicated memory tools. |
| `features.goals` | Disables the goal extension and its model-visible goal tools. |
| `features.deferred_executor` | Removes the tool used to wait for a deferred environment executor. |
| `features.request_permissions_tool` | Removes the model-visible permission-request tool. This is separate from sandbox/approval policy. |
| `features.token_budget` | Removes context-window/token-budget utility tools. |
| `features.current_time_reminder` | Removes current-time and configured sleep utility tools. |
| `features.standalone_web_search`, `web_search = "disabled"` | Disables extension-backed and hosted web-search surfaces. |
| `tools.experimental_request_user_input.enabled` | Removes `request_user_input`. |

Call `default_thread_config_overrides(...)` after `minimal_tools()` to re-enable
one selected config capability. Thread-default builder calls are applied in
order, including complete `default_thread_params(...)` replacements. Environment
access is a protocol field rather than a config override; set it with
`default_environment_access(...)` on an in-process runtime builder or
`environment_access(...)` on a thread/turn builder.

`EnvironmentAccess` preserves the protocol's three states: `Inherit` omits the
field, `Disabled` sends an empty list, and `Selected(...)` sends explicit
environment IDs and working directories. On a new thread, `Inherit` selects the
Codex default; on an existing thread's turn, it keeps the thread's sticky value.

For a predictable chat-only deployment, use a dedicated `codex_home` without
unneeded MCP/plugin/memory configuration and a dedicated `cwd` without project
instructions; both directories must already exist. `SandboxMode::ReadOnly`
limits filesystem mutation, but neither it nor prompt text is a hard
tool-disable policy. The current high-level SDK
does not provide an all-tools-off switch; applications that must guarantee that
no Codex tools are exposed need native configuration/tool-policy support or a
direct model API intended for tool-free chat.

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
`Config`, plus native thread-default escape hatches. The SDK takes an explicit
snapshot of common effective values—such as `cwd`, model/provider, service tier,
personality, sandbox or named permissions, approval policy/reviewer, reasoning,
ephemeral state, instructions, and prompt-context switches—and forwards it
through `thread/start` automatically.

The app-server still reloads `CODEX_HOME` and project config layers for each
thread. A resolved `Config` does not retain enough provenance to convert every
possible direct field mutation back into TOML overrides. Carry uncommon
thread-scoped in-memory mutations explicitly with dotted native config keys:

```rust,no_run
use std::collections::HashMap;
use serde_json::json;

let codex = ctx
    .builder_with_config(config)
    .default_thread_config_overrides(HashMap::from([
        ("features.plugins".to_string(), json!(false)),
        ("project_doc_max_bytes".to_string(), json!(0)),
    ]))
    .start()
    .await?;
```

Use `default_thread_params(ThreadStartParams)` when replacing the complete
native default request is preferable. Per-thread `thread().params(...)` and
per-turn native params remain available for narrower overrides.

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
- Trusted automation without an interactive approval channel: `Never`; consume
  the thread stream until the matching `TurnCompleted` and handle error events
  explicitly.
- More conservative interactive workflows: `UnlessTrusted`.

Because this is the native Codex type, granular policy can be passed directly
with `AskForApproval::Granular`; long-lived defaults can also remain in
`config.toml`.

## Temporary-thread Turns

`codex.turn(input)` is a convenience path: it creates a temporary thread, starts
one turn, and returns a `TurnHandle`. `input` can be `&str` / `String` or native
`Vec<UserInput>`. Take the long-lived event stream from the handle's owning
thread.

```rust,no_run
let turn = codex
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
    .start()
    .await?;
let mut events = turn.thread().event_stream()?;
```

For multimodal or lower-level input, pass native `UserInput` directly:

```rust,no_run
let turn = codex
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
    .start()
    .await?;
let mut events = turn.thread().event_stream()?;
```

The temporary thread remains accessible through `turn.thread()` and can be
resumed later when it is not configured as ephemeral.

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
let mut events = thread.event_stream()?;

let first = thread
    .turn("Draft a repair plan first.")
    .start()
    .await?;
while let Some(event) = events.next().await {
    if matches!(
        event,
        AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(ref done))
            if done.turn.id == first.turn_id()
    ) {
        break;
    }
}

let second = thread
    .turn("Implement the first step from that plan.")
    .start()
    .await?;
```

The SDK does not serialize turns for a thread ID. Applications must wait for
the previous turn's `TurnCompleted` event, or otherwise coordinate access,
before starting another turn on the same thread ID. Keep the same
`ThreadEventStream` for the thread's entire lifetime; `TurnCompleted` does not
end it. Dropping the stream only stops local consumption and does not interrupt
the server turn. Different thread IDs can run concurrently.

Saved threads can be resumed, forked, listed, archived, and unarchived through
the shared `Codex` handle:

```rust,no_run
let resumed = codex.resume_thread("thread-id").await?;
let forked = resumed.fork().await?;
let _page = codex.list_threads().await?;

forked.set_name("investigation").await?;
let forked_id = forked.id().to_string();
let mut forked_events = forked.event_stream()?;
forked.archive().await?;
while let Some(event) = forked_events.next().await {
    if matches!(
        event,
        AppServerEvent::ServerNotification(ServerNotification::ThreadArchived(_))
    ) {
        break;
    }
}
let restored = codex.unarchive_thread(forked_id).await?;
let _snapshot = restored.read(true).await?;
```

`ThreadArchived` is delivered as the old stream's final event. A successful
`archive()` response establishes that local boundary before it returns, so an
immediate `unarchive_thread(...)` is safe even when the upstream notification is
still queued. Unarchive creates a new `Thread` attachment with a new long-lived
stream. `ThreadDeleted` and `ThreadClosed` are terminal when their notifications
are observed.

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
use tokio_stream::StreamExt;

let mut stream = thread.event_stream()?;
let turn = thread
    .turn("Run the pg MCP smoke test.")
    .approval_policy(AskForApproval::OnRequest)
    .start()
    .await?;
let turn_id = turn.turn_id().to_string();

while let Some(event) = StreamExt::next(&mut stream).await {
    match event {
        AppServerEvent::ServerNotification(
            ServerNotification::AgentMessageDelta(delta),
        ) if delta.turn_id == turn_id => {
            print!("{}", delta.delta);
        }
        AppServerEvent::ServerNotification(
            ServerNotification::TurnCompleted(done),
        ) if done.turn.id == turn_id => {
            eprintln!("turn completed: {:?}", done.turn.status);
            break;
        }
        AppServerEvent::ServerRequest(request) => {
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

Steering and interruption stay on the `TurnHandle`, independently of the
thread's event stream:

```rust,no_run
use tokio_stream::StreamExt;

let mut stream = thread.event_stream()?;
let turn = thread.turn("Count from 1 to 100.").start().await?;
turn.steer("Stop after 10 numbers.").await?;

while let Some(event) = StreamExt::next(&mut stream).await {
    if matches!(
        event,
        AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(ref done))
            if done.turn.id == turn.turn_id()
    ) {
        break;
    }
}
```

`TurnCompleted` is a per-turn boundary only. Continue polling the same
`ThreadEventStream` for later turns.

Common events:

- `ServerNotification`: native Codex app-server notifications, such as
  `AgentMessageDelta`, `TurnCompleted`, `Error`, token usage, and so on.
- `ServerRequest`: requests that the application needs to answer.
- `Lagged`: the upstream source, bounded pre-attachment buffer, or a full live
  event queue skipped best-effort messages.
- `Disconnected`: the runtime connection closed.

`AppServerEvent` is the native Codex app-server client event type. Its protocol
payloads are native `ServerNotification` / `ServerRequest` values, so Codex
protocol field and variant changes are exposed directly at compile time.

The runtime routes each thread-scoped event directly to its owning
`ThreadEventStream`; traffic from other threads does not consume that stream's
queue. Events that arrive before `thread/start`, resume, fork, or unarchive
returns are retained in a fixed 32,768-event per-thread pre-attachment buffer
and replayed in order. At most 1,024 inactive or unattached thread routes are
retained; the oldest route is evicted when that limit is exceeded. Neither
`channel_capacity()` nor `event_stream_capacity()` configures these fixed
limits. Event overflow removes the oldest event and prepends `Lagged` when the
stream attaches; in an extreme pre-attachment burst this can remove a reliable
event.
Live queues preserve transcript deltas, completion notifications, and all
`ServerRequest` values by applying backpressure. Progress-style events are
best-effort under pressure; the next deliverable event is preceded by `Lagged`
when any were skipped. Consume active streams continuously so one full reliable
queue does not pause the shared app-server event pump.
The SDK does not synthesize a batch result or hide native turn status; inspect
`TurnCompleted` directly.

Events without a thread target belong to the shared runtime. Take the one
runtime stream when the application needs account/config notifications,
threadless server requests, lag reports, or disconnect handling:

```rust,no_run
use tokio_stream::StreamExt;

let mut runtime_events = codex.event_stream()?;
while let Some(event) = runtime_events.next().await {
    tracing::debug!(?event, "runtime event");
}
```

## Handling ServerRequest

`ServerRequest` appears for command approvals, file change approvals, permission
requests, MCP elicitations, and similar cases. If the application does not
answer, the turn may wait.

```rust,no_run
match event {
    AppServerEvent::ServerRequest(request) => {
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

The pinned app-server client currently serializes a server-request response
write with reads from the same event source. The SDK bounds the complete queue
and write operation to 30 seconds; a stalled write can pause event forwarding
until it completes or times out.

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
the matching `ThreadEventStream`. Requests without a thread target are delivered
once through `CodexEventStream`. Both stream types expose the same resolve,
approve, and reject helpers.

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

let turn = thread.turn_params(params).start().await?;
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

Streaming callers own timeout policy. A turn timeout should not discard the
thread's long-lived stream; call `turn.interrupt().await` when cancellation is
wanted and keep consuming thread events.

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

let turn = codex
    .turn("Summarize the risks in the current repository.")
    .approval_policy(AskForApproval::Never)
    .output_schema(schema)
    .start()
    .await?;
let turn_id = turn.turn_id().to_string();
let mut events = turn.thread().event_stream()?;

let mut response = String::new();
while let Some(event) = events.next().await {
    match event {
        AppServerEvent::ServerNotification(
            ServerNotification::AgentMessageDelta(delta),
        ) if delta.turn_id == turn_id => response.push_str(&delta.delta),
        AppServerEvent::ServerNotification(ServerNotification::TurnCompleted(done))
            if done.turn.id == turn_id => break,
        _ => {}
    }
}
let summary: ReviewSummary = serde_json::from_str(&response)?;
```

The model returns structured content through assistant-message events. The SDK
sends the schema but deliberately leaves message selection and deserialization
to the application instead of inventing a second batch result model.

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

### Inspect Model-Visible Requests

OpenTelemetry spans are not a request-body capture: they do not contain the
complete request Codex assembled for the model. When debugging prompt assembly,
tool exposure, or conversation history, enable Codex's local rollout trace
before starting the process that hosts the Codex runtime:

```sh
mkdir -p /tmp/codex-rollout-traces
CODEX_ROLLOUT_TRACE_ROOT=/tmp/codex-rollout-traces \
  cargo run --example minimal
```

For a remote SDK client, set `CODEX_ROLLOUT_TRACE_ROOT` on the remote app-server
process, not on the client. Each independent root thread creates a
`trace-<trace-id>-<thread-id>` bundle containing `manifest.json`, `trace.jsonl`,
and referenced JSON files under `payloads/`.

List every model inference attempt and its request payload:

```sh
bundle="$(ls -dt /tmp/codex-rollout-traces/trace-* | head -n 1)"
jq -r '
  select(.payload.type == "inference_started")
  | [.payload.codex_turn_id, .payload.inference_call_id,
     .payload.request_payload.path]
  | @tsv
' "$bundle/trace.jsonl"
```

For example, inspect the most recent inference attempt:

```sh
request_path="$(
  jq -r '
    select(.payload.type == "inference_started")
    | .payload.request_payload.path
  ' "$bundle/trace.jsonl" | tail -n 1
)"
jq . "$bundle/$request_path"
```

The payload shows the model-visible request, including fields such as the model,
instructions, conversation input, tools, reasoning settings, and text/output
configuration. Retries and transport fallback can create more than one
`inference_started` event for a turn, so inspect the matching inference attempt
rather than assuming there is only one request.

This is semantic request capture, not a raw network capture. For the normal HTTP
transport it is usually the exact provider request. When WebSocket reuse omits
already-sent input, the trace can instead store the complete logical request the
model sees, rather than the incremental bytes sent on that WebSocket message.
It does not capture authentication headers or compressed wire bytes.

Rollout traces are separate from OTel and are never uploaded by Codex. They are
also highly sensitive: a bundle can contain prompts, responses, tool
inputs/outputs, terminal output, and local paths. Do not write bundles into the
repository or enable them broadly in production; restrict access and remove
them when the investigation is complete.

For a reduced semantic graph in addition to the raw payloads, run:

```sh
codex debug trace-reduce "$bundle"
```

This writes `state.json` inside the bundle; the original inference requests
remain in `payloads/`.

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
- Take its optional `CodexEventStream` once when the application handles
  runtime-scoped notifications or threadless requests.
- Create one `Thread` for each browser/business session.
- Take one `ThreadEventStream` and keep it connected for that thread's lifetime.
- Start a `Turn` for each user input; route its events by `turn_id` without
  replacing the thread stream.
- Convert `AppServerEvent` values into SSE/WebSocket events.
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

Dropping the final `Codex`, `Thread`, `TurnHandle`, `ThreadEventStream`, or
`CodexEventStream` also signals runtime shutdown. Call `shutdown()` explicitly
when the service needs to await bounded cleanup and observe shutdown errors.

`ObservabilityGuard` tries to flush on drop. If you need deterministic flushing:

```rust,no_run
observability.shutdown();
```

Long-lived connection services should still wire process shutdown signals into
their SSE/WebSocket handling so downstream clients are closed deliberately.

## Error Handling

The SDK error type is `codex_sdk::Error`:

- `Config`: Codex config failed to load.
- `RuntimeStart`: app-server runtime failed to start.
- `Arg0DispatchRequired`: in-process startup was attempted without helper paths
  from `run_main` or an explicit `arg0_paths(...)` call.
- `RuntimeClosed`: runtime/event stream closed.
- `CodexEventStreamTaken`: the runtime's single global event stream was already
  taken from this handle or one of its clones.
- `ThreadEventStreamTaken`: the thread's single event stream was already taken
  from this handle or one of its clones.
- `ThreadLifecycleInProgress`: resume or unarchive already owns the route for
  that thread id.
- `TemporaryTurnStart`: `Codex::turn(...)` created its temporary thread but the
  first `turn/start` failed; `temporary_thread()` exposes the still-attached
  `Thread`, and `into_temporary_turn_failure()` returns it with the root error.
- `InvalidThreadId`: a thread id could not be parsed as a native Codex thread id.
- `RuntimeTask`: runtime background task failed during shutdown.
- `RuntimeShutdown` / `RuntimeShutdownTimeout` / `RuntimeShutdownFailed`:
  bounded runtime cleanup failed, including a failure remembered by a later
  idempotent shutdown call.
- `Protocol`: JSON-RPC/app-server protocol error.
- `Approval` / `ServerRequestResponseTimeout`: server request response failed.
- `Observability`: OTel/tracing initialization failed.

Server-side APIs should map these errors to structured error responses while
preserving `thread_id`, `turn_id`, and request id in logs.

## Configuration Layering Recommendations

Where to put configuration:

- SDK builder: overrides explicitly owned by this process, such as
  `client_name`, `cwd`, and an explicitly selected default sandbox.
- SDK builder: also a good place for process-wide base/developer instructions
  and default reasoning/prompt-size policy such as `reasoning_effort`,
  `reasoning_summary`, or the composed `pure_chat_profile()`.
- `CODEX_HOME/config.toml`: long-lived user/service defaults, such as model
  provider, MCP, OTel, and auth store.
- Project `.codex/config.toml`: repo-scoped sandbox, MCP, hooks, and model
  instructions.
- `.env`: deployment environment variables, OTEL endpoint, and provider API key
  values.
- Prompt/thread options: local overrides for this request.

With this layering, long-lived configuration is not hard-coded into application
code when upgrading the SDK or the underlying Codex tag.
