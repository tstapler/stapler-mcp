# Implementation Plan: read-website-daemon-recovery

**Feature**: Keep the shared stapler-mcp daemon alive across client disconnects, make `ensure_daemon` restart/wait correctly (no spurious "Connection refused"), and make failures diagnosable (error paths, non-crashing `stapler_daemon_status`).
**Date**: 2026-10-29
**Status**: Ready for implementation
**Complexity**: 2 (bug fix, infra, no UI)
**ADRs**: [ADR-001](../decisions/ADR-001-ensure-daemon-spawn-unless-timeout.md), [ADR-002](../decisions/ADR-002-daemon-status-probe-does-not-spawn.md)
**Requirements**: [requirements.md](../requirements.md) (AC0-AC4 numbering used throughout; `/backlog/done-N`)

Research: [stack.md](../research/stack.md), [pitfalls.md](../research/pitfalls.md), [build-vs-buy.md](../research/build-vs-buy.md). `research/prior-draft.md` and the old root `plan.md`/`validation.md` are superseded.

---

## Verified code facts (HEAD ec2a245)

| # | Fact | Source |
|---|------|--------|
| F1 | `Daemon::run` and `run_cancellable` propagate `serve_connection` errors with `?`, so one EPIPE/read-timeout ends the accept loop | `crates/core/src/daemon.rs:160`, `:187` |
| F2 | `run_daemon` calls `exit(1)` on any `run_cancellable` error, leaving the socket file behind (stale -> ECONNREFUSED) | `crates/cli/src/main.rs:964-966` |
| F3 | Accept loop is serial; handler bounded by `REQUEST_TIMEOUT` 30s; `CONN_TIMEOUT` 120s | `daemon.rs:23,31,183-187,196-203` |
| F4 | `ensure_daemon` treats any ping failure (including `PortError::Timeout` from a busy-but-alive daemon, `DIAL_TIMEOUT` 2s) as "down" and spawns, then polls to a 10s deadline | `crates/core/src/client.rs:10-11,78-105` |
| F5 | `NativeSocketFactory::connect` maps connect-timeout to `PortError::Timeout` and every other failure (ENOENT, ECONNREFUSED) to `PortError::Io(String)`; `read_frame` timeout is also `Timeout` | `crates/native/src/socket.rs:96-101,38-40` |
| F6 | `crates/wasm` also calls `ensure_daemon` (with a `WasmSocketFactory` that only ever yields `Timeout` or `Io(msg)`), so a new `PortError` variant would never be produced there | `crates/wasm/src/lib.rs:370`, `crates/wasm/src/socket.rs:30-37` |
| F7 | `SocketTransport` passes `..Default::default()` (10s startup) and formats `"ensure daemon: {e} ({hint})"` with no socket/log path; no test seam | `crates/thin-client/src/transport.rs:44-57` |
| F8 | Socket is bound only after `NativeBrowser::launch` (lock `main.rs:344`, launch `:378`, `run_cancellable` `:947` -> `remove_stale`+`bind` `daemon.rs:180-181`), so the whole cold Chrome start is ENOENT/ECONNREFUSED to clients | `crates/cli/src/main.rs:344,378,947` |
| F9 | `stapler_daemon_status` goes through `transport.call` (= `ensure_daemon`, auto-spawns) and returns `Err(String)` when down; `DaemonStatusOutput` has a non-optional `pong: bool` and no "down" fields; it is struct-literal-constructed at 3 sites in cli | `crates/thin-client/src/mcp_router.rs:866-877`, `crates/core/src/schema.rs:591-603`, `crates/cli/src/main.rs:162,178,191` |
| F10 | `DaemonTransport` has three impls: `SocketTransport`, `ChannelTransport` (`crates/cli/src/transport.rs:105`), test `FakePingTransport` (`crates/thin-client/tests/tool_schema.rs:199`) | grep |
| F11 | Daemon logging convention is `eprintln!("stapler-mcp: ...")` (stderr is redirected to `daemon.log` by the spawner); `eprintln!` already compiles in `crates/core` | `crates/cli/src/main.rs:313,348,381,965`; `crates/core/src/paths.rs:74` |
| F12 | Lock file holds the daemon PID (`write_pid`), already read by `daemon_ping.rs`; `flock` is released by the kernel on SIGKILL; `remove_stale` then `bind` is safe after winning the lock | `crates/cli/tests/daemon_ping.rs:134`, `crates/native/src/lock.rs:42-45`, `daemon.rs:147-149` |
| F13 | `libc` is a dep of `native` only; `futures = "0.3"` is already a regular dep of `cli`; `daemon_ping.rs` is one env-mutating `#[tokio::test]` (header comment) | `crates/cli/Cargo.toml`, `crates/native/Cargo.toml` |
| F14 | `crates/core` dev-deps: `tokio` with `rt, macros, time` only (enough for `#[tokio::test]`; no `test-util`/pause needed if the fake clock advances in the fake sleeper) | `crates/core/Cargo.toml:32-33` |

---

## Pattern Decisions

Step 0.5 alternatives considered for the overall approach:

| Approach | Strength | Weakness | Verdict |
|----------|----------|----------|---------|
| A. Minimal in-repo fix: log-and-continue in accept loop, `ensure_daemon` waits on Timeout, longer timeout, richer error, non-spawning status probe | Targets the verified root cause; no new deps; unit-testable with existing fakes | Does not help crashes from other causes (panic, OOM); restart still relies on next client | **Chosen** |
| B. Bind socket before Chrome launch (lazy browser) | Removes the ECONNREFUSED window entirely | Handlers would need lazy browser init; large refactor of `run_daemon`; out of scope for a bug fix | Rejected (see Unresolved/known limits) |
| C. systemd/launchd socket activation or OSS supervision crate | Init system owns the socket | Three platforms (Manjaro/Ubuntu, macOS, WSL2), bootstrap changes, replaces wasm-portable `SocketFactory` ports (per build-vs-buy.md) | Rejected |

| Component | Pattern Chosen | Source | Alternative Rejected | Reason |
|-----------|---------------|--------|---------------------|--------|
| Accept loop (`Daemon::run`, `run_cancellable`) | Error isolation per request: one private `serve_logged` helper called by both loops; listener `accept` errors stay fatal | PoEAA-style Service Layer boundary; Release It! "bulkhead" per connection | Duplicate `match` in both loops | Both loops are already near-duplicates (`daemon.rs:151-192`); one helper keeps the policy in a single place |
| `ensure_daemon` failure classification | Branch on the existing typed `PortError::Timeout` ("alive but busy") vs everything else ("not listening -> spawn") | type-driven-design (use existing sum type) | New `PortError::Unreachable` variant; string-match on `Io(msg)` | New variant is never produced by the wasm adapter (F6) so wasm would stop spawning; string matching is fragile. See ADR-001 |
| `ensure_daemon` state | Single loop with a `spawned: bool` flag instead of ping -> spawn -> poll phases | GoF-free; simple state machine | Keep three phases and bolt on a second poll loop | One loop expresses "spawn at most once, only when not listening" directly |
| Error message | Pure function `ensure_error_message(err, sock_path, log_path, hint) -> String` | Functional core / imperative shell | Generic `ensure_and_call<S,Sp,Sl,C>` over ports | Pure fn is the smallest test seam (research pitfalls (e)); the generic version is a larger refactor for the same assertion |
| Daemon-status probe | New defaulted method `DaemonTransport::status_probe(&self) -> DaemonStatusOutput`; `SocketTransport` overrides with ping-only (no spawn) | Template Method (default impl delegates to `call`) | Change `call`'s signature; add `ping_only` returning `Result` | Default impl means `ChannelTransport` and `FakePingTransport` compile unchanged (F10); returning the output struct makes "down" a value, not an error. See ADR-002 |
| `DaemonStatusOutput` "down" shape | Additive optional fields `error`, `socket_path`, `log_path` (`skip_serializing_if = "Option::is_none"`) with `pong: false` | Additive schema evolution | Separate `Down` output type / enum | `pong` already means reachable; additive fields keep the existing running-state JSON byte-identical |

---

## Tech Debt Disposition

| Area | Existing Issue | Disposition | Justification |
|------|----------------|--------------|----------------|
| `crates/core/src/daemon.rs` `run` / `run_cancellable` | Near-duplicate accept loops (both carry the F1 bug) | Extend as-is, with the fix in one shared helper | Fix lands once via `serve_logged`; merging the loops would also touch the wasm-vs-native `cfg` split, which is out of scope |
| `crates/cli/src/main.rs` `run_daemon` | Chrome launch before socket bind (F8); http-port file written/removed before the lock (`main.rs:326-341`), so a losing parallel spawn with different env can clobber the winner's port file | Extend as-is (untouched) | Timeout raise covers AC0/AC1; the port-file ordering only matters with divergent `STAPLER_MCP_HTTP_PORT` across concurrent clients, outside the AC. Recorded under Known limits |
| `crates/thin-client/src/transport.rs` | Error text built inline with hard-wired `Native*` adapters, no seam | Isolate via seam | Task 3.1.1a extracts the pure formatter; no wider refactor needed |

---

## Risk Control

- **Feature flag**: not gated (bug fix).
- **Rollback**: standard revert via PR close + revert commit. Behavior changes to call out in the PR: (1) `stapler_daemon_status` no longer auto-spawns (ADR-002); (2) `ensure_daemon` no longer spawns on a ping `Timeout` (ADR-001); (3) default startup timeout 10s -> 30s in core, 45s in `SocketTransport`.
- **Staged rollout**: full rollout on merge.
- Migration Plan: N/A (no schema/data change; `DaemonStatusOutput` fields are additive).
- Observability Plan: N/A beyond one new daemon-log line (task 1.1.1a, `eprintln!` to `daemon.log`, per F11); no metrics/alerts.

## Known limits (documented, not fixed here)

1. **Queued long calls vs client `CALL_TIMEOUT`** (`transport.rs:16`, 120s): the daemon serves connections serially with a 30s handler cap, so with more than about 4 `read_website` calls queued the tail callers can hit the 120s client timeout while still correctly queued. AC1 only requires that none return "Connection refused"; after this plan such a caller gets `timed out` (and its abandoned connection no longer kills the daemon, task 1.1.1a). Concurrent handling is out of scope (requirements.md).
2. **Silent client** that connects and sends nothing blocks the serial loop up to `CONN_TIMEOUT` 120s (`daemon.rs:23,197`); not changed.
3. **Chrome-launch failure** still `exit(1)`s the daemon (`main.rs:381`); clients poll the full timeout then report the error, which now names the log path (AC4).
4. **Orphaned Chrome after SIGKILL** can hold a persistent profile's `SingletonLock` (only when `STAPLER_MCP_BROWSER_PROFILE_DIR` is set); ephemeral mode (default, used by the tests) is unaffected.
5. http-port file ordering race (see Tech Debt Disposition).

## Unresolved Questions

None blocking. ADR-002's behavior change (status stops auto-spawning) is a deliberate default; the owner can veto at PR review without affecting other stories.

## Dependency Visualization

```
Wave 1 (disjoint files)     Wave 2                      Wave 3
-----------------------     ------------------------    -----------------------------------
T1.1.1a daemon.rs  ───────> T1.1.1b daemon tests ─────┐
                       └──────────────────────────────┼─> T4.1.1c client-disconnect e2e (needs 1.1.1a)
T2.1.1a client.rs  ───────> T2.1.1b client tests ─────┼─> T4.1.1a SIGKILL-restart e2e   (needs 1.1.1a, 2.1.1a)
                       └──────────────────────────────┴─> T4.1.1b parallel-cold-start e2e (needs 1.1.1a, 2.1.1a)
T3.1.2a schema.rs+cli ────> T3.1.2b status_probe ───────(also after T3.1.1a: both edit transport.rs)
                            T3.1.1a transport.rs ───────┘
```

Serialization constraints: 2.1.1a -> 2.1.1b (same file `client.rs`); 3.1.1a -> 3.1.2b (same file `transport.rs`); 4.1.1a/b/c are separate new files and are independent of each other.

---

## Phase 1: Daemon recovery

### Epic 1.1: Daemon survives per-connection errors

**Goal**: A client that disconnects mid-request never ends the accept loop.

#### Story 1.1.1: Log and continue on per-connection errors (AC2)
**As a** parallel research subagent, **I want** the daemon to keep serving after another client disconnects early, **so that** my next `read_website` doesn't see a dead socket.
**Acceptance Criteria**:
- A `serve_connection` error (read timeout, EPIPE on `write_frame`) is logged with `eprintln!` and the accept loop continues, in both `run` and `run_cancellable`; a listener `accept` error is still returned (fatal).
  - *Given* a `Daemon` running over a scripted `SocketFactory` whose first accepted `Conn` fails `write_frame` with `PortError::Io("Broken pipe (os error 32)")` and whose second `Conn` sends `{"tool":"ping"}`, *When* `run_cancellable` is driven and then cancelled, *Then* the second `Conn` receives `{"result":{"pong":true}}`, and `run_cancellable` returns `Ok(())`. The log line (`stapler-mcp: connection error (continuing): ...`) is verified by code review and the e2e daemon log, not by the unit test.
  - *Given* the same script but a listener whose `accept()` returns `Err(PortError::Io("too many open files"))`, *When* `run` is driven, *Then* it returns `Err(PortError::Io("too many open files"))`.
**Files**: `crates/core/src/daemon.rs`

##### Task 1.1.1a: Swallow-and-log helper (XS) — AC2
- Add `async fn serve_logged<C: Conn>(&self, conn: C)` next to `serve_connection` (`daemon.rs:196`): `if let Err(e) = self.serve_connection(conn).await { eprintln!("stapler-mcp: connection error (continuing): {e}"); }`.
- Replace `self.serve_connection(conn).await?;` at `:160` and `:187` with `self.serve_logged(conn).await;`. Leave `accept().await?` (`:159`, `:185`) untouched.
- Update the `run`/`run_cancellable` doc comments to state the policy (per-connection errors logged; accept errors fatal). The `main.rs:964-966` exit-on-error path stays as is: it now fires only for bind/accept failures.
- Files: `crates/core/src/daemon.rs`

##### Task 1.1.1b: Unit tests with scripted ports (S) — AC2
- In `daemon.rs` `mod tests` (after `PendingSocketFactory`, ~line 389) add `ScriptedConn` (fields: `read: Option<Result<Option<Vec<u8>>, PortError>>`, `write_result: Result<(), PortError>`, shared `Rc<RefCell<Vec<Vec<u8>>>>` of written frames) and `ScriptedListener` popping from a `VecDeque`, then `std::future::pending()` (or an `Err` entry for the accept-error test). Do not touch `NeverConn`/`PendingListener`.
- Tests (names in the repo's `should_` style):
  - `run_cancellable_should_keep_serving_after_a_connection_write_error`
  - `run_cancellable_should_keep_serving_after_a_connection_read_error`
  - `run_should_keep_serving_after_a_connection_write_error` (second conn sends `{"tool":"shutdown"}` so `run` returns `Ok(())`)
  - `run_should_return_err_when_accept_fails`
- Use the existing `tokio::join!` + `yield_now` + `request_shutdown` pattern and 200ms `tokio::time::timeout` guard (`daemon.rs:396-411`); `Daemon` is `!Send`, so no `tokio::spawn`.
- Files: `crates/core/src/daemon.rs`

---

## Phase 2: Client recovery

### Epic 2.1: `ensure_daemon` restarts or waits correctly

**Goal**: Spawn only when nothing is listening; wait (without spawning) when the daemon is alive but busy; allow a cold Chrome start.

#### Story 2.1.1: Spawn on not-listening, wait on Timeout, 30s default (AC0, AC1)
**As a** client after a daemon crash or a cold start, **I want** `ensure_daemon` to start exactly one daemon and wait for it, **so that** my call succeeds instead of returning "Connection refused".
**Acceptance Criteria**:
- When the first ping fails with anything other than `PortError::Timeout`, `ensure_daemon` spawns once and polls until ping succeeds or `startup_timeout` (default now 30s) elapses.
  - *Given* a `SocketFactory` fake whose first two `connect` calls return `Err(PortError::Io("Connection refused (os error 111)"))` and the third returns a conn that answers pong, *When* `ensure_daemon` runs with `EnsureOptions::default()`, *Then* it returns `Ok(())` and the `ProcessSpawner` fake recorded exactly 1 `spawn_daemon` call (AC0, AC1).
- When ping returns `PortError::Timeout` (connect OK, no reply: daemon alive but serving another request), `ensure_daemon` does not spawn and keeps polling to the deadline.
  - *Given* a fake whose first ping yields `PortError::Timeout` and whose 4th ping answers pong, *When* `ensure_daemon` runs, *Then* it returns `Ok(())` with 0 `spawn_daemon` calls.
  - *Given* a fake that yields `PortError::Timeout` on every ping and a fake clock advanced only by the fake sleeper, *When* `ensure_daemon` runs with `startup_timeout: Some(30s)`, *Then* it returns `Err(PortError::Timeout)` once the fake clock passes 30_000ms, with 0 spawns.
- Spawn happens at most once per call, even if later pings fail again.
  - *Given* a fake returning `Io("Connection refused")` forever, *When* `ensure_daemon` runs with a 5s timeout, *Then* it returns `Err(PortError::Io("Connection refused (os error 111)"))` after exactly 1 spawn.
**Files**: `crates/core/src/client.rs`

##### Task 2.1.1a: Rewrite `ensure_daemon` control flow (S) — AC0, AC1
- `DEFAULT_STARTUP_TIMEOUT` 10s -> 30s (`client.rs:10`); update the doc comment at `:59-61`.
- Collapse `:78-105` into one loop: compute deadline up front; `spawned = false`; each iteration pings with `DIAL_TIMEOUT` if the previous error was `PortError::Timeout` (or first iteration), else `POLL_CAP`; `Ok` -> return; `Err(PortError::Timeout)` -> record, sleep, continue (no spawn); any other `Err(e)` -> if `!spawned`, `spawner.spawn_daemon(..).await?` and set `spawned = true`; record `e`, sleep, continue. On deadline return the last error. Keep 50ms -> 500ms backoff.
- Keep the public signature and `EnsureOptions` unchanged (wasm caller at `crates/wasm/src/lib.rs:370` keeps compiling; see ADR-001).
- Files: `crates/core/src/client.rs`

##### Task 2.1.1b: Unit tests with fake ports (M) — AC0, AC1
- Add `#[cfg(test)] mod tests` in `client.rs` (file has none today): `FakeSocket` (scripted `VecDeque<Result<FakeConn, PortError>>` for `connect`; `FakeConn` answers `{"result":{"pong":true}}` on `read_frame` or returns the scripted read error), `CountingSpawner` (`Cell<u32>`), `FakeSleeper` that advances a shared `Cell<u64>`, `FakeClock` reading it. Implement the `SocketFactory`/`Conn`/`ProcessSpawner`/`SleepPort`/`ClockPort` traits from `crates/core/src/ports.rs` (note `SocketFactory` needs an associated `Listener` type; use an uninhabited listener that is never bound).
- Tests: `ensure_daemon_should_spawn_once_when_not_listening`, `ensure_daemon_should_not_spawn_when_ping_times_out_then_recovers`, `ensure_daemon_should_return_timeout_without_spawning_after_deadline`, `ensure_daemon_should_spawn_at_most_once_when_pings_keep_failing`, `ensure_daemon_should_not_spawn_when_first_ping_succeeds`, `ensure_daemon_default_timeout_should_be_30s` (asserts the fake clock reaches >= 30_000 before giving up with `EnsureOptions::default()`).
- Files: `crates/core/src/client.rs`

---

## Phase 3: Diagnostics

### Epic 3.1: Errors and status that explain themselves

**Goal**: A failed `ensure_daemon` names the socket and log; `stapler_daemon_status` reports "down" as a normal result.

#### Story 3.1.1: Error message with paths and a startup budget above Chrome cold start (AC4, AC0)
**As a** user whose tool call failed, **I want** the error to say where the socket and log are, **so that** I can see why the daemon didn't start.
**Acceptance Criteria**:
- The `ensure daemon` error text contains the socket path and the daemon log path.
  - *Given* `err = PortError::Io("Connection refused (os error 111)")`, `sock_path = "/home/alice/.stapler-mcp/daemon.sock"`, `log_path = "/home/alice/.stapler-mcp/daemon.log"`, *When* `ensure_error_message(&err, sock_path, log_path, hint)` is called, *Then* the result contains `"ensure daemon: io error: Connection refused (os error 111)"`, `"socket: /home/alice/.stapler-mcp/daemon.sock"`, `"daemon log: /home/alice/.stapler-mcp/daemon.log"`, and the hint text.
  - *Given* `err = PortError::Timeout`, *When* formatted, *Then* the result additionally says the daemon is reachable but did not answer in time (so the user is not told it failed to start).
- `SocketTransport` waits up to 45s for startup (margin over the 30s AC0 budget).
  - *Given* `SocketTransport::call` on a cold start where Chrome takes 20s to launch, *When* it runs, *Then* `ensure_daemon` receives `startup_timeout: Some(45s)` and the call succeeds (asserted via the `STARTUP_TIMEOUT` constant being >= 30s in a unit test; the live behavior is covered by task 4.1.1a).
**Files**: `crates/thin-client/src/transport.rs`

##### Task 3.1.1a: Extract formatter, add paths, set timeout (S) — AC4, AC0
- Add `const STARTUP_TIMEOUT: Duration = Duration::from_secs(45);` and pass `startup_timeout: Some(STARTUP_TIMEOUT)` at `transport.rs:51-54`.
- Add `pub(crate) fn ensure_error_message(err: &PortError, sock_path: &str, log_path: &str, hint: &str) -> String` (variant-specific wording for `Timeout`; default: `format!("ensure daemon: {err} (socket: {sock_path}; daemon log: {log_path}; {hint})")`). Call it from `:57` via `map_err(|e| ensure_error_message(&e, &sock_path, &log_path, &daemon_locator::lookup_hint()))`.
- Add `#[cfg(test)] mod tests` in `transport.rs`: `ensure_error_message_should_include_socket_and_log_paths`, `ensure_error_message_should_explain_timeout_separately`, `startup_timeout_should_cover_cold_chrome_start` (asserts `STARTUP_TIMEOUT >= 30s`).
- Files: `crates/thin-client/src/transport.rs`

#### Story 3.1.2: `stapler_daemon_status` reports an unreachable daemon (AC3)
**As an** agent diagnosing a failure, **I want** `stapler_daemon_status` to return a clear result when the daemon is down, **so that** I get paths to inspect instead of a tool error (and the status check doesn't itself launch Chrome).
**Acceptance Criteria**:
- When the daemon cannot be reached, the tool returns `Ok` with `pong: false`, `error`, `socketPath`, `logPath`; the running-state output is unchanged.
  - *Given* a `DaemonTransport` fake whose `call` returns `Err("ensure daemon: io error: Connection refused (os error 111) (...)")`, *When* `call_daemon_status_for_test()` runs, *Then* it returns `Ok(DaemonStatusOutput { pong: false, error: Some("...Connection refused..."), .. })` and does not return `Err`.
  - *Given* the existing `FakePingTransport` with `{"pong": true, "browserProfileMode": "persistent at /home/alice/.stapler-mcp/browser-profile", "browserProfileWarning": "looks unsafe"}`, *When* `call_daemon_status_for_test()` runs, *Then* `pong == true`, mode and warning are the same strings, and `error`, `socket_path`, `log_path` are `None` (existing test `daemon_status_should_deserialize_run_daemons_camelcase_wire_keys` passes unchanged).
- `SocketTransport::status_probe` never spawns a daemon.
  - *Given* no daemon and an empty `STAPLER_MCP_HOME` tempdir, *When* `SocketTransport.status_probe()` runs, *Then* it returns `pong: false` with `socketPath` ending `/daemon.sock` and `logPath` ending `/daemon.log`, and no `daemon.lock` file exists afterward.
**Files**: `crates/core/src/schema.rs`, `crates/cli/src/main.rs`, `crates/thin-client/src/transport.rs`, `crates/thin-client/src/mcp_router.rs`, `crates/thin-client/tests/tool_schema.rs`

##### Task 3.1.2a: Extend `DaemonStatusOutput` (S) — AC3
- `schema.rs:589-603`: add `#[derive(Default)]` and three fields, each `#[serde(default, skip_serializing_if = "Option::is_none")]`: `error: Option<String>`, `socket_path: Option<String>`, `log_path: Option<String>` (struct is `rename_all = "camelCase"`). Doc-comment: `pong == false` means unreachable and `error` explains.
- Update the three struct literals in `crates/cli/src/main.rs` (`:162`, `:178`, `:191`) with `..Default::default()`.
- Check `crates/thin-client/tests/tool_schema.rs` still passes (its schema assertions are derived from the structs; run it and fix any hard-coded expectation).
- Files: `crates/core/src/schema.rs`, `crates/cli/src/main.rs`

##### Task 3.1.2b: Non-spawning `status_probe` (M) — AC3 (after 3.1.1a, 3.1.2a)
- `transport.rs` trait `DaemonTransport`: add defaulted `fn status_probe(&self) -> impl Future<Output = DaemonStatusOutput> + Send`; default body awaits `self.call(PING_TOOL, json!({}))` and maps `Ok(v)` via `serde_json::from_value::<DaemonStatusOutput>` (deserialize error -> down output), `Err(e)` -> `DaemonStatusOutput { pong: false, error: Some(e), ..Default::default() }`. `ChannelTransport` and `FakePingTransport` inherit it unchanged.
- `SocketTransport` override: `client::call(&socket, &sock_path, PING_TOOL, None, STATUS_PING_TIMEOUT /* 2s, matches main.rs:100 */)` with no `ensure_daemon`; on error return `pong: false`, `error: Some(ensure_error_message(...))`, `socket_path`, `log_path` populated. Reuse `ensure_error_message` from 3.1.1a.
- `mcp_router.rs:866-877`: `daemon_status` returns `Ok(Json(self.transport.status_probe().await))`; update the tool `description` (`:861`) with one sentence: reports `pong:false` plus socket/log paths when unreachable and does not start the daemon.
- Tests in `tool_schema.rs`: add `FailingTransport` (`call` -> `Err(..)`) and `daemon_status_should_report_down_instead_of_erroring`; keep the existing running-state test; add a `SocketTransport` test with a tempdir `STAPLER_MCP_HOME` asserting the no-spawn Given/Then above (this file's tests must not race on `set_var`: put the env-mutating test in its own file `crates/thin-client/tests/status_probe_no_spawn.rs` if `tool_schema.rs` has other env readers).
- Files: `crates/thin-client/src/transport.rs`, `crates/thin-client/src/mcp_router.rs`, `crates/thin-client/tests/tool_schema.rs` (+ optional `crates/thin-client/tests/status_probe_no_spawn.rs`)

---

## Phase 4: End-to-end verification

### Epic 4.1: Real-binary recovery tests

**Goal**: Prove AC0-AC2 against the real `stapler-mcp-daemon` binary, in new test files (one env-mutating `#[tokio::test]` per file, per the `daemon_ping.rs` header contract, F13). Each follows `daemon_ping.rs`: tempdir + `TestEnv` + `set_var("STAPLER_MCP_HOME", ..)`, `env!("CARGO_BIN_EXE_stapler-mcp-daemon")`, ephemeral Chrome profile. Write these first (TDD) and confirm tests 4.1.1a and 4.1.1c fail against the pre-fix daemon/client.

#### Story 4.1.1: Kill, parallel cold start, and disconnect e2e (AC0, AC1, AC2)
**As a** maintainer, **I want** end-to-end tests of the exact failure scenarios, **so that** the bug cannot regress unnoticed.
**Acceptance Criteria**:
- After SIGKILL the next `ensure_daemon` restarts the daemon within 30s.
  - *Given* a daemon started by `ensure_daemon` with PID `P1` read from `daemon.lock`, *When* the test calls `libc::kill(P1, SIGKILL)` and then `ensure_daemon` with `startup_timeout: Some(30s)`, *Then* `ensure_daemon` returns `Ok(())` in under 30s, the lock file now holds `P2 != P1`, and a `stapler_browser_list_sessions` call returns `{"sessions":[]}` (AC0).
- N parallel cold starts all succeed.
  - *Given* no daemon and a fresh tempdir, *When* 8 concurrent `ensure_daemon` futures (`futures::future::join_all`) run with `startup_timeout: Some(45s)`, *Then* all 8 return `Ok(())` (none `Err` containing `Connection refused`) and exactly one live daemon PID is recorded in `daemon.lock` (AC1).
- Abandoned client connections do not kill the daemon.
  - *Given* a running daemon with PID `P`, *When* the test opens 20 raw `UnixStream`s to `daemon.sock`, writes `{"tool":"ping"}\n` on each and drops them without reading, then calls `client::ping`, *Then* the ping succeeds and `daemon.lock` still holds `P` (AC2).
**Files**: `crates/cli/Cargo.toml`, `crates/cli/tests/daemon_kill_restart.rs`, `crates/cli/tests/daemon_parallel_cold_start.rs`, `crates/cli/tests/daemon_client_disconnect.rs`

##### Task 4.1.1a: SIGKILL-then-restart test (M) — AC0
- Add `libc = "0.2"` to `crates/cli/Cargo.toml` `[dev-dependencies]` (same version already in the lockfile via `native`).
- New `crates/cli/tests/daemon_kill_restart.rs`: spawn via `ensure_daemon`, parse PID from `lock_path`, `unsafe { libc::kill(pid, libc::SIGKILL) }`, poll until `client::ping` errors, assert the stale `daemon.sock` still exists (the bug precondition), then `ensure_daemon` with 30s and `Instant`-based elapsed assert `< 30s`; assert new PID differs; call `stapler_browser_list_sessions` via `client::call`; finish with `shutdown`.
- Files: `crates/cli/Cargo.toml`, `crates/cli/tests/daemon_kill_restart.rs`

##### Task 4.1.1b: Parallel cold-start test (M) — AC1
- New `crates/cli/tests/daemon_parallel_cold_start.rs`: `join_all` of 8 `ensure_daemon` calls sharing the same `NativeSocketFactory`/`NativeSpawner`; assert all `Ok`, and for any `Err` print the message and fail if it contains `Connection refused`; assert `daemon.lock` PID is alive (`libc::kill(pid, 0) == 0`); then one `client::ping`; `shutdown`.
- `futures` is already a regular dep of `cli` (F13); runs on the default current_thread `#[tokio::test]` runtime because the calls are I/O-bound.
- Files: `crates/cli/tests/daemon_parallel_cold_start.rs`

##### Task 4.1.1c: Client-disconnect test (S) — AC2
- New `crates/cli/tests/daemon_client_disconnect.rs`: start daemon, record PID, open 20 `tokio::net::UnixStream`s, write the ping frame, drop all, `sleep(500ms)`, `client::ping` must succeed, lock-file PID unchanged. Pre-fix this exits the daemon on the first abandoned connection (F1, F2).
- Files: `crates/cli/tests/daemon_client_disconnect.rs`

---

## Acceptance coverage matrix

| AC | Requirement | Tasks | Verified by |
|----|-------------|-------|-------------|
| 0 | SIGKILL -> next call restarts, succeeds within 30s | 2.1.1a, 3.1.1a (timeouts), 4.1.1a | `ensure_daemon_*` unit tests (2.1.1b), `daemon_kill_restart.rs` |
| 1 | N parallel cold-start calls all succeed, none "Connection refused" | 2.1.1a, 3.1.1a, 4.1.1b | `ensure_daemon_should_spawn_once_*`, `daemon_parallel_cold_start.rs` |
| 2 | Client disconnect: daemon logs and continues | 1.1.1a, 1.1.1b, 4.1.1c | `run*_should_keep_serving_*` unit tests, `daemon_client_disconnect.rs` |
| 3 | `stapler_daemon_status` returns a diagnostic when unreachable | 3.1.2a, 3.1.2b | `daemon_status_should_report_down_instead_of_erroring`, no-spawn test |
| 4 | `ensure_daemon` failure message includes socket and log paths | 3.1.1a | `ensure_error_message_should_include_socket_and_log_paths` |

Every task maps to at least one AC: 1.1.1a/b -> 2; 2.1.1a/b -> 0,1; 3.1.1a -> 4 (and 0,1 for the timeout); 3.1.2a/b -> 3; 4.1.1a -> 0; 4.1.1b -> 1; 4.1.1c -> 2.

## Effort estimate

Bands and weights INFERRED (ESTIMATION.md); recalibrate after the first implementer run. Output share assumed 12% for implementation runs, so CU = raw x (0.88 + 5 x 0.12) = raw x 1.48 (no cache discount applied; actual cost is lower because plan/repo context is re-read as cached input).

| Story | Tasks (class) | Raw tokens | Output share | CU (x1.48) | Wall-clock blocker |
|-------|---------------|-----------:|-------------:|-----------:|--------------------|
| 1.1.1 | 1.1.1a XS, 1.1.1b S | ~80k | 12% | ~118k | none |
| 2.1.1 | 2.1.1a S, 2.1.1b M | ~170k | 12% | ~252k | none |
| 3.1.1 | 3.1.1a S | ~50k | 12% | ~74k | none |
| 3.1.2 | 3.1.2a S, 3.1.2b M | ~150k | 12% | ~222k | none |
| 4.1.1 | 4.1.1a M, 4.1.1b M, 4.1.1c S | ~250k | 12% | ~370k | CI/dev machine with Chrome and a few minutes of real daemon startups |
| **Sum** | 10 tasks | ~700k | | ~1.04M | |

- With the story verification multiplier x1.75: ~1.2M raw, ~1.8M CU. Size band: Medium (informational; the work itself is small, the weight is test scaffolding and review loops).
- **Critical path**: 3 agent waves (Wave 1: 1.1.1a, 2.1.1a, 3.1.2a in parallel; Wave 2: 1.1.1b, 2.1.1b, 3.1.1a; Wave 3: 3.1.2b, 4.1.1a, 4.1.1b, 4.1.1c in parallel), then one verify/repair pass.
- **Overrun checkpoint**: if cumulative spend on completed stories exceeds the table value by more than 25% (e.g. > ~92k CU over the first two stories' ~370k), pause and report the delta and cause.
- **Wall-clock blockers**: (1) e2e tests need real headless Chrome and network-free startup (same prerequisite as the existing `daemon_ping.rs`), owner: CI/maintainer machine; (2) PR review of ADR-002's behavior change, owner: Tyler.

## Review resolutions (architecture: CONCERNS, adversarial: CONCERNS; 0 blockers)

Reviews: `architecture-review.md`, `adversarial-review.md` in this directory. Concerns folded in as implementation constraints:

1. **Trait default compiles?** Run `cargo check` on the `status_probe` default first; if the `&self`-across-await borrow fails without `Sync`, build the `call` future before the `async` block (preferred) or add `where Self: Sync`. (arch C / adv C2)
2. **Busy daemon reported as down.** `status_probe` treats `PortError::Timeout` as alive-but-busy (`pong:false`, `error` says "daemon busy: ping timed out", paths included), distinct from refused/not-found ("not running"). Add an `unreachable(..)` constructor so contradictory states (`pong:true` + `error`) are not built by hand. (arch, adv C4)
3. **Respawn after lost flock.** In `ensure_daemon`, if the error returns to non-Timeout after a Timeout phase, allow one more spawn; cap total spawns (2). (adv C1)
4. **Kill test asserts error kind.** After SIGKILL, the 4.1.1a test asserts the first ping error is not `Timeout`. (adv C3)
5. **Logging is tested.** 4.1.1c asserts the connection-error line appears in `daemon.log`; unit tests assert `serve_logged` returns Ok on connection error. (arch, adv C6)
6. **AC1 vs CALL_TIMEOUT.** AC1 is verified with N=8 short-page requests (well under 120s); the >~4 long-request limit stays documented under Known limits. (adv C5)
7. **Silent-client stall (≤120s CONN_TIMEOUT)** stays a documented limit; out of scope (concurrent handling).
