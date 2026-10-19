# Validation Plan: mcp-streamable-http

**Date**: 2026-09-14

## Happy Path Scenario
Given `stapler-mcp --daemon` is running with `STAPLER_MCP_HTTP_PORT=47439` set and a bearer token already generated at `~/.stapler-mcp/http-token`, when an MCP client configured with `{"type": "http", "url": "http://127.0.0.1:47439/mcp", "headers": {"Authorization": "Bearer <token>"}}` POSTs a `tools/call` request for e.g. `stapler_browser_list_sessions`, then it receives a `200` with a valid MCP result identical in shape to what the stdio path returns for the same call — with no `stapler-mcp` stdio subprocess spawned for that client session.

Migration Plan is N/A per `plan.md` (no schema/data changes) — Step 5 (migration test) is skipped.

---

## Requirement → Test Mapping

Requirements are drawn from `requirements.md`'s Success Metrics, Scope, Constraints, NFRs, and Observability Requirements, cross-referenced against `plan.md`'s Stories/Tasks (which already specify most acceptance criteria as concrete Given/When/Then — this table converts each into a named test and states where the plan's own AC needed sharpening for determinism, notably the SIGTERM and bridge-saturation cases per the reviews' concern).

### REQ-1: Daemon serves MCP over Streamable HTTP on `127.0.0.1:<port>`, reachable by an http-transport MCP client

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-1 | `crates/cli/tests/http_transport.rs` | `initialize_request_over_http_should_return_valid_server_info_when_daemon_running` | Integration | POST a spec-shaped `initialize` request to `/mcp` with a valid bearer header against a real HTTP-enabled daemon; assert `200` and `serverInfo.name == "stapler-mcp"` (plan Story 3.1.1 AC, verbatim) |
| REQ-1 | `crates/cli/src/http_server.rs` (`#[cfg(test)]`) | `service_factory_should_build_mcp_router_with_channel_transport_when_invoked` | Unit | Call the `service_factory` closure (Task 3.1.1a) directly; assert it returns an `McpRouter<ChannelTransport>` whose `transport` holds a clone of the supplied `bridge_tx` (no live socket/HTTP server needed) |
| REQ-1 (no-per-session-process claim) | — | — | Manual | Not unit-testable in Rust: the claim is about Claude Code's process model, not `stapler-mcp`'s. Verified manually as part of Task 9.1.1 by confirming `ps aux \| grep stapler-mcp` shows exactly one process (the daemon) after a Claude Code session using the HTTP transport starts, versus N+1 today under stdio. Record the count in `NOTES.md` alongside Task 9.1.1a's reconnect-behavior note — this repo's automated suite cannot observe Claude Code's own process tree |

### REQ-2: All 27 `McpRouter` tools remain callable with identical schemas via HTTP as via stdio

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-2 | `crates/cli/tests/http_transport.rs` | `registered_tools_should_be_schema_identical_between_socket_and_channel_transport_when_compared` | Unit (no live daemon) | Task 8.1.1a: call `McpRouter::<SocketTransport>::registered_tools()` and `McpRouter::<ChannelTransport>::registered_tools()` (both via the `#[cfg(test)]` accessor); assert same length, and for every tool, identical `name`/`description`/`input_schema` — mechanically enforces Pattern Decisions row 1 (one source of truth for the tool list) |
| REQ-2 | `crates/cli/src/transport.rs` (`#[cfg(test)]`) | `mcp_router_call_should_return_tool_not_found_error_when_unknown_tool_name_given` | Unit (error path) | Construct an `McpRouter<ChannelTransport>` (or drive `DaemonTransport::call` directly) with a tool name not in the registry; assert the call returns `Err(...)` with a message matching `Daemon::handle_request`'s existing `"unknown tool {other:?}"` shape (`daemon.rs:70`) — proves the HTTP path surfaces the same error shape as stdio, not a bridge-specific one |
| REQ-2 | `crates/cli/tests/http_transport.rs` | `tools_call_over_http_should_return_identical_result_to_stdio_when_same_tool_and_params_given` | Integration | Task 8.1.1c (dual-transport): against one running HTTP-enabled daemon instance, call `stapler_browser_list_sessions` once via the existing stdio `client::call` (Unix socket) and once via an HTTP POST to `/mcp`; assert byte-identical (or structurally-identical, allowing for request-id differences) results. Guards against a partial-rollout regression where one transport silently diverges (`pitfalls.md` §3) |
| REQ-2 | `crates/cli/tests/tool_schema.rs` (renamed target: `mcp_router.rs`) | `should_list_twenty_seven_tools_with_nonempty_descriptions_and_matching_input_schema_when_tools_list_called` | Regression (Integration-adjacent) | Existing test (Task 1.2.1f), extended from its current 23-tool assertion set to the corrected 27-tool set (plan.md's front-matter correction) — must still pass verbatim after the `ThinClient` → `McpRouter` rename |

### REQ-3: Concurrent HTTP sessions call tools against shared daemon state without corruption or deadlock

*(Phase 8 concurrency emphasis — see "Concurrency & Shutdown Deep Dive" below for full test mechanics, not just the table row.)*

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-3 | `crates/cli/tests/http_transport.rs` | `concurrent_http_navigate_calls_should_both_succeed_with_distinct_session_ids_when_fired_via_tokio_join` | Concurrency | Task 8.1.2a: two `stapler_browser_navigate` calls fired concurrently via `tokio::join!` from two separate `reqwest` clients against the same running daemon; assert both return `200` with distinct, valid `sessionId`s |
| REQ-3 | `crates/cli/tests/http_transport.rs` | `session_id_created_by_one_http_request_should_be_reusable_by_a_second_independent_http_request` | Concurrency (sequential cross-request) | Task 8.1.2b: first HTTP POST returns `sessionId: "abc"`; a second, independent HTTP POST calls `stapler_browser_click` with that `sessionId`; assert success — replicates today's already-working cross-connection reuse (`research/features.md` §2) under the new stateless-HTTP design |
| REQ-3 | `crates/cli/tests/http_transport.rs` | `bridge_channel_should_complete_all_requests_without_drop_or_deadlock_when_burst_exceeds_channel_capacity` | Concurrency (saturation) | New — not an explicit plan.md task, added here per the review's flagged gap (see deep dive below). Fires a burst of 100 concurrent `stapler_browser_list_sessions` calls (>`BRIDGE_CHANNEL_CAPACITY = 64`) against one daemon; asserts all 100 eventually return `200` with valid bodies — none dropped, none hung past a bounded test timeout |

### REQ-4: Bearer-token auth rejects unauthenticated HTTP requests

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-4 | `crates/cli/src/http_server.rs` (`#[cfg(test)]`) | `require_bearer_token_should_call_next_when_correct_token_presented` | Unit (happy) | Task 4.2.1c: `tower::ServiceExt::oneshot` against the router directly with the correct `Authorization: Bearer <token>` header; assert the request reaches downstream (no `401`) |
| REQ-4 | `crates/cli/src/http_server.rs` (`#[cfg(test)]`) | `require_bearer_token_should_return_401_when_authorization_header_missing` | Unit (error) | Same harness, no `Authorization` header at all; assert `401` and that the rejection reason recorded is `"missing Authorization header"`, distinct from the wrong-token case |
| REQ-4 | `crates/cli/tests/http_transport.rs` | `http_request_with_wrong_bearer_token_should_return_401_and_correct_token_should_succeed_when_daemon_running` | Integration | Task 8.1.1b: against a real spawned HTTP-enabled daemon in an isolated `STAPLER_MCP_HOME`, POST with no header (expect `401`), then with the real token read from the generated `http-token` file (expect a valid MCP response for `stapler_browser_list_sessions`) |

### REQ-5: `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` pass with no new warnings

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-5 | `.github/workflows/ci.yml` (existing `native` job) | — | CI Gate | Not a designed test — an existing, unmodified CI gate this feature must not regress. Verified by running both commands locally before every PR and confirming CI's existing `native` job (fmt/clippy/build/test, lines 17-23) stays green through all 8 phases of implementation, not just at the end |

### REQ-6: HTTP listener is opt-in via `STAPLER_MCP_HTTP_PORT`; unset = today's stdio-only behavior, unmodified

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-6 | `crates/cli/tests/http_transport.rs` | `daemon_started_without_http_port_env_should_bind_no_new_tcp_listener_when_ss_checked` | Integration | Story 3.2.1 AC #1: spawn `stapler-mcp --daemon` with `STAPLER_MCP_HTTP_PORT` absent from its environment; assert `ss -ltnp` (or a raw `TcpStream::connect` probe against the plan's default port, cheaper than shelling to `ss`) shows nothing listening, and that `daemon_architecture_and_tools_round_trip` (`daemon_ping.rs`) still passes unmodified |
| REQ-6 | `crates/cli/tests/http_transport.rs` | `stapler_mcp_http_port_env_should_be_treated_as_disabled_when_value_is_not_a_valid_u16` | Integration (error/edge) | `STAPLER_MCP_HTTP_PORT=not-a-port` set before daemon start; assert the daemon still starts successfully (stdio path unaffected) and does not panic — Task 3.2.1a's `.and_then(|s| s.parse::<u16>().ok())` silently falls through to `None`/disabled rather than erroring, which this test pins down as intentional rather than an accidental swallow |

### REQ-7: Existing stdio thin-client path remains available and unmodified

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-7 | `crates/cli/tests/daemon_ping.rs` | `daemon_architecture_and_tools_round_trip` | Regression (Integration) | Existing test, run unmodified after Epic 1.2's `ThinClient` → `McpRouter<SocketTransport>` rename (Task 1.2.1g's explicit AC) — zero behavior change is the pass condition |
| REQ-7 | `crates/cli/src/transport.rs` (`#[cfg(test)]`) | `socket_transport_call_should_reproduce_call_daemon_behavior_when_used_as_default_transport` | Unit | Confirms `SocketTransport`'s extracted `call` body (Task 1.2.1c) is behavior-identical to the pre-refactor `call_daemon` free function — same `ensure_daemon` + `client::call` sequence, same `CALL_TIMEOUT` |

### REQ-8: SIGTERM triggers the same single graceful-shutdown path as `SHUTDOWN_TOOL`, exactly once, protecting in-flight requests and `Rc` sole ownership

*(Phase 8 concurrency emphasis — see "Concurrency & Shutdown Deep Dive" below.)*

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-8 | `crates/core/src/daemon.rs` (`#[cfg(test)]`) | `request_shutdown_should_set_the_shutdown_flag_and_cancel_the_token` | Unit | Task 2.1.1c, verbatim from plan.md: `daemon.request_shutdown()` sets `shutdown.get() == true` and `cancellation_token().is_cancelled() == true` |
| REQ-8 | `crates/core/src/daemon.rs` (`#[cfg(test)]`) | `shutdown_tool_dispatch_should_cancel_the_same_token_request_shutdown_does` | Unit | Task 2.1.1c, verbatim: a `SHUTDOWN_TOOL` RPC dispatch (not a direct `request_shutdown()` call) still unblocks a separate task awaiting `cancellation_token().cancelled()` — proves the RPC path and the future SIGTERM path funnel through one signal, not two independent ones |
| REQ-8 | `crates/core/src/daemon.rs` (`#[cfg(test)]`) | `run_cancellable_should_return_ok_promptly_when_cancelled_with_no_pending_connection` | Unit (error/edge — cancellation while idle) | Story 2.1.1 AC #3: `run_cancellable` on a bound socket with nothing connecting; cancel from another task; assert it returns `Ok(())` within a short bounded timeout (e.g. 100ms) rather than hanging for a connection |
| REQ-8 | `crates/cli/src/transport.rs` (`#[cfg(test)]`) | `bridge_consumer_should_reply_shutting_down_error_to_queued_messages_when_cancelled_mid_drain` | Unit | Story 2.2.1 AC #2: pre-load the bridge channel with 2-3 unconsumed `BridgeMessage`s, cancel the token, run the consumer's cancellation branch + drain loop; assert every pending `oneshot::Receiver` resolves to `Response::err("daemon shutting down")` rather than a dropped-sender `RecvError` |
| REQ-8 | `crates/cli/tests/shutdown.rs` (new) | `sigterm_mid_request_should_complete_or_fail_cleanly_and_run_cleanup_exactly_once` | Shutdown (Integration) | Task 8.3.1a, see deep dive below for exact mechanics |

### REQ-9: Bridge channel is bounded (capacity 64) and logs backpressure distinctly under saturation

*(Phase 8 concurrency emphasis — see deep dive below.)*

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-9 | `crates/cli/src/transport.rs` (`#[cfg(test)]`) | `channel_transport_call_should_complete_independently_when_two_clones_call_concurrently` | Unit | Task 2.3.1b, verbatim: two `ChannelTransport` clones from the same sender call `.call("ping"-equivalent, ...)` concurrently via `tokio::join!`; both resolve without either blocking indefinitely |
| REQ-9 | `crates/cli/src/transport.rs` (`#[cfg(test)]`) | `channel_transport_call_should_return_error_when_bridge_receiver_dropped` | Unit (error) | Drop the `bridge_rx`/consumer task before calling; assert `ChannelTransport::call` returns a `String` error (channel-closed), mapped to the same error shape `SocketTransport` uses for an unreachable daemon — not a panic or an unhandled `SendError` |
| REQ-9 | `crates/cli/src/transport.rs` (`#[cfg(test)]`, `start_paused = true`) | `channel_transport_send_should_trigger_backpressure_branch_when_blocked_past_250ms` | Unit (deterministic, paused-clock) | See deep dive below — replaces a flaky wall-clock burst test with a `tokio::time::pause()`-driven one |
| REQ-9 | `crates/cli/tests/http_transport.rs` | `daemon_log_should_contain_backpressure_line_when_burst_saturates_bridge_channel` | Integration (best-effort, see deep dive) | Real-daemon companion to the paused-clock unit test above — confirms the log line actually reaches `daemon.log` end-to-end, accepting this one is inherently timing-sensitive and documenting that explicitly rather than asserting it strictly |

### REQ-10: HTTP bind failure degrades to stdio/socket-only rather than crashing the daemon

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-10 | `crates/cli/src/http_server.rs` (`#[cfg(test)]`) | `run_http_server_should_log_addr_in_use_and_return_non_fatal_error_when_port_already_bound` | Unit (error) | Task 3.1.1d: pre-bind a `TcpListener` to a port, then call `run_http_server` on the same port; assert it returns an `Err` (not a panic) that `run_daemon`'s caller treats as non-fatal, and that the error variant is distinguishable as `AddrInUse` specifically |
| REQ-10 | `crates/cli/tests/http_transport.rs` | `daemon_should_keep_serving_stdio_when_http_port_already_in_use_at_startup` | Integration | Pre-bind the configured `STAPLER_MCP_HTTP_PORT` with a dummy `TcpListener` before spawning the daemon; assert the daemon still starts, the Unix socket still answers `ping`/`tools/call`, and the process does not exit — only the HTTP arm is degraded |

### REQ-11: Bearer token is generated once (CSPRNG, 0600), reused on subsequent starts, never logged

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-11 | `crates/native/src/http_token.rs` (`#[cfg(test)]`) | `generate_or_load_should_create_a_new_token_when_none_exists` | Unit | Task 4.1.1d, verbatim: no `http-token` file present; `generate_or_load` creates one, mode `0600`, 64-char hex |
| REQ-11 | `crates/native/src/http_token.rs` (`#[cfg(test)]`) | `generate_or_load_should_reuse_an_existing_token_when_file_present` | Unit | Task 4.1.1d, verbatim: pre-write a token file; `generate_or_load` returns its exact contents rather than generating a new one |
| REQ-11 | `crates/native/src/http_token.rs` (`#[cfg(test)]`) | `generate_or_load_should_return_err_when_token_file_directory_is_unwritable` | Unit (error) | New: base dir's parent made read-only (`std::fs::set_permissions`, Unix-only, skip on non-Unix); assert `generate_or_load` returns a clean `Err`, not a panic — the write-failure branch has no explicit plan.md AC and is worth pinning down given the security-sensitivity of this file |
| REQ-11 | `crates/cli/tests/http_transport.rs` | `daemon_log_should_never_contain_the_generated_token_value_when_grepped` | Integration | Story 4.1.1 AC #2, verbatim: spawn a real HTTP-enabled daemon with no pre-existing token file; grep `daemon.log` for the token's literal value (read back from the generated file) after startup; assert zero matches |

### REQ-12: `stapler-mcp --print-config` prints a ready-to-paste, idempotent config block; never fabricates a token

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-12 | `crates/cli/tests/http_transport.rs` (or new `crates/cli/tests/cli_commands.rs`) | `print_config_stdout_should_be_valid_json_with_real_token_when_token_present` | Integration | Story 7.1.2 AC #1: run the built binary (`env!("CARGO_BIN_EXE_stapler-mcp")`, `--print-config`) against a fixture dir with a pre-generated token; parse stdout as JSON; assert `headers.Authorization == "Bearer <the real token>"` and `url` matches the configured port |
| REQ-12 | same file | `print_config_stdout_should_be_plain_text_not_json_when_token_absent` | Integration (error) | Story 7.1.2 AC #2: run against a fixture dir with no token file; assert stdout does **not** parse as JSON (`serde_json::from_str` returns `Err`) and instead contains the exact `STAPLER_MCP_HTTP_PORT=... stapler-mcp --daemon` suggestion, with no fabricated token string appearing anywhere in the output |
| REQ-12 | same file | `print_config_output_should_be_byte_identical_across_two_consecutive_runs` | Integration | UX surface 3 AC #4: run `--print-config` twice in a row against the same fixture; assert stdout is byte-for-byte identical both times |

### REQ-13: `stapler-mcp --status` reports daemon/HTTP reachability with next-step guidance and correct scriptable exit codes

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-13 | `crates/cli/tests/http_transport.rs` (or `cli_commands.rs`) | `status_output_should_report_not_running_with_next_step_command_when_no_daemon` | Integration (error/edge) | Story 7.1.1 AC #1: run `--status` against an empty `STAPLER_MCP_HOME` with nothing listening; assert stdout contains `"daemon: not running"` plus a concrete next-step command, and exit code is nonzero |
| REQ-13 | same file | `status_output_should_report_running_pid_and_http_listening_when_daemon_and_http_up` | Integration | Story 7.1.1 AC #2: run `--status` against a real running HTTP-enabled daemon; assert stdout contains `"daemon: running (pid <N>)"` and `"http: listening on 127.0.0.1:<port>"`, and exit code is `0` |
| REQ-13 | same file | `status_output_should_distinguish_http_disabled_from_http_unreachable_when_probed` | Integration | UX surface 4 AC #4: run once against a daemon started with no `STAPLER_MCP_HTTP_PORT` (expect `"http: not listening (STAPLER_MCP_HTTP_PORT not set — stdio only)"`) and once against a daemon started with the port set but the listener pre-occupied by another process (expect a distinct "port unreachable" message) — asserts these two log lines are textually different, since collapsing them sends an operator down the wrong branch |
| REQ-13 | same file | `status_http_probe_should_succeed_without_reading_token_file_when_port_listening` | Integration | UX surface 4 AC #2: delete/rename the token file before running `--status` against a running HTTP-enabled daemon; assert the HTTP line still reports "listening" (raw TCP-connect probe only, no auth attempted) |

### REQ-14: Tracked `mcp-servers.json` never contains a literal token; HTTP config lives only in an untracked, machine-local override

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-14 | repo root (CI/pre-commit style check, not `cargo test`) | `tracked_mcp_servers_json_should_not_contain_bearer_or_authorization_when_grepped` | Script/CI grep | UX surface 1 AC #1: a one-line `grep -qi "bearer\|authorization" mcp-servers.json` (exit nonzero on match) added to CI or run manually before every commit touching that file — cheap enough to automate even though the plan doesn't currently wire this into `.github/workflows/ci.yml`; flagged as a gap below |
| REQ-14 | — | — | Manual | UX surface 1 AC #3/#4: operator walkthrough — switching a machine to HTTP edits exactly one block in one untracked file; reverting is deleting that block. Verified once by hand during Task 9.1.1's manual pass, not automatable (depends on Claude Code's own override-file mechanism, referenced generically in `ux.md`) |

### REQ-15: README documents both transports, troubleshooting steps, and the token-distribution warning

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-15 | `README.md` | — | Doc/Manual | Task 7.2.1a-d: reviewer checklist — architecture diagram shows both stdio and HTTP arrows; troubleshooting section lists the 3 commands in the exact order `systemctl status` → `--status` → `systemctl start`/`--daemon`; token-distribution warning names `--print-config` and explicitly calls out the `${ENV_VAR}`-in-`headers` bug as unsafe. No automated test — this is prose review, listed explicitly (per the validation template) rather than silently assumed done |

### REQ-16: systemd user unit and launchd agent `.example` templates ship with auto-restart policy by default

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-16 | `crates/cli/tests/service_templates.rs` (new, cheap file-content assertions — no systemd/launchd needed in CI) | `systemd_template_should_contain_restart_on_failure_and_start_limit_fields_when_read` | Integration (file-content, automatable) | Read `scripts/stapler-mcp.service.example` as a string; assert it contains `Restart=on-failure`, `RestartSec=5`, `StartLimitBurst=5`, `StartLimitIntervalSec=60` — catches an operator-facing regression (e.g. someone strips the restart policy in a later edit) without needing real systemd in CI |
| REQ-16 | same file | `launchd_plist_template_should_contain_keepalive_and_throttleinterval_fields_when_read` | Integration (file-content) | Read `scripts/com.tstapler.stapler-mcp.plist.example`; assert it contains `KeepAlive` and `ThrottleInterval` keys |
| REQ-16 | same file | `readme_persistent_service_section_should_reference_both_template_files_when_read` | Integration (file-content) | Read `README.md`; assert the "Running as a persistent service" section's text contains both `stapler-mcp.service.example` and `com.tstapler.stapler-mcp.plist.example` filenames (Task 5.2.1c) |
| REQ-16 | manual (`systemd-analyze verify`, `plutil -lint`) | — | Manual | Story 5.2.1 ACs' own syntax-validity checks — explicitly deferred to manual per plan.md ("run manually, not in CI — no systemd in the CI container") |

### REQ-17: HTTP session start/end and rejected-auth requests are logged distinctly from tool-call errors

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-17 | `crates/cli/tests/http_transport.rs` | `daemon_log_should_contain_distinct_lines_for_missing_header_and_invalid_token_rejections` | Integration | Story 4.2.1 AC, verbatim: trigger both a no-header request and a wrong-token request against a real daemon; grep `daemon.log` for two textually distinct lines (`"missing Authorization header"` vs `"invalid bearer token"`), neither matching the shape of an ordinary tool-call error line |
| REQ-17 | same file | `daemon_log_should_never_echo_the_presented_wrong_token_value_when_rejecting_a_request` | Integration | Story 4.2.1's explicit "never the presented value" constraint: send a request with a distinctive, greppable wrong token (e.g. `"WRONG-TOKEN-MARKER-xyz"`); assert that literal string never appears in `daemon.log` or the HTTP response body |
| REQ-17 | same file | `http_connection_start_and_end_log_lines_should_share_a_correlation_id_when_one_request_completes` | Integration | Task 6.1.1c: make one HTTP request; grep `daemon.log` for a `"http request start {id} ..."` line and a `"http request end {id} status=... elapsed_ms=..."` line sharing the same `{id}` |

### REQ-18: `daemon.log` is created with mode 0600

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-18 | `crates/native/src/spawn.rs` (`#[cfg(test)]`) | `daemon_log_should_be_created_with_mode_0600_when_opened_fresh` | Unit | Task 6.1.1a: open a fresh log path via the updated `OpenOptions` (with `.mode(0o600)`) in a tempdir; assert `std::fs::metadata(...).permissions().mode() & 0o777 == 0o600` — no live daemon needed |

### REQ-19: CI scans dependencies for known vulnerabilities

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-19 | `.github/workflows/ci.yml` | — | CI Gate | Task 8.2.1a: `cargo audit` step added to the `native` job; verified by intentionally pinning a `Cargo.lock` entry to a version with a known RUSTSEC advisory in a throwaway branch and confirming CI fails, then reverting — a one-time manual proof during implementation, not a recurring test |

### REQ-20: The `!Send` core and `crates/wasm` adapter are unaffected by this feature

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-20 | existing wasm CI job / `cargo build --workspace` | `cargo_build_workspace_should_succeed_for_wasm32_target_when_core_gains_cancellation_token_field` | Build-gate (Regression) | Task 1.1.1c's `cfg(not(target_arch = "wasm32"))`-scoping AC: after `tokio-util`/`"macros"` are added to `crates/core`, `cargo build --workspace` (which includes the existing wasm32 target build, per whatever CI job already does this) still succeeds with no new wasm-side dependency pulled in |
| REQ-20 | — | — | Regression (no new test needed) | `Daemon::run` (used by `crates/wasm/src/lib.rs:257`) is left byte-for-byte unchanged per Tech Debt Disposition's last row — the existing wasm build/test job is the regression gate; this plan adds `Daemon::run_cancellable` as a new sibling method instead of touching `run`, so no wasm-side test needs to change |

### REQ-21: A single hung tool call is bounded by a timeout and doesn't block concurrent unrelated calls (pre-mortem.md P1 #1)

*Not one of `requirements.md`'s original numbered requirements — added by `pre-mortem.md`'s P1 gate; sharpened into `plan.md` as Story 2.1.2 (core-level timeout) and Story 8.1.2's Task 8.1.2c (integration-level proof). Distinct from REQ-3/REQ-9's existing concurrency/saturation rows: those cover many fast requests contending for channel capacity, not one request that never returns at all.*

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-21 | `crates/core/src/daemon.rs` (`#[cfg(test)]`) | `handle_request_should_time_out_a_hung_handler_instead_of_blocking_forever` | Unit | Task 2.1.2b: a registered handler that never resolves (`std::future::pending()`); assert `handle_request` returns `Response::err(...)` within `REQUEST_TIMEOUT` rather than hanging |
| REQ-21 | `crates/core/src/daemon.rs` (`#[cfg(test)]`) | `handle_request_should_still_serve_subsequent_calls_after_a_timeout` | Unit | Task 2.1.2b: after the hung call above times out, a second `handle_request(Request{tool:"ping",..})` on the same `Daemon` still returns `Response::ok(json!({"pong": true}))` — proves the dispatch core is freed, not wedged |
| REQ-21 | `crates/cli/tests/http_transport.rs` | `unrelated_concurrent_http_calls_should_complete_when_one_call_is_hung` | Concurrency | Task 8.1.2c: in-process `Daemon` + bridge consumer + HTTP server (Task 2.3.1b's harness pattern) with one test-only handler that never resolves; fire it on a background task, then fire two unrelated fast calls concurrently via `tokio::join!`; assert both complete within a short bound while the hung call's own response eventually resolves to the Task 2.1.2a timeout error rather than either request hanging the test |

### REQ-22: A tool-handler panic inside the bridge consumer doesn't kill the HTTP transport (pre-mortem.md P1 #2)

*Not one of `requirements.md`'s original numbered requirements — added by `pre-mortem.md`'s P1 gate, sharpening the CONCERN already on record in `adversarial-review.md` ("No supervision or recovery if a tool call panics inside the bridge consumer task"). Sharpened into `plan.md` as Task 2.2.1e (`catch_unwind` + distinct log line) and Story 8.1.3 (integration-level proof).*

| Requirement | Test File | Test Name | Type | Scenario |
|---|---|---|---|---|
| REQ-22 | `crates/cli/tests/http_transport.rs` | `panicking_handler_should_return_clean_error_and_log_distinct_line_instead_of_killing_consumer` | Integration | Task 8.1.3a: in-process harness (Task 8.1.2c's pattern) with a test-only handler that panics; fire an HTTP request at it; assert the response is a clean error (not a hung connection) and captured log output contains `"bridge consumer panicked"` (Task 2.2.1e), not a generic tool-call-error line |
| REQ-22 | `crates/cli/tests/http_transport.rs` | `http_transport_should_keep_serving_requests_after_a_handler_panic` | Integration | Task 8.1.3a: immediately after the panicking call above, a second, unrelated HTTP request to a fast handler on the same daemon returns `200` — proves the bridge consumer task and its `mpsc::Receiver` are still alive, not permanently dead until a manual restart |

---

## Concurrency & Shutdown Deep Dive

The requirements table above names the three edge cases flagged by architecture-review and adversarial-review as needing real test designs, not prose assurance. Full mechanics below — these are the cases most likely to be underspecified if implemented from the table rows alone.

### 1. SIGTERM mid-request (`sigterm_mid_request_should_complete_or_fail_cleanly_and_run_cleanup_exactly_once`, `crates/cli/tests/shutdown.rs`)

**Problem with a naive design**: racing a real SIGTERM against a real in-flight browser-automation call is inherently timing-sensitive — if the call finishes before the signal lands, the test asserts nothing about the shutdown path at all; if it's too slow, the test is flaky in CI.

**Design**: reuse `daemon_ping.rs`'s existing `spawn_mock_brave_server` pattern (a hand-rolled `TcpListener` that accepts a connection but controls exactly when it writes a response) to build a mock HTTP endpoint that accepts the connection immediately, then deliberately waits 2 seconds before writing any response body. Steps:
1. Spawn a real `stapler-mcp --daemon` with `STAPLER_MCP_HTTP_PORT` set, in an isolated `STAPLER_MCP_HOME` (mirrors `TestEnv` in `daemon_ping.rs`).
2. Fire `stapler_browser_navigate` (or `fetch_page`, whichever tool call this repo's browser layer supports pointing at an arbitrary local URL with a controllable delay) at the slow mock endpoint's URL, via an HTTP POST to `/mcp`, on a background task.
3. Sleep 200ms (long enough that the request has been accepted and is genuinely in flight, short enough that it's nowhere near the mock server's 2s delay) then send `SIGTERM` to the daemon's PID.
4. Await the background request's outcome with a bounded timeout of `SHUTDOWN_GRACE_TIMEOUT` (5s per plan.md) + 2s margin. Assert the outcome is *either* a valid HTTP response *or* a clean connection-level error (`reqwest::Error` whose `is_connect()`/`is_request()` is true) — never a bare hang past the timeout.
5. Assert the daemon process itself exits within the same bound (`tokio::process::Child::wait()` with a timeout).
6. Grep `daemon.log` for the cleanup sequence's marker lines (whatever exact strings `shutdown_cleanup`, Task 2.2.1d, emits — e.g. "reaper aborted" / "browser closed", following the existing pre-refactor cleanup's log lines at `main.rs:462-485`) and assert they appear **exactly once** — the test that actually proves there is one shutdown path, not a race between an old independent SIGTERM-handler copy and the new joined one.

This closes the gap the plan itself calls out: architecture-review's Blocker 1 and adversarial-review's Blocker 2 both named this exact race as unverified by any existing task before Task 8.3.1a was added; this test design makes Task 8.3.1a concrete rather than leaving "send SIGTERM mid-request" underspecified.

### 2. Bridge-channel saturation (`channel_transport_send_should_trigger_backpressure_branch_when_blocked_past_250ms`, `crates/cli/src/transport.rs`, plus its real-daemon companion in `http_transport.rs`)

**Problem with a naive design**: Story 6.1.1's own AC ("fire a burst of concurrent requests, assert the log line appears after 250ms") is a real-clock race — whether any single send actually blocks past 250ms depends on how fast `Daemon::handle_request` happens to drain the queue on the test machine, which is exactly the kind of environment-dependent timing this codebase's own `daemon_ping.rs` avoids by using generous, not tight, timeouts.

**Design — deterministic unit test (primary)**: construct a `tokio::sync::mpsc::channel::<BridgeMessage>(1)` directly (capacity 1, not the real `BRIDGE_CHANNEL_CAPACITY = 64`, deliberately shrunk so saturation is trivial to force) inside a `#[tokio::test(start_paused = true)]` test. Fill the one slot with a dummy message (no consumer draining — the test never spawns the bridge consumer task at all, isolating `ChannelTransport::call`'s send-side logic from `Daemon` entirely). Call `ChannelTransport::call(...)` on a second, pending message; this blocks on `bridge_tx.send(...).await`. From a second task, `tokio::time::advance(Duration::from_millis(300))`. Assert the >250ms backpressure branch fires — this requires Task 6.1.1b's implementation to expose *some* observable signal for testability (e.g. return an internal enum distinguishing "sent immediately" vs "sent after backpressure warning," or accept an injectable logging sink), which this validation plan flags as a testability requirement on that task, not just an implementation-detail nicety. `tokio::time::pause()` makes this fully deterministic — no real 250ms wall-clock wait, no flakiness.

**Design — real-daemon companion (secondary, best-effort)**: `daemon_log_should_contain_backpressure_line_when_burst_saturates_bridge_channel` fires a burst of 100 concurrent calls (well over the real capacity of 64) against a real running daemon and greps for the log line, documented explicitly as best-effort/non-blocking on CI flakiness — if this one is ever observed flaky, the deterministic paused-clock unit test above is the test that actually gates correctness; this one is a real-world sanity check on top.

**Design — no-drop guarantee (`bridge_channel_should_complete_all_requests_without_drop_or_deadlock_when_burst_exceeds_channel_capacity`, `http_transport.rs`)**: fire 100 concurrent `stapler_browser_list_sessions` calls via `futures::future::join_all` (already a `crates/cli` dev-dependency) against one real daemon; assert every one of the 100 futures resolves to `Ok(200)` within a generous bounded timeout (e.g. 30s) — the actual correctness property (`mpsc::channel`'s bounded semantics mean `send` backpressures rather than drops, so this should hold by construction, but it's the one test that would catch a bridge-consumer bug that silently swallows a message instead of replying).

### 3. Concurrent HTTP sessions against shared browser-pool state

Already reasonably concrete in plan.md (Task 8.1.2a/b); the one addition worth calling out: use a **fast, side-effect-light** navigation target for the concurrency test rather than a real outbound network fetch (`daemon_ping.rs`'s existing `fetch_page` test already covers real-network behavior separately) — either `about:blank` or a locally-hosted static page — so this test's runtime and flakiness profile is dominated by the concurrency logic under test, not network latency. `browser_session.rs`'s existing tests are the precedent to follow for whatever local-page convention that file already uses.

---

## UX Acceptance Tests

32 acceptance criteria across the 8 non-interactive surfaces in `design/ux.md`, one test per criterion. Several are automatable as integration tests (spawning the real binary/daemon and asserting on stdout/exit code/log contents); the rest are manual/doc-review checklist items, marked explicitly rather than silently skipped, per the validation template's instruction to cover every UX acceptance criterion with a test *or* a manual step.

| UX Criterion | Test File | Test Name | Tool | Steps |
|---|---|---|---|---|
| 1.1 Tracked `mcp-servers.json` never contains a literal token | repo root | `tracked_mcp_servers_json_should_not_contain_bearer_or_authorization_when_grepped` | Script/grep | `grep -qi "bearer\|authorization" mcp-servers.json`; assert no match |
| 1.2 README names `${ENV_VAR}`-in-`headers` substitution as unsafe, explicitly | `README.md` | — | Manual | Reviewer confirms the exact sentence exists in the token-distribution section |
| 1.3 Switching to HTTP requires editing exactly one block in one untracked file | — | — | Manual | Operator walkthrough: start from a stdio-only machine, follow the documented steps, confirm `git diff` shows zero changes to the tracked `mcp-servers.json` |
| 1.4 Reverting to stdio is deleting/reverting that one block, nothing else | — | — | Manual | Same walkthrough in reverse; confirm the tracked file was never touched |
| 2.1 Token value never appears in startup output or `daemon.log` | `crates/cli/tests/http_transport.rs` | `daemon_log_should_never_contain_the_generated_token_value_when_grepped` | Integration | (same test as REQ-11's row above) |
| 2.2 Startup message names the exact next command (`--print-config`) | `crates/cli/tests/http_transport.rs` | `first_run_http_startup_message_should_name_print_config_command_when_token_generated` | Integration | Spawn daemon with HTTP enabled and no prior token; assert stdout/log contains the literal substring `stapler-mcp --print-config` |
| 2.3 Second daemon start prints no "generated new token" line | `crates/cli/tests/http_transport.rs` | `second_daemon_start_should_omit_generated_token_line_when_token_file_already_exists` | Integration | Start, stop, restart the daemon; assert the "generated new token" line appears on the first start's log only |
| 2.4 File permissions (`0600`) stated in the message itself | `crates/cli/tests/http_transport.rs` | `first_run_startup_message_should_state_0600_permission_when_token_generated` | Integration | Assert stdout/log contains `0600` (or equivalent explicit permission text) alongside the token-generation line |
| 3.1 Output with token present is valid JSON, no prose mixed in | `crates/cli/tests/http_transport.rs` (or `cli_commands.rs`) | `print_config_stdout_should_be_valid_json_with_real_token_when_token_present` | Integration | (same test as REQ-12's row above) |
| 3.2 Output with no token present is not JSON | same file | `print_config_stdout_should_be_plain_text_not_json_when_token_absent` | Integration | (same test as REQ-12's row above) |
| 3.3 No-token path never fabricates a placeholder token | same file | `print_config_should_not_fabricate_a_token_string_when_none_exists` | Integration | Assert the no-token output contains no string matching a hex-token shape (`[0-9a-f]{16,}`) |
| 3.4 Running twice produces byte-identical output | same file | `print_config_output_should_be_byte_identical_across_two_consecutive_runs` | Integration | (same test as REQ-12's row above) |
| 4.1 "not running" case always includes a next-step command | `cli_commands.rs` | `status_output_should_report_not_running_with_next_step_command_when_no_daemon` | Integration | (same test as REQ-13's row above) |
| 4.2 HTTP probe is a raw TCP connect, never needs the token file | `cli_commands.rs` | `status_http_probe_should_succeed_without_reading_token_file_when_port_listening` | Integration | (same test as REQ-13's row above) |
| 4.3 Exit code nonzero when not running, zero when running | `cli_commands.rs` | `status_exit_code_should_be_nonzero_when_stopped_and_zero_when_running` | Integration | Assert `Command::status().code()` differs between the two scenarios as specified |
| 4.4 Distinguishes "HTTP never enabled" from "HTTP enabled but unreachable" | `cli_commands.rs` | `status_output_should_distinguish_http_disabled_from_http_unreachable_when_probed` | Integration | (same test as REQ-13's row above) |
| 5.1 README troubleshooting lists the 3 commands in exact order | `README.md` | — | Manual | Reviewer confirms order: `systemctl --user status` → `stapler-mcp --status` → `systemctl --user start` / `--daemon` |
| 5.2 Binding failure is fast (`ECONNREFUSED`), never hangs | `crates/cli/tests/http_transport.rs` | `http_connect_should_fail_fast_with_econnrefused_when_daemon_not_running` | Integration | `TcpStream::connect` (or a `reqwest` request) against the configured port with no daemon running; assert the error returns within a short bound (e.g. 1s), not a stalled connection |
| 5.3 Exit path always terminates in a daemon-start command | `README.md` | — | Manual | Reviewer confirms no dead-end prose exists in the troubleshooting section |
| 5.4 Stdio fallback documented in the same troubleshooting section | `README.md` | — | Manual | Reviewer confirms the stdio-fallback pointer is co-located with the HTTP troubleshooting steps, not a separate/harder-to-find section |
| 6.1 401 body distinguishes missing header from wrong token | `crates/cli/tests/http_transport.rs` | `http_401_response_body_should_distinguish_missing_header_from_invalid_token` | Integration | Two requests (no header; wrong token); assert distinct response bodies |
| 6.2 Rejected-request log line is visually distinct from a tool-call error line | `crates/cli/tests/http_transport.rs` | `daemon_log_should_contain_distinct_lines_for_missing_header_and_invalid_token_rejections` | Integration | (same test as REQ-17's row above, plus a comparison against a genuine tool-call error's log shape) |
| 6.3 Neither log nor response ever echoes the presented (wrong) token | `crates/cli/tests/http_transport.rs` | `daemon_log_should_never_echo_the_presented_wrong_token_value_when_rejecting_a_request` | Integration | (same test as REQ-17's row above) |
| 6.4 Exit path is always "re-run `--print-config` and re-paste" | `README.md` | — | Manual | Reviewer confirms the 401-troubleshooting text names `--print-config` explicitly, not just "check your token" |
| 7.1 No persistent session to go stale under `stateful_mode: false` | `crates/cli/src/http_server.rs` (`#[cfg(test)]`) | `streamable_http_config_should_have_stateful_mode_false_when_constructed` | Unit | Inspect the constructed `StreamableHttpServerConfig` struct's `stateful_mode` field directly; assert `false` |
| 7.2 SIGTERM runs the same cleanup as `SHUTDOWN_TOOL`, gracefully | `crates/cli/tests/shutdown.rs` | `sigterm_mid_request_should_complete_or_fail_cleanly_and_run_cleanup_exactly_once` | Integration | (same test as REQ-8's deep-dive row above) |
| 7.3 `NOTES.md` records the manual reconnect-verification outcome either way | `NOTES.md` | — | Manual | Task 9.1.1a: Tyler performs the kill-and-restart check against a real Claude Code session, records the outcome |
| 7.4 Fallback-to-stdio documented as the escape hatch when the client doesn't reconnect | `README.md` | — | Manual | Reviewer confirms this is stated explicitly, not left implicit |
| 8.1 Template header states the exact copy/enable/verify sequence | `scripts/stapler-mcp.service.example` | — | Manual | Reviewer confirms the header comment lists `daemon-reload` → `enable --now` → `--status` |
| 8.2 `Restart=on-failure` + `RestartSec`/`StartLimitBurst` present by default | `crates/cli/tests/service_templates.rs` | `systemd_template_should_contain_restart_on_failure_and_start_limit_fields_when_read` | Integration (file-content) | (same test as REQ-16's row above) |
| 8.3 launchd plist's `KeepAlive`/`ThrottleInterval` are the documented equivalent | `crates/cli/tests/service_templates.rs` | `launchd_plist_template_should_contain_keepalive_and_throttleinterval_fields_when_read` | Integration (file-content) | (same test as REQ-16's row above) |
| 8.4 Both templates cross-referenced from README's persistent-service section | `crates/cli/tests/service_templates.rs` | `readme_persistent_service_section_should_reference_both_template_files_when_read` | Integration (file-content) | (same test as REQ-16's row above) |

---

## Test Stack

- **Unit**: `cargo test`, Rust's built-in `#[test]` / `#[tokio::test]` (including `#[tokio::test(start_paused = true)]` for the deterministic backpressure test), `assert!`/`assert_eq!` from `std` — no external assertion crate is used anywhere in this codebase today (confirmed: no `pretty_assertions`/`claims`/`assert_matches` in any `Cargo.toml`), so none is introduced for this feature either.
- **Integration**: `#[tokio::test]` spawning the real `stapler-mcp` binary (`env!("CARGO_BIN_EXE_stapler-mcp")`) against an isolated `STAPLER_MCP_HOME` tempdir, following `crates/cli/tests/daemon_ping.rs`'s existing `TestEnv`/`spawn_mock_*_server` conventions exactly. HTTP-side calls use `reqwest` (already a workspace dependency of `crates/native`, resolved at `0.13.4`) and `futures::future::join_all`/`tokio::join!` (already a `crates/cli` dev-dependency) for concurrency fan-out.
- **E2E / UX**: no browser/GUI involved (per `design/ux.md`'s framing, this feature has no GUI) — "E2E" here means the same real-binary integration tests above, plus the explicit Manual rows in the UX table for the handful of criteria that are genuinely about prose (README wording) or a third-party client's behavior (Claude Code's reconnect handling) that this repo's test suite cannot observe.

## Gaps Found During Validation

- **`reqwest` is not yet a `crates/cli` dev-dependency.** `crates/cli/Cargo.toml`'s `[dev-dependencies]` currently lists only `tempfile` and `futures` (verified by reading the file). Plan.md's Task 8.1.1b already assumes "a `reqwest` client POSTs..." without an explicit Cargo.toml task to add it there. No version-conflict risk — `reqwest = "0.13"` is already resolved workspace-wide via `crates/native`'s non-dev dependency — but this is a real one-line gap: add `reqwest = { version = "0.13", features = ["rustls"], default-features = false }` to `crates/cli/Cargo.toml`'s `[dev-dependencies]` as part of Epic 8.1's setup, before Task 8.1.1b is written.
- **Story 6.1.1's backpressure AC is a wall-clock race as written.** The plan's own Given/When/Then ("a burst saturates the channel... takes longer than 250ms... a line appears") has no task that makes this deterministic. This validation plan's deep-dive section above proposes the fix (a `start_paused = true` unit test isolating `ChannelTransport::call`'s send logic from a real consumer) and treats it as a testability requirement on Task 6.1.1b's implementation, not an optional nicety — flagging this now so the implementer doesn't build the log line in a way that's only observable via real timing.
- **`crates/cli/tests/service_templates.rs` and `crates/cli/tests/cli_commands.rs` are new files this validation plan introduces that plan.md doesn't explicitly name** (plan.md's Story 5.2.1/7.1.1/7.1.2 tasks describe the *behavior* to implement but not a specific test file for the file-content/CLI-invocation checks). Recommend creating them during Phase 8 rather than folding everything into the already-large `http_transport.rs`, to keep the systemd/launchd file-content checks (which need no running daemon at all) fast and independent of the daemon-spawning integration tests.
- **REQ-14's tracked-file grep check has no CI wiring named in plan.md.** `.github/workflows/ci.yml`'s existing `native` job doesn't include a step for this; it's cheap enough (a single `grep`) that this validation plan recommends adding it to Epic 8.2 alongside the `cargo-audit` step, rather than leaving it as a purely manual pre-commit habit that's easy to forget on a solo project.

## Coverage Targets and How to Measure

| Stack | Coverage command | Target |
|---|---|---|
| Rust (all crates) | `cargo tarpaulin --out Stdout --workspace` | ≥80% line, with the explicit exception that Manual-only UX rows (README prose, `.example` template syntax validity, the Claude Code reconnect check) are not expected to move this number — they have no corresponding source line to cover |

- All public service methods touched by this feature (`Daemon::handle_request`, `Daemon::request_shutdown`, `Daemon::run_cancellable`, `DaemonTransport::call` for both impls, `require_bearer_token`, `generate_or_load`): happy path + error paths covered per the table above.
- Both external-facing integrations this feature adds (the HTTP listener itself, and the bearer-token file on disk) have unit-mocked coverage plus at least one integration test against the real daemon binary.
- All 32 UX acceptance criteria in `design/ux.md` have a corresponding automated test or an explicitly named manual step — none silently omitted.
