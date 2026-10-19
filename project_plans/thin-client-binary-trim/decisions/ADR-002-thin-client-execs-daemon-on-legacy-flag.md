# ADR-002: `stapler-mcp --daemon` execs the sibling `stapler-mcp-daemon`

**Status**: Accepted
**Date**: 2026-10-18

## Context

Before the split, `stapler-mcp --daemon` started the daemon. The requirements constrain the thin client to stay "a drop-in replacement for the existing `stapler-mcp` invocation (same argv interface)". After the split `crates/thin-client/src/main.rs` parses no arguments, so `stapler-mcp --daemon` would start a stdio thin client instead of a daemon. Existing launchd/systemd units, scripts or older thin clients that spawn `current_exe() --daemon` would break silently (`research/architecture.md` section 1, risk "High for existing users").

## Decision

`main()` checks argv before building the Tokio runtime. If any argument equals `--daemon`, the thin client replaces itself with `stapler-mcp-daemon` via `std::os::unix::process::CommandExt::exec`, passing the original arguments through unchanged (the daemon already accepts and ignores `--daemon`). The target is resolved by the same function the auto-start path uses (`daemon_exe_path`: canonicalized sibling of `current_exe()`, else bare name via `PATH`). If the exec fails, print `stapler-mcp: cannot exec stapler-mcp-daemon (looked beside <exe>, then PATH): <error>` to stderr and exit 127.

## Consequences

- Costs std only: no extra linked code, no change to the daemon, no change to the MCP tool surface.
- `exec` (not spawn-and-wait) preserves PID and foreground semantics for service managers.
- Supersedes the alternative "reject `--daemon` with an error": that breaks existing units. Also avoids the `exe_hint = None` trap, where `NativeSpawner` would fall back to `current_exe()` and spawn another thin client.
- Covered by a thin-client integration test using a fake daemon script (Story 2.1.2) and a real two-binary test in `crates/cli/tests` (Task 2.1.2b).
