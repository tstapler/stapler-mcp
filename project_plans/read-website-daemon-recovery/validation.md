# daemon-resilient-restart — Validation

## AC coverage matrix

| AC | Covered by | Kind |
|---|---|---|
| 1. Daemon restarts within 30 s after SIGKILL | `daemon_should_recover_after_kill_and_restart` (new) | Integration |
| 2. N parallel cold-start calls all succeed | `daemon_architecture_and_tools_round_trip` (existing, step 4 proves no second daemon) + parallel-callers variant (new) | Integration |
| 3. Daemon survives per-connection I/O errors | `run_should_continue_accepting_after_a_connection_error` + `run_cancellable_should_continue_accepting_after_a_connection_error` (new) | Unit |
| 4. `stapler_daemon_status` reports state without crashing | `tool_schema.rs` + `daemon_ping.rs` existing tests | Unit / Integration |
| 5. Error messages include socket path and log path | assert on error string in transport layer test (new) | Unit |

## Pre-conditions for implementation

- [ ] `crates/core/src/daemon.rs` — understand the `!Send` + `LocalSet` constraint
  before any change to ensure no `tokio::spawn` (only `spawn_local`) is introduced.
- [ ] The mock `SocketFactory` pattern in `daemon.rs`'s existing tests is sufficient to
  test the error-swallowing fix without a real Unix socket.
- [ ] The integration test in `daemon_ping.rs` requires the `stapler-mcp-daemon` binary
  (`env!("CARGO_BIN_EXE_stapler-mcp-daemon")`). Changes must build the `cli` crate.

## Risks

| Risk | Likelihood | Mitigation |
|---|---|---|
| `eprintln` in daemon hot path spams logs | Low | Connection errors require client disconnect; bounded in practice |
| 30 s timeout increases CI wall-clock time | Low | Integration test already uses 30 s for ensure_daemon; no change there |
| Chrome cold-start still exceeds 30 s on slow CI | Medium | Test uses `CARGO_BIN_EXE_stapler-mcp-daemon` (pre-built); browser is already launched before `run` — 30 s is for the socket to appear, not for Chrome |
| Increasing `POLL_CAP` (optional) causes backcompat issues | N/A | That change is optional/future |

## Manual verification steps (for the primary bug)

1. Run `stapler-mcp-daemon &` to start the daemon.
2. Run a long crawl that will hold the daemon for > 2 s.
3. In parallel, run 5 `read_website` calls against different URLs.
4. Confirm all 5 succeed (previously: failed with "Connection refused").
5. `kill -9 $(cat ~/.stapler-mcp/daemon.lock)` to SIGKILL the daemon.
6. Run a single `read_website` call.
7. Confirm it auto-restarts and succeeds within 30 s.
8. `stapler_daemon_status` returns a clear result in all states.

## Acceptance criteria sign-off checklist

- [ ] `cargo test -p stapler-mcp-core` passes (unit tests for daemon.rs)
- [ ] `cargo test -p stapler-mcp-cli -- daemon_architecture` passes (integration)
- [ ] New `daemon_should_recover_after_kill_and_restart` integration test passes
- [ ] No regressions in `crates/cli/tests/` test suite
- [ ] Error message observed via `stapler_daemon_status` when daemon is down includes
  socket path
