# Research: Architecture (integration points and data flow)

Evidence is from the working tree on branch `backlog/stapler-mcp-trim-thin-client-binary`. Items marked INFERRED were not run.

## 1. Daemon auto-start data flow

1. `crates/thin-client/src/thin_client.rs` `call_daemon` builds the `NativeSocketFactory`, `NativeSpawner`, `NativeSleeper` and `NativeClock`, then calls `client::ensure_daemon` with `EnsureOptions { exe_hint: Some(daemon_exe_path()), .. }` (lines ~53-79).
2. `crates/core/src/client.rs:63` `ensure_daemon` pings the socket first (2s dial timeout). If the ping succeeds, nothing is spawned. Otherwise it calls `spawner.spawn_daemon(exe_hint, log_path)`, then polls with ping (backoff 50ms doubling to a 500ms cap, 10s default deadline).
3. `crates/native/src/spawn.rs:9` `NativeSpawner::spawn_daemon` uses `exe_hint`, or `current_exe()` if the hint is `None`. It runs `Command::new(exe).arg("--daemon")` with stdin null and stdout/stderr appended to the log, calls `setsid()` in `pre_exec`, and never waits on the child.
4. `crates/cli/src/main.rs:26` (bin `stapler-mcp-daemon`) accepts and ignores `--daemon`, so the spawner needs no change. The daemon takes the `daemon.lock` exclusive lock, so concurrent thin clients that all spawn are safe (the losers print "daemon already running" to the log and exit).

Core and spawner are unchanged by the split. The only new logic is the hint resolution in `thin_client.rs:37-50` (`sibling_daemon_path`: `current_exe().with_file_name("stapler-mcp-daemon")`, used if it exists, else the bare name `stapler-mcp-daemon`).

### exe_hint behaviour matrix

| Case | Behaviour today | Risk / recommendation |
|---|---|---|
| Sibling present | Absolute path is used. | Good. This is the case acceptance criterion 6 tests. |
| Sibling missing, daemon on `PATH` | Bare name goes to `Command::new`, which does a PATH lookup. | Works. Version skew becomes possible (see below). |
| Sibling missing, not on `PATH` | `spawn()` fails with `PortError::Io("No such file or directory")`. The user sees `ensure daemon: ...`. | Low. Add a message naming `stapler-mcp-daemon` and where it looked. Also `sibling.exists()` is not an executable check (a directory or non-exec file passes). |
| Symlinked install | On Linux `current_exe()` resolves symlinks (`/proc/self/exe`), so the sibling is looked up next to the real binary. If only `stapler-mcp` is symlinked into `~/bin`, the real dir holds both and it works. If both are symlinked separately into different dirs, the sibling lookup misses and falls to PATH. INFERRED for macOS: `current_exe` uses `_NSGetExecutablePath`, which may not resolve symlinks. | Medium. Tests should cover a symlink-to-binary layout. Optionally `canonicalize()` before `with_file_name`. |
| Version skew | `ping` is only a liveness check (`PING_TOOL = "ping"`, `crates/core/src/protocol.rs:9`). There is no version handshake. An old running daemon from a previous install keeps serving a new thin client. A thin client that sees a new tool but an old daemon gets a daemon-side "unknown tool" error. | Medium. This predates the split, but the split makes a mixed-version install more likely (two artifacts). Options: have `ping` return the version and warn on mismatch (core change, out of scope per "do not modify daemon", so list it as a follow-up), or document that upgrades should kill the daemon. |
| Legacy invocation `stapler-mcp --daemon` | `crates/thin-client/src/main.rs` does not parse args, so it starts a thin client on stdio and does not start a daemon. Existing launchd/systemd units or scripts would break silently. | High for existing users. Consider having the thin client exec `stapler-mcp-daemon` when it sees `--daemon`. This is cheap, keeps the drop-in constraint ("same argv interface"), and costs no extra linked code. |
| `exe_hint = None` | Falls back to `current_exe()`, which is now the thin client. It would spawn another thin client with `--daemon`, which is wrong. The `None` path is only used by wasm or tests. | Low now. Worth a comment or test so nobody reuses `None` natively. |

## 2. npm packaging vs criterion 7

- `npm/package.json` (`"files": ["bin", "pkg"]`, `bin.stapler-mcp = bin/stapler-mcp.js`) ships JS plus `wasm-bindgen` output (`npm/pkg/stapler_mcp_wasm.js`). Its own description says "no native binary required". `npm/bin/stapler-mcp.js` calls `wasm.run_daemon()` for `--daemon` and `wasm.ensure_daemon_and_call` for tools. It spawns itself, never a Rust-native `stapler-mcp-daemon`.
- README.md:68-73 documents the npm path as "zero native binary" on purpose (macOS Gatekeeper). Bundling native executables contradicts that design.
- The CI `wasm` job (`ci.yml:44-56`) builds only `stapler-mcp-core` and `stapler-mcp-wasm` for wasm32, so it is unaffected by the split.
- Conclusion: criterion 7 ("npm/ packaging bundles both `stapler-mcp` and `stapler-mcp-daemon`") does not match the npm package as designed. Three options for the owner:
  1. Interpret it as "the package still works and nothing regresses". This is the recommended default, since npm uses the WASM client and WASM daemon and neither is affected.
  2. Add an optional native-binary distribution (for example per-platform `optionalDependencies` or release tarballs holding both binaries side by side). This is new scope and conflicts with the Gatekeeper rationale.
  3. Reword the criterion to a native release artifact (a tarball or `cargo install` that installs both bins). `cargo install --path crates/thin-client` plus `crates/cli` puts both in `~/.cargo/bin`, which satisfies the sibling lookup.
- Needs an owner decision. Do not silently satisfy it by editing `npm/files`.

## 3. Other references to the old single binary

- `.github/workflows/ci.yml`: `cargo build --workspace`, `cargo test --workspace` and clippy cover both bins automatically (lines 17-23), so nothing breaks. There is no size gate and no `cargo tree` gate. Criterion 1 and criterion 4 need new CI steps if they are to be enforced: `cargo tree -e normal -p stapler-mcp | grep -E 'chromiumoxide|fastembed|reqwest'` must be empty, and a `cargo build --profile release-thin -p stapler-mcp` size check. Note `release-thin` is a named profile, because `lto` cannot be set per package (see the comment in `Cargo.toml:13-15`). Plain `--release` gives opt-level z and codegen-units 1 for the package but no fat LTO. Criterion 3 as worded ("scoped via `[profile.release.package]`") is only partly achievable, and the working tree already handles this correctly.
- `README.md` stale spots:
  - Lines 24-26 diagram: the daemon box says `stapler-mcp --daemon`; it should say `stapler-mcp-daemon`.
  - Lines 33-37 and 41: the description of `--daemon`.
  - Lines 133-139: `cargo build -p stapler-mcp` now builds only the thin client, and `./target/debug/stapler-mcp --daemon` no longer starts a daemon. It should be `cargo build -p stapler-mcp -p stapler-mcp-daemon`.
  - Line 152: the note about "what execs `--daemon`" (the sibling lookup and PATH fallback should be documented).
  - Line 169 notes that `cargo build` covers the native workspace; the new crate is part of it.
- `crates/cli/src/main.rs:51` still prints `stapler-mcp: daemon already running`. This is cosmetic.

## 4. Hotspot / disposition

| File | State | Disposition | Reason |
|---|---|---|---|
| `crates/native/src/lib.rs` (30 lines) | Pure `mod`/`pub use` list, with `#[cfg(feature = "heavy")]` on `ax`, `browser`, `embed`, `http`. | **Extend as-is** | Already the seam. Not a hotspot; no logic. Remaining work is in `crates/native/Cargo.toml` (make the `chromiumoxide`, `fastembed`, `reqwest` and `scraper`-style deps `optional = true` under `heavy`, and `default = ["heavy"]`) and verifying with `cargo tree`. |
| `crates/cli/src/main.rs` (466 lines) | Daemon only, all handler registration in `run_daemon`. | **Extend as-is** (do not refactor in this change) | The file is the daemon, and the requirements forbid changing daemon behaviour. It is long, but this work only renames the bin and sets `features = ["heavy"]`. Splitting handler registration into modules is a separate refactor and not needed for the criteria. |
| `crates/thin-client/src/thin_client.rs` | Moved from cli, adds `sibling_daemon_path`. | Extend as-is, plus tests for `sibling_daemon_path` (present, absent, symlink). | Pure function, easy to test without spawning. |

Seam summary: the `heavy` feature is the isolation seam. `crates/cli` depends with `features = ["heavy"]`, `crates/thin-client` with `default-features = false`. The thin client's `tool_schema.rs` test moved with it, so schema checks stay with the proxy.

## 5. Risks to carry into planning

1. Legacy `--daemon` on the thin client name (medium-high). Decide: exec the sibling, or document as a breaking change.
2. Criterion 7 mismatch with the WASM-only npm package (needs an owner decision).
3. No version handshake between thin client and daemon (pre-existing, more likely after the split).
4. The integration test for criterion 6 needs both bins built. Use `CARGO_BIN_EXE_*`, which Cargo only sets for bins of the same package. A thin-client test cannot see `stapler-mcp-daemon`. Options: build the daemon in a `build.rs` or test helper, locate it via `target/<profile>/`, or put the test in a crate that depends on both. Check that `cargo test --workspace` builds both bins before the test runs (workspace tests do build all bins, but a per-package `-p stapler-mcp` run will not).
