# Requirements: Trim Thin-Client Binary Size

## Background

Every open Claude Code session spawns its own `stapler-mcp` thin-client process (stdio transport,
proxying to the shared `--daemon`). With ~30 sessions open this shows up as ~30 `stapler-mcp`
rows at ~4 MB RSS each. RSS is already low because unused pages aren't resident, but the binary
artifact is oversized and carries unnecessary attack surface: `crates/cli/src/main.rs`
unconditionally imports all of `stapler-mcp-native` (including `NativeBrowser` backed by
`chromiumoxide` and `NativeEmbedder` backed by `fastembed`) even though `thin_client.rs`
only needs the lightweight adapters: `NativeEnv`, `NativeSocketFactory`, `NativeSpawner`,
`NativeSleeper`, `NativeClock`.

## Goals

1. Shrink the thin-client binary artifact (on-disk size, link time) by eliminating linked-in
   symbols from `chromiumoxide` and `fastembed` that the thin-client path never calls.
2. Reduce attack surface: a memory-corruption exploit in an unused, linked-in browser or ML
   library cannot be reached if those symbols are absent from the thin-client binary.
3. Add standard release-profile size tuning to the thin-client target.
4. Optionally: evaluate whether dropping the async runtime (tokio) for a blocking I/O relay
   loop further reduces binary size and per-process overhead.

## Non-Goals

- Moving RSS from ~4 MB to near-zero (unused pages are already not resident; the remaining
  baseline is Rust stdlib + libc + rmcp + tokio).
- Changing the observable MCP tool surface exposed to Claude Code.
- Modifying the daemon binary.
- Removing the per-session process entirely (that is tracked separately as issue #38).

## Constraints

- The thin-client binary must remain a drop-in replacement for the existing `stapler-mcp`
  invocation (same argv interface: no `--daemon` flag → thin-client mode).
- Existing tests in `crates/cli` (`tests/tool_schema.rs` etc.) must continue to pass.
- The daemon binary must continue to function identically.

## Acceptance Criteria

Authoritative list is the backlog item (ec050b7d-1969-405b-ab88-9e2ed99e05fd):

1. `cargo tree -e normal --package stapler-mcp` (thin-client crate) shows no `chromiumoxide`, `fastembed`, or `reqwest`.
2. `stapler-mcp-daemon` builds and `cargo test --workspace` passes.
3. Thin-client release profile: `strip = true`, `lto = "fat"`, `opt-level = "z"`, `codegen-units = 1`, scoped via `[profile.release.package."stapler-mcp"]`.
4. Stripped release thin-client is >= 20% smaller than the pre-split combined binary.
5. `stapler-mcp-native` has a default-enabled `heavy` feature gating `NativeBrowser`, `NativeEmbedder`, `NativeHttp` and their transitive deps; `crates/thin-client` uses `default-features = false`.
6. Daemon auto-start works: thin client calls `ensure_daemon` with an `exe_hint` resolving to `stapler-mcp-daemon` in the same directory, verified by an integration test.
7. `npm/` packaging bundles both `stapler-mcp` and `stapler-mcp-daemon`.
