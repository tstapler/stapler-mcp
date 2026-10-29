# daemon-resilient-restart — Research

## Codebase map

| File | Role |
|---|---|
| `crates/core/src/daemon.rs` | `Daemon` struct, `run`/`run_cancellable`/`serve_connection` |
| `crates/core/src/client.rs` | `ensure_daemon`, `call`, `ping` — thin-client side |
| `crates/core/src/ports.rs` | Port traits: `SocketFactory`, `ProcessSpawner`, `SleepPort`, `ClockPort`, `ProcessLock` |
| `crates/core/src/paths.rs` | Path helpers: `socket_path`, `log_path`, `lock_path`, `base_dir` |
| `crates/native/src/socket.rs` | `NativeSocketFactory` / `NativeConn` — Unix socket impl |
| `crates/native/src/spawn.rs` | `NativeSpawner::spawn_daemon` — process fork+exec |
| `crates/native/src/lock.rs` | `NativeLock` — advisory flock over `daemon.lock` |
| `crates/thin-client/src/transport.rs` | `SocketTransport::call` — the MCP tool call path |
| `crates/thin-client/src/daemon_locator.rs` | Binary path resolution, `lookup_hint()` |
| `crates/cli/src/main.rs` | `run_daemon()` — full startup wiring + accept loop |
| `crates/cli/tests/daemon_ping.rs` | Integration test: end-to-end daemon lifecycle |

## Key constants

```rust
// client.rs
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const DIAL_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_START: Duration = Duration::from_millis(50);
const POLL_CAP: Duration = Duration::from_millis(500);

// daemon.rs
const CONN_TIMEOUT: Duration = Duration::from_secs(120);
#[cfg(not(test))]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// transport.rs
const CALL_TIMEOUT: Duration = Duration::from_secs(120);
```

## Failure mode trace

### Crash via EPIPE (Defect 1)

1. Daemon is busy handling `read_website` (up to 30 s via `REQUEST_TIMEOUT`).
2. Six parallel callers each invoke `ensure_daemon`:
   - Initial ping (`DIAL_TIMEOUT = 2 s`): `UnixStream::connect` succeeds immediately
     (OS accept-backlog accepts the SYN without the application calling `accept()`).
   - Client writes the ping request to the socket buffer.
   - Client waits up to `DIAL_TIMEOUT / POLL_CAP` (2 s / 500 ms) for a response.
   - Daemon is blocked in `serve_connection` serving the first call; the queued
     connections are not `accept()`ed.
   - Client timeout fires; the client-side `NativeConn` is dropped, closing the socket.
3. Daemon finishes the first call, loops back to `accept()`, dequeues the timed-out
   connection.
4. Daemon's `serve_connection` reads the stale ping request (it was already buffered).
5. Daemon calls `write_frame` (response) on the now-closed socket → `EPIPE` →
   `PortError::Io("...")`.
6. `serve_connection` propagates the error via `?`; `run_cancellable` propagates it via
   `?`; `run_daemon` calls `std::process::exit(1)`.
7. Socket file remains but no process listens; all subsequent `connect()` calls get
   `ECONNREFUSED`.

### Restart race on cold start (Defect 2)

When the daemon is not running (or just crashed as above):
1. N parallel callers all fail the initial ping with `ECONNREFUSED`.
2. All N call `spawner.spawn_daemon(...)` → N daemon processes fork.
3. Only one wins `NativeLock::acquire_exclusive`; the rest print "daemon already
   running" and return immediately (clean).
4. The winner removes the stale socket, binds a new one, then starts launching Chrome
   (`NativeBrowser::launch`).
5. Meanwhile, all N callers are in the polling loop with `DEFAULT_STARTUP_TIMEOUT = 10 s`.
6. Chrome cold-start on Linux can exceed 10 s; the polling loop times out before Chrome
   is ready.
7. `ensure_daemon` returns `Err(last_err)` where `last_err` is the last
   `PortError::Timeout` from the ping (displayed as "Connection refused" or "timeout").

The integration test works because it passes `startup_timeout: Some(Duration::from_secs(30))`.

## Prior art / patterns

### Unix daemon resilience

The standard pattern for a Unix accept loop is to catch per-connection errors and log
them, never propagating them to the outer loop. Errors on an individual connection
should not affect unrelated connections. This is analogous to how TCP servers in
production (nginx, tokio's `accept_many`, hyper) handle per-accept errors.

### `tokio` Unix socket accept patterns

`tokio::net::UnixListener::accept()` itself only fails on a system-level error (e.g.,
file-descriptor exhaustion), not on a per-connection error. The pattern is:
```rust
loop {
    let conn = listener.accept().await?;  // system error: exit loop
    tokio::task::spawn_local(async move {
        if let Err(e) = serve(conn).await {
            eprintln!("connection error: {e}");  // per-connection: log and continue
        }
    });
}
```

Since the daemon is single-threaded (one connection at a time), spawning is not
applicable here, but the error-handling principle still applies: swallow per-connection
errors at the loop level.

### flock-based daemon serialization

The existing `NativeLock` / `daemon.lock` pattern correctly handles multiple concurrent
spawn attempts. The race is benign (losers exit cleanly). No additional serialization
is needed at the process level.

## Open questions

1. Should `serve_connection` errors be distinguished by kind? An `EPIPE` (client hung
   up) is expected in the timeout scenario; a listener-level error (e.g., `EBADF`) is
   fatal. The simplest correct approach is: `serve_connection` errors are non-fatal by
   definition; listener-level errors (from `accept()`) propagate as today.

2. Should `DEFAULT_STARTUP_TIMEOUT` be increased globally, or only in `SocketTransport`?
   Increasing the constant itself affects every test that relies on it; patching only
   `SocketTransport`'s `EnsureOptions` is a smaller, safer change.

3. Should `stapler_daemon_status` propagate `ensure_daemon` errors or swallow them with
   a "not running" payload? The current implementation calls `transport.call(PING_TOOL)`
   which runs `ensure_daemon` first — if the daemon is down, it will try to start it.
   For a diagnostics tool, starting the daemon on status check seems reasonable (matches
   the pattern for all other tools); only the error message needs improvement.
