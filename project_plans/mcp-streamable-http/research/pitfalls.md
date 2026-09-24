# Research: Pitfalls

Scope: known failure modes for (1) local HTTP MCP servers as an attack surface, (2) bearer-token
handling, (3) migration/rollout mechanics specific to this transport change, (4) `!Send`/concurrency
footguns bridging the daemon's `Rc<RefCell<...>>` core to a `Send`-bound HTTP layer, (5) general
Streamable HTTP transport tuning. Each finding is labeled VERIFIED (local source/command read) or
UNVERIFIED/INFERRED (web search only, cited).

## 1. Local HTTP MCP servers as an attack surface

**The core trust-boundary change is real and already well-precedented by CVEs in this exact
ecosystem, including in `rmcp` itself.**

- VERIFIED — `rmcp-2.2.0/src/transport/streamable_http_server/tower.rs:60-78`: `StreamableHttpServerConfig`
  defaults `allowed_hosts` to `["localhost", "127.0.0.1", "::1"]` (loopback-only `Host` header
  validation, which defeats classic DNS rebinding — the rebound hostname in the `Host` header won't
  match). But `allowed_origins` **defaults to an empty `Vec`, which the doc comment states explicitly
  disables `Origin` validation** (`tower.rs:70-78`). This means a malicious webpage's `fetch()`
  aimed directly at `http://127.0.0.1:<port>/mcp` (no DNS rebinding needed — just the literal loopback
  URL, which browsers allow non-`no-cors` requests to reach) passes the `Host` check trivially and
  is not blocked by `Origin` checking unless the daemon opts in. **This is the concrete reason bearer-token
  auth cannot be treated as optional even with `allowed_hosts` at its secure default** — Host validation
  stops rebinding, not same-origin-policy-bypassing direct requests from an already-loaded malicious page.
- VERIFIED — the vendored `rmcp-2.2.0/Cargo.toml:15` confirms package version `2.2.0`.
- UNVERIFIED/INFERRED (web search) — `rmcp`'s Streamable HTTP transport has **two prior CVEs in this
  exact code path**, both apparently fixed before 2.2.0 based on version-number comparison (not
  independently verified by reading the historical diff):
  - [GHSA-89vp-x53w-74fx / CVE-2026-42559](https://github.com/modelcontextprotocol/rust-sdk/security/advisories/GHSA-89vp-x53w-74fx) — prior to 1.4.0, the Streamable HTTP server didn't validate the `Host` header at all, so DNS rebinding could reach any tool with full read/write/exec capability the server exposed. Fixed in 1.4.0 by adding `validate_dns_rebinding_headers()` and the loopback-only `allowed_hosts` default — this matches what's read directly in `tower.rs` above, so the fix's *presence* is corroborated by source, even though the historical absence wasn't independently checked.
  - [GHSA-9pj6-vhgr-3mwh](https://github.com/modelcontextprotocol/rust-sdk/security/advisories/GHSA-9pj6-vhgr-3mwh) — versions ≤1.7.0: an unauthenticated POST that isn't a well-formed `InitializeRequest` leaks one session-table entry per request (session allocated before body validation, no cleanup on early-return), letting an attacker exhaust memory at >2000 req/s with zero auth. Fixed in 2.0.0.
  - Net: this is not a hypothetical risk category — `rmcp`'s HTTP transport has been the subject of two disclosed vulnerabilities in the same area this project is about to expose. **Plan-phase action item: pin `rmcp = "2.2"` (or higher) explicitly rather than a loose `"2"` range that could resolve backward on a future `cargo update`, and add `cargo audit`/`cargo deny` to CI** — there is currently no dependency-vulnerability scanning in this repo's checks (not verified beyond what `make ready`/CI config was inspected for this doc; recommend confirming in the plan phase).
- UNVERIFIED/INFERRED (web search) — [CVE-2025-49596 in `@modelcontextprotocol/inspector`](https://www.oligo.security/blog/critical-rce-vulnerability-in-anthropic-mcp-inspector-cve-2025-49596) (CVSS 9.4): an unauthenticated local HTTP proxy for MCP could be reached via DNS rebinding from a malicious webpage and used to spawn arbitrary stdio MCP servers — full RCE. Same MCP ecosystem, same "local HTTP surface + no auth + no Host validation" root cause this item's requirements doc already treats as a hard ship-blocker. Confirms the item's own risk framing rather than introducing a new one.
- UNVERIFIED/INFERRED (web search) — the [`0.0.0.0 Day` browser vulnerability](https://www.oligo.security/blog/0-0-0-0-day-exploiting-localhost-apis-from-the-browser) (18-year-old flaw, disclosed 2024) let public websites reach services bound to `0.0.0.0` via `fetch(..., {mode: "no-cors"})`, bypassing CORS/Private Network Access. Chrome 128+/Safari/Firefox now block this specifically for `0.0.0.0` — **but this repo's Out-of-Scope section already restricts binding to `127.0.0.1` only, which this browser-side mitigation does not cover** (127.0.0.1 was never blocked by the 0.0.0.0-day fix; it's a distinct, still-open surface, which is exactly why `allowed_hosts`/bearer-token auth still matter for a `127.0.0.1` bind).
- Net assessment: `allowed_hosts` alone (even at its secure default) is **not sufficient** — it stops rebinding-style attacks but not same-origin direct-URL attacks from a page that already knows/guesses the port. Bearer-token auth, as the requirements doc already asserts, is required to restore the trust boundary a Unix socket's file permissions gave for free. This corroborates rather than overturns the requirements doc's existing stance.

## 2. Bearer token handling pitfalls

- VERIFIED — `crates/native/src/lock.rs:33-39`: the daemon's *lock* file is opened with `.mode(0o600)`
  explicitly (owner-only). Whatever file eventually stores the bearer token (per the requirements doc's
  open question — "file under `~/.stapler-mcp/`?") **must follow this same explicit-mode pattern**; it is
  not the codebase's default behavior.
- VERIFIED — `crates/core/src/paths.rs:27-29` + `crates/native/src/spawn.rs:21-24`: `daemon.log` is opened
  via `OpenOptions::new().create(true).append(true)` with **no `.mode(...)` call at all**, unlike the lock
  file. Its permissions fall through to the process umask default (commonly `0644`/`0664`, group/world
  readable). **Concrete risk**: all daemon logging today is ad hoc `eprintln!(...)` (verified across
  `crates/native/src/{fs,embed,browser}.rs`, `crates/native/src/spawn.rs`) — there is no structured
  logging framework with header redaction. If the new HTTP layer's request/session logging is added in
  this same style (e.g. `eprintln!("request: {:?}", req)` for debugging an `http::Request`, which
  `Debug`-prints all headers including `Authorization`), a bearer token lands directly in a
  group/world-readable file. This is a genuine, codebase-specific gap to close in the plan/implementation
  phase (mandate an explicit redaction helper or scrub `Authorization` before any log line touches a
  request, and set the log file's mode like the lock file's).
- VERIFIED — `mcp-servers.json` (`/home/tstapler/dotfiles/.config/mcp/mcp-servers.json`) **is git-tracked
  and has commit history** (`git log` shows commits touching it in the `dotfiles` repo, e.g. adding the
  `stapler-mcp` stdio entry with inline documentation in `_fork`/`_requires` fields). The existing pattern
  for secrets in this file is to *not* inline them — e.g. `BRAVE_API_KEY` is documented as coming from
  "the environment of whichever thin client first spawns the daemon," with `"env": {}` left empty in the
  committed file. **If the HTTP transport's config is added following the obvious pattern (a `headers:
  {"Authorization": "Bearer <token>"}` block), a literal token would be committed to this already-tracked,
  already-`llm-sync`-mirrored file** — a direct regression versus the current env-var-indirection pattern.
- UNVERIFIED/INFERRED (web search) — [Claude Code GitHub issue #51581](https://github.com/anthropics/claude-code/issues/51581) and [#6204](https://github.com/anthropics/claude-code/issues/6204): `${ENV_VAR}` substitution, which **does** work in the stdio `env` block, is reported as **not working in the `http` transport's `headers` map** — the literal `${VAR}` string is sent as-is. If accurate and still unfixed, this closes off the obvious mitigation (keep the token out of the committed file via env-var interpolation in `headers`) and makes the token-in-committed-file risk from the previous bullet worse, not just theoretical. **Plan-phase action item: verify this bug's current status against the Claude Code version in use before committing to a `headers`-based design; if still broken, the token must be handled some other way** — e.g. the daemon writing a local-only token file that some wrapper script reads at MCP-client-launch time to build the header dynamically (mirroring how `BRAVE_API_KEY` is left to ambient environment rather than inlined), or per-machine `.gitignore`'d config, rather than a literal `mcp-servers.json` edit.
- UNVERIFIED/INFERRED (web search) — bearer-token comparison should use a constant-time equality check
  (e.g. the [`subtle`](https://docs.rs/subtle) or `constant_time_eq` crates) rather than `==`/`memcmp`-style
  string comparison, to avoid timing side-channels revealing the token byte-by-byte. Low real-world severity
  for a same-machine attacker who could otherwise just read the token file, but cheap to do correctly and
  worth a one-line note in the plan (`ConstantTimeEq` instead of `str::eq`).
- Token generation/rotation: no existing precedent in this codebase (requirements doc's own Feasibility
  Risks section already flags this). Generate with a CSPRNG (`rand::rngs::OsRng` or similar) at first
  daemon start, persist to a `0o600` file under `~/.stapler-mcp/`, and regenerate-on-demand rather than
  building any rotation *schedule* — this is a single-user local daemon, not a multi-tenant service, so
  scheduled rotation is over-engineering relative to the Appetite/Constraints already stated (solo project,
  no dedicated QA).

## 3. Migration/rollout pitfalls specific to this transport change

- VERIFIED — `crates/native/src/lock.rs:22-50` (`ProcessLock::acquire_exclusive`): the flock is a
  **process-uniqueness lock**, acquired via `std::fs::File::try_lock()` (OS-level `flock(2)`, released
  automatically on process exit, including a crash) on `~/.stapler-mcp/daemon.lock`, taken in
  `crates/cli/src/main.rs:71-83` **before** any browser/embedder/HTTP-listener setup happens. Because
  only one process can ever hold this lock, it transitively prevents two `stapler-mcp --daemon` processes
  from both reaching an HTTP-port-bind call — **but it does not protect against the port itself being
  already held by something unrelated to this lock** (a zombie/orphaned prior daemon process that somehow
  bypassed the lock, an unrelated process that happened to grab the same port, or a socket stuck in
  `TIME_WAIT`). In that scenario: the new daemon wins the flock race cleanly, then fails at `bind()`,
  exits non-zero, and a `systemd`/`launchd` unit configured with `Restart=on-failure`/`KeepAlive` will
  crash-loop it indefinitely without ever resolving the underlying port conflict. **This is a genuinely new
  failure mode this migration introduces** — the existing Unix-socket path has an analogous risk (stale
  `daemon.sock` file) but socket files don't have an equivalent to a lingering listening port outliving
  its process in the same way, and there's no existing crash-loop-guarding precedent in this repo (no
  systemd/launchd unit exists today, confirmed by the requirements doc). **Plan-phase action item**: the
  systemd/launchd unit template should set a bounded restart count/backoff (e.g. `StartLimitBurst`/
  `StartLimitIntervalSec` in systemd, `ThrottleInterval` in launchd) rather than unconditional
  `Restart=on-failure`, and the daemon should log the specific bind failure distinctly from other startup
  failures so an operator can diagnose "port taken by someone else" vs. "crashed for another reason" from
  the log alone (ties into the Observability Requirements already in the requirements doc).
- VERIFIED — `crates/core/src/client.rs:63-106` (`ensure_daemon`): thin clients auto-spawn the daemon on
  a failed ping and poll with backoff. This lazy-spawn model is **retained** per the requirements doc's
  Out-of-Scope decision to keep the stdio path available. **A genuinely mixed fleet is possible during
  rollout**: some `mcp-servers.json` entries pointing at the stdio `command` (still using the Unix socket),
  others pointing at the new `http` `url`. Since both would target the *same* daemon process and the daemon
  is single-threaded (`current_thread` + `LocalSet`, VERIFIED `crates/cli/src/main.rs:28-36`), there's no
  shared-mutable-state *corruption* risk from mixing transports — both dispatch through the same
  in-process handler registry serially. The real risk is operational: whichever transport's listener
  (Unix socket accept loop vs. HTTP server) is wired up *first* or with a bug in the newer HTTP path could
  silently degrade one transport while the other keeps working, making a partial rollout look
  successful when it's actually only validated on the old path. **Recommend an explicit dual-transport
  integration test** (both a stdio `tools/call` and an HTTP `tools/call` in the same test process against
  one daemon instance) rather than relying on manual spot checks, given "solo project, no dedicated QA"
  is a stated constraint.
- VERIFIED — restarting the daemon to pick up a new binary drops all in-flight state: `NativeBrowser`
  (browser pool), `NativeEmbedder`, `docs::SourceLocks` are all constructed fresh in `run_daemon`
  (`crates/cli/src/main.rs:86-100`) with no persistence across restarts. Under the old stdio model, a
  daemon restart mid-session meant the *next* `ensure_daemon` call transparently respawned it — each thin
  client call is a fresh Unix-socket dial, so there's no persistent client-side connection to break. Under
  Streamable HTTP's `stateful_mode` (default `true` per `tower.rs:114`, VERIFIED), a client holds an
  `Mcp-Session-Id` and, if configured, an open SSE stream for server-to-client messages. **A daemon restart
  now breaks an actual live connection** (SSE stream drops, session ID becomes invalid) in a way the old
  transport never had to handle, which is new failure-mode surface introduced specifically by this
  migration, not present in the current architecture. Whether the MCP client (Claude Code) reconnects and
  re-initializes transparently or surfaces an error to the user is a client-side behavior to verify in the
  plan/validate phase, not something this daemon controls.

## 4. `!Send`/concurrency footguns bridging `!Send` core to `Send` HTTP layer

- VERIFIED — `crates/cli/src/main.rs:25-42`: the whole daemon runs on
  `tokio::runtime::Builder::new_current_thread()` + a single `tokio::task::LocalSet`, with the doc comment
  explicitly stating "no `Send` bounds anywhere." State (`NativeBrowser`, `NativeEmbedder`, etc.) is held
  in `Rc<...>` (`main.rs:86-97`), which cannot cross an `await` point on a different task/thread without a
  compile error — this class of bug is caught at compile time, not a runtime footgun, confirming the
  requirements doc's own framing.
- VERIFIED — `rmcp-2.2.0/src/transport/streamable_http_server/session.rs:75-135`: `SessionManager`'s
  trait bound is `Send + Sync + 'static`, and every one of its trait methods returns `impl Future<...> +
  Send`. This is a hard compile-time requirement on whatever type serves as the session manager /
  `tower::Service` — it cannot itself hold an `Rc<RefCell<...>>` or otherwise be `!Send`. This corroborates
  the requirements doc's architectural finding directly against the SDK source, not just the earlier
  `tower.rs:571-577` `Service` impl bound already cited there.
- UNVERIFIED/INFERRED (general Rust knowledge, not from a specific search result) — the subtler risk once
  a channel-based bridge is chosen (e.g. `Send` HTTP tasks on a multi-thread runtime send requests over an
  `mpsc`/`oneshot` channel into the single `!Send` `LocalSet` task, which processes them serially and sends
  responses back) is **not the channel deadlocking under normal load** — Tokio's `mpsc`/`oneshot` are
  deadlock-safe by construction as long as nothing awaits its own response synchronously from inside the
  handler that would need to process it. The actual known failure patterns in this shape of bridge are:
  - **Bounded-channel backpressure indistinguishable from a hang**: if the bridge channel from HTTP tasks
    into the `LocalSet` is bounded (reasonable, to cap memory), a burst of concurrent HTTP requests (e.g.
    several subagents calling tools simultaneously — exactly the concurrency scenario the Success Metrics
    require testing) will block new HTTP tasks on `channel.send().await` once the buffer fills, and every
    request queues behind whatever's currently running in the single-threaded core (e.g. a slow `browser`
    tool call). This is *correct* behavior, not a bug, but externally indistinguishable from a stuck
    server unless it's specifically documented/logged — worth an explicit "queue depth" or "request
    waiting on daemon core" log line per the Observability Requirements, since a bare HTTP client only
    sees a slow response with no indication why.
  - **A stateful browser `Page` handle used across the bridge boundary incorrectly**: today, a single Unix
    socket connection = a single sequential dispatch into the `!Send` core per `client::call` round-trip
    (VERIFIED `crates/core/src/client.rs:23-48` — one dial, one frame write, one frame read, no persistent
    connection). Concurrent thin clients today already share the daemon core serially via the `LocalSet`'s
    single-threaded scheduling — this isn't new. What *is* new is that Streamable HTTP's `stateful_mode`
    could tempt a design where a `Page`/browser-session handle gets cached keyed by `Mcp-Session-Id` on the
    `Send` HTTP side (e.g. in the `SessionManager` implementation) for a perceived performance win, bypassing
    the channel hand-off into the `!Send` core for "fast" operations. Since `NativeBrowser`'s `Page` handles
    are `!Send` `Rc`-based objects (VERIFIED — `NativeBrowser` itself is `Rc`-wrapped in `main.rs:88-89`;
    not independently confirmed whether individual `Page` handles inside it are `!Send`, but they're
    constructed from the same `!Send`-by-design core), any such shortcut would not compile if attempted
    directly — but could still be attempted indirectly (e.g. an `unsafe impl Send` wrapper "because it's
    fine, single daemon" to silence the compiler) which would reintroduce exactly the thread-affinity bug
    class the `!Send` design was built to prevent at compile time. **Plan-phase guidance: treat any
    `unsafe impl Send`/`unsafe impl Sync` appearing anywhere in this migration's diff as an automatic design
    review trigger**, not a routine unsafe-block review — it would be undoing the one compile-time guarantee
    this architecture currently has.

## 5. General Streamable HTTP transport pitfalls

- VERIFIED — `rmcp-2.2.0/src/transport/streamable_http_server/tower.rs:109-120` (`StreamableHttpServerConfig::default()`):
  `sse_keep_alive: Some(Duration::from_secs(15))`, `sse_retry: Some(Duration::from_secs(3))`,
  `stateful_mode: true`, `json_response: false`. For this project's single-user/handful-of-sessions
  scale target (per the requirements doc's Non-functional Requirements), the defaults are almost
  certainly fine as-is; the two tradeoffs worth an explicit plan-phase decision rather than silently
  inheriting the default:
  - `stateful_mode: true` + default `json_response: false` means **every** tool call response goes over
    SSE framing even for simple request/response tools with no server-initiated messages, per the doc
    comment on `json_response` (`tower.rs:50-54`, VERIFIED) which states `json_response: true` "eliminates
    SSE framing overhead for simple request-response tools." Given 23 tools, most of which are
    straightforward request/response (not the kind that need server-push), `json_response: true` may be
    the better default for this daemon specifically — worth benchmarking against the Non-functional
    Requirement of "should not regress the current per-call latency" rather than assuming SSE's overhead
    is negligible at this call volume.
  - `stateful_mode: false` (stateless) would sidestep both the session-restart-breaks-live-connection
    issue (Pitfall 3) and the session-table-leak CVE class (Pitfall 1) entirely, at the cost of losing
    server-initiated messages/resumability — likely an acceptable trade for a tool-call-dominated MCP
    server with no current use of server-push features. **This deserves a first-class decision in the plan
    phase, not a default-inherited one**, since it changes the shape of several other risks in this
    document simultaneously.
- UNVERIFIED/INFERRED (web search) — "reconnection storm" is a named risk in Streamable HTTP client
  implementations generally (automatic SSE reconnect-with-`Last-Event-ID` on drop) but requires an
  `event_store`/`session_store` configured for replay to matter; `StreamableHttpServerConfig::session_store`
  defaults to `None` (VERIFIED, `tower.rs:100,120`), so out of the box there's no replay-storm amplification
  risk — a dropped session just fails to resume and the client must re-`initialize`. Only becomes relevant
  if a future iteration adds a `SessionStore` for cross-instance recovery, which is explicitly Out of Scope
  here (single local daemon, no multi-instance story).

## Summary of plan-phase action items surfaced by this research

1. Pin `rmcp` to `>=2.2` explicitly (not a loose `"2"` range) and add dependency-vulnerability scanning
   (`cargo audit`/`cargo deny`) to CI, given two prior CVEs in this exact transport code path.
2. Decide `stateful_mode`/`json_response` deliberately (leaning stateless + `json_response: true` given
   this daemon's tool-call-dominated, no-server-push usage pattern) rather than inheriting SDK defaults
   silently — this changes the shape of several other risks (session-restart breakage, session-table
   exposure) at once.
3. Bearer-token file must get the same explicit `0o600` treatment `lock.rs` already gives the lock file;
   `daemon.log` currently has no explicit mode and all logging today is unredacted `eprintln!` — mandate a
   redaction discipline (or a structured logger with a deny-list) before any HTTP-layer request logging
   is added.
4. Verify whether Claude Code's `${ENV_VAR}` substitution in `http`-transport `headers` actually works
   (reported broken in [#51581](https://github.com/anthropics/claude-code/issues/51581)/[#6204](https://github.com/anthropics/claude-code/issues/6204)) before designing around it — `mcp-servers.json` is
   git-tracked today and a literal token in a `headers` block would be a real, not hypothetical, leak
   into the `dotfiles` repo's history and its `llm-sync` mirror.
5. Bound the systemd/launchd unit's restart behavior (`StartLimitBurst`/`ThrottleInterval`) rather than
   unconditional restart-on-failure, and log bind-failure distinctly, since the existing `flock`-based
   lock only prevents two daemons at once — it does not prevent a stale port holder from causing a
   crash loop.
6. Treat any `unsafe impl Send`/`Sync` appearing in the diff as an automatic design-review trigger — it
   would silently undo the compile-time thread-affinity guarantee the current `!Send` architecture
   provides today.
