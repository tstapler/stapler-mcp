# Research: Features — mcp-streamable-http

Scope: how comparable multi-session MCP/RPC daemons split protocol-session
state from shared resource state, how that maps onto this repo's existing
`!Send` daemon core and browser-session model, what new concurrency/failure
modes the HTTP transport introduces, and what a real user would need beyond
the literal ask.

## 1. How `rmcp`'s own session abstractions split protocol state from shared resources

**Verified: `rmcp` already assumes one fresh `ServerHandler` instance per HTTP session, not a shared handler instance.**

`StreamableHttpService<S, M>` takes a `service_factory: Arc<dyn Fn() -> Result<S, io::Error> + Send + Sync>`
(`rmcp-2.2.0/src/transport/streamable_http_server/tower.rs:550`, constructed
in `StreamableHttpService::new`, `tower.rs:631-648`). Every time a client's
first (`initialize`) POST creates a new session, the service calls
`self.get_service()` — i.e. invokes the factory — to build a **brand-new `S`**
for that session (`tower.rs:1154-1156`, and again in the cross-instance
restore path at `tower.rs:789-794`). That new `S` is then handed to
`spawn_session_worker`, which `tokio::spawn`s a task running
`serve_server::<S, M::Transport, _, _>(service, transport)` for the lifetime
of the session (`tower.rs:660-693`).

So the idiom `rmcp` itself bakes in is:
- **Per-session**: the `ServerHandler` instance `S`, and everything
  `LocalSessionManager` tracks about the MCP *protocol* session (event IDs,
  SSE reconnect cache, pending-request routing) — see
  `LocalSessionWorker`'s per-session `tx_router`/`resource_router`/`common`
  fields (`rmcp-2.2.0/src/transport/streamable_http_server/session/local.rs:319-333`).
  Each session also gets its own `tokio::spawn`ed task/worker
  (`session/local.rs:1179-1203`'s `create_local_session`, invoked from
  `LocalSessionManager::create_session`, `session.rs` — actually
  `session/local.rs:50-55`).
- **Cross-session (must be shared explicitly by the embedder)**: any
  "real" backend resource the `service_factory` closure captures and clones
  into each new `S` — e.g. `Arc<SomeSharedState>::clone()` inside the
  closure body. `rmcp` has no opinion on this; it is exactly the same
  shape as a typical Axum handler capturing `State<Arc<AppState>>` per
  request. The crate's own doc comment for accessing HTTP-layer state
  confirms this pattern (`tower.rs:524-546`, "Accessing custom axum/tower
  extension state").

This directly answers the research question: yes, the intended design is
"isolated protocol state per HTTP session, shared resources via an explicit
handle cloned into each session's handler" — which matches how most
comparable single-process multi-client RPC daemons behave (e.g. a
`tonic`/gRPC service holding `Arc<Mutex<..>>`/`Arc<RwLock<..>>` app state
cloned into each request handler; a `tower::Service` behind `axum::Router`
with `.with_state()`). Nothing about `LocalSessionManager` shares state
between sessions on its own — that would have to be layered on by whatever
this daemon's `service_factory` closure captures.

**Verified: `LocalSessionManager`'s session worker is spawned with `tokio::spawn`, not `spawn_local`.**

`spawn_session_worker` (`tower.rs:660-693`) and `create_session`
(`session/local.rs:50-55`, via `WorkerTransport::spawn(worker)`) both
require `S: Send` — enforced at the trait-bound level by
`SessionManager: Send + Sync + 'static` (`session.rs:75`) and
`tower_service::Service`'s own `S: crate::Service<RoleServer> + Send + 'static`
bound (`tower.rs:574`). This is the literal mechanism behind the
requirements doc's "Verified architectural finding" #2 — it is not merely
a type-level annotation that could be relaxed with a feature flag; `rmcp`
actually calls `tokio::spawn` (a genuine cross-thread-capable spawn) on
the per-session worker, so `S` really must be `Send` for this to compile
against a real multi-threaded (or even `current_thread`-but-not-`LocalSet`)
executor.

## 2. Concurrent access from multiple HTTP sessions vs. today's Unix-socket model

**Verified: today's daemon is strictly one-request-at-a-time across ALL Unix-socket clients — not concurrent at all at the transport layer.**

`Daemon::run` (`crates/core/src/daemon.rs:80-98`) is a bare loop:
```
loop {
    let mut conn = listener.accept().await?;
    ...
    if let Some(bytes) = conn.read_frame().await? {
        let resp_bytes = self.handle_request_bytes(&bytes).await;
        conn.write_frame(&resp_bytes).await?;
    }
    if self.shutdown.get() { return Ok(()); }
}
```
There is no `tokio::spawn`/`spawn_local` per connection. A second thin
client's connection isn't even `accept()`ed until the first connection's
entire request/response round-trip (including the full `.await` on the
tool handler, e.g. a browser `navigate` that can take seconds) has
finished and the loop comes back around. So "the daemon already serves
concurrent Unix-socket clients today," as the backlog item's own framing
assumed, is not accurate at the transport layer — it serves them
*sequentially*, queued in the OS accept backlog. This sharpens (and
partially contradicts) the requirements doc's "Verified architectural
finding," which characterizes today's daemon as already concurrent.

**But: `NativeBrowser`'s internals are already written to be safe for genuine concurrent access — this looks deliberate, not accidental.**

Despite the serial accept loop, `crates/native/src/browser.rs` has explicit
TOCTOU guards for concurrent callers:
- `NewSessionSlotGuard` (`browser.rs:311-335`) exists specifically because
  "concurrent no-`session_id` `navigate()` calls could all observe
  `sessions.len() < MAX_OPEN_SESSIONS` before any of them reaches its own
  `.insert`" (`browser.rs:301-304`).
- Each `BrowserSession` carries its own `lock: Rc<tokio::sync::Mutex<()>>`
  (`browser.rs:166`), and `SessionState::is_busy()`
  (`browser.rs:83-91`) exists so the idle reaper "must never evict a busy
  session even if its `last_used` looks stale."
- The idle reaper itself (`spawn_reaper`, `browser.rs:269-286`) already runs
  concurrently with in-flight tool calls today, on the same `LocalSet`
  thread, via `tokio::task::spawn_local` — this is the one piece of
  already-concurrent daemon-state access in production, and it's exactly
  why `is_busy()`/the per-session lock exist.

So the *resource layer* (`NativeBrowser`) was already built to the
"multiple concurrent callers may be in flight" contract even though the
*transport layer* in front of it (`Daemon::run`'s serial accept loop)
never actually exercises more than one client call at a time today. This
meaningfully de-risks HTTP-transport concurrency: the hard TOCTOU
bugs are pre-solved at the `BrowserDriver` port level. What's new is that
the daemon's front door will, for the first time in production, actually
deliver overlapping calls — from `rmcp`'s per-session `tokio::spawn`ed
workers (§1) — to a core that previously only ever saw one in-flight call
at a time (modulo the reaper).

**Genuinely new edge case: `SessionId` (browser tab identity) is orthogonal to `Mcp-Session-Id` (HTTP/MCP transport identity), and today's stdio path conflates them 1:1 by accident of process lifetime.**

`ports::SessionId` (`crates/core/src/ports.rs:141`) is an *application-level*
handle to a persistent browser tab, explicitly passed as a tool argument
(`BrowserNavigateInput`/`BrowserClickInput` etc. — see the `SessionId`
threading through `BrowserDriver`'s methods, `ports.rs:252-393`), created by
`NativeBrowser::new_session_id()` (`browser.rs:379-385`) and cached
client-side by whichever caller made the `navigate` call. Nothing ties a
`ports::SessionId` to a specific stdio thin-client process or Unix-socket
connection — the daemon has always supported one caller opening a browser
session and a *different* call (even from a different thin-client process)
reusing that `SessionId` string, since each Unix-socket call is a fresh
connect/call/disconnect (`crates/core/src/client.rs:23-48`, no persistent
connection state).

With `stateful_mode: true` (the `rmcp` default,
`streamable_http_server/tower.rs:114`), the HTTP transport adds a *second*,
independent session concept: `Mcp-Session-Id` (`rmcp`'s
`session::SessionId`, MCP-protocol-level, tied to one SSE-backed
`LocalSessionWorker`). These two IDs already don't need to correlate today
(nothing stops Claude Code's stdio thin client 1 from calling `navigate`
and thin client 2 from calling `click` with the returned `SessionId`) — so
this isn't a *new* correctness requirement, but it is a *new place a naive
implementation could accidentally introduce coupling*: e.g. if the
`service_factory` closure that `rmcp` calls once per HTTP session
(§1) creates any *per-`S`-instance* browser state instead of purely
cloning `Rc`/`Arc` handles to the one shared `NativeBrowser`, a
`ports::SessionId` created on HTTP session A would silently become
invisible/unusable from HTTP session B — a regression from today's
already-working cross-connection `SessionId` reuse. This is worth an
explicit integration test (the requirements doc's own Success Metric
already calls for "≥2 concurrent HTTP sessions against a stateful tool at
once" — this finding says the test should also cover *browser*
`SessionId` reuse **across** two different `Mcp-Session-Id`s, not just
concurrent calls within one).

**Thread-affinity risk is real but structural, not incidental.** Because
`rmcp` schedules each HTTP session's worker via genuine `tokio::spawn`
(§1) while `NativeBrowser`'s `Page`/`Rc`/`RefCell` state is `!Send` and
confined to the daemon's single `current_thread` + `LocalSet`
(`crates/cli/src/main.rs:28-36`, `crates/core/src/daemon.rs:1-6` doc
comment), an HTTP session's tool-call task **cannot** call directly into
`NativeBrowser` methods — it must cross a channel/bridge into the
`LocalSet` thread and back, on every single tool call, not just at session
creation. This is the same bridge design called out as the "Rabbit Hole"
in requirements.md, but this reading of `tower.rs`/`session/local.rs`
confirms *where* exactly the `Send` boundary bites: it's not just "the
`ServerHandler` trait needs `Send`," it's "every per-session worker task
`rmcp` spawns needs `Send`, for the entire lifetime of that session,"
which means the bridge must be a long-lived channel per session (or a
single shared channel multiplexing all sessions into the `LocalSet`), not
a one-time handoff.

## 3. New failure modes / edge cases this transport change introduces

**Daemon restart while clients are connected — no equivalent exists today.**
Today, `crates/core/src/client.rs::call` opens a fresh Unix-socket
connection per tool call and closes it immediately after
(`client.rs:23-48`); `ensure_daemon` (`client.rs:63-106`) transparently
respawns the daemon and retries with backoff if the daemon is unreachable.
A daemon restart is invisible to the calling Claude Code session — the
*next* tool call just triggers a fresh `ensure_daemon` cycle. With HTTP
Streamable transport in `stateful_mode`, a client (per the MCP spec, and
per Claude Code's actual `http` transport behavior) holds a live SSE
connection tied to one `Mcp-Session-Id` for the session's duration. A
daemon restart drops that TCP connection; the *client* — not this repo's
code — decides whether to reconnect and whether it resends
`Mcp-Session-Id` (triggering `rmcp`'s 404-if-unknown-session path,
`tower.rs:940-953`/`1069-1082`, since `LocalSessionManager` holds sessions
only in-memory, `session/local.rs:33`) or starts a brand-new `initialize`
handshake. This repo cannot make that reconnect "transparent" the way
`ensure_daemon` does today unless it also configures `rmcp`'s
`session_store: Option<Arc<dyn SessionStore>>` (`tower.rs:79-100`) with a
persistent (e.g. on-disk) store — and even then, `restore_session`
(`session.rs:147-159`) only recreates the *protocol* session and replays
`initialize`; it does **not** restore any `ports::SessionId` browser tabs
that were open in the killed daemon process (those `Page` handles die with
the process, full stop — there is no possible restore for a live CDP
connection). **Net new requirement this surfaces**: a systemd/launchd
"auto-restart" unit (in scope per requirements.md) makes the daemon
*process* highly available, but it cannot make in-flight *browser
sessions* survive a restart — this needs to be a documented limitation
plus a decision on `session_store` (persist protocol handshake state at
minimum, so an MCP client's reconnect doesn't error, even though its
browser tabs are gone).

**No SIGTERM/signal handling exists today — relevant the moment a process supervisor is introduced.**
`crates/cli/src/main.rs`'s only shutdown path is the explicit
`SHUTDOWN_TOOL` RPC, which sets `Daemon.shutdown` (`daemon.rs:54-57`),
checked once per completed request (`daemon.rs:94-96`) — there is no
`tokio::signal::unix::SIGTERM` handler anywhere in `crates/cli`. The
careful shutdown sequence that aborts the idle reaper before closing the
browser (`main.rs:462-485`, with its own comment explaining why ordering
matters) only runs on that RPC-triggered clean exit path. A systemd unit
with `Restart=on-failure`/`KeepAlive` (in scope per requirements.md) will,
by default, send `SIGTERM` on stop/restart — which today bypasses this
whole cleanup path and kills the process (and the Chrome subprocess under
it) uncleanly. This is a real, concrete gap introduced by the "add a
systemd unit" scope item, independent of the HTTP transport work itself,
and should be called out in planning: a `SIGTERM` handler that triggers
the same shutdown sequence as `SHUTDOWN_TOOL` is needed for a supervised
daemon to restart gracefully.

**Port-already-in-use handling has no analog today, and the existing "why Unix socket" rationale says so explicitly.** `README.md:51-56` states the
reason for choosing a Unix socket in the first place: "Simple, fast,
local-only, **no port-conflict risk** across concurrent sessions." A fixed
TCP port reintroduces exactly the class of failure the original design
avoided — `EADDRINUSE` if some *other* unrelated process (or a stale/zombie
`stapler-mcp --daemon` that didn't release the port on a crash) is already
bound. The existing `flock`-based single-instance arbitration
(`crates/native/src/lock.rs:22-50`, non-blocking `try_lock`, `WouldBlock`
→ `LockError::AlreadyRunning`, checked in `main.rs:71-83` before any
socket/browser setup) still correctly prevents *two `stapler-mcp`
daemons* from running — but it says nothing about a *third-party* process
already squatting the chosen port. This needs its own explicit handling
(bind-then-fail-loudly vs. port auto-selection vs. configurable port per
the Open Questions) that has no precedent to crib from in this codebase.

**Lock-race behavior generalizes cleanly** — `lock.rs`'s `try_lock`/
`AlreadyRunning` semantics (`lock.rs:42-46`) are transport-agnostic; they
guard "should this process become *the* daemon," not "can this process
bind the socket/port," so no rework is needed there specifically for HTTP
— but the lock's win still needs to happen *before* the port bind attempt
(mirroring today's `main.rs:71-94` ordering: lock, then browser launch,
then socket bind) so that a losing racer never gets far enough to touch
the port at all.

## 4. Unstated needs (what Tyler would actually need beyond the literal ask)

**Session visibility for MCP sessions themselves, not just browser sessions.**
The tool surface already has exactly this pattern for browser tabs —
`stapler_browser_list_sessions` (`crates/cli/src/thin_client.rs:202-213`,
backed by `BrowserDriver::list_sessions`, `ports.rs:303`, returning
`SessionSummary { session_id, tab_count, idle_ms, blocked, crashed }`,
`ports.rs:225-240`). Once the daemon hosts several live *HTTP* sessions
(one per concurrent Claude Code session/subagent, exactly the scenario
this project exists to reduce process count for), there is no equivalent
visibility into *those* — `LocalSessionManager.sessions`
(`session/local.rs:33`) is an in-memory `HashMap` with no introspection
API exposed by `rmcp` itself. A debugging/observability tool (or at minimum
a log line per session create/close, already called for in
requirements.md's Observability Requirements) is the natural analog to
`stapler_browser_list_sessions` — likely worth its own admin-only endpoint
or tool (e.g. `stapler_daemon_list_sessions`) given how directly the
existing browser-session tool models the exact same "list live daemon-side
handles a caller can't otherwise see" problem.

**Graceful restart that doesn't kill in-flight browser sessions is, on current evidence, not fully achievable — this should be surfaced as an explicit constraint, not solved silently.** As found in §3, a CDP
`Page` handle cannot survive its owning process dying, restart or not —
this is a hard boundary, not an engineering gap. The realistic version of
"graceful restart" is: (a) SIGTERM handler that finishes in-flight
requests and does the existing browser-close cleanup rather than getting
SIGKILLed mid-request (new — see §3), (b) an explicit user-facing signal
(closed SSE stream + a documented reconnect story) rather than a silent
hang, and (c) accepting that any open browser tabs are lost on restart and
making that visible (e.g. logging session IDs that were torn down by the
restart) rather than leaving callers to discover it via a confusing "no
active browser session" error on their next tool call. Planning should
size this as "make restart loud and clean," not "make restart invisible"
— the latter isn't possible given the CDP process-lifetime constraint.

**Auth-token distribution has a ready-made answer already in Tyler's own toolchain that the requirements doc's Open Questions didn't connect.** The
project's own `CLAUDE.md`/dotfiles ecosystem already has a `secrets` role
using 1Password (`bootstrap/roles/secrets` per the dotfiles repo's
`CLAUDE.md`, referenced from this session's system prompt) and this
project's README explicitly says the whole point is not making the user
hand-manage more machine state. A bearer token stored in a file under
`~/.stapler-mcp/` (generated once at first daemon start, `0600`
permissions — mirroring the existing lock file's `0o600` in
`lock.rs:38`) and read directly into `mcp-servers.json`'s `headers` field
is the lowest-friction option consistent with what's already in this repo
(no existing 1Password-CLI dependency inside `stapler-mcp` itself) — worth
flagging in planning as the default unless there's a reason to reach for
1Password specifically.
