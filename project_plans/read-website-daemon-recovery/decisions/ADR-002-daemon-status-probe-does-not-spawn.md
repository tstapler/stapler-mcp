# ADR-002: `stapler_daemon_status` pings without spawning and reports "down" as a result

**Status**: Proposed (read-website-daemon-recovery, plan Task 3.1.2b)

## Context

`stapler_daemon_status` calls `transport.call`, which runs `ensure_daemon` and auto-spawns, then returns `Err(String)` if startup fails (`crates/thin-client/src/mcp_router.rs:866-877`, pitfalls.md (b)). AC3 requires a clear diagnostic result when the daemon is unreachable. The CLI `--status` already pings without spawning (`crates/cli/src/main.rs:101-128`).

## Decision

Add a defaulted `DaemonTransport::status_probe() -> DaemonStatusOutput`. `SocketTransport` overrides it with a single 2s `client::call` ping (no `ensure_daemon`). Failure returns `pong: false` plus new optional `error`, `socketPath`, `logPath` fields. Other transports use the default (delegates to `call`).

## Alternatives rejected

- **Keep auto-spawn and only map `Err` to a down result**: status would still launch Chrome as a side effect, and its answer could be "down" only after a 45s wait.
- **New output enum / separate type**: breaks the existing outputSchema consumers; additive optional fields keep the running-state JSON unchanged.

## Consequences

- Behavior change: calling status on a stopped daemon reports `pong: false` instead of starting it. The next real tool call starts it. Called out in the PR description.
- `DaemonStatusOutput` gains three optional fields; its outputSchema (derived by schemars) grows accordingly.
