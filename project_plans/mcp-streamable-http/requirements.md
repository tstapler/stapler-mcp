# Requirements: mcp-streamable-http

**Date**: 2026-09-14
**Type**: feature addition / transport migration
**Complexity**: 4 — high-stakes / cross-cutting (changes the daemon's concurrency model, not just its transport; touches every consumer's MCP client config; adds a new local attack surface)
**Backlog item**: 33bcf54c-b854-4aff-9457-1898250fba70

## Problem Statement
Every MCP client session (each Claude Code session, each subagent) spawns its own `stapler-mcp` thin-client process over stdio, which proxies tool calls to the single shared `--daemon` over a Unix socket (`crates/cli/src/main.rs::run_thin_client`, `crates/cli/src/thin_client.rs`). Each thin client is cheap individually (~4MB RSS), but the *count* of processes scales with concurrent sessions/subagents, which is the thing worth fixing per issue #37. The proposal is to have the daemon serve MCP directly over Streamable HTTP so no per-session process is spawned at all.

## Baseline
- `crates/cli/src/thin_client.rs` holds the *entire* rmcp `ServerHandler`/`#[tool_router]` implementation (`ThinClient`, 27 `#[tool]`-annotated methods) and speaks real MCP protocol to Claude Code over `rmcp::transport::stdio()`. Each tool method calls `call_daemon`, which calls `stapler_mcp_core::client::ensure_daemon` (spawns `--daemon` if unreachable) then `client::call`, which JSON-encodes the input, opens a Unix socket connection, and round-trips through the daemon's **hand-rolled** `Request`/`Response` framing (`crates/core/src/protocol.rs`).
- `crates/cli/src/main.rs::run_daemon` does **not** use rmcp's `ServerHandler` at all. It builds a `stapler_mcp_core::daemon::Daemon` — a plain `HashMap<&str, Handler>` dispatcher (`crates/core/src/daemon.rs`) — and registers each tool as a closure via `json_handler`. The daemon serves this over a raw length-framed JSON protocol on a Unix socket, not MCP.
- The daemon's core (`crates/core`) is **deliberately single-threaded**: `daemon.rs`'s own doc comment states "Deliberately single-threaded (no `Send` bounds anywhere) — the native binary runs this on a `current_thread` tokio runtime + `LocalSet`, which is what lets the exact same handler-registry code also satisfy a `!Send` wasm-bindgen adapter later." State is held in `Rc<RefCell<...>>` (e.g. `NativeBrowser`, `Daemon`'s own `handlers`/`shutdown` fields), not `Arc<Mutex<...>>`.
- `rmcp` 2.2.0 is already a dependency (`crates/cli/Cargo.toml`, features `["server", "macros", "transport-io", "schemars"]`) but **not** with the `transport-streamable-http-server` feature.
- Lifecycle today: the daemon is lazily started by whichever thin client's `ensure_daemon` call finds it unreachable first (`crates/core/src/client.rs::ensure_daemon`, backed by `crates/native/src/spawn.rs::NativeSpawner`); an flock on `~/.stapler-mcp/daemon.lock` arbitrates the race (`crates/native/src/lock.rs`). No systemd/launchd unit exists anywhere in this repo today (confirmed: no `.plist`/`.service` files, no launchd/systemd mentions outside `project_plans/docs-index/`'s own unrelated research notes).

## Verified architectural finding (changes the shape of this work)
This is **not** a pure transport-layer swap, contrary to how the backlog item's own open question frames it ("should be largely a transport-layer swap since the daemon already serves concurrent Unix-socket clients today"). Two independent facts combine to make it a genuine redesign:

1. **The MCP `ServerHandler` lives in the wrong process today.** `ThinClient` (which implements `rmcp::ServerHandler`) is only ever constructed by the stdio thin client, never by `run_daemon`. Hosting MCP over Streamable HTTP *from the daemon* means either (a) moving/duplicating the 27-tool `#[tool_router]` block from `thin_client.rs` into the daemon binary and rewiring each tool method to call the local handler directly instead of `call_daemon`'s socket round-trip, or (b) keeping the existing hand-rolled `Daemon` dispatcher as-is and writing a *new*, separate translation layer that speaks MCP-over-HTTP on the front and calls `Daemon::handle_request_bytes` on the back — effectively re-implementing what `ThinClient` already does, just in-process instead of over a socket. Either way, this is new design work, not a `stdio()` → `streamable_http_server()` one-line transport substitution.
2. **`rmcp`'s Streamable HTTP server requires `Send`.** `StreamableHttpService<S, M>`'s `tower::Service` impl (`rmcp-2.2.0/src/transport/streamable_http_server/tower.rs:571-577`) bounds `S: crate::Service<RoleServer> + Send + 'static`. The daemon's whole core is intentionally `!Send` (`Rc<RefCell<...>>`, no `Send` bounds, single `current_thread` + `LocalSet` runtime) specifically so the same handler-registry code doubles as the wasm-bindgen adapter. Putting a `Send`-bound HTTP server directly in front of that `!Send` core does not typecheck as-is. This needs an explicit design decision in planning: e.g. bridge via an internal channel from `Send` HTTP-handling tasks (multi-threaded runtime, or `tokio::task::spawn` off the `LocalSet`) into the existing `!Send` `LocalSet`-confined state, versus reworking the shared state to `Arc<Mutex<...>>`/`Arc<tokio::sync::Mutex<...>>` and dropping the `!Send` constraint (which would also affect the wasm adapter's shared-code story — need to confirm whether that's actually still a live constraint or already redundant since wasm and native are separate crates now).

## Users / Consumers
- Every local MCP client that currently launches `stapler-mcp` as a stdio subprocess per session: Claude Code (each session + each subagent), and any other MCP-capable client configured the same way via `mcp-servers.json`.
- `stapler-scripts/llm-sync` (per the dotfiles repo's `AGENTS.md`), which mirrors `mcp-servers.json` entries to Gemini/OpenCode/Antigravity — a transport-type change here has downstream effects there, out of scope for this repo but worth flagging.

## Success Metrics
- `stapler-mcp --daemon` serves MCP tool calls directly over Streamable HTTP on `127.0.0.1:<port>`, reachable by an MCP client configured with `{"type": "http", "url": "http://127.0.0.1:<port>/mcp", "headers": {...}}` — no `command`/`args` stdio subprocess spawned per session.
- All 27 tools currently exposed by `ThinClient` (see `crates/cli/src/thin_client.rs`) remain callable with identical input/output schemas via the new HTTP transport — verified by an integration test analogous to the existing stdio `tools/list`/`tools/call` coverage (`crates/cli/tests/tool_schema.rs` precedent).
- Concurrent sessions (multiple simultaneous HTTP client connections) can call tools against the shared daemon state (browser pool, embedder, docs index) without corruption or deadlock — verified by a test that drives ≥2 concurrent HTTP sessions against a stateful tool (e.g. a browser-session tool) at once.
- A bearer-token (or equivalent) auth check rejects unauthenticated requests to the HTTP endpoint before this ships — since a Unix socket's filesystem permissions implicitly scope access to the daemon's owning user, and a TCP port on `127.0.0.1` does not.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` both pass with no new warnings.
- The original problem (per the Problem Statement, sourced from issue #37) is actually solved, not just the new transport made correct: process count/RSS for N concurrent Claude Code sessions drops from N stdio thin-client processes to zero once those sessions are reconfigured onto the HTTP path — verified by a `pstree`/`ps` before-vs-after comparison (see `implementation/plan.md` Phase 9), mirroring the diagnostic method the README's own origin story used to first identify the problem.

## Appetite
Large (2-4+ weeks) — this reaches into the daemon's core concurrency model (see "Verified architectural finding" above), not just a config/transport swap. Actual sizing is a plan-phase output once the `Send`-bridging design is chosen.

The underlying problem this fixes is low-severity by the item's own framing (per-session thin clients are cheap individually — "nice to have less, not a leak") — the Appetite is justified by the real architectural work required (the `!Send`/`Send` bridge, a new auth surface, new lifecycle management), not by the severity of the problem being fixed. This is an honest tradeoff to state up front, not a scope change.

## Constraints
- Solo project — no dedicated QA beyond automated tests and manual spot checks.
- Must not silently drop the existing stdio thin-client path without a documented decision — machines without a persistent launcher (systemd/launchd unit) installed have no other way to reach an MCP server that isn't started per-session (see Open Questions).
- Whatever concurrency-bridging design is chosen must not break the wasm adapter's use of the same `crates/core` handler-registry code (needs re-confirmation of whether that shared-code constraint is still live and load-bearing, per the architectural finding above).
- Security: bearer-token auth (or equivalent) is a hard ship-blocker per the item description, not a fast-follow.

## Non-functional Requirements
- **Performance**: no explicit SLO; should not regress the current per-call latency of a Unix-socket round trip (already low — same-host, single hop).
- **Scalability**: same design target as the existing daemon — a handful of concurrent local sessions (single user, interactive use), not a multi-tenant/high-concurrency server.
- **Security classification**: local-only, but the trust boundary changes materially: a Unix socket restricts access via filesystem permissions (same-user only, no other process on the machine can connect without also being that user); a TCP port on `127.0.0.1` is reachable by *any* local process/user with network namespace access, regardless of file permissions. Bearer-token auth is required to restore an equivalent trust boundary.
- **Availability/lifecycle**: today's lazy "first thin client spawns the daemon" model breaks once no thin client exists to do the spawning. A persistent launcher (systemd user unit on Linux, `launchd` user agent on macOS) is required for this to work unattended; needs an explicit decision on what happens on machines without one installed (see Open Questions).

## Scope
### In Scope
- Add `transport-streamable-http-server` (and any features it depends on transitively — see research) to `rmcp`'s feature set in `crates/cli/Cargo.toml`.
- Design and implement how the daemon hosts MCP over Streamable HTTP given the `!Send` constraint identified above — this is the core design decision for the plan phase.
- Wire all 27 existing tools (currently defined only in `ThinClient`) to be reachable via the new HTTP-hosted `ServerHandler`, with schema/behavior parity to the existing stdio path.
- Bind to `127.0.0.1:<port>` (port: fixed default vs. configurable — plan-phase decision) plus bearer-token authentication (Claude Code's `http` transport type supports a `headers` field for this).
- Update this repo's own README/docs describing the thin-client + daemon architecture to reflect the new transport once implemented (the "Architecture: thin client + shared daemon" section currently states the stdio-thin-client model as "the entire point of the project, not an optional nicety" — this needs a deliberate rewrite, not an incidental one).
- A systemd user unit (Linux) and/or `launchd` user agent (macOS) example/template for starting the daemon at login with auto-restart (`KeepAlive`/`Restart=on-failure`) — at minimum as a documented example; whether this repo installs it automatically is a plan-phase scope call given this repo has no existing Ansible/install-script ownership of `stapler-mcp`'s consumers' machine setup (that lives in the separate `dotfiles` repo's `bootstrap-pyinfra/`).

### Out of Scope
- Actually decommissioning the stdio thin-client path — per the item's own open question, whether it stays as a fallback transport is undecided; default to keeping it available unless research/planning finds a concrete reason to remove it.
- Non-localhost network exposure (binding to anything other than `127.0.0.1`) — this stays a local-machine-only daemon.
- Changes to `stapler-scripts/llm-sync` in the sibling `dotfiles` repo (out of this repo's scope; flagged as a downstream consumer only).
- Any change to the wasm/`crates/wasm` adapter's own transport (it doesn't currently speak MCP directly and isn't part of this proposal).

## Rabbit Holes
- **The `!Send` bridge design** (see "Verified architectural finding") is the single largest source of risk and rework in this project. Get this wrong and either the daemon's shared state needs an invasive `Arc<Mutex<...>>` rewrite, or the HTTP-serving layer ends up bolted on awkwardly (e.g. a second runtime, cross-runtime channels, and all the lifecycle/shutdown coordination that implies).
- **Duplicated tool-registration boilerplate**: today tool schemas/descriptions live once in `thin_client.rs` (`#[tool_router]`) and handler wiring lives once in `main.rs::run_daemon` (`Daemon::register`). Whatever design is chosen must avoid ending up with *three* copies of "list of 27 tools" (stdio `ServerHandler`, daemon's HTTP `ServerHandler`, daemon's internal `Daemon` dispatcher) — a maintenance trap where adding a 28th tool requires remembering three edit sites.
- **Session semantics under `transport-streamable-http-server-session`**: needs confirming whether per-client SSE sessions interact safely with the daemon's existing shared state (browser pool, embedder) — the item's own open question, still unresolved, now sharper given the `!Send` finding: a session's `Send` HTTP-handling task must safely coordinate with `!Send` daemon-owned state without introducing thread-affinity bugs (e.g. a browser handle used from the wrong thread).
- **Auth-token distribution**: where the bearer token is generated, stored, and how `mcp-servers.json` on each consumer machine picks it up without becoming another manually-synced secret.

## Alternatives Considered
- **Status quo (stdio thin client per session)**: rejected as the status quo this item exists to fix — cheap individually but the process count itself was flagged (issue #37) as the thing worth eliminating.
- **Keep the Unix-socket transport, but make the thin client persistent/pooled instead of per-session**: not proposed by the item but worth surfacing in research as a lower-risk alternative that would avoid both the HTTP auth surface and the `!Send` bridging problem — plan phase should at least note why HTTP was preferred (if the reason is purely "MCP clients speak `http` transport type more simply than a Unix-socket relay," a persistent multiplexing thin client achieves the "no new process per session" goal too and stays `!Send`-compatible with the existing architecture).
  **Why HTTP was chosen anyway** (per `research/build-vs-buy.md` Option 3): the MCP stdio transport is spec-modeled as one-stdio-pipe-per-client-connection, so a persistent thin client doesn't satisfy the item's literal ask ("daemon speaks MCP directly," reachable via `{"type": "http", ...}` with no `command`/`args` subprocess) without inventing a new mux protocol this codebase and the MCP spec have no precedent for — build-vs-buy.md found no answer for whether a single physical subprocess can transparently multiplex multiple logical sessions under the stdio transport model. HTTP is also the transport MCP clients (Claude Code) already speak natively, with no custom relay/multiplexing logic required on our side. Both reasons hold even though the pooled thin client is lower-risk and solves the same process-count problem — it was surfaced and explicitly deferred, not silently dropped.

## Feasibility Risks
- The `Send` bound on `rmcp`'s Streamable HTTP server (verified above) may force a larger rework than the item's description anticipates ("should be largely a transport-layer swap") — this is a real risk to the Appetite estimate, not a hypothetical.
- No systemd/launchd unit exists anywhere in this repo or its install tooling today; this item would be introducing net-new machine-lifecycle-management scope this repo has not previously owned.
- Bearer-token auth design (generation, storage, rotation, distribution to `mcp-servers.json`) has no existing precedent in this codebase to follow.

## Observability Requirements
- Log HTTP session creation/teardown at the same level as existing daemon logging (`log_path` usage precedent, per the browser-automation project's own observability note).
- Log rejected (missing/invalid bearer token) requests distinctly from tool-call errors, since these indicate a potential unauthorized-access attempt worth being able to grep for.

## Risk Control
- No feature-flag system exists in this codebase. Recommend the Streamable HTTP path ship additively alongside the existing stdio path (per Out of Scope) so a broken HTTP transport doesn't strand every consumer — rollback is "point `mcp-servers.json` back at the stdio command."
- No staged rollout infrastructure exists (single local daemon, no external users).

## Open Questions
- Does `transport-streamable-http-server-session` handle multiple concurrent client sessions cleanly against the daemon's shared `!Send` state? (Item's own open question — now sharpened by the verified `Send`-bound finding above; this is a design question for planning, not a testable unknown to defer.)
- Fallback story for machines without a persistent launcher installed: does the stdio thin-client path stay available indefinitely, or is there a deprecation plan? (Item's own open question; this requirements doc defaults to "stays available" per Out of Scope pending a planning-phase decision.)
- Is the `!Send`/wasm-shared-code constraint still load-bearing today, or has `crates/wasm` diverged enough from `crates/core`'s daemon-registration path that relaxing `!Send` in the native daemon specifically (not touching wasm) is actually free? Needs a direct answer in research before the plan phase commits to a bridging design.
- Where does the bearer token live (env var, file under `~/.stapler-mcp/`, OS keychain via the existing `secrets`/1Password tooling this user already uses elsewhere)? Not specified by the item; plan-phase decision.
- Fixed vs. configurable port for the HTTP listener, and how a consumer's `mcp-servers.json` discovers it if configurable.
