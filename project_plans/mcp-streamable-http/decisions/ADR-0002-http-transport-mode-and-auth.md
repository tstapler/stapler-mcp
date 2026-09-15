# ADR-0002: Stateless Streamable HTTP mode, hand-rolled bearer-auth middleware, opt-in listener, file-backed token

**Status**: Accepted
**Date**: 2026-09-14
**Deciders**: Tyler Stapler (solo project)
**Related**: `requirements.md` §"Security classification"/§"Open Questions", `research/pitfalls.md` §1-2,5,
`research/stack.md` §5, `research/ux.md` §3, ADR-0001, `implementation/plan.md` Phases 3-4

## Context

Once ADR-0001's channel bridge makes it possible to host MCP over Streamable HTTP at all, four further
decisions were left open by `requirements.md` and flagged by research as needing a first-class,
plan-phase answer rather than a silently inherited default:

1. **`StreamableHttpServerConfig`'s `stateful_mode`/`json_response` fields.** `rmcp`'s defaults are
   `stateful_mode: true`, `json_response: false` (`rmcp-2.2.0/src/transport/streamable_http_server/tower.rs:109-120`).
   Under `stateful_mode: true`, a client holds a live `Mcp-Session-Id` and (with `json_response: false`)
   every tool response — even a plain request/response call — goes over SSE framing. `research/pitfalls.md`
   §5 flagged this as a first-class decision: stateful mode means a daemon restart drops a live client
   connection in a way today's per-call Unix-socket dial never has to handle, and it's the same code path
   as an already-patched session-table-leak DoS (`GHSA-9pj6-vhgr-3mwh`, fixed 2.0.0, present in this
   codebase's history as a live vulnerability class in this exact transport).
2. **How to enforce bearer-token auth.** `requirements.md` treats this as a hard ship-blocker: a Unix
   socket's filesystem permissions restrict access to the daemon's owning user for free; a TCP port on
   `127.0.0.1` does not (`research/pitfalls.md` §1 — `allowed_hosts` defaults to loopback-only, which
   stops DNS rebinding, but `allowed_origins` defaults to empty/disabled, which does **not** stop a
   malicious webpage's direct `fetch()` to the literal loopback URL).
3. **Whether the HTTP listener is always-on or opt-in**, and what a fixed vs. configurable port implies
   for `mcp-servers.json`.
4. **Where the bearer token lives and how it reaches `mcp-servers.json`** without landing in that file's
   git history — `research/pitfalls.md` §2 confirmed `mcp-servers.json` (in the sibling `dotfiles` repo)
   is git-tracked and `llm-sync`-mirrored, and that Claude Code's `${ENV_VAR}` substitution in `http`
   transport `headers` is reported broken in unresolved upstream issues, closing off the obvious
   env-var-indirection mitigation.

## Decision

1. **`stateful_mode: false`, `json_response: true`.** Every one of this daemon's 27 tools is a plain
   request/response call with no server-initiated push in use — SSE framing buys nothing here. Going
   stateless also removes the `Mcp-Session-Id` lifecycle entirely: no session-restart-breaks-live-connection
   failure mode to design around, and no session-table bookkeeping in the same shape as the already-patched
   DoS. The "≥2 concurrent HTTP sessions against a stateful tool" Success Metric is satisfied at the
   `ports::SessionId` (application-level browser-tab handle) layer instead — which already works across
   independent connections today on the Unix-socket path — not at the `Mcp-Session-Id` (MCP-protocol)
   layer, which this decision makes moot.
2. **A hand-rolled `axum::middleware::from_fn_with_state` bearer-check**, not `tower-http`'s
   `ValidateRequestHeaderLayer::bearer`. Verified, not assumed: `ValidateRequestHeaderLayer::bearer`
   requires the wrapped service's response body type to implement `Default`
   (`tower-http-0.6.11/src/auth/require_authorization.rs:108-129`), and `StreamableHttpService`'s own
   response type, `BoxResponse = Response<BoxBody<Bytes, Infallible>>`, does not implement `Default`
   (`research/stack.md` §5) — so the idiomatic `tower-http` layer cannot wrap `StreamableHttpService`
   directly; it has to sit at the `axum::Router` level instead, and even there it has no hook for the
   distinct rejected-request log line `requirements.md`'s Observability Requirements demand. A ~20-line
   custom middleware avoids both problems and drops a dependency (`tower-http`) this project would
   otherwise add solely for one call.
3. **Opt-in via `STAPLER_MCP_HTTP_PORT`** (unset = HTTP disabled, unmodified stdio-only behavior; set =
   HTTP listener bound to that port). No feature-flag system exists in this codebase; this env var is
   the closest equivalent and doubles as port configuration, resolving the "fixed vs. configurable port"
   open question in favor of configurable-but-must-be-explicit.
4. **Token stored at `~/.stapler-mcp/http-token`, mode `0600`**, generated once via a CSPRNG on first
   HTTP-enabled daemon start (mirroring `crates/native/src/lock.rs:33-39`'s existing `.mode(0o600)`
   pattern for `daemon.lock`). Distribution to `mcp-servers.json` is a **documented manual step**
   (`stapler-mcp --print-config` prints the ready-to-paste JSON block, per the Jupyter
   token-on-first-run precedent `research/ux.md` §3 cites) into a machine-local, **not git-committed**
   config — never a literal token in the tracked `mcp-servers.json`, and no reliance on `${ENV_VAR}`
   substitution given its reported breakage.

## Rationale

- **Stateless mode directly neutralizes two of the three concrete risk classes `research/pitfalls.md`
  raised** (session-restart-breaks-live-connection; session-table-leak DoS shape) rather than mitigating
  them after the fact, at zero functional cost for a tool-call-dominated, no-server-push workload.
- **The hand-rolled middleware isn't just avoiding a dependency for its own sake** — it's avoiding a
  verified type-level incompatibility (`BoxBody: Default`) that would otherwise force an awkward
  workaround, while simultaneously satisfying a requirement (distinct rejection logging) `tower-http`'s
  layer has no extension point for.
- **Opt-in-by-env-var costs nothing for machines that haven't adopted HTTP yet** — the stdio path is
  completely unmodified code, satisfying `requirements.md`'s "must not silently drop the existing
  stdio... path" constraint by construction, not by policy.
- **File-backed token with a manual print-and-paste step is proportionate to a single-user local tool.**
  This codebase has no existing secret-manager integration (`research/ux.md` §3: no 1Password/keychain
  usage found anywhere in this tree — that tooling lives in the sibling `dotfiles` repo's `secrets`
  Ansible role, a machine-bootstrap concern, not something `stapler-mcp` itself touches today). Building
  a 1Password/keychain integration for this would be new scope disproportionate to the item's Appetite.

## Consequences

- **Positive**: no `SessionStore`/resumability code path to build, test, or reason about — `rmcp`'s
  `session_store` stays `None` (its own default), which also means no cross-instance-recovery
  replay-storm risk (`research/pitfalls.md` §5) ever becomes relevant.
- **Positive**: `--status`'s HTTP reachability check (`implementation/plan.md` Story 7.1.1) can be a bare
  TCP connect probe with no token required, since there's no persistent session to authenticate into for
  a liveness check — simpler UX, and doesn't require the *status* command itself to read the secret file.
- **Negative, accepted**: losing `stateful_mode`'s SSE-based server-initiated messages means this daemon
  cannot push unsolicited notifications to an HTTP client in the future without revisiting this decision.
  Accepted because no tool in this daemon currently needs server-push, and revisiting a config field
  later is cheap relative to building around a session model this daemon doesn't need today.
- **Negative, accepted**: the manual `--print-config`-then-paste step means onboarding a new machine is
  one extra manual step compared to a hypothetical fully-automated secret distribution. Accepted per
  Rationale above — the alternative (an env-var-substitution or keychain-integration solution) either
  doesn't work (per the linked Claude Code bugs) or is disproportionate new scope.
- **Negative, accepted**: a daemon operator who *wants* HTTP always-on (e.g. relying on a systemd unit
  they never think about again) must remember to set `STAPLER_MCP_HTTP_PORT` in that unit's
  `Environment=` line (`implementation/plan.md` Task 5.2.1a already does this in the shipped template) —
  it is not automatically inferred from "a launcher unit exists."

## Alternatives Considered

| Alternative | Rejected because |
|---|---|
| Default `stateful_mode: true` + SSE-framed `json_response: false` (inherit `rmcp`'s defaults) | Silently inherits both the session-restart-breaks-live-connection failure mode and the session-table-leak CVE shape for a workload that gets no benefit from either — `research/pitfalls.md` §5 explicitly calls out that inheriting these defaults "changes the shape of several other risks... simultaneously," and this project's own tools are all request/response, not server-push. |
| `tower-http`'s `ValidateRequestHeaderLayer::bearer` | Verified type incompatibility (`BoxBody: Default` required, not implemented by `StreamableHttpService`'s response type) plus no hook for the required distinct-rejection log line — see Decision §2. |
| Always-on HTTP listener bound to a fixed default port, no opt-in gate | No feature-flag system exists in this repo to fall back on if something goes wrong (`requirements.md` Risk Control), and it would silently open a new local network attack surface on every machine running this daemon, including ones that haven't set up a bearer-token-aware client config yet. |
| Literal bearer token committed into `mcp-servers.json`'s `headers` block, or relying on Claude Code's `${ENV_VAR}` substitution in that block | `mcp-servers.json` is git-tracked and `llm-sync`-mirrored to other tools (`research/pitfalls.md` §2) — a literal token would be a real secret leaked into commit history; `${ENV_VAR}` substitution in the `http` transport's `headers` map is reported broken in linked, unresolved Claude Code issues, so it cannot be relied on as the safe alternative. |
| OS keychain / 1Password custody of the token, via the `secrets` Ansible role already used elsewhere in Tyler's toolchain | No existing integration point inside `stapler-mcp` itself (the `op` CLI/1Password tooling lives in the sibling `dotfiles` repo, a separate machine-bootstrap concern) — legitimate future enhancement, but new integration scope disproportionate to this item's Appetite; a `0600` file mirrors an existing in-repo pattern (`daemon.lock`) instead of introducing a new one. |
