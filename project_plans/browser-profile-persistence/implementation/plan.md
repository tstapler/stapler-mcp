# Implementation Plan: Persistent browser profile / `user_data_dir` reuse across daemon restarts

**Feature**: Opt-in, daemon-startup-only env var (`STAPLER_MCP_BROWSER_PROFILE_DIR`) that
points `NativeBrowser::launch()` at a durable `user_data_dir` surviving daemon restarts,
with default (ephemeral, pid+timestamp-scoped) behavior unchanged when unset.
**Date**: 2026-09-23
**Status**: Ready for implementation
**ADRs**: `project_plans/browser-profile-persistence/decisions/ADR-0001-env-var-literal-path-opt-in.md`

**Scheduling note**: this plan is a triage deliverable for a `later`-priority backlog item
(see `requirements.md`'s Priority section — `later`, as labeled in the source item). It is
fully specified and ready to implement *when scheduled*, not a recommendation to implement
immediately. The thoroughness below (full research, ADR, pre-mortem, task-level detail) is
not a claim about this item's urgency — it reflects that planning depth is decoupled from
implementation priority in this repo's SDD process: agent-driven planning is comparatively
cheap, so a `later`-priority item still gets a complete, ready-to-pick-up plan rather than a
shallow placeholder, and the cost of that thoroughness is paid once, now, not deferred to
whoever eventually schedules the work.

---

## Step 0.5 — Creative pass (alternatives explored)

1. **MCP tool parameter on `stapler_browser_navigate`** (the item's literal original
   proposal). *Strength*: matches the item's own wording exactly, zero daemon-config
   surface to document. *Weakness*: `user_data_dir` is consumed once at
   `Browser::launch()` (`crates/native/src/browser.rs:354-360`), before any session
   exists — a tool param would silently no-op on every call after a daemon's first
   `navigate`, which `research/ux.md` §2 calls "the worst kind of silent failure for an
   LLM agent." **Rejected.**
2. **Daemon-startup env var, literal path, no computed default** (chosen). *Strength*:
   fits the existing `STAPLER_MCP_HOME`/`BRAVE_API_KEY` env-var-config convention exactly
   (no new parsing layer, no new dependency) and sidesteps the XDG-vs-`~/.stapler-mcp`
   default-location debate by not shipping a default at all. *Weakness*: opting in takes
   one extra step (typing a full path) versus a bare boolean flag. **Chosen** — see
   ADR-0001 for the full trade-off.
3. **Daemon-startup env var as a boolean toggle, with a computed default path** (e.g.
   `STAPLER_MCP_BROWSER_PROFILE=1` → always `~/.stapler-mcp/browser-profile`, or an XDG
   state dir). *Strength*: friendliest one-line opt-in for an operator, closest to
   Playwright MCP's own UX. *Weakness*: makes this codebase responsible for picking a
   "safe enough" default location for what `research/pitfalls.md` §3 identifies as a
   near-plaintext-at-rest credential store on Linux without a keyring — the exact tension
   between `research/ux.md` §1 and `research/pitfalls.md` §4 that ADR-0001 exists to
   resolve. **Rejected** — see ADR-0001's Alternatives Considered table.

Rejected alternatives are also recorded in the Pattern Decisions table below.

---

## Domain Glossary

| Term | Definition | Notes |
|------|-----------|-------|
| `STAPLER_MCP_BROWSER_PROFILE_DIR` | Opt-in environment variable read once by `run_daemon()` (`crates/cli/src/main.rs`) via `EnvPort`. Non-empty value → literal path used as a durable `user_data_dir`. Unset/empty → ephemeral (unchanged) behavior. | `STAPLER_MCP_*`-prefixed, matching `STAPLER_MCP_HOME`/`STAPLER_MCP_ALLOW_PRIVATE_NETWORKS` |
| `browser_profile_dir` | `paths::browser_profile_dir<E: EnvPort>(env: &E) -> Option<String>` — resolves `STAPLER_MCP_BROWSER_PROFILE_DIR` into `Some(path)` or `None`. New function in `crates/core/src/paths.rs`, sibling to `docs_index_dir`/`embedding_cache_dir`. | Returns `Option`, not always-populated `String` like its siblings — no computed default (see ADR-0001). Expands a leading `~/`/bare `~` via `EnvPort::home_dir()` before the absolute-path check (defense in depth for pre-mortem #1 — a JSON `env` config block never shell-expands `~`); falls back to the existing relative-path rejection, not a panic, if `home_dir()` returns `None` |
| `status_extra` / `browserProfileMode` / `browserProfileWarning` | `Daemon::set_status_extra(&self, extra: serde_json::Value)` (`crates/core/src/daemon.rs`) stores JSON merged into every `ping` response's `result` object. `run_daemon()` calls it once at startup with `browserProfileMode` (`"ephemeral"` or `"persistent at {path}"`) and `browserProfileWarning` (`null` or the git/sync-folder hazard message) — camelCase wire keys, matching every other tool's response and `DaemonStatusOutput`'s `#[serde(rename_all = "camelCase")]` (see Task 1.2.3e). | New diagnostic surface addressing pre-mortem #2/#3 — see Story 1.2.3. Set once before `Daemon::run()`'s accept loop starts; unused by the wasm adapter (`crates/wasm/src/lib.rs`), which never calls it, so `ping` there is unchanged |
| `stapler_daemon_status` | New MCP tool registered on `ThinClient` (`crates/cli/src/thin_client.rs`), backed by `call_daemon(PING_TOOL, DaemonStatusInput {})` — the exact same socket call every other tool (`stapler_browser_navigate`, etc.) already uses. Gives an MCP client (including the LLM agent driving this daemon) an actual, listed tool to call to read `browserProfileMode`/`browserProfileWarning`. Native-only — see Unresolved Questions for why this isn't mirrored in the wasm/npm distribution's `list_tools_json()`. | New in Story 1.2.4, closing the UX-review finding that `ping`'s extra fields (Story 1.2.3) were previously reachable only from Rust test code, not from any MCP client or operator |
| `--status` | New bare CLI flag on the native `stapler-mcp` binary (`crates/cli/src/main.rs`, parsed the same way as the existing `--daemon` flag — `args().any(...)`, no new arg-parser). Connects to the daemon's existing socket, calls `PING_TOOL` via `client::call`, and prints the resolved `browserProfileMode`/`browserProfileWarning` to stdout for a human operator. Does not auto-spawn the daemon (diagnostic-only, mirrors `--daemon`'s own "not run by hand" framing) — prints "not reachable" and exits non-zero if no daemon is running. | New in Story 1.2.4, the operator-facing half of the same reachability fix |
| `persistent_profile_dir` | The `Option<PathBuf>` parameter added to `NativeBrowser::launch(persistent_profile_dir: Option<PathBuf>)` (`crates/native/src/browser.rs`), threaded in from `run_daemon()`. | Replaces the current zero-arg `launch()` signature |
| `user_data_dir` | Pre-existing Chrome/chromiumoxide launch-time directory holding one Chrome profile (cookies, local storage, login state, `SingletonLock`). Either the existing ephemeral `$TMPDIR/stapler-mcp-chromium-<pid>-<now_millis>` path (unchanged) or the resolved `persistent_profile_dir` (new). | `chromiumoxide::BrowserConfigBuilder::user_data_dir` |
| `SingletonLock` | Chrome's own process-singleton lock file written inside `user_data_dir` at launch. A second `Browser::launch` against an already-live `user_data_dir` fails on this lock. | Pre-existing Chrome behavior; newly *reachable* once a named dir is reused across daemon instances |
| `NativeBrowser::launch` | Existing sole constructor and sole `BrowserConfig`/`user_data_dir` call site (`crates/native/src/browser.rs:338`). This plan changes its signature, not its role. | Isolate via seam per Tech Debt Disposition below (two extracted helper functions) |

---

## Pattern Decisions

| Component | Pattern Chosen | Source | Alternative Rejected | Reason |
|-----------|---------------|--------|---------------------|--------|
| `paths::browser_profile_dir` | Free function, same convention as `docs_index_dir`/`embedding_cache_dir` (Transaction Script-style pure function, PoEAA) | Fowler (PoEAA) | A `BrowserProfileConfig` struct/service type | Every sibling in `paths.rs` is a stateless pure function over `EnvPort`; a config object is unneeded ceremony for one `Option<String>` |
| Ephemeral vs. persistent representation | `Option<PathBuf>` (standard-library sum type) | type-driven-design | Custom `enum ProfileMode { Ephemeral, Persistent(PathBuf) }` | Exactly one call site, one branch point; a bespoke enum for a single use is speculative generality `research/architecture.md` §2 explicitly warns against elsewhere in this same item |
| Opt-in surface | Env var, literal path required, no computed default | ADR-0001 | (a) MCP tool parameter, (b) new CLI flag/parser, (c) boolean-toggle env var + computed default location | (a) silently no-ops after a daemon's first `navigate` call; (b) no `clap`/arg-parser exists anywhere in this repo; (c) reopens the XDG-vs-`~/.stapler-mcp` default-location debate the research flagged — see ADR-0001 |
| Error message on `Browser::launch` failure when `persistent_profile_dir` is `Some` | Lightweight message-wrapping (GoF Decorator in spirit, not a new type), realized as a small named helper function (`describe_launch_error`) rather than an inline closure, so it's independently unit-testable per the architecture review's remediation | GoF | New `PortError::ProfileLocked(String)` variant | `research/architecture.md` §4 and `research/pitfalls.md` §1 both conclude no new `PortError` variant is warranted; existing `PortError::Other` plus a conditional message prefix is proportionate |
| Directory permission hardening (`0700`) | Applied only on the `persistent_profile_dir` branch, via a small named helper function (`harden_persistent_dir_permissions`) rather than an inline `#[cfg(unix)]` block, so it's independently unit-testable | N/A — targeted mitigation | Apply `0700` to both ephemeral and persistent dirs | Ephemeral-dir permission behavior is pre-existing and out of this item's scope (relies on umask, unchanged); the persistent dir is the new standing credential store this item is responsible for hardening (`research/pitfalls.md` §3) |
| Unsafe profile location detection (git repo / sync-folder ancestry) | Warn via `eprintln!` at daemon startup; non-blocking (does not refuse to launch) | ADR-0001 (updated) | Reject outright (refuse to launch, exit non-zero) | A single-user personal-tool daemon (`later` priority) should not block an operator's own explicit, deliberate opt-in on a heuristic that can false-positive (e.g. a legitimately dotfiles-managed home dir under git); a loud warning at every startup is cheap, visible, and preserves operator agency, matching this plan's existing fail-open/log style elsewhere (e.g. `browser_profile_dir`'s relative-path rejection still just warns-and-falls-back rather than crashing the daemon) |
| Diagnostic surface for active profile mode + unsafe-location warning (pre-mortem #2, #3) | Extend the existing `ping` RPC response (`PING_TOOL` handler, `crates/core/src/daemon.rs`) with two extra fields via a new `Daemon::set_status_extra` | Story 1.2.3 | (a) A new dedicated RPC tool (e.g. `browser_profile_status`); (b) append the warning to the first `stapler_browser_navigate` response after startup | `ping` is already the daemon's one existing health-check surface an operator would think to call when something seems wrong, and both symptoms (`daemon.log`-only warning, unverifiable env propagation) are startup-time, static-for-the-process-lifetime facts — a second RPC tool or a one-time mutation of an unrelated tool's response shape would be two different visibility mechanisms for one class of problem. `daemon.log`'s own warning (Task 1.2.1c) is itself only a startup-time signal, so `ping` echoing the same scope is not a regression in freshness. **Reachability addendum** (added after UX review of this plan): Story 1.2.3 alone only extends `ping`'s wire-level response — it does not, by itself, give an operator or an MCP client any way to actually send a `ping` request, since `PING_TOOL`/`SHUTDOWN_TOOL` are deliberately internal daemon-lifecycle verbs (`crates/core/src/client.rs`'s `ensure_daemon`), never registered as an MCP tool on `ThinClient` and never exposed via a CLI flag. Story 1.2.4 closes that gap by reusing this same `ping` call (verified: `Daemon::dispatch()` routes `PING_TOOL` through the identical `client::call` transport every registered tool already uses — the transport is not the reason it was unreachable, only the missing registration was), rather than introducing a second dedicated RPC tool at the wire level. |

---

## Tech Debt Disposition

| Area | Existing Issue | Disposition | Justification |
|------|----------------|--------------|----------------|
| `crates/native/src/browser.rs`, `NativeBrowser::launch()` | `research/architecture.md`'s Tech Debt Disposition section originally concluded this function ("short, ~40 lines, single-purpose") was fine to extend as-is; the architecture review of this plan revised that once three new pieces of logic (unsafe-location warning, `0700` permission hardening, SingletonLock-aware error wrapping) were added inline | **Isolate via seam** | Each new piece of logic is extracted into its own small, named, pure-where-possible helper function (`harden_persistent_dir_permissions`, `describe_launch_error`, plus an unsafe-location detector) called from `launch()`, rather than inlined as `#[cfg(unix)]` blocks and closures directly in the function body. This keeps `launch()` itself a thin orchestrator and makes each new behavior unit-testable in isolation (via `tempfile`-free temp-dir helpers and `std::fs::metadata(...).permissions().mode()`) without a real Chrome process — see Tasks 1.2.1c–1.2.1h |

No other hotspot or architecture violation in `research/architecture.md` is touched by this
feature (confirmed: `ports.rs`'s `BrowserDriver` trait is explicitly *not* touched per
`research/architecture.md` §2, and `MAX_OPEN_SESSIONS`/reaper/`blocked` handling are
explicitly orthogonal per §3).

---

## Observability Plan

- **Logs**: `run_daemon()` prints one `eprintln!` line at startup naming the active mode,
  matching the existing style of adjacent lines (`"stapler-mcp: failed to launch browser: {e}"`,
  `crates/cli/src/main.rs:91`):
  - Ephemeral: `stapler-mcp: browser profile: ephemeral (temp dir, does not survive daemon restart)`
  - Persistent: `stapler-mcp: browser profile: persistent at {path}`
  On a `SingletonLock`-driven launch failure with `persistent_profile_dir` set, the
  existing `eprintln!("stapler-mcp: failed to launch browser: {e}")` at `main.rs:91` now
  receives an `e` whose message is prefixed to name the profile dir explicitly (Task 1.2.1g).
  Additionally, when `persistent_profile_dir` is `Some` and its resolved path's ancestry
  contains a `.git` directory or a common sync-folder segment (`Dropbox`, `iCloud Drive`,
  `Syncthing`), `launch()` prints one more `eprintln!` line before proceeding (non-blocking):
  `stapler-mcp: warning: browser profile dir {path} looks like it's inside a git repository or
  synced folder — this directory holds near-plaintext browser credentials; consider moving it`
  (Task 1.2.1c).
- **On-demand diagnostic surface**: because `daemon.log` is written by a detached process
  with no controlling terminal and nothing prompts an operator to open it, both the active
  profile mode and the unsafe-location warning above are *also* echoed in every `ping` RPC
  response (`result.browserProfileMode`, `result.browserProfileWarning`) via
  `Daemon::set_status_extra` — Story 1.2.3, addressing pre-mortem #2 and #3. That response is
  reachable via the `stapler_daemon_status` MCP tool (LLM-agent audience, native-only) and the
  `stapler-mcp --status` CLI flag (human-operator audience) — Story 1.2.4.
- **Metrics**: none — this codebase has no metrics/telemetry crate anywhere in the
  workspace (verified: no `metrics`/`prometheus`/`tracing` dependency in any `Cargo.toml`);
  a single-user local daemon has no metrics pipeline to emit into. Not introducing one for
  this feature.
- **Alerts**: no new alerts required — no alerting pipeline exists for this local daemon.

## Risk Control

- **Feature flag**: not gated by a separate flag — the opt-in *is* the gate
  (`STAPLER_MCP_BROWSER_PROFILE_DIR` absent by default; default behavior unchanged).
- **Rollback procedure**: standard revert via PR close + revert commit. No runtime
  rollback path is needed beyond that: an operator who opted in and wants to stop can
  simply unset `STAPLER_MCP_BROWSER_PROFILE_DIR` and restart the daemon — no data
  migration, no stored state outside the profile directory itself (which is left in
  place, not deleted, matching this item's explicit out-of-scope note on cleanup).
- **Staged rollout**: full rollout on merge — single-user local daemon, no staged-rollout
  infrastructure exists or is warranted.

## Unresolved Questions

Not fully closed — four gaps are flagged explicitly rather than silently assumed:

- **Crash-path profile-dir reuse is untested.** Story 1.3.1's smoke test (Task 1.3.1a)
  only exercises the graceful-shutdown restart path (`"shutdown"` RPC, clean exit). It does
  not exercise an ungraceful daemon termination (`kill -9` / OOM / host crash) before a
  restart against the same `STAPLER_MCP_BROWSER_PROFILE_DIR`, which is the scenario
  `research/pitfalls.md` flags as a real edge case (stale `SingletonLock` after a crash,
  hostname-mismatch recovery in containers). This plan deliberately does not extend the
  test to cover it: Chromium's `SingletonLock` pid-liveness recovery is standard,
  long-shipped behavior (`research/pitfalls.md`'s own characterization), and this is a
  `later`-priority personal-tool feature where expanding the integration test's scope
  (spawning and forcibly killing a subprocess, then asserting on stale-lock recovery) is
  disproportionate to the risk. Accepted, not silently assumed.
- **The one integration test this plan does add (Story 1.3.1, and the new Story 1.3.2
  SingletonLock-collision test) is `#[ignore]`d, and this repo's CI never runs `--ignored`
  tests.** Both close the gap they cover — cross-restart cookie persistence, and the
  SingletonLock collision error message — only if a human runs them manually
  (`cargo test -p stapler-mcp --test browser_profile_persistence -- --ignored`, and the
  equivalent for Task 1.3.2a) against a real Chrome binary before relying on this feature
  in practice. Not overhauling this repo's ignored-test-running infrastructure — that's out
  of scope for this item — but noting the risk stays open until that manual run happens.
- **`stapler_daemon_status` (and `--status`) is native-only — deliberately not added to
  `crates/wasm/src/lib.rs`'s `list_tools_json()`.** That function is a second,
  hand-maintained MCP tool registry the README's "Two distributions, one core" section
  documents as kept in parity with `ThinClient`'s tool router; Story 1.2.4 does not touch
  it. This is an accepted scope exclusion, not an oversight: confirmed by reading
  `crates/wasm/src/lib.rs`'s own `run_daemon()`, the wasm adapter never calls
  `Daemon::set_status_extra` (Task 1.2.3c) — its `ping` response stays `{"pong": true}`
  forever, with no `browserProfileMode`/`browserProfileWarning` to report — because the
  entire browser-profile-persistence feature this status tool exists to diagnose is itself
  already wasm-excluded (`research/architecture.md` §2: "wasm target has no equivalent
  concept" — `crates/wasm/src/browser.rs` is thin JS glue running inside a host
  browser/JS context with no separate daemon process and no `user_data_dir` to configure;
  "the profile is just whatever cookie jar the host browser/context already has"). Adding
  `stapler_daemon_status` to `list_tools_json()` would register a tool that can only ever
  return null/absent fields for wasm — no diagnostic payload for its one purpose. If a
  future item gives wasm its own notion of daemon-wide status worth reporting, parity
  should be revisited then; today there is nothing for it to report.
- **A relative-path `STAPLER_MCP_BROWSER_PROFILE_DIR` rejection is indistinguishable from
  an unset one in both diagnostic surfaces.** Task 1.1.1a's `browser_profile_dir` prints the
  rejection reason to `stderr` (visible only in `daemon.log`, per this plan's existing
  fail-open/log style), but `browserProfileMode` in the `ping`/`stapler_daemon_status`
  response (Story 1.2.3/1.2.4) reports `"ephemeral"` either way — an operator who typoed a
  relative path sees the same status as one who never set the var at all, with no signal
  from the diagnostic surface this plan otherwise built specifically to avoid that class of
  silent ambiguity (pre-mortem #2). Not extending `status_extra` with a third
  rejected-value field: the fix-then-verify loop this plan already recommends
  (`stapler-mcp --status` / `stapler_daemon_status`, README Task 1.4.1a) still surfaces the
  rejection the moment an operator checks `daemon.log`, and adding a new field for one edge
  case is disproportionate scope for a `later`-priority item. Accepted, not silently
  assumed.
- **A `SingletonLock` collision on daemon startup is undiscoverable via either new
  diagnostic surface, by architectural necessity.** `Daemon::set_status_extra` (Task
  1.2.3e) only runs after `NativeBrowser::launch()` succeeds; when `launch()` fails because
  a second daemon already holds the configured `STAPLER_MCP_BROWSER_PROFILE_DIR`'s lock,
  `run_daemon()`'s existing (unchanged) control flow prints `describe_launch_error`'s
  SingletonLock-aware message to `stderr`/`daemon.log` and exits before any socket exists —
  so neither `stapler_daemon_status` nor `stapler-mcp --status` can query a daemon that
  never started; both report "not reachable," not the specific cause. `daemon.log` remains
  the only place this specific error is visible. Accepted: the alternative (persisting a
  last-launch-failure reason somewhere a *future* daemon or a standalone check could read
  without a live socket) is more machinery than this `later`-priority item's smallest-change
  framing warrants; not silently assumed.
- **This plan's own scope has grown considerably past requirements.md's four acceptance
  criteria** (four repair passes across adversarial/architecture/pre-mortem/triad review
  added ~14 tasks: permission hardening, unsafe-location detection, and a full
  operator/agent-reachable diagnostic surface with its own regression test). That growth is
  each individually justified by a specific, concrete review finding (cited inline at each
  addition) rather than speculative scope-padding, but the repo's own `pm-product-management`
  skill warns against "gold plating" a `later`-priority item, and this plan is long enough
  that the observation is worth naming rather than leaving implicit. Before implementation
  actually starts (whenever this item is picked up), a fresh skim to confirm every task here
  still earns its place — rather than treating "already planned" as sufficient justification
  on its own — is a reasonable gate to add, not a change to make now.

---

## Dependency Visualization

```
Epic 1.1 (paths.rs resolver)              Epic 1.2 (launch-time wiring)
┌─────────────────────────┐               ┌──────────────────────────────┐
│ Task 1.1.1a: implement   │               │ Task 1.2.1a: launch() signature│
│   browser_profile_dir()  │               │   + branch + create_dir_all   │
│   (rejects relative path)│               └──────────────┬────────────────┘
└────────────┬─────────────┘                              │
             │                              ┌──────────────▼────────────────┐
┌────────────▼─────────────┐               │ Task 1.2.1b: fix              │
│ Task 1.1.1b: unit tests   │               │   browser_session.rs:1003     │
│   (unset/empty/set/       │               │   call site to launch(None)   │
│   relative-path)          │               │   + grep preflight for other  │
└────────────┬─────────────┘               │   call sites (BLOCKER fix)    │
             │                              └──────────────┬────────────────┘
             │                                              │
             │                              ┌──────────────▼────────────────┐
             │                              │ Task 1.2.1c: unsafe-location   │
             │                              │   warn helper + call from      │
             │                              │   launch()                     │
             │                              └──────────────┬────────────────┘
             │                                              │
             │                              ┌──────────────▼────────────────┐
             │                              │ Task 1.2.1d: unit tests for    │
             │                              │   unsafe-location helper       │
             │                              └──────────────┬────────────────┘
             │                                              │
             │                              ┌──────────────▼────────────────┐
             │                              │ Task 1.2.1e: extract           │
             │                              │   harden_persistent_dir_       │
             │                              │   permissions() + call it      │
             │                              └──────────────┬────────────────┘
             │                                              │
             │                              ┌──────────────▼────────────────┐
             │                              │ Task 1.2.1f: unit tests for    │
             │                              │   harden_persistent_dir_       │
             │                              │   permissions()                │
             │                              └──────────────┬────────────────┘
             │                                              │
             │                              ┌──────────────▼────────────────┐
             │                              │ Task 1.2.1g: extract           │
             │                              │   describe_launch_error() +    │
             │                              │   call it in map_err           │
             │                              └──────────────┬────────────────┘
             │                                              │
             │                              ┌──────────────▼────────────────┐
             │                              │ Task 1.2.1h: unit tests for    │
             │                              │   describe_launch_error()      │
             │                              └──────────────┬────────────────┘
             │                                              │
             └──────────────────┬───────────────────────────┘
                                 │  (both feed main.rs wiring)
                    ┌────────────▼─────────────┐
                    │ Task 1.2.2a: run_daemon() │
                    │   reads env var, calls    │
                    │   launch(Some/None)       │
                    │   (no clone needed)       │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.2b: startup log  │
                    │   line (ephemeral/        │
                    │   persistent)              │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.3a: capture      │
                    │   unsafe-location warning │
                    │   on NativeBrowser        │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.3b: unit tests   │
                    │   for compute_unsafe_     │
                    │   profile_warning         │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.3c: Daemon       │
                    │   status_extra + merge    │
                    │   into ping response      │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.3d: unit tests   │
                    │   for merged ping response│
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.3e: wire mode +  │
                    │   warning into            │
                    │   status_extra in main.rs │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.3f: assert new   │
                    │   ping fields in the real │
                    │   daemon round-trip test  │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.4a: add          │
                    │   DaemonStatusInput/      │
                    │   Output to schema.rs     │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.4b: register     │
                    │   stapler_daemon_status   │
                    │   tool on ThinClient      │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.4c: add --status │
                    │   CLI flag to main.rs     │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.4d: assert       │
                    │   stapler_daemon_status   │
                    │   in tool_schema.rs       │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.4e: assert       │
                    │   --status output in the  │
                    │   real daemon round-trip  │
                    │   test                    │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.2.4f: live round-  │
                    │   trip stapler_daemon_    │
                    │   status through          │
                    │   DaemonStatusOutput      │
                    │   (case-mismatch guard)   │
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.3.1a: cross-restart│
                    │   cookie-persistence      │
                    │   smoke test (#[ignore])  │
                    │   [graceful-shutdown only,│
                    │   see Unresolved Questions]│
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.3.2a: SingletonLock│
                    │   collision smoke test    │
                    │   (#[ignore], real Chrome)│
                    └────────────┬─────────────┘
                                 │
                    ┌────────────▼─────────────┐
                    │ Task 1.4.1a: README docs  │
                    │   (env var + hazard note) │
                    └───────────────────────────┘
```

---

## Phase 1: Opt-in persistent browser profile

### Epic 1.1: Profile directory resolution
**Goal**: Give `run_daemon()` a single, testable way to learn the operator's opt-in
choice, following the exact pattern every other daemon state path in `paths.rs` uses.

#### Story 1.1.1: Add `paths::browser_profile_dir` opt-in resolver
**As a** daemon operator, **I want** an environment variable that tells the daemon to use
a durable profile directory, **so that** I don't have to re-authenticate on login-gated
sites after every daemon restart.
**Acceptance Criteria**:
- `browser_profile_dir(&env)` returns `None` when `STAPLER_MCP_BROWSER_PROFILE_DIR` is unset.
  - *Given* a `MockEnv` whose `var("STAPLER_MCP_BROWSER_PROFILE_DIR")` returns `None`,
    *When* `paths::browser_profile_dir(&env)` is called, *Then* it returns `None`.
- `browser_profile_dir(&env)` returns `None` when the var is set to an empty string
  (mirrors `base_dir`'s existing nonempty-check idiom at `paths.rs:10-13`).
  - *Given* a `MockEnv` whose `var("STAPLER_MCP_BROWSER_PROFILE_DIR")` returns `Some("")`,
    *When* `paths::browser_profile_dir(&env)` is called, *Then* it returns `None`.
- `browser_profile_dir(&env)` returns `Some(path)` verbatim when the var is set to a
  non-empty, absolute value.
  - *Given* a `MockEnv` whose `var("STAPLER_MCP_BROWSER_PROFILE_DIR")` returns
    `Some("/home/testuser/.stapler-mcp/browser-profile".to_string())`, *When*
    `paths::browser_profile_dir(&env)` is called, *Then* it returns
    `Some("/home/testuser/.stapler-mcp/browser-profile".to_string())`.
- `browser_profile_dir(&env)` returns `None` (and prints a warning) when the var is set to
  a non-empty but **relative** path, rather than silently passing it through to
  `create_dir_all` where it would land at an unpredictable location relative to the
  detached daemon's cwd.
  - *Given* a `MockEnv` whose `var("STAPLER_MCP_BROWSER_PROFILE_DIR")` returns
    `Some("relative/profile-dir".to_string())`, *When* `paths::browser_profile_dir(&env)`
    is called, *Then* it returns `None` and `stderr` contains a line naming the rejected
    value and that an absolute path is required.
- `browser_profile_dir(&env)` expands a leading `~/` (or a bare `~`) to the operator's home
  directory (via `EnvPort::home_dir()`) before applying the absolute-path check — defense in
  depth for pre-mortem #1, since a value pasted from the README into an MCP client's JSON
  `env` config block is never shell-expanded.
  - *Given* a `MockEnv` whose `var("STAPLER_MCP_BROWSER_PROFILE_DIR")` returns
    `Some("~/.stapler-mcp/browser-profile".to_string())` and whose `home_dir()` returns
    `Some("/home/testuser".to_string())`, *When* `paths::browser_profile_dir(&env)` is
    called, *Then* it returns `Some("/home/testuser/.stapler-mcp/browser-profile".to_string())`.
  - *Given* the same `~`-prefixed value but `home_dir()` returns `None`, *When*
    `paths::browser_profile_dir(&env)` is called, *Then* it returns `None` (falls back to
    ephemeral, same rejection path as the relative-path case) rather than panicking.
**Files**: `crates/core/src/paths.rs`

##### Task 1.1.1a: Implement `browser_profile_dir` (~4 min)
- In `crates/core/src/paths.rs`, add a new constant
  `const ENV_BROWSER_PROFILE_DIR: &str = "STAPLER_MCP_BROWSER_PROFILE_DIR";` near the top
  (alongside `ENV_HOME_OVERRIDE` at line 7).
- Add `pub fn browser_profile_dir<E: EnvPort>(env: &E) -> Option<String>` after
  `embedding_cache_dir` (after line 41), following `base_dir`'s nonempty-check shape:
  `env.var(ENV_BROWSER_PROFILE_DIR).filter(|v| !v.is_empty())`.
- Before the absolute-path check, insert a `~`-expansion step (defense in depth for
  pre-mortem #1 — a value pasted into an MCP client's JSON `env` config block is never
  shell-expanded, so the README's copy-pasteable example must still work if an operator
  types `~` anyway): if the filtered value is exactly `"~"` or starts with `"~/"`, call
  `env.home_dir()`. If it returns `Some(home)`, replace the value with `home` (for a bare
  `"~"`) or `format!("{home}{}", &v[1..])` (for `"~/rest"` — `&v[1..]` already includes the
  leading `/`, matching `base_dir`'s own `format!("{home}/.stapler-mcp")` sibling pattern at
  `paths.rs:16`). If `env.home_dir()` returns `None`, leave `v` unchanged so it falls
  through to the absolute-path check below and gets rejected the same way a relative path
  is — never panic.
- Extend the chain with the existing absolute-path boundary check against the (possibly
  `~`-expanded) value: if it does not start with `/`
  (`!v.starts_with('/')` — this crate targets Unix only per `research/stack.md` §3, so a
  bare prefix check is sufficient, no need for `Path::is_absolute()`'s Windows-aware
  logic), print
  `eprintln!("stapler-mcp: STAPLER_MCP_BROWSER_PROFILE_DIR must be an absolute path, got {v:?} — falling back to ephemeral profile")`
  and return `None` instead of `Some(v)`.
- Add a one-line rustdoc noting: opt-in, no computed default (see ADR-0001), absolute-path
  only (leading `~`/`~/` expanded via `home_dir()` first) — unset/empty/relative/unexpandable-`~`
  means the caller should use the existing ephemeral pid+timestamp dir.
- Files: `crates/core/src/paths.rs`

##### Task 1.1.1b: Unit tests for the six resolver cases (~6 min)
- In `crates/core/src/paths.rs`'s existing `#[cfg(test)] mod tests` block, add
  `should_return_none_for_browser_profile_dir_when_unset`,
  `should_return_none_for_browser_profile_dir_when_empty`,
  `should_return_configured_path_for_browser_profile_dir_when_set`,
  `should_return_none_for_browser_profile_dir_when_relative_path_given`,
  `should_expand_leading_tilde_for_browser_profile_dir_when_home_dir_available`, and
  `should_return_none_for_browser_profile_dir_when_tilde_prefixed_and_home_dir_unavailable`,
  reusing the existing `MockEnv` struct with two new fields: `browser_profile_override:
  Option<String>` (extend `var`'s match arm to also handle
  `"STAPLER_MCP_BROWSER_PROFILE_DIR"`) and `home_dir_override: Option<String>` (change
  `home_dir()` to return `self.home_dir_override.clone()` instead of the current hardcoded
  `Some("/home/testuser".to_string())`). Update the three pre-existing `MockEnv` struct
  literals in this file (`should_build_docs_index_dir_under_home_override`,
  `should_build_embedding_cache_dir_under_home_override`,
  `should_build_docs_index_dir_under_default_home`) to add
  `home_dir_override: Some("/home/testuser".to_string())` so their behavior is unchanged.
- For `should_expand_leading_tilde_for_browser_profile_dir_when_home_dir_available`,
  construct `MockEnv { browser_profile_override: Some("~/.stapler-mcp/browser-profile".to_string()), home_dir_override: Some("/home/testuser".to_string()) }`
  and assert `browser_profile_dir(&env)` returns
  `Some("/home/testuser/.stapler-mcp/browser-profile".to_string())`.
- For `should_return_none_for_browser_profile_dir_when_tilde_prefixed_and_home_dir_unavailable`,
  use the same `browser_profile_override` value with `home_dir_override: None` and assert
  `browser_profile_dir(&env)` returns `None` (same fallback path as the relative-path case,
  not a panic).
- Run `cargo test -p stapler-mcp-core paths::` and confirm all pass.
- Files: `crates/core/src/paths.rs`

### Epic 1.2: Launch-time wiring
**Goal**: Make the resolved opt-in choice actually control which `user_data_dir` Chrome
launches with, without touching any other `NativeBrowser` behavior.

#### Story 1.2.1: `NativeBrowser::launch` accepts an optional persistent profile dir
**As a** daemon operator, **I want** the persistent directory I configured to actually
become Chrome's profile directory, **so that** cookies/login state survive a restart.
**Acceptance Criteria**:
- `launch(None)` produces byte-identical behavior to today's zero-arg `launch()`: a fresh
  `$TMPDIR/stapler-mcp-chromium-<pid>-<now_millis>` dir.
  - *Given* `NativeBrowser::launch(None)` is called, *When* `BrowserConfig` is built,
    *Then* `user_data_dir` equals
    `std::env::temp_dir().join(format!("stapler-mcp-chromium-{}-{}", std::process::id(), now_millis()))`,
    exactly as before this change.
- `launch(Some(dir))` creates `dir` (idempotently), sets its permissions to `0700` on
  Unix, and passes it straight to `BrowserConfig::builder().user_data_dir(dir)`.
  - *Given* `NativeBrowser::launch(Some(PathBuf::from("/tmp/test-profile")))` is called on
    Linux and `/tmp/test-profile` does not yet exist, *When* `launch` runs, *Then*
    `/tmp/test-profile` exists afterward with mode `0700` and Chrome is launched with
    `--user-data-dir=/tmp/test-profile`.
- When `persistent_profile_dir` is `Some` and `Browser::launch` fails because another live
  Chrome already holds that dir's `SingletonLock`, the returned `PortError::Other` message
  names the profile dir and the fact that it's in use, rather than raw chromiumoxide text.
  - *Given* `persistent_profile_dir = Some(PathBuf::from("/tmp/shared-profile"))` and a
    second `NativeBrowser::launch(Some(PathBuf::from("/tmp/shared-profile")))` call is made
    while a first one is still live, *When* the second `Browser::launch` call fails,
    *Then* `NativeBrowser::launch` returns
    `Err(PortError::Other("browser profile dir /tmp/shared-profile appears to be in use by another stapler-mcp daemon (SingletonLock): <underlying error>".to_string()))`.
- When `persistent_profile_dir` is `Some` and its path's ancestry contains a `.git`
  directory or a common sync-folder segment (`Dropbox`, `iCloud Drive`, `Syncthing`),
  `launch()` still succeeds but prints a loud warning naming the hazard, rather than
  silently accepting a standing-credential-store location.
  - *Given* `persistent_profile_dir = Some(PathBuf::from("/home/user/dotfiles/.git/../browser-profile"))`
    resolving to a path under a `.git`-containing ancestor, *When* `launch` runs, *Then* it
    still returns `Ok(...)` and `stderr` contains a line naming the path and the git/sync
    hazard.
**Files**: `crates/native/src/browser.rs`, `crates/cli/tests/browser_session.rs`

##### Task 1.2.1a: Change `launch()` signature and branch on `persistent_profile_dir` (~5 min)
- In `crates/native/src/browser.rs`, add `use std::path::PathBuf;` near the top imports
  (no existing `std::path` import in this file).
- Change `pub async fn launch() -> Result<Self, PortError>` (line 338) to
  `pub async fn launch(persistent_profile_dir: Option<PathBuf>) -> Result<Self, PortError>`.
- Immediately inside the function body, before consuming `persistent_profile_dir`, capture
  `let is_persistent = persistent_profile_dir.is_some();` — Tasks 1.2.1c, 1.2.1e, and
  1.2.1g all branch on this flag.
- Replace the unconditional `let user_data_dir = std::env::temp_dir().join(...)` block
  (lines 347-351) with a branch: `let user_data_dir = match persistent_profile_dir { Some(dir) => dir, None => std::env::temp_dir().join(format!("stapler-mcp-chromium-{}-{}", std::process::id(), now_millis())) };`
  keeping the existing `create_dir_all` call (line 352) unconditional and unchanged
  (works identically for both branches).
- Update the doc comment above the existing `user_data_dir` block (lines 339-346) to
  note it now only explains the `None`/ephemeral case; add one sentence pointing at the
  new `Some`/persistent case below it.
- **This changes `NativeBrowser::launch`'s public signature — Task 1.2.1b (next) fixes the
  only other call site in the workspace and must land in the same change**, or every
  subsequent task's `cargo test`/`cargo build` in this story fails to compile.
- Files: `crates/native/src/browser.rs`

##### Task 1.2.1b: Fix the now-broken `browser_session.rs` call site (BLOCKER fix) (~3 min)
- Run `grep -rn "NativeBrowser::launch(" crates/` first and confirm every call site found;
  as of this plan there are exactly two: `crates/cli/src/main.rs:88` (updated by Task
  1.2.2a, later in this plan) and `crates/cli/tests/browser_session.rs:1003`. If the grep
  turns up any additional call site not already covered by a task in this plan, add one
  before proceeding.
- In `crates/cli/tests/browser_session.rs`, at line 1003
  (`navigate_concurrent_should_not_exceed_max_open_sessions`), change
  `let mut browser = NativeBrowser::launch()` to
  `let mut browser = NativeBrowser::launch(None)`, preserving the rest of the
  `.await.expect(...)` chain unchanged — this test exercises `MAX_OPEN_SESSIONS`
  behavior, unrelated to profile persistence, so it should keep using ephemeral mode.
- Run `cargo build -p stapler-mcp --tests` and confirm the workspace compiles (this test
  is `#[ignore]`d and requires a real Chrome binary to actually run, but it must still
  compile).
- Files: `crates/cli/tests/browser_session.rs`

##### Task 1.2.1c: Warn on unsafe persistent profile locations (~5 min)
- In `crates/native/src/browser.rs`, add a small helper function (near `NativeBrowser`'s
  other free functions, e.g. next to `now_millis`):
  `fn unsafe_profile_location_reason(dir: &std::path::Path) -> Option<&'static str>` that
  walks `dir.ancestors()` and returns `Some("appears to be inside a git repository")` if
  any ancestor contains a `.git` entry (`ancestor.join(".git").exists()`), or
  `Some("appears to be inside a cloud-synced folder")` if any path component's `to_str()`
  matches `"Dropbox"`, `"iCloud Drive"`, or `"Syncthing"` exactly, else `None`. Check the
  git case first; return on the first match found while walking ancestors.
- In `launch()`, immediately after `create_dir_all(&user_data_dir)` and only when
  `is_persistent` is `true`, call this helper and, if it returns `Some(reason)`, print
  `eprintln!("stapler-mcp: warning: browser profile dir {} {reason} — this directory holds near-plaintext browser credentials; consider moving it", user_data_dir.display())`
  and continue (does not return an `Err`; matches ADR-0001's warn-not-reject decision).
- Files: `crates/native/src/browser.rs`

##### Task 1.2.1d: Unit tests for `unsafe_profile_location_reason` (~5 min)
- In `crates/native/src/browser.rs`'s existing `#[cfg(test)] mod tests` block, add:
  `should_return_none_for_safe_profile_path` (a plain temp-style path with no `.git`
  ancestor or sync-folder segment), `should_detect_git_repo_ancestor` (construct a temp
  dir via `std::env::temp_dir().join(format!("stapler-mcp-test-git-{}", now_millis()))`,
  create a `.git` subdirectory inside it with `std::fs::create_dir_all`, then check a
  nested child path under it — this avoids adding a new `tempfile` dev-dependency to this
  crate, mirroring `launch()`'s own temp-dir-naming convention; clean up with
  `std::fs::remove_dir_all` at the end of the test), and
  `should_detect_sync_folder_segment` (a constructed `PathBuf` like
  `PathBuf::from("/home/testuser/Dropbox/browser-profile")`, no real filesystem I/O needed
  for this case since it's a pure path-component check).
- Files: `crates/native/src/browser.rs`

##### Task 1.2.1e: Extract `harden_persistent_dir_permissions` and call it (~4 min)
- In `crates/native/src/browser.rs`, add
  `#[cfg(unix)] fn harden_persistent_dir_permissions(dir: &std::path::Path) -> Result<(), PortError>`
  that calls `std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))`,
  mapping any error through `PortError::Other(e.to_string())`.
- Add `use std::os::unix::fs::PermissionsExt;` gated `#[cfg(unix)]` at the top of the file
  (this crate is native-only and Unix-only per `research/stack.md` §3's note that
  `NativeEnv::home_dir()` has no Windows fallback, so a bare top-level `#[cfg(unix)]` import
  is safe).
- In `launch()`, immediately after `create_dir_all(&user_data_dir)` (and after Task
  1.2.1c's warning check), add a `#[cfg(unix)]`-gated call:
  `harden_persistent_dir_permissions(&user_data_dir)?` — only when `is_persistent` is
  `true`.
- Files: `crates/native/src/browser.rs`

##### Task 1.2.1f: Unit tests for `harden_persistent_dir_permissions` (~4 min)
- In the same `#[cfg(test)] mod tests` block, add
  `#[cfg(unix)] #[test] fn should_set_mode_0700_on_persistent_dir()`: create a temp dir via
  `std::env::temp_dir().join(format!("stapler-mcp-test-perms-{}", now_millis()))` and
  `std::fs::create_dir_all`, call `harden_persistent_dir_permissions(&dir)`, then assert
  `std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777 == 0o700` (using
  `std::os::unix::fs::PermissionsExt`); clean up with `std::fs::remove_dir_all`.
- Files: `crates/native/src/browser.rs`

##### Task 1.2.1g: Extract `describe_launch_error` and call it in `map_err` (~4 min)
- In `crates/native/src/browser.rs`, add
  `fn describe_launch_error(e: impl std::fmt::Display, dir: &std::path::Path, is_persistent: bool) -> PortError`
  that returns
  `PortError::Other(format!("browser profile dir {} appears to be in use by another stapler-mcp daemon (SingletonLock): {e}", dir.display()))`
  when `is_persistent` is `true`, else `PortError::Other(e.to_string())`.
- At the existing `Browser::launch(config).await.map_err(|e| PortError::Other(e.to_string()))?`
  call (lines 358-360), change the `map_err` closure to
  `|e| describe_launch_error(e, &user_data_dir, is_persistent)`.
- Files: `crates/native/src/browser.rs`

##### Task 1.2.1h: Unit tests for `describe_launch_error` (~3 min)
- In the same `#[cfg(test)] mod tests` block, add
  `should_name_profile_dir_and_singletonlock_when_persistent` (asserts the formatted
  message contains both the dir's display string and `"SingletonLock"` when
  `is_persistent` is `true`) and `should_pass_through_raw_error_when_ephemeral` (asserts
  the message equals the underlying error's `to_string()` when `is_persistent` is
  `false`) — both plain string-formatting assertions, no filesystem or Chrome needed.
- Files: `crates/native/src/browser.rs`

#### Story 1.2.2: Wire the env var through `run_daemon()`
**As a** daemon operator, **I want** setting `STAPLER_MCP_BROWSER_PROFILE_DIR` in the
daemon's environment to be sufficient, **so that** I don't need any other configuration
step.
**Acceptance Criteria**:
- With `STAPLER_MCP_BROWSER_PROFILE_DIR` unset, `run_daemon()` calls `NativeBrowser::launch(None)`
  and prints the ephemeral-mode startup log line.
  - *Given* `STAPLER_MCP_BROWSER_PROFILE_DIR` is not set in the daemon process's
    environment, *When* `run_daemon()` runs, *Then* it calls `NativeBrowser::launch(None)`
    and `stderr` contains
    `stapler-mcp: browser profile: ephemeral (temp dir, does not survive daemon restart)`.
- With `STAPLER_MCP_BROWSER_PROFILE_DIR` set to a path, `run_daemon()` calls
  `NativeBrowser::launch(Some(PathBuf::from(path)))` and prints the persistent-mode
  startup log line naming that path.
  - *Given* `STAPLER_MCP_BROWSER_PROFILE_DIR=/home/user/.stapler-mcp/browser-profile` is
    set in the daemon process's environment, *When* `run_daemon()` runs, *Then* it calls
    `NativeBrowser::launch(Some(PathBuf::from("/home/user/.stapler-mcp/browser-profile")))`
    and `stderr` contains
    `stapler-mcp: browser profile: persistent at /home/user/.stapler-mcp/browser-profile`.
**Files**: `crates/cli/src/main.rs`

##### Task 1.2.2a: Read the resolver and pass its result into `launch()` (~4 min)
- In `crates/cli/src/main.rs`'s `run_daemon()`, immediately before the existing
  `let mut browser = match NativeBrowser::launch().await { ... }` block (line 88), add
  `let persistent_profile_dir = stapler_mcp_core::paths::browser_profile_dir(&env).map(std::path::PathBuf::from);`
  (mirrors the existing `paths::embedding_cache_dir(&env)` call style at line 95).
- Change the `NativeBrowser::launch().await` call (line 88) to
  `NativeBrowser::launch(persistent_profile_dir).await` — **no `.clone()` needed**: Task
  1.2.2b's startup log line is inserted *before* this call and only borrows
  `&persistent_profile_dir`, so by the time this line moves the value into `launch(...)`,
  the log line has already read it. Confirm the final ordering in the function body is:
  (1) compute `persistent_profile_dir`, (2) Task 1.2.2b's log line (borrows), (3) this
  `launch(persistent_profile_dir)` call (moves).
- Files: `crates/cli/src/main.rs`

##### Task 1.2.2b: Add the startup log line (~2 min)
- Immediately before the `NativeBrowser::launch(...)` call from Task 1.2.2a (i.e. before
  the move, so this only borrows `&persistent_profile_dir`), add:
  `match &persistent_profile_dir { Some(dir) => eprintln!("stapler-mcp: browser profile: persistent at {}", dir.display()), None => eprintln!("stapler-mcp: browser profile: ephemeral (temp dir, does not survive daemon restart)") };`
- Files: `crates/cli/src/main.rs`

#### Story 1.2.3: Expose active profile mode and unsafe-location warning via `ping`
**As a** daemon operator, **I want** to positively verify whether persistence is actually
active without losing cookies and guessing, **so that** an env var that never reached the
daemon's actual process environment (the same footgun this repo already has for
`BRAVE_API_KEY`/`STAPLER_MCP_HOME`, README:150-152) is diagnosable, and so that the
git/sync-folder credential-exposure warning (Task 1.2.1c) isn't visible only to someone who
happens to open `daemon.log` — which the daemon writes to only because it has no
controlling terminal, not because anyone is expected to tail it (pre-mortem #2, #3). Reuses
the daemon's one existing health-check RPC (`ping`) rather than adding a second, different
visibility mechanism — `daemon.log`'s own warning is itself only ever a startup-time signal
(Task 1.2.1c fires once, at launch), so a `ping` response that reflects the same
startup-time snapshot is not a freshness regression. **This story only changes what `ping`'s
response contains — it does not make `ping` itself callable by anything other than test code
or the internal `ensure_daemon` health check.** Story 1.2.4 (below) adds the actual entry
points (an MCP tool and a CLI flag) that let an operator or an LLM agent trigger this and see
the result.
**Acceptance Criteria**:
- The `ping` RPC response's `result` object includes a `browserProfileMode` field naming
  the daemon's actual active mode. (camelCase on the wire — matching every other tool's
  `*Output` struct convention in `crates/core/src/schema.rs` and Task 1.2.4a's
  `DaemonStatusOutput`; see Task 1.2.3e's rationale note.)
  - *Given* a daemon started with `STAPLER_MCP_BROWSER_PROFILE_DIR` unset, *When* a client
    sends a `ping` request, *Then* the response's `result.browserProfileMode` equals
    `"ephemeral"`.
  - *Given* a daemon started with `STAPLER_MCP_BROWSER_PROFILE_DIR=/home/user/.stapler-mcp/browser-profile`,
    *When* a client sends a `ping` request, *Then* the response's `result.browserProfileMode`
    equals `"persistent at /home/user/.stapler-mcp/browser-profile"`.
- The `ping` RPC response also includes a `browserProfileWarning` field mirroring Task
  1.2.1c's git/sync-folder hazard warning, so that warning is discoverable without reading
  `daemon.log`.
  - *Given* a daemon started with `STAPLER_MCP_BROWSER_PROFILE_DIR` pointed at a path whose
    ancestry contains a `.git` directory, *When* a client sends a `ping` request, *Then*
    the response's `result.browserProfileWarning` is a non-null string naming the path
    and the git/sync hazard (the same text `daemon.log` received).
  - *Given* a daemon started with `STAPLER_MCP_BROWSER_PROFILE_DIR` unset, or set to a safe
    location, *When* a client sends a `ping` request, *Then* `result.browserProfileWarning`
    is `null`.
- A daemon that never calls `Daemon::set_status_extra` (the wasm adapter,
  `crates/wasm/src/lib.rs:52`, which has no browser-profile concept) sees no change to its
  `ping` response.
  - *Given* a `Daemon` on which `set_status_extra` is never called, *When*
    `handle_request_bytes` processes a `ping` request, *Then* the response equals
    `{"result": {"pong": true}}`, byte-identical to today.
**Files**: `crates/native/src/browser.rs`, `crates/core/src/daemon.rs`, `crates/cli/src/main.rs`, `crates/cli/tests/daemon_ping.rs`

##### Task 1.2.3a: Extract `compute_unsafe_profile_warning` and store its result on `NativeBrowser` (~4 min)
- In `crates/native/src/browser.rs`, add a small pure helper (same isolate-via-seam pattern
  as `describe_launch_error`, Task 1.2.1g):
  `fn compute_unsafe_profile_warning(dir: &std::path::Path, is_persistent: bool) -> Option<String>`
  — returns `None` when `is_persistent` is `false`; otherwise delegates to Task 1.2.1c's
  `unsafe_profile_location_reason(dir)` and, if it returns `Some(reason)`, formats and
  returns `Some(format!("browser profile dir {} {reason} — this directory holds near-plaintext browser credentials; consider moving it", dir.display()))`; otherwise `None`.
- In `launch()`, replace Task 1.2.1c's inline
  `if let Some(reason) = unsafe_profile_location_reason(&user_data_dir) { eprintln!(...) }`
  (only reached when `is_persistent`) with:
  `let unsafe_profile_warning = compute_unsafe_profile_warning(&user_data_dir, is_persistent); if let Some(msg) = &unsafe_profile_warning { eprintln!("stapler-mcp: warning: {msg}"); }`
  — identical printed text to before, now also captured in a binding.
- Add field `unsafe_profile_warning: Option<String>` to the `NativeBrowser` struct (near
  `pub reaper`) and include `unsafe_profile_warning,` in the `Ok(NativeBrowser { ... })`
  construction.
- Add `pub fn unsafe_profile_warning(&self) -> Option<&str> { self.unsafe_profile_warning.as_deref() }`.
- Files: `crates/native/src/browser.rs`

##### Task 1.2.3b: Unit tests for `compute_unsafe_profile_warning` (~3 min)
- In the same `#[cfg(test)] mod tests` block, add
  `should_return_none_for_compute_unsafe_profile_warning_when_ephemeral` (any `dir`,
  `is_persistent = false` → `None`, no filesystem I/O needed) and
  `should_return_formatted_warning_for_compute_unsafe_profile_warning_when_persistent_and_unsafe`
  (reuse Task 1.2.1d's git-ancestor or sync-folder fixture, `is_persistent = true` →
  asserts the returned `Some(msg)` contains both the dir's display string and
  `"near-plaintext browser credentials"`).
- Files: `crates/native/src/browser.rs`

##### Task 1.2.3c: Add `Daemon::set_status_extra` and merge it into the `ping` response (~4 min)
- In `crates/core/src/daemon.rs`, add a field `status_extra: RefCell<serde_json::Value>` to
  the `Daemon` struct, initialized to `serde_json::json!({})` in `Daemon::new()`.
- Add `pub fn set_status_extra(&self, extra: serde_json::Value)` that stores it:
  `*self.status_extra.borrow_mut() = extra;`. Intended to be called at most once, before
  `run()`'s accept loop starts — single-writer, read-only afterward, so no
  concurrent-mutation concern on this single-threaded daemon.
- In `dispatch()`, change the `PING_TOOL` arm from
  `PING_TOOL => Response::ok(serde_json::json!({"pong": true})),` to merge in the stored
  extra fields:
  ```
  PING_TOOL => {
      let mut pong = serde_json::json!({"pong": true});
      if let (serde_json::Value::Object(extra), serde_json::Value::Object(pong_obj)) =
          (&*self.status_extra.borrow(), &mut pong)
      {
          pong_obj.extend(extra.clone());
      }
      Response::ok(pong)
  }
  ```
- This is additive and backward compatible: when `set_status_extra` is never called (e.g.
  the wasm adapter's `Daemon::new()` at `crates/wasm/src/lib.rs:52`), `status_extra` stays
  `{}` and `ping`'s response is unchanged (`{"pong": true}`).
- Files: `crates/core/src/daemon.rs`

##### Task 1.2.3d: Unit tests for the merged `ping` response (~3 min)
- In `crates/core/src/daemon.rs`'s existing `#[cfg(test)] mod tests` block, add
  `should_merge_status_extra_fields_into_ping_response` (construct a `Daemon`, call
  `daemon.set_status_extra(serde_json::json!({"browserProfileMode": "persistent at /tmp/x"}))`
  — camelCase, matching what Task 1.2.3e's real caller actually sets (this merge itself is
  generic over key casing, but the example should match production usage rather than
  imply snake_case is the convention), call `daemon.handle_request_bytes(...)` with a
  serialized `ping` `Request`, deserialize the response bytes, and assert it equals
  `serde_json::json!({"result": {"pong": true, "browserProfileMode": "persistent at /tmp/x"}})`)
  and `should_leave_ping_response_unchanged_when_status_extra_never_set` (same flow without
  calling `set_status_extra`, asserting the response equals
  `serde_json::json!({"result": {"pong": true}})` — covers the wasm-adapter no-op case).
- Files: `crates/core/src/daemon.rs`

##### Task 1.2.3e: Wire `browserProfileMode`/`browserProfileWarning` into `status_extra` (~4 min)
- In `crates/cli/src/main.rs`'s `run_daemon()`, after the `NativeBrowser::launch(persistent_profile_dir).await`
  call succeeds (Task 1.2.2a) and before `daemon.run(...)` is called (`daemon` is
  constructed at line 100, after `browser` is bound), add:
  ```
  let browser_profile_mode = match &persistent_profile_dir {
      Some(dir) => format!("persistent at {}", dir.display()),
      None => "ephemeral".to_string(),
  };
  daemon.set_status_extra(serde_json::json!({
      "browserProfileMode": browser_profile_mode,
      "browserProfileWarning": browser.unsafe_profile_warning(),
  }));
  ```
  (`browser` is the `Rc<NativeBrowser>` already bound at line 88; `unsafe_profile_warning()`
  returns `Option<&str>`, which `serde_json::json!` serializes as `null` or a string
  automatically.)
- **Use camelCase JSON keys here, not `browser_profile_mode`/`browser_profile_warning`.**
  This is the one hand-built `json!()` response body in the daemon (`PING_TOOL`'s handler,
  `crates/core/src/daemon.rs`'s `dispatch()`, is special-cased ahead of the `handlers` map
  and never goes through a `#[serde(rename_all = "camelCase")]`-derived `Output` struct the
  way every other registered tool's response does). Every other tool's RPC response is
  already camelCase on the wire as a side effect of serializing its `*Output` struct
  (confirmed directly: `crates/cli/tests/daemon_ping.rs`'s real round-trip test asserts
  `fetch_result["finalUrl"]`, not `final_url`). Task 1.2.4a's `DaemonStatusOutput` follows
  that same universal convention (`#[serde(rename_all = "camelCase")]`, matching literally
  every other `*Output` struct in `crates/core/src/schema.rs`, no exceptions found) and
  Task 1.2.4b's `call_daemon` deserializes this exact `status_extra` payload into it via
  `serde_json::from_value`. Since both of `DaemonStatusOutput`'s new fields are
  `#[serde(default)]`, a snake_case/camelCase mismatch here would deserialize to `None` on
  every call with no error — silently wrong, not absent. Emitting camelCase here is what
  keeps this new field pair consistent with the wire format every other tool already uses,
  not a special case.
- This must run before the daemon starts accepting connections, so the first `ping` an
  operator sends already reflects the real mode.
- Files: `crates/cli/src/main.rs`

##### Task 1.2.3f: Assert the new `ping` fields in the real daemon round-trip test (~3 min)
- In `crates/cli/tests/daemon_ping.rs`'s `daemon_architecture_and_tools_round_trip` test
  (not `#[ignore]`d — it already builds and runs the real daemon binary), replace step 3's
  `client::ping(&socket, &sock_path, Duration::from_secs(2)).await.expect("ping should succeed against running daemon");`
  with
  `let ping_result = client::call(&socket, &sock_path, stapler_mcp_core::protocol::PING_TOOL, None, Duration::from_secs(2)).await.expect("ping should succeed against running daemon");`
  followed by
  `assert_eq!(ping_result["browserProfileMode"], serde_json::json!("ephemeral"));`
  and
  `assert_eq!(ping_result["browserProfileWarning"], serde_json::Value::Null);`
  — camelCase keys, matching Task 1.2.3e's wire format (this assertion reads the raw
  `serde_json::Value` returned by `client::call`, not a `DaemonStatusOutput`, so it would
  not have caught the case-mismatch bug on its own — Task 1.2.4f adds the assertion that
  does, by going through the actual `DaemonStatusOutput` deserialization path). This test
  never sets `STAPLER_MCP_BROWSER_PROFILE_DIR`, so ephemeral mode with no
  warning is the correct expectation. This is a real assertion against the actual daemon
  binary and Task 1.2.3e's wiring, not a synthetic unit test.
- Add `use stapler_mcp_core::protocol::PING_TOOL;` to this file's imports if not already
  present (verify the file's existing `use stapler_mcp_core::{...}` block before editing;
  as of this plan it imports only `client` and `paths` from `stapler_mcp_core`).
- Files: `crates/cli/tests/daemon_ping.rs`

#### Story 1.2.4: Make the `ping` diagnostic surface reachable by an operator and an MCP client
**As a** daemon operator, or **as** the LLM agent driving `stapler_browser_navigate`, **I
want** an actual, documented way to trigger the diagnostic call Story 1.2.3 built, **so
that** "I set the env var and have no way to verify it worked" (pre-mortem #2) is actually
closed, not just closed at the wire-protocol level.

**Why this story exists**: a UX review of this plan found that Story 1.2.3 extends `ping`'s
*response*, but `ping`/`PING_TOOL` itself (`crates/core/src/protocol.rs:9`) is a daemon-
internal lifecycle verb, deliberately not registered as an MCP tool. Confirmed against the
actual code before choosing a fix:
- `ThinClient` (`crates/cli/src/thin_client.rs`) — the `rmcp::ServerHandler` an MCP client
  (including the LLM agent driving this daemon) actually talks to over stdio — only exposes
  the tools explicitly listed via `#[tool(name = "...")]` in its `#[tool_router] impl`
  block. `ping` is not one of them; there is no way for an MCP client to send it.
- `crates/cli/src/main.rs`'s `main()` only branches on `--daemon` (`args().any(|a| a ==
  "--daemon")`); there is no `--status`/`--ping`-style flag, so there is no CLI invocation
  either. The only caller of `ping` today is `crates/core/src/client.rs`'s internal
  `ensure_daemon`/`ping` functions (used by `ThinClient::call_daemon` before *every* tool
  call, and by `crates/cli/tests/daemon_ping.rs`, a Rust test file an operator cannot run).
- The dispatch mechanism itself is **not** the obstacle: `Daemon::dispatch()`
  (`crates/core/src/daemon.rs:51-74`) special-cases the `PING_TOOL`/`SHUTDOWN_TOOL` strings
  ahead of the `handlers` map lookup, but every registered tool (e.g.
  `stapler_browser_navigate`) reaches the daemon through the exact same `client::call`
  function (`crates/core/src/client.rs:23-48`) that `client::ping` already uses internally.
  There is no separate transport or protocol layer to build — only a missing registration.

Given that, the smallest proportionate fix is to reuse this existing, already-working
transport twice: once as a real MCP tool (for the LLM-agent audience) and once as a CLI flag
(for the human-operator audience) — both audiences named in pre-mortem #2, and both cheap
once the underlying `client::call`/`PING_TOOL` plumbing is confirmed shared. Rejected: (a)
building only the MCP tool — leaves an operator debugging via `daemon.log` alone, the exact
gap `research/pitfalls.md` and Story 1.2.3's own rationale call out as insufficient; (b)
building only the CLI flag — leaves the LLM agent (the other named audience in pre-mortem #2,
and the one actually driving `stapler_browser_navigate` day to day) with no way to check
its own daemon's state via a tool call. Neither omission is justified by the code found
above — both additions are a few lines each, reusing existing types and existing transport,
proportionate to a `later`-priority feature.

**Acceptance Criteria**:
- An MCP client can call a `stapler_daemon_status` tool and see the same
  `browserProfileMode`/`browserProfileWarning` fields Story 1.2.3 added to `ping`.
  - *Given* a running daemon started with `STAPLER_MCP_BROWSER_PROFILE_DIR` unset, *When*
    an MCP client calls the `stapler_daemon_status` tool (no parameters), *Then* the
    response's `browserProfileMode` field equals `"ephemeral"` and `browserProfileWarning`
    is absent/null.
  - *Given* the same daemon, *When* `ThinClient::registered_tools()` (the same in-process
    accessor `crates/cli/tests/tool_schema.rs` already uses for every other tool) is
    inspected, *Then* it includes a tool named `stapler_daemon_status` with a non-empty
    description and an `inputSchema` matching `DaemonStatusInput`'s `schemars`-derived
    schema.
  - *Given* the same daemon, *When* the `stapler_daemon_status` tool method itself is
    called in-process (not just its registration inspected — Task 1.2.4f) and its response
    is deserialized into `DaemonStatusOutput`, *Then* `browser_profile_mode` (the Rust
    field) equals `Some("ephemeral".to_string())` and `browser_profile_warning` equals
    `None` — a live assertion on the deserialized struct, not the raw JSON, so it fails if
    the wire keys `status_extra` emits (Task 1.2.3e) and `DaemonStatusOutput`'s expected
    keys (Task 1.2.4a) ever disagree in casing.
- An operator can run `stapler-mcp --status` against a running daemon and see the resolved
  browser profile mode (and warning, if any) printed to stdout, without needing to know the
  MCP protocol or read `daemon.log`.
  - *Given* a running daemon started with `STAPLER_MCP_BROWSER_PROFILE_DIR` set to a path
    whose ancestry contains a `.git` directory, *When* an operator runs
    `stapler-mcp --status`, *Then* stdout contains a line naming
    `persistent at <path>` and a second line containing the git/sync-folder hazard warning
    text, and the process exits `0`.
  - *Given* no daemon is currently running (no live socket), *When* an operator runs
    `stapler-mcp --status`, *Then* it prints a "daemon not reachable" message to stderr and
    exits non-zero — it does **not** auto-spawn a daemon (diagnostic-only, consistent with
    `--daemon`'s own "normally auto-started, not run by hand" framing in the README).
**Files**: `crates/core/src/schema.rs`, `crates/cli/src/thin_client.rs`, `crates/cli/src/main.rs`, `crates/cli/tests/tool_schema.rs`, `crates/cli/tests/daemon_ping.rs`

##### Task 1.2.4a: Add `DaemonStatusInput`/`DaemonStatusOutput` to `schema.rs` (~3 min)
- In `crates/core/src/schema.rs`, near `BrowserListSessionsInput`/`BrowserListSessionsOutput`
  (the existing empty-input/typed-output pair closest in shape), add:
  ```
  #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
  #[serde(rename_all = "camelCase")]
  pub struct DaemonStatusInput {}

  #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
  #[serde(rename_all = "camelCase")]
  pub struct DaemonStatusOutput {
      pub pong: bool,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub browser_profile_mode: Option<String>,
      #[serde(default, skip_serializing_if = "Option::is_none")]
      pub browser_profile_warning: Option<String>,
  }
  ```
  `browser_profile_mode`/`browser_profile_warning` are `Option`, not always-populated,
  because the wasm adapter never calls `set_status_extra` (Task 1.2.3c) and its `ping`
  response stays `{"pong": true}` — deserializing that into `DaemonStatusOutput` must still
  succeed with both fields `None`, not fail.
- The `#[serde(rename_all = "camelCase")]` here is not optional boilerplate: it's what makes
  this struct expect `browserProfileMode`/`browserProfileWarning` on the wire, matching
  Task 1.2.3e's `status_extra` payload (which must emit those same camelCase keys) and every
  other `*Output` struct in this file. If the two ever disagree on casing, both fields
  silently deserialize to `None` (they're `#[serde(default)]`) with no error — see Task
  1.2.4f for the live round-trip test that guards against exactly this.
- Files: `crates/core/src/schema.rs`

##### Task 1.2.4b: Register `stapler_daemon_status` as an MCP tool on `ThinClient` (~4 min)
- In `crates/cli/src/thin_client.rs`, add `DaemonStatusInput, DaemonStatusOutput` to the
  existing `use stapler_mcp_core::schema::{...}` import block.
- In the `#[tool_router] impl ThinClient` block, add (placed after `remove_indexed_source`,
  the last existing tool method):
  ```
  #[tool(
      name = "stapler_daemon_status",
      description = "Check whether the shared stapler-mcp daemon is reachable and report daemon-wide status that isn't tied to any one browser session — currently the active browser profile persistence mode (\"ephemeral\" or \"persistent at <path>\") and, if the configured profile directory looks unsafe (inside a git repository or a cloud-synced folder), a warning naming the hazard. Use this to confirm STAPLER_MCP_BROWSER_PROFILE_DIR actually took effect rather than inferring it from whether cookies survived a restart."
  )]
  async fn daemon_status(
      &self,
      _params: Parameters<DaemonStatusInput>,
  ) -> Result<Json<DaemonStatusOutput>, String> {
      call_daemon(
          stapler_mcp_core::protocol::PING_TOOL,
          DaemonStatusInput {},
      )
      .await
      .map(Json)
  }
  ```
  This calls the exact same `call_daemon` helper (and therefore the exact same
  `client::call` socket transport) every other tool method in this file already uses —
  `PING_TOOL`'s handler in `Daemon::dispatch()` ignores its `params` entirely, so passing
  `DaemonStatusInput {}` as the (unused) request body is harmless.
- `#[tool(...)]`-annotated methods on this `impl` block are module-private (matching every
  other tool method here, e.g. `fetch_page`, `browser_navigate` — they're invoked through
  the macro-generated `ToolRouter`, not called directly), so Task 1.2.4f's live round-trip
  test can't call `daemon_status` itself from outside the module. Add a second,
  `#[cfg(test)]`-gated accessor next to the existing `registered_tools()` (in the other
  `impl ThinClient` block, further down this file) that exercises the exact same
  `call_daemon::<DaemonStatusInput, DaemonStatusOutput>` deserialization path:
  ```
  #[cfg(test)]
  #[allow(dead_code)]
  pub async fn call_daemon_status_for_test(&self) -> Result<DaemonStatusOutput, String> {
      self.daemon_status(Parameters(DaemonStatusInput {}))
          .await
          .map(|Json(output)| output)
  }
  ```
  Same rationale as `registered_tools()`'s own doc comment: only reachable from the separate
  integration-test binary that pulls this file in via `#[path]`, invisible to dead-code
  analysis of this compilation unit, hence `#[allow(dead_code)]`.
- Files: `crates/cli/src/thin_client.rs`

##### Task 1.2.4c: Add a `--status` CLI flag to `main.rs` (~5 min)
- In `crates/cli/src/main.rs`, add `use std::time::Duration;` to the top imports.
- In `main()`, alongside the existing `let is_daemon = std::env::args().any(|a| a ==
  "--daemon");`, add `let is_status = std::env::args().any(|a| a == "--status");` and change
  the `if is_daemon { ... } else { ... }` dispatch to a three-way branch: `is_daemon` →
  `run_daemon()` (unchanged), `is_status` → a new `run_status()` (below), else →
  `run_thin_client()` (unchanged). `run_status()` returns an `i32` exit code; call
  `std::process::exit(code)` after `local.block_on(&rt, run_status())` returns, mirroring
  how `run_thin_client`/`run_daemon` already call `std::process::exit(1)` on their own error
  paths.
- Add:
  ```
  async fn run_status() -> i32 {
      let env = NativeEnv;
      let socket = NativeSocketFactory;
      let sock_path = paths::socket_path(&env);
      match stapler_mcp_core::client::call(
          &socket,
          &sock_path,
          stapler_mcp_core::protocol::PING_TOOL,
          None,
          Duration::from_secs(2),
      )
      .await
      {
          Ok(result) => {
              let mode = result
                  .get("browserProfileMode")
                  .and_then(|v| v.as_str())
                  .unwrap_or("ephemeral");
              println!("stapler-mcp: daemon reachable");
              println!("stapler-mcp: browser profile: {mode}");
              if let Some(warning) = result.get("browserProfileWarning").and_then(|v| v.as_str())
              {
                  println!("stapler-mcp: warning: {warning}");
              }
              0
          }
          Err(e) => {
              eprintln!("stapler-mcp: daemon not reachable: {e}");
              1
          }
      }
  }
  ```
  **Use `browserProfileMode`/`browserProfileWarning` here, not `browser_profile_mode`/
  `browser_profile_warning`.** This function reads the raw `serde_json::Value` returned by
  `client::call` directly (it never goes through `DaemonStatusOutput`), so it must match
  Task 1.2.3e's actual camelCase wire keys itself — getting this wrong would make `--status`
  silently print `"ephemeral"` for every daemon (via `unwrap_or("ephemeral")`) regardless of
  the real mode, the same class of silent-wrong-data bug as the MCP-tool path, just with a
  fallback masking it instead of a `None`.
  Deliberately calls `client::call` directly (bare `ping`, not `ensure_daemon`) — `--status`
  is a read-only diagnostic, so it must **not** auto-spawn a daemon just to report that none
  was running; that would make "is a daemon running" unanswerable by this flag.
  **Known residual gap**: Task 1.2.4f's regression guard type-checks
  `DaemonStatusOutput`'s camelCase deserialization (the MCP-tool path) but not this raw
  `serde_json::Value::get("browserProfileMode")` string-literal lookup — only Task 1.2.4e's
  weaker stdout substring match exercises this function. If Task 1.2.3e's wire-key spelling
  ever drifts again, this path could regress silently in a way 1.2.4f wouldn't catch. Not
  worth a second typed-struct regression test for one string constant in a `later`-priority
  item; flagged here rather than silently assumed covered.
- Files: `crates/cli/src/main.rs`

##### Task 1.2.4d: Assert `stapler_daemon_status` is registered, in `tool_schema.rs` (~3 min)
- In `crates/cli/tests/tool_schema.rs`, add `DaemonStatusInput` to the existing `use
  stapler_mcp_core::schema::{...}` import block, and add one more entry to the `expected`
  vec (after the `stapler_browser_find` entry):
  ```
  (
      "stapler_daemon_status",
      schema_for_input::<DaemonStatusInput>().expect("DaemonStatusInput schemars schema"),
  ),
  ```
  The existing loop at the bottom of this test already asserts every `expected` tool is
  present with a non-empty description and a matching `inputSchema` — no other change to
  this file is needed.
- Files: `crates/cli/tests/tool_schema.rs`

##### Task 1.2.4e: Assert `--status` output against a real running daemon (~4 min)
- In `crates/cli/tests/daemon_ping.rs`'s `daemon_architecture_and_tools_round_trip` test,
  after step 3 (the real `ping` round trip, now extended by Task 1.2.3f) and before step 4
  (the second `ensure_daemon` call), add a step that spawns the already-built test binary
  (`env!("CARGO_BIN_EXE_stapler-mcp")`, the same `exe` binding already in this test) with
  `--status` via `std::process::Command`, setting `STAPLER_MCP_HOME` in its environment to
  the same `home` this test already uses, and asserts: the process exits `0`, and its
  captured stdout contains `"browser profile: ephemeral"` (this test never sets
  `STAPLER_MCP_BROWSER_PROFILE_DIR`, matching Task 1.2.3f's same-scope assertion).
- Files: `crates/cli/tests/daemon_ping.rs`

##### Task 1.2.4f: Live round-trip the `stapler_daemon_status` tool method itself, through `DaemonStatusOutput` (~4 min)
- **Why this task exists, distinct from 1.2.3f/1.2.4e**: neither existing round-trip
  assertion exercises the actual bug surface. Task 1.2.3f asserts on the raw
  `serde_json::Value` `client::call` returns (`ping_result["browserProfileMode"]`) — it
  never deserializes into `DaemonStatusOutput`. Task 1.2.4e asserts on `--status`'s stdout
  text, which is produced by `run_status()`'s own raw `.get("browserProfileMode")` lookup
  (Task 1.2.4c) — also not `DaemonStatusOutput`. Task 1.2.4d only checks tool
  *registration*/schema shape, never calls the tool. **None of the existing tasks would
  have caught a `browser_profile_mode` (snake_case) vs. `browserProfileMode` (camelCase)
  mismatch between Task 1.2.3e's `status_extra` payload and `DaemonStatusOutput`'s
  `#[serde(rename_all = "camelCase")]`** — that mismatch deserializes silently to `None`
  (both fields are `#[serde(default)]`), not an error, so only an assertion on the actual
  deserialized Rust field values closes the gap. This task is that assertion, and is meant
  as a permanent regression guard for this bug class, not a one-time fix verification.
- In `crates/cli/tests/daemon_ping.rs`, add
  `#[path = "../src/thin_client.rs"] mod thin_client;` near the top of the file (same
  `#[path]` trick `crates/cli/tests/tool_schema.rs` already uses and documents in its own
  module doc comment, needed because `crates/cli` is a bin-only crate with no `[lib]`
  target), and add `use stapler_mcp_core::schema::DaemonStatusOutput;` to the existing
  `use stapler_mcp_core::{...}` import block if not already present.
- In `daemon_architecture_and_tools_round_trip`, immediately after Task 1.2.4e's `--status`
  subprocess step and before step 4 (the second `ensure_daemon` call), add:
  ```
  let status = thin_client::ThinClient::new()
      .call_daemon_status_for_test()
      .await
      .expect("stapler_daemon_status should succeed against the running daemon");
  assert_eq!(status.browser_profile_mode, Some("ephemeral".to_string()));
  assert_eq!(status.browser_profile_warning, None);
  ```
  This calls `ThinClient::call_daemon_status_for_test()` (added in Task 1.2.4b), which
  calls the real `daemon_status` tool method, which calls `call_daemon::<DaemonStatusInput,
  DaemonStatusOutput>` — the exact same generic deserialization path
  `crates/cli/src/thin_client.rs`'s `call_daemon` uses for every other tool, against the
  same live daemon this test already spawned via `ensure_daemon` (reuses the process-global
  `STAPLER_MCP_HOME` env var already set earlier in this test, since `ThinClient::new()`
  constructs its own `NativeEnv` internally, matching how every other in-test tool call in
  this file already relies on that same process-global env state).
  Before the fix in Task 1.2.3e, this assertion fails with `status.browser_profile_mode ==
  None` (not an error) — confirming this is a real regression guard, not a vacuously
  passing check.
- Files: `crates/cli/tests/daemon_ping.rs`

### Epic 1.3: Verification
**Goal**: Confirm, against a real Chrome/CDP daemon (not just unit-level branching logic),
that a cookie set before a simulated daemon restart is still present after — de-risking
the unconfirmed upstream chromiumoxide report (`research/pitfalls.md` §5, issue #252)
before relying on this feature — and that the one Story 1.2.1 acceptance criterion no unit
test can cover (a real `SingletonLock` collision) actually produces the documented error.

#### Story 1.3.1: Cross-restart cookie-persistence smoke test
**As a** developer shipping this feature, **I want** an automated test that proves a
cookie survives a daemon restart when persistence is opted into, **so that** this feature
isn't shipped on an unverified assumption about chromiumoxide's `user_data_dir` behavior.
**Acceptance Criteria**:
- A cookie set via `stapler_browser_evaluate` before the daemon is stopped is still
  readable via `stapler_browser_evaluate` after the daemon is restarted against the same
  `STAPLER_MCP_BROWSER_PROFILE_DIR`.
  - *Given* a daemon started with `STAPLER_MCP_BROWSER_PROFILE_DIR` set to a fixed
    `tempfile::tempdir()` path (held outside the per-test `STAPLER_MCP_HOME`, so it
    outlives the "restart"), a session navigated to the mock site, and
    `document.cookie = "sticky=yes"` set via `stapler_browser_evaluate`, *When* the
    `"shutdown"` RPC is called, the daemon subprocess exits, `client::ensure_daemon` is
    called again (same `STAPLER_MCP_BROWSER_PROFILE_DIR`), a fresh `stapler_browser_navigate`
    session is opened, and `document.cookie` is read via `stapler_browser_evaluate`,
    *Then* the result contains `"sticky=yes"`.
**Files**: `crates/cli/tests/browser_profile_persistence.rs` (new)

##### Task 1.3.1a: Write the `#[ignore]` cross-restart integration test (~5 min)
- Create `crates/cli/tests/browser_profile_persistence.rs`, following
  `crates/cli/tests/browser_session.rs`'s harness conventions: a local `TestEnv`
  implementing `EnvPort`, a `spawn_mock_site()` helper (copy the minimal single-page
  version from `browser_session.rs:70-160`, or factor a shared page if one already serves
  a plain HTML page with no JS needed beyond `document.cookie`), and
  `client::ensure_daemon`/`client::call` exactly as `start_daemon` does at
  `browser_session.rs:170-209`.
- Test body: create one `tempfile::tempdir()` for `STAPLER_MCP_BROWSER_PROFILE_DIR`
  (separate from the per-run `STAPLER_MCP_HOME` tempdir, since the profile dir must
  outlive the simulated restart while `STAPLER_MCP_HOME` may also be reused or fresh —
  reuse the same `STAPLER_MCP_HOME` across both daemon starts in this test for simplicity,
  since this item doesn't require profile persistence to survive a `STAPLER_MCP_HOME`
  change too). Set both env vars, call `ensure_daemon`, `stapler_browser_navigate` to the
  mock site, `stapler_browser_evaluate` with `document.cookie = "sticky=yes"`, call
  `"shutdown"`, sleep briefly (mirror `daemon_ping.rs`'s pattern around its own `shutdown`
  test) or poll until the socket is gone, call `ensure_daemon` again (same env), navigate
  again, `stapler_browser_evaluate` with `document.cookie`, assert the response contains
  `"sticky=yes"`.
- Mark `#[tokio::test]` `#[ignore]` (requires a real Chrome binary, same as every other
  test in `browser_session.rs`). Document the `cargo test -p stapler-mcp --test
  browser_profile_persistence -- --ignored` invocation in a module-level doc comment,
  matching `browser_session.rs:20-21`'s own convention.
- Files: `crates/cli/tests/browser_profile_persistence.rs`

#### Story 1.3.2: SingletonLock collision smoke test
**As a** developer shipping this feature, **I want** an automated test (against a real
Chrome binary) proving Story 1.2.1's third acceptance criterion — the SingletonLock
collision error message — actually fires, **so that** this AC isn't only checked by unit
tests that fake the underlying error, given `describe_launch_error` (Task 1.2.1g) is unit
tested against a synthetic error string, not a real chromiumoxide `SingletonLock` failure.
**Acceptance Criteria**:
- Two concurrent `NativeBrowser::launch(Some(dir))` calls against the same real, existing
  `dir` — the second returns an `Err` whose message contains `"appears to be in use"` and
  `"SingletonLock"`.
  - *Given* a first `NativeBrowser::launch(Some(PathBuf::from(dir)))` call has completed
    and its returned `NativeBrowser` is still held (not dropped), *When* a second
    `NativeBrowser::launch(Some(PathBuf::from(dir)))` call is made against the same `dir`,
    *Then* the second call returns `Err(PortError::Other(msg))` where `msg` contains
    `"appears to be in use"` and `"SingletonLock"`.
**Files**: `crates/cli/tests/browser_profile_persistence.rs`

##### Task 1.3.2a: Write the `#[ignore]` SingletonLock collision integration test (~5 min)
- In `crates/cli/tests/browser_profile_persistence.rs` (created by Task 1.3.1a), add a
  second `#[tokio::test]` `#[ignore]` test (needs its own `LocalSet`, mirroring
  `browser_session.rs`'s `navigate_concurrent_should_not_exceed_max_open_sessions` pattern
  at lines 1000-1005, since `NativeBrowser::launch` spawns a `!Send` reaper via
  `spawn_local`): create one `tempfile::tempdir()`, call
  `NativeBrowser::launch(Some(dir.path().to_path_buf())).await` and hold the result in a
  binding that stays alive for the test's duration, then call
  `NativeBrowser::launch(Some(dir.path().to_path_buf())).await` again and assert the
  second call's `Err` message contains `"appears to be in use"` and `"SingletonLock"`.
- Files: `crates/cli/tests/browser_profile_persistence.rs`

### Epic 1.4: Documentation
**Goal**: Make the opt-in discoverable and its hazards explicit, following this repo's
existing `BRAVE_API_KEY` documentation contract exactly.

#### Story 1.4.1: Document `STAPLER_MCP_BROWSER_PROFILE_DIR`
**As a** daemon operator, **I want** the README to tell me how to opt in and what I'm
trading off, **so that** I don't accidentally create a standing credential leak or commit
a profile directory to git.
**Acceptance Criteria**:
- README documents the env var with the same "must be set in the daemon's actual
  environment" contract `BRAVE_API_KEY` already uses, plus an explicit warning against
  git-repo and sync-folder (Dropbox/iCloud/Syncthing) paths, plus a one-line note that a
  profile corrupted by a Chrome version change should be deleted and let Chrome recreate
  it, plus a note that this does not isolate multiple logged-in identities (out of scope).
  - *Given* the merged README, *When* a reader searches for
    `STAPLER_MCP_BROWSER_PROFILE_DIR`, *Then* they find a row in the Tools/State-layout
    area explaining: default is ephemeral; setting the var to a non-empty absolute path
    makes it persistent; the var must be set in the **daemon's** environment (same
    caveat as `BRAVE_API_KEY`, `README.md:150-152`); don't point it at a git repo or a
    cloud-synced folder; corruption after a Chrome upgrade is fixed by deleting the
    directory.
- The example value shown is a literal absolute path, not `~/.stapler-mcp/browser-profile`
  (pre-mortem #1), and the section points the reader at the `stapler_daemon_status` MCP tool
  and the `stapler-mcp --status` CLI flag (Story 1.2.4, both backed by the
  `browserProfileMode` field Story 1.2.3 added) as the way to verify the opt-in actually
  took effect, rather than inferring it from cookie behavior (pre-mortem #2).
  - *Given* the merged README's new subsection, *When* a reader looks for a copy-pasteable
    example, *Then* it reads as a literal absolute path (e.g.
    `/home/alice/.stapler-mcp/browser-profile`) with a substitute-your-home-dir note, and a
    separate line names both the `stapler_daemon_status` tool (for an LLM agent/MCP client)
    and `stapler-mcp --status` (for a human operator at a terminal) as the way to confirm
    persistence is active.
**Files**: `README.md`

##### Task 1.4.1a: Add the README section (~6 min)
- In `README.md`, add one row to the `## Tools implemented` table's
  `stapler_browser_navigate` entry (around line 96) noting the new opt-in exists (one
  clause, pointing at the State layout section for detail), **and add a new, separate row
  to the same table for `stapler_daemon_status` itself** (native-only — see Unresolved
  Questions for why it isn't in the wasm/npm distribution), matching every other MCP tool
  in that table having its own row: something like "Report daemon-wide status not tied to
  any one browser session — currently the active browser profile persistence mode and any
  unsafe-location warning (Story 1.2.4). Diagnostic-only; also reachable from a terminal via
  `stapler-mcp --status`." Then add a new subsection
  under `### State layout` (after line 164's bullet list) titled something like `#### Persistent browser profile (opt-in)` covering: the env var name, the literal-path
  (absolute-path-only) requirement (no default), the "must be set in the daemon's
  environment" callout mirrored from the `BRAVE_API_KEY` paragraph at lines 150-153, the
  git-repo/sync-folder warning (noting the daemon also warns — but does not block — at
  startup if it detects this, per Task 1.2.1c), and the "delete and let it recreate"
  corruption-recovery note.
- The example value in this new subsection must be a **literal absolute path** (e.g.
  `/home/alice/.stapler-mcp/browser-profile`), with a one-line note directing the reader to
  substitute their own home directory — not `~/.stapler-mcp/browser-profile`. Add a
  one-line callout immediately after the example: `~` at the start of the value *is*
  expanded by `browser_profile_dir` itself (Task 1.1.1a) if typed, but any other
  non-absolute value (a shell variable reference, a relative path) is still rejected and
  falls back to ephemeral mode — so the literal absolute form is the reliable, recommended
  way to write it, especially since most MCP clients set this var via a JSON `env` block
  that never runs shell expansion at all.
- Alongside the corruption-recovery note, add a one-line caveat: the `0700` permission
  hardening (Task 1.2.1e) is applied to the profile directory itself, not recursively — a
  pre-existing directory populated by another process under a looser umask keeps its old
  file-level permissions; delete and let Chrome recreate the directory if you need a clean
  hardened profile.
- Add a one-line pointer to the new diagnostic surface (Story 1.2.4): "to confirm
  persistence actually took effect, run `stapler-mcp --status` (or, from an MCP client/LLM
  agent, call the `stapler_daemon_status` tool) and check the reported browser profile mode
  (`\"ephemeral\"` or `\"persistent at <path>\"`) rather than inferring it from whether
  cookies survived a restart."
- Files: `README.md`
