# Adversarial Review: read-website-daemon-recovery

**Date**: 2026-10-29
**Verdict**: CONCERNS

Reviewed `implementation/plan.md`, `decisions/ADR-001`, `decisions/ADR-002` against `requirements.md` and the code at HEAD (`crates/core/src/{daemon,client,ports}.rs`, `crates/native/src/{socket,spawn,lock}.rs`, `crates/thin-client/src/{transport,mcp_router}.rs`, `crates/cli/src/main.rs`, `crates/wasm/src/{lib,socket}.rs`, `crates/thin-client/tests/tool_schema.rs`). All of the plan's cited facts F1-F12 that I opened hold (line numbers match). No blocker found; the three focus questions are answered below.

## Focus questions

### 1. Can "spawn unless Timeout" leave a dead-daemon / stale-socket case that surfaces as Timeout (never restarts)?

Verdict: no for the failure modes in the requirements; yes only for a wedged-but-alive daemon, which the plan already accepts (ADR-001 Consequences, out of scope: watchdog).

- Stale socket after SIGKILL: `connect` to a socket file with no listener returns ECONNREFUSED immediately, which `NativeSocketFactory::connect` maps to `PortError::Io` (`crates/native/src/socket.rs:96-100`), not `Timeout`. So AC0 restarts correctly. VERIFIED by reading the code; not run.
- Dead process still holding the listener (the only way a dead daemon could look "alive"): requires a child to inherit the listening fd. `NativeBrowser::launch` (Chrome) runs before `bind` (`main.rs:378` vs `daemon.rs:181`), so Chrome cannot inherit it; later children (`op`, etc.) would only inherit it if it were not CLOEXEC, and Rust/tokio create it CLOEXEC. INFERRED, not tested. Cheap to prove: 4.1.1a already kills the daemon; it could also assert `ping` returns `Io`, not `Timeout`, right after the kill (see concern C3).
- `Timeout` also comes from `call`'s `write_frame`/`read_frame` and from the connect timeout (`socket.rs:38-40,55-57,97-99`). A daemon in its 5s shutdown grace (`SHUTDOWN_GRACE_TIMEOUT`, `main.rs:304`) still has the socket bound but is not accepting, so clients see `Timeout` and wait; when the process exits they see refused and then spawn. Correct, because `spawned` is still false at that point.
- Residual gap (C1 below): once `spawned = true`, a spawn that loses the flock to a daemon that is on its way out is never retried.

### 2. Can N-parallel cold start still yield "Connection refused"?

Yes, in two ways, neither covered by the plan's AC1 test.

- Budget exhaustion: the socket binds only after the Chrome launch (F8). If Chrome plus the other startup steps take longer than the 45s `SocketTransport` budget (30s in core/wasm), the last error is the stored `Io("Connection refused ...")` and the user sees exactly the AC1 string. The 8-future e2e test only proves the happy path on a fast machine.
- Spawn-once starvation: with `spawned` set, if the one daemon this call started exits (lost the flock to a dying daemon, or `exit(1)` on browser launch failure, `main.rs:381`) nothing restarts it for the rest of the budget. Other parallel callers each have their own `spawned` flag, so in a parallel start the odds are good, but a lone caller is not protected.
- Not a problem: 8 concurrent callers all see ENOENT/refused and each spawn once, so 8 processes start and 7 exit on `AlreadyRunning` (`main.rs:344-349`), and `lock.rs` opens without truncate so the winner's PID survives. The plan's wording "start exactly one daemon" (Story 2.1.1 goal) is imprecise: it is one winning daemon, N spawn attempts.

### 3. Does the status probe change break existing tests?

Not at runtime, but one compile risk (C2).

- `FakePingTransport` (`tool_schema.rs:199`) implements only `call`; the defaulted `status_probe` delegates to `call`, so `daemon_status_should_deserialize_run_daemons_camelcase_wire_keys` still sees `pong: true` and the same two strings. `call_daemon_status_for_test` calls `daemon_status`, which will call `status_probe`; fine.
- The three struct literals for `DaemonStatusOutput` (`main.rs:162,178,191`) need `..Default::default()`, which the plan lists. `#[derive(Default)]` on the struct is fine (`pong: bool` defaults to false).
- `daemon_ping.rs` does not call `DaemonStatusOutput`; its step 4 calls `ensure_daemon` with a running daemon (first ping succeeds), unaffected by the Timeout rule or the 30s default.

## Blockers
None.

## Concerns
- [ ] C1: "Spawn at most once per call" can strand a call whose single spawn lost the flock to a daemon that was shutting down (the old daemon's 5s grace period, `main.rs:304`), because `spawned` is never reset. The call then waits the whole budget and returns "Connection refused" (AC0/AC1 string). Fix: allow a re-spawn when the failure kind changes back to non-`Timeout` after a `Timeout` phase (the daemon we waited on went away), or cap respawns at 2 with a minimum gap (for example one per 5s). Add a unit test: scripted `Timeout, Timeout, Io(refused), pong` expects 1 spawn; `Io, Timeout, Io, pong` expects 2.
- [ ] C2: The defaulted `status_probe` as written (`async` body that awaits `self.call(...)`) borrows `&self` across an await inside an `impl Future + Send`. `DaemonTransport` has no `Sync` supertrait, so this very likely fails to compile ("future cannot be sent between threads"); `McpRouter`'s own bound (`T: DaemonTransport + Send + Sync`) does not help the trait definition. Fix: in the default body create the call future first (`let fut = self.call(...); async move { match fut.await {...} }`), which is `Send` by the existing trait contract and does not capture `self`; or add `: Sync` to the trait. The plan should say which.
- [ ] C3: AC0's e2e does not exercise the stated failure mode of the Timeout rule. 4.1.1a asserts the stale socket exists and that the ping fails, but not that the failure is an `Io` (non-`Timeout`) error. Assert `matches!(err, PortError::Io(_))` right after the kill so a future change to the socket adapter cannot silently turn "dead daemon" into "busy daemon". Also, `startup_timeout: Some(30s)` plus `elapsed < 30s` is redundant (ensure_daemon would have returned `Err` at 30s) and makes the test sit on the Chrome cold-start edge on slow CI; use 45s for the call and assert `< 30s` separately if the 30s AC is the point. The test also drives `ensure_daemon`, not `SocketTransport::call`, so the real client path (45s constant) is only covered by a constant-value unit test.
- [ ] C4: `stapler_daemon_status` while the daemon is alive but busy (a 30s `read_website` in flight): `SocketTransport::status_probe`'s 2s ping returns `Timeout`, and the plan reports `pong: false` with an error. `pong` is documented as "reachable", so a busy daemon is reported as down, which will send agents down the wrong diagnosis path (and the 2s probe is also queued behind the serial accept loop). Fix: distinguish in the output (for example `error` text "reachable but busy" plus `pong` unspecified, or a `busy: true` field), or give the probe a longer timeout. At minimum add a test: probe with `Timeout` must not say "failed to start".
- [ ] C5: AC1 says "all eventually succeed" for N parallel `read_website` calls, but Known limit 1 concedes that more than about 4 queued 30s calls hit the client's 120s `CALL_TIMEOUT` (`transport.rs:16`) and fail with `timed out`. The plan reads "none returns Connection refused" as the bar, which is a weaker test than the requirement text, and the e2e test (4.1.1b) only calls `ensure_daemon`, never a real tool call. Either (a) raise `CALL_TIMEOUT` to cover queue depth (for example 300s) since the cost is only a longer wait, or (b) get the owner's explicit sign-off that AC1 means connect/startup only, and add a test of 8 parallel `ping`/`list_sessions` calls (fast handlers) through `client::call` after `ensure_daemon`. Also note each timed-out poll ping (`POLL_CAP` 500ms) leaves an abandoned queued connection that the daemon later answers into a closed peer; harmless after 1.1.1a but worth a sentence.
- [ ] C6: AC2 says the daemon "logs the per-connection error", yet no test asserts the log line (plan says verified "by code review and the e2e daemon log", but 4.1.1c never reads `daemon.log`). Add to 4.1.1c: after the 20 abandoned connections, read `daemon.log` and assert it contains `connection error (continuing)` (the pre-fix daemon would not have written it). Cheap and closes the only AC2 clause with no automated check.

## Minors
- Fatal `accept()` errors stay fatal (ADR/Story 1.1.1) and still `exit(1)` leaving a stale socket (`main.rs:964-966`); acceptable because clients now spawn on refused, but a transient `EMFILE`/`ECONNABORTED` still kills the daemon, which is the AC2 symptom class. Consider retrying with a short sleep for those kinds, or note it as a Known limit.
- `ensure_error_message` is reused by `status_probe` for a plain ping failure, so the status text would read "ensure daemon: ..." even though nothing tried to ensure; use a neutral prefix there.
- The doc claim "start exactly one daemon" in Story 2.1.1 should say "one winner; each caller may spawn one attempt, losers exit on flock".
- 2.1.1b's fake `SocketFactory` needs the associated types named `L` and `C` (`socket.rs:87-89`); the plan says "associated `Listener` type", wrong name only.
- The http-port-file clobber with parallel spawns (`main.rs:326-341`, before the lock) is correctly recorded as a known limit; with 8 parallel spawns and no `STAPLER_MCP_HTTP_PORT`, the 7 losers each `remove_file` the winner's port file if the winner had written one from the daemon's own env. Only matters when HTTP is enabled; keep it in Known limits.
- Status behavior change (no auto-spawn) is flagged for the PR; the bootstrap/docs that tell users `stapler_daemon_status` "starts the daemon" (if any) were not searched.
- Effort estimate and wave plan look internally consistent (4.1.x correctly serialized on the shared `Cargo.toml` edit in 4.1.1a only).
