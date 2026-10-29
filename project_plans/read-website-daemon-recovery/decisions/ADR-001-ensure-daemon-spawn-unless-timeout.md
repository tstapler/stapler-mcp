# ADR-001: `ensure_daemon` spawns on any failure except `PortError::Timeout`

**Status**: Proposed (read-website-daemon-recovery, plan Task 2.1.1a)

## Context

`ensure_daemon` (`crates/core/src/client.rs:78`) treats every ping failure as "daemon down" and spawns. A busy single-threaded daemon (serving a long `read_website`) makes the 2s ping return `PortError::Timeout`, so callers spawn useless processes and fail at the 10s poll deadline (pitfalls.md (a)). It must spawn only when nothing is listening.

`NativeSocketFactory::connect` yields `PortError::Io(String)` for ENOENT/ECONNREFUSED and `Timeout` for connect/read timeouts (`crates/native/src/socket.rs:96-101,38-40`). The wasm adapter, which also calls `ensure_daemon` (`crates/wasm/src/lib.rs:370`), only ever produces `Timeout` or `Io(msg)` (`crates/wasm/src/socket.rs:30-37`).

## Decision

Branch on the existing typed `PortError::Timeout`: Timeout means "alive but busy" (wait, do not spawn); any other error means "not listening" (spawn once). No new `PortError` variant.

## Alternatives rejected

- **New `PortError::Unreachable` variant**: only the native adapter would produce it; the wasm adapter would never spawn after this change.
- **Match on `Io(msg)` text** (`"Connection refused"`, `"No such file"`): locale/OS-dependent and fragile.

## Consequences

- A genuinely wedged daemon (alive, never answers) is not respawned; the caller gets `Timeout` after the startup deadline with the socket/log paths in the message (AC4). Watchdog/restart of a wedged daemon is out of scope.
- A spurious extra spawn on a non-Timeout `Io` error (e.g. a connection reset) is harmless: the loser exits on the flock (`crates/cli/src/main.rs:344-349`).
