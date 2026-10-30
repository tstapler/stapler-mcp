# Stack and test patterns: read-website-daemon-recovery

Evidence: Cargo.toml files, crates/core/src/daemon.rs, crates/native/src/lock.rs, crates/cli/tests/daemon_ping.rs (read at HEAD 7d2ecc4).

## Crates and versions (VERIFIED from Cargo.toml)

| Crate | Spec | Where | Relevance |
|---|---|---|---|
| tokio | `1` (rt, macros, time, net, io-util, sync, signal; `test-util` in cli dev-deps) | core, native, cli, thin-client | `tokio::time::pause` and `#[tokio::test]` cover timeout and backoff tests |
| tokio-util | `0.7` | core, cli | `CancellationToken` used by `run_cancellable` |
| libc | `0.2` | native only (`libc::setsid` at crates/native/src/spawn.rs:58) | Already available for `libc::kill(pid, SIGKILL)` in native tests |
| tempfile | `3` | cli, native, thin-client dev-deps | Isolated `STAPLER_MCP_HOME` per test |
| nix / rustix / fs2 / fd-lock | absent | | Not needed (see below) |

Edition 2021 (workspace). `File::try_lock` / `TryLockError` in lock.rs are std (stable since Rust 1.89), so the toolchain is already recent.

## Fix surface (no new deps)

- AC2: in `Daemon::run` and `run_cancellable` (daemon.rs:160, :187) replace `self.serve_connection(conn).await?` with a `match` that logs and continues. Keep `accept().await?` fatal, or classify it. No crate needed.
- AC4 and startup timeout: `transport.rs` and `EnsureOptions.startup_timeout` (client.rs:17) in-repo only.
- AC3: `stapler_daemon_status` error mapping, in-repo only.

## Test patterns that apply

1. Mock `SocketFactory` (daemon.rs tests ~L350-390: `PendingSocketFactory`, a stub `Conn`/`Listener`). For AC2 add a `Listener` that yields a `Conn` whose `write_frame` returns `Err(PortError)` (simulated EPIPE), then a second good `Conn`; assert `run_cancellable` returns `Ok` only after cancel and the second connection was served. Fast, deterministic, no sockets.
2. Real-binary e2e (crates/cli/tests/daemon_ping.rs): builds the actual binary, isolated state dir via a `TestEnv: EnvPort`, uses `NativeSocketFactory`/`NativeSpawner`/`ensure_daemon`. File header notes it is deliberately one `#[tokio::test]` because `set_var` is process-global; new e2e cases (kill and restart, N parallel cold starts) should go in a new test file (separate test binary = separate process) or be sequenced inside that single test.
3. SIGKILL in tests (AC0): read the PID from the lock file (`NativeLockGuard::write_pid` writes it) or from ping/status, then `libc::kill(pid as i32, libc::SIGKILL)` in an `unsafe` block. `libc` is currently a dependency of `native` only; add `libc = "0.2"` to `crates/cli` `[dev-dependencies]` (same version, no new crate in the lockfile). `nix` would add a dependency for one call; not worth it.
4. AC1 parallel cold start: `tokio::join!`/`JoinSet` of N `ensure_daemon` calls against the real binary in a tempdir; assert all Ok. Needs `rt-multi-thread`-free pattern: `join_all` on a current_thread runtime is sufficient since the calls are I/O-bound (futures crate or `tokio::task::JoinSet` with `spawn_local`; check whether `futures` is already a dependency before using `join_all`, UNVERIFIED).
5. Timeout logic: `tokio::time::pause()` (cli already enables `test-util`) to test the raised startup timeout without waiting 30 s. core dev-deps lack `test-util`; add the feature there if the test lives in core.

## Is a cross-process startup-serialization crate needed? No

`NativeLock::acquire_exclusive` (lock.rs:25-50) already does `OpenOptions` + std `File::try_lock()` (an flock on the lock file). The daemon takes it at cli/src/main.rs:344 and maps `WouldBlock` to `LockError::AlreadyRunning` (main.rs:346). So concurrent spawns already serialize: exactly one daemon wins the lock, losers exit as AlreadyRunning, and every client's `ensure_daemon` just keeps polling ping with backoff until the winner binds. The kernel releases flock on SIGKILL, so a killed daemon never leaves a stuck lock (this is what makes AC0 work).

Residual gaps (design, not crate, issues):
- Lock is held before `remove_stale`/`bind`; a loser that exits quickly is harmless, but check ordering so a loser never calls `remove_stale` on the winner's live socket (lock acquired before `run_cancellable` at main.rs:344 vs `remove_stale` inside it; looks correct, UNVERIFIED end to end).
- Client-side single-flight (a lock around spawn in `ensure_daemon`) would reduce redundant spawn attempts but is an optimization, not required for AC1. If wanted, reuse `ProcessLock` via a second lock path rather than adding `fd-lock`/`fs2`.
