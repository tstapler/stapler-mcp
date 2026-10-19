# Build vs. Buy: mcp-streamable-http

Grounds every claim below in the vendored `rmcp` 2.2.0 source
(`~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/rmcp-2.2.0`), this
repo's current code, and web research on prior art / known issues. See
`project_plans/mcp-streamable-http/requirements.md` for the authoritative
spec this evaluates against.

## Summary

| # | Option | Verdict |
|---|--------|---------|
| 1 | `rmcp`'s own `transport-streamable-http-server` | **Recommended**, conditional on the Send-bridge (Q6) being planned as first-class work |
| 2 | Hand-rolled HTTP+SSE, thin client keeps doing MCP translation | **Not recommended** — relocates the Send/!Send problem instead of solving the actual goal |
| 3 | Persistent/pooled thin-client multiplexer | **Viable** fallback — lower risk, but doesn't meet the item's literal HTTP requirement |
| 4 | SaaS/managed | N/A, dismissed |
| 5 | Fork/adapt prior art | No direct fork target found; two reference points worth citing (below) |
| 6 | Send/!Send channel-bridge pattern | Well-trodden in principle (canonical Tokio pattern); correctness risk is in the shutdown/backpressure details, not the concept |

## 1. `rmcp`'s `transport-streamable-http-server`

**Pros**
- Officially maintained alongside the SDK already vendored at `rmcp = { version = "2", features = [...] }` in [`crates/cli/Cargo.toml`](https://github.com/tstapler/stapler-mcp/blob/90745240d5d07d551c98eff05cc10c4c4f702090/crates/cli/Cargo.toml#L14) — enabling `transport-streamable-http-server` is additive, not a new dependency.
- Handles the parts of the Streamable HTTP spec that are tedious and easy to get subtly wrong by hand: session ID headers, SSE resumption via `Last-Event-ID`, POST/GET/DELETE semantics, DNS-rebinding `Host` validation.
- That last point isn't theoretical: `rmcp` shipped a DNS-rebinding fix in 1.4.0 (`StreamableHttpServerConfig::allowed_hosts` now defaults to a loopback-only allowlist, GHSA-89vp-x53w-74fx) and a session-table-leak DoS fix in 2.0.0 (GHSA-9pj6-vhgr-3mwh, verified via `gh api repos/modelcontextprotocol/rust-sdk/security-advisories/...` — `patched_versions: "2.0.0"`, `vulnerable_version_range: "<= 1.7.0"`). The vendored 2.2.0 already contains both fixes. This is *both* a pro (upstream is actively hardening exactly this component, so bugs get fixed without this project having to find them) and a con (see below).
- As a `tower::Service`, bearer-auth wiring is idiomatic: a `tower-http` layer (`ValidateRequestHeaderLayer` or a small custom layer) composes in front of it, matching how Claude Code's `http` transport's `headers` field is meant to be consumed.

**Cons**
- Confirmed showstopper, not hypothetical: `StreamableHttpService<S, M>`'s `tower_service::Service` impl bounds `S: crate::Service<RoleServer> + Send + 'static` (rmcp-2.2.0 `src/transport/streamable_http_server/tower.rs`, the `impl<RequestBody, S, M> tower_service::Service<...>` block). The daemon's core is deliberately `!Send` — [`crates/core/src/daemon.rs`](https://github.com/tstapler/stapler-mcp/blob/628ec11be71645f42ecbe6dcd11d432c52f4a4e6/crates/core/src/daemon.rs#L1-L6)'s own module doc says so explicitly ("Deliberately single-threaded (no `Send` bounds anywhere)"), backed by `RefCell<HashMap<...>>` handlers and an `Rc<Cell<bool>>` shutdown flag, run from a `current_thread` runtime + `LocalSet` in [`crates/cli/src/main.rs`](https://github.com/tstapler/stapler-mcp/blob/628ec11be71645f42ecbe6dcd11d432c52f4a4e6/crates/cli/src/main.rs#L25-L43). You cannot hand `Daemon` (or a `ServerHandler` built directly on top of it) to `StreamableHttpService` as-is.
- This forces new design work, not a feature-flag flip: either (a) move/duplicate `ThinClient`'s 23-tool `#[tool_router]` block from [`thin_client.rs`](https://github.com/tstapler/stapler-mcp/blob/51def0e9b3e34908241ed37adf27d044e2b56d9c/crates/cli/src/thin_client.rs) into the daemon and rewire each tool off `call_daemon`'s socket round-trip to a direct local call, or (b) build the Send/!Send bridge (Q6). Either path is exactly the "not a pure transport swap" finding requirements.md already flags.
- `transport-streamable-http-server` transitively pulls `transport-streamable-http-server-session` (rmcp Cargo.toml, `transport-streamable-http-server = ["transport-streamable-http-server-session", ...]`), which brings SSE resumption, a `SessionStore` abstraction, and pending-restore bookkeeping (`pending_restores: Option<Arc<RwLock<HashMap<SessionId, watch::Sender<...>>>>>` in `tower.rs`) — real complexity for a workload requirements.md itself scopes as "a handful of concurrent local sessions (single user, interactive use)."
- The two CVEs above are evidence this component is a more complex, actively-changing attack surface than a minimal hand-rolled listener would be — already fixed in 2.2.0, but a signal that upstream `rmcp`'s HTTP session code has non-trivial edge cases worth budgeting review time for, not "batteries included, zero risk."

**Verdict: Recommended, conditionally.** This is what the item's Success Metrics literally specify (an MCP client configured with `{"type": "http", ...}`, no per-session subprocess) and it is the officially supported transport for the SDK already in use. Recommend it *if and only if* the plan phase treats the Send-bridge design (Q6) as a first-class task with its own review, not something assumed away by "just enable the feature." Given Appetite is already sized Large (2-4+ weeks) and requirements.md's own architectural finding already prices this rework in, this remains the right default — but the three-copies-of-23-tools trap (Rabbit Holes in requirements.md) is a concrete constraint the plan phase must resolve, e.g. by generating the daemon's `ServerHandler` methods from the same registration table `Daemon::register` already uses, rather than hand-duplicating `ThinClient`.

## 2. Hand-rolled HTTP+SSE over the existing hand-rolled protocol

Keep `crates/core/src/protocol.rs`'s `{tool, params}` / `{result|error}` JSON wire shape, swap the Unix-socket `SocketFactory` for a minimal HTTP listener, and leave `ThinClient` (the actual MCP-speaking `ServerHandler`) as a stdio subprocess that now dials HTTP instead of a Unix socket.

**Pros**
- Genuinely sidesteps Send/!Send: [`protocol.rs`](https://github.com/tstapler/stapler-mcp/blob/628ec11be71645f42ecbe6dcd11d432c52f4a4e6/crates/core/src/protocol.rs) and `Daemon::handle_request_bytes` are already transport-agnostic pure functions; a minimal HTTP accept loop in place of the Unix-socket one is close to what the backlog item's own open question assumed ("should be largely a transport-layer swap").
- Much smaller surface than option 1: no SSE resumption, no session store, no `allowed_hosts` DNS-rebinding logic to replicate (though a from-scratch listener would still need to reimplement `Host` validation and bearer auth by hand to reach parity with what `rmcp` already ships).

**Cons — decisive**
- Does not satisfy the item's stated Success Metric: "`stapler-mcp --daemon` serves MCP tool calls directly over Streamable HTTP... reachable by an MCP client configured with `{"type": "http", "url": ...}`" (requirements.md line 29). A client using the `http` transport type expects real MCP-over-HTTP (JSON-RPC envelope, `initialize` handshake, `tools/list`, spec-shaped SSE) — a bespoke `{tool,params}` JSON-over-HTTP endpoint is not that, and Claude Code's `http` client will not speak to it correctly.
- Critically, it **does not eliminate the thin-client process** — the 23-tool `#[tool_router]`/`ServerHandler` machinery still has to live somewhere to translate real MCP into this wire format, and that's exactly `ThinClient`'s job today. Relocating its *inner* transport from Unix socket to HTTP changes nothing about the fact that each Claude Code session still spawns its own `stapler-mcp` stdio subprocess. Issue #37's actual complaint — process count scaling with sessions — is untouched.
- Net assessment: this option looks like it dodges the Send/!Send conflict, but only because it moves the MCP-`ServerHandler`-must-be-somewhere problem sideways rather than resolving it. It's strictly dominated by option 3, which achieves the same "no daemon-side Send rework" property while actually addressing the process-count goal.

**Verdict: Not recommended.** Worth naming explicitly in the plan phase as a rejected alternative (it's an easy thing to reach for reflexively once the Send bound is understood) with this exact reasoning: it trades a real design problem for a fake one.

## 3. Persistent/pooled thin-client multiplexer

One long-lived stdio-or-socket-based process replacing per-session spawn of `ThinClient`, leaving the daemon and its Unix-socket transport untouched.

**Pros**
- Solves the actual, named goal (issue #37: process count) directly, without touching the daemon's transport or its `!Send` core at all — `client::call` in [`crates/core/src/client.rs`](https://github.com/tstapler/stapler-mcp/blob/628ec11be71645f42ecbe6dcd11d432c52f4a4e6/crates/core/src/client.rs) keeps working exactly as today.
- No Send/!Send bridge needed anywhere — only the *lifecycle* of the `ThinClient` process changes (long-lived vs. spawn-per-session), not its architecture.
- Avoids the entire new local-network attack surface this item otherwise has to open: no TCP port on `127.0.0.1`, no bearer-token generation/storage/distribution problem, no DNS-rebinding or session-table-leak bug class to worry about. Stays inside the trust boundary requirements.md's own Security Classification section says is already correct (Unix socket filesystem permissions, same-user-only) rather than having to re-earn an equivalent boundary via auth.
- Smaller, better-understood implementation problem: multiplexing N logical clients over one long-lived process is a connection-pooling problem, not a concurrency-model redesign.

**Cons**
- Does not satisfy the item's literal Success Metrics either. `mcp-servers.json` entries are still `command`/`args` stdio configs per the MCP stdio transport's spec model (one stdio pipe = one client connection) — whether a single physical subprocess can transparently serve multiple concurrent *logical* Claude Code sessions without a client-side protocol change is an open question this research did not find answered in the MCP spec's stdio transport section or anywhere in this codebase. If it can't, this option needs its own new mux protocol (e.g. the persistent process accepting Unix-socket connections from short-lived per-session shims), which dilutes the "lower risk" claim.
- Explicitly out of scope per requirements.md's own text ("not proposed by the item... worth surfacing in research as a lower-risk alternative"). Adopting it as the actual implementation choice would be a scope pivot from what was chartered, not a research footnote — it needs a deliberate decision, not a default.

**Verdict: Viable, surface-don't-silently-drop.** Meaningfully lower-risk than option 1 and closer to "reuses existing architecture" than either alternative. Recommend the plan phase document this as the considered-and-explicitly-deferred alternative, with the rationale requirements.md already asks for: if HTTP was chosen purely because "MCP clients speak `http` more simply than relaying a Unix socket," this option meets the same "no process-count blowup" goal while staying `!Send`-compatible — but if HTTP access (e.g. from tooling outside the stdio-subprocess model, or future non-Claude-Code consumers) is a real driver, option 1 is required regardless of the extra Send-bridge cost. Recommend making this call explicit in the plan phase rather than assuming HTTP is warranted by the process-count goal alone.

## 4. SaaS/managed

**N/A, dismissed.** The daemon is local-only by explicit constraint: "Non-localhost network exposure (binding to anything other than `127.0.0.1`) — this stays a local-machine-only daemon" (requirements.md, Out of Scope). It needs access to the user's own local browser pool, filesystem, and embedder cache; no managed/hosted MCP gateway product is a fit for a process that must run co-located with those resources. Not evaluated further.

## 5. Prior art / fork-adapt candidates

Web search turned up no project that is a direct fork target for "shared daemon speaking Streamable HTTP with bearer auth over a `!Send` single-threaded core," but two references are worth citing for pattern comparison, not code reuse:

- **Warmplane** ([github.com/Warmplane/warmplane](https://github.com/Warmplane/warmplane)) — a Rust "local control plane" that explicitly supports "daemon-co-hosted HTTP/SSE MCP server endpoints" alongside a core daemon, with bearer-token auth "automatically enforced when binding to public non-loopback network interfaces." Confirms the daemon-hosts-HTTP-MCP-directly shape is a validated pattern elsewhere, and that scoping bearer auth to non-loopback binds (vs. always-on) is a design choice worth at least considering here — though requirements.md already treats bearer auth as a hard ship-blocker regardless of bind address, which this research doesn't find reason to relax.
- **windbgr-mcp** ([docs.rs/windbgr-mcp](https://docs.rs/windbgr-mcp/latest/windbgr_mcp/)) — an MCP server explicitly described as exposing tools "over stdio and Streamable HTTP," i.e. dual-transport like this item's Out-of-Scope decision to keep stdio available. Confirms dual-transport-simultaneously is a done thing elsewhere, not a novel risk unique to this project.
- No project was found that documents solving the specific `!Send`-core-behind-a-`Send`-required-HTTP-server problem for `rmcp` specifically — the Send-bridge design in Q6 below is this project's own problem to solve, informed by general Tokio patterns rather than an MCP-specific precedent.

## 6. The Send/!Send bridge: well-trodden or bespoke?

**Well-trodden as a general Tokio pattern; the specifics here need real design attention.**

The shape needed — `Send` HTTP-handling tasks communicating into a `!Send`, `LocalSet`-confined worker via a channel — is the textbook "actor" pattern documented in [Alice Ryhl's "Actors with Tokio"](https://ryhl.io/blog/actors-with-tokio/), and is exactly the accepted answer to [tokio-rs/tokio#2095, "Is it possible to use LocalSet from within a spawned task?"](https://github.com/tokio-rs/tokio/issues/2095): create the `LocalSet` on a dedicated thread/runtime, and communicate with it via an `mpsc` channel from `Send` tasks elsewhere, with a response returned via `oneshot`. This is not a novel technique — it's the standard way to wrap `!Send` resources (GUI handles, non-thread-safe C bindings, single-threaded database drivers) behind a `Send` interface, and [tokio-rs/tokio#2397](https://github.com/tokio-rs/tokio/issues/2397) documents *why* naive alternatives (moving a `!Send` future directly into a multi-threaded `LocalSet`) are unsound, reinforcing that the channel-based bridge — not a shortcut — is the correct shape.

**One concrete, favorable finding from this codebase specifically:** the daemon's existing `run` loop is *already* strictly serial — [`Daemon::run`](https://github.com/tstapler/stapler-mcp/blob/628ec11be71645f42ecbe6dcd11d432c52f4a4e6/crates/core/src/daemon.rs#L80-L98) accepts one Unix-socket connection, fully awaits `handle_request_bytes`, writes the response, and only then loops back to `accept()` again — no per-connection spawning, no concurrent dispatch today. That means the bridge doesn't need to introduce new concurrency semantics into the `!Send` core at all; it only needs to add a second request *source* (an `mpsc::Receiver` fed by the HTTP side) that the same loop polls via `tokio::select!` alongside the existing `listener.accept()`, dispatching through the same `dispatch()` either way. This is a smaller, more natural change than "redesign the core's concurrency model" — but note that requirements.md's Success Metrics require *concurrent* HTTP sessions against shared state "without corruption or deadlock," which the current strictly-serial loop trivially satisfies by construction (one request in flight at a time) — the plan phase should confirm that serial dispatch is an acceptable latency tradeoff under concurrent HTTP load (a slow tool call from session A blocks session B's calls until it completes) rather than assuming "no corruption" implies "no user-visible contention."

**Where the real risk lives** — not in the channel-bridge concept, but in getting the following details right, none of which are addressed by citing the pattern alone:
- **Shutdown draining**: when `Daemon::run`'s loop exits (the `shutdown` flag path), in-flight `oneshot` senders held by not-yet-processed `mpsc` messages must not simply be dropped — a dropped `oneshot::Sender` surfaces to the HTTP-side caller as a `RecvError`, which needs to be mapped to a clean MCP tool-call error, not left to panic or hang an HTTP response indefinitely.
- **Backpressure**: an unbounded `mpsc` channel here would echo the exact failure shape of the already-patched `GHSA-9pj6-vhgr-3mwh` session-leak DoS (unbounded growth under request pressure) — the channel must be bounded, with an explicit decision on what happens when it's full (reject with backpressure vs. block the HTTP task).
- **Where the bridge task itself lives**: whether the HTTP-serving side runs on a second (multi-threaded) runtime that must itself be spun up and shut down in lockstep with the existing `current_thread` + `LocalSet` runtime, or whether `tokio::task::spawn` (not `spawn_local`) is used from within the same process for the `Send` HTTP tasks while the channel receiver runs as a `spawn_local` task on the existing `LocalSet` — this is a concrete decision requirements.md correctly flags as needing "explicit design" and this research confirms has no MCP-specific precedent to crib from (see Q5).

**Verdict:** cite the pattern with confidence (it's canonical, not bespoke), but budget real design and test time for the shutdown and backpressure edges specifically — those are exactly the kind of details a generic "use a channel" citation doesn't cover, and get either one wrong and the failure mode is a deadlock or a dropped request under daemon shutdown/restart, which is precisely the risk requirements.md's Rabbit Holes section already flags as "the single largest source of risk and rework in this project."
