# Implementation Plan: Trim Thin-Client Binary Size

## Chosen Approach: Option A — Feature-flag native crate + new thin-client crate

Split the thin client into its own workspace crate (`crates/thin-client`) that depends
on `stapler-mcp-native` without its heavy feature. The existing `crates/cli` retains the
daemon code. Release-profile tuning is applied to the workspace `[profile.release]`.

---

## Phase 1 — Feature-flag `stapler-mcp-native`

**Task 1.1 — Add `heavy` feature to `crates/native/Cargo.toml`**

```toml
[features]
default = ["heavy"]
heavy = ["dep:chromiumoxide", "dep:fastembed", "dep:reqwest"]
```

Move `chromiumoxide`, `fastembed`, and `reqwest` to optional dependencies:
```toml
[dependencies]
chromiumoxide = { version = "0.9", optional = true }
fastembed = { version = "=5.17.2", optional = true }
reqwest = { version = "0.13", features = ["rustls"], default-features = false, optional = true }
```

**Task 1.2 — Gate heavy exports in `crates/native/src/lib.rs`**

Wrap the heavy module re-exports:
```rust
#[cfg(feature = "heavy")]
mod browser;
#[cfg(feature = "heavy")]
mod embed;
#[cfg(feature = "heavy")]
mod http;
// fs and lock are lightweight; keep unconditional
mod fs;
mod lock;

#[cfg(feature = "heavy")]
pub use browser::NativeBrowser;
#[cfg(feature = "heavy")]
pub use embed::NativeEmbedder;
#[cfg(feature = "heavy")]
pub use http::NativeHttp;
pub use fs::NativeFs;
pub use lock::{NativeLock, NativeLockGuard};
```

**Task 1.3 — Update `crates/cli/Cargo.toml` to declare `heavy` explicitly**

```toml
stapler-mcp-native = { path = "../native", features = ["heavy"] }
```

Verify `cargo build -p stapler-mcp` still compiles (daemon path must retain heavy
symbols).

---

## Phase 2 — Create `crates/thin-client` workspace crate

**Task 2.1 — Scaffold `crates/thin-client/`**

```
crates/thin-client/
  Cargo.toml
  src/
    main.rs        (thin client entry point, formerly part of cli/main.rs)
    thin_client.rs (moved verbatim from cli/src/thin_client.rs)
```

`Cargo.toml`:
```toml
[package]
name = "stapler-mcp"          # keeps the same binary name for drop-in replacement
version.workspace = true
edition.workspace = true

[[bin]]
name = "stapler-mcp"
path = "src/main.rs"

[dependencies]
stapler-mcp-core = { path = "../core" }
stapler-mcp-native = { path = "../native", default-features = false }
tokio = { version = "1", features = ["rt", "macros", "time", "net", "io-util", "sync"] }
rmcp = { version = "2", features = ["server", "macros", "transport-io", "schemars"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
futures = "0.3"
```

Note: `default-features = false` opts out of the `heavy` feature (and its transitive
deps `chromiumoxide`, `fastembed`, `reqwest`).

**Task 2.2 — Write `crates/thin-client/src/main.rs`**

```rust
mod thin_client;

use rmcp::{transport::stdio, ServiceExt};

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, run_thin_client());
}

async fn run_thin_client() {
    let client = thin_client::ThinClient::new();
    let service = match client.serve(stdio()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("stapler-mcp: failed to start stdio transport: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = service.waiting().await {
        eprintln!("stapler-mcp: stdio transport error: {e}");
        std::process::exit(1);
    }
}
```

**Task 2.3 — Copy `thin_client.rs` to `crates/thin-client/src/thin_client.rs`**

Move the file verbatim. No changes to the impl needed — all its imports are from
`stapler-mcp-core` and the light `stapler-mcp-native` exports which remain available
under `default-features = false`.

**Task 2.4 — Add `crates/thin-client` to `Cargo.toml` workspace**

```toml
[workspace]
members = ["crates/core", "crates/native", "crates/cli", "crates/thin-client", "crates/wasm"]
default-members = ["crates/core", "crates/native", "crates/cli", "crates/thin-client"]
```

---

## Phase 3 — Update `crates/cli` to be daemon-only

**Task 3.1 — Strip thin-client imports and branch from `crates/cli/src/main.rs`**

Remove the `mod thin_client;`, `run_thin_client()` function, and the `--daemon` branch
in `main()`. The new `main.rs` unconditionally runs the daemon. The binary name stays
`stapler-mcp-daemon` or can be renamed; since Claude Code launches the thin-client
binary by path, the daemon is only launched indirectly via `client::ensure_daemon`
which calls the thin-client binary with `--daemon`. This needs careful handling:

- `client::ensure_daemon` in `stapler-mcp-core/src/client.rs` spawns the current binary
  with `--daemon`. With the split, the thin-client binary still accepts `--daemon` but
  the daemon code is gone from it.
- **Resolution:** Keep `crates/cli`'s binary named `stapler-mcp` and have it handle
  `--daemon`. The `crates/thin-client` binary becomes a separate build artifact. The
  install/packaging step ships the thin-client binary as the primary `stapler-mcp` on
  `PATH`, but the daemon is spawned by the thin-client via a path to the same binary.
  
  Since `ensure_daemon` re-execs `std::env::current_exe()` (the thin-client binary)
  with `--daemon`, the thin-client binary MUST handle `--daemon` too — meaning we
  cannot fully separate daemon logic from the thin-client binary unless `ensure_daemon`
  is changed to look up a distinct `stapler-mcp-daemon` binary.

**Task 3.2 — Wire `exe_hint` to find `stapler-mcp-daemon`**

`NativeSpawner::spawn_daemon` (verified in `crates/native/src/spawn.rs`) uses
`std::env::current_exe()` when `exe_hint` is `None`, then appends `--daemon`. Since the
thin-client binary will no longer contain daemon code, it must supply an `exe_hint` so
the spawner finds the separate daemon binary.

In `crates/thin-client/src/thin_client.rs`, replace:

```rust
EnsureOptions::default()
```

with:

```rust
EnsureOptions {
    exe_hint: Some(daemon_exe_path()),
    ..Default::default()
}
```

where `daemon_exe_path()` is:

```rust
fn daemon_exe_path() -> String {
    // Look for stapler-mcp-daemon in the same directory as the running binary.
    // Falls back to PATH lookup if not found (e.g. in dev with `cargo run`).
    if let Ok(mut p) = std::env::current_exe() {
        p.set_file_name("stapler-mcp-daemon");
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    "stapler-mcp-daemon".to_string()
}
```

The packaging/install step (`npm/` publish scripts or the Homebrew formula) must ship
both binaries: `stapler-mcp` (thin client, on PATH) and `stapler-mcp-daemon` (in the
same directory, not necessarily on PATH).

---

## Phase 4 — Release profile tuning

**Task 4.1 — Add size-optimized profile to workspace `Cargo.toml`**

```toml
[profile.release]
strip = true
lto = "fat"
opt-level = "z"
codegen-units = 1
```

This applies to all workspace members, including the daemon. This is acceptable — the
daemon also benefits from a smaller binary. If daemon build time becomes a concern in CI
a named profile (`[profile.release-thin]`) scoped to the thin-client crate can be used
instead.

---

## Phase 5 — Verification

**Task 5.1 — Confirm `cargo tree` shows no heavy deps in thin-client**

```
cargo tree -e normal --package stapler-mcp \
  | grep -E 'chromiumoxide|fastembed|reqwest'
```

Expected: no output.

**Task 5.2 — Measure binary size before and after**

```
cargo build --release
ls -lh target/release/stapler-mcp   # thin-client
ls -lh target/release/stapler-mcp-daemon  # daemon (if renamed)
```

Record both sizes and confirm ≥ 20% reduction in thin-client binary vs. the pre-split
baseline.

**Task 5.3 — Run full test suite**

```
cargo test --workspace
```

The `tool_schema.rs` integration test uses `ThinClient` from `crates/thin-client` — 
move the test there or update the `#[path]` reference.

---

## Risk Notes

- **`ensure_daemon` re-exec path:** The most critical coupling to resolve. If
  `ensure_daemon` does `std::env::current_exe()` + `--daemon`, and the thin-client
  binary no longer contains daemon code, the daemon will fail to start. This must be
  resolved in Task 3.2 before shipping.
- **Binary naming conflict:** Both `crates/cli` and `crates/thin-client` produce a
  binary named `stapler-mcp` today. One must be renamed. The daemon should become
  `stapler-mcp-daemon`; the thin-client keeps the user-facing `stapler-mcp` name.
- **MCP config compatibility:** The MCP client config (`.config/mcp/mcp-servers.json`)
  must still point to a binary named `stapler-mcp` on PATH; the thin-client binary
  satisfies this without config changes.

## Out of Scope (Future)

- Blocking I/O relay replacing tokio in thin client (Option C from research).
- Issue #38 — eliminating the per-session process entirely.
