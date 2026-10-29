# daemon-resilient-restart — Implementation Plan

## Overview

Three targeted changes, each independently releasable and testable:

1. **Fix `serve_connection` to not crash the daemon on per-connection I/O errors** (the
   primary bug).
2. **Increase `startup_timeout` in `SocketTransport`** to match the integration test.
3. **Improve `ensure_daemon` error messages** with socket and log paths.

---

## Change 1 — Swallow per-connection errors in the accept loop

**File**: `crates/core/src/daemon.rs`

**Change**: In `run` and `run_cancellable`, replace the `?` propagation of
`serve_connection` with a `match` that logs the error and continues the loop.

### Before (both variants share the same pattern)

```rust
self.serve_connection(conn).await?;
```

### After

```rust
if let Err(e) = self.serve_connection(conn).await {
    eprintln!("stapler-mcp: connection error (ignored): {e}");
    // Per-connection I/O errors (EPIPE, timeout) must not terminate the
    // accept loop.  Listener-level errors (from accept() above) still
    // propagate via ? and exit the loop as today.
}
```

This matches the Unix convention for accept loops: connection errors are non-fatal;
listener-level errors (returned by `accept()` itself) are fatal.

**Test to add** (`daemon.rs` unit tests):

```
run_should_continue_accepting_after_a_connection_error
```

Use a mock `SocketFactory` whose `Listener` returns:
- First `accept()`: a `Conn` whose `read_frame()` returns `Err(PortError::Io("broken
  pipe".into()))`.
- Second `accept()`: a `Conn` that successfully handles a ping and triggers shutdown.

Assert that `run()` returns `Ok(())` (not an error from the first connection).

---

## Change 2 — Increase startup_timeout in SocketTransport

**File**: `crates/thin-client/src/transport.rs`

**Change**: Pass an explicit `startup_timeout: Some(Duration::from_secs(30))` instead
of using the 10 s default.

```rust
client::ensure_daemon(
    &socket, &spawner, &sleeper, &clock,
    &sock_path, &log_path,
    EnsureOptions {
        exe_hint: Some(daemon_locator::daemon_exe_path()),
        startup_timeout: Some(Duration::from_secs(30)),  // was: default (10 s)
    },
)
.await
.map_err(|e| format!("ensure daemon: {e} ({})", daemon_locator::lookup_hint()))?;
```

Rationale: Chrome cold-start on Linux can exceed 10 s; the integration test already
uses 30 s for this reason.

**No new test required**: the integration test (`daemon_ping.rs`) already covers
end-to-end startup timing with a 30 s budget.

---

## Change 3 — Include socket and log paths in ensure_daemon error

**File**: `crates/thin-client/src/transport.rs`

**Change**: Extend the error message in `SocketTransport::call` to include both paths.

```rust
.map_err(|e| format!(
    "ensure daemon: {e} (socket: {sock_path}, log: {log_path}, {})",
    daemon_locator::lookup_hint()
))?;
```

This requires moving the `map_err` after the paths are computed (they already are, just
before the `ensure_daemon` call). No behavioral change; diagnostics only.

---

## Additional hardening (optional, lower priority)

### `POLL_CAP` too short for a busy daemon

When a long `read_website` is in flight, `ensure_daemon`'s polling pings with 500 ms
timeout fail, leaving queued connections that are read by the daemon after they've
timed out. After Change 1, these cause per-connection errors that are now logged and
skipped instead of crashing the daemon. The fundamental behavior (the daemon handles
the abandoned ping and moves on) is still slightly wasteful but no longer dangerous.

If desired, a future change could increase `POLL_CAP` to 2 s in `client.rs` to reduce
the frequency of abandoned connections. This is a lower-priority improvement given
Change 1 makes the existing behavior safe.

---

## Test plan

| Test | Kind | Target file |
|---|---|---|
| `run_should_continue_accepting_after_a_connection_error` | Unit | `crates/core/src/daemon.rs` |
| `run_cancellable_should_continue_accepting_after_a_connection_error` | Unit | `crates/core/src/daemon.rs` |
| Existing `daemon_architecture_and_tools_round_trip` | Integration | `crates/cli/tests/daemon_ping.rs` |
| `daemon_should_recover_after_kill_and_restart` (new) | Integration | `crates/cli/tests/daemon_ping.rs` |

The new integration test:
1. Starts the daemon via `ensure_daemon`.
2. Sends a ping to confirm it's running and records the PID from the lock file.
3. `SIGKILL`s the daemon process.
4. Calls `ensure_daemon` again (expect it to restart the daemon within 30 s).
5. Sends a ping; asserts success and a different PID in the lock file.

---

## Risk and rollback

- All three changes are backward-compatible (no API changes, only error-handling and
  timeout values).
- Change 1 is the highest-value fix; Changes 2 and 3 can land independently.
- Rollback: revert the three targeted diffs; no schema or protocol changes to undo.

---

## Adversarial review

**Challenge**: "Swallowing `serve_connection` errors might hide a real bug — e.g., if
`write_frame` fails because the socket is full rather than because the client is gone."

**Response**: `PortError::Io` is logged (not silently dropped), so the error is
visible in the daemon log. The daemon log path is now included in `ensure_daemon` error
messages (Change 3), making it findable. Full details (`e.to_string()`) are preserved
in the log.

**Challenge**: "Increasing the startup timeout to 30 s means a failed cold start keeps
callers blocked for 30 s instead of 10 s."

**Response**: Correct and acceptable. A failed start returns a clear error with the
socket/log paths (Change 3). The 10 s timeout was producing false failures on successful
but slow starts, which is worse.

**Challenge**: "The per-connection eprintln in a hot loop will spam the daemon log."

**Response**: Connection errors are rare (they require the client to disconnect mid-
request or time out). Even in the parallel-callers scenario the total number of
abandoned pings is bounded by the number of callers (typically ≤ 20 in practice).
If this becomes a concern, a log-rate limiter can be added later.
