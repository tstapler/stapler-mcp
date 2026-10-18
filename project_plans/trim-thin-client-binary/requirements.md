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

1. **AC-1 (Split binary):** A new separate binary target exists for the thin client that does
   **not** link `chromiumoxide`, `fastembed`, or `reqwest` (verified via `cargo tree -e normal
   --package <thin-client-crate>` showing none of those crate names).
2. **AC-2 (Daemon unchanged):** The daemon binary (`--daemon` path) continues to link all
   existing native adapters; `cargo test` in `crates/cli` passes green.
3. **AC-3 (Release profile):** The thin-client binary's release profile includes
   `strip = true`, `lto = "fat"`, `opt-level = "z"`, `codegen-units = 1` (or equivalent
   workspace-level profile override scoped to that target).
4. **AC-4 (Binary size):** `cargo build --release` produces a thin-client binary measurably
   smaller than the current combined binary (target: ≥ 20% reduction in stripped binary size).
5. **AC-5 (Feature flag in native):** `stapler-mcp-native` exposes a Cargo feature (e.g.
   `heavy`) that gates `NativeBrowser`, `NativeEmbedder`, `NativeHttp`, `NativeFs`,
   `NativeLock` (and their transitive heavy deps) — the thin-client crate opts out of it.
6. **AC-6 (CI):** `make ready` (ansible-lint + shellcheck) is unaffected; Rust workspace
   `cargo test --workspace` passes.
