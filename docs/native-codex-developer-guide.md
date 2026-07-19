# Native Codex Integration And Configuration Developer Guide

This guide is for `codex-sdk-rs` maintainers and developers who need to
understand lower-level Codex behavior. It explains how the native Codex
app-server starts, where configuration comes from, how file configuration and
code configuration are layered, and how these concepts map inside this crate.

This document is based on the Codex manual fetched on 2026-07-15. Official
entry points:

- Configuration basics: https://learn.chatgpt.com/docs/config-file/config-basic
- Advanced configuration: https://learn.chatgpt.com/docs/config-file/config-advanced
- Configuration reference: https://learn.chatgpt.com/docs/config-file/config-reference
- MCP: https://learn.chatgpt.com/docs/extend/mcp
- Skills: https://learn.chatgpt.com/docs/build-skills
- App-server: https://learn.chatgpt.com/docs/app-server
- Codex SDK: https://learn.chatgpt.com/docs/codex-sdk

## When To Use Native Codex

The native Codex app-server is suitable for rich clients or deep product
integrations: authentication, historical sessions, approvals, server requests,
streaming agent events, MCP, skills, hooks, telemetry, and related features all
live at this layer. The official guidance also distinguishes:

- Deep client integrations: use app-server.
- CI or automation tasks: prefer the official SDK or non-interactive mode.
- Codex as one specialist inside a larger agent orchestration: you can use the
  Codex CLI as an MCP server.

`codex-sdk-rs` currently uses the app-server protocol as its boundary: it can
embed the Codex Rust crates and start an in-process app-server, or connect to an
already-running remote WebSocket / Unix-socket app-server, then wrap the
lower-level JSON-RPC/app-server protocol in a stable Rust API.

## Native App-server Lifecycle

The app-server protocol is JSON-RPC 2.0 style, and wire messages usually omit
`"jsonrpc":"2.0"`.

Typical lifecycle:

1. Start the app-server runtime.
2. After the connection is established, send `initialize`, then send the
   `initialized` notification.
3. Call `thread/start` to create a thread.
4. Call `turn/start` to start a turn on the thread.
5. Continuously read server notifications and server requests.
6. The turn is complete after `turn/completed`.
7. Before process exit, close the client/runtime and flush telemetry.

Core concepts:

- `Thread`: a Codex session container that preserves context and later turns.
- `Turn`: one user request and the agent work for it.
- `Item`: an input/output unit inside a turn, such as a user message, agent
  message, command execution, file change, or tool call.
- `ServerNotification`: events pushed by the server, such as agent deltas, turn
  completion, and token usage.
- `ServerRequest`: requests that need a client response, such as command
  approval, file-change approval, or MCP elicitation.

## Transport Forms

The official app-server supports these transports:

- `stdio://`: default, JSONL.
- `ws://` / `wss://`: WebSocket, experimental.
- `unix://`: WebSocket upgrade over a Unix socket.
- `off`: do not expose a local transport.

This crate currently uses `codex_app_server_client::InProcessAppServerClient`,
which starts the app-server worker in the same process instead of using an
external stdio/ws transport. Deployment is simpler, while events and requests
still keep the app-server protocol model.

## Codex Configuration Sources

Codex configuration is layered, not a single file.

Official configuration priority from highest to lowest:

1. CLI flags and one-off `--config` overrides.
2. Project `.codex/config.toml`, loaded from the project root down to the
   current working directory, with the closest layer to `cwd` taking priority.
3. Profile files, such as `~/.codex/deep-review.config.toml`, selected with
   `--profile deep-review`.
4. User config: `~/.codex/config.toml`.
5. System config: usually `/etc/codex/config.toml` on Unix.
6. Codex built-in defaults.

Notes:

- Project `.codex/config.toml` is loaded only when the project is trusted.
- Project config cannot override high-risk or machine-local configuration such
  as credentials, providers, or telemetry.
- Relative paths are usually resolved relative to the containing `.codex/`
  directory or according to the semantics of the current layer.
- Enterprise/managed machines may also enforce configuration through
  `requirements.toml`.

## Important Directories And Environment Variables

`CODEX_HOME` is the most important root directory, defaulting to `~/.codex`. It
stores:

- `config.toml`
- `auth.json` or keychain/keyring credentials
- logs, history, sessions, skills, and standalone package metadata
- MCP OAuth credentials and related state

Common environment variables:

- `CODEX_HOME`: overrides the Codex state/config root. The directory must
  already exist.
- `CODEX_SQLITE_HOME`: overrides the SQLite state location; `sqlite_home`
  config takes priority.
- `CODEX_ACCESS_TOKEN`: ChatGPT/Codex access token for trusted automation
  scenarios.
- `CODEX_API_KEY`: mainly used in the official docs for one-off non-interactive
  `codex exec` runs.
- `CODEX_CA_CERTIFICATE` / `SSL_CERT_FILE`: enterprise TLS CA.
- `RUST_LOG`: Rust log filtering.

Provider API keys are usually not fixed Codex environment variables. Instead,
the provider configuration specifies the variable name through `env_key`.

## Common config.toml Configuration

### Model And Provider

```toml
model = "gpt-5.x"
model_provider = "openai"
```

If you only need to point the built-in OpenAI provider at a proxy or data
residency base URL, prefer:

```toml
openai_base_url = "https://us.api.openai.com/v1"
```

Custom provider example:

```toml
model = "gpt-5.x"
model_provider = "proxy"

[model_providers.proxy]
name = "OpenAI through proxy"
base_url = "https://proxy.example.com/v1"
env_key = "OPENAI_API_KEY"
wire_api = "responses"
```

Providers can configure static headers, environment-variable headers, or
command-backed auth.

### Reasoning, Verbosity, And Context Limits

```toml
model_reasoning_effort = "high"
model_reasoning_summary = "auto"
model_verbosity = "medium"
model_context_window = 128000
model_auto_compact_token_limit = 64000
tool_output_token_limit = 12000
```

Not every provider/wire API supports every field. For example,
`model_verbosity` mainly applies to Responses API capable models.

### Approval Policy

```toml
approval_policy = "on-request"
```

Common values:

- `untrusted`: only known-safe read-only commands run automatically; everything
  else asks the user.
- `on-request`: the model asks for approval when needed.
- `never`: never asks for approval; higher risk.
- granular policy: finer-grained control by approval category.

Granular example:

```toml
approval_policy = { granular = {
  sandbox_approval = true,
  rules = true,
  mcp_elicitations = true,
  request_permissions = false,
  skill_approval = false
} }
```

### Sandbox

```toml
sandbox_mode = "workspace-write"
```

Common modes:

- `read-only`: read-only.
- `workspace-write`: allows reads and writes to the workspace plus configured
  writable roots.
- `danger-full-access`: disables the filesystem sandbox; use only when the host
  environment is already isolated.

Workspace-write configuration:

```toml
[sandbox_workspace_write]
writable_roots = ["/Users/me/.cache/my-tool"]
network_access = false
exclude_tmpdir_env_var = false
exclude_slash_tmp = false
```

### Shell Environment Passing

```toml
[shell_environment_policy]
inherit = "none"
set = { PATH = "/usr/bin", MY_FLAG = "1" }
exclude = ["AWS_*", "AZURE_*"]
include_only = ["PATH", "HOME"]
ignore_default_excludes = false
```

This configuration controls which environment variables Codex-launched
shell/tool child processes can see. Production integrations should avoid
passing the full environment directly to tools that the model can trigger.

### MCP Servers

MCP is configured in `[mcp_servers.NAME]` tables in `config.toml`.

STDIO example:

```toml
[mcp_servers.context7]
command = "npx"
args = ["-y", "@upstash/context7-mcp"]
env_vars = ["LOCAL_TOKEN"]

[mcp_servers.context7.env]
MY_ENV_VAR = "MY_ENV_VALUE"
```

Streamable HTTP example:

```toml
[mcp_servers.pg]
url = "http://127.0.0.1:3000/mcp"
bearer_token_env_var = "PG_MCP_TOKEN"
startup_timeout_sec = 20
tool_timeout_sec = 60
default_tools_approval_mode = "prompt"
```

Approval-related settings:

- `default_tools_approval_mode = "auto" | "prompt" | "writes" | "approve"`
- `[mcp_servers.NAME.tools.TOOL].approval_mode = "..."`
- `enabled_tools` / `disabled_tools` can be used as tool allow/deny lists.

### Skills

Codex skills are directories containing `SKILL.md`. Codex initially puts only
the skill name/description/path into context, then reads the full `SKILL.md`
only when the skill is triggered.

Common locations:

- repo: `$REPO_ROOT/.agents/skills`
- user: `$HOME/.agents/skills`
- admin: `/etc/codex/skills`
- system: Codex built-ins

Skills can be triggered explicitly, for example by writing `$ppt-master` in the
prompt, or selected implicitly based on their descriptions.

Disable a skill:

```toml
[[skills.config]]
path = "/path/to/skill/SKILL.md"
enabled = false
```

### Hooks

Hooks can live in:

- `~/.codex/hooks.json`
- `~/.codex/config.toml`
- project `.codex/hooks.json`
- project `.codex/config.toml`

TOML example:

```toml
[[hooks.PreToolUse]]
matcher = "^Bash$"

[[hooks.PreToolUse.hooks]]
type = "command"
command = "/usr/local/bin/check-command"
timeout = 30
statusMessage = "Checking command"
```

Project-local hooks are also restricted by trusted project rules.

### OTel

Native Codex `[otel]` configuration controls Codex's own structured
log/trace/metrics export.

```toml
[otel]
environment = "staging"
exporter = "none"
log_user_prompt = false
```

OTLP HTTP example:

```toml
[otel]
exporter = { otlp-http = {
  endpoint = "https://otel.example.com/v1/logs",
  protocol = "binary",
  headers = { "x-otlp-api-key" = "${OTLP_TOKEN}" }
}}
```

## Native Configuration In Code

`codex-sdk-rs` currently uses these native Rust types internally:

- `codex_arg0::arg0_dispatch_or_else`
- `codex_arg0::Arg0DispatchPaths`
- `codex_core::config::ConfigBuilder`
- `codex_core::config::ConfigOverrides`
- `codex_app_server_client::AppServerClient`
- `codex_app_server_client::InProcessAppServerClient`
- `codex_app_server_client::InProcessClientStartArgs`
- `codex_app_server_client::RemoteAppServerClient`
- `codex_app_server_client::RemoteAppServerConnectArgs`
- `codex_app_server_protocol::ClientRequest`
- `codex_app_server_protocol::ServerNotification`
- `codex_app_server_protocol::ServerRequest`

Startup flow, roughly:

1. `run_main` initializes helper dispatch paths through `arg0_dispatch_or_else`.
2. `CodexBuilder` uses `ConfigBuilder` to load `CODEX_HOME`, user/project
   config, and profile results, then injects convenience SDK overrides through
   `ConfigOverrides`; callers that already have a complete native
   `codex_core::config::Config` use `CodexWithConfigBuilder`.
3. `RuntimeHandle::start` receives an already-resolved native `Config`; it no
   longer owns SDK-specific runtime options.
4. Build `ExecServerRuntimePaths` and `EnvironmentManager`.
5. Initialize state storage with `codex_core::init_state_db`.
6. Build `InProcessClientStartArgs` through `sdk_in_process_client_start_args`,
   keeping SDK-mode defaults such as loader context, feedback, and `log_db`
   inside the runtime.
7. Start app-server with
   `InProcessAppServerClient::start(InProcessClientStartArgs { ... })`.
8. Move the full `AppServerClient` into `AppServerDriver`, which exclusively
   owns event polling and shutdown. `RuntimeHandle` uses `AppServerHandle` for
   typed requests and server-request responses without sharing the event source.
9. Drive later work through typed `ClientRequest::ThreadStart` and
   `ClientRequest::TurnStart`.

Remote mode does not use local `ConfigBuilder` / `InProcessClientStartArgs`.
`CodexRemoteBuilder` only builds `RemoteAppServerConnectArgs`; config, auth,
tooling, sandbox, working directory, and other runtime defaults belong to the
remote app-server. The local SDK still uses the same `ClientRequest` /
`ServerNotification` / `ServerRequest` surface.

### ConfigBuilder And ConfigOverrides

Key points for code configuration overrides:

```rust,no_run
ConfigBuilder::default()
    .codex_home(codex_home)
    .harness_overrides(ConfigOverrides {
        cwd: Some(cwd),
        model,
        approval_policy: Some(approval_policy),
        sandbox_mode: Some(sandbox_mode),
        codex_self_exe: arg0_paths.codex_self_exe.clone(),
        codex_linux_sandbox_exe: arg0_paths.codex_linux_sandbox_exe.clone(),
        main_execve_wrapper_exe: arg0_paths.main_execve_wrapper_exe.clone(),
        ephemeral: Some(ephemeral),
        ..Default::default()
    })
    .build()
    .await?;
```

These overrides are similar to "one-off code-layer configuration" and take
priority over file defaults. They are appropriate for:

- The current process working directory.
- Service-enforced default sandbox/approval.
- The current client's model override.
- Helper executable paths.
- The default ephemeral session setting.

Do not put provider auth, MCP OAuth, team policy, or other long-lived state into
the default SDK builder. Those settings should continue to live in
`CODEX_HOME/config.toml`, project `.codex/config.toml`, or an enterprise
management layer.

If the caller already has a complete native `Config`, the SDK should accept it
through `Codex::builder_with_config(config)` /
`CodexMain::builder_with_config(config)`. `CodexWithConfigBuilder` should only
expose runtime options that are not part of `Config`, such as `client_name`,
`client_version`, and `channel_capacity`. For new configuration capabilities,
prefer exposing Codex native types over adding SDK-specific `Options` /
`Config` middle structures.

### InProcessClientStartArgs

`InProcessClientStartArgs` is the real app-server startup boundary. When
maintaining it, pay close attention to:

- `arg0_paths`: shell/apply_patch/sandbox helper locations.
- `config`: parsed Codex `Config`.
- `loader_overrides` / `cloud_config_bundle`: app-server config reload context;
  the SDK currently supplies defaults internally instead of exposing them as
  public runtime startup parameters.
- `feedback`: Codex feedback integration.
- `state_db` / `log_db`: local state and log databases.
- `environment_manager`: execution environment management.
- `config_warnings`: startup warnings passed to the app-server.
- `session_source`: marks the source of this client.
- `enable_codex_api_key_env`: whether to enable the environment variable API
  key entry point.
- `client_name` / `client_version`: identify the integration in observability
  and compliance logs.
- `experimental_api`: whether to enable experimental app-server fields/methods.
- `mcp_server_openai_form_elicitation`: whether to allow MCP OpenAI extended
  form elicitation.
- `channel_capacity`: event/request channel capacity.

## ServerRequest Handling Principles

The native app-server pushes approvals, MCP elicitations, permission requests,
and similar interactions as `ServerRequest`s. The client must answer them, or
the turn may hang.

Design principles:

- Streaming APIs should expose `ServerRequest` to callers.
- The SDK does not provide a synchronous collector; applications retain control
  of request handling while consuming the native event stream.
- The response shape must match the request type, such as approval decisions or
  MCP elicitation action/content.
- Request ids must come from the current client event stream.
- Events only carry native Codex request data; resolve/reject behavior that owns
  the runtime belongs on `Codex` or `ThreadEventStream` methods.
- Most requests should be routed precisely by thread/turn. Global requests that
  lack thread/turn ids may be seen by multiple active streams; applications
  should handle them idempotently by `RequestId`.

## Thread Concurrency Boundary

The native app-server allows multiple threads, and one app-server client can
manage multiple threads. Running multiple turns on the same thread at the same
time makes context ordering, server request ownership, and token usage more
complex.

`codex-sdk-rs` deliberately does not keep an active-turn registry or serialize
turns for callers:

- Different thread IDs can run concurrently.
- Applications must not start overlapping turns for the same thread ID unless
  they explicitly accept the app-server's behavior.
- Dropping a local `ThreadEventStream` does not interrupt an active server turn.
  It also permanently gives up local event consumption for that `Thread`
  handle, because the stream is unique across all of its clones.

This keeps the runtime free of duplicate turn-lifecycle state. Serialization is
an application-level contract, including across independently resumed `Thread`
handles for the same ID.

## Event Dispatch Boundary

Each thread exposes one long-lived
`codex_app_server_client::AppServerEvent` stream directly:

- `AppServerEvent::ServerNotification(codex_app_server_protocol::ServerNotification)`
- `AppServerEvent::ServerRequest(codex_app_server_protocol::ServerRequest)`
- `AppServerEvent::Lagged`
- `AppServerEvent::Disconnected`

Event filtering rules should prefer typed protocol fields such as `thread_id`,
`turn_id`, `thread.id`, and `turn.id`, instead of guessing from JSON `Value`
fields. When upgrading the Codex tag, a compile failure from a new
`ServerRequest` / `ServerNotification` variant is a useful signal: decide
explicitly whether it is turn-scoped, thread-scoped, or global.

Dispatch from the runtime to `ThreadEventStream` currently uses a Tokio
broadcast channel. A receiver is created before thread start/resume/fork or
unarchive, then retained by `Thread` until the application takes it. Slow
consumers receive `Lagged { skipped }`; this means the stream may have missed
deltas, requests, or `TurnCompleted`. `TurnCompleted` is a turn boundary, not a
stream boundary. `ThreadClosed` and runtime disconnection are stream terminal
events.

## Dependency Audit Boundary

`deny.toml` is not a general-purpose template. It describes the dependency
policy for the current SDK plus the current Codex tag. During maintenance,
distinguish three cases:

- Direct SDK dependencies can be upgraded or replaced: prefer changing
  `Cargo.toml` / `Cargo.lock`.
- Codex tag transitive dependencies that the SDK cannot safely replace: write
  narrow exceptions and reasons in `deny.toml`.
- Old exceptions no longer match: delete them so cargo-deny warnings are not
  drowned in noise.

For banned crates, prefer narrowing allowed paths with `bans.deny[].wrappers`
instead of deleting the ban. That way, if the SDK or a new upstream path
introduces a similar crate directly in the future, `cargo deny` still reports
it.

## Maintenance Guidelines

- Prefer keeping durable configuration in native Codex configuration layers;
  do not copy every config key into the SDK API.
- SDK APIs should prefer passing through native Codex protocol params; common
  fields can also have convenience setters.
- The event layer should expose native `ServerNotification` and
  `ServerRequest` directly; runtime-backed response behavior belongs on
  `Codex` / `ThreadEventStream`.
- Event subscription should happen before the thread lifecycle request that
  creates its handle, so fast initial events are buffered before the handle is
  returned.
- Shutdown should close the runtime and give long-lived connections or
  background tasks a clear shutdown path.
- When updating the Codex git tag, run the
  [Codex upgrade checklist](upgrade-checklist.md) and compare app-server
  protocol changes.
