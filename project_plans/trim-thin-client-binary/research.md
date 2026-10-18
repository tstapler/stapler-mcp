# Research: Trim Thin-Client Binary Size

## Codebase Findings

### Current dependency graph (thin-client path)

`crates/cli/src/main.rs` branches on `--daemon`. In the thin-client branch it calls
`thin_client::ThinClient::new()` and runs the `rmcp` stdio transport. The imports in
`main.rs` pull in the full native crate unconditionally:

```rust
use stapler_mcp_native::{
    NativeBrowser, NativeClock, NativeEmbedder, NativeEnv, NativeFs, NativeHttp, NativeLock,
    NativeSocketFactory,
};
```

But `thin_client.rs` uses only:

```rust
use stapler_mcp_native::{
    NativeClock, NativeEnv, NativeSleeper, NativeSocketFactory, NativeSpawner,
};
```

### Heavy transitive dependencies (from `crates/native/Cargo.toml`)

| Symbol        | Crate           | Why heavy                                      |
|---------------|-----------------|------------------------------------------------|
| NativeBrowser | chromiumoxide   | Chrome DevTools Protocol client, async runtime |
| NativeEmbedder| fastembed       | ONNX ML model loading, ~50 MB of data          |
| NativeHttp    | reqwest         | TLS stack (rustls), HTTP/2, large dep tree     |
| NativeFs      | (tokio fs)      | Already in tokio, not independently heavy      |
| NativeLock    | (libc flock)    | Lightweight                                    |

### Light symbols (already in thin-client path)

`NativeEnv`, `NativeSocketFactory`, `NativeConn`, `NativeListener`, `NativeSleeper`,
`NativeClock`, `NativeSpawner` — these depend only on `tokio` (net, time, process),
`libc`, and `stapler-mcp-core`.

### Files affected

- `crates/cli/src/main.rs` — all daemon imports
- `crates/cli/src/thin_client.rs` — only light adapters
- `crates/cli/Cargo.toml` — single binary target today
- `crates/native/src/lib.rs` — single flat re-export
- `crates/native/Cargo.toml` — no features today, all deps unconditional
- `Cargo.toml` (workspace) — members list, profile section

### Existing test surface

`crates/cli/tests/tool_schema.rs` uses `ThinClient::registered_tools()`, a
`#[cfg(test)]` method on `ThinClient`. It does **not** test any daemon code. This test
will continue to work as long as `ThinClient` and `thin_client.rs` remain accessible.

## Design Options

### Option A — Feature-flag `stapler-mcp-native` + new workspace crate

Add a `heavy` feature to `crates/native` that gates `NativeBrowser`, `NativeEmbedder`,
`NativeHttp`, and their transitive deps. Create a new `crates/thin-client` workspace
crate (binary target `stapler-mcp`) that depends on `stapler-mcp-native` without the
`heavy` feature and on `stapler-mcp-core`. Move `thin_client.rs` there. The existing
`crates/cli` becomes the daemon-only binary (renamed `stapler-mcp-daemon` or kept as
`stapler-mcp --daemon`).

**Pros:** Clean separation, matches the item's proposal exactly, unambiguous linker
behavior (no dead code left in thin-client binary), testable via `cargo tree`.

**Cons:** Requires renaming or restructuring how Claude Code launches the binary. The
MCP config in `.config/mcp/mcp-servers.json` presumably hardcodes the binary path; the
thin-client crate would need to produce a binary named `stapler-mcp` so no client config
changes are needed.

### Option B — Second `[[bin]]` in `crates/cli` with feature flags

Add `[[bin]] name = "stapler-mcp-thin" path = "src/thin_main.rs"` to `crates/cli`. The
new entrypoint imports only light symbols. Release profile tuning targets this bin only.

**Pros:** Stays in one crate, simpler workspace.

**Cons:** Both binaries still link the same crate with the same feature set — you'd need
`[features]` in `crates/cli` too, making it messy. `cargo tree --package stapler-mcp`
would still show heavy deps because the crate's dependency declaration is shared.

**Verdict:** Option A is cleaner and what the backlog item describes.

### Option C — Blocking relay (drop tokio in thin client)

Replace `run_thin_client`'s async machinery with a synchronous `std::os::unix::net`
relay loop. The thin client's job is: receive JSON-RPC on stdin → forward to daemon Unix
socket → return response on stdout, with auto-start logic.

**Analysis:** `ensure_daemon` in `stapler_mcp_core::client` is async and uses
`tokio::net::UnixStream`. Rewriting it to blocking would require duplicating or
refactoring that module. The `rmcp` transport is also async. The potential savings
(eliminating the tokio reactor thread and its stack) are small (~200 KB RSS per
process), and the complexity cost is high. This is a P4 future improvement if the
split binary approach still leaves RSS uncomfortably high.

**Verdict:** Out of scope for this iteration; worth noting as a follow-up.

## Rust Profile Tuning Reference

For a size-optimized release profile:

```toml
[profile.release]
strip = true
lto = "fat"
opt-level = "z"
codegen-units = 1
```

`opt-level = "z"` is Rust's "optimize for size, aggressively" level (one step beyond
`"s"`). `lto = "fat"` enables whole-program link-time optimization across crate
boundaries. `codegen-units = 1` prevents splitting into parallel codegen units that
can't optimize across each other. `strip = true` removes debug symbols from the
artifact (symbols are stripped at link time, not at install time).

These can be scoped to a specific `[[bin]]` via a named profile or using
`[profile.release.package."<crate-name>"]` but workspace-level `[profile.release]`
is simplest here since the daemon binary also benefits.

## Issue #38 Reference

Issue #38 (removing the per-session process entirely) would supersede this work by
eliminating the thin-client process. This item is explicitly scoped to the in-flight
state where a per-session process still exists.
