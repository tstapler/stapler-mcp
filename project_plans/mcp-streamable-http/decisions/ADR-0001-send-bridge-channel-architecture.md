# ADR-0001: Bounded mpsc/oneshot channel bridge between the `!Send` daemon core and `rmcp`'s `Send`-bound Streamable HTTP transport

**Status**: Accepted
**Date**: 2026-09-14
**Deciders**: Tyler Stapler (solo project)
**Related**: `requirements.md` §"Verified architectural finding", `research/stack.md` §3, `research/architecture.md` (Option A/B/C), `research/build-vs-buy.md` §6, `research/pitfalls.md` §4, `implementation/plan.md` Phases 2-3

## Context

`stapler-mcp --daemon` needs to serve MCP tool calls directly over `rmcp`'s Streamable HTTP transport
(`requirements.md`'s core Success Metric), so that consumers can point `mcp-servers.json` at
`{"type": "http", "url": "http://127.0.0.1:<port>/mcp"}` instead of spawning a stdio thin-client
process per Claude Code session/subagent (the actual goal behind issue #37: process-count bloat).

The daemon's core (`crates/core/src/daemon.rs`, `crates/native/src/browser.rs`) is deliberately
`!Send`: state lives in `Rc<RefCell<...>>` (`Daemon::handlers`, `Daemon::shutdown`,
`NativeBrowser::sessions` and friends), run on a single `current_thread` tokio runtime + `LocalSet`
(`crates/cli/src/main.rs:28-36`). This is not incidental — the same `!Send` core is reused verbatim by
`crates/wasm/src/lib.rs:17,52` (`Daemon::new()`, identical type, not a divergent copy — confirmed by
direct read, `research/architecture.md`'s "New empirical finding"), so relaxing `!Send` in `crates/core`
would force the change onto the wasm adapter too, which has no use for `Send` (wasm-bindgen's execution
model is inherently single-threaded).

`rmcp` 2.2.0's `StreamableHttpService<S, M>` — the officially supported way to host MCP over Streamable
HTTP — bounds `S: crate::Service<RoleServer> + Send + 'static`
(`rmcp-2.2.0/src/transport/streamable_http_server/tower.rs:571-577`), and this bound is exercised, not
just declared: `spawn_session_worker` drives every session via a genuine `tokio::spawn`
(`tower.rs:660-693`), which requires `F: Send + 'static` regardless of runtime flavor
(`current_thread` or `multi_thread` — this is a property of the `tokio::spawn` API itself, confirmed in
`research/stack.md` §3). `rmcp` does ship a `local` feature relaxing its base `Service` trait to `!Send`
(`rmcp-2.2.0/Cargo.toml:85`, `src/service.rs:136,151`), but the `StreamableHttpService` `tower_service::Service`
impl has exactly one, unconditional, non-feature-gated definition requiring `S: Send`
(`research/architecture.md`, "confirms there is no escape hatch") — there is no way to hand a `!Send`
handler to the HTTP transport as-is, under any feature combination.

So passing `Daemon` (or anything built directly on it) as `S` does not typecheck, and no configuration
of `rmcp` changes that. A design decision is required.

## Decision

Bridge the two worlds with a bounded, in-process channel — the standard Tokio "actor" pattern (Alice
Ryhl, "Actors with Tokio"; the accepted answer to
[tokio-rs/tokio#2095](https://github.com/tokio-rs/tokio/issues/2095)):

1. **The daemon keeps running on exactly the runtime it runs on today** — one `current_thread` runtime,
   one `LocalSet`, no second runtime. `Daemon` and every port implementation
   (`NativeBrowser`, `NativeEmbedder`, `docs::SourceLocks`, etc.) are untouched.
2. A **bounded `tokio::sync::mpsc::Sender/Receiver<BridgeMessage>`** is constructed in `run_daemon`
   (`crates/cli/src/main.rs`), where `BridgeMessage = (Request, oneshot::Sender<Response>)`. `Request`/
   `Response` (`crates/core/src/protocol.rs`) are already plain, `Send`-safe serde structs used across
   the existing Unix-socket path — no new wire type is needed.
3. A single long-lived **bridge consumer task**, spawned via `tokio::task::spawn_local` (so it stays on
   the `LocalSet`, `!Send`-compatible), owns a clone of `Daemon` (wrapped in `Rc`) and loops: receive a
   `BridgeMessage`, call the new `Daemon::handle_request` (a public, typed sibling of the existing
   private `dispatch`, avoiding a redundant serialize/deserialize round trip the byte-oriented
   `handle_request_bytes` would otherwise force), reply via the paired `oneshot::Sender`.
4. Each HTTP session gets a **small, cheap, `Send` facade** — `McpRouter<ChannelTransport>`, where
   `ChannelTransport` is just a cloned `mpsc::Sender<BridgeMessage>` behind a `DaemonTransport` trait
   implementation. This is exactly the shape `rmcp` itself already assumes: `StreamableHttpService`'s
   `service_factory` is called once per session to build a fresh, cheap `S`
   (`tower.rs:631-693`, confirmed in `research/features.md` §1) — structurally identical to how today's
   stdio thin client is one `ThinClient` instance per OS process, just multiplied per HTTP session
   instead of per process.
5. The **HTTP-serving side** (axum/hyper) runs via genuine `tokio::spawn` (not `spawn_local`), which
   works fine cooperatively on the same single-OS-thread `current_thread` runtime — `tokio::spawn`'s
   `Send` bound exists for API uniformity with the multi-threaded runtime, not because a
   `current_thread` runtime demands cross-thread movement (`research/architecture.md`, Option C).

The bridge channel is **bounded** (capacity 64, not unbounded) specifically because an unbounded queue
under request pressure is the same failure shape as the already-patched session-table-leak DoS,
`GHSA-9pj6-vhgr-3mwh`. On daemon shutdown, any `BridgeMessage`s still queued when the shutdown flag is
observed get an explicit `Response::err("daemon shutting down")` reply rather than having their
`oneshot::Sender` silently dropped (which would otherwise surface to the HTTP caller as an opaque
`RecvError`).

## Rationale

- **Zero changes to `crates/core::daemon::Daemon`'s state model or any `crates/native`/`crates/wasm`
  port implementation.** `Daemon::handle_request_bytes` (renamed dispatch aside) was already written as
  a pure, transport-agnostic `&self, bytes -> bytes` function specifically to be independently testable
  — the bridge needs a second "front door" into that same function, not a rewrite of what's behind it.
- **Matches the shape `rmcp` itself expects**, rather than fighting the framework: a fresh, cheap,
  `Send` `S` per session is the documented, tested pattern (`rmcp-2.2.0/tests/test_streamable_http_*.rs`
  all construct their `S` via a `service_factory` closure the same way).
- **No new concurrency semantics are introduced into the `!Send` core.** The existing Unix-socket accept
  loop (`Daemon::run`, `daemon.rs:80-98`) already serializes every client one request at a time; the
  bridge consumer task preserves the exact same property for HTTP-originated requests — concurrent HTTP
  sessions get concurrent *requests in flight* (queued on the channel), but the single consumer still
  processes them one `.await`-yielded step at a time on the one `LocalSet` thread. The browser pool's
  single-instance invariant, upheld today informally by the `!Send`/single-thread design, needs no new
  mechanism to keep holding.
- **`NativeBrowser`'s internals were already built for this.** `NewSessionSlotGuard`
  (`crates/native/src/browser.rs:311-335`), per-session `tokio::sync::Mutex`-guarded locks, and
  `SessionState::is_busy()` all exist specifically because the idle reaper already runs concurrently
  with in-flight tool calls today (`research/features.md` §2) — the hard TOCTOU bugs this bridge's
  concurrency could otherwise introduce are pre-solved at the port level.

## Consequences

- **Positive**: this is the smallest structural change available that satisfies the `Send` bound —
  one new channel, one new spawned task, one new trait (`DaemonTransport`) with two implementations
  (existing `SocketTransport` behavior extracted unchanged, new `ChannelTransport`). No second runtime's
  startup/shutdown/panic-propagation lifecycle to manage.
- **Positive**: directly answers the "session semantics" open question from `requirements.md` — because
  each HTTP session's `S` is stateless itself (just a channel-sender clone), `rmcp`'s per-session SSE/
  `Mcp-Session-Id` bookkeeping never touches `!Send` state at all; only the single bridge consumer does,
  serialized through the channel.
- **Negative, accepted**: a slow tool call from one HTTP session (e.g. a multi-second `browser` action)
  blocks every other concurrent HTTP session's request behind it in the bridge consumer's serial
  processing — this is a real, user-visible latency characteristic under concurrent load, not a
  correctness bug. `requirements.md`'s Success Metric only requires "without corruption or deadlock,"
  which this design satisfies by construction; it does not require low-latency fairness under
  contention, which is out of scope for this daemon's stated scale target ("a handful of concurrent
  local sessions," not a multi-tenant server).
- **Negative, accepted**: any future attempt to "shortcut" this bridge for perceived performance (e.g.
  caching a `Page`/browser-session handle keyed by `Mcp-Session-Id` directly on the `Send` HTTP side)
  would not compile without an `unsafe impl Send` wrapper — and any such wrapper appearing in a future
  diff should be treated as an automatic design-review trigger (`research/pitfalls.md` §4), since it
  would silently undo the one compile-time thread-affinity guarantee this architecture currently
  provides.

## Alternatives Considered

| Alternative | Rejected because |
|---|---|
| `Arc<Mutex<...>>`/`Arc<tokio::sync::Mutex<...>>` rewrite of `Daemon`/`NativeBrowser`, making the whole core genuinely `Send` and shareable directly (no channel, no bridge task) | 65+ separate `Rc<RefCell<...>>` construction/access sites in the 3466-line `crates/native/src/browser.rs` (verified `grep -c`), none written with lock-ordering discipline in mind (the entire point of the `!Send` design was to avoid ever reasoning about concurrent mutation) — converting requires auditing every site for held-lock-across-`.await` bugs and reentrant-acquisition risk with no existing precedent to lean on. It also forces the same cost onto `crates/wasm`'s adapters, which are confirmed to reuse `Daemon` verbatim (`crates/wasm/src/lib.rs:17,52`) and have no use for `Send` at all — dead weight in an inherently single-threaded execution model, or a fork of the shared tool-function signatures that reintroduces the code duplication `crates/core/src/ports.rs`'s design exists to avoid. |
| Two separate tokio runtimes (one driving the `!Send` `LocalSet`, one multi-threaded runtime for axum/hyper), bridged by a channel between runtimes | Strictly more moving parts than the single-runtime design above for no additional capability: `LocalSet::spawn_local` on the *existing* `current_thread` runtime already gives the `!Send` worker a place to live, and `tokio::spawn` already works fine on that same `current_thread` runtime for the `Send` HTTP side (confirmed: the `Send` bound on `tokio::spawn` is an API-uniformity property, not a cross-thread-movement requirement). A second runtime would only be justified by a genuine need for multi-core parallelism, which `requirements.md`'s own Non-functional Requirements explicitly disclaim ("a handful of concurrent local sessions... not a multi-tenant/high-concurrency server"). |
| Hand-rolled HTTP+SSE over the existing `{tool,params}`/`{result\|error}` wire protocol (`crates/core/src/protocol.rs`), keeping `ThinClient` as the actual MCP-speaking `ServerHandler` but making it dial HTTP instead of a Unix socket | Sidesteps the `Send`/`!Send` conflict but solves the wrong problem: it does not eliminate the per-session thin-client process (issue #37's actual complaint), since the 23/27-tool `ServerHandler` machinery still has to live in a process somewhere to translate real MCP into this bespoke wire format — relocating its *inner* transport from Unix socket to HTTP changes nothing about process count. It also fails `requirements.md`'s literal Success Metric: an MCP client configured with `{"type": "http", ...}` expects real MCP-over-HTTP (JSON-RPC envelope, `initialize` handshake, spec-shaped SSE), not a bespoke JSON-over-HTTP endpoint. |
