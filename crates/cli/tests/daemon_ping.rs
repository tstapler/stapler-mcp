//! The one real check for the daemon architecture (mirrors the Go
//! integration test): builds the actual binary, drives it end-to-end over the
//! real socket in a fully isolated state dir. Also exercises the two real
//! tools ported in Phase 1b.
//!
//! Deliberately a single `#[tokio::test]` function: `std::env::set_var` is
//! process-global, and `cargo test` runs tests in separate threads of the
//! same process by default — splitting this into multiple env-var-mutating
//! tests would race.

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::EnvPort;
use stapler_mcp_native::{NativeClock, NativeSleeper, NativeSocketFactory, NativeSpawner};

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

/// Hand-rolled single-endpoint HTTP mock (no request parsing needed — every
/// connection gets the same canned JSON body) so this test doesn't need a
/// mock-server crate for one fixed response.
async fn spawn_mock_brave_server(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock server");
    let addr = listener.local_addr().expect("mock server addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    format!("http://{addr}/res/v1/web/search")
}

#[tokio::test]
async fn daemon_architecture_and_tools_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_string_lossy().to_string();

    let mock_base_url = spawn_mock_brave_server(
        r#"{"web":{"results":[{"title":"Rust Programming Language","url":"https://www.rust-lang.org/","description":"A language empowering everyone."}]}}"#,
    )
    .await;

    // The spawned daemon subprocess reads the *real* process environment via
    // its own `NativeEnv`, at spawn time — `TestEnv` below only fakes
    // `EnvPort` for this test's own client-side path computations. All three
    // overrides must be set for real, and *before* the daemon is spawned
    // (env is a one-time snapshot at fork/exec, not a live view).
    std::env::set_var("STAPLER_MCP_HOME", &home);
    std::env::set_var("BRAVE_API_KEY", "test-key");
    std::env::set_var("BRAVE_API_BASE_URL", &mock_base_url);
    let env = TestEnv { home };
    std::fs::create_dir_all(paths::base_dir(&env)).unwrap();

    let sock_path = paths::socket_path(&env);
    let lock_path = paths::lock_path(&env);
    let log_path = paths::log_path(&env);

    let socket = NativeSocketFactory;
    let spawner = NativeSpawner;
    let sleeper = NativeSleeper;
    let clock = NativeClock;
    let exe = env!("CARGO_BIN_EXE_stapler-mcp").to_string();

    // 1. Pre-daemon ping fails.
    assert!(client::ping(&socket, &sock_path, Duration::from_millis(500))
        .await
        .is_err());

    // 2. ensure_daemon auto-spawns and becomes reachable within a bounded timeout.
    //    Generous timeout: this daemon also launches a real headless browser.
    let opts = EnsureOptions {
        startup_timeout: Some(Duration::from_secs(30)),
        exe_hint: Some(exe.clone()),
    };
    client::ensure_daemon(&socket, &spawner, &sleeper, &clock, &sock_path, &log_path, opts)
        .await
        .expect("daemon should auto-start");

    // 3. Real ping round trip.
    client::ping(&socket, &sock_path, Duration::from_secs(2))
        .await
        .expect("ping should succeed against running daemon");

    // 4. A second ensure_daemon call reuses the already-running daemon rather
    //    than spawning a redundant one — checked via the PID recorded in the
    //    lockfile staying unchanged.
    let pid_before = std::fs::read_to_string(&lock_path).expect("lockfile should exist");
    let opts2 = EnsureOptions {
        startup_timeout: Some(Duration::from_secs(5)),
        exe_hint: Some(exe.clone()),
    };
    client::ensure_daemon(&socket, &spawner, &sleeper, &clock, &sock_path, &log_path, opts2)
        .await
        .expect("second ensure_daemon should succeed against the same daemon");
    let pid_after = std::fs::read_to_string(&lock_path).expect("lockfile should still exist");
    assert_eq!(pid_before, pid_after, "no second daemon should have been spawned");

    // 5. `brave_web_search` against the mock server: header/query plumbing
    //    and response reduction all work over the real socket.
    let search_result = client::call(
        &socket,
        &sock_path,
        "brave_web_search",
        Some(json!({"query": "rust programming language"})),
        Duration::from_secs(10),
    )
    .await
    .expect("brave_web_search should succeed against the mock server");
    assert_eq!(
        search_result["results"][0]["title"],
        "Rust Programming Language"
    );

    // 6. `fetch_page` against a real URL — the one place this test suite
    //    depends on outbound network access, mirroring the original Go
    //    README's manual verification step.
    let fetch_result = client::call(
        &socket,
        &sock_path,
        "fetch_page",
        Some(json!({"url": "https://example.com"})),
        Duration::from_secs(30),
    )
    .await
    .expect("fetch_page should succeed against a real URL");
    assert_eq!(fetch_result["title"], "Example Domain");
    assert_eq!(fetch_result["finalUrl"], "https://example.com/");

    // 7. `shutdown` cleanly stops the daemon.
    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        client::ping(&socket, &sock_path, Duration::from_millis(500))
            .await
            .is_err(),
        "daemon should no longer be reachable after shutdown"
    );
}
