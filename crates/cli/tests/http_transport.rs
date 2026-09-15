//! Integration coverage for the Streamable HTTP transport (plan.md Phase 8,
//! Epics 8.1/8.3). Two harnesses are used, matching the plan's own split:
//!
//! - **Real subprocess** (`spawn_http_daemon`): the actual `stapler-mcp`
//!   binary, started with `STAPLER_MCP_HTTP_PORT` set, exercised over a real
//!   `TcpListener` via `reqwest` — used wherever the test needs the real
//!   fixed 27-tool set or real process lifecycle (auth, dual-transport,
//!   concurrency, SIGTERM).
//! - **In-process harness** (`spawn_in_process_harness`): a bare `Daemon` +
//!   bridge channel + `transport::run_bridge_consumer` +
//!   `http_server::run_http_server`, constructed directly inside the test
//!   (mirroring `transport.rs`'s own `channel_transport_call_should_*`
//!   tests) — used wherever the test needs a handler the real binary's fixed
//!   tool set can't provide (a handler that hangs or panics on demand).
//!
//! `crates/cli` is a bin-only crate, so `mcp_router.rs`/`transport.rs`/
//! `http_server.rs` are pulled in via `#[path]`, exactly as
//! `tests/tool_schema.rs` already does.

use std::rc::Rc;
use std::time::Duration;

use serde_json::{json, Value};

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::daemon::{json_handler, Daemon};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::EnvPort;
use stapler_mcp_native::{NativeClock, NativeSleeper, NativeSocketFactory, NativeSpawner};

#[path = "../src/transport.rs"]
mod transport;
#[path = "../src/mcp_router.rs"]
mod mcp_router;
#[path = "../src/http_server.rs"]
mod http_server;

struct TestEnv {
    home: String,
}

impl EnvPort for TestEnv {
    fn var(&self, key: &str) -> Option<String> {
        if key == "STAPLER_MCP_HOME" {
            Some(self.home.clone())
        } else {
            None
        }
    }

    fn home_dir(&self) -> Option<String> {
        Some(self.home.clone())
    }
}

/// Binds an ephemeral port, reads it back, then drops the listener so the
/// daemon subprocess can bind it instead. Small TOCTOU race, same as every
/// other "grab a free port for a test subprocess" helper — acceptable here
/// since nothing else on this machine is racing to bind it.
fn free_tcp_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

/// Spawns a real `stapler-mcp --daemon` subprocess with the HTTP transport
/// enabled, in a fully isolated `STAPLER_MCP_HOME`. Returns the still-live
/// `TempDir` (must outlive the daemon), the port, and the bearer token read
/// back from the persisted `http-token` file.
async fn spawn_http_daemon() -> (tempfile::TempDir, TestEnv, u16, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_string_lossy().to_string();
    let port = free_tcp_port();

    std::env::set_var("STAPLER_MCP_HOME", &home);
    std::env::set_var("STAPLER_MCP_HTTP_PORT", port.to_string());
    let env = TestEnv { home: home.clone() };
    std::fs::create_dir_all(paths::base_dir(&env)).unwrap();

    let sock_path = paths::socket_path(&env);
    let log_path = paths::log_path(&env);
    let socket = NativeSocketFactory;
    let spawner = NativeSpawner;
    let sleeper = NativeSleeper;
    let clock = NativeClock;
    let exe = env!("CARGO_BIN_EXE_stapler-mcp").to_string();

    client::ensure_daemon(
        &socket,
        &spawner,
        &sleeper,
        &clock,
        &sock_path,
        &log_path,
        EnsureOptions {
            startup_timeout: Some(Duration::from_secs(60)),
            exe_hint: Some(exe),
        },
    )
    .await
    .expect("daemon should auto-start");

    // The HTTP listener binds concurrently with (not strictly before) the
    // Unix socket becoming reachable, so poll the token file briefly rather
    // than assuming it exists the instant `ensure_daemon` returns.
    let token_path = paths::http_token_path(&env);
    let token = poll_until_nonempty_file(&token_path, Duration::from_secs(5)).await;

    (tmp, env, port, token)
}

async fn poll_until_nonempty_file(path: &str, timeout: Duration) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            let trimmed = contents.trim().to_string();
            if !trimmed.is_empty() {
                return trimmed;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for {path} to become non-empty");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn shutdown_daemon(socket: &NativeSocketFactory, sock_path: &str) {
    let _ = client::call(socket, sock_path, "shutdown", None, Duration::from_secs(2)).await;
}

/// POSTs one MCP `tools/call` JSON-RPC request to `port`'s `/mcp` endpoint
/// and returns the parsed JSON-RPC response body.
async fn http_call_tool(port: u16, token: &str, tool: &str, arguments: Value) -> (reqwest::StatusCode, Value) {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://127.0.0.1:{port}/mcp"))
        .header("accept", "application/json, text/event-stream")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        }))
        .send()
        .await
        .expect("send tools/call request");
    let status = resp.status();
    let body: Value = resp.json().await.expect("parse JSON-RPC response body");
    (status, body)
}

/// Pulls a `CallToolResult`'s output value out of a JSON-RPC response body,
/// preferring `structuredContent` (what `Json<Out>`-returning tools produce)
/// and falling back to parsing the first `content` block's `text` as JSON.
fn tool_result_value(body: &Value) -> Value {
    let result = &body["result"];
    if !result["structuredContent"].is_null() {
        return result["structuredContent"].clone();
    }
    let text = result["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("expected structuredContent or content[0].text, got: {body:?}"));
    serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_string()))
}

fn is_tool_error(body: &Value) -> bool {
    body["result"]["isError"].as_bool().unwrap_or(false) || !body["error"].is_null()
}

// --- Task 8.1.1a: schema parity (no live daemon) ---------------------------

#[test]
fn socket_and_channel_transport_should_register_identical_tool_schemas() {
    let socket_tools = mcp_router::McpRouter::<transport::SocketTransport>::registered_tools();
    let channel_tools = mcp_router::McpRouter::<transport::ChannelTransport>::registered_tools();

    assert_eq!(
        socket_tools.len(),
        channel_tools.len(),
        "expected the same tool count over both transports"
    );
    assert_eq!(
        socket_tools.len(),
        27,
        "expected all 27 ThinClient-era tools to be registered"
    );

    for socket_tool in &socket_tools {
        let channel_tool = channel_tools
            .iter()
            .find(|t| t.name == socket_tool.name)
            .unwrap_or_else(|| panic!("tool `{}` missing from ChannelTransport router", socket_tool.name));
        assert_eq!(socket_tool.description, channel_tool.description);
        assert_eq!(
            socket_tool.input_schema, channel_tool.input_schema,
            "tool `{}`'s inputSchema differs between transports",
            socket_tool.name
        );
    }
}

// --- Task 8.1.1b: auth reject/accept against a real daemon ------------------

#[tokio::test]
async fn http_request_without_token_is_rejected_and_with_token_succeeds() {
    let (_tmp, env, port, token) = spawn_http_daemon().await;

    let (unauth_status, _) = http_call_tool(port, "wrong-token", "stapler_browser_list_sessions", json!({})).await;
    assert_eq!(unauth_status, reqwest::StatusCode::UNAUTHORIZED);

    let (auth_status, body) = http_call_tool(port, &token, "stapler_browser_list_sessions", json!({})).await;
    assert_eq!(auth_status, reqwest::StatusCode::OK, "got: {body:?}");
    assert!(!is_tool_error(&body), "expected a successful call, got: {body:?}");
    let value = tool_result_value(&body);
    assert!(value["sessions"].as_array().is_some(), "got: {body:?}");

    let socket = NativeSocketFactory;
    shutdown_daemon(&socket, &paths::socket_path(&env)).await;
}

// --- Task 8.1.1c: dual transport (stdio + HTTP) against one daemon ---------

#[tokio::test]
async fn stdio_and_http_calls_both_reach_the_same_running_daemon() {
    let (_tmp, env, port, token) = spawn_http_daemon().await;
    let socket = NativeSocketFactory;
    let sock_path = paths::socket_path(&env);

    // Open a session over the Unix-socket (stdio-equivalent) path.
    let stdio_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_navigate",
        Some(json!({ "url": "data:text/html,<html><body>stdio session</body></html>" })),
        Duration::from_secs(30),
    )
    .await
    .expect("stdio navigate should succeed");
    let stdio_session_id = stdio_result["sessionId"].as_str().expect("sessionId present").to_string();

    // The same daemon's session list is now visible over the HTTP path.
    let (status, body) = http_call_tool(port, &token, "stapler_browser_list_sessions", json!({})).await;
    assert_eq!(status, reqwest::StatusCode::OK, "got: {body:?}");
    let value = tool_result_value(&body);
    let sessions = value["sessions"].as_array().expect("sessions array");
    assert!(
        sessions.iter().any(|s| s["sessionId"].as_str() == Some(stdio_session_id.as_str())),
        "expected the stdio-opened session to be visible over HTTP, got: {value:?}"
    );

    // And a session opened over HTTP is visible back over the stdio path.
    let (nav_status, nav_body) = http_call_tool(
        port,
        &token,
        "stapler_browser_navigate",
        json!({ "url": "data:text/html,<html><body>http session</body></html>" }),
    )
    .await;
    assert_eq!(nav_status, reqwest::StatusCode::OK, "got: {nav_body:?}");
    let nav_value = tool_result_value(&nav_body);
    let http_session_id = nav_value["sessionId"].as_str().expect("sessionId present").to_string();

    let list_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_list_sessions",
        None,
        Duration::from_secs(10),
    )
    .await
    .expect("stdio list_sessions should succeed");
    let stdio_sessions = list_result["sessions"].as_array().expect("sessions array");
    assert!(
        stdio_sessions.iter().any(|s| s["sessionId"].as_str() == Some(http_session_id.as_str())),
        "expected the HTTP-opened session to be visible over stdio, got: {list_result:?}"
    );

    shutdown_daemon(&socket, &sock_path).await;
}

// --- Task 8.1.2a/b: concurrent calls + cross-request SessionId reuse -------

#[tokio::test]
async fn two_concurrent_http_navigate_calls_both_succeed_with_distinct_session_ids() {
    let (_tmp, env, port, token) = spawn_http_daemon().await;

    let (result_a, result_b) = tokio::join!(
        http_call_tool(
            port,
            &token,
            "stapler_browser_navigate",
            json!({ "url": "data:text/html,<html><body>session A</body></html>" }),
        ),
        http_call_tool(
            port,
            &token,
            "stapler_browser_navigate",
            json!({ "url": "data:text/html,<html><body>session B</body></html>" }),
        ),
    );

    let (status_a, body_a) = result_a;
    let (status_b, body_b) = result_b;
    assert_eq!(status_a, reqwest::StatusCode::OK, "got: {body_a:?}");
    assert_eq!(status_b, reqwest::StatusCode::OK, "got: {body_b:?}");
    let session_a = tool_result_value(&body_a)["sessionId"].as_str().expect("sessionId present").to_string();
    let session_b = tool_result_value(&body_b)["sessionId"].as_str().expect("sessionId present").to_string();
    assert_ne!(session_a, session_b, "each concurrent call should get its own session");

    let socket = NativeSocketFactory;
    shutdown_daemon(&socket, &paths::socket_path(&env)).await;
}

#[tokio::test]
async fn session_id_from_one_http_request_is_reusable_by_a_second_independent_http_request() {
    let (_tmp, env, port, token) = spawn_http_daemon().await;

    let (nav_status, nav_body) = http_call_tool(
        port,
        &token,
        "stapler_browser_navigate",
        json!({ "url": "data:text/html,<html><body><button id=\"go\">Go</button></body></html>" }),
    )
    .await;
    assert_eq!(nav_status, reqwest::StatusCode::OK, "got: {nav_body:?}");
    let nav_value = tool_result_value(&nav_body);
    let session_id = nav_value["sessionId"].as_str().expect("sessionId present").to_string();

    // A second, independent HTTP request (no shared connection state, since
    // `stateful_mode: false`) reuses that same sessionId successfully.
    let (snapshot_status, snapshot_body) = http_call_tool(
        port,
        &token,
        "stapler_browser_snapshot",
        json!({ "sessionId": session_id }),
    )
    .await;
    assert_eq!(snapshot_status, reqwest::StatusCode::OK, "got: {snapshot_body:?}");
    assert!(!is_tool_error(&snapshot_body), "got: {snapshot_body:?}");

    let socket = NativeSocketFactory;
    shutdown_daemon(&socket, &paths::socket_path(&env)).await;
}

// --- In-process harness, shared by Tasks 8.1.2c / 8.1.3a / 8.1.4a ----------

struct InProcessHarness {
    port: u16,
    token: String,
    daemon: Rc<Daemon>,
    bridge_tx: tokio::sync::mpsc::Sender<transport::BridgeMessage>,
    cancel: tokio_util::sync::CancellationToken,
}

impl InProcessHarness {
    async fn call(&self, tool: &'static str, params: Value) -> Result<Value, String> {
        use transport::DaemonTransport;
        transport::ChannelTransport::new(self.bridge_tx.clone())
            .call(tool, params)
            .await
    }

    fn shutdown(&self) {
        self.daemon.request_shutdown();
    }
}

/// Constructs a bare `Daemon` (with any extra test-only handlers already
/// registered by the caller) plus the exact same bridge channel / consumer /
/// HTTP server production code uses (`transport::run_bridge_consumer`,
/// `http_server::run_http_server`), all spawned on the current `LocalSet`.
/// Must be run from inside `local.run_until(...)` since `Daemon` is `!Send`.
async fn spawn_in_process_harness(daemon: Daemon) -> InProcessHarness {
    let daemon = Rc::new(daemon);
    let (bridge_tx, bridge_rx) =
        tokio::sync::mpsc::channel::<transport::BridgeMessage>(transport::BRIDGE_CHANNEL_CAPACITY);
    let cancel = daemon.cancellation_token();

    let consumer_daemon = daemon.clone();
    let consumer_cancel = cancel.clone();
    tokio::task::spawn_local(transport::run_bridge_consumer(consumer_daemon, bridge_rx, consumer_cancel));

    let port = free_tcp_port();
    let token = stapler_mcp_core::http_auth::BearerToken::new("test-harness-token".to_string());
    let server_bridge_tx = bridge_tx.clone();
    let server_token = token.clone();
    let server_cancel = cancel.clone();
    tokio::task::spawn(http_server::run_http_server(server_bridge_tx, port, server_token, server_cancel));

    // Give the listener a moment to actually bind before the first request.
    wait_for_port_open(port, Duration::from_secs(5)).await;

    InProcessHarness {
        port,
        token: token.as_str().to_string(),
        daemon,
        bridge_tx,
        cancel,
    }
}

async fn wait_for_port_open(port: u16, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for 127.0.0.1:{port} to accept connections");
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Registers a `"hang"` handler that never resolves and a `"boom"` handler
/// that panics, on top of the always-present `PING_TOOL`/`SHUTDOWN_TOOL`
/// built-ins — the minimal fixture Tasks 8.1.2c/8.1.3a/8.1.4a need, none of
/// which the real binary's fixed 27-tool set can provide on demand.
fn daemon_with_test_handlers() -> Daemon {
    let daemon = Daemon::new();
    #[derive(serde::Deserialize, serde::Serialize)]
    struct Empty {}
    daemon.register(
        "hang",
        json_handler(|_input: Empty| async {
            std::future::pending::<Result<Empty, String>>().await
        }),
    );
    daemon.register(
        "boom",
        json_handler(|_input: Empty| async move {
            panic!("intentional test panic");
            #[allow(unreachable_code)]
            Ok::<Empty, String>(Empty {})
        }),
    );
    daemon
}

// --- Task 8.1.2c: a hung call doesn't starve concurrent callers -----------

#[test]
fn hung_tool_call_does_not_block_unrelated_concurrent_http_callers() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("build runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let harness = spawn_in_process_harness(daemon_with_test_handlers()).await;

        let hang_call = tokio::task::spawn_local({
            let harness_call = InProcessHarness {
                port: harness.port,
                token: harness.token.clone(),
                daemon: harness.daemon.clone(),
                bridge_tx: harness.bridge_tx.clone(),
                cancel: harness.cancel.clone(),
            };
            async move { harness_call.call("hang", json!({})).await }
        });

        // Two unrelated fast calls must both complete promptly, not queue
        // behind the hung one.
        let fast_bound = Duration::from_secs(2);
        let (ping_a, ping_b) = tokio::join!(
            tokio::time::timeout(fast_bound, harness.call("ping", json!({}))),
            tokio::time::timeout(fast_bound, harness.call("ping", json!({}))),
        );
        assert_eq!(ping_a.expect("ping A should not be starved by the hung call"), Ok(json!({"pong": true})));
        assert_eq!(ping_b.expect("ping B should not be starved by the hung call"), Ok(json!({"pong": true})));

        // The hung call itself eventually resolves to the core-level request
        // timeout rather than hanging this test forever.
        let hang_result = tokio::time::timeout(Duration::from_secs(35), hang_call)
            .await
            .expect("hung call task should not hang past the request timeout")
            .expect("hung call task should not panic");
        let err = hang_result.expect_err("hung handler should time out, not succeed");
        assert!(err.contains("timed out"), "unexpected error: {err}");

        harness.shutdown();
    });
}

// --- Task 8.1.3a: panic recovery -------------------------------------------

#[test]
fn panicking_tool_handler_produces_clean_error_and_transport_keeps_serving() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("build runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let harness = spawn_in_process_harness(daemon_with_test_handlers()).await;

        let boom_result = tokio::time::timeout(Duration::from_secs(5), harness.call("boom", json!({})))
            .await
            .expect("panicking call should not hang");
        let err = boom_result.expect_err("a panicking handler must produce a clean error, not a hang");
        assert!(
            err.contains("panicked"),
            "expected the bridge consumer's panic-translation message, got: {err}"
        );

        // The bridge consumer (and the mpsc::Receiver it owns) must still be
        // alive: a second, unrelated call still succeeds.
        let ping_result = tokio::time::timeout(Duration::from_secs(5), harness.call("ping", json!({})))
            .await
            .expect("ping after a panic should not hang");
        assert_eq!(ping_result, Ok(json!({"pong": true})));

        harness.shutdown();
    });
}

// --- Task 8.1.4a: bridge channel burst beyond BRIDGE_CHANNEL_CAPACITY -----

#[test]
fn burst_beyond_bridge_channel_capacity_resolves_every_request() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("build runtime");
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async {
        let harness = spawn_in_process_harness(daemon_with_test_handlers()).await;

        const TOTAL_CALLS: usize = transport::BRIDGE_CHANNEL_CAPACITY + 36;
        let calls = (0..TOTAL_CALLS).map(|_| {
            let harness_call = InProcessHarness {
                port: harness.port,
                token: harness.token.clone(),
                daemon: harness.daemon.clone(),
                bridge_tx: harness.bridge_tx.clone(),
                cancel: harness.cancel.clone(),
            };
            async move {
                tokio::time::timeout(Duration::from_secs(35), harness_call.call("ping", json!({}))).await
            }
        });

        let results = futures::future::join_all(calls).await;
        for (i, result) in results.into_iter().enumerate() {
            let call_result = result.unwrap_or_else(|_| panic!("call #{i} hung past its bound"));
            assert!(
                call_result == Ok(json!({"pong": true})) || call_result.is_err_and(|e| e.contains("timed out")),
                "call #{i}: expected a normal reply or a clean timeout"
            );
        }

        harness.shutdown();
    });
}

// --- Task 8.3.1a: SIGTERM mid-request ---------------------------------------

#[tokio::test]
async fn sigterm_mid_request_lets_the_in_flight_request_resolve_and_the_daemon_exit_cleanly() {
    let (_tmp, env, port, token) = spawn_http_daemon().await;
    let lock_path = paths::lock_path(&env);
    let pid: u32 = std::fs::read_to_string(&lock_path)
        .expect("read daemon lock file")
        .trim()
        .parse()
        .expect("parse daemon pid");

    // Fire a real tool call in the background, then send SIGTERM almost
    // immediately after — before it necessarily completes.
    let call_task = tokio::spawn(async move {
        http_call_tool(
            port,
            &token,
            "stapler_browser_navigate",
            json!({ "url": "data:text/html,<html><body>sigterm race</body></html>" }),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(20)).await;

    let status = std::process::Command::new("kill")
        .arg("-TERM")
        .arg(pid.to_string())
        .status()
        .expect("send SIGTERM");
    assert!(status.success(), "kill -TERM should succeed");

    // The in-flight request must resolve one way or the other — never hang
    // past a generous bound — whether it completes before shutdown wins the
    // race or the connection is cut cleanly by the graceful-shutdown path.
    let _ = tokio::time::timeout(Duration::from_secs(10), call_task).await;

    // The daemon process must actually exit within its shutdown grace period
    // plus a margin, not linger.
    let exited = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            // Signal 0 probes for existence without actually signaling.
            let alive = std::process::Command::new("kill")
                .arg("-0")
                .arg(pid.to_string())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !alive {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    let log_debug = std::fs::read_to_string(paths::log_path(&env)).unwrap_or_default();
    eprintln!("DEBUG log contents:\n{log_debug}");
    eprintln!("DEBUG kill -0 alive check after wait: {:?}", std::process::Command::new("kill").arg("-0").arg(pid.to_string()).status());
    assert!(exited.is_ok(), "daemon process (pid {pid}) did not exit within the grace period");

    let log = std::fs::read_to_string(paths::log_path(&env)).unwrap_or_default();
    let sigterm_lines = log.lines().filter(|l| l.contains("received SIGTERM")).count();
    assert_eq!(sigterm_lines, 1, "expected exactly one SIGTERM shutdown sequence, got log:\n{log}");
}
