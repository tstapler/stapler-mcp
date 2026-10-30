# read-website-daemon-recovery — Requirements

Source: backlog item 100b6198-300b-4840-a2e9-a92328ac87ed (tstapler/stapler-mcp#55).

## Problem

`read_website` and `stapler_daemon_status` fail with
`ensure daemon: io error: Connection refused (os error 111)` after the first call in a
session. The first call succeeds; later calls, including parallel ones from subagents, fail.
The `ensure daemon` step does not recover.

## Impact

Parallel research subagents fell back to WebFetch/WebSearch and cited sources only via
summaries, weakening sourcing.

## Root cause (verified against code at c61d2f6)

- `crates/core/src/daemon.rs:160` and `:187` propagate `serve_connection` errors with `?`;
  `crates/cli/src/main.rs:964-966` exits the process on any `run_cancellable` error. A client
  that disconnects before the daemon writes its response (EPIPE/timeout) therefore kills the
  daemon, leaving a stale socket (ECONNREFUSED).
- `crates/thin-client/src/transport.rs:51-54` uses the default 10 s startup timeout, which
  can be too short for a cold Chrome start.
- `transport.rs:57` error omits socket and log paths.

## Acceptance criteria

0. After the daemon is killed (SIGKILL), the next `read_website` call restarts it and
   succeeds within 30 s on a cold Chrome start.
1. N parallel `read_website` calls on a cold start (daemon not running) all eventually
   succeed; none returns "Connection refused".
2. When a client disconnects mid-request (timeout, process kill), the daemon logs the
   per-connection error and continues serving subsequent connections without exiting.
3. `stapler_daemon_status` does not crash or return an unhandled error when the daemon is
   unreachable; it returns a clear diagnostic result.
4. When `ensure_daemon` fails, the error message includes the socket path and daemon log
   path.

(Numbering matches `/backlog/done-N`; the backlog tool displays these as 1-5.)

## Out of scope

- Multi-threaded or concurrent request handling in the daemon.
- Watchdog / health monitoring.
- The HTTP (`ChannelTransport`) path.
