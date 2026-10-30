# Pitfalls: daemon survival + parallel cold-start ensure_daemon

All line refs at HEAD 7d2ecc4, verified by reading code (not run).

## (a) Busy daemon -> ping timeouts -> caller failure: YES (and it is the likely crash trigger)

- Accept loop is strictly serial: `serve_connection` is awaited inline (`crates/core/src/daemon.rs:183-187`), and it awaits the handler (`:199`, handler bounded by `REQUEST_TIMEOUT` = 30s, `:31,:126`).
- `ensure_daemon` first does `ping(.., DIAL_TIMEOUT=2s)` (`crates/core/src/client.rs:11,78`). `UnixStream::connect` succeeds from the kernel backlog even while the daemon is busy, write succeeds, but `read_frame` is bounded by the conn timeout (`crates/native/src/socket.rs:101,38`; `client.rs:31`) -> `PortError::Timeout` after 2s. Ping is treated as "daemon down".
- Then the caller calls `spawn_daemon` (`client.rs:82`). The spawned process loses the flock and exits cleanly (`main.rs:344-349`), but the caller keeps polling with a 500ms per-ping cap until the 10s deadline (`client.rs:13,86-105`). A long read_website (up to 30s) therefore makes the status/other call fail with a Timeout error even though the daemon is healthy. Also each such call spawns a useless process.
- Compounding crash path: the abandoned ping connections remain in the backlog with the request bytes already buffered. When the daemon frees up it reads the ping, then `write_frame` to a peer that already closed -> EPIPE/ECONNRESET -> `serve_connection(..)?` (`daemon.rs:160/187`) -> `run_cancellable` returns Err -> `main.rs:964-966` `exit(1)`. So busy-daemon + client timeout is a direct kill-the-daemon path, the same mechanism as requirement #2. (Rust ignores SIGPIPE, so it surfaces as an Err, not a signal.)
- Fix implications: (1) per-connection errors must be logged and swallowed in both `run` and `run_cancellable`; (2) a ping Timeout (connect OK, no reply) should not be treated as "not running" / should not spawn; only ECONNREFUSED/ENOENT should trigger spawn, with Timeout retried against the same deadline; (3) the poll deadline must be measured with a ping timeout longer than the max tool time or tolerate Timeout as "alive but busy"; (4) a silent client that connects and sends nothing blocks the whole loop for `CONN_TIMEOUT` 120s (`daemon.rs:23,197`) - consider a short read timeout for the first frame (concurrency itself is out of scope).

## (b) stapler_daemon_status path

- `daemon_status` (`crates/thin-client/src/mcp_router.rs:866-877`) calls `self.transport.call(PING_TOOL, ..)`. With `SocketTransport` (`crates/thin-client/src/transport.rs:44-57`) that runs `ensure_daemon`, so it AUTO-SPAWNS (unlike `--status`, which deliberately calls `client::call` directly, `crates/cli/src/main.rs:101-128`).
- When the daemon cannot be reached/started it returns `Err(String)` = `"ensure daemon: {e} ({hint})"` -> an MCP tool error. `DaemonStatusOutput` has a non-optional `pong: bool` shape (see `tool_schema.rs:228-236`), so there is no existing way to express "down". Criterion 3 needs either a new output variant/field (e.g. `reachable: bool`, `error: Option<String>`, with socket/log paths) or a non-spawning ping path in the router. Changing the output struct changes the tool outputSchema, which `crates/thin-client/tests/tool_schema.rs` compares against the Input/Output schemars (schema test will need updating).
- Decision needed: should status spawn? Currently yes; a "diagnostic when unreachable" is only reachable if the spawn+poll fails. Cleanest: use ping-only (no spawn) with a short timeout and return a struct with paths, mirroring `run_status`.

## (c) Startup order matters for the 30s criterion

- Order in `run_daemon`: create dir -> http-port file (`main.rs:312-341`) -> acquire flock (`:344`) -> `NativeBrowser::launch` (`:378`) -> register handlers -> `daemon.run_cancellable` which does `remove_stale` + `bind` (`:947`, `daemon.rs:180-181`). So the socket is bound only AFTER Chrome launch. During Chrome start every ping gets ENOENT/ECONNREFUSED, and the 10s default (`client.rs:10`; transport passes `..Default::default()`, `transport.rs:51-54`) is the binding constraint. A cold Chrome start > 10s fails with the last error (e.g. "Connection refused").
- Options: raise startup timeout to >=30s (cheap, matches criterion 0), and/or bind+accept before Chrome launch (larger refactor; handlers would need lazy browser). Raising the timeout is the minimal change; note launch failure is `exit(1)` (`main.rs:381`) so the client polls the full timeout before reporting - consider surfacing the log path (criterion 4) for this case.
- Parallel cold start (criterion 1): N callers each ping-fail then each `spawn_daemon`. One wins the flock; N-1 exit "already running" (`main.rs:346-349`) - harmless but wasteful. RISK: the http-port file write/remove happens BEFORE the lock (`main.rs:326-341`), so a loser with a different `STAPLER_MCP_HTTP_PORT` env (or none) can delete/overwrite the winner's port file. Same env usually, so low risk; flag it, consider moving below the lock.
- Pings in the cold window each take a fast failure, so the poll loop (50ms->500ms) is fine; all N converge once the socket binds. After bind, the serial loop serves N queued pings quickly, but the first real read_website call blocks later ones (up to 30s each): N parallel read_websites each need `CALL_TIMEOUT` 120s (`transport.rs:16`) >= N*30s worst case; with N>4 a tail caller can time out on read even though it is queued correctly (and then its abandoned conn triggers the EPIPE crash path above unless fixed).

## (d) Stale socket after SIGKILL

- flock is released by the kernel on process death, so the next daemon acquires it (`crates/native/src/lock.rs:42-45`, std `File::try_lock`; stderr "already running" only when truly held). The socket file persists, so clients see ECONNREFUSED (matches the issue); the new daemon runs `remove_stale` (`socket.rs:104-107`, unconditional unlink, errors ignored) then `bind` - correct, and safe because it runs only after winning the lock (`daemon.rs:147-149`). `lock.rs` does not truncate; `write_pid` rewrites (`main.rs:356`).
- Caveats: (1) a SIGKILLed daemon's orphaned Chrome may hold the persistent profile dir's SingletonLock (only when `STAPLER_MCP_BROWSER_PROFILE_DIR` set) -> `launch` fails -> `exit(1)` -> clients time out; ephemeral temp-dir mode is unaffected. (2) `ensure_daemon` ping on a stale socket returns ECONNREFUSED quickly, so recovery cost is dominated by Chrome cold start (see c). (3) No code path removes a stale socket on the client side; relying on daemon startup is fine.
- Not covered by tests: grep shows no test killing the daemon with SIGKILL.

## (e) Thin-client test seams for the error message

- The message is built inline in `SocketTransport::call` (`transport.rs:57`) using hard-wired `Native*` adapters; there is no seam, and `SocketTransport` has no generic parameters. Existing `FakePingTransport` (`crates/thin-client/tests/tool_schema.rs:193-206`) fakes the whole `DaemonTransport`, so it bypasses the message.
- Best seam: extract a pure function (e.g. `fn ensure_error_message(err, sock_path, log_path, hint) -> String`) in transport.rs and unit-test it; or make an `ensure_and_call<S,Sp,Sl,C>` generic over the core ports (they are traits in `crates/core/src/ports.rs`) and test with fakes. `client.rs` currently has no unit tests; `daemon.rs` tests already define fake ports/Conn (`daemon.rs:~340-440`, includes `remove_stale` fake at `:386`) that can be reused as a pattern for failing-`write_frame` tests (criterion 2) via `serve_connection`/`run`.
- Integration: `crates/cli/tests/daemon_ping.rs` is a single `#[tokio::test]` (spawns real daemon via `ensure_daemon`, mutates env with `set_var`), so new SIGKILL/parallel-cold-start tests need their own test files/processes to avoid env races.

## Summary of risks to design around
1. Swallowing per-connection errors alone is insufficient; ping Timeout on a busy daemon still spawns processes and fails callers.
2. Startup timeout default (10s) < cold Chrome start; socket bound only after Chrome launch.
3. `stapler_daemon_status` spawns and returns `Err(String)`; criterion 3 requires an output-shape or path change plus schema test update.
4. Error-message test needs a small refactor (no current seam).
