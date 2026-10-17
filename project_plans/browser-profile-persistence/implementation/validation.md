# Validation Plan: browser-profile-persistence

**Date**: 2026-09-23

## Happy Path Scenario
Given a daemon operator sets `STAPLER_MCP_BROWSER_PROFILE_DIR` to an absolute path and
restarts the `stapler-mcp` daemon, when a browser session sets a cookie, the daemon is
shut down, and a new daemon is started against the same env var, then a fresh session's
`document.cookie` still contains the value set before the restart (`sticky=yes`).

## Requirement → Test Mapping

Requirement numbers below follow `requirements.md`'s "Acceptance criteria (draft, refined
further in plan.md)" list; each is cross-referenced against the concrete Story/Task
acceptance criteria in `plan.md` that actually implement it, citing plan.md's real test
names rather than inventing parallel ones.

| Requirement | Test File | Test Name | Type | Scenario |
|-------------|-----------|-----------|------|----------|
| REQ-2/3: Opt-in resolver exists, no computed default, returns `None` when unset (Story 1.1.1) | `crates/core/src/paths.rs` | `should_return_none_for_browser_profile_dir_when_unset` | Unit | Happy path — `MockEnv` with `var(...)` returning `None` → `browser_profile_dir(&env)` returns `None` (Task 1.1.1b) |
| REQ-2/3: Opt-in resolver rejects malformed/absent input rather than silently misbehaving (Story 1.1.1) | `crates/core/src/paths.rs` | `should_return_none_for_browser_profile_dir_when_empty` | Unit | Error/edge path — `MockEnv` with `var(...)` returning `Some("")` → `browser_profile_dir(&env)` returns `None`, mirrors `base_dir`'s nonempty-check idiom (Task 1.1.1b) |
| REQ-2/3: Opt-in resolver returns the literal path verbatim, no transformation (Story 1.1.1) | `crates/core/src/paths.rs` | `should_return_configured_path_for_browser_profile_dir_when_set` | Unit | Happy path — absolute path in → `Some(path)` out, unchanged (Task 1.1.1b) |
| REQ-3: Opt-in surface rejects unusable (relative) paths rather than passing them through to `create_dir_all` at an unpredictable cwd-relative location (Story 1.1.1) | `crates/core/src/paths.rs` | `should_return_none_for_browser_profile_dir_when_relative_path_given` | Unit | Error path — relative path in → `None` out + `stderr` names the rejected value (Task 1.1.1b) |
| REQ-4: Default ephemeral behavior is byte-identical when not opted into (Story 1.2.1, AC1) | `crates/native/src/browser.rs` | *(no dedicated new unit test — covered by existing `launch(None)` code path; behavior is asserted by inspection per Task 1.2.1a's AC, and exercised end-to-end whenever any existing `browser_session.rs` test calls `NativeBrowser::launch(None)`, e.g. `navigate_concurrent_should_not_exceed_max_open_sessions`)* | Unit (regression, existing) | `launch(None)` produces the pre-existing pid+timestamp `$TMPDIR` path — no behavior change from before this feature (Task 1.2.1a/1.2.1b) |
| REQ-2/3: Persistent dir is created and hardened to `0700` on the opt-in branch (Story 1.2.1, AC2) | `crates/native/src/browser.rs` | `should_set_mode_0700_on_persistent_dir` | Unit | Happy path — `harden_persistent_dir_permissions(&dir)` sets mode `0700` on a fresh temp dir (Task 1.2.1f) |
| REQ-2: `SingletonLock` collision on the persistent branch produces an actionable error naming the profile dir, not raw chromiumoxide text (Story 1.2.1, AC3) | `crates/native/src/browser.rs` | `should_name_profile_dir_and_singletonlock_when_persistent` | Unit | Error path — `describe_launch_error(e, dir, is_persistent=true)` message contains the dir's display string and `"SingletonLock"` (Task 1.2.1h) |
| REQ-4: Ephemeral branch's error message is unchanged (no persistent-mode wrapping leaks into default behavior) (Story 1.2.1, AC3) | `crates/native/src/browser.rs` | `should_pass_through_raw_error_when_ephemeral` | Unit | Error path (regression) — `describe_launch_error(e, dir, is_persistent=false)` message equals the underlying error's `to_string()`, unchanged from pre-feature behavior (Task 1.2.1h) |
| REQ-2: Unsafe profile location (git/sync-folder ancestry) is detected and warned, not silently accepted (Story 1.2.1, AC4) | `crates/native/src/browser.rs` | `should_return_none_for_safe_profile_path` | Unit | Happy path — plain path with no `.git` ancestor or sync-folder segment → `unsafe_profile_location_reason` returns `None` (Task 1.2.1d) |
| REQ-2: Unsafe profile location (git ancestry) is detected (Story 1.2.1, AC4) | `crates/native/src/browser.rs` | `should_detect_git_repo_ancestor` | Unit | Error/edge path — nested path under a directory containing `.git` → `unsafe_profile_location_reason` returns `Some("appears to be inside a git repository")` (Task 1.2.1d) |
| REQ-2: Unsafe profile location (cloud-sync ancestry) is detected (Story 1.2.1, AC4) | `crates/native/src/browser.rs` | `should_detect_sync_folder_segment` | Unit | Error/edge path — path containing a `Dropbox`/`iCloud Drive`/`Syncthing` component → `unsafe_profile_location_reason` returns `Some(...)` (Task 1.2.1d) |
| REQ-2/4: Env-var wiring at daemon startup selects `launch(None)` vs. `launch(Some(path))` and logs the active mode, without a dedicated automated test (Story 1.2.2) | `crates/cli/src/main.rs` (`run_daemon()`) | *(no automated test — Story 1.2.2's two ACs are asserted only by code inspection/manual `eprintln!` verification per Tasks 1.2.2a/1.2.2b; `main.rs`'s `run_daemon()` has no existing unit-test harness for its startup branching to extend)* | — (gap, see below) | N/A |
| REQ-2: End-to-end, a cookie set before a daemon restart survives the restart when persistence is opted into (Story 1.3.1) | `crates/cli/tests/browser_profile_persistence.rs` (new) | Task 1.3.1a's cross-restart cookie-persistence test (unnamed in plan.md prose; file is new, so this is its sole test) | Integration (`#[ignore]`, real Chrome, manual-run only) | Set `document.cookie="sticky=yes"` pre-shutdown, restart daemon against the same `STAPLER_MCP_BROWSER_PROFILE_DIR`, re-read `document.cookie` post-restart, assert it still contains `"sticky=yes"` — this **is** the Happy Path Scenario above |
| REQ-2: A real (not synthetic-error) `SingletonLock` collision against the same persistent dir produces the documented error (Story 1.3.2) | `crates/cli/tests/browser_profile_persistence.rs` (new) | Task 1.3.2a's SingletonLock collision test (unnamed in plan.md prose) | Integration (`#[ignore]`, real Chrome, manual-run only) | Two concurrent `NativeBrowser::launch(Some(dir))` calls against the same real dir; second returns `Err` containing `"appears to be in use"` and `"SingletonLock"` |
| REQ-1: Sessions-share-one-profile premise is documented (already closed by research, not by new tests) | N/A | N/A | N/A | Closed by `requirements.md`'s "Ground truth" section (code-comment + research citation), not a test — no code behavior changes for this criterion |

### Gaps plan.md leaves open (not new tests proposed — flagged per plan.md's own Unresolved Questions)

- **Story 1.2.2 (env-var-to-`launch()` wiring in `run_daemon()`) has no automated test at
  all**, unit or integration — plan.md's own Task 1.2.2a/1.2.2b description only says to
  implement the wiring and log line, with no corresponding test task. This is a genuine
  coverage gap: `paths::browser_profile_dir` is unit tested in isolation (Story 1.1.1) and
  `NativeBrowser::launch`'s branching is unit tested in isolation (Story 1.2.1), but the
  glue in `main.rs` that reads the env var and threads it through is only exercised
  end-to-end by the `#[ignore]`d Story 1.3.1 integration test (which requires a real Chrome
  binary and is not run in CI). Given this is a `later`-priority, single-user personal-tool
  feature and `main.rs`'s `run_daemon()` has no existing unit-test seam for its startup
  sequence to extend (confirmed: no test module wraps `run_daemon()` in
  `crates/cli/src/main.rs` today), this plan does not add one — consistent with plan.md's
  own proportionality calls elsewhere (e.g. declining to extend Story 1.3.1 to cover
  crash-path recovery). Flagged here rather than silently assumed covered.
- **Crash-path (`kill -9`) profile-dir reuse is explicitly untested** — carried over
  verbatim from plan.md's "Unresolved Questions" section; not re-litigated here.
- **Both integration tests are `#[ignore]`d and this repo's CI never runs `--ignored`
  tests** — per plan.md, both close their respective gaps only when a human runs
  `cargo test -p stapler-mcp --test browser_profile_persistence -- --ignored` manually
  before relying on this feature in practice.

## UX Acceptance Tests
N/A — no user-facing surface, pure daemon-startup infrastructure.

## Test Stack
- **Unit**: Rust's built-in `#[test]` harness (`cargo test`), plain `assert!`/`assert_eq!`
  (no third-party assertion crate in any `Cargo.toml` in this workspace). `crates/core`'s
  existing `MockEnv` test double (implementing `EnvPort`) is reused/extended for
  `paths::browser_profile_dir` tests; `crates/native`'s `browser.rs` tests use real
  temp-dir filesystem I/O (`std::env::temp_dir()` + `std::fs::create_dir_all`/
  `remove_dir_all`) rather than a mocking crate, matching this file's existing convention
  of avoiding a new `tempfile` dev-dependency in `stapler-mcp-native`.
- **Integration**: `#[tokio::test]` (Tokio's async test harness, `crates/core`'s
  `dev-dependencies` on `tokio = { features = ["rt", "macros"] }`), `#[ignore]`d, driven
  through `crates/cli`'s existing daemon-harness conventions in
  `crates/cli/tests/browser_session.rs` (`TestEnv: EnvPort`, `client::ensure_daemon`/
  `client::call`, a `spawn_mock_site()` HTTP helper). Uses `crates/cli`'s `tempfile = "3"`
  dev-dependency for scratch profile/home directories. Requires a real Chrome binary;
  excluded from default `cargo test` runs and from CI (never run with `--ignored`).
- **E2E / UX**: N/A.

## Coverage Targets and How to Measure

| Stack | Coverage command | Target |
|---|---|---|
| Rust (`stapler-mcp-core`, `stapler-mcp-native`, `stapler-mcp`) | `cargo tarpaulin --out Stdout` | ≥80% line |

- All public functions this feature adds or changes (`paths::browser_profile_dir`,
  `NativeBrowser::launch`, `unsafe_profile_location_reason`,
  `harden_persistent_dir_permissions`, `describe_launch_error`): happy path + error paths
  covered by the unit tests above.
- The one external integration this feature touches (a real Chrome/CDP process via
  chromiumoxide): unit-mocked at the branching-logic level (Story 1.2.1's unit tests) plus
  the two `#[ignore]`d real-Chrome integration tests (Stories 1.3.1, 1.3.2) — both must be
  run manually (`cargo test -p stapler-mcp --test browser_profile_persistence -- --ignored`)
  before this feature is relied upon in practice, per plan.md's Unresolved Questions.
