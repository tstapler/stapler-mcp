# ADR-0001: Opt-in via `STAPLER_MCP_BROWSER_PROFILE_DIR`, literal path required, no computed default

**Status**: Accepted
**Date**: 2026-09-23
**Deciders**: Tyler Stapler (solo project)
**Related**: `project_plans/browser-profile-persistence/requirements.md`, `research/stack.md`,
`research/architecture.md` §1, `research/ux.md` §1, `research/pitfalls.md` §3–4,
`research/build-vs-buy.md`, `plan.md` Step 0.5 and Pattern Decisions

## Context

`NativeBrowser::launch()` (`crates/native/src/browser.rs:337-377`) builds exactly one
`chromiumoxide::BrowserConfig` per daemon process and passes it a `user_data_dir` —
currently always a pid+timestamp-scoped temp directory under `$TMPDIR`, torn down
(well, leaked — separate finding) on every restart. The item asks for an opt-in way to
point that directory at a stable location that survives a daemon restart, without
changing default (ephemeral) behavior.

Two sub-questions needed resolving, both flagged as open tensions across the research:

1. **Where does the opt-in surface live?** `research/ux.md` and `research/architecture.md`
   independently land on a daemon-startup env var (no `clap`/arg-parser exists in this
   repo; a per-call MCP tool parameter would silently no-op on every `navigate()` after
   the daemon's first, since `user_data_dir` is consumed once at `Browser::launch()`,
   before any session exists).
2. **What does the env var's value mean, and does the feature need a shipped default
   location?** `research/ux.md` §1 suggested the var *override* a computed default under
   `~/.stapler-mcp/browser-profile` (mirroring `paths.rs`'s `base_dir()`-relative
   convention). `research/pitfalls.md` §4 separately argues that whatever default we pick
   must be a proper XDG state dir, not `~/.stapler-mcp/` or a git/sync-folder path, because
   a persistent profile is a standing, near-plaintext-at-rest credential store (§3) and an
   LLM-agent-driven daemon turns any prompt-injection-triggered file read into a standing
   leak, not a one-session one. These two recommendations partially disagree on *where*
   the default should live.

## Decision

`STAPLER_MCP_BROWSER_PROFILE_DIR` (env var, `STAPLER_MCP_*`-prefixed to match
`STAPLER_MCP_HOME`/`STAPLER_MCP_ALLOW_PRIVATE_NETWORKS`, read once in `run_daemon()`
via `EnvPort`, never inside `browser.rs` itself — keeps `browser.rs` decoupled from
`std::env`, per `research/architecture.md` §1). When set to a non-empty value, that
value is used **literally** as the persistent `user_data_dir` (create it, `chmod 0700`
on Unix, pass straight to `BrowserConfig::builder().user_data_dir(...)`). When unset or
empty, behavior is byte-identical to today: a fresh pid+timestamp temp dir.

**No computed default location is introduced.** `paths::browser_profile_dir(&env)`
returns `Option<String>` — `None` when the operator hasn't opted in, `Some(literal_value)`
when they have. There is no third state where the var is "on" but the path is
auto-derived.

## Rationale

- **Dissolves the ux.md-vs-pitfalls.md tension rather than picking a side.** Both
  research files' disagreement was about *where a shipped default should point*. Requiring
  the operator to supply the literal path removes the need to ship a default at all — the
  XDG-vs-`~/.stapler-mcp` debate becomes the operator's own choice, informed by a README
  warning (Story 1.4.1) steering them away from git repos and sync-folder roots
  (`research/pitfalls.md` §4), rather than this codebase silently picking a location that
  becomes a standing credential store by default.
- **Matches the closest prior-art precedent for this item's framing.** `research/features.md`
  contrasts Playwright MCP (persistent-by-default, opt-*out*) against Anchor Browser
  (explicit, named, opt-*in* profile, decoupled from any one session) and concludes Anchor
  Browser's shape matches this item's stated framing (ephemeral default, explicit opt-in)
  better. An explicit literal path is the natural expression of "explicit, named."
- **Mirrors an existing pattern in this exact file.** `paths::base_dir()`
  (`crates/core/src/paths.rs:9-17`) already does "env var literal-value override, else a
  computed default" for `STAPLER_MCP_HOME`. This feature is deliberately *not* that
  pattern (no computed default branch) — the difference is intentional, not an
  inconsistency: `base_dir()`'s default (`~/.stapler-mcp`) is safe to compute because it's
  not a credential store; a browser profile directory is, so it earns the extra "operator
  must decide" step the general HOME-dir override doesn't need.
- **Zero new dependencies.** No `dirs`/`directories` crate, no XDG-resolution helper code —
  `research/build-vs-buy.md` already flags `dirs` as YAGNI-adjacent for one call site, and
  removing the "compute a default" requirement removes the only reason that crate was even
  discussed.

## Consequences

- **Positive**: default (unset) behavior is provably unchanged — the `None` branch in
  `NativeBrowser::launch` is the exact code path that exists today, untouched.
- **Positive, revised during plan review**: a cheap runtime check (ancestor `.git`
  directory or a common sync-folder path segment — `Dropbox`, `iCloud Drive`, `Syncthing`)
  now backs the README warning with a loud, non-blocking `eprintln!` at daemon startup
  (Task 1.2.1c), rather than relying on the README alone — the adversarial review of this
  plan judged a documentation-only mitigation too weak given this directory is a
  near-plaintext-at-rest credential store reachable via prompt injection
  (`research/pitfalls.md` §3). This is still deliberately a **warning, not a hard reject**:
  the heuristic can false-positive (e.g. a dotfiles-managed home directory under git), and
  a single-user personal-tool daemon should not block an operator's own explicit opt-in on
  it. The maintenance cost is small — one pure ancestry-walk helper function, unit tested,
  no new dependency.
- **Negative**: an operator who wants persistence has to type a full path rather than a
  single boolean-ish flag (`STAPLER_MCP_BROWSER_PROFILE_DIR=1`). Accepted: the README
  callout gives a copy-pasteable example using a **literal absolute path** (e.g.
  `/home/alice/.stapler-mcp/browser-profile`, with a note to substitute the operator's own
  home directory) rather than `~/.stapler-mcp/browser-profile` — `~` is only expanded by an
  interactive shell, not by the JSON `env` block most MCP clients actually use to set this
  var, so a literal `~`-prefixed value copied from the doc would silently fail
  `browser_profile_dir`'s absolute-path check and fall back to ephemeral mode
  (pre-mortem #1). As defense in depth, `browser_profile_dir` itself now also expands a
  leading `~/`/bare `~` via `EnvPort::home_dir()` before that check (Task 1.1.1a), so the
  shorthand still works if an operator types it anyway despite the doc no longer leading
  with it. The extra step of typing a full path is still a one-time daemon-config action,
  not a per-call cost.
- **Negative**: two daemons with different `STAPLER_MCP_HOME`s pointed at the same
  explicit `STAPLER_MCP_BROWSER_PROFILE_DIR` are not caught by the existing per-`base_dir`
  flock (`NativeLock`) — they collide on Chrome's own `SingletonLock` instead, surfaced as a
  daemon-startup error (Story 1.2.1). Documented, not coded around, per
  `research/architecture.md` §4's own conclusion that this needs a doc note, not new
  locking logic.

## Alternatives Considered

| Alternative | Rejected because |
|---|---|
| MCP tool parameter on `stapler_browser_navigate` (the item's literal original proposal) | `user_data_dir` is consumed once at `Browser::launch()`, before any session/tab exists (`browser.rs:354-360`); a tool param would silently no-op on every call after a daemon's first `navigate`, including every later MCP session sharing the same long-lived daemon — the worst kind of silent failure for an LLM agent (`research/ux.md` §2) |
| New CLI flag (e.g. `--browser-profile-dir <path>`) | No `clap`/arg-parser exists anywhere in `crates/cli/src/main.rs` (only a bare `args().any(|a| a == "--daemon")` check); adding one for a single flag is disproportionate next to the existing all-env-var config convention |
| Env var as a boolean toggle + computed default under `~/.stapler-mcp/browser-profile` | Reopens the exact XDG-vs-`~/.stapler-mcp` hazard debate the research flagged without resolving it; makes this codebase responsible for picking a "safe enough" default location for what pitfalls.md identifies as a near-plaintext-at-rest credential store |
| Env var as a boolean toggle + computed default under an XDG state dir (`$XDG_STATE_HOME`) | Same objection as above, plus introduces a second app-config-dir resolution convention alongside `paths.rs`'s existing `EnvPort`-based one (`research/stack.md` §3 explicitly recommends against adding `dirs`/`directories` as a direct dependency to avoid this) |
