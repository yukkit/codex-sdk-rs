# Codex Upgrade Checklist

Use this checklist when bumping the `openai/codex` git tag used by this SDK.

The SDK intentionally exposes Codex app-server protocol types directly. That
means protocol changes should be visible at compile time instead of being hidden
behind `serde_json::Value`.

## 1. Update Dependencies

Make sure local tooling is present:

```sh
make setup
```

Update every Codex git dependency in `Cargo.toml` to the same tag:

```toml
codex-app-server-client = { git = "https://github.com/openai/codex", tag = "rust-vX.Y.Z", package = "codex-app-server-client" }
codex-app-server-protocol = { git = "https://github.com/openai/codex", tag = "rust-vX.Y.Z", package = "codex-app-server-protocol" }
codex-arg0 = { git = "https://github.com/openai/codex", tag = "rust-vX.Y.Z", package = "codex-arg0" }
codex-config = { git = "https://github.com/openai/codex", tag = "rust-vX.Y.Z", package = "codex-config" }
codex-core = { git = "https://github.com/openai/codex", tag = "rust-vX.Y.Z", package = "codex-core" }
codex-feedback = { git = "https://github.com/openai/codex", tag = "rust-vX.Y.Z", package = "codex-feedback" }
codex-otel = { git = "https://github.com/openai/codex", tag = "rust-vX.Y.Z", package = "codex-otel" }
codex-protocol = { git = "https://github.com/openai/codex", tag = "rust-vX.Y.Z", package = "codex-protocol" }
```

Then refresh the lockfile:

```sh
cargo update
```

## 2. Compile First

Run the compiler before doing manual cleanup:

```sh
cargo check
```

Expected compile failures are useful signals. In particular:

- `ServerNotification` match failures usually mean
  `src/event.rs::notification_matches` needs a new routing rule.
- `ServerRequest` match failures usually mean
  `src/event.rs::request_matches` needs a new routing rule.
- `AppServerEvent` / `InProcessServerEvent` failures usually mean
  `src/runtime/mod.rs::spawn_event_loop` needs updates.

## 3. Review Protocol Changes

Compare the upstream protocol definitions for the old and new tags:

- `codex-rs/app-server-protocol/src/protocol/common.rs`
- `codex-rs/app-server-protocol/src/protocol/v2/`
- generated `schema/typescript/ServerNotification.ts`
- generated `schema/typescript/ServerRequest.ts`

For every new or changed `ServerNotification`, decide:

- Does it belong to a specific `thread_id`?
- Does it belong to a specific `turn_id`?
- Is it global and should active thread streams see it?
- Does the thread stream need new terminal or routing behavior for it?

For `ThreadStartParams` and `TurnStartParams`, decide:

- Did structured-output fields such as `output_schema` change shape or
  persistence semantics?
- Does `ThreadBuilder` still pass native `ThreadStartParams` through without
  hiding important protocol behavior?
- Do `CodexTurnBuilder` and `TurnBuilder` still pass native `TurnStartParams`
  through without hiding important protocol behavior?

For every new or changed `ServerRequest`, decide:

- Does it carry `thread_id` and/or `turn_id`?
- Should it be delivered to one active thread stream or treated as global?
- What response type should callers pass to `Codex::resolve_server_request()`
  or `ThreadEventStream::resolve_server_request()`?
- Is `approve_server_request()` with `{}` still valid for that request?

## 4. Recheck SDK Semantics

Verify these SDK boundary assumptions still hold:

- Each `Thread` should expose one long-lived native `AppServerEvent` stream.
- `AppServerEvent::ServerRequest` should expose the native `ServerRequest`
  directly. Runtime behavior such as resolve/reject belongs on `Codex` or
  `ThreadEventStream`.
- Thread start/resume/fork/unarchive paths must subscribe before issuing their
  lifecycle request, so the thread stream cannot miss fast initial events.
- `TurnCompleted` must remain a normal event and must not end the thread stream.
- `ServerRequest` variants must remain visible to matching thread streams.
- Explicit `Codex::shutdown()` should notify streams with `Disconnected` and
  wait for the runtime task to exit.
- The SDK should not add implicit turn serialization. Applications own the
  no-overlap contract for turns sharing a thread ID.
- Global `ServerRequest`s that lack thread/turn ids may be visible to multiple
  active thread streams. If Codex adds more global requests, document the expected
  application-level de-duplication behavior.

## 5. Recheck Dependency Policy

Run:

```sh
make deny_check
```

Then review `deny.toml` intentionally:

- Remove advisory ignores that no longer match the graph.
- Prefer dependency upgrades over new ignores when the SDK directly controls the
  dependency.
- For unavoidable Codex transitive dependencies, keep exceptions narrow and add
  reasons.
- For banned crates that are only acceptable under known upstream parents, use
  `bans.deny[].wrappers` instead of deleting the ban.
- Keep `sources.allow-git` aligned with actual git dependencies in
  `Cargo.lock`.

## 6. Validate

Run:

```sh
make setup
make deny_check
make fmt_check
cargo check
RUSTDOCFLAGS='-D warnings -D missing-docs' cargo doc --no-deps
cargo test
```

If example behavior changed, also run or compile the example:

```sh
cargo check -p example
```

## 7. Update Docs

Update docs when protocol behavior or SDK semantics changed:

- `README.md`
- `docs/sdk-user-guide.md`
- `docs/native-codex-developer-guide.md`
- this checklist
- `docs/api-parity.md`

Pay special attention to examples that use `run().await`: they should not imply
interactive approvals are handled unless they use streaming.
