# Build vs buy: daemon recovery

Problem (requirements.md): per-connection errors kill the daemon (daemon.rs:160, :187 use `?`; cli main.rs:964-966 exits), leaving a stale socket; startup timeout is 10 s; error omits paths.

## Option A: minimal in-repo fix (swallow per-connection errors, raise startup timeout, richer error)

Pros
- Targets the verified root cause directly; about 20-40 lines across daemon.rs, transport.rs, and the status tool.
- Zero new dependencies; keeps the thin client small (workspace has size-optimized `release-thin` profile, opt-level z).
- Existing flock lock already handles cold-start races (see stack.md), so AC1 is covered.
- Unit-testable with the existing mock `SocketFactory`.

Cons
- Does not address daemon crashes from other causes (panic, OOM, Chrome); restart relies on the next client's `ensure_daemon`, which AC0 already requires and the lock makes safe.
- A fixed longer timeout is a guess; mitigate by making it overridable via `EnsureOptions`.
- Watchdog and concurrent handling stay out of scope (per requirements).

Verdict: ADOPT.

## Option B: adopt an OSS library

Candidates (from general knowledge; not re-verified online): a connection/supervision layer such as `tower`/`hyper` over UDS, `tarpc`, or `interprocess` for sockets; `daemonize`/`fork` crates for process management.

Pros
- Battle-tested accept loops and error isolation.

Cons
- Replaces the custom frame protocol and the wasm-portable `SocketFactory`/`Conn` ports (core is built for wasm32 too), a large rewrite for a bug fixed by a `match`.
- Grows the thin client binary and compile surface.
- Library adoption does not remove the need to decide "log and continue" on per-connection errors.

Verdict: REJECT.

## Option C: systemd (or launchd) socket activation

Pros
- The init system owns the listening socket, so a connect never gets ECONNREFUSED while the unit exists; restart is automatic.

Cons
- The repo targets Manjaro, Ubuntu, macOS (launchd differs), and WSL2 where systemd may be absent; two integration paths.
- Needs unit files in the bootstrap (ansible/pyinfra) and fd-passing support in the daemon (`listenfd`-style), plus a `SocketFactory` variant.
- Cold Chrome start still takes time; clients just block instead of failing.
- Per-user service lifecycle adds ops burden for a single-user tool; watchdog is explicitly out of scope.

Verdict: REJECT now; possible future hardening.

## Option D: fork an existing daemon framework

Pros
- None specific; would inherit supervision features.

Cons
- Maintenance burden of a fork, divergence from upstream, and it still would not fix this repo's `?` propagation or error messages.

Verdict: REJECT.

## Recommendation

Option A. Pair it with tests: mock-socket unit test for AC2, SIGKILL-and-restart e2e for AC0, parallel cold-start e2e for AC1.
