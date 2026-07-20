# API Parity

This document records how the `codex-sdk-rs` public API maps to the official
Codex TypeScript and Python SDK public APIs. It is a reference for keeping Rust
API design moving in a consistent direction.

Reference surfaces:

- TypeScript SDK: `openai/codex/sdk/typescript`
- Python SDK: `openai/codex/sdk/python`
- Rust SDK: this repository, `codex-sdk-rs`

This is not a compatibility promise. Re-check this table whenever the official
SDKs or the upstream Codex protocol change.

## Overall Assessment

The Rust SDK is a closer fit for the Python SDK than for the TypeScript SDK.

The TypeScript SDK currently behaves more like a CLI wrapper: `Codex` creates a
CLI executor, and `Thread.run` / `Thread.runStreamed` complete turns through the
Codex CLI. It has `ThreadOptions`, but it does not expose a full app-server
style thread lifecycle, turn handle, steering, or interrupt surface.

The Python SDK is closer to an app-server SDK: it has thread lifecycle APIs,
`TurnHandle`, streaming, steering, interrupt, login/account, models, retry, and
error surfaces.

`codex-sdk-rs` currently follows the app-server protocol path: it can start an
in-process runtime or connect to a remote WebSocket / Unix-socket app-server.
API design therefore primarily aligns with Python app-server semantics while
preserving Rust builder style and native Codex protocol escape hatches.

## Current Mapping

| Capability | TypeScript SDK | Python SDK | Current Rust SDK | Notes |
| --- | --- | --- | --- | --- |
| Client/runtime startup | `new Codex(options)` | `Codex(config)` / `AsyncCodex(config)` | `Codex::builder()` / `builder_with_config(config)` / `remote_websocket(url)` / `remote_unix_socket(path)` | Rust supports in-process and remote app-server backends. |
| Runtime configuration | `CodexOptions`: `codexPathOverride`, `baseUrl`, `apiKey`, `config`, `env` | `CodexConfig`: `codex_bin`, `launch_args_override`, `config_overrides`, `cwd`, `env`, client metadata | In-process: `CodexBuilder` / `CodexWithConfigBuilder`; remote: transport/client identity plus complete native thread defaults | In-process sandbox/approval setters are explicit overrides; omitted values inherit native config and trust defaults. In remote mode, runtime config belongs to the app-server. |
| Native escape hatch | `CodexOptions.config` only becomes CLI `--config` overrides | `thread_start(config=...)` and similar methods can carry protocol config fragments; high-level methods map to generated params | `builder_with_config(Config)`, in-process `default_thread_config_overrides(...)`, universal `default_thread_params(...)`, `thread_params(...)`, `turn_params(...)`, `*_params(...)` | Rust projects common resolved defaults and preserves native config/params escape hatches for fields reloaded at `thread/start`. |
| Create thread | `codex.startThread(options)` | `codex.thread_start(...)` | `codex.thread().start().await?` | Semantics are aligned; Rust uses a builder. |
| Resume thread | `codex.resumeThread(id, options)` | `codex.thread_resume(id, ...)` | `codex.resume_thread(id).await?` / `resume_thread_params(...)` | Core capability is aligned. |
| List/fork/archive | No high-level API | `thread_list` / `thread_fork` / `thread_archive` / `thread_unarchive` | `list_threads` / `fork_thread` / `archive_thread` / `unarchive_thread`, plus `Thread::fork` / `Thread::archive` | Rust is currently closer to Python. |
| Read/name/compact thread | None | `thread.read(...)` / `set_name(...)` / `compact()` | `Thread::read(...)` / `set_name(...)` / `compact()` | Rust is aligned with Python's common thread operations. |
| Thread source/session source | None | `thread_start(session_start_source=..., thread_source=...)`, `thread_fork(thread_source=...)` | Default `ThreadSource::User`; full control through `ThreadStartParams` / `ThreadForkParams` | Rust avoids a dedicated high-level setter for low-frequency protocol metadata. |
| One complete turn | `thread.run(input, turnOptions)` | `thread.run(input, approval_mode=..., model=..., sandbox=..., output_schema=...)` | Start with `thread.turn(input).start().await?`, then consume the matching `TurnCompleted` from the thread event stream | Rust intentionally exposes native turn status instead of a batch result. |
| Build/start controllable turn | No handle; only `run` or `runStreamed` | `thread.turn(input, ...) -> TurnHandle` | `thread.turn(input).start().await? -> TurnHandle` | Rust is aligned with Python's handle concept. |
| Streaming | `thread.runStreamed(input, turnOptions)` returns `AsyncGenerator<ThreadEvent>` | `turn.stream()` returns a notification iterator / async iterator | Take `thread.event_stream()?` once and keep it across turns | Rust models the app-server lifecycle directly: one long-lived event stream belongs to each thread. |
| Steer / interrupt | None; `TurnOptions.signal` can abort a CLI run | `TurnHandle.steer(...)` / `interrupt()` | `TurnHandle::steer(...)` / `interrupt()` | Rust keeps active-turn control separate from thread event consumption. |
| Top-level one-off turn | None | No dedicated top-level API; callers start a thread first | `codex.turn(input).start().await?` / `codex.turn_params(params)` | The returned `TurnHandle` exposes its temporary owning thread through `thread()`. |
| Input types | `Input` is a string or `UserInput[]`; `UserInput` only has text/local image | `str` or `TextInput` / `ImageInput` / `LocalImageInput` / `SkillInput` / `MentionInput` / list | `impl IntoTurnInput`, supporting `&str` / `String` / native `UserInput` / `Vec<UserInput>` | Rust reuses native app-server `UserInput`, covering text, image, localImage, skill, and mention. |
| Turn options | Only `outputSchema` / `signal`; model/sandbox/approval live in `ThreadOptions` | `run` / `turn` accept approval, cwd, effort, model, output_schema, personality, sandbox, service_tier, summary | `TurnBuilder` / `CodexTurnBuilder` provide similar setters and can accept full `TurnStartParams` | Rust keeps turn configuration on builders. |
| Structured output | `TurnOptions.outputSchema` | `output_schema=` | `output_schema(...)`; applications deserialize assistant-message events | Rust avoids choosing or concatenating message items on the caller's behalf. |
| Result shape | `Turn { items, finalResponse, usage }` | `TurnResult { id, status, error, times, final_response, items, usage }` | Long-lived native `AppServerEvent` stream; each turn emits `TurnCompleted` | Status, errors, items, and usage remain protocol-native. |
| Reasoning | `ThreadOptions.modelReasoningEffort`; no summary | Turn params `effort` / `summary` | Runtime defaults `reasoning_effort` / `reasoning_summary`; turn overrides `effort` / `reasoning_summary` / `summary` | Rust covers the common fields. |
| Sandbox / filesystem | `ThreadOptions.sandboxMode`, `networkAccessEnabled`, `additionalDirectories` | `Sandbox` presets; detailed fields require lower-level config/protocol | Thread default `SandboxMode`; turn-level `sandbox(SandboxMode)` and `sandbox_policy(SandboxPolicy)` | Rust's `sandbox_policy` is the fine-grained network/writable-roots escape hatch. |
| Web search / network | `webSearchMode` / `webSearchEnabled` / `networkAccessEnabled` | No high-level turn setter | No high-level web-search setter; network can be controlled through `SandboxPolicy` or native `Config`/params | These are lower-frequency or more change-prone options, so there is no high-level API yet. |
| Approval | `ApprovalMode` supports `never`, `on-request`, `on-failure`, `untrusted` | `ApprovalMode.deny_all` / `auto_review`, mapped to lower-level approval policy plus reviewer | Native `AskForApproval` / `approval_policy(...)` | Rust keeps Codex-native approval types to avoid second-order semantics. |
| cwd/model/model_provider | `workingDirectory` / `model`; no model provider | `cwd` / `model` / `model_provider` | `cwd` / `model` / `model_provider` | Rust is closer to Python. |
| Service tier / personality | No high-level API | Supported on thread and turn params | Builder/thread/turn support `service_tier` / `personality` | Common setters are present. |
| Base/developer instructions | No high-level API | `base_instructions` / `developer_instructions`, mainly on thread lifecycle methods | Runtime, thread, and temporary-thread builders have same-name setters | Existing threads do not support changing instructions for a single turn; create/resume/fork a thread or use thread lifecycle params. |
| Prompt context toggles | None | No high-level API | In-process `minimal_prompt_context()` and `include_*_instructions(...)` | Rust adds high-level support for embedded low-token or chat-only scenarios. |
| Minimal tool profile | Native config overrides | Native config overrides | In-process `minimal_tools()` | Disables known configurable tool families and discovery surfaces; it is not an all-tools-off guarantee. |
| Pure chat profile / environment access | Native config overrides | Native params | In-process `pure_chat_profile()` / `EnvironmentAccess` | The profile composes prompt, project-document, tool, and environment settings; each can be overridden later in builder order. |
| Login/account/models | None | `login_api_key` / `login_chatgpt` / `account` / `logout` / `models` | `models` / `models_params`, `account` / `account_params`; login/logout are missing | Rust has read-only P1 coverage; login flow remains the host system's responsibility or a future high-level API. |
| Errors/retry | Plain `Error` | Public error types / `retry_on_overload` | `Error` / `Result` | Rust has basic errors; retry is not yet wrapped in a high-level helper. |

## Current Tradeoffs

### Streaming-only turns

Python's `Thread.run(...)` can take turn parameters directly:

```python
thread.run(
    input,
    approval_mode=None,
    cwd=None,
    effort=None,
    model=None,
    output_schema=None,
    personality=None,
    sandbox=None,
    service_tier=None,
    summary=None,
)
```

Rust keeps a builder and one native event stream per thread:

```rust,no_run
let mut events = thread.event_stream()?;
let turn = thread
    .turn("Review this diff.")
    .model("gpt-5.x")
    .sandbox(SandboxMode::ReadOnly)
    .effort(ReasoningEffort::High)
    .start()
    .await?;
```

There is no `Thread::run`, `send`, or SDK-owned collector. This keeps failure,
interruption, multiple message items, approvals, and usage visible as native
events instead of collapsing them into an ambiguous final string. Applications
match events by `turn.turn_id()` when they need a per-turn view; the stream
itself remains attached to the thread.

### Native Escape Hatch

The Rust SDK does not intend to provide a high-level setter for every Codex
protocol field. Principles:

- High-frequency, stable fields used by official SDKs get setters, such as
  `cwd`, `model`, `sandbox`, `approval_policy`, `effort`, `summary`,
  `output_schema`, `personality`, and `service_tier`.
- Low-frequency or experimental fields are exposed through native params such
  as `ThreadStartParams`, `TurnStartParams`, `ThreadResumeParams`,
  `ThreadForkParams`, and `ThreadListParams`.
- Common effective runtime configuration is projected from `Config` into native
  thread params because embedded app-server threads reload file/project layers.
  Applications with uncommon in-memory mutations should use
  `default_thread_config_overrides(...)` or `default_thread_params(...)` rather
  than introducing another SDK config struct.

### Prelude Strategy

`prelude` only contains types that are used frequently in day-to-day code:
`Codex`, `CodexEventStream`, builders, `Thread`, `ThreadEventStream`,
`TurnBuilder`, `TurnHandle`,
`Account`, `Model`, core params, `SandboxMode`, `SandboxPolicy`,
`AskForApproval`, `ReasoningEffort`, `ReasoningSummary`, `UserInput`, and
similar types.

Lower-frequency response types remain exposed at the crate root, for example:

```rust,no_run
let page: codex_sdk::ThreadListResponse = codex.list_threads().await?;
```

## Future Optional Alignment Items

1. Add `login_api_key` / `login_chatgpt` / `logout` so the SDK can complete
   auth flows independently.
2. Add a retry helper if production demand calls for one, closer to Python's
   `retry_on_overload`.
3. Reevaluate naming consistency if the TS SDK changes from a CLI wrapper into
   an app-server SDK.
