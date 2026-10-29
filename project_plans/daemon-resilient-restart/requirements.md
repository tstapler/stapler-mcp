# daemon-resilient-restart — Requirements

## Problem

`read_website` (and `stapler_daemon_status`) fail with a connection error when the daemon
isn't reachable:

```
ensure daemon: io error: Connection refused (os error 111)
```

Observed 2026-10-28 on Linux (Manjaro). The first `read_website` call in a session
succeeded; subsequent calls, including from several parallel subagents, failed with the
same error. `stapler_daemon_status` then also failed, so the "ensure daemon" step is not
(re)starting the daemon.

## Root-cause analysis (pre-implementation investigation)

Two distinct defects cooperate to produce the observed failure.

### Defect 1 — Per-connection I/O errors crash the daemon (P1)

`Daemon::serve_connection` in `crates/core/src/daemon.rs` propagates connection-level
errors (broken pipe, timeout) upward via `?`. In both `run` and `run_cancellable` the
caller does the same:

```rust
self.serve_connection(conn).await?;   // propagates to run_cancellable, then run_daemon
```

`run_daemon` exits on any error from `run_result`. This means:

1. Parallel callers each call `ensure_daemon`, which pings with `POLL_CAP = 500 ms`.
2. The daemon is busy serving a long `read_website` request; the OS accept-backlog
   queues the ping connections.
3. The clients' 500 ms timeout expires; each client drops its connection.
4. The daemon eventually `accept`s those dropped connections, reads the stale ping
   request, then tries to `write_frame` the response to a socket the client already
   closed — gets `EPIPE` / BrokenPipe.
5. The error propagates up and exits the daemon process.

After the crash, subsequent calls see "Connection refused" (socket file exists, no
process listening) and `ensure_daemon` tries to restart the daemon — but the restart
succeeds too slowly for the 10-second `startup_timeout`, so all parallel callers fail.

### Defect 2 — Startup timeout too short for Chrome cold-start (P2)

`SocketTransport::call` in `crates/thin-client/src/transport.rs` passes
`EnsureOptions::default()`, which uses `DEFAULT_STARTUP_TIMEOUT = 10 s`. The daemon
launches a headless Chrome browser at startup (`NativeBrowser::launch`). On a cold
start or after a crash this can exceed 10 s. The integration test in
`crates/cli/tests/daemon_ping.rs` hard-codes 30 s for exactly this reason — production
code should match.

### Defect 3 — Error message omits socket path and log path (P3)

When `ensure_daemon` fails the error is `"ensure daemon: {e}"` with no hint about where
the socket is or where daemon logs are written. This makes diagnosing failures harder.

## Acceptance criteria

1. After the daemon is killed (SIGKILL), the next `read_website` call restarts it and
   succeeds within a reasonable timeout (≤ 30 s on cold Chrome start).
2. N parallel `read_website` calls on a cold start (daemon not running) all eventually
   succeed; none returns "Connection refused".
3. When a client disconnects mid-request (timeout, process kill), the daemon logs the
   per-connection error and continues serving subsequent connections — it does not exit.
4. `stapler_daemon_status` reports state accurately and does not itself error when the
   daemon is unreachable (returns a clear "not running" result or error, not a crash).
5. When `ensure_daemon` fails, the error message includes the socket path and daemon log
   path so operators and agents can diagnose the failure without additional tooling.

## Out of scope

- Making the daemon multi-threaded or supporting concurrent request handling.
- Automatic daemon health monitoring / watchdog.
- Changes to the HTTP transport path (`ChannelTransport`).
