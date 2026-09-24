# Architecture research: persistent browser profile / user-data-dir reuse

item_id: 10f6836c-71fc-4db2-8bc8-117d65da6a2d

## 1. Where the opt-in surface belongs: daemon config, not a per-call parameter

`crates/cli/src/main.rs` has no CLI-flag framework at all — no `clap`, no `Parser`
derive. `main()` (`crates/cli/src/main.rs:25`) only checks
`std::env::args().any(|a| a == "--daemon")` for mode selection. The one piece of
runtime-tunable daemon config that exists today, `BRAVE_API_KEY` /
`BRAVE_API_BASE_URL` (`crates/cli/src/main.rs:122-124`), is read via raw
`std::env::var(...).unwrap_or_default()` inline inside the tool-handler closure at
the point of use, not centralized or passed through a struct.

Separately, and more relevant, `crates/core/src/paths.rs` already defines the
project's actual convention for daemon state locations: `base_dir(env)` plus derived
paths (`cache_dir`, `docs_index_dir`, `embedding_cache_dir`,
`crates/core/src/paths.rs:9-40`), all built through the `EnvPort` trait
(`crates/core/src/ports.rs:105-108`: `var()` / `home_dir()`) so they're mockable in
tests (see the mock `EnvPort` impl and tests at the bottom of `paths.rs`). This is
the idiomatic seam for anything "where does the daemon keep its durable state,"
and it already has default-vs-override behavior (home-dir default, env override) and
test coverage for exactly that pattern.

**Recommendation:** add a `paths::browser_profile_dir(env)` (mirroring
`docs_index_dir`/`embedding_cache_dir`) read through `EnvPort`, opt-in via an env var
(e.g. `STAPLER_BROWSER_PROFILE_DIR`, matching the existing `STAPLER_*` / `BRAVE_*`
naming already in use) rather than a bespoke `--user-data-dir` CLI flag — introducing
a flag-parsing framework for one flag is disproportionate when the repo has none, and
env var fits the existing config-flow pattern better than a new CLI arg surface would.
Read the var once in `run_daemon()` (`crates/cli/src/main.rs:60`) before calling
`NativeBrowser::launch()`, not inside `launch()` itself — `main.rs` is already the
seam that resolves `paths::*` via `NativeEnv` (`crates/cli/src/main.rs:61-64`) before
handing resolved values downstream, so this keeps `launch()`'s dependency on
`EnvPort` where the rest of the config resolution already happens, and keeps
`browser.rs` decoupled from `std::env` (it currently has zero env-var reads).

This directly answers the requirements doc's framing: the literal proposal (a
`user_data_dir` parameter on `stapler_browser_navigate`) doesn't fit because
`user_data_dir` is consumed exactly once, at `Browser::launch()`
(`crates/native/src/browser.rs:354-360`), before any session/tab exists — by the time
a `navigate()` call happens, the Chrome process and its profile are already fixed.
The right opt-in surface is daemon-launch-time config, either a CLI flag or an env
var; given the absence of any flag-parsing today, env var is the smaller diff.

## 2. `BrowserDriver` port trait: no construction method exists — don't thread `Option<PathBuf>` through it

`BrowserDriver` (`crates/core/src/ports.rs:252-303+`) only defines post-construction
operations: `navigate_and_extract`, `navigate`, `click`, `type_text`, `snapshot`,
`close_session`, `list_sessions`, etc. There is no `launch()`/`new()`/constructor
method on the trait at all — construction is entirely outside the port abstraction.
`NativeBrowser::launch()` is an inherent `impl NativeBrowser` method
(`crates/native/src/browser.rs:337-377`), called directly by name in
`crates/cli/src/main.rs:88`, not through any trait-object indirection.

Since the trait was never designed to model "how do you build one of these,"
adding a profile-dir parameter has nothing to plug into on the trait side — it's a
constructor-argument problem for the native adapter specifically, not a
port-interface change. Threading `Option<PathBuf>` through `ports.rs` would be
speculative generality with no current second implementation to justify it.

**wasm target has no equivalent concept.** `crates/wasm/src/browser.rs` doesn't
launch a Chrome subprocess at all — every `BrowserDriver` method delegates to a
`#[wasm_bindgen]` JS glue function (`jsBrowserNavigate`, `jsBrowserClick`, etc.,
`crates/wasm/src/browser.rs:13-60+`) running inside whatever browser/JS host already
exists. There's no `Browser::launch()`, no `user_data_dir`, no separate Chrome
process for the wasm adapter to configure — the "profile" is just whatever cookie
jar the host browser/context already has. This confirms the item's own scope note:
persistent-profile config is inherently native-only and belongs in
`crates/native`/`crates/cli`, not in `crates/core` where both adapters would have to
implement it.

## 3. `MAX_OPEN_SESSIONS`, the reaper, and crash/`blocked` handling are orthogonal

Checked directly in `crates/native/src/browser.rs`:

- `MAX_OPEN_SESSIONS` (`browser.rs:42`) caps concurrent tabs (`sessions.len()`,
  guarded by `NewSessionSlotGuard`, `browser.rs:300-335`) within one already-running
  Chrome process. It has no relationship to which `user_data_dir` that process was
  launched with.
- The reaper (`spawn_reaper`, `browser.rs:275`) and `touch_or_evict`
  (`browser.rs:220`) operate on `BrowserSession` entries (tabs/`Page`s) — idle
  eviction closes a tab, never touches the profile directory or the `Browser`
  handle itself.
- `blocked` state (`browser.rs:154`, `poll_blocked_grace_period`, `browser.rs:941`)
  tracks a per-session SSRF-guard rejection (navigated to a disallowed host), purely
  a per-tab/per-navigation concern.

All three operate strictly below the `Browser`-process level; profile persistence is
a property of the one `Browser` object created once in `launch()`. None of them read
or need to read `user_data_dir`. No integration point, no change needed to any of
the three for this item.

## 4. Data-flow / consistency guarantees

- **Directory must exist and be writable before `Browser::launch()`.** The existing
  ephemeral path already does this: `std::fs::create_dir_all(&user_data_dir)`
  (`browser.rs:352`) runs before `BrowserConfig::builder().user_data_dir(...)`
  (`browser.rs:354-357`). A persistent-dir variant needs the same
  `create_dir_all` (now idempotent — the dir may already exist from a prior daemon
  run) before the `Browser::launch()` call, and should surface a clear
  `PortError::Other` (same pattern as the existing `.map_err` at `browser.rs:352`
  and `:357`) if the directory can't be created or isn't writable (e.g. wrong
  permissions, path points at a file). No new error variant needed — existing
  `PortError::Other(String)` covers it.
- **Two daemons pointed at the same persistent dir concurrently.** The existing code
  comment at `browser.rs:339-346` already documents *why* per-pid/timestamp temp dirs
  exist: Chrome's own `SingletonLock` inside `user_data_dir` means a second process
  pointed at an already-locked profile dir fails to launch rather than corrupting
  state. That failure surfaces through the same `Browser::launch(config).await`
  `.map_err(...)` at `browser.rs:358-360` as any other launch failure, and
  `run_daemon()` already exits non-zero with a printed error on launch failure
  (`crates/cli/src/main.rs:88-94`). So concurrent daemons on one persistent dir fail
  loudly and safely today, for free — no new locking/coordination logic needed. This
  is also consistent with the daemon's existing single-instance-per-machine
  assumption: `run_daemon()` already takes an exclusive flock via `NativeLock`
  (`crates/cli/src/main.rs:71-83`) before doing anything else, so two daemons
  racing for the *same* `base_dir` (and thus, if the profile dir defaults under
  `base_dir`, the same profile) is already a scenario the daemon partially guards
  against — though that lock is per-`base_dir`, not per-profile-dir, so a user who
  points two daemons (e.g. two different `base_dir`s via `STAPLER_MCP_HOME` or
  similar) at the *same explicit* `STAPLER_BROWSER_PROFILE_DIR` isn't caught by the
  flock and would rely on Chrome's `SingletonLock` failure alone. Worth a one-line
  doc note on the env var, not new code.
- **Restart timing:** since `user_data_dir` is consumed once at process start and
  never touched again during the daemon's life (confirmed — no other reference to
  `user_data_dir` outside `launch()` in `browser.rs`), there's no mid-run
  consistency concern (no concurrent writers to reason about beyond Chrome itself
  managing its own profile files, which it already does for the ephemeral case).

## Tech Debt Disposition

`crates/native/src/browser.rs`'s `NativeBrowser::launch()` is fine to extend as-is —
it's a short, single-purpose function (~40 lines) with the relevant constraint
already documented in its own comments (the `SingletonLock` rationale at
`browser.rs:339-346`), and the change is additive (branch on whether a persistent
dir was supplied, same `create_dir_all` + `BrowserConfig::builder()` shape either
way). No refactor-first or seam-isolation work is warranted before making this
change.
