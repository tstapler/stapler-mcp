# Implementation Plan: thin-client-binary-trim

**Feature**: Split the per-session `stapler-mcp` thin client from the shared `stapler-mcp-daemon` so the thin client no longer links `chromiumoxide`, `fastembed` or `reqwest`, and size-tune it.
**Date**: 2026-10-18
**Status**: Ready for implementation (Phase 1 already implemented in the working tree, uncommitted; remaining work is Phases 2-3). Complexity 2.
**ADRs**: [ADR-001](../decisions/ADR-001-release-thin-named-profile-for-lto.md) (AC3 via named profile), [ADR-002](../decisions/ADR-002-thin-client-execs-daemon-on-legacy-flag.md) (`--daemon` exec), [ADR-003](../decisions/ADR-003-npm-packaging-interpretation-of-ac7.md) (AC7 interpretation, Proposed)

Task status legend: **DONE** = present in the working tree as of 2026-10-18 (read, not yet re-verified by a gate; Story 3.1.1 re-verifies), **TODO** = remaining.

Inputs: `requirements.md`, `research/{stack,architecture,pitfalls,build-vs-buy}.md`. `features.md` and `ux.md` were intentionally skipped (no user-facing surface; the MCP tool surface is unchanged).

## Alternatives considered (Step 0.5 creative pass)

| Approach | Strength | Weakness | Verdict |
|---|---|---|---|
| A. Crate split (`crates/thin-client`) + default-on `heavy` feature in `stapler-mcp-native` + named `release-thin` profile | Removes the heavy crates from the thin client's dependency graph, which is deterministic and checkable with `cargo tree`; no protocol or daemon change | Feature unification: any invocation that selects both packages relinks heavy into the thin client | **Chosen** |
| B. Second `[[bin]]` in `crates/cli` | One crate, fewer manifests | Both bins share one dependency declaration, so `cargo tree -p stapler-mcp` would still show heavy crates; AC1 unreachable | Rejected |
| C. Blocking `std` relay or byte-pipe thin client, no rmcp/tokio | Likely drops most of the remaining ~2.7 MB and removes the async runtime | Reimplements an MCP subset (or changes the daemon, a non-goal); needs a differential-test harness; correctness long tail | Deferred (follow-up, goal 4 evaluated not built) |

## Requirements traceability

| AC | Satisfied by | Verified by |
|---|---|---|
| AC1 `cargo tree` clean | Stories 1.1.1, 1.1.2 | Task 3.1.1c, Task 3.1.2a/b (CI) |
| AC2 daemon builds, `cargo test --workspace` passes | Story 1.1.3 | Task 3.1.1b, Task 3.1.2b |
| AC3 release profile | Story 1.1.4, ADR-001 (deviation: `lto` via named profile) | Task 3.1.1b |
| AC4 >= 20% smaller | Stories 1.1.1-1.1.4 | Task 3.1.1a/b, Task 3.1.2a (size gate) |
| AC5 `heavy` feature | Story 1.1.1, Story 1.1.2 | Task 1.1.1c, Task 3.1.1c |
| AC6 auto-start via sibling `exe_hint` | Story 2.1.1 | Story 2.1.2 |
| AC7 npm | Story 3.2.2, ADR-003 (OWNER DECISION) | Task 3.2.2a |

---

## Domain Glossary
*(Names are used verbatim in code, tests, scripts and comments.)*

| Term | Definition | Notes |
|---|---|---|
| `thin client` | The per-session `stapler-mcp` process: stdio MCP server that forwards each tool call to the daemon over a Unix socket | Package and bin `stapler-mcp`, `crates/thin-client` |
| `daemon` | The shared long-running process owning browsers, embedder and HTTP; one per machine | Package and bin `stapler-mcp-daemon`, `crates/cli` |
| `heavy` | Default-on Cargo feature of `stapler-mcp-native` gating the modules `ax`, `browser`, `embed`, `http` (`NativeBrowser`, `NativeEmbedder`, `NativeHttp`) and the optional deps `chromiumoxide`, `fastembed`, `reqwest`, `url` | Default-on, so the daemon needs no flag change beyond declaring it; thin client uses `default-features = false` |
| `light adapters` | `NativeEnv`, `NativeSocketFactory`, `NativeSpawner`, `NativeSleeper`, `NativeClock` (plus `NativeFs`, `NativeLock`), available without `heavy` | |
| `release-thin` | Named Cargo profile (inherits `release`; `lto = "fat"`, `opt-level = "z"`, `codegen-units = 1`, `strip = true`) used to build the shipped thin client | Output in `target/release-thin/` (ADR-001) |
| `DAEMON_BIN` | Constant `"stapler-mcp-daemon"` in `thin_client.rs` | |
| `sibling_daemon_path` | Pure function: `Option<&Path>` (current exe) -> `String` path of `DAEMON_BIN` beside it if it is a file, else the bare name | Unit-tested; gains canonicalization (Task 2.1.1a) |
| `daemon_exe_path` | `sibling_daemon_path(current_exe())`; the value placed in `EnsureOptions.exe_hint` and used by the `--daemon` exec | |
| `exe_hint` | `EnsureOptions` field naming the executable `NativeSpawner::spawn_daemon` runs with `--daemon`; `None` falls back to `current_exe()` (wrong for the thin client) | Never `None` natively |
| `LaunchMode` | Enum `{ ThinClient, ExecDaemon }` returned by `launch_mode(args)`; `ExecDaemon` iff any arg is exactly `--daemon` | Sum type replacing a bare bool (new, Task 2.1.1b) |
| `heavy denylist` | Exact crate names that must not appear in the thin client's normal graph or binary: `chromiumoxide`, `fastembed`, `reqwest`, `hyper`, `ort`, `tokio-rustls`, `rustls` | Match whole names, not substrings (`ort ` matches `dtoa-short`) |
| `pre-split baseline` | Stripped `stapler-mcp` built from `main` at `628ec11`: 48,517,136 B | AC4 reference; rebuilt in Task 3.1.1a |
| `feature unification` | Cargo unions features for all packages selected in one invocation, so `-p stapler-mcp -p stapler-mcp-daemon` or `--workspace` links heavy into the thin client | Drives rule: thin-client verification and release builds use `-p stapler-mcp` alone |
| `legacy invocation` | `stapler-mcp --daemon` as used by pre-split service units/scripts/old thin clients | Handled by ADR-002 |

---

## Pattern Decisions

| Component | Pattern Chosen | Source | Alternative Rejected | Reason |
|---|---|---|---|---|
| Heavy adapters isolation | Compile-time seam via Cargo feature (`heavy`) | Cargo features; Ports-and-Adapters | Second bin in same crate (Approach B) | Shared dependency declaration keeps heavy crates in `cargo tree` |
| Daemon discovery | Pure function `sibling_daemon_path` with injected current-exe | Functional core / imperative shell | Read `current_exe()` inside the spawner | Testable without spawning; `exe_hint` already a port parameter |
| Launch mode | Sum type `LaunchMode` + `launch_mode(&[String])` | type-driven-design | `bool` or ad-hoc `args.contains` in `main` | Exhaustive `match`; unit-testable; extensible if a third mode appears |
| Legacy `--daemon` | exec the sibling (ADR-002) | Adapter / forwarding stub | Reject with error; ignore flag | Keeps drop-in constraint; breaks no service unit |
| Size tuning | Named Cargo profile (ADR-001) | Cargo profiles | Workspace-wide LTO; build-std; UPX | `lto` not per-package; daemon unchanged; UPX/build-std rejected in build-vs-buy |
| Thin-client test access to `ThinClient` | Add a `[lib]` target; `tests/tool_schema.rs` and `tests/daemon_exec.rs` import `stapler_mcp::thin_client` | pragmatic | Keep `#[path]` include | Superseded: `[lib]` was needed to test `daemon_locator` and dropped the dead-code hack |
| Verification | Executable gate script (`scripts/check-thin-client.sh`) run locally and by CI | Fitness function | Prose checklist | Regression of feature unification is silent otherwise |
| Domain/wire types | No new newtypes: only `String` paths and constants cross boundaries | PoEAA Transaction Script is enough | Domain Model | No domain logic added; this is build/packaging work |

---

## Tech Debt Disposition

| Area | Existing Issue | Disposition | Justification |
|---|---|---|---|
| `crates/native/src/lib.rs` (30 lines) | Flat `mod`/`pub use` list, now `#[cfg(feature = "heavy")]` per gated module | Extend as-is | It is the seam; no logic; not a hotspot (`research/architecture.md` section 4) |
| `crates/cli/src/main.rs` (466 lines) | Long `run_daemon` registration; now daemon-only | Extend as-is (no refactor) | Requirements forbid changing daemon behaviour; splitting handler registration is a separate refactor not needed for any AC |
| `crates/thin-client/src/thin_client.rs` | 27 `#[tool]` handlers, moved verbatim; gains `daemon_exe_path` hardening | Extend as-is plus tests | Moved, not rewritten; additions are a small pure function with tests |
| `tests/tool_schema.rs` `#[path]` include | Compiles `thin_client.rs` twice | Extend as-is | Works; revisit only on friction (`[lib]` is the alternative) |
| `crates/core/src/protocol.rs` `ping` | Liveness only; no version handshake, so a stale daemon survives upgrade | Isolate by documenting; fix is a follow-up | Fix edits the daemon, a non-goal (see Follow-ups) |

---

## Migration Plan
Not applicable: no schema or data changes. The only on-disk compatibility surface is the Unix socket path and wire protocol, both unchanged.

## Observability Plan
- **Logs**: no new service boundaries. Two new stderr messages from the thin client, both single-line and prefixed `stapler-mcp:`: the failed-exec message (ADR-002) and the existing `ensure daemon: ...` error, extended with `(looked for stapler-mcp-daemon beside <exe>, then PATH)`. Daemon stdout/stderr keep going to `daemon.log` via the existing spawner.
- **Metrics**: none. Binary size is tracked by the CI size gate (Task 3.1.2a), not a runtime metric.
- **Alerts**: no new alerts required.

## Risk Control
- **Feature flag**: not gated. `heavy` is a compile-time feature (default on), not a rollout flag.
- **Rollback procedure**: standard revert via PR close + revert commit. Reverting restores the single combined binary; no data migration.
- **Staged rollout**: full rollout on merge. Known upgrade hazard (stale running daemon) is documented as a follow-up, not mitigated here.

## Unresolved Questions
- [ ] AC7 interpretation: confirm ADR-003 option 1 (npm stays WASM-only; verify no regression; document native two-binary layout), or choose option 2/3 (new scope) — blocks closing Story 3.2.2 only — owner: Tyler.
- [ ] Confirm ADR-001 deviation (AC3 `lto = "fat"` delivered by `release-thin`, not per-package) is acceptable as the reading of AC3 — blocks nothing in the code; needed at PR review — owner: Tyler.
- [ ] Adopt `panic = "abort"` in `release-thin` if the spike (Task 2.2.1b) shows a measurable win and all tests pass? Resolved by measurement; owner is informed in the PR, and only vetoes — owner: Tyler.
- [ ] Stale-daemon-after-upgrade policy (handshake, restart, version-keyed socket) — blocks nothing here; needs the "do not modify the daemon" constraint relaxed — owner: Tyler (follow-up).

## Dependency Visualization

```
Phase 1 (DONE except 1.1.1c)
  1.1.1a,b -> 1.1.1c ----------------------------------------------+
  1.1.2a-c, 1.1.3a-b, 1.1.4a-b                                      |
                                                                    v
Wave 1:  2.1.1a -> 2.1.1b -> 2.1.1c        3.1.1a (baseline build, independent)
            |                                       |
Wave 2:     +--> 2.1.2a, 2.1.2b            2.2.1a -> 2.2.1b   (spike; touches Cargo.toml only)
            |            |                          |
Wave 3:     |            +--------+-----------------+
            |                     v
            |          3.1.2a (script) -> 3.1.2b (ci.yml)      3.2.1a -> 3.2.1b     3.2.2a -> 3.2.2b
            |                     |                                 |                     |   (3.2.2b waits on OWNER DECISION)
Wave 4:     +---------------------+---> 3.1.1b -> 3.1.1c (final measured gate, after profile is final)
```

---

## Review resolutions (Phase 3 repair pass)

Adversarial review: CONCERNS (0 blockers). Architecture review: CONCERNS (0 blockers). Folded in:

- **Size gate:** ceiling in `scripts/check-thin-client.sh` is 8,000,000 B (about 3x the measured 2,733,624 B release-thin size), not `0.8 x` baseline; the 20% AC4 claim is reported separately from the baseline.
- **Feature graph check:** `scripts/check-thin-client.sh` also asserts `heavy` is absent from `cargo tree -e features -p stapler-mcp`, always invoked with `-p stapler-mcp` alone (feature unification).
- **CI:** the fat-LTO build and the script run in their own job, not the `native` job.
- **Daemon discovery:** lives in `crates/thin-client/src/daemon_locator.rs`, not the handler file; the `exe_hint = None` trap is guarded there. Use `args_os` for the `--daemon` check; exec path is `#[cfg(unix)]`.
- **`strip = true` is workspace-wide:** scope it to `release-thin` and the thin-client package; record in ADR-001 that the daemon release profile is unchanged.
- **Task 2.1.2b test coupling:** the test builds/locates the thin client explicitly and fails if the sibling is missing or older than the daemon binary.
- **Task 2.2.1b:** drop the trial that gates tokio `fs` (breaks the ungated `NativeFs`).
- **Task 1.1.1c:** `crates/native/tests/` does not exist; the check is a `cargo build/clippy -p stapler-mcp-native --no-default-features`.
- **AC7 / ADR-003:** remains an owner decision; the PR states AC7 was reinterpreted (npm is WASM-only). Docs add a restart-the-daemon-after-upgrade note; the version-handshake fix stays a follow-up.
- **macOS `current_exe` symlink behavior:** INFERRED, untested in CI; stated as such in docs.

## Pre-mortem resolutions (Phase 4 repair pass)

- **P1 install contract:** new Story 3.2.3. One entry point (`scripts/install.sh`, wired to `make install`) runs `cargo install --path crates/thin-client` then `--path crates/cli` into the same bin dir (`cargo install` takes one package per call). Acceptance: clean `cargo install --root <tmp>` drives one tool call and the daemon auto-starts; replacing only the thin client or only the daemon is tested; the missing-sibling error names the install command. The external dotfiles `.config/mcp/mcp-servers.json` install note is out of this repo: tracked as a follow-up for the owner, not edited here.
- **P1 feature unification in default builds:** `default-members` becomes the thin client plus core/native only (the daemon is built with `-p stapler-mcp-daemon` or `--workspace`); `scripts/check-thin-client.sh` also builds with repo-root default selection and runs the size ceiling plus denylist string scan; README build guidance updated; CI tests stay on `--workspace`.
- **P2 AC6 proof:** add an MCP-level test that issues a real `tools/call` through the thin client and verifies the running daemon process is the `stapler-mcp-daemon` binary (not a respawned thin client).
- **P2 wasm glue:** `crates/wasm/src/glue/process.js` `findNativeBinary` should prefer `stapler-mcp-daemon` for `--daemon`; resolve ADR-003 before merging.
- **P2 stale daemon:** README documents the manual restart; open the follow-up issue before merge.
- **strip scope:** `strip = true` moves out of workspace `[profile.release]` into `release-thin` only; the daemon release profile is unchanged (reconciles Story 1.1.4).

## Phase 1: Split and gate the dependency graph (implemented in working tree)

### Epic 1.1: Thin-client crate, `heavy` feature, release profiles
**Goal**: `cargo tree -e normal -p stapler-mcp` is free of the heavy denylist and the stripped thin client is far smaller than the pre-split binary, while the daemon is unchanged.

#### Story 1.1.1: `heavy` feature in `stapler-mcp-native` (AC5)
**As a** maintainer, **I want** the browser, embedder and HTTP adapters behind a default-on feature, **so that** consumers can opt out of their dependency trees.
**Acceptance Criteria**:
- `crates/native/Cargo.toml` has `default = ["heavy"]` and `heavy = ["dep:chromiumoxide", "dep:fastembed", "dep:reqwest", "dep:url"]`.
  - *Given* the `stapler-mcp-native` package, *When* `cargo tree -e normal -p stapler-mcp-native --no-default-features --prefix none | awk '{print $1}' | grep -cxE 'chromiumoxide|fastembed|reqwest'` runs, *Then* the count is 0, and with default features the same pipeline prints a count of at least 3.
- The crate compiles, lints and tests clean in both configurations.
  - *Given* `NativeBrowser` is gated, *When* `cargo clippy -p stapler-mcp-native --no-default-features --all-targets -- -D warnings` runs, *Then* exit code is 0 with no `unused_imports`/`dead_code` from helpers that only the gated modules use.
**Files**: `crates/native/Cargo.toml`, `crates/native/src/lib.rs`, `crates/native/src/*.rs`, `crates/native/tests/*`

##### Task 1.1.1a: Feature and optional deps in `crates/native/Cargo.toml` **DONE**
- `[features] default = ["heavy"]`, `heavy = [...]`; `chromiumoxide`, `fastembed`, `reqwest`, `url` marked `optional = true`.
- Files: `crates/native/Cargo.toml`

##### Task 1.1.1b: Gate modules and re-exports in `crates/native/src/lib.rs` **DONE**
- `#[cfg(feature = "heavy")]` on `ax`, `browser`, `embed`, `http` modules and their `pub use`.
- Files: `crates/native/src/lib.rs`

##### Task 1.1.1c: cfg hygiene audit for the no-`heavy` build (XS) **TODO**
- Confirm ungated modules (`env`, `fs`, `lock`, `sleep`, `spawn`, others) do not `use` gated modules, `url::`, `reqwest::` or `chromiumoxide::`; gate per-variant any error enum variant or `From` impl naming a gated type.
- Gate integration tests under `crates/native/tests/` that import `NativeBrowser`/`NativeHttp`/`NativeEmbedder` with `#![cfg(feature = "heavy")]` or `[[test]] required-features = ["heavy"]`; ensure test code in ungated modules does not use now-optional deps.
- Record expected test counts per configuration: `cargo test -p stapler-mcp-native [--no-default-features] -- --list | grep -c ': test$'` (the no-default count will be lower; write both numbers in the PR).
- Check `[lints.rust] unexpected_cfgs` is not suppressed so a misspelled `feature = "heavey"` is caught.
- Check whether non-heavy code uses `tokio::fs`/`tokio::process`; make those tokio features conditional on `heavy` only if no ungated code needs them (`NativeSpawner` needs `process`; expect no change).
- Files: `crates/native/src/*.rs`, `crates/native/tests/*`, `crates/native/Cargo.toml`

#### Story 1.1.2: `crates/thin-client` package (AC1, AC5)
**As a** Claude Code user running ~30 sessions, **I want** the per-session process built from a crate that cannot link the browser or ML stack, **so that** the artifact is small and has less attack surface.
**Acceptance Criteria**:
- `cargo tree -e normal -p stapler-mcp` lists none of the heavy denylist.
  - *Given* `crates/thin-client/Cargo.toml` depends on `stapler-mcp-native = { path = "../native", default-features = false }`, *When* `cargo tree -e normal -p stapler-mcp --prefix none | awk '{print $1}' | grep -xE 'chromiumoxide|fastembed|reqwest|hyper|ort|tokio-rustls|rustls'` runs, *Then* it prints nothing (exit 1 from grep).
- The thin client's tool schema test still passes.
  - *Given* `crates/thin-client/tests/tool_schema.rs` including `src/thin_client.rs` via `#[path]`, *When* `cargo test -p stapler-mcp --test tool_schema` runs, *Then* it passes and asserts the same 27 registered tools as before the move.
**Files**: `crates/thin-client/Cargo.toml`, `crates/thin-client/src/main.rs`, `crates/thin-client/src/thin_client.rs`, `crates/thin-client/tests/tool_schema.rs`, `Cargo.toml`, `Cargo.lock`

##### Task 1.1.2a: Scaffold package `stapler-mcp` with bin `stapler-mcp` **DONE**
- `Cargo.toml` (path deps on core and native `default-features = false`; tokio features `rt, macros, time, net, io-util, sync`; rmcp 2 `server, macros, transport-io, schemars`), `src/main.rs` (current-thread runtime + `LocalSet`, serve over stdio).
- Files: `crates/thin-client/Cargo.toml`, `crates/thin-client/src/main.rs`

##### Task 1.1.2b: Move `thin_client.rs` and `tool_schema.rs` from `crates/cli` **DONE**
- `git mv`; add `DAEMON_BIN`, `daemon_exe_path`, `sibling_daemon_path` and the `exe_hint: Some(daemon_exe_path())` wiring in `call_daemon`; two unit tests (`prefers_daemon_beside_current_exe`, `falls_back_to_bare_name_when_no_sibling`) already exist in `thin_client.rs`.
- Files: `crates/thin-client/src/thin_client.rs`, `crates/thin-client/tests/tool_schema.rs`

##### Task 1.1.2c: Workspace membership and lockfile **DONE**
- Add `crates/thin-client` to `members` and `default-members`; `resolver = "2"` is set; `Cargo.lock` updated for the package rename.
- Files: `Cargo.toml`, `Cargo.lock`

#### Story 1.1.3: Daemon package `stapler-mcp-daemon` (AC2)
**As a** maintainer, **I want** `crates/cli` to be daemon-only with a distinct bin name, **so that** both binaries can coexist in `target/`.
**Acceptance Criteria**:
- The daemon builds with `heavy` and the full workspace passes tests.
  - *Given* `crates/cli/Cargo.toml` names package and bin `stapler-mcp-daemon` and requests `features = ["heavy"]`, *When* `cargo build --workspace && cargo test --workspace` runs, *Then* both succeed and `target/debug/stapler-mcp` and `target/debug/stapler-mcp-daemon` both exist.
**Files**: `crates/cli/Cargo.toml`, `crates/cli/src/main.rs`, `crates/cli/tests/{browser_session,daemon_ping,docs_index,webcrawl}.rs`

##### Task 1.1.3a: Rename package/bin; remove thin-client branch from `main.rs`; `features = ["heavy"]` **DONE**
- Files: `crates/cli/Cargo.toml`, `crates/cli/src/main.rs`

##### Task 1.1.3b: Point cli integration tests at `env!("CARGO_BIN_EXE_stapler-mcp-daemon")` **DONE**
- Files: `crates/cli/tests/browser_session.rs`, `crates/cli/tests/daemon_ping.rs`, `crates/cli/tests/docs_index.rs`, `crates/cli/tests/webcrawl.rs`

#### Story 1.1.4: Release profiles (AC3, AC4)
**As a** maintainer, **I want** size tuning confined to the thin client, **so that** the daemon build is unchanged.
**Acceptance Criteria**:
- `[profile.release] strip = true`; `[profile.release.package."stapler-mcp"]` has `opt-level = "z"`, `codegen-units = 1`; `[profile.release-thin]` has `lto = "fat"`, `opt-level = "z"`, `codegen-units = 1`, `strip = true` (deviation for `lto` recorded in ADR-001).
  - *Given* the root `Cargo.toml`, *When* `cargo build --profile release-thin -p stapler-mcp` runs, *Then* it produces `target/release-thin/stapler-mcp` of 2,733,624 B (measured; +/- toolchain drift), at least 20% below 48,517,136 B (threshold 38,813,708 B).
**Files**: `Cargo.toml`, `project_plans/thin-client-binary-trim/decisions/ADR-001-release-thin-named-profile-for-lto.md`

##### Task 1.1.4a: Add profiles to root `Cargo.toml` **DONE**
- Files: `Cargo.toml`

##### Task 1.1.4b: ADR-001 documenting the `lto` deviation **DONE** (written in this planning pass)
- Files: `project_plans/thin-client-binary-trim/decisions/ADR-001-release-thin-named-profile-for-lto.md`

---

## Phase 2: Drop-in compatibility and size follow-through

### Epic 2.1: Legacy `--daemon` handling and daemon discovery (AC6)
**Goal**: A thin client started as `stapler-mcp --daemon`, or auto-starting a daemon, always runs `stapler-mcp-daemon` from the same directory, and failure modes say so.

#### Story 2.1.1: Exec the sibling daemon on `--daemon`; harden discovery
**As a** user with an existing service unit running `stapler-mcp --daemon`, **I want** it to keep starting the daemon, **so that** the upgrade does not silently break it.
**Acceptance Criteria**:
- `stapler-mcp --daemon` becomes the daemon.
  - *Given* a directory holding the thin client and a `stapler-mcp-daemon` executable, *When* `<dir>/stapler-mcp --daemon --flag-x` runs, *Then* the process image becomes `<dir>/stapler-mcp-daemon` and receives argv `["--daemon", "--flag-x"]`.
- No `--daemon` argument leaves behavior unchanged.
  - *Given* argv `["stapler-mcp"]`, *When* `launch_mode` is evaluated, *Then* it returns `LaunchMode::ThinClient` and the stdio MCP server starts.
- A missing daemon gives a diagnosable error.
  - *Given* a directory with only `stapler-mcp` and an empty `PATH`, *When* `stapler-mcp --daemon` runs, *Then* stderr contains `cannot exec stapler-mcp-daemon (looked beside` and the exit code is 127.
- Symlinked installs resolve to the real directory.
  - *Given* `/tmp/x/bin/stapler-mcp` is a symlink to `/tmp/x/real/stapler-mcp` and `/tmp/x/real/stapler-mcp-daemon` exists, *When* `sibling_daemon_path` is given the symlink path, *Then* it returns `/tmp/x/real/stapler-mcp-daemon`.
**Files**: `crates/thin-client/src/main.rs`, `crates/thin-client/src/thin_client.rs`

##### Task 2.1.1a: Harden `sibling_daemon_path`/`daemon_exe_path` (S) **TODO**
- Canonicalize the current exe before `with_file_name` (macOS `current_exe` may not resolve symlinks; INFERRED), use `is_file()` not `exists()`, make `daemon_exe_path` and `sibling_daemon_path` `pub(crate)`, and extend the `ensure daemon: {e}` error with `(looked for stapler-mcp-daemon beside <exe>, then PATH)`.
- Add a comment that `exe_hint = None` must never be used natively (it would spawn another thin client).
- Files: `crates/thin-client/src/thin_client.rs`

##### Task 2.1.1b: `LaunchMode` and exec in `main()` (S) **TODO**
- `launch_mode(args: &[String]) -> LaunchMode` (`ExecDaemon` iff any arg `== "--daemon"`). In `main()` before building the runtime: on `ExecDaemon`, `Command::new(daemon_exe_path()).args(&args[1..]).exec()`; on error print the ADR-002 message to stderr and `exit(127)`. Add `mod` visibility as needed.
- Files: `crates/thin-client/src/main.rs`

##### Task 2.1.1c: Unit tests (XS) **TODO**
- `launch_mode` table test (`[]`, `["x"]`, `["--daemon"]`, `["x","--daemon"]`, `["--daemons"]` -> only the exact flag matches); `sibling_daemon_path` symlink-layout test and directory-named-`stapler-mcp-daemon` test (not a file -> bare-name fallback). Keep the two existing tests.
- Files: `crates/thin-client/src/thin_client.rs`, `crates/thin-client/src/main.rs`

#### Story 2.1.2: Integration proof of AC6
**As a** maintainer, **I want** tests that fail loudly if the thin client cannot find the sibling daemon, **so that** auto-start cannot regress unseen.
**Acceptance Criteria**:
- Thin client resolves and execs the sibling (fake daemon).
  - *Given* `tempdir/stapler-mcp` (a copy of `env!("CARGO_BIN_EXE_stapler-mcp")`) and `tempdir/stapler-mcp-daemon` (an executable `#!/bin/sh` script that writes its argv to `tempdir/argv.txt`), *When* `tempdir/stapler-mcp --daemon` runs, *Then* `argv.txt` contains `--daemon` and the test fails (does not skip) if the script is absent.
- Real two-binary end to end.
  - *Given* `STAPLER_MCP_HOME` set to a tempdir and `Path::new(env!("CARGO_BIN_EXE_stapler-mcp-daemon")).with_file_name("stapler-mcp")` existing, *When* that thin client is run with `--daemon`, *Then* `ping` over `<home>/daemon.sock` succeeds within 10 s (the `ensure_daemon` deadline) and the test panics with a message naming the missing binary if the thin client was not built.
- Existing daemon tests still pass with the renamed bin.
  - *Given* `crates/cli/tests/daemon_ping.rs` using `CARGO_BIN_EXE_stapler-mcp-daemon`, *When* `cargo test -p stapler-mcp-daemon --test daemon_ping` runs, *Then* it passes.
**Files**: `crates/thin-client/tests/daemon_exec.rs` (new), `crates/cli/tests/thin_exec.rs` (new)

##### Task 2.1.2a: `crates/thin-client/tests/daemon_exec.rs` with a fake daemon (M) **TODO**
- Cases: sibling present (copied-binary layout), symlink layout, sibling absent with empty `PATH` (exit 127 and message), `--daemon` forwarded verbatim. Use `tempfile` (already a dev-dependency); set the exec bit explicitly; do not add the daemon crate as a dev-dependency (it would break feature isolation for `cargo test -p stapler-mcp`).
- Files: `crates/thin-client/tests/daemon_exec.rs`

##### Task 2.1.2b: `crates/cli/tests/thin_exec.rs` real two-binary test (S) **TODO**
- Locate the thin client as the sibling of `CARGO_BIN_EXE_stapler-mcp-daemon`; assert it exists with a message ("run `cargo build --workspace` first; `cargo test -p stapler-mcp-daemon` alone does not build the thin client"); run `--daemon` with an isolated `STAPLER_MCP_HOME`; reuse the ping helper pattern from `daemon_ping.rs`; kill the child on drop.
- Files: `crates/cli/tests/thin_exec.rs`

### Epic 2.2: Size follow-through (evaluate, keep only measured wins)
**Goal**: Decide with measurements whether cheap further trims are worth keeping, and record why heavier options are deferred (requirements goal 4).

#### Story 2.2.1: Attribution and cheap-trim spike
**As a** maintainer, **I want** measured attribution of the remaining ~2.7 MB, **so that** further trimming is evidence-driven.
**Acceptance Criteria**:
- Attribution recorded.
  - *Given* `cargo-bloat` installed, *When* `cargo bloat --profile release-thin -p stapler-mcp --crates -n 15` runs, *Then* the top crates and sizes are captured in the PR description (expected leaders: `std`, `rmcp`, `tokio`, `schemars`/`serde`; INFERRED).
- A trim is kept only if it measurably helps and nothing regresses.
  - *Given* a candidate change (`rmcp` `default-features = false`; `panic = "abort"` in `release-thin`; conditional tokio `fs`), *When* `release-thin` is rebuilt and `cargo test --workspace` plus the Story 2.1.2 tests run, *Then* the change is kept iff the stripped size drops (e.g. below 2,733,624 B) and all tests pass; otherwise it is reverted and the measurement is noted.
**Files**: `Cargo.toml`, `crates/thin-client/Cargo.toml`, `crates/native/Cargo.toml`

##### Task 2.2.1a: Install `cargo-bloat` and record crate attribution (XS) **TODO**
- `brew install cargo-bloat` or `cargo install cargo-bloat`; run against `release-thin`; paste output into the PR (not a repo file).

##### Task 2.2.1b: Trial three trims, one at a time (M) **TODO**
- (1) `rmcp` `default-features = false` (drops `base64`; confirm with `cargo tree -e features`); (2) `panic = "abort"` in `[profile.release-thin]` only after confirming nothing relies on `catch_unwind` or tokio task `JoinError` panic isolation in the thin-client path (read `thin_client.rs`/`main.rs`); (3) tokio `fs` gating per Task 1.1.1c findings. Measure each with `stat -c %s`.
- Record rejected-for-now options with reason in the PR: blocking relay (Approach C/D; needs differential-test harness; D changes the daemon), `-Zbuild-std`, UPX (breaks macOS signing, raises RSS), issue #38.
- Files: `Cargo.toml`, `crates/thin-client/Cargo.toml`, `crates/native/Cargo.toml`

---

## Phase 3: Verification gates, CI and docs

### Epic 3.1: Measured verification and regression gates
**Goal**: Every AC claim is backed by a command run in this repo, and the two silent regressions (feature unification, size) are caught by CI.

Rules for every command in this epic: thin-client checks use `-p stapler-mcp` **alone** (feature unification); release artifacts are built with **separate** invocations, never `-p stapler-mcp -p stapler-mcp-daemon` and never `--workspace`.

#### Story 3.1.1: Measure and verify AC1-AC5
**As a** reviewer, **I want** reproducible numbers, **so that** I can trust the size and attack-surface claims.
**Acceptance Criteria**:
- Baseline is reproducible.
  - *Given* a worktree of `main` at `628ec11` (which may lack `strip = true`), *When* `CARGO_PROFILE_RELEASE_STRIP=true cargo build --release -p stapler-mcp` runs, *Then* `stat -c %s target/release/stapler-mcp` is 48,517,136 (or the remeasured value, recorded with `rustc -V`, target triple, `Cargo.lock` hash and the commit SHA).
- Size AC met under both like-for-like and shipped profiles.
  - *Given* the candidate built with `cargo build --release -p stapler-mcp` and with `cargo build --profile release-thin -p stapler-mcp`, *When* sizes are compared to the baseline, *Then* 3,657,192 B and 2,733,624 B (or remeasured) are both <= 38,813,708 B, and a gzip size is reported for each.
- Attack-surface evidence.
  - *Given* `target/release-thin/stapler-mcp`, *When* `strings -a target/release-thin/stapler-mcp | grep -ciE 'chromiumoxide|fastembed|reqwest|onnx'` runs, *Then* the count is 0, and the same command on the daemon binary (positive control) is greater than 0.
**Files**: none (measurements go in the PR description)

##### Task 3.1.1a: Rebuild the pre-split baseline from `main` (S) **TODO**
- `git worktree add` a scratch worktree at `628ec11`, build `CARGO_PROFILE_RELEASE_STRIP=true cargo build --release -p stapler-mcp` (same strip setting as the candidate), record size, `rustc -V`, triple, `Cargo.lock` hash. This is a cold build; run it in wave 1 in parallel with other work.

##### Task 3.1.1b: Candidate sizes and test matrix (S) **TODO**
- Build the three numbers (post-split `--release`, `release-thin`, daemon `--release`), `ls`-record, gzip sizes. Run `cargo test --workspace`, `cargo test -p stapler-mcp` (thin features only), `cargo test -p stapler-mcp-native --no-default-features`, and clippy in both configurations (`cargo clippy -p stapler-mcp-native --no-default-features --all-targets -- -D warnings`, `cargo clippy --workspace --all-targets -- -D warnings`). Record test counts per configuration.

##### Task 3.1.1c: AC1/AC5 graph checks (XS) **TODO**
- Run the exact-name `cargo tree` denylist command; `cargo tree -e features -p stapler-mcp -i stapler-mcp-native` (expect no `heavy`); `cargo tree -e normal -p stapler-mcp -p stapler-mcp-daemon` as the documented negative control showing unification.

#### Story 3.1.2: CI gates
**As a** maintainer, **I want** CI to fail if the thin client regains heavy deps or grows back, **so that** the win is permanent.
**Acceptance Criteria**:
- Denylist gate.
  - *Given* a PR adding `reqwest = "0.13"` to `crates/thin-client/Cargo.toml`, *When* `scripts/check-thin-client.sh` runs, *Then* it exits non-zero and prints `reqwest`.
- Size gate.
  - *Given* a `release-thin` thin client of 2,733,624 B, *When* the script compares to `0.8 * 48_517_136`, *Then* it exits 0; at 40,000,000 B it exits non-zero.
- Both feature configurations run in CI.
  - *Given* `.github/workflows/ci.yml`, *When* a PR touches `crates/native`, *Then* the `native` job runs clippy and tests for `-p stapler-mcp-native --no-default-features` as well as the existing `--workspace` steps.
**Files**: `scripts/check-thin-client.sh` (new, shared by CI and local use), `.github/workflows/ci.yml`

##### Task 3.1.2a: `scripts/check-thin-client.sh` (M) **TODO**
- Steps: (1) exact-name `cargo tree` denylist; (2) `cargo build --profile release-thin -p stapler-mcp`; (3) binary string scan with a positive control against a daemon build (skip the control with a flag in CI if the daemon build is too slow, but run it in the local verification of Task 3.1.1b); (4) size ceiling `0.8 * 48_517_136 = 38_813_708` (constant with a comment naming the baseline commit `628ec11`). `shellcheck`-clean.
- Files: `scripts/check-thin-client.sh`

##### Task 3.1.2b: Wire `ci.yml` (S) **TODO**
- In the `native` job add: `cargo check -p stapler-mcp`, `cargo clippy -p stapler-mcp-native --no-default-features --all-targets -- -D warnings`, `cargo test -p stapler-mcp-native --no-default-features`, `scripts/check-thin-client.sh`. Keep `cargo build --workspace` before `cargo test --workspace` so the Task 2.1.2b test finds both binaries. Leave the `wasm` job untouched.
- Files: `.github/workflows/ci.yml`

### Epic 3.2: Documentation and packaging (AC7)
**Goal**: Docs match the two-binary reality and AC7 is handled honestly.

#### Story 3.2.1: README matches the split
**As a** new contributor, **I want** README commands to build and run the right binary, **so that** I do not run a thin client expecting a daemon.
**Acceptance Criteria**:
- Stale references corrected.
  - *Given* README.md lines 24-26, 31-41, 133-139, 150-152, 169, *When* a reader follows the "Manual daemon inspection" command, *Then* it reads `cargo build -p stapler-mcp-daemon && ./target/debug/stapler-mcp-daemon` (or the legacy-compatible `stapler-mcp --daemon`), and the architecture diagram shows `stapler-mcp-daemon`.
- Build guidance explains the unification trap.
  - *Given* the README Development section, *When* it describes release builds, *Then* it says to run `cargo build --profile release-thin -p stapler-mcp` and `cargo build --release -p stapler-mcp-daemon` as two separate invocations, that `cargo build`/`--workspace` link `heavy` into the debug thin client, and that `cargo run` needs `-p` (two bin packages).
**Files**: `README.md`

##### Task 3.2.1a: Fix stale `--daemon`/diagram/dev references (S) **TODO**
- Document sibling lookup then `PATH` fallback and the `--daemon` exec compatibility (ADR-002) near line 152; note `BRAVE_API_KEY` is read from the environment of whichever process spawns the daemon.
- Files: `README.md`

##### Task 3.2.1b: Build/install guidance (XS) **TODO**
- Separate-invocation rule, `release-thin` output path (`target/release-thin/`), `cargo install --path crates/thin-client` and `cargo install --path crates/cli` (two commands, both bins land in `~/.cargo/bin` so the sibling lookup works), `cargo run -p ...`.
- Files: `README.md`

#### Story 3.2.2: npm packaging (AC7) — OWNER DECISION (ADR-003)
**As a** maintainer, **I want** the npm package confirmed unaffected, **so that** the split does not regress the WASM distribution.
**Acceptance Criteria**:
- Under ADR-003 option 1, npm builds and passes.
  - *Given* the CI `wasm` job steps, *When* `cargo build -p stapler-mcp-core -p stapler-mcp-wasm --target wasm32-unknown-unknown`, `wasm-bindgen`, and `npm test` run in `npm/`, *Then* all pass.
  - *Given* `npm/package.json` with `"files": ["bin", "pkg"]`, *When* `npm pack --dry-run` lists contents, *Then* only `bin/` and `pkg/` entries appear and no native executable is present.
- Native two-binary layout documented.
  - *Given* README.md, *When* a reader looks for native install steps, *Then* it states that `stapler-mcp` and `stapler-mcp-daemon` must sit in the same directory.
**Files**: `npm/package.json` (read only unless the owner picks option 2/3), `README.md`

##### Task 3.2.2a: Verify npm build/tests and package contents (S) **TODO**
- Run the `wasm` job steps locally (requires `wasm32-unknown-unknown`, `wasm-bindgen-cli 0.2.126`, Node 20) and `npm pack --dry-run`; capture the listing.
- Files: `npm/` (no edits)

##### Task 3.2.2b: Apply the owner's AC7 decision (XS for option 1) **TODO, blocked on owner decision**
- Option 1: README sentence plus PR note that AC7 was met as "no regression, native layout documented". Options 2/3 are out of this plan's scope and need their own plan.
- Files: `README.md`

---

## Follow-ups (recorded, not planned work)

1. **Stale daemon after upgrade**: `ping` is liveness-only (`crates/core/src/protocol.rs:9`), so a new thin client talks to an old running daemon. Fix options (version in `ping` + restart under `NativeLock`; exit on exe change; version-keyed socket) all modify the daemon, a non-goal here. Needs the owner to relax that constraint.
2. **Blocking relay / byte pipe** (Approach C/D) behind requirements goal 4, with a differential-test harness against the rmcp thin client; related to issue #38.
3. **Release automation**: the repo has no release workflow (only `ci.yml`); if one is added it must call the two build commands from Story 3.2.1 and ship both binaries in one directory.

## Effort estimate
Remaining work only (Phase 1 is already implemented). Cost = price-weighted fresh-input equivalents: `in + 5 * out`, with an assumed 12% output share (CU = raw x 1.48, ignoring cache discounts, so conservative). Verification multiplier x1.75 applied to every story.

| Story | Remaining tasks | Cost class | Raw tokens (base) | Raw incl. x1.75 | Output share | CU | Wall-clock blocker |
|---|---|---|---|---|---|---|---|
| 1.1.1 (1.1.1c) | 1 | XS | 25k | 44k | 12% | 65k | none |
| 2.1.1 | 3 (S, S, XS) | S | 110k | 193k | 12% | 285k | none |
| 2.1.2 | 2 (M, S) | M | 150k | 263k | 12% | 389k | CI for exec-bit/PATH behavior |
| 2.2.1 | 2 (XS, M) | M | 125k | 219k | 12% | 324k | none |
| 3.1.1 | 3 (S, S, XS) | S | 95k | 166k | 8% | 246k | Cold fat-LTO and baseline builds (compile time) |
| 3.1.2 | 2 (M, S) | M | 120k | 210k | 12% | 311k | CI queue |
| 3.2.1 | 2 (S, XS) | S | 60k | 105k | 15% | 155k | none |
| 3.2.2 | 2 (S, XS) | S | 50k | 88k | 8% | 130k | Owner decision AC7 (Tyler) |
| **Total** | **17** | | **735k** | **1.29M** | | **1.90M CU** | |

- Size band: Medium (0.5-3M CU), INFERRED. Excludes SDD phases 4-7 overhead (validate, verify, ship).
- **Critical path (agent waves)**: Wave 1: 2.1.1a -> b -> c (serial on `thin_client.rs`/`main.rs`) in parallel with 1.1.1c and 3.1.1a (baseline build). Wave 2: 2.1.2a and 2.1.2b (depend on 2.1.1) in parallel with 2.2.1 (touches Cargo manifests only). Wave 3: 3.1.2a -> 3.1.2b (needs final profile from 2.2.1 and the tests from 2.1.2), in parallel with 3.2.1 and 3.2.2a. Wave 4: 3.1.1b/c final measurements (after profile is final), then 3.2.2b. Four to five waves.
- **Overrun checkpoint**: pause and report if cumulative spend exceeds 25% over the table for completed stories (total ceiling about 2.38M CU, i.e. 1.25 x 1.90M); informational, not a cut line.
- **Wall-clock blockers**: (1) owner decision on AC7/ADR-003 (Tyler); (2) cold fat-LTO and pre-split baseline compiles; (3) CI queue for the first run of the new `ci.yml` steps; (4) macOS symlink behavior of `current_exe` is INFERRED and can only be confirmed on a Mac (Tyler's work machine).
- Basis: bands and weights INFERRED; recalibrate after the first implementer run (Story 2.1.1).
