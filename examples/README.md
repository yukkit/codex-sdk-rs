# codex-sdk-rs examples

This directory contains Cargo-native examples for the main SDK usage patterns.
Build every example without starting Codex:

```sh
cargo check --examples
```

The minimal example remains deliberately small:

```sh
cargo run --example minimal
```

All in-process examples must enter through `codex_sdk::run_main`; the remote
example uses a normal Tokio entry point because helper dispatch lives in the
remote app-server process.

## Example targets

| Example | Purpose | Important SDK surfaces | Side effects |
| --- | --- | --- | --- |
| `minimal` | Minimal in-process turn | `run_main`, `CodexBuilder`, `Thread`, `TurnHandle`, `ThreadEventStream` | Read-only |
| `repo_review` | Structured one-off repository review, optionally with an image | temporary thread, native `UserInput`, reasoning options, output schema | Read-only |
| `interactive_repair` | Two-turn diagnosis and repair workflow | reusable thread, approvals, `SandboxPolicy`, steering, interruption | Read-only by default; opt-in writes |
| `thread_lifecycle` | Persistent thread administration | create, resume, read, name, compact, fork, list, archive, unarchive | Writes Codex thread state |
| `parallel_batch` | Independent concurrent analyses | cloned `Codex`, one stream per thread, cross-thread concurrency | Read-only |
| `runtime_ops` | Production startup and inventory | observability, warmup, runtime events, models, account, shutdown | Read-only inventory requests |
| `remote_client` | Remote WebSocket or Unix-socket connection | `CodexRemoteBuilder`, auth token, runtime/thread streams | Owned by remote server policy |
| `native_request` | Native protocol escape hatch | `next_request_id`, `ClientRequest`, `request_typed` | Read-only inventory requests |

Run an example target with:

```sh
cargo run --example repo_review
cargo run --example interactive_repair
cargo run --example parallel_batch
cargo run --example runtime_ops
cargo run --example native_request
```

## Repository review

`repo_review` constrains the final response with a JSON Schema and deserializes
it into a Rust struct. Add a local screenshot or diagram to the review with:

```sh
CODEX_EXAMPLE_IMAGE=/absolute/path/to/image.png \
  cargo run --example repo_review
```

Optional model settings are read from `CODEX_MODEL`, `CODEX_MODEL_PROVIDER`, and
`CODEX_SERVICE_TIER`.

## Interactive repair safety

`interactive_repair` is read-only and rejects every server request by default.
Enable workspace writes and simple command/file/permission approvals explicitly:

```sh
CODEX_EXAMPLE_ALLOW_WRITES=1 \
CODEX_EXAMPLE_APPROVE_REQUESTS=1 \
  cargo run --example interactive_repair
```

The demo constructs the native response payloads for command execution, file
change, and turn-scoped permission requests. MCP elicitations, tool user-input
requests, dynamic tool calls, and other request types are rejected.

Steer or interrupt the implementation turn with:

```sh
CODEX_EXAMPLE_STEER='Only change documentation.' \
  cargo run --example interactive_repair

CODEX_EXAMPLE_INTERRUPT=1 \
  cargo run --example interactive_repair
```

The example waits for the matching `TurnStarted` notification before sending a
steer or interrupt request; `turn/start` returning alone is not used as the
active-turn boundary.

## Thread lifecycle

The lifecycle binary defaults to listing saved threads:

```sh
cargo run --example thread_lifecycle -- create
cargo run --example thread_lifecycle -- list
cargo run --example thread_lifecycle -- inspect THREAD_ID
cargo run --example thread_lifecycle -- resume THREAD_ID
cargo run --example thread_lifecycle -- compact THREAD_ID
cargo run --example thread_lifecycle -- archive THREAD_ID
cargo run --example thread_lifecycle -- unarchive THREAD_ID
```

`create` also forks the new thread, archives the branch, and immediately
unarchives it to demonstrate the stream boundary guaranteed by the SDK.

## Runtime warmup

`runtime_ops` warms models, skills, permission profiles, managed requirements,
account state, and MCP status. Connector/app inventory is disabled by default:

```sh
CODEX_EXAMPLE_WARM_APPS=1 cargo run --example runtime_ops
```

Use `CODEX_EXAMPLE_RELOAD_SKILLS=1` to bypass cached skill inventory. Individual
warmup failures are reported in `WarmupResult` and do not stop later steps.

## Remote client

Select exactly one remote transport. A bearer token is optional and read from
`CODEX_APP_SERVER_TOKEN`:

```sh
CODEX_WS_URL=wss://codex.example.com/rpc \
CODEX_APP_SERVER_TOKEN=secret \
  cargo run --example remote_client

CODEX_UNIX_SOCKET=/tmp/codex-app-server.sock \
  cargo run --example remote_client
```

`CODEX_REMOTE_CWD` sets the working directory understood by the remote server.
`CODEX_MODEL` overrides the remote thread model, which is useful when the
remote server has a different CLI version from the client. Local `CODEX_HOME`
configuration is not applied to a remote runtime.

## Event-stream rule

Take one `ThreadEventStream` for a thread and keep consuming it across all of
that thread's turns. `TurnCompleted` ends one turn, not the stream. Different
thread IDs can run concurrently; do not overlap turns on the same thread ID.
