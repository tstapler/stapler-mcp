# Implementation Plan: mcp-streamable-http

**Feature**: Host MCP directly over Streamable HTTP from `stapler-mcp --daemon`, additive to the existing stdio thin-client path, bridging the daemon's deliberately `!Send` core to `rmcp`'s `Send`-bound HTTP transport via an in-process channel actor.
**Date**: 2026-09-14
**Status**: Ready for implementation
**ADRs**: ADR-0001 (Send/!Send channel-bridge architecture), ADR-0002 (HTTP transport mode, bind/auth, and token-distribution design)

**Correction to carry forward**: `requirements.md` and all six `research/*.md` files describe `ThinClient` as having 23 `#[tool]` methods. Direct read of `crates/cli/src/thin_client.rs` at current `HEAD` (`628ec11`) shows **27** `#[tool]`-annotated methods (`grep -c '^    #\[tool($' crates/cli/src/thin_client.rs` → 27; `grep -c '    async fn'` → 27) — commits `51def0e` ("add checkbox set-checked, history nav, resize, and find tools") and `107e1e4` ("add browser screenshot, evaluate, and fill_form tools") landed after the number "23" was written into the requirements/research artifacts. This plan uses **27** everywhere a tool count matters. None of the research's architectural conclusions depend on the exact count — the design is unaffected — but every task below cites the real, current file contents.

---

## Domain Glossary
*(Ubiquitous language — every domain term that appears as a type, method, or variable name. Exact names here must be used consistently in code, tests, and comments.)*

| Term | Definition | Notes |
|------|-----------|-------|
| `DaemonTransport` | Trait abstracting "how does an `McpRouter` reach the daemon core": `fn call(&self, tool: &'static str, params: serde_json::Value) -> impl std::future::Future<Output = Result<serde_json::Value, String>> + Send` (return-position-impl-trait-in-trait, explicit `+ Send`) | The single seam separating the stdio and HTTP tool-call paths; only new trait this project introduces. The `+ Send` bound is not optional: `rmcp-macros-2.2.0/src/tool.rs:343-383` unconditionally rewrites every `#[tool]`-annotated method into `Pin<Box<dyn Future<Output = ReturnType> + Send + '_>>`, and `ChannelTransport` must also satisfy `StreamableHttpService`'s `S: Send` bound (`tower.rs:571-577`), so `rmcp`'s `local`/`#[tool(local)]` escape hatch isn't usable here — implementations still write a plain `async fn call(...)` body; the bound just has to appear on the trait |
| `SocketTransport` | `DaemonTransport` impl that dials the existing Unix socket per call | Behavior-identical extraction of today's `call_daemon` free function in `thin_client.rs` |
| `ChannelTransport` | `DaemonTransport` impl that sends a `BridgeMessage` over the in-process bridge channel instead of dialing a socket | New; holds a cloned `mpsc::Sender<BridgeMessage>` |
| `McpRouter<T: DaemonTransport>` | Generalized/renamed `ThinClient`; the single `rmcp` `#[tool_router]`/`ServerHandler` type holding all 27 tool definitions, generic over which `DaemonTransport` reaches the daemon | Renamed from `crates/cli/src/thin_client.rs`'s `ThinClient`; file renamed to `crates/cli/src/mcp_router.rs` |
| `BridgeMessage` | Type alias for `(stapler_mcp_core::protocol::Request, tokio::sync::oneshot::Sender<stapler_mcp_core::protocol::Response>)` | The payload carried on the bridge channel |
| bridge channel / bridge consumer task | The bounded `tokio::sync::mpsc::Sender/Receiver<BridgeMessage>` plus its single `spawn_local` consumer loop that serializes every channel-originated request through `Daemon::handle_request` | Mirrors how `Daemon::run`'s existing accept loop already serializes Unix-socket clients today; its `recv()` loop races against `Daemon`'s `CancellationToken` the same way `Daemon::run_cancellable` does, and its `spawn_local` `JoinHandle` is captured so `run_daemon`'s single `tokio::join!` can await it (bounded by a grace timeout) before `shutdown_cleanup` runs |
| `Daemon::handle_request` | New `pub async fn(&self, req: Request) -> Response` (renamed from the existing private `dispatch`) | Shared entrypoint both the Unix-socket loop and the bridge consumer call |
| `Daemon::request_shutdown` | New `pub fn(&self)` setting the existing `shutdown: Rc<Cell<bool>>` flag **and** cancelling `Daemon`'s own `CancellationToken` (see below), in one call | The single method every shutdown trigger funnels through: the internal `SHUTDOWN_TOOL` dispatch arm (`daemon.rs:54-57`, updated to call this instead of setting the flag directly) and the SIGTERM handler both call it — so a shutdown requested over the Unix socket, the bridge channel, or SIGTERM all produce the identical cancellation signal |
| `Daemon`'s `CancellationToken` | `tokio_util::sync::CancellationToken` field owned by `Daemon` itself; exposed via `pub fn cancellation_token(&self) -> CancellationToken` (cheap `.clone()` — internally `Arc`-backed) | Cancelled by `request_shutdown`; raced against by `Daemon::run_cancellable`'s accept loop, the bridge consumer's `recv()` loop, and the HTTP server's `axum::serve(...).with_graceful_shutdown(...)` — the single signal all three shutdown-sensitive loops select against. New direct dependency `tokio-util`, needed by **both** `crates/core` (the field itself) and `crates/cli` (constructing the HTTP graceful-shutdown future) |
| `Daemon::run_cancellable` | New `pub async fn(&self, socket: &S, sock_path: &str) -> Result<(), PortError>` — cancellation-aware sibling of the existing `Daemon::run`, racing `listener.accept()` against `self.cancellation_token().cancelled()` via `tokio::select!` | Used only by the native accept loop (`main.rs`); `Daemon::run` itself is left byte-for-byte unchanged, so `crates/wasm/src/lib.rs:257`'s existing call site needs no edit — wasm has no signal handling and stays out of this feature's scope |
| `shutdown_cleanup` | New `async fn(daemon: Rc<Daemon>, browser: Rc<NativeBrowser>)` (exact signature TBD at implementation time) factored out of the existing post-`daemon.run()` sequence (today inline at `main.rs:462-485`, unchanged in shape — just moved) | Called from exactly one place: after `run_daemon`'s single `tokio::join!` (accept loop + bridge consumer + optional HTTP server, each awaited with a bounded grace timeout) completes — regardless of whether shutdown was triggered by `SHUTDOWN_TOOL` or `SIGTERM`. Exactly one cleanup code path, not two |
| `Mcp-Session-Id` | `rmcp`'s own HTTP/MCP-protocol session identifier | Moot under this plan's `stateful_mode: false` decision — listed here only to distinguish it from the next term, per `research/features.md`'s finding that the two are easy to conflate |
| `ports::SessionId` | This codebase's existing, unrelated concept: an application-level handle to a persistent browser tab (`crates/core/src/ports.rs:141`) | Orthogonal to any HTTP transport session; unaffected by this feature |
| `BearerToken` | Newtype wrapping the daemon's HTTP auth secret | Constant-time `PartialEq`; never `Debug`-printed or logged |
| http-token file | `~/.stapler-mcp/http-token`, `0600`-permissioned, generated once via CSPRNG on first daemon start with HTTP enabled | Mirrors `daemon.lock`'s existing `0o600` pattern (`crates/native/src/lock.rs:38`) |
| http-port file | `~/.stapler-mcp/http-port`, plain-text port number, (re)written by the daemon on every startup — present with the current port when `STAPLER_MCP_HTTP_PORT` was set at that startup, removed otherwise | Companion to the http-token file (`http_port_path`, Task 3.2.1c); the persisted source of truth `--status`/`--print-config` read from, since the systemd/launchd unit's `Environment=STAPLER_MCP_HTTP_PORT=...` is scoped to the daemon's own process and is never exported to an operator's interactive shell (`ux.md` Surfaces 1, 3, 4, 8) |
| `STAPLER_MCP_HTTP_PORT` | Opt-in env var, read only by the daemon process itself at startup, that enables the HTTP listener and selects its port | Absent by default — today's stdio-only behavior is the unmodified default; doubles as this project's feature flag (see Risk Control). On daemon startup this value is persisted to the http-port file so that separate CLI invocations (`--status`, `--print-config`) can discover it without needing this var set in their own shell environment |
| `StreamableHttpServerConfig` | `rmcp`'s per-listener config struct (`rmcp-2.2.0/src/transport/streamable_http_server/tower.rs:60-120`) | This plan sets `stateful_mode: false`, `json_response: true` |
| `require_bearer_token` | Hand-rolled `axum::middleware::from_fn_with_state` performing the bearer check and emitting the distinct rejected-request log line | New, in `crates/cli/src/http_server.rs` |
| `BRIDGE_CHANNEL_CAPACITY` | Fixed capacity constant (64) for the bounded bridge `mpsc` channel | Bounds memory under a request burst — the same failure shape as the already-patched `GHSA-9pj6-vhgr-3mwh` session-leak DoS if left unbounded |
| `REQUEST_TIMEOUT` | Fixed timeout constant (30s) bounding how long `Daemon::handle_request` waits on a registered tool handler's future before returning a timeout error | Wraps the tool-dispatch arm inside `handle_request` (Task 2.1.2a) — the single point both the Unix-socket accept loop (via `handle_request_bytes`) and the bridge consumer (Task 2.2.1c) already funnel through, so one change bounds both paths against a hung real-world call (pre-mortem.md P1 #1) |
| `http_token_path` | New `crates/core/src/paths.rs` function returning `{base_dir}/http-token` | Follows the existing `socket_path`/`lock_path`/`log_path` pattern exactly |

---

## Pattern Decisions

| Component | Pattern Chosen | Source | Alternative Rejected | Reason |
|-----------|---------------|--------|---------------------|--------|
| Tool-call dispatch seam (`McpRouter`) | Strategy (GoF), via `DaemonTransport` trait | GoF | Three independent tool-list definitions (separate stdio `ServerHandler` + HTTP `ServerHandler` + `Daemon` dispatcher) | Maintenance trap — adding tool #28 would require 3 edit sites instead of 1 (`requirements.md` Rabbit Holes; `architecture.md` "duplicated tool-list risk") |
| `!Send` core ↔ `Send` HTTP bridge | Actor pattern (bounded `mpsc` + `oneshot`, single `spawn_local` consumer) | Tokio community pattern (Ryhl, "Actors with Tokio"); `build-vs-buy.md` §6 | `Arc<Mutex<...>>`/`Arc<tokio::sync::Mutex<...>>` rewrite of `Daemon`/`NativeBrowser` | 65+ `Rc<RefCell<...>>` construction/access sites in the 3466-line `crates/native/src/browser.rs` (verified `grep -c`), unaudited lock-ordering safety, and forces the cost onto the `crates/wasm` adapter (confirmed still live, `crates/wasm/src/lib.rs:17,52`) which has no use for `Send` (`stack.md` §3, `architecture.md` Option A) |
| `!Send` core ↔ `Send` HTTP bridge (alt. 2) | (same as above) | | Two separate tokio runtimes bridged by a channel | Strictly more moving parts than one `current_thread` runtime + `spawn_local` (for the `!Send` consumer) + `tokio::spawn` (for the `Send` axum/hyper accept loop, which works fine on a `current_thread` runtime per `architecture.md`'s Option C finding) — no genuine multi-core need exists for "a handful of concurrent local sessions" (`stack.md` §3) |
| `!Send` core ↔ `Send` HTTP bridge (alt. 3) | (same as above) | | Hand-rolled HTTP+SSE with the thin client still doing MCP translation, just dialing HTTP instead of a Unix socket | Relocates the `Send`/`!Send` problem instead of solving it; doesn't eliminate the per-session thin-client process (issue #37's actual goal is untouched), and a bespoke `{tool,params}`-over-HTTP endpoint isn't real MCP-over-HTTP, failing the item's own Success Metric (`build-vs-buy.md` §2) |
| HTTP session lifecycle mode | Stateless (`stateful_mode: false`) + `json_response: true` | `rmcp` config, informed by `pitfalls.md` §5 | Default `stateful_mode: true` + SSE-framed `json_response: false` | Avoids the session-restart-breaks-live-connection failure class and the session-table-leak CVE shape (`GHSA-9pj6-vhgr-3mwh`) entirely; this daemon's 27 tools are all request/response, no server-push in use, so SSE framing buys nothing |
| Bearer-token check | Hand-rolled `axum::middleware::from_fn_with_state` (Decorator, GoF) | GoF; `stack.md` §5 | `tower-http`'s `ValidateRequestHeaderLayer::bearer` | Verified friction: `StreamableHttpService`'s `BoxResponse` body type doesn't implement `Default`, which `ValidateRequestHeaderLayer::bearer` requires when applied directly around the service (`stack.md` §5); and it has no hook for the distinct rejected-request log line `requirements.md`'s Observability Requirements demand. A ~20-line hand-rolled middleware avoids both and drops the `tower-http` dependency entirely |
| Bearer-token type | Newtype `BearerToken(String)` with constant-time `PartialEq` | type-driven-design | Raw `String` compared with `==` | Prevents both an accidental non-constant-time timing side-channel and a stray `Debug`/log of the raw secret anywhere else in the codebase |
| HTTP listener enablement | Opt-in via `STAPLER_MCP_HTTP_PORT` env var (absent = HTTP disabled entirely) | Feature Toggle (Fowler) | Always-on HTTP listener bound to a fixed default port | No feature-flag system exists in this repo (`requirements.md` Risk Control); an always-on listener would silently open a new local network attack surface even for machines that haven't opted into the launcher unit yet |
| HTTP bind failure | Log distinctly, degrade to stdio-only, daemon keeps running | — | Abort the entire daemon process on HTTP bind failure | A stale/zombie process or unrelated program squatting the port would otherwise crash-loop the *whole* daemon (including the already-working stdio path) under a `Restart=on-failure` unit — turns a narrow HTTP-only failure into a total outage (`pitfalls.md` §3) |
| Auth-token distribution to `mcp-servers.json` | Document a manual `stapler-mcp --print-config` copy-paste into a machine-local, non-committed config; never a literal token in the git-tracked file | Jupyter token-on-first-run precedent (`ux.md` §3) | Literal token inlined into `mcp-servers.json`'s `headers` block, or relying on `${ENV_VAR}` substitution | `mcp-servers.json` is git-tracked and `llm-sync`-mirrored (`pitfalls.md` §2); Claude Code's `${ENV_VAR}` substitution in `http`-transport `headers` is reported broken in linked, unresolved GitHub issues — not safe to design around |
| Port discovery for `--status`/`--print-config` | Read the persisted `~/.stapler-mcp/http-port` file (written by the daemon on every startup); `STAPLER_MCP_HTTP_PORT` in the invoking shell's own environment is consulted only as a fallback, for the case of running `--status`/`--print-config` manually in the same shell the daemon was foreground-started from | Mirrors the http-token file precedent (row above) | Reading `STAPLER_MCP_HTTP_PORT` from the invoking shell's environment as the sole source | The systemd/launchd unit's `Environment=STAPLER_MCP_HTTP_PORT=...` scopes the var to the unit's own process, never the operator's interactive shell (`ux.md` Surfaces 1, 3, 4, 8) — a shell-env-only read would report "HTTP disabled" for a daemon that is actually running with HTTP enabled via the persistent service |

---

## Tech Debt Disposition

| Area | Existing Issue | Disposition | Justification |
|------|----------------|--------------|----------------|
| `crates/core/src/daemon.rs`'s `dispatch` method (private) | Blocks the bridge consumer task from calling the dispatch core directly with a typed `Request` (it can currently only be reached via the byte-oriented `handle_request_bytes`, forcing a redundant serialize/deserialize round trip for an in-process caller) | **Refactor-first** | Tiny, mechanical, and load-bearing for every later story: rename `dispatch` → `pub async fn handle_request`, update `handle_request_bytes` to call it. Sequenced as Story 2.1.1, before any bridge/channel work depends on it |
| `crates/native/src/browser.rs` (3466 lines, 65+ `Rc<RefCell<...>>` sites, `NewSessionSlotGuard`/per-session `tokio::sync::Mutex` TOCTOU guards) | Large, deliberately `!Send`, stateful file — not itself a SOLID/DDD violation (`architecture.md`'s own read: "not a hotspot to refactor first... the risk is purely the `Send`/`!Send` transport mismatch"), but any careless touch here is high-blast-radius | **Isolate via seam** | This feature's entire architecture (the channel bridge, Pattern Decision row 2) exists specifically so `browser.rs`'s internals — and every other `crates/native`/`crates/wasm` port implementation — need **zero** changes. No task in this plan edits `browser.rs` |
| `crates/cli/src/main.rs::run_daemon` (491 lines, repetitive `daemon.register(...)` boilerplate, lines 100-457) | Long procedural function, but its length is repeated registration boilerplate, not entangled logic (`architecture.md`'s own read) | **Extend as-is** | Adding the channel-consumer task spawn, the opt-in HTTP-listener spawn, and the SIGTERM handler is additive to the function's existing shape (already a sequence of "construct dependency → register/wire it" steps) and does not compound an existing violation — it doesn't touch the 27 `daemon.register` calls at all. **Revisited per architecture-review's Concern 2**: "doesn't touch the 27 `daemon.register` calls" answers a narrower question than whether the plan's own additions are safe. They touch the existing post-`daemon.run()` cleanup's sole-ownership invariant directly — `Rc`-wrapping `daemon` (Task 2.2.1b), a second concurrent task competing for `Rc<NativeBrowser>`/`Rc<Daemon>` ownership (the bridge consumer), and a signal handler (Epic 5.1) all touch the same `Rc::get_mut(&mut browser)` sole-ownership requirement the existing cleanup at `main.rs:462-485` depends on. This plan keeps that invariant honest rather than sidestepping it: the accept loop, bridge consumer, and (if enabled) HTTP server are joined via a single `tokio::join!` before `shutdown_cleanup` ever runs (Epic 2.2/5.1), so by the time `shutdown_cleanup` executes, every task that held an `Rc<NativeBrowser>`/`Rc<Daemon>` clone has already exited and dropped it — sole ownership is genuinely, not assumedly, true at that point |
| `crates/core/src/daemon.rs::Daemon::run`'s accept loop | Its `loop { listener.accept().await? ...; if shutdown.get() { return } }` shape only re-checks the shutdown flag after a new connection arrives — insufficient for SIGTERM to unblock it promptly, per architecture-review's Blocker 1 | **Extend via a new sibling method** | `Daemon::run` itself is left completely unchanged (preserves `crates/wasm/src/lib.rs:257`'s existing call site, and the "Isolate via seam" row below still holds for `browser.rs`). A new `Daemon::run_cancellable` (Task 2.1.1d) wraps the same per-connection body in a `tokio::select!` against `Daemon`'s own `CancellationToken`, used only by the native accept loop in `main.rs`. This is a small, additive, non-invasive change to `daemon.rs` — architecture-review's own remediation names this exact shape ("a thin cancellation-aware wrapper around it") as acceptable, and this row makes that budget explicit rather than leaving it implied by the browser.rs-only framing below |

---

## Migration Plan

N/A — no schema or data changes. This feature adds a new transport and new on-disk artifacts (`~/.stapler-mcp/http-token`), not a data migration; the token file is generated fresh on first HTTP-enabled daemon start, with no prior state to migrate from or reversibility concern beyond "delete the file to force regeneration."

## Observability Plan

- **Logs** (all via the existing ad hoc `eprintln!` style — no structured logging framework exists in this codebase; introducing one is out of proportion to this feature):
  - HTTP connection start/end, each tagged with a per-connection correlation id and peer address — satisfies `requirements.md`'s "log HTTP session creation/teardown" at the connection level, since `stateful_mode: false` (Pattern Decisions) means there is no persistent `Mcp-Session-Id` lifecycle to log instead.
  - Every rejected (missing/invalid bearer token) request, logged distinctly from tool-call errors, stating the failure class (`"missing Authorization header"` / `"invalid bearer token"`) but never the token value itself.
  - HTTP bind failure, logged distinctly from other startup failures (`"stapler-mcp: HTTP port {port} already in use ..."`), so an operator can `grep` to tell "port taken" apart from any other startup error.
  - Bridge-channel backpressure: a warning line when a request waits >250ms on `bridge_tx.send(...).await`, so a slow response under concurrent HTTP load is legible as "queued behind the daemon core" rather than looking like a stuck server.
  - Bridge consumer panic: a distinct `"stapler-mcp: bridge consumer panicked ..."` line (Task 2.2.1e), so a handler panic is legible as exactly that rather than surfacing only as a generic "channel closed" error on every subsequent HTTP-originated call (`pre-mortem.md` P1 #2).
  - SIGTERM receipt and the resulting shutdown sequence, at the same level as the existing `SHUTDOWN_TOOL`-triggered cleanup log lines in `main.rs`.
  - `daemon.log` gains an explicit `.mode(0o600)` at open time (`crates/native/src/spawn.rs`), closing the gap `pitfalls.md` §2 flagged (today it inherits the process umask).
- **Metrics**: none — no metrics framework exists in this codebase today (confirmed: no `metrics`/`prometheus` crate in any `Cargo.toml`), and introducing one for a single-user local daemon is disproportionate to this feature. Logging-only observability, consistent with the existing precedent and the "solo project, no dedicated QA" constraint.
- **Alerts**: no alerting infrastructure exists (local-only tool, no oncall); no new alerts required.

## Risk Control

- **Feature flag**: `STAPLER_MCP_HTTP_PORT` (unset by default). Unset = today's stdio-only behavior, completely unmodified code path. Set = HTTP listener starts on that port. This is the closest equivalent to a feature flag this codebase has, and it doubles as the port configuration (Pattern Decisions).
- **Rollback procedure**: unset `STAPLER_MCP_HTTP_PORT` (or stop/remove the systemd/launchd unit) and/or point the consumer's `mcp-servers.json` entry back at the existing stdio `command`/`args` config — the stdio path is untouched and stays the default. No data to roll back (Migration Plan: N/A).
- **Staged rollout**: none — single local daemon, no external users, per `requirements.md`. Full rollout on merge; adoption is inherently staged per-machine already, since each machine's `mcp-servers.json`/launcher-unit setup is a separate manual step (`ux.md` §3).

## Unresolved Questions

- [ ] Does `rmcp`'s `#[tool_router]` proc macro support a generic `impl<T: DaemonTransport> McpRouter<T>` block, **with `DaemonTransport::call` returning the `Send`-bounded RPITIT signature this plan now specifies** (Domain Glossary; Task 1.2.1b)? The macro-mechanics half of this question is already answered — `tool_router.rs:88`'s `item_impl.generics.split_for_impl()` confirms generic impl blocks are structurally supported — so the spike's real job is confirming the `Send`-bounded trait shape compiles inside it, not the generic-impl mechanics alone. — blocks Story 1.2.1 — owner: implementer, resolved by Task 1.2.1a's spike, which must exercise this literal trait shape (documented fallback: two non-generic router structs sharing tool definitions via a `macro_rules!` template, following the `register_browser_tool!` precedent in `project_plans/browser-automation/decisions/ADR-0003-daemon-wiring-boilerplate-macro.md`, rather than one generic type). **A spike failure for any reason — including this one — triggers a scoped re-plan of Phase 1.2 onward before implementation proceeds past it**, since Tasks 1.2.1b through Phase 8 are written in the generic-type voice and would need their literal wording revisited under the fallback.
- [ ] Is `47439` (this plan's proposed default `STAPLER_MCP_HTTP_PORT`) actually free on Tyler's real machines? — blocks Story 5.2.1 (shipping the systemd/launchd templates with a default value) — owner: Tyler, `lsof -i :47439` (or `ss -ltnp`) on each machine before enabling the unit.
- [ ] Does `axum::body::Body` implement `Default`? (`stack.md` §5 flagged this as unverified — `axum` isn't vendored locally). This plan's hand-rolled auth middleware (Pattern Decisions) doesn't depend on this fact, but confirm no other axum API surface used in Task 3.1.* relies on it — blocks nothing directly; owner: implementer, resolved by `cargo build` once `axum` is added in Task 1.1.1b.
- [ ] Whether the sibling `dotfiles` repo's bootstrap should install the systemd/launchd unit automatically, vs. this repo shipping it only as a documented `.example` template — `requirements.md`'s own Scope leaves this open, and it's `dotfiles`-repo-owned, not this repo's call. This plan ships `.example` templates only (Story 5.2.1); `ux.md` argues the "install automatically" follow-up materially affects whether the UX trade-off (persistent-daemon failure mode) is justified — owner: Tyler, tracked as a follow-up outside this repo.
- [ ] Manual confirmation that Claude Code actually recovers (auto-reconnects) from a daemon restart mid-HTTP-session, rather than requiring a full Claude Code restart, given `ux.md`'s cited open Claude Code issues (`#21721`, `#11868`) in this exact area — not automatable against a real Claude Code client — blocks final ship sign-off (Phase 9) — owner: Tyler, manual check in Task 9.1.1.
- **Accepted residual risk, not tracked as an open question requiring resolution before ship**: the bridge consumer's `catch_unwind` guard (Task 2.2.1e, pre-mortem.md P1 #2) only recovers from a Rust panic inside the per-request dispatch. It does not, and cannot, recover the bridge consumer task from a non-panic death — an `abort()`, an OOM-kill of the whole process, or any other fatal signal — which would still drop the paired `mpsc::Receiver` and leave every subsequent `ChannelTransport::call` failing until a manual/`Restart=on-failure` daemon restart. A full supervisor/restart mechanism for the bridge-consumer task specifically (as opposed to `Restart=on-failure` restarting the whole daemon process, which this plan already relies on) is explicitly out of scope for this pass — consistent with this project's existing "no feature-flag system, no staged rollout" risk-tolerance level (`requirements.md` Risk Control).

## Dependency Visualization

```
Phase 1 — Foundations
  Epic 1.1 Cargo deps (axum, rmcp feature, tokio-util, rand, subtle)
  Epic 1.2 DaemonTransport seam (McpRouter<T>, SocketTransport)  ─┐
                                                                    │ behavior-preserving,
                                                                    │ no HTTP yet — safe to ship alone
                                                                    ▼
Phase 2 — Bridge (the !Send/Send seam itself)
  Epic 2.1 Daemon::handle_request + Daemon::request_shutdown
  Epic 2.2 mpsc/oneshot bridge channel + spawn_local consumer     ─┐
  Epic 2.3 ChannelTransport (implements DaemonTransport)           │
                                                                    ▼
Phase 3 — HTTP listener
  Epic 3.1 axum Router + StreamableHttpService wiring
  Epic 3.2 Opt-in wiring in run_daemon (STAPLER_MCP_HTTP_PORT)    ─┐
                                                                    ▼
Phase 4 — Auth                        Phase 5 — Lifecycle
  Epic 4.1 token gen/storage            Epic 5.1 SIGTERM + shutdown coordination
  Epic 4.2 require_bearer_token         Epic 5.2 systemd/launchd templates + bind-failure handling
         │                                      │
         └──────────────┬───────────────────────┘
                         ▼
Phase 6 — Observability (logging, redaction, daemon.log mode)
                         │
                         ▼
Phase 7 — UX (--status, --print-config, README rewrite)
                         │
                         ▼
Phase 8 — Testing (unit, integration, dual-transport, concurrency, CI hardening)
                         │
                         ▼
Phase 9 — Manual verification & ship prep
```

Phases 4 and 5 are mutually independent (both depend only on Phase 3 completing) and may be implemented in either order or in parallel by separate work streams; everything from Phase 6 onward depends on both.

---

## Phase 1: Foundations

### Epic 1.1: Cargo dependencies
**Goal**: Every crate the HTTP path needs is declared and resolves, with no behavior change yet.

#### Story 1.1.1: Add rmcp's HTTP transport feature and the new direct dependencies
**As a** implementer, **I want** `crates/cli/Cargo.toml` and `crates/core/Cargo.toml` to declare everything the HTTP path and the cancellation-aware shutdown design (Epic 2.1/2.2/5.1) need, **so that** subsequent stories compile against real types instead of stubs.
**Acceptance Criteria**:
- `cargo build -p stapler-mcp` succeeds after the dependency edits, with `rmcp` resolved to `>=2.2.0, <3.0.0`.
  - *Given* `crates/cli/Cargo.toml:14` currently reads `rmcp = { version = "2", features = ["server", "macros", "transport-io", "schemars"] }`, *When* it's changed to `rmcp = { version = "2.2", features = ["server", "macros", "transport-io", "schemars", "transport-streamable-http-server"] }`, `axum = "0.8"` and `tokio-util = "0.7"` are added to `[dependencies]`, and `tokio`'s `features` list gains `"signal"` (currently `["rt", "macros", "time", "net", "io-util", "sync"]`, needed for `tokio::signal::unix::signal(...)`, Task 5.1.1a), *Then* `cargo build -p stapler-mcp` succeeds and `Cargo.lock`'s `rmcp` entry shows version `2.2.0`.
- `cargo build --workspace` succeeds with `crates/core` able to construct and `select!` on a `tokio_util::sync::CancellationToken` (`Daemon`'s new field, Task 2.1.1b/2.1.1d).
  - *Given* `crates/core/Cargo.toml`'s non-dev `tokio` dependency currently declares only `features = ["rt"]` and neither `tokio-util` nor `tokio`'s `"macros"` feature (needed for `tokio::select!` inside `Daemon::run_cancellable`), *When* `tokio-util = "0.7"` is added to `crates/core/Cargo.toml`'s `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` and `"macros"` is added to that same block's `tokio` `features` list, *Then* `cargo build --workspace` (including `crates/wasm`'s wasm32 target, which doesn't pull in this `cfg`-gated block) succeeds.
**Files**: `crates/cli/Cargo.toml`, `crates/core/Cargo.toml`, `Cargo.lock`

##### Task 1.1.1a: Edit `crates/cli/Cargo.toml`'s `rmcp` line (~2 min)
- Change `version = "2"` → `version = "2.2"`; add `"transport-streamable-http-server"` to the `features` list.
- Files: `crates/cli/Cargo.toml`

##### Task 1.1.1b: Add `axum` and `tokio-util` as direct dependencies; add `tokio`'s `"signal"` feature (~3 min)
- Add `axum = "0.8"` and `tokio-util = "0.7"` to `crates/cli/Cargo.toml`'s `[dependencies]`. Add `"signal"` to the existing `tokio = { version = "1", features = [...] }` line's `features` list (required by Task 5.1.1a's `tokio::signal::unix::signal(...)`; adversarial-review's Blocker 1 — without it, `cargo build -p stapler-mcp` fails once Story 5.1.1 lands). After editing, run `cargo build -p stapler-mcp` once and check the compiler's own missing-feature diagnostics for any other transitively-required `tokio` feature this plan's usage needs (e.g. anything `tokio_util::sync::CancellationToken` itself pulls in) rather than assuming the feature list above is exhaustive.
- Files: `crates/cli/Cargo.toml`

##### Task 1.1.1c: Add `tokio-util` and `tokio`'s `"macros"` feature to `crates/core/Cargo.toml` (~2 min)
- Add `tokio-util = "0.7"` and `"macros"` (to the existing `tokio = { version = "1", features = ["rt"] }` line) in `crates/core/Cargo.toml`'s `[target.'cfg(not(target_arch = "wasm32"))'.dependencies]` block — needed for `Daemon`'s new `CancellationToken` field and `Daemon::run_cancellable`'s `tokio::select!` (Task 2.1.1b/2.1.1d). Scoped to the non-wasm32 target block already used for this crate's other native-only tokio usage, so `crates/wasm`'s build is unaffected.
- Files: `crates/core/Cargo.toml`

##### Task 1.1.1d: Resolve and commit the updated lockfile (~3 min)
- Run `cargo build --workspace`; confirm `axum`, `sse-stream`, `uuid` appear as new entries in `Cargo.lock` and `hyper`/`tower`/`tower-service`/`http*`/`bytes` resolve to their already-locked versions (no upgrade cascade) per `research/stack.md` §4.
- Files: `Cargo.lock`

#### Story 1.1.2: Add `rand` (native) and `subtle` (core) for token generation/comparison
**As a** implementer, **I want** a CSPRNG and a constant-time comparison available where they're needed, **so that** Phase 4's `BearerToken` work has real primitives to build on.
**Acceptance Criteria**:
- `cargo build --workspace` succeeds with `rand` declared in `crates/native/Cargo.toml` and `subtle` declared in `crates/core/Cargo.toml`.
  - *Given* neither crate declares `rand` or `subtle` directly today (confirmed: `grep -rn "^rand\|^subtle" crates/*/Cargo.toml` has no hits), *When* `rand = "0.9"` is added to `crates/native/Cargo.toml` (matching the already-locked `0.9.5`, not the also-present `0.10.2`, to avoid a third resolved version) and `subtle = "2"` is added to `crates/core/Cargo.toml`, *Then* `cargo build --workspace` succeeds with no new duplicate major version of `rand` in `Cargo.lock`.
**Files**: `crates/native/Cargo.toml`, `crates/core/Cargo.toml`, `Cargo.lock`

##### Task 1.1.2a: Add `rand = "0.9"` to `crates/native/Cargo.toml` (~2 min)
- Files: `crates/native/Cargo.toml`

##### Task 1.1.2b: Add `subtle = "2"` to `crates/core/Cargo.toml` (~2 min)
- Files: `crates/core/Cargo.toml`

##### Task 1.1.2c: `cargo build --workspace`, commit lockfile (~2 min)
- Files: `Cargo.lock`

---

### Epic 1.2: `DaemonTransport` seam (behavior-preserving refactor)
**Goal**: `ThinClient` becomes `McpRouter<T: DaemonTransport>`, with `SocketTransport` reproducing today's stdio behavior exactly — zero observable change, verified by the existing test suite passing unmodified in assertions.

#### Story 1.2.1: Extract `DaemonTransport`/`SocketTransport`, generalize `ThinClient` → `McpRouter`
**As a** implementer, **I want** the single 27-tool `#[tool_router]` block parameterized over how it reaches the daemon, **so that** Phase 3's HTTP path reuses it verbatim instead of duplicating it (Pattern Decisions row 1).
**Acceptance Criteria**:
- The stdio path's behavior is unchanged: `daemon_architecture_and_tools_round_trip` (`crates/cli/tests/daemon_ping.rs`) and `tools/list` schema coverage (`crates/cli/tests/tool_schema.rs`) both still pass.
  - *Given* `crates/cli/tests/daemon_ping.rs`'s `daemon_architecture_and_tools_round_trip` test drives a real spawned `--daemon` process over the Unix socket via `client::call`, *When* `ThinClient` is renamed to `McpRouter<SocketTransport>` (with `SocketTransport` as `McpRouter`'s default type parameter so `McpRouter::new()` keeps compiling) and every `#[tool]` method body changes from `call_daemon("tool_name", params.0).await.map(Json)` to `self.transport.call("tool_name", params.0).await.map(Json)`, *Then* `cargo test -p stapler-mcp --test daemon_ping` still passes with identical assertions.
- `McpRouter::registered_tools()` still reports all 27 tools with non-empty descriptions and matching schemas.
  - *Given* `crates/cli/tests/tool_schema.rs:38` currently does `#[path = "../src/thin_client.rs"] mod thin_client;` and calls `ThinClient::registered_tools()`, *When* the file is renamed to `mcp_router.rs` and the test updated to `#[path = "../src/mcp_router.rs"] mod mcp_router;` / `McpRouter::registered_tools()`, *Then* `cargo test -p stapler-mcp --test tool_schema` passes with the same 27-tool assertion set (extended per Task 4 to 27, matching Task 1.2.1's corrected count).
**Files**: `crates/cli/src/thin_client.rs` → `crates/cli/src/mcp_router.rs`, `crates/cli/src/transport.rs` (new), `crates/cli/src/main.rs`, `crates/cli/tests/tool_schema.rs`, `crates/cli/tests/daemon_ping.rs`

##### Task 1.2.1a: Spike — confirm `#[tool_router]` works on a generic `impl<T: DaemonTransport> McpRouter<T>` block, using the literal `Send`-bounded `DaemonTransport` shape (~8 min)
- Write a minimal throwaway generic struct with one `#[tool]` method behind `#[tool_router]`/`#[tool_handler]`, generic over a trait with **exactly** Task 1.2.1b's `Send`-bounded RPITIT signature (not a simplified stand-in — a trait without the `Send` bound would pass the spike while hiding the real compile error this plan is designed around; see Domain Glossary and architecture-review's Blocker 2/Concern 1) to confirm it compiles; if it doesn't — for this reason or any other — fall back to two non-generic structs sharing tool definitions via a `macro_rules!` (see ADR-0003 precedent) and treat the failure as triggering a scoped re-plan of Phase 1.2 onward (Tasks 1.2.1b–g, Epic 2.3, Epic 3.1, and Epic 8.1 are all written in the generic-type voice and need their literal wording revisited under the fallback) before implementation proceeds past this task. Record the outcome in this task's own commit message.
- Files: (scratch, not committed) or `crates/cli/src/mcp_router.rs` directly if confirming inline

##### Task 1.2.1b: Define `DaemonTransport` trait in new `crates/cli/src/transport.rs`, with an explicit `Send` bound (~4 min)
- `pub trait DaemonTransport { fn call(&self, tool: &'static str, params: serde_json::Value) -> impl std::future::Future<Output = Result<serde_json::Value, String>> + Send; }` — a return-position-impl-trait-in-trait (RPITIT) with an explicit `+ Send` bound, not a plain `async fn`. Required because every `#[tool]`-annotated method on `McpRouter<T>` is macro-rewritten by `rmcp-macros-2.2.0` into a `Pin<Box<dyn Future<Output = ReturnType> + Send + '_>>` (verified `rmcp-macros-2.2.0/src/tool.rs:343-383`), so the compiler cannot prove `self.transport.call(...)`'s returned future is `Send` for a generic `T: DaemonTransport` unless the trait itself says so — without this bound, the generic `impl<T: DaemonTransport> McpRouter<T>` block from Task 1.2.1d does not compile. (Alternative considered and rejected as the default: `#[trait_variant::make(Send)]` from the `trait_variant` crate, which lets the trait definition stay a plain `async fn` — functionally equivalent, but pulls in a new dependency for no behavior this plan needs; use the explicit RPITIT form unless implementation discovers a concrete reason to prefer `trait_variant`.) Implementations still write ordinary `async fn call(&self, ...) -> Result<...> { ... }` bodies — the bound lives on the trait, not on every impl.
- Files: `crates/cli/src/transport.rs` (new)

##### Task 1.2.1c: Implement `SocketTransport` — move `call_daemon`'s body in unchanged (~4 min)
- Move the existing `call_daemon` function body (`thin_client.rs:35-65`) into `impl DaemonTransport for SocketTransport`, preserving the `ensure_daemon` + `client::call` sequence and `CALL_TIMEOUT` constant exactly.
- Files: `crates/cli/src/transport.rs`

##### Task 1.2.1d: Rename `thin_client.rs` → `mcp_router.rs`, `ThinClient` → `McpRouter<T: DaemonTransport = SocketTransport>` (~5 min)
- Add a `transport: T` field alongside the existing `tool_router` field; update `McpRouter::new()` to build `Self { transport: SocketTransport, tool_router: Self::tool_router() }`.
- Files: `crates/cli/src/mcp_router.rs` (renamed), `crates/cli/src/main.rs` (`mod thin_client;` → `mod mcp_router;`)

##### Task 1.2.1e: Update all 27 `#[tool]` method bodies to call `self.transport.call(...)` (~5 min)
- Mechanical find-replace: `call_daemon("X", params.0).await.map(Json)` → `self.transport.call("X", serde_json::to_value(params.0).map_err(|e| e.to_string())?).await.map(Json)` (or equivalent, matching whatever signature Task 1.2.1b settles on).
- Files: `crates/cli/src/mcp_router.rs`

##### Task 1.2.1f: Update `main.rs::run_thin_client` and `tests/tool_schema.rs`'s `#[path]`/type references (~3 min)
- Files: `crates/cli/src/main.rs`, `crates/cli/tests/tool_schema.rs`

##### Task 1.2.1g: Run the full existing test suite, confirm zero behavior change (~3 min)
- `cargo test --workspace`; all of `daemon_ping.rs`, `tool_schema.rs`, `browser_session.rs`, `docs_index.rs`, `webcrawl.rs` pass unmodified.
- Files: none (verification only)

---

## Phase 2: Bridge (the `!Send`/`Send` seam)

### Epic 2.1: `Daemon::handle_request` + `Daemon::request_shutdown`

#### Story 2.1.1: Expose a typed, public dispatch entrypoint on `Daemon`, and a single cancellation-aware shutdown signal
**As a** implementer, **I want** `Daemon`'s dispatch core reachable without a bytes round trip, and every shutdown trigger (SIGTERM, `SHUTDOWN_TOOL` over any transport) to produce one identical cancellation signal, **so that** the bridge consumer task (Epic 2.2), the native accept loop, and the HTTP server (Phase 3/5) can all react to shutdown promptly instead of only noticing a flag after their next event.
**Acceptance Criteria**:
- `Daemon::handle_request_bytes` and the new `Daemon::handle_request` produce identical results for the same logical request.
  - *Given* a `Daemon` with `"ping"` (built-in) and one registered handler, *When* `daemon.handle_request(Request { tool: "ping".into(), params: None }).await` is called directly, *Then* it returns `Response::ok(json!({"pong": true}))`, identical to what `handle_request_bytes` returns for the equivalent serialized bytes today.
- `Daemon::request_shutdown` sets the same flag `SHUTDOWN_TOOL` sets today, **and** cancels `Daemon`'s own `CancellationToken` in the same call — and `SHUTDOWN_TOOL` dispatched over any transport produces the identical effect, since the dispatch arm now calls this same method instead of setting the flag directly.
  - *Given* a fresh `Daemon`, *When* `daemon.request_shutdown()` is called, *Then* `daemon.shutdown.get() == true` and `daemon.cancellation_token().is_cancelled() == true` (verified via a subsequent `handle_request(Request{tool:"ping",..})` still succeeding, since neither signal short-circuits `handle_request` itself — matching today's `SHUTDOWN_TOOL` semantics at `daemon.rs:54-57`, which this task updates to call `self.request_shutdown()` instead of setting the field inline).
  - *Given* a fresh `Daemon` and a clone of `daemon.cancellation_token()` held by a test task blocked on `.cancelled().await`, *When* a **separate** `daemon.handle_request(Request{tool: SHUTDOWN_TOOL.into(), params: None}).await` call runs (simulating the `SHUTDOWN_TOOL` RPC path, not `request_shutdown()` called directly), *Then* the blocked task's `.cancelled().await` resolves — proving the RPC path and the direct SIGTERM-handler path produce the same cancellation, not two independent ones.
- `Daemon::run_cancellable`'s accept loop unblocks on cancellation without waiting for a new connection.
  - *Given* a `Daemon` running `run_cancellable` against a bound socket with no pending connection, *When* `daemon.request_shutdown()` is called from another task, *Then* `run_cancellable` returns `Ok(())` promptly (bounded by a short test timeout, e.g. 100ms) rather than hanging until a connection arrives.
**Files**: `crates/core/src/daemon.rs`

##### Task 2.1.1a: Rename `dispatch` → `pub async fn handle_request`, update `handle_request_bytes` (~3 min)
- `daemon.rs:51` `async fn dispatch(&self, req: Request) -> Response` → `pub async fn handle_request(&self, req: Request) -> Response`; `handle_request_bytes` (`daemon.rs:42-49`) now calls `self.handle_request(req).await`.
- Files: `crates/core/src/daemon.rs`

##### Task 2.1.1b: Add a `CancellationToken` field, `pub fn request_shutdown(&self)`, and `pub fn cancellation_token(&self) -> CancellationToken`; route the `SHUTDOWN_TOOL` dispatch arm through `request_shutdown` (~5 min)
- Add `cancel: tokio_util::sync::CancellationToken` to `Daemon`'s fields, initialized in `Daemon::new()`. `pub fn request_shutdown(&self) { self.shutdown.set(true); self.cancel.cancel(); }` — thin wrapper setting both signals in one call. `pub fn cancellation_token(&self) -> CancellationToken { self.cancel.clone() }` (cheap — `CancellationToken` is `Arc`-backed internally). Update the existing `SHUTDOWN_TOOL` dispatch arm (`daemon.rs:54-57`, currently `self.shutdown.set(true)`) to call `self.request_shutdown()` instead, so the Unix-socket/bridge-channel RPC path and the SIGTERM handler (Phase 5) both funnel through this one method rather than the RPC path only setting the flag while SIGTERM separately cancels a token nothing else observes.
- Files: `crates/core/src/daemon.rs`

##### Task 2.1.1c: Add unit tests for the new methods and the shared-cancellation behavior (~5 min)
- Mirror the existing `json_handler_should_accept_omitted_params_for_zero_field_input` test style (`daemon.rs:160-169`): `handle_request_should_round_trip_ping_without_bytes_serialization`, `request_shutdown_should_set_the_shutdown_flag_and_cancel_the_token`, `shutdown_tool_dispatch_should_cancel_the_same_token_request_shutdown_does`.
- Files: `crates/core/src/daemon.rs`

##### Task 2.1.1d: Add `Daemon::run_cancellable`, a cancellation-aware sibling of `Daemon::run` (~5 min)
- `pub async fn run_cancellable<S: SocketFactory>(&self, socket: &S, sock_path: &str) -> Result<(), PortError>`, reusing `Daemon::run`'s existing per-connection body (`daemon.rs:80-98`) but replacing the loop's blocking `listener.accept().await` with `tokio::select! { _ = self.cancel.cancelled() => return Ok(()), accept_result = listener.accept() => { let mut conn = accept_result?; ... } }`, keeping the existing post-connection `if self.shutdown.get() { return Ok(()) }` check as a second, redundant exit path (harmless — `request_shutdown` always sets both signals together, so whichever the loop notices first wins). `Daemon::run` itself is left completely unchanged — `crates/wasm/src/lib.rs:257`'s call site needs no edit, consistent with this feature's "no change to the wasm adapter's transport" scope boundary (`requirements.md`'s Out of Scope). Factor the shared per-connection body into a small private helper if that avoids duplicating it between `run` and `run_cancellable`.
- Files: `crates/core/src/daemon.rs`

---

#### Story 2.1.2: Bounded per-request timeout on tool dispatch
**As a** implementer, **I want** `Daemon::handle_request`'s call into a registered tool handler bounded by a timeout, **so that** a single hung real-world call (an infinite JS loop, a stalled CDP command, a dialog blocking the page) can't freeze every concurrent session indefinitely on either the Unix-socket accept loop or the bridge-channel consumer — both of which serialize every request through this exact method, and the whole point of this migration (frictionless concurrent multi-session usage) makes that collision more likely than under the old per-process stdio model (`pre-mortem.md` Failure #1, P1).
**Acceptance Criteria**:
- A tool handler that never resolves causes `handle_request` to return a timeout error after a bounded duration, instead of hanging forever.
  - *Given* a `Daemon` with a registered handler that never completes (`std::future::pending()`), *When* `daemon.handle_request(Request{tool:"hangs".into(),params:None}).await` is called, *Then* it returns within `REQUEST_TIMEOUT` (or a short test-only override) with `Response::err("tool call \"hangs\" timed out after ...")`, not by hanging.
- A hung call doesn't block a subsequent, unrelated call on the same `Daemon` from completing.
  - *Given* the same hung-handler `Daemon`, *When* `daemon.handle_request(hanging_request).await` is issued and, after it returns its timeout error, a **second** `daemon.handle_request(Request{tool:"ping".into(),params:None}).await` is issued, *Then* the second call still returns `Response::ok(json!({"pong": true}))` — proving the timeout actually frees the dispatch core for the next request rather than leaving it wedged.
**Files**: `crates/core/src/daemon.rs`

##### Task 2.1.2a: Add `REQUEST_TIMEOUT` constant and wrap the tool-handler future in `tokio::time::timeout` (~4 min)
- In `handle_request`'s `other => { ... }` arm (`daemon.rs:58-72`, post-rename from Task 2.1.1a), wrap `fut.await` in `tokio::time::timeout(REQUEST_TIMEOUT, fut).await`, mapping the `Err(_elapsed)` case to `Response::err(format!("tool call {other:?} timed out after {REQUEST_TIMEOUT:?}"))`. Add `const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);` alongside the existing `CONN_TIMEOUT` constant (`daemon.rs:21`) — 30s chosen to comfortably exceed any real browser navigation/CDP round trip while still bounding the worst case. `PING_TOOL`/`SHUTDOWN_TOOL` arms are left unwrapped since they never await anything that can hang. This is the single point both `Daemon::run`'s accept loop (via `handle_request_bytes`) and the bridge consumer (Task 2.2.1c, calling `handle_request` directly) go through, so one change bounds both paths per `pre-mortem.md`'s P1 #1.
- Files: `crates/core/src/daemon.rs`

##### Task 2.1.2b: Add unit tests proving timeout-then-recovery (~4 min)
- `handle_request_should_time_out_a_hung_handler_instead_of_blocking_forever`, `handle_request_should_still_serve_subsequent_calls_after_a_timeout` (mirrors Task 2.1.1c's test style), registering a handler (via a raw `Handler` closure, or `json_handler` wrapping a future that never resolves) that returns `std::future::pending()`.
- Files: `crates/core/src/daemon.rs`

---

### Epic 2.2: mpsc/oneshot bridge channel + `spawn_local` consumer

#### Story 2.2.1: Wire the bridge channel into `run_daemon`, joined into a single cancellation-driven shutdown
**As a** implementer, **I want** a bounded channel from `Send` callers into the `!Send` `Daemon`, whose consumer task reacts to shutdown the moment it's requested and is awaited exactly once alongside every other daemon-owned task, **so that** the HTTP listener (Phase 3) has a `Send`-safe way to reach daemon state without touching `Rc<RefCell<...>>` directly, and shutdown has exactly one completion path (architecture-review's Blocker 1, adversarial-review's Blocker 2).
**Acceptance Criteria**:
- A message sent on the bridge channel is processed by `Daemon::handle_request` and the reply reaches the sender via the paired `oneshot`.
  - *Given* `run_daemon` has spawned the bridge consumer task and holds `bridge_tx: mpsc::Sender<BridgeMessage>`, *When* a caller does `let (reply_tx, reply_rx) = oneshot::channel(); bridge_tx.send((Request{tool:"ping".into(),params:None}, reply_tx)).await.unwrap(); let resp = reply_rx.await.unwrap();`, *Then* `resp == Response::ok(json!({"pong": true}))`.
- On shutdown, queued-but-unprocessed bridge messages get a clean error reply, not a dropped `oneshot::Sender`.
  - *Given* the bridge consumer loop's `tokio::select!` observes `daemon.cancellation_token()` cancelled, *When* it breaks out of its receive loop and drains any remaining already-received messages, *Then* each gets `reply_tx.send(Response::err("daemon shutting down"))` rather than being silently dropped (which would otherwise surface to the HTTP caller as an opaque `RecvError`, per `build-vs-buy.md` §6's shutdown-draining warning).
- The bridge consumer unblocks on cancellation without waiting for a new message.
  - *Given* the bridge consumer task is blocked on an empty `bridge_rx.recv()`, *When* `daemon.request_shutdown()` is called from another task, *Then* the consumer's loop exits promptly (bounded by a short test timeout) rather than hanging until a message arrives.
- `run_daemon`'s single `tokio::join!` — not two independent shutdown sequences — is what triggers cleanup.
  - *Given* `run_daemon` awaits `daemon.run_cancellable(...)`, the bridge consumer's `JoinHandle` (bounded by a grace timeout), and, if HTTP is enabled, the HTTP server's `JoinHandle` (bounded by a grace timeout) via a single `tokio::join!`, *When* any shutdown trigger fires (`SHUTDOWN_TOOL` over any transport, or SIGTERM), *Then* all three resolve, the join completes exactly once, and `shutdown_cleanup(...)` (extracted in Task 2.2.1d) runs exactly once afterward — there is no second, independent cleanup invocation anywhere in `main.rs`.
- A tool-handler panic inside the bridge consumer's per-request dispatch doesn't kill the consumer task or, with it, the whole HTTP transport (`pre-mortem.md` P1 #2 — sharpens the CONCERN already on record in `adversarial-review.md`).
  - *Given* the bridge consumer loop (Task 2.2.1c, guarded per Task 2.2.1e), *When* `daemon.handle_request(req)` panics while processing one bridge message, *Then* the panic is caught, the corresponding `oneshot::Sender` receives `Response::err("internal error: tool handler panicked")` rather than being dropped, a distinct `"bridge consumer panicked"` line is logged, and the consumer's `loop` continues to the next `bridge_rx.recv()` rather than the task exiting — so the paired `mpsc::Receiver` stays alive and every subsequent `ChannelTransport::call` keeps working, verified end-to-end by Task 8.1.3a.
**Files**: `crates/cli/src/main.rs`, `crates/cli/src/transport.rs` (for the `BridgeMessage` type alias)

##### Task 2.2.1a: Define `BridgeMessage` type alias (~2 min)
- `pub type BridgeMessage = (stapler_mcp_core::protocol::Request, tokio::sync::oneshot::Sender<stapler_mcp_core::protocol::Response>);` and `pub const BRIDGE_CHANNEL_CAPACITY: usize = 64;`.
- Files: `crates/cli/src/transport.rs`

##### Task 2.2.1b: Wrap `daemon` in `Rc`, construct the channel (~3 min)
- `main.rs:100`: `let daemon = Daemon::new();` → `let daemon = Rc::new(Daemon::new());` (all subsequent `daemon.register(...)` calls at lines 102-457 keep working via `Deref`); add `let (bridge_tx, bridge_rx) = tokio::sync::mpsc::channel::<BridgeMessage>(BRIDGE_CHANNEL_CAPACITY);` after registration, before the join point (Task 2.2.1d).
- Files: `crates/cli/src/main.rs`

##### Task 2.2.1c: Spawn the bridge consumer task via `spawn_local`, cancellation-aware, capturing its `JoinHandle` (~6 min)
- Loop: `loop { tokio::select! { _ = daemon.cancellation_token().cancelled() => break, msg = bridge_rx.recv() => { match msg { Some((req, reply_tx)) => { let resp = daemon.handle_request(req).await; let _ = reply_tx.send(resp); } None => break, } } } }`, followed by a drain loop replying `Response::err("daemon shutting down")` to anything left in `bridge_rx` via `try_recv()`. Store the `tokio::task::JoinHandle` returned by `spawn_local` in a variable `bridge_consumer_handle` for Task 2.2.1d's join.
- Files: `crates/cli/src/main.rs`

##### Task 2.2.1d: Join the accept loop, bridge consumer, and (if enabled) HTTP server exactly once; extract and call `shutdown_cleanup` exactly once (~6 min)
- Replace the previous plain `daemon.run(&socket, &sock_path)` call with `daemon.run_cancellable(&socket, &sock_path)` (Task 2.1.1d) and await it, `bridge_consumer_handle` (Task 2.2.1c), and — if `STAPLER_MCP_HTTP_PORT` was set (Task 3.2.1a) — the HTTP server's `JoinHandle` (Task 3.2.1a/5.1.1b), together via a single `tokio::join!`. Wrap each `JoinHandle` await in `tokio::time::timeout(SHUTDOWN_GRACE_TIMEOUT, handle)` (suggested `SHUTDOWN_GRACE_TIMEOUT = Duration::from_secs(5)`) so a pathologically stuck task (e.g. a hung CDP call) can't block shutdown forever; log distinctly and proceed to cleanup anyway if a timeout fires, rather than hanging. Extract the existing post-`daemon.run()` cleanup body (today inline at `main.rs:462-485`: drop `daemon`, `Rc::get_mut(&mut browser)`, abort reaper, `browser.close()`) into `async fn shutdown_cleanup(daemon: Rc<Daemon>, browser: Rc<NativeBrowser>)` (or equivalent — exact param shape at implementation time), and call it from exactly one place: immediately after the `tokio::join!` above completes, regardless of which trigger caused the shutdown. `run_daemon` returns normally afterward (no `std::process::exit` anywhere in this path, so `main` returns and destructors run) — see Task 5.1.1a, which removes the SIGTERM handler's own independent copy of this sequence in favor of triggering this one.
- Files: `crates/cli/src/main.rs`

##### Task 2.2.1e: Wrap the bridge consumer's per-request dispatch in `catch_unwind`, add a distinct "bridge consumer panicked" log line (~5 min)
- In Task 2.2.1c's loop body, replace `let resp = daemon.handle_request(req).await;` with a `catch_unwind`-guarded call, e.g. `let resp = match std::panic::AssertUnwindSafe(daemon.handle_request(req)).catch_unwind().await { Ok(resp) => resp, Err(payload) => { eprintln!("stapler-mcp: bridge consumer panicked handling tool call {:?}: {}", req.tool, panic_message(&payload)); Response::err("internal error: tool handler panicked".to_string()) } };`, using `futures::FutureExt::catch_unwind` (already a direct dependency of `crates/cli`, `Cargo.toml:20` — no new dependency needed), wrapped in `std::panic::AssertUnwindSafe` (required because `Daemon`'s `Rc<RefCell<...>>` fields aren't `UnwindSafe` by default — this is a best-effort recovery for a panicking handler body, per `pre-mortem.md` P1 #2's own framing of "convert a handler panic into an `Err` reply," not a guarantee against every possible mid-mutation inconsistency a panic could leave behind). Log the distinct `"stapler-mcp: bridge consumer panicked ..."` line — never reusing `daemon.log`'s existing generic tool-call-error format — so this failure mode is grep-distinguishable from both a normal tool error and the generic "channel closed" error a *dead* consumer would otherwise produce (`pre-mortem.md` P1 #2's first-symptom column). Catching the panic (keeping the `spawn_local` task, and with it the `mpsc::Receiver`, alive) is the actual fix; the log line is diagnostics on top of it. Add this same bullet to `Observability Plan`'s Logs list (Phase 6 has no line item for this today, per `adversarial-review.md`'s matching CONCERN).
- Files: `crates/cli/src/main.rs`

---

### Epic 2.3: `ChannelTransport`

#### Story 2.3.1: Implement `DaemonTransport` over the bridge channel
**As a** implementer, **I want** an HTTP-session-facing `DaemonTransport` impl, **so that** `McpRouter<ChannelTransport>` (Phase 3) can be constructed per HTTP session at near-zero cost.
**Acceptance Criteria**:
- A `ChannelTransport` clone can independently make a call and receive its own reply, unblocked by a concurrent call from another clone.
  - *Given* two `ChannelTransport` instances cloned from the same `mpsc::Sender<BridgeMessage>`, *When* both call `.call("ping", json!({}))` concurrently via `tokio::join!`, *Then* both resolve to `Ok(json!({"pong": true}))` without either blocking indefinitely (validates the bridge doesn't require exclusive per-call access to the sender).
**Files**: `crates/cli/src/transport.rs`

##### Task 2.3.1a: Implement `ChannelTransport` struct + `DaemonTransport` impl (~4 min)
- `pub struct ChannelTransport { tx: mpsc::Sender<BridgeMessage> }` (derives `Clone`); `call` builds a `Request`, a fresh `oneshot::channel()`, sends, awaits the reply, maps a dropped-sender/closed-channel error to a `String` error consistent with `SocketTransport`'s error shape.
- Files: `crates/cli/src/transport.rs`

##### Task 2.3.1b: Unit test with an in-process `Daemon` + bridge consumer (~5 min)
- Spin up a minimal `Daemon` with one registered handler, a real bridge channel, and a `spawn_local`-driven consumer inside a `#[tokio::test]` using `tokio::task::LocalSet`; exercise `ChannelTransport::call` end-to-end without spawning the real `stapler-mcp` binary.
- Files: `crates/cli/src/transport.rs` (`#[cfg(test)] mod tests`)

---

## Phase 3: HTTP listener

### Epic 3.1: axum `Router` + `StreamableHttpService` wiring

#### Story 3.1.1: Stand up the `/mcp` route
**As a** implementer, **I want** a real `tower_service::Service` speaking Streamable HTTP MCP, **so that** an MCP client configured with `{"type": "http", "url": "http://127.0.0.1:<port>/mcp"}` can reach the daemon (`requirements.md`'s core Success Metric).
**Acceptance Criteria**:
- A raw HTTP POST to `/mcp` with a well-formed `initialize` request gets a valid MCP `initialize` response.
  - *Given* `run_http_server(bridge_tx, 47439)` is running, *When* an HTTP client POSTs a spec-shaped `initialize` JSON-RPC request to `http://127.0.0.1:47439/mcp` with the correct bearer header, *Then* the response is a `200` with a valid `InitializeResult` body naming `"stapler-mcp"` as `serverInfo.name` (matching `ServerInfo::new(...).with_server_info(Implementation::new("stapler-mcp", ...))` already set in `mcp_router.rs`'s `ServerHandler::get_info`).
**Files**: `crates/cli/src/http_server.rs` (new)

##### Task 3.1.1a: `service_factory` closure building `McpRouter<ChannelTransport>` per session (~4 min)
- `move || Ok(McpRouter::with_transport(ChannelTransport::from(bridge_tx.clone())))` — requires `McpRouter::with_transport(transport: T) -> Self` constructor added alongside `McpRouter::new()` (Task 1.2.1d's default-`SocketTransport` constructor stays for the stdio path).
- Files: `crates/cli/src/http_server.rs`, `crates/cli/src/mcp_router.rs`

##### Task 3.1.1b: Build `StreamableHttpService` with `stateful_mode: false, json_response: true` (~3 min)
- `StreamableHttpService::new(factory, Arc::new(LocalSessionManager::default()), StreamableHttpServerConfig { stateful_mode: false, json_response: true, ..Default::default() })` (Pattern Decisions row 5).
- Files: `crates/cli/src/http_server.rs`

##### Task 3.1.1c: Mount via `axum::Router::nest_service("/mcp", service)` (~2 min)
- Files: `crates/cli/src/http_server.rs`

##### Task 3.1.1d: Bind `TcpListener::bind(("127.0.0.1", port))`, distinguish `AddrInUse` (~4 min)
- On `AddrInUse`, log `"stapler-mcp: HTTP port {port} already in use — continuing with stdio/socket transport only"` and return an error the caller (`run_daemon`) treats as non-fatal (Pattern Decisions row 8); any other bind error is also logged but likewise non-fatal to the daemon as a whole.
- Files: `crates/cli/src/http_server.rs`

---

### Epic 3.2: Opt-in wiring in `run_daemon`

#### Story 3.2.1: Start the HTTP listener only when `STAPLER_MCP_HTTP_PORT` is set
**As a** operator, **I want** the HTTP surface off by default, **so that** machines that haven't opted in see zero behavior change (Risk Control).
**Acceptance Criteria**:
- With `STAPLER_MCP_HTTP_PORT` unset, `stapler-mcp --daemon` behaves identically to today (Unix socket only, no port bound).
  - *Given* `STAPLER_MCP_HTTP_PORT` is not present in the daemon process's environment, *When* `stapler-mcp --daemon` starts, *Then* `ss -ltnp` (or equivalent) shows no new listening TCP port, and `daemon_architecture_and_tools_round_trip` (`crates/cli/tests/daemon_ping.rs`) still passes unmodified.
- With `STAPLER_MCP_HTTP_PORT=47439` set, the HTTP listener starts alongside the existing Unix socket.
  - *Given* `STAPLER_MCP_HTTP_PORT=47439` is set before `stapler-mcp --daemon` starts, *When* the daemon finishes startup, *Then* a `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:47439/mcp` (no auth header) returns `401`, confirming the listener is up and auth-gated (Phase 4 dependency noted, but reachability itself is this story's scope).
- The daemon persists the resolved port to `~/.stapler-mcp/http-port` on every startup, so a *separate* process (e.g. `--status`, `--print-config`, run later in a shell that never had `STAPLER_MCP_HTTP_PORT` set — the systemd/launchd-unit case) can discover it without reading this process's own environment.
  - *Given* `STAPLER_MCP_HTTP_PORT=47439` is set before `stapler-mcp --daemon` starts, *When* the daemon finishes startup, *Then* `~/.stapler-mcp/http-port` (or `$STAPLER_MCP_HOME/http-port` under test) contains exactly `47439`, readable by a process with no `STAPLER_MCP_HTTP_PORT` in its own environment.
  - *Given* `STAPLER_MCP_HTTP_PORT` is *not* set (or a stale `http-port` file exists from a previous HTTP-enabled run), *When* `stapler-mcp --daemon` starts, *Then* any existing `http-port` file is removed, so a later `--status`/`--print-config` invocation doesn't report a stale port for a daemon that isn't actually serving HTTP this run.
**Files**: `crates/cli/src/main.rs`, `crates/core/src/paths.rs`

##### Task 3.2.1a: Read `STAPLER_MCP_HTTP_PORT`, parse as `u16`, gate the spawn, capture the `JoinHandle` (~4 min)
- `let http_handle: Option<tokio::task::JoinHandle<_>> = std::env::var("STAPLER_MCP_HTTP_PORT").ok().and_then(|s| s.parse::<u16>().ok()).map(|port| tokio::task::spawn(http_server::run_http_server(bridge_tx.clone(), port, token, daemon.cancellation_token())));` placed after Epic 2.2's bridge setup. `http_handle` (an `Option`, `None` when HTTP is disabled) feeds Task 2.2.1d's `tokio::join!` — when `None`, that arm of the join is skipped/treated as already-complete rather than awaited.
- Files: `crates/cli/src/main.rs`

##### Task 3.2.1b: Log a startup line stating whether HTTP is enabled and on which port (~2 min)
- `eprintln!("stapler-mcp: HTTP transport disabled (STAPLER_MCP_HTTP_PORT not set)")` / `eprintln!("stapler-mcp: HTTP transport listening on 127.0.0.1:{port}/mcp")`.
- Files: `crates/cli/src/main.rs`

##### Task 3.2.1c: Add `http_port_path` to `crates/core/src/paths.rs`; persist/remove the http-port file on every daemon startup (~4 min)
- `pub fn http_port_path<E: EnvPort>(env: &E) -> String { format!("{}/http-port", base_dir(env)) }`, following the exact pattern of `http_token_path` (Task 4.1.1a). In `run_daemon`, immediately after Task 3.2.1a resolves `STAPLER_MCP_HTTP_PORT`: if a port was parsed, write it (plain-text, no trailing newline, overwrite any existing content) to `http_port_path(&env)` — unconditionally on the parsed value being valid, independent of whether the subsequent bind (Task 3.1.1d) later succeeds or fails, since this file records *configuration intent* ("was HTTP requested and with which port"), not liveness; `--status`'s separate TCP-connect probe (Task 7.1.1c) is what confirms liveness. If no port was parsed (env var unset or unparseable), remove `http_port_path(&env)` if it exists (`std::fs::remove_file`, ignoring `NotFound`), so a stale port from a previous HTTP-enabled run never leaks into a later `--status`/`--print-config` reading of a daemon that started without HTTP this time.
- Files: `crates/core/src/paths.rs`, `crates/cli/src/main.rs`

---

## Phase 4: Auth

### Epic 4.1: Token generation & storage

#### Story 4.1.1: `BearerToken` generation, persistence, constant-time comparison
**As a** operator, **I want** a durable, file-backed bearer token generated automatically, **so that** the HTTP surface is never reachable unauthenticated, without me hand-managing a secret (Pattern Decisions row 10; `requirements.md`'s hard ship-blocker).
**Acceptance Criteria**:
- First daemon start with HTTP enabled generates a token; a second start reuses it.
  - *Given* `~/.stapler-mcp/http-token` (or `$STAPLER_MCP_HOME/http-token` under test) does not exist, *When* `stapler-mcp --daemon` starts with `STAPLER_MCP_HTTP_PORT` set, *Then* the file is created with mode `0600` containing a 64-character hex string, and a second daemon start (after the first is stopped) reads back the identical token rather than generating a new one.
- The token is never logged.
  - *Given* the daemon's startup log line for HTTP enablement (Task 3.2.1b), *When* grepped for the generated token's literal value, *Then* it does not appear anywhere in `daemon.log`.
**Files**: `crates/core/src/paths.rs`, `crates/core/src/schema.rs` or a new `crates/core/src/http_auth.rs` (for `BearerToken`), `crates/native/src/http_token.rs` (new)

##### Task 4.1.1a: Add `http_token_path` to `crates/core/src/paths.rs` (~2 min)
- `pub fn http_token_path<E: EnvPort>(env: &E) -> String { format!("{}/http-token", base_dir(env)) }`, following the exact pattern of `socket_path`/`lock_path`/`log_path` at `paths.rs:19-29`.
- Files: `crates/core/src/paths.rs`

##### Task 4.1.1b: Define `BearerToken` newtype with constant-time `PartialEq` (~3 min)
- `pub struct BearerToken(String)`; manual `impl PartialEq` using `subtle::ConstantTimeEq`; deliberately no `Debug`/`Display` impl that exposes the inner value (or a redacted `Debug` printing `"BearerToken(***)"`).
- Files: `crates/core/src/http_auth.rs` (new)

##### Task 4.1.1c: Implement `generate_or_load` in `crates/native/src/http_token.rs` (~5 min)
- Mirrors `crates/native/src/lock.rs:33-39`'s `.mode(0o600)` open pattern: if the file exists and is non-empty, read and return its contents as `BearerToken`; otherwise generate 32 random bytes via `rand::rngs::OsRng`/`RngCore::fill_bytes`, hex-encode, write, return.
- Files: `crates/native/src/http_token.rs` (new)

##### Task 4.1.1d: Unit tests (generate-once, reuse, mode) (~4 min)
- `generate_or_load_should_create_a_new_token_when_none_exists`, `generate_or_load_should_reuse_an_existing_token`, mirroring `paths.rs`'s tempdir-based test style.
- Files: `crates/native/src/http_token.rs`

---

### Epic 4.2: Auth middleware

#### Story 4.2.1: `require_bearer_token` middleware, wired in front of `/mcp`
**As a** operator, **I want** every unauthenticated HTTP request rejected before it reaches any tool logic, **so that** the trust boundary a Unix socket gave for free is restored on TCP (`requirements.md`'s Security Classification).
**Acceptance Criteria**:
- A request with no `Authorization` header, or the wrong token, gets `401` and a distinct log line; the correct token succeeds.
  - *Given* the daemon's real generated token is `"abc123..."`, *When* a client POSTs to `/mcp` with `Authorization: Bearer wrong-token`, *Then* the response is `401` and `daemon.log` contains a line matching `"rejected unauthenticated HTTP request ... invalid bearer token"` (not the tool-call error log format).
  - *Given* the same setup, *When* a client POSTs with `Authorization: Bearer abc123...` (the real token) and a well-formed MCP request body, *Then* the request reaches `McpRouter`'s tool dispatch (no `401`).
**Files**: `crates/cli/src/http_server.rs`

##### Task 4.2.1a: Implement `require_bearer_token` middleware (~5 min)
- `async fn require_bearer_token(State(token): State<BearerToken>, headers: HeaderMap, req: Request, next: Next) -> Response` — extract `Authorization` header, constant-time-compare against `token`, `401` with a small JSON body on mismatch/absence (logging the failure class, never the presented value), else `next.run(req).await`.
- Files: `crates/cli/src/http_server.rs`

##### Task 4.2.1b: Wire via `Router::layer(axum::middleware::from_fn_with_state(token.clone(), require_bearer_token))` (~2 min)
- Applied to the whole router (or at minimum wrapping `/mcp`), per `stack.md` §5's finding that the auth layer must sit at the `axum::Router` level, not around the bare `StreamableHttpService`.
- Files: `crates/cli/src/http_server.rs`

##### Task 4.2.1c: Integration test — reject/accept, log line present (~5 min)
- Covered by `crates/cli/tests/http_transport.rs` (Phase 8, Task 8.1.2) — this task just confirms the middleware compiles and unit-tests cleanly in isolation first via a small `tower::ServiceExt::oneshot` call against the router directly.
- Files: `crates/cli/src/http_server.rs` (`#[cfg(test)] mod tests`)

---

## Phase 5: Lifecycle

### Epic 5.1: SIGTERM + shutdown coordination

#### Story 5.1.1: Graceful shutdown on SIGTERM, driving the single shutdown path exactly once
**As a** operator running the daemon under systemd/launchd, **I want** `SIGTERM` to trigger the *same* clean-shutdown sequence the `SHUTDOWN_TOOL` RPC already triggers — not a second, independent copy of it — with real sole ownership of `Rc<NativeBrowser>`/`Rc<Daemon>` at cleanup time, **so that** `Restart=on-failure` doesn't uncleanly kill the Chrome subprocess every restart, and an in-flight HTTP request/browser operation isn't hard-killed or raced against a concurrent `browser.close()` (`pitfalls.md` §3; architecture-review's Blocker 1; adversarial-review's Blocker 2).
**Acceptance Criteria**:
- Sending `SIGTERM` runs `shutdown_cleanup` (Task 2.2.1d) exactly once, through the same join every other shutdown trigger uses, and the process exits by returning from `main`.
  - *Given* a running `stapler-mcp --daemon` with an open browser session, *When* it receives `SIGTERM` (e.g. `kill -TERM <pid>`), *Then* `daemon.request_shutdown()` cancels `Daemon`'s `CancellationToken`, which unblocks `run_cancellable`'s accept loop (Task 2.1.1d), the bridge consumer (Task 2.2.1c), and the HTTP server's graceful shutdown (Task 5.1.1b) — `run_daemon`'s single `tokio::join!` (Task 2.2.1d) then completes, `shutdown_cleanup` runs exactly once (the same "reaper aborted"/`browser.close()` log lines a `SHUTDOWN_TOOL`-triggered exit already produces), and `main` returns normally with exit code `0` — no `std::process::exit()` call anywhere in the SIGTERM path.
  - *Given* `Rc::get_mut(&mut browser)` inside `shutdown_cleanup` requires sole ownership, *When* `shutdown_cleanup` runs, *Then* it succeeds (returns `Some`), because every task that held an `Rc<NativeBrowser>`/`Rc<Daemon>` clone (the accept loop, the bridge consumer, the HTTP server's per-session `McpRouter<ChannelTransport>` instances, which only hold a channel sender, not the `Rc`s directly) has already exited via the join before `shutdown_cleanup` is called — not a race, by construction.
- An in-flight HTTP request at the moment of SIGTERM completes or fails cleanly, rather than being hard-killed.
  - Covered by Task 8.3.1a (Phase 8) — SIGTERM sent mid-request, in-flight request observed to either complete or fail with a clean error, never by the process simply vanishing.
**Files**: `crates/cli/src/main.rs`, `crates/cli/src/http_server.rs`

##### Task 5.1.1a: Spawn a `tokio::signal::unix::signal(SignalKind::terminate())` listener task — minimal, no direct cleanup (~3 min)
- On receipt: log `"stapler-mcp: received SIGTERM, shutting down"`, call `daemon.request_shutdown()` (Task 2.1.1b) — that single call sets the shutdown flag and cancels `Daemon`'s `CancellationToken`, which is all this task does. It does **not** run any cleanup sequence itself, does **not** call `browser.close()`, and does **not** call `std::process::exit()`. All actual cleanup happens exactly once, later, when `run_daemon`'s own `tokio::join!` (Task 2.2.1d) naturally completes because every loop it's joining unblocked on the now-cancelled token — this task's only job is to be the thing that cancels it. This replaces the previous design (independent copy of `main.rs:462-485`'s cleanup plus `std::process::exit(0)`) that architecture-review's Blocker 1 and adversarial-review's Blocker 2 identified as racing a concurrent `browser.close()` against still-running request handlers and skipping Rust destructors.
- **Named invariant — why this task's own `Rc<Daemon>` clone is safe to omit from Task 2.2.1d's `tokio::join!`**: this task must hold its own `Rc<Daemon>` clone (it's `!Send`, so it runs via `spawn_local`, not `tokio::spawn`) to call `request_shutdown()` after `.await`ing the signal, and that clone is dropped when the task's future completes. It is deliberately *not* joined explicitly, because it has no further `.await` after calling `request_shutdown()` — it completes in the very same poll that fires cancellation, on this process's single-threaded cooperative runtime, strictly before the accept loop, bridge consumer, or HTTP server (each of which needs at least one more wakeup to notice cancellation and unwind) resolve their own joined handles. This ordering is a property of single-threaded cooperative scheduling, not a race — stated explicitly here rather than left as an implicit assumption (architecture-review's corresponding finding).
- Files: `crates/cli/src/main.rs`

##### Task 5.1.1b: Use `Daemon`'s own `CancellationToken` for `axum::serve(...).with_graceful_shutdown(...)`, capture the serve `JoinHandle` (~4 min)
- No separately-constructed `CancellationToken` — `run_http_server` (Task 3.1.1, Task 3.2.1a) takes `daemon.cancellation_token()` as a parameter and builds its graceful-shutdown future from `token.cancelled_owned().await` (or equivalent), passed to `.with_graceful_shutdown(...)`. `run_http_server` itself is spawned via `tokio::task::spawn` (Task 3.2.1a), and the `tokio::task::JoinHandle` that call returns is what Task 2.2.1d's `tokio::join!` awaits — so "cancel the token" and "the HTTP server's graceful shutdown has actually finished flushing in-flight responses" are two distinct, both-awaited events, not one assumed to imply the other.
- Files: `crates/cli/src/main.rs`, `crates/cli/src/http_server.rs`

##### Task 5.1.1c: Bound the post-cancellation join with a grace timeout; log distinctly on a forced exit (~3 min)
- Wrap each of the three awaited handles in Task 2.2.1d's `tokio::join!` (`run_cancellable`, bridge consumer, HTTP server) in `tokio::time::timeout(SHUTDOWN_GRACE_TIMEOUT, ...)` (Task 2.2.1d); if any times out, log `"stapler-mcp: shutdown grace period ({SHUTDOWN_GRACE_TIMEOUT:?}) exceeded waiting on <which task>, proceeding to cleanup anyway"` distinctly from the normal SIGTERM-received log line, so a pathologically stuck request (e.g. a hung CDP call) can't block shutdown forever, and an operator can `grep` to tell "clean shutdown" apart from "forced after timeout." This replaces the previous doc-only task that accepted "no `tokio::select!` around `accept()`" as a tradeoff — that tradeoff no longer exists (`Daemon::run_cancellable`, Task 2.1.1d, does select on cancellation now); this task's job is the *residual* risk (a handler that's actually hung, not just slow to notice cancellation).
- **Accepted, degraded behavior on timeout (explicit, not an oversight)**: on a grace-timeout, the underlying timed-out task keeps running in the background — it is not `.abort()`-ed — so `shutdown_cleanup`'s `Rc::get_mut(&mut browser)` sole-ownership requirement can fail in exactly this branch, and the reaper-abort/`browser.close()` sequence is silently skipped rather than run. This is a known, accepted residual risk scoped to "a handler that's actually hung" (not the common-case shutdown path, which this design otherwise makes race-free by construction) — logged distinctly so it's diagnosable, not fixed by this pass.
- Files: `crates/cli/src/main.rs`

---

### Epic 5.2: systemd/launchd templates + bind-failure handling

#### Story 5.2.1: Ship `.example` unit templates matching the existing `scripts/docs-mcp-server.service.example` convention
**As a** operator, **I want** a copy-paste-ready persistent-service template, **so that** the HTTP path is actually usable unattended (`requirements.md`'s Scope item).
**Acceptance Criteria**:
- `scripts/stapler-mcp.service.example` follows the exact structure of the existing `scripts/docs-mcp-server.service.example` and includes a bounded restart policy.
  - *Given* `scripts/docs-mcp-server.service.example`'s existing shape (`[Unit]`/`[Service]`/`[Install]`, `Restart=on-failure`, `RestartSec=5`), *When* `scripts/stapler-mcp.service.example` is written with `ExecStart=%h/.cargo/bin/stapler-mcp --daemon`, `Environment=STAPLER_MCP_HTTP_PORT=47439`, `Restart=on-failure`, `RestartSec=5`, `StartLimitBurst=5`, `StartLimitIntervalSec=60`, `WantedBy=default.target`, *Then* `systemd-analyze verify` (run manually, not in CI — no systemd in the CI container) reports no syntax errors.
- `scripts/com.tstapler.stapler-mcp.plist.example` provides the launchd equivalent, including `KeepAlive`/throttling.
  - *Given* macOS `launchd` agent conventions (`Label`, `ProgramArguments`, `KeepAlive`, `ThrottleInterval`, `EnvironmentVariables`), *When* the plist is written with `Label: com.tstapler.stapler-mcp`, `KeepAlive: {SuccessfulExit: false}`, `ThrottleInterval: 10`, `EnvironmentVariables: {STAPLER_MCP_HTTP_PORT: "47439"}`, *Then* `plutil -lint` (manual) reports valid XML/plist syntax.
**Files**: `scripts/stapler-mcp.service.example` (new), `scripts/com.tstapler.stapler-mcp.plist.example` (new)

##### Task 5.2.1a: Write `scripts/stapler-mcp.service.example` (~4 min)
- Files: `scripts/stapler-mcp.service.example`

##### Task 5.2.1b: Write `scripts/com.tstapler.stapler-mcp.plist.example` (~4 min)
- Files: `scripts/com.tstapler.stapler-mcp.plist.example`

##### Task 5.2.1c: Cross-reference both from `README.md`'s new "Running as a persistent service" section (~2 min)
- Files: `README.md` (also touched by Phase 7, Task 7.2.2 — same section, sequenced here to avoid a merge conflict with Phase 7's broader rewrite)

---

## Phase 6: Observability

### Epic 6.1: Logging, redaction, file permissions

#### Story 6.1.1: Close the `daemon.log` mode gap; add the new required log lines
**As a** operator, **I want** `daemon.log` to never leak the bearer token and to carry the specific log lines `requirements.md`'s Observability Requirements call for, **so that** a rejected-auth attempt or a stuck request under load is diagnosable from the log alone.
**Acceptance Criteria**:
- `daemon.log` is created with mode `0600`, matching `daemon.lock`.
  - *Given* `crates/native/src/spawn.rs:21-24` opens `log_path` via `OpenOptions::new().create(true).append(true)` with no `.mode(...)` call today, *When* `.mode(0o600)` (via `std::os::unix::fs::OpenOptionsExt`) is added, *Then* a freshly created `daemon.log` has permissions `-rw-------` (verified via `ls -l` or `std::fs::metadata(...).permissions()` in a test).
- Bridge-channel backpressure is logged when a send blocks >250ms.
  - *Given* the bridge channel (`BRIDGE_CHANNEL_CAPACITY = 64`) is saturated by a burst of concurrent HTTP requests, *When* `ChannelTransport::call`'s `bridge_tx.send(...).await` takes longer than 250ms (measured via `tokio::time::timeout` wrapping the send, retrying without the timeout after logging), *Then* a line `"stapler-mcp: HTTP request queued >250ms waiting on daemon core"` appears in `daemon.log`, and the request still eventually completes (not dropped).
**Files**: `crates/native/src/spawn.rs`, `crates/cli/src/transport.rs`, `crates/cli/src/http_server.rs`

##### Task 6.1.1a: Add `.mode(0o600)` to `daemon.log`'s `OpenOptions` in `spawn.rs` (~2 min)
- Files: `crates/native/src/spawn.rs`

##### Task 6.1.1b: Wrap `ChannelTransport::call`'s send in a 250ms `tokio::time::timeout`-then-log-then-continue pattern (~4 min)
- Files: `crates/cli/src/transport.rs`

##### Task 6.1.1c: Add HTTP connection start/end logging with a correlation id (~4 min)
- A small `axum::middleware::from_fn` (composed alongside `require_bearer_token`) assigning a `uuid`-derived (already a transitive dep per `research/stack.md` §1) or simple atomic-counter correlation id, logging `"http request start {id} from {peer}"` / `"http request end {id} status={status} elapsed_ms={ms}"`.
- Files: `crates/cli/src/http_server.rs`

##### Task 6.1.1d: Code-comment audit — confirm no `{:?}`-style request/header dump exists anywhere in the new HTTP code (~2 min, review-only)
- Files: `crates/cli/src/http_server.rs`, `crates/cli/src/transport.rs`

---

## Phase 7: UX

### Epic 7.1: `--status` and `--print-config` subcommands

#### Story 7.1.1: `stapler-mcp --status`
**As a** operator, **I want** a single command that tells me whether the daemon and (if configured) the HTTP listener are reachable, **so that** I'm not left inferring daemon health from a cryptic Claude Code transport error (`ux.md` §1-2).
**Acceptance Criteria**:
- With no daemon running, `--status` reports both as down with next-step guidance.
  - *Given* no `stapler-mcp --daemon` process is running and `STAPLER_MCP_HOME` points at an empty temp dir, *When* `stapler-mcp --status` runs, *Then* stdout includes `"daemon: not running"` and a line suggesting `systemctl --user start stapler-mcp` or `stapler-mcp --daemon`.
- With a daemon running and HTTP enabled (port persisted to `~/.stapler-mcp/http-port` by that daemon's own startup, per Task 3.2.1c — **not** `STAPLER_MCP_HTTP_PORT` read from `--status`'s own invoking shell, which the systemd/launchd-started case never has set), `--status` confirms both the socket and the port.
  - *Given* a real daemon is running with HTTP enabled, having persisted `47439` to `~/.stapler-mcp/http-port` on its own startup (Task 3.2.1c), and `--status` is invoked from a plain shell with no `STAPLER_MCP_HTTP_PORT` set (the systemd/launchd case — `ux.md` Surfaces 4, 8), *When* `stapler-mcp --status` runs, *Then* stdout includes `"daemon: running (pid <N>)"` and `"http: listening on 127.0.0.1:47439"` (a raw TCP connect probe against the port read from `~/.stapler-mcp/http-port`, no bearer token required to confirm reachability — Pattern Decisions' auth design deliberately keeps `--status` from needing to read the token file for this check).
  - *Given* no `~/.stapler-mcp/http-port` file exists (daemon last started without `STAPLER_MCP_HTTP_PORT`), *When* `stapler-mcp --status` runs against a running daemon, *Then* stdout reports HTTP as not configured (distinct wording from "configured but unreachable" — see Task 7.1.1c).
**Files**: `crates/cli/src/main.rs`, `crates/core/src/paths.rs`

##### Task 7.1.1a: Parse `--status` before the existing `--daemon` flag check (~2 min)
- Files: `crates/cli/src/main.rs`

##### Task 7.1.1b: Implement the Unix-socket ping-only check (no auto-spawn) (~3 min)
- Reuse `client::ping` (already separate from `ensure_daemon`) directly, with a short timeout.
- Files: `crates/cli/src/main.rs`

##### Task 7.1.1c: Implement the optional HTTP TCP-connect probe against the persisted port (~4 min)
- Read the port to probe from `http_port_path(&env)` (Task 3.2.1c) — **not** from `STAPLER_MCP_HTTP_PORT` in `--status`'s own environment, since the daemon may have been started by a systemd/launchd unit whose `Environment=` scoping never reaches this invocation's shell (`ux.md` Surfaces 4, 8). If `STAPLER_MCP_HTTP_PORT` happens to also be set in the invoking shell, prefer it only when the `http-port` file is absent (manual-foreground-testing convenience — Pattern Decisions' "port discovery" row). Three distinct outcomes, worded distinctly: (1) no `http-port` file and no env var → `"http: not configured (STAPLER_MCP_HTTP_PORT not set on last daemon start)"`; (2) `http-port` file present but the TCP connect fails → `"http: not listening on 127.0.0.1:<port> (configured but unreachable)"`; (3) `http-port` file present and the TCP connect succeeds → `"http: listening on 127.0.0.1:<port>"`.
- Files: `crates/cli/src/main.rs`

#### Story 7.1.2: `stapler-mcp --print-config`
**As a** operator setting up a new machine, **I want** the exact `mcp-servers.json` HTTP block printed for me, **so that** I never hand-assemble the URL/header myself (`ux.md`'s Jupyter-token-on-first-run precedent).
**Acceptance Criteria**:
- With a token already generated, `--print-config` prints a ready-to-paste JSON block containing the real token and the daemon's actual persisted port.
  - *Given* `~/.stapler-mcp/http-token` exists and `~/.stapler-mcp/http-port` contains `47439` (both persisted by the daemon on its own startup, Task 3.2.1c/4.1.1c — `--print-config` reads these files, not `STAPLER_MCP_HTTP_PORT` from its own invoking shell, which the systemd/launchd-started case never has set), *When* `stapler-mcp --print-config` runs, *Then* stdout is a JSON object shaped `{"type": "http", "url": "http://127.0.0.1:47439/mcp", "headers": {"Authorization": "Bearer <the real token>"}}`.
- With no token yet generated, `--print-config` says so instead of fabricating one.
  - *Given* `~/.stapler-mcp/http-token` does not exist, *When* `stapler-mcp --print-config` runs, *Then* stdout says the daemon hasn't started with HTTP enabled yet and suggests starting it first, rather than generating a token from a read-only command.
- With a token present but no persisted port (an inconsistent/stale state — e.g. the token file survived from an earlier HTTP-enabled run but the daemon's most recent startup had `STAPLER_MCP_HTTP_PORT` unset, so Task 3.2.1c removed `~/.stapler-mcp/http-port`), `--print-config` reports that instead of guessing a port.
  - *Given* `~/.stapler-mcp/http-token` exists but `~/.stapler-mcp/http-port` does not, *When* `stapler-mcp --print-config` runs, *Then* stdout says HTTP isn't currently enabled on the running daemon and suggests restarting it with `STAPLER_MCP_HTTP_PORT` set, rather than printing a block with a fabricated or previously-cached port.
**Files**: `crates/cli/src/main.rs`, `crates/core/src/paths.rs`

##### Task 7.1.2a: Implement `--print-config`, reading (not generating) the token file and the persisted `http-port` file (~5 min)
- Read the port from `http_port_path(&env)` (Task 3.2.1c), not from `STAPLER_MCP_HTTP_PORT` in `--print-config`'s own environment — same rationale as Task 7.1.1c. Both the token file and the port file must be present to emit the JSON block; either missing produces the corresponding plain-English "not enabled" message (see acceptance criteria) instead of partial/guessed output.
- Files: `crates/cli/src/main.rs`

---

### Epic 7.2: README rewrite

#### Story 7.2.1: Update the architecture section to describe both transports additively
**As a** reader of this repo, **I want** the README's architecture section to reflect reality once HTTP ships, **so that** it doesn't keep asserting the stdio-only model is "the entire point of the project, not an optional nicety" when that's no longer the whole story.
**Acceptance Criteria**:
- The `## Architecture: thin client + shared daemon` section (`README.md:19-56`) documents both the stdio thin client and the opt-in HTTP path, and the ASCII diagram shows both.
  - *Given* `README.md:24-29`'s current diagram shows only stdio clients feeding into `daemon.sock`, *When* the section is rewritten to add a second arrow from `"MCP client (http transport)"` into `127.0.0.1:<port>/mcp` alongside the existing stdio arrows, with prose explaining HTTP is opt-in via `STAPLER_MCP_HTTP_PORT` and the stdio path remains the default/fallback, *Then* the README accurately describes the shipped behavior with no stale claims.
**Files**: `README.md`

##### Task 7.2.1a: Rewrite `README.md:19-56`'s diagram and prose (~5 min)
- Files: `README.md`

##### Task 7.2.1b: Add "Running as a persistent service" section referencing the Phase 5 templates (~4 min)
- Files: `README.md`

##### Task 7.2.1c: Add a "Troubleshooting" subsection with the ordered diagnostic steps from `ux.md` §2 (~4 min)
- Order: (1) `systemctl --user status stapler-mcp` / `launchctl print gui/$(id -u)/com.tstapler.stapler-mcp`, (2) `stapler-mcp --status`, (3) `systemctl --user start stapler-mcp` (or `stapler-mcp --daemon` directly if no launcher installed).
- Files: `README.md`

##### Task 7.2.1d: Document the token-distribution warning (~3 min)
- State explicitly: never paste a literal token into the git-tracked `mcp-servers.json`; use `stapler-mcp --print-config` and paste into a machine-local, non-committed override; note Claude Code's reported `${ENV_VAR}`-in-`headers` bug as the reason env-var indirection isn't a safe substitute either.
- Files: `README.md`

##### Task 7.2.1e: Add a numbered "First time enabling HTTP" quick-start section, referencing `ux.md`'s ordered walkthrough by surface number rather than re-describing each surface (~3 min)
- None of Tasks 7.2.1a-d ties the individually-documented surfaces (architecture diagram, persistent-service setup, troubleshooting, token-distribution warning) into one ordered path for a first-time setup — each covers one surface, not the sequence across them. Mirror `design/ux.md`'s new "First-time HTTP setup" walkthrough section (1. start daemon with HTTP enabled → 2. token auto-generated, logged → 3. `--print-config` → 4. paste into config → 5. optionally install the persistent unit → 6. verify with `--status`), linking each step to its existing surface number instead of duplicating the content.
- Files: `README.md`

---

## Phase 8: Testing

### Epic 8.1: Integration tests

#### Story 8.1.1: HTTP transport parity and auth tests
**As a** implementer, **I want** the HTTP path tested with the same rigor as the existing stdio/socket path, **so that** `requirements.md`'s Success Metrics are actually verified, not just designed for.
**Acceptance Criteria**:
- All 27 tools are reachable and schema-identical over HTTP vs. stdio.
  - *Given* `McpRouter::<SocketTransport>::registered_tools()` and `McpRouter::<ChannelTransport>::registered_tools()` (both callable via the `#[cfg(test)]` accessor from Task 1.2.1's generic router), *When* compared tool-by-tool, *Then* every name, description, and `inputSchema` is identical between the two — mechanically enforcing Pattern Decisions row 1's single-source-of-truth goal.
- An unauthenticated request is rejected; an authenticated one succeeds against a real running daemon.
  - *Given* a real `stapler-mcp --daemon` process spawned with `STAPLER_MCP_HTTP_PORT` set inside a fully isolated `STAPLER_MCP_HOME` temp dir (mirroring `daemon_ping.rs`'s `TestEnv` pattern), *When* a `reqwest` client POSTs to `/mcp` first with no `Authorization` header, then with the real token read from the generated `http-token` file, *Then* the first gets `401` and the second gets a valid MCP response for e.g. `stapler_browser_list_sessions`.
**Files**: `crates/cli/tests/http_transport.rs` (new)

##### Task 8.1.1a: Schema-parity test (no live daemon needed) (~4 min)
- Files: `crates/cli/tests/http_transport.rs`

##### Task 8.1.1b: Spawn a real HTTP-enabled daemon, test unauth-rejected/auth-accepted (~5 min)
- Files: `crates/cli/tests/http_transport.rs`

##### Task 8.1.1c: Dual-transport test — one stdio `client::call` and one HTTP POST against the same running daemon instance (~5 min)
- Per `pitfalls.md` §3's explicit recommendation, guarding against a partial rollout where one transport silently regresses while the other looks fine.
- Files: `crates/cli/tests/http_transport.rs`

#### Story 8.1.2: Concurrency and cross-request `SessionId` reuse
**As a** implementer, **I want** the Success Metric "≥2 concurrent HTTP sessions against a stateful tool at once" actually exercised, **so that** the bridge's serialization behavior is proven, not assumed.
**Acceptance Criteria**:
- Two concurrent HTTP tool calls both succeed without corruption or deadlock.
  - *Given* a running HTTP-enabled daemon, *When* two `stapler_browser_navigate` calls (to different, cheap local URLs) are fired concurrently via `tokio::join!` from two separate `reqwest` clients, *Then* both return `200` with valid, distinct `sessionId`s.
- A `ports::SessionId` created by one HTTP request is reusable by a second, independent HTTP request.
  - *Given* the first request's response includes `sessionId: "abc"`, *When* a second, separate HTTP POST calls `stapler_browser_click` with `sessionId: "abc"`, *Then* it succeeds — replicating today's already-working cross-connection `SessionId` reuse (`research/features.md` §2) under the stateless HTTP design.
- A single hung tool call doesn't starve other concurrent HTTP callers (`pre-mortem.md` P1 #1).
  - *Given* an in-process `Daemon` + bridge consumer + HTTP server (Task 2.3.1b's harness pattern — a directly-constructed `Daemon` with a test-only registered handler that never resolves, not the real `stapler-mcp` binary's fixed tool set), *When* one HTTP request calls the hung tool while two other, unrelated HTTP requests call a fast handler (e.g. `"ping"`) concurrently via `tokio::join!`, *Then* the two unrelated requests both complete successfully within a short bound (well under `REQUEST_TIMEOUT`) rather than queuing behind the hung call, and the hung call's own response eventually resolves to the `Response::err(...)` timeout from Task 2.1.2a instead of hanging the test.
**Files**: `crates/cli/tests/http_transport.rs`

##### Task 8.1.2a: Concurrent-calls test (~5 min)
- Files: `crates/cli/tests/http_transport.rs`

##### Task 8.1.2b: Cross-request `SessionId` reuse test (~4 min)
- Files: `crates/cli/tests/http_transport.rs`

##### Task 8.1.2c: Hung-handler concurrency test — a stuck call doesn't block unrelated concurrent callers (~6 min)
- Build the in-process harness (`Daemon` + bridge channel + `spawn_local` consumer + `run_http_server`, following Task 2.3.1b's pattern of constructing these directly inside the test rather than spawning the real binary) and register one extra test-only handler returning `std::future::pending()`. Fire an HTTP request at that handler on a background task, then, without waiting for it, fire two more HTTP requests at a fast handler concurrently via `tokio::join!`; assert both fast requests return `200` within a short bound. Separately await the hung request's own task and assert it eventually resolves to the timeout error from Task 2.1.2a rather than the test itself hanging. This is the integration-level proof `pre-mortem.md`'s P1 #1 explicitly asks for, complementing Task 2.1.2b's core-level unit test.
- Files: `crates/cli/tests/http_transport.rs`

---

#### Story 8.1.3: Bridge-consumer panic recovery
**As a** implementer, **I want** proof that a tool-handler panic doesn't permanently kill the HTTP transport, **so that** `pre-mortem.md`'s P1 #2 (a single panicking handler silently and permanently breaking every HTTP-originated call until a manual daemon restart) is verified closed, not just designed against (matches the CONCERN already on record in `adversarial-review.md`).
**Acceptance Criteria**:
- A tool call that panics inside the bridge consumer produces a clean error reply and a distinct log line, and the transport keeps serving subsequent requests.
  - *Given* the same in-process harness as Task 8.1.2c, with a test-only registered handler that panics (`panic!("intentional test panic")`) when called, *When* an HTTP request calls that handler, *Then* the response is a clean JSON-RPC/HTTP error (never a hung connection), and the captured log output contains a line matching `"bridge consumer panicked"` (Task 2.2.1e) rather than a generic tool-call-error line.
  - *Given* the same daemon immediately after the panicking call above, *When* a second, unrelated HTTP request calls the fast handler, *Then* it returns `200` with a valid response — proving the bridge consumer task, and the `mpsc::Receiver` it owns, are both still alive rather than the whole HTTP transport being dead until a manual restart.
**Files**: `crates/cli/tests/http_transport.rs`

##### Task 8.1.3a: Panic-recovery integration test (~6 min)
- Reusing Task 8.1.2c's in-process harness and test-only-handler mechanism, register a handler that panics instead of hanging; fire an HTTP request at it and assert the response is a clean error and the log contains the distinct "bridge consumer panicked" line; then fire a second, normal HTTP request at a fast handler and assert it still succeeds.
- Files: `crates/cli/tests/http_transport.rs`

---

#### Story 8.1.4: Bridge channel backpressure at and beyond `BRIDGE_CHANNEL_CAPACITY`
**As a** implementer, **I want** the channel actually driven past its bound (64) under test, **so that** Story 6.1.1's backpressure-warning log and eventual-completion behavior are verified end-to-end, not just designed for (`adversarial-review.md`'s CONCERN: "no task in Phase 8 actually bursts >`BRIDGE_CHANNEL_CAPACITY` (64) concurrent requests" — Task 8.1.2a only exercises 2).
**Acceptance Criteria**:
- Sending more concurrent requests than the channel's capacity triggers the backpressure warning and every request still resolves.
  - *Given* the same in-process harness as Task 8.1.2c (`Daemon` + bridge channel with `BRIDGE_CHANNEL_CAPACITY = 64` + `spawn_local` consumer + `run_http_server`), *When* more than 64 concurrent HTTP requests are fired at once (e.g. 100, via `futures::future::join_all`) against a fast handler, *Then* `daemon.log` contains at least one `"HTTP request queued >250ms waiting on daemon core"` line (Task 6.1.1b), and every one of the 100 requests eventually resolves — either with a normal `200` response or, if it outlives `REQUEST_TIMEOUT` (Task 2.1.2a) while queued, a clean timeout error — never a hang past `REQUEST_TIMEOUT` plus a small margin.
**Files**: `crates/cli/tests/http_transport.rs`

##### Task 8.1.4a: Channel-saturation burst test (~6 min)
- Reusing Task 8.1.2c's harness, fire >64 concurrent requests via `futures::future::join_all`, assert the backpressure log line appears and every response arrives (success or clean timeout) within a bounded margin over `REQUEST_TIMEOUT`, not a bare hang.
- Files: `crates/cli/tests/http_transport.rs`

---

### Epic 8.2: CI hardening

#### Story 8.2.1: Add dependency-vulnerability scanning
**As a** maintainer, **I want** `cargo audit` (or `cargo deny`) in CI, **so that** a future `rmcp` CVE in this exact transport code path (two already fixed by 2.2.0) doesn't go unnoticed (`pitfalls.md` §1 action item).
**Acceptance Criteria**:
- CI fails if a known-vulnerable dependency version is resolved.
  - *Given* `.github/workflows/ci.yml`'s `native` job currently runs `cargo fmt --check`, `cargo clippy`, `cargo build`, `cargo test` (lines 17-23), *When* a `cargo install cargo-audit --locked` + `cargo audit` step is added after the existing steps, *Then* a pull request introducing a dependency with a published RUSTSEC advisory fails CI.
**Files**: `.github/workflows/ci.yml`

##### Task 8.2.1a: Add the `cargo-audit` install + run steps to the `native` job (~3 min)
- Files: `.github/workflows/ci.yml`

---

### Epic 8.3: Shutdown-lifecycle tests

#### Story 8.3.1: SIGTERM mid-request behaves cleanly, not just "the process exits"
**As a** implementer, **I want** direct evidence that the single-join shutdown redesign (Epic 2.1/2.2/5.1) actually protects an in-flight request, **so that** Story 5.1.1's acceptance criteria are verified, not just designed for — per adversarial-review's own recommendation that no existing task exercises this race.
**Acceptance Criteria**:
- An HTTP request in flight at the moment SIGTERM is sent either completes successfully or fails with a clean, legible error — never a hard-killed connection or a silently-corrupted browser session.
  - *Given* a real `stapler-mcp --daemon` process spawned with `STAPLER_MCP_HTTP_PORT` set inside an isolated `STAPLER_MCP_HOME` (mirroring `daemon_ping.rs`'s `TestEnv` pattern), *When* a slow-ish tool call (e.g. `stapler_browser_navigate` to a deliberately slow local URL, or a small artificial delay if the daemon exposes one under test) is fired and, part-way through, the daemon process receives `SIGTERM`, *Then* the in-flight HTTP request either returns a valid response or a connection-level error the test can assert on (not a bare timeout/hang), the daemon process exits within `SHUTDOWN_GRACE_TIMEOUT` plus a small margin, and `daemon.log` shows exactly one cleanup sequence's worth of log lines (reaper-abort, browser-close), not two — confirming there is only one shutdown path, not a race between the SIGTERM handler's old independent copy and `run_daemon`'s join.
**Files**: `crates/cli/tests/http_transport.rs` (or a new `crates/cli/tests/shutdown.rs`)

##### Task 8.3.1a: SIGTERM-mid-request integration test (~6 min)
- Spawn a real HTTP-enabled daemon, start a slow tool call, send `SIGTERM` to the daemon's pid while it's in flight, assert on the request's outcome and the daemon log's cleanup-line count (exactly one occurrence of the reaper-abort/browser-close sequence).
- Files: `crates/cli/tests/http_transport.rs` (or `crates/cli/tests/shutdown.rs`, new)

---

## Phase 9: Manual verification & ship prep

### Epic 9.1: Residual-risk manual checks

#### Story 9.1.1: Confirm Claude Code's real reconnect behavior
**As a** ship-readiness gate, **I want** the one risk research couldn't verify automatically checked by hand, **so that** this doesn't ship on an unverified assumption about a third-party client's behavior (`ux.md` §4c).
**Acceptance Criteria**:
- Claude Code recovers from a killed-and-restarted daemon without requiring a full Claude Code restart, or the residual risk is explicitly documented as accepted if it doesn't.
  - *Given* a Claude Code session with an `mcp-servers.json` entry pointing at the HTTP transport and an active tool-call history, *When* the daemon is `kill -9`'d and restarted (systemd/launchd auto-restart, or manually), *Then* the next tool call either succeeds transparently (best case) or fails with a legible error that resolves on retry without restarting Claude Code itself — either outcome gets recorded in this project's `NOTES.md` phase-log entry (per `README.md`'s existing "see NOTES.md for the phase-by-phase build log" convention) so the residual risk is documented either way.
**Files**: `NOTES.md`

##### Task 9.1.1a: Manual test + `NOTES.md` entry (~5 min, manual — not automatable)
- Files: `NOTES.md`

---

#### Story 9.1.2: Confirm the process-count/RSS reduction — the actual outcome metric, not just transport correctness
**As a** ship-readiness gate, **I want** direct before/after evidence that this migration solves the problem it was chartered for (`requirements.md`'s Problem Statement, issue #37 — "many per-session thin clients" → "zero"), **so that** this doesn't ship having only proven the new transport works without proving the original complaint is actually resolved.
**Acceptance Criteria**:
- A `pstree`/`ps` comparison, taken before (N Claude Code sessions on the stdio thin-client path) and after (the same N sessions reconfigured onto the HTTP path), shows the expected drop.
  - *Given* N Claude Code sessions (Tyler picks a representative N, e.g. matching the "9-subagent tree" the README's own origin story cites) all running against `stapler-mcp` over the stdio thin-client path, *When* `pstree -p $(pgrep -f 'stapler-mcp --daemon' | head -1)` (or equivalent `ps`) is captured and shows N `stapler-mcp` thin-client child processes, *Then* the same N sessions reconfigured to the HTTP transport (surface 1) show zero per-session `stapler-mcp` child processes in the equivalent `pstree`/`ps` capture — only the one shared `--daemon` process remains. Record both captures (or their RSS totals) in `NOTES.md`.
**Files**: `NOTES.md`

##### Task 9.1.2a: Manual before/after `pstree`/`ps` capture + `NOTES.md` entry (~10 min, manual — not automatable)
- Files: `NOTES.md`

---

#### Story 9.1.3: Latency comparison — HTTP path vs. the existing stdio/socket path
**As a** ship-readiness gate, **I want** a real number for `requirements.md`'s Non-functional Requirement ("should not regress the current per-call latency of a Unix-socket round trip"), **so that** this isn't shipped on an unverified assumption (`adversarial-review.md`'s CONCERN, flagged across two prior review rounds and never actioned; `research/pitfalls.md` §5).
**Acceptance Criteria**:
- A representative tool call's latency is measured over both paths and recorded — no pass/fail threshold, since this NFR has no hard SLO; the task's job is to produce a number.
  - *Given* a running HTTP-enabled daemon, *When* a representative tool call (e.g. `ping` or a cheap `stapler_browser_list_sessions`) is timed over N repetitions (e.g. 20) via the stdio `client::call` path and, separately, via an HTTP POST to `/mcp`, *Then* both median/mean latencies are recorded side by side in `NOTES.md`, with no acceptance gate on the delta — this is a manual/scripted timing comparison, not a benchmark suite.
**Files**: `NOTES.md`

##### Task 9.1.3a: Manual/scripted latency comparison + `NOTES.md` entry (~10 min)
- Files: `NOTES.md`
