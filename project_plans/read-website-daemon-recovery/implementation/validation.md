# Validation Plan: read-website-daemon-recovery

**Date**: 2026-10-29
**Complexity**: 2 (bug fix, infra, no UI)
**Inputs**: `../requirements.md` (AC0-AC4), `plan.md` (incl. "Review resolutions"), `adversarial-review.md`, `architecture-review.md`
**Crates**: `stapler-mcp-core` (`crates/core`), `stapler-mcp` (`crates/thin-client`), `stapler-mcp-daemon` (`crates/cli`), `stapler-mcp-wasm` (`crates/wasm`, compile gate only)

## Happy Path Scenario

Given a machine where the shared stapler-mcp daemon has been SIGKILLed and left a stale `daemon.sock` (so a client sees ECONNREFUSED), when an agent calls `read_website` (or 8 subagents call it in parallel from a cold start), then `ensure_daemon` spawns one winning daemon, waits through the Chrome cold start (45 s client budget, 30 s core default), and every call succeeds without "Connection refused"; and when a client later disconnects mid-request the daemon logs `connection error (continuing)` and keeps serving.

Error paths and edge cases below are variations on this scenario: ping `Timeout` (busy, do not spawn), repeated refusals (bounded spawns), abandoned connections, unreachable-daemon diagnostics.

## Requirement → Test Mapping

Test types: **Unit** = in-crate `#[cfg(test)]` with scripted fake ports, no real process; **Integration** = test file under `tests/` exercising real adapter code (`SocketTransport`, tempdir `STAPLER_MCP_HOME`) without launching Chrome; **E2E** = real `stapler-mcp-daemon` binary + headless Chrome (same prerequisite as existing `crates/cli/tests/daemon_ping.rs`).

### AC0: After SIGKILL, the next call restarts the daemon and succeeds within 30 s on a cold Chrome start

| Test File | Test Name | Type | Scenario | Command |
|-----------|-----------|------|----------|---------|
| `crates/core/src/client.rs` (`mod tests`) | `ensure_daemon_should_spawn_once_when_not_listening` | Unit | Happy path: `Io("Connection refused")` x2 then pong gives `Ok`, exactly 1 `spawn_daemon` | `cargo test -p stapler-mcp-core ensure_daemon_should_spawn_once_when_not_listening` |
| `crates/core/src/client.rs` | `ensure_daemon_default_timeout_should_be_30s` | Unit | Error path: `EnsureOptions::default()` against permanent refusal; fake clock reaches >= 30_000 ms before giving up | `cargo test -p stapler-mcp-core ensure_daemon_default_timeout_should_be_30s` |
| `crates/core/src/client.rs` | `ensure_daemon_should_not_spawn_when_first_ping_succeeds` | Unit | Regression: healthy daemon, 0 spawns, returns immediately | `cargo test -p stapler-mcp-core ensure_daemon_should_not_spawn_when_first_ping_succeeds` |
| `crates/thin-client/src/transport.rs` (`mod tests`) | `startup_timeout_should_cover_cold_chrome_start` | Unit | `STARTUP_TIMEOUT >= 30s` (45 s) so the real client path has margin over the AC budget | `cargo test -p stapler-mcp startup_timeout_should_cover_cold_chrome_start` |
| `crates/cli/tests/daemon_kill_restart.rs` | `sigkill_should_be_recovered_by_next_ensure_daemon_within_30s` | E2E | SIGKILL scenario (see assertions below) | `cargo test -p stapler-mcp-daemon --test daemon_kill_restart` |

**SIGKILL e2e assertions** (`sigkill_should_be_recovered_by_next_ensure_daemon_within_30s`, one env-mutating `#[tokio::test]` per file):
1. Start daemon via `ensure_daemon`, read PID `P1` from `daemon.lock`.
2. `libc::kill(P1, SIGKILL)`; poll until `client::ping` errors.
3. The first failing ping error is `matches!(err, PortError::Io(_))`, NOT `PortError::Timeout` (adversarial C3: proves "dead daemon" stays distinguishable from "busy daemon").
4. Stale `daemon.sock` still exists (the bug precondition).
5. `ensure_daemon` with `startup_timeout: Some(45s)`, wall time measured with `Instant`, asserted `< 30s` separately (AC0 budget, adversarial C3: avoids sitting on the edge of the call's own timeout).
6. New lock-file PID `P2 != P1`; `stapler_browser_list_sessions` via `client::call` returns `{"sessions":[]}`; finish with `shutdown`.

### AC1: N parallel cold-start calls all succeed; none returns "Connection refused"

| Test File | Test Name | Type | Scenario | Command |
|-----------|-----------|------|----------|---------|
| `crates/core/src/client.rs` | `ensure_daemon_should_spawn_once_when_not_listening` | Unit | Happy path (shared with AC0): one caller spawns once and polls to success | `cargo test -p stapler-mcp-core ensure_daemon_should_spawn_once_when_not_listening` |
| `crates/core/src/client.rs` | `ensure_daemon_should_not_spawn_when_ping_times_out_then_recovers` | Unit | Busy daemon (`Timeout`, `Timeout`, `Timeout`, pong): `Ok`, 0 spawns. Parallel callers queued behind a serial daemon must not spawn extra processes | `cargo test -p stapler-mcp-core ensure_daemon_should_not_spawn_when_ping_times_out_then_recovers` |
| `crates/core/src/client.rs` | `ensure_daemon_should_return_timeout_without_spawning_after_deadline` | Unit | Error path: `Timeout` forever, 30 s fake clock, returns `Err(PortError::Timeout)`, 0 spawns (never "Connection refused" for an alive daemon) | `cargo test -p stapler-mcp-core ensure_daemon_should_return_timeout_without_spawning_after_deadline` |
| `crates/core/src/client.rs` | `ensure_daemon_should_spawn_at_most_once_when_pings_keep_failing` | Unit | Error path: `Io(refused)` forever, 5 s timeout, returns last `Io` error after exactly 1 spawn | `cargo test -p stapler-mcp-core ensure_daemon_should_spawn_at_most_once_when_pings_keep_failing` |
| `crates/core/src/client.rs` | `ensure_daemon_should_spawn_once_when_refused_follows_initial_timeouts` | Unit | Review resolution 3: `Timeout, Timeout, Io(refused), pong` gives `Ok`, 1 spawn | `cargo test -p stapler-mcp-core ensure_daemon_should_spawn_once_when_refused_follows_initial_timeouts` |
| `crates/core/src/client.rs` | `ensure_daemon_should_respawn_once_when_daemon_vanishes_after_timeout_phase` | Unit | Review resolution 3 (adversarial C1): `Io, Timeout, Io, pong` gives `Ok`, 2 spawns (the daemon we waited on exited after losing the flock) | `cargo test -p stapler-mcp-core ensure_daemon_should_respawn_once_when_daemon_vanishes_after_timeout_phase` |
| `crates/core/src/client.rs` | `ensure_daemon_should_cap_total_spawns_at_two` | Unit | Review resolution 3: alternating `Io`/`Timeout` forever never exceeds 2 spawns | `cargo test -p stapler-mcp-core ensure_daemon_should_cap_total_spawns_at_two` |
| `crates/cli/tests/daemon_parallel_cold_start.rs` | `eight_parallel_cold_starts_should_all_succeed_without_connection_refused` | E2E | 8 concurrent `ensure_daemon` (`futures::future::join_all`, `startup_timeout: Some(45s)`) on an empty tempdir. Then 8 parallel short `client::call("stapler_browser_list_sessions")` (adversarial C5: short requests, far under the 120 s `CALL_TIMEOUT`) | `cargo test -p stapler-mcp-daemon --test daemon_parallel_cold_start` |

**Parallel cold-start e2e assertions**: all 8 `ensure_daemon` return `Ok`; any `Err` is printed and fails the test if it contains `Connection refused`; `daemon.lock` PID is alive (`libc::kill(pid, 0) == 0`); exactly one live daemon PID (the N-1 losers exit on the flock; plan wording is "one winner, N spawn attempts"); all 8 follow-up calls return `{"sessions":[]}`; one final `client::ping`; `shutdown`.

### AC2: Client disconnect mid-request: daemon logs the per-connection error and keeps serving

| Test File | Test Name | Type | Scenario | Command |
|-----------|-----------|------|----------|---------|
| `crates/core/src/daemon.rs` (`mod tests`) | `run_cancellable_should_keep_serving_after_a_connection_write_error` | Unit | Happy path: conn 1 `write_frame` fails `Io("Broken pipe (os error 32)")`; conn 2 sends `{"tool":"ping"}` and receives `{"result":{"pong":true}}`; `run_cancellable` returns `Ok(())` | `cargo test -p stapler-mcp-core run_cancellable_should_keep_serving_after_a_connection_write_error` |
| `crates/core/src/daemon.rs` | `run_cancellable_should_keep_serving_after_a_connection_read_error` | Unit | Error path: conn 1 `read_frame` fails (read timeout); conn 2 served | `cargo test -p stapler-mcp-core run_cancellable_should_keep_serving_after_a_connection_read_error` |
| `crates/core/src/daemon.rs` | `run_should_keep_serving_after_a_connection_write_error` | Unit | Same policy in `Daemon::run` (conn 2 sends `{"tool":"shutdown"}`, `run` returns `Ok(())`) | `cargo test -p stapler-mcp-core run_should_keep_serving_after_a_connection_write_error` |
| `crates/core/src/daemon.rs` | `run_should_return_err_when_accept_fails` | Unit | Error path: listener `accept()` returns `Err(Io("too many open files"))`; `run` returns that error (listener errors stay fatal) | `cargo test -p stapler-mcp-core run_should_return_err_when_accept_fails` |
| `crates/core/src/daemon.rs` | `serve_logged_should_return_unit_when_connection_fails` | Unit | Review resolution 5: `serve_logged` on a failing `ScriptedConn` returns normally (does not propagate); proves the swallow independent of the loop | `cargo test -p stapler-mcp-core serve_logged_should_return_unit_when_connection_fails` |
| `crates/cli/tests/daemon_client_disconnect.rs` | `abandoned_connections_should_not_kill_daemon_and_should_be_logged` | E2E | See assertions below | `cargo test -p stapler-mcp-daemon --test daemon_client_disconnect` |

**Disconnect e2e assertions**: start daemon, record PID `P`; open 20 raw `tokio::net::UnixStream`s to `daemon.sock`, write `{"tool":"ping"}\n` on all of them, then drop all without reading; `sleep(500ms)`; `client::ping` succeeds; `daemon.lock` still holds `P` and `kill(P, 0) == 0`; `daemon.log` contains `connection error (continuing)` (review resolution 5, adversarial C6, architecture AC2-log concern: the only automated check of the "logs the error" clause). The unit tests deliberately do not assert on stderr output.

### AC3: `stapler_daemon_status` returns a clear diagnostic when the daemon is unreachable

| Test File | Test Name | Type | Scenario | Command |
|-----------|-----------|------|----------|---------|
| `crates/thin-client/tests/tool_schema.rs` | `daemon_status_should_deserialize_run_daemons_camelcase_wire_keys` (existing, must pass unchanged) | Unit | Happy path / regression: `FakePingTransport` returns running-state JSON; `pong == true`, mode and warning strings identical, `error`/`socket_path`/`log_path` are `None` | `cargo test -p stapler-mcp --test tool_schema daemon_status_should_deserialize_run_daemons_camelcase_wire_keys` |
| `crates/thin-client/tests/tool_schema.rs` | `daemon_status_should_report_down_instead_of_erroring` | Unit | Error path: `FailingTransport::call` returns `Err("ensure daemon: io error: Connection refused (os error 111) (...)")`; `call_daemon_status_for_test()` returns `Ok(DaemonStatusOutput { pong: false, error: Some(..Connection refused..), .. })`, not `Err` | `cargo test -p stapler-mcp --test tool_schema daemon_status_should_report_down_instead_of_erroring` |
| `crates/core/src/schema.rs` (`mod tests`) | `daemon_status_output_unreachable_should_serialize_error_and_paths` | Unit | Review resolution 2: `DaemonStatusOutput::unreachable(error, socket, log)` yields `pong:false` and camelCase `error`, `socketPath`, `logPath` | `cargo test -p stapler-mcp-core daemon_status_output_unreachable_should_serialize_error_and_paths` |
| `crates/core/src/schema.rs` | `daemon_status_output_running_should_serialize_unchanged_without_error_fields` | Unit | Additive-schema guard: running output has no `error`/`socketPath`/`logPath` keys (`skip_serializing_if`) | `cargo test -p stapler-mcp-core daemon_status_output_running_should_serialize_unchanged_without_error_fields` |
| `crates/thin-client/src/transport.rs` | `status_from_ping_error_should_say_not_running_on_refused` | Unit | `Io("Connection refused")` gives `pong:false`, error says daemon not running, includes both paths, neutral prefix (not "ensure daemon:"; adversarial minor) | `cargo test -p stapler-mcp status_from_ping_error_should_say_not_running_on_refused` |
| `crates/thin-client/src/transport.rs` | `status_from_ping_error_should_say_busy_on_timeout` | Unit | Review resolution 2 (adversarial C4, architecture): `PortError::Timeout` gives `pong:false`, error says "daemon busy: ping timed out", does NOT say "failed to start" or "not running", paths included | `cargo test -p stapler-mcp status_from_ping_error_should_say_busy_on_timeout` |
| `crates/thin-client/tests/status_probe_no_spawn.rs` | `socket_transport_status_probe_should_not_spawn_and_should_report_paths` | Integration | Empty tempdir `STAPLER_MCP_HOME`, no daemon: `SocketTransport.status_probe()` returns `pong:false`, `socketPath` ends `/daemon.sock`, `logPath` ends `/daemon.log`; afterwards no `daemon.lock` and no `daemon.sock` exist (ADR-002 no auto-spawn). Own file because it mutates env | `cargo test -p stapler-mcp --test status_probe_no_spawn` |

The helper name `status_from_ping_error` is provisional: the implementer may name the pure classifier differently, but the two behaviors (refused gives "not running", `Timeout` gives "busy") must each keep a named test. Compile gate for review resolution 1 (defaulted `status_probe` Send/Sync): `cargo check -p stapler-mcp -p stapler-mcp-daemon` is the first step of task 3.1.2b.

### AC4: When `ensure_daemon` fails, the error message includes the socket path and daemon log path

| Test File | Test Name | Type | Scenario | Command |
|-----------|-----------|------|----------|---------|
| `crates/thin-client/src/transport.rs` | `ensure_error_message_should_include_socket_and_log_paths` | Unit | Happy path of the formatter: `Io("Connection refused (os error 111)")` with `sock_path=/home/alice/.stapler-mcp/daemon.sock`, `log_path=.../daemon.log`; result contains `ensure daemon: io error: Connection refused (os error 111)`, `socket: /home/alice/.stapler-mcp/daemon.sock`, `daemon log: /home/alice/.stapler-mcp/daemon.log`, and the hint | `cargo test -p stapler-mcp ensure_error_message_should_include_socket_and_log_paths` |
| `crates/thin-client/src/transport.rs` | `ensure_error_message_should_explain_timeout_separately` | Unit | Error path: `PortError::Timeout` message says the daemon is reachable but did not answer in time (user is not told it failed to start), paths still present | `cargo test -p stapler-mcp ensure_error_message_should_explain_timeout_separately` |
| `crates/thin-client/tests/status_probe_no_spawn.rs` | `socket_transport_status_probe_should_not_spawn_and_should_report_paths` | Integration | Real `SocketTransport` + real `paths::socket_path/log_path` resolution: the paths in the output are the ones the formatter embeds (shared with AC3) | `cargo test -p stapler-mcp --test status_probe_no_spawn` |
| `crates/thin-client/tests/ensure_error_paths.rs` (optional, add if feasible) | `socket_transport_call_should_include_paths_when_daemon_cannot_start` | Integration | `SocketTransport.call` in a tempdir `STAPLER_MCP_HOME` where the daemon exe cannot be spawned/started: the returned `Err(String)` contains the tempdir `daemon.sock` and `daemon.log` paths. Feasibility depends on `daemon_locator::daemon_exe_path()` not resolving to a real daemon in the thin-client test environment; confirm during implementation, otherwise drop (the two unit tests plus the probe integration already cover the formatter and path resolution). Own file (env mutation) | `cargo test -p stapler-mcp --test ensure_error_paths` |

## Review-Resolution Traceability

| Plan resolution | Covered by |
|-----------------|-----------|
| 1. Trait default compiles (`status_probe` Send/Sync) | `cargo check -p stapler-mcp -p stapler-mcp-daemon` gate; `daemon_status_should_*` tests in `tool_schema.rs` compile against `FakePingTransport` unchanged |
| 2. Busy daemon not reported as down; `unreachable(..)` constructor | `status_from_ping_error_should_say_busy_on_timeout`, `daemon_status_output_unreachable_should_serialize_error_and_paths` |
| 3. Respawn after lost flock, total spawns capped at 2 | `ensure_daemon_should_respawn_once_when_daemon_vanishes_after_timeout_phase`, `ensure_daemon_should_cap_total_spawns_at_two`, `ensure_daemon_should_spawn_once_when_refused_follows_initial_timeouts` |
| 4. Kill test asserts error kind | step 3 of `sigkill_should_be_recovered_by_next_ensure_daemon_within_30s` |
| 5. Logging is tested | `serve_logged_should_return_unit_when_connection_fails`, log assertion in `abandoned_connections_should_not_kill_daemon_and_should_be_logged` |
| 6. AC1 verified with N=8 short requests | follow-up 8 parallel `list_sessions` calls in `eight_parallel_cold_starts_should_all_succeed_without_connection_refused` |
| 7. Silent-client stall stays a documented limit | not tested (out of scope: concurrent handling) |

## TDD Red/Green Expectations

Write Phase 4 e2e tests first and confirm against the pre-fix tree:
- `abandoned_connections_should_not_kill_daemon_and_should_be_logged` fails (daemon exits on the first EPIPE, F1/F2; no `connection error (continuing)` line).
- `sigkill_should_be_recovered_by_next_ensure_daemon_within_30s` may pass or fail pre-fix depending on Chrome speed vs the old 10 s timeout; the unit test `ensure_daemon_default_timeout_should_be_30s` is the deterministic red for the timeout.
- `ensure_daemon_should_not_spawn_when_ping_times_out_then_recovers` fails pre-fix (old code spawns on any ping failure, F4).
- `daemon_status_should_report_down_instead_of_erroring` fails pre-fix (returns `Err`, F9).

## UX Acceptance Tests

N/A: pure infrastructure, no user-facing UI surface (`design/ux.md` does not exist). The tool-output change (`stapler_daemon_status` down shape) is covered by AC3 tests above.

## Migration Test

N/A (no schema/data migration; `DaemonStatusOutput` fields are additive and optional).

## Test Stack
- **Unit**: Rust built-in `#[test]` and `#[tokio::test]` (core dev-deps: `tokio` with `rt, macros, time`); hand-written fakes implementing the `ports.rs` traits (`SocketFactory` with associated types `L` and `C`, `Conn`, `ProcessSpawner`, `SleepPort`, `ClockPort`), `Cell`-based spawn counters, a fake clock advanced only by the fake sleeper (no `tokio::test-util`); `std::assert!`/`assert_eq!`.
- **Integration**: `tests/*.rs` in `stapler-mcp` using real `SocketTransport`/`paths` with `tempfile` dirs and `STAPLER_MCP_HOME`; one env-mutating `#[tokio::test]` per file (contract from `daemon_ping.rs` header).
- **E2E**: `tests/*.rs` in `stapler-mcp-daemon` using `env!("CARGO_BIN_EXE_stapler-mcp-daemon")`, ephemeral Chrome profile, `libc` (new dev-dep `libc = "0.2"`) for `kill`, `futures::future::join_all` for parallelism.
- **Compile gates**: `cargo check -p stapler-mcp-wasm` (and `--target wasm32-unknown-unknown` if the target is installed) because `crates/wasm` calls `ensure_daemon` and shares `Daemon::run`.

## Coverage Targets and How to Measure

| Stack | Coverage command | Target |
|---|---|---|
| Rust | `cargo tarpaulin -p stapler-mcp-core -p stapler-mcp --out Stdout` (tarpaulin availability not verified in this repo) | >= 80% line on `crates/core/src/client.rs`, the new `serve_logged` path in `crates/core/src/daemon.rs`, and the new code in `crates/thin-client/src/transport.rs` |

- All new/changed public functions (`ensure_daemon`, `serve_logged`, `ensure_error_message`, `status_probe`, `DaemonStatusOutput::unreachable`): happy path + error path covered by named tests above.
- External integrations (daemon process, socket, Chrome): unit tests use fakes; at least one e2e per AC0, AC1, AC2.
- Run order for the full suite: `cargo test -p stapler-mcp-core`, then `cargo test -p stapler-mcp`, then `cargo test -p stapler-mcp-daemon --test daemon_kill_restart --test daemon_parallel_cold_start --test daemon_client_disconnect --test daemon_ping`, then `make ready`.

## Test Counts

| Kind | Count | Notes |
|------|------:|-------|
| Unit | 21 | core `daemon.rs` 5, core `client.rs` 9, core `schema.rs` 2, thin-client `transport.rs` 5 |
| Integration | 2 new (+1 optional) | `daemon_status_should_report_down_instead_of_erroring` (`tool_schema.rs`, fake transport), `socket_transport_status_probe_should_not_spawn_and_should_report_paths`; optional `socket_transport_call_should_include_paths_when_daemon_cannot_start` |
| Existing regression (not counted) | 1 | `daemon_status_should_deserialize_run_daemons_camelcase_wire_keys` must pass unchanged |
| E2E | 3 | kill/restart, 8-parallel cold start, client disconnect |
| UX / Migration | 0 / N/A | infrastructure only |
| **Total new** | 26 firm (+1 optional) | |

**Requirements covered: 5/5** (per-AC test mappings, some tests shared across ACs: AC0: 5, AC1: 8, AC2: 6, AC3: 7 incl. the existing regression, AC4: 4 incl. one shared and one optional).

## Known Residual Risks (testing)

1. **Log-line assertion race** (AC2 e2e): the daemon is serial and idle, so it may answer the first ping before the test drops its stream; the rest hit EPIPE on a closed Unix peer (writes to a closed Unix stream fail immediately). With 20 connections at least one error is expected; INFERRED, not run. If flaky, hold all 20 open until every frame is written, then drop together.
2. **E2E environment**: the three e2e tests need real headless Chrome like `daemon_ping.rs`; on a machine without it they cannot go green (blocker named in plan.md Effort estimate).
3. **Timing**: the 30 s AC0 assertion depends on host speed; the deterministic proof of the budget is the unit test on the fake clock, the e2e is the live check.
4. **AC1 long requests**: more than about 4 queued 30 s `read_website` calls can still hit the 120 s client `CALL_TIMEOUT` (plan Known limit 1); intentionally not tested.
