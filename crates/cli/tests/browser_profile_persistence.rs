//! Real-Chrome integration tests for `STAPLER_MCP_BROWSER_PROFILE_DIR`
//! (`project_plans/browser-profile-persistence/implementation/plan.md`
//! Epic 1.3, Stories 1.3.1 and 1.3.2), de-risking the two acceptance
//! criteria unit tests alone can't cover: whether a cookie set before a
//! daemon restart is actually still readable after one when persistence is
//! opted into (chromiumoxide's `user_data_dir` behavior across restarts is
//! otherwise only an unconfirmed upstream report — `research/pitfalls.md`
//! §5, issue #252), and whether a real `SingletonLock` collision produces
//! the actionable error `describe_launch_error` promises (that function is
//! otherwise only unit-tested against a synthetic error string, not a real
//! chromiumoxide failure).
//!
//! Requires a real Chrome/Chromium binary, same as `browser_session.rs`, so
//! this is `#[ignore]`d out of the default run. Run explicitly with:
//!   cargo test -p stapler-mcp --test browser_profile_persistence -- --ignored --test-threads=1

use std::collections::HashMap;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::EnvPort;
use stapler_mcp_native::{
    NativeBrowser, NativeClock, NativeSleeper, NativeSocketFactory, NativeSpawner,
};

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

/// Minimal single-page mock site, copied from `browser_session.rs`'s
/// `spawn_mock_site` (only the routing/serving scaffolding is needed here —
/// this file's tests only care about `document.cookie`, not page content).
async fn spawn_mock_site() -> (String, tokio::sync::oneshot::Sender<()>) {
    let page =
        "<html><head><title>Browser Profile Persistence Fixture</title></head><body></body></html>";
    let routes: HashMap<&str, String> = HashMap::from([("/", page.to_string())]);

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock site");
    let addr = listener.local_addr().expect("mock site addr");
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();

    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => return,
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { return };
                    let routes = routes.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 4096];
                        let n = stream.read(&mut buf).await.unwrap_or(0);
                        let request = String::from_utf8_lossy(&buf[..n]);
                        let path = request
                            .lines()
                            .next()
                            .and_then(|line| line.split_whitespace().nth(1))
                            .unwrap_or("/")
                            .to_string();
                        let body = routes.get(path.as_str()).cloned();
                        let resp = match body {
                            Some(b) => format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                b.len(),
                                b
                            ),
                            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                        };
                        let _ = stream.write_all(resp.as_bytes()).await;
                        let _ = stream.shutdown().await;
                    });
                }
            }
        }
    });

    (format!("http://{addr}"), shutdown_tx)
}

/// Story 1.3.1: a cookie set before the daemon is stopped is still readable
/// after the daemon is restarted against the same
/// `STAPLER_MCP_BROWSER_PROFILE_DIR`.
#[tokio::test]
#[ignore]
async fn cookie_set_before_restart_survives_daemon_restart_with_persistent_profile() {
    let home_tmp = tempfile::tempdir().expect("home tempdir");
    let home = home_tmp.path().to_string_lossy().to_string();
    // Kept separate from `home_tmp` and held for the whole test: the
    // profile dir must outlive the simulated restart, while `STAPLER_MCP_HOME`
    // is reused across both daemon starts here purely for simplicity (this
    // item doesn't require persistence to survive a `STAPLER_MCP_HOME`
    // change too).
    let profile_tmp = tempfile::tempdir().expect("profile tempdir");
    let profile_dir = profile_tmp.path().to_string_lossy().to_string();

    std::env::set_var("STAPLER_MCP_HOME", &home);
    // The mock site below binds a real 127.0.0.1 listener; opt the daemon
    // subprocess out of the SSRF guard so it can reach it — same rationale
    // as `browser_session.rs`.
    std::env::set_var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS", "1");
    std::env::set_var("STAPLER_MCP_BROWSER_PROFILE_DIR", &profile_dir);

    let env = TestEnv { home };
    std::fs::create_dir_all(paths::base_dir(&env)).unwrap();

    let (site_url, shutdown_site) = spawn_mock_site().await;

    let sock_path = paths::socket_path(&env);
    let log_path = paths::log_path(&env);
    let socket = NativeSocketFactory;
    let spawner = NativeSpawner;
    let sleeper = NativeSleeper;
    let clock = NativeClock;
    let exe = env!("CARGO_BIN_EXE_stapler-mcp").to_string();
    let ensure_options = || EnsureOptions {
        startup_timeout: Some(Duration::from_secs(60)),
        exe_hint: Some(exe.clone()),
    };

    client::ensure_daemon(
        &socket,
        &spawner,
        &sleeper,
        &clock,
        &sock_path,
        &log_path,
        ensure_options(),
    )
    .await
    .expect("daemon should auto-start with a persistent profile dir");

    let navigate_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_navigate",
        Some(json!({ "url": site_url })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_navigate should succeed");
    let session_id = navigate_result["sessionId"]
        .as_str()
        .expect("sessionId present")
        .to_string();

    client::call(
        &socket,
        &sock_path,
        "stapler_browser_evaluate",
        Some(json!({
            "sessionId": session_id,
            // `max-age` is required: a cookie with no expiry is a *session*
            // cookie, which Chrome discards on process exit regardless of
            // `user_data_dir` persistence — that would make this test pass
            // or fail on browser-session semantics, not on whether the
            // profile directory itself survived the daemon restart.
            "function": "() => { document.cookie = 'sticky=yes; path=/; max-age=3600'; return document.cookie; }",
        })),
        Duration::from_secs(10),
    )
    .await
    .expect("setting the cookie should succeed");

    client::call(
        &socket,
        &sock_path,
        "shutdown",
        None,
        Duration::from_secs(2),
    )
    .await
    .expect("shutdown call should succeed");
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        client::ping(&socket, &sock_path, Duration::from_millis(500))
            .await
            .is_err(),
        "daemon should no longer be reachable after shutdown"
    );

    // Restart against the same `STAPLER_MCP_BROWSER_PROFILE_DIR` (still set
    // in this process's env, inherited by the freshly spawned subprocess).
    client::ensure_daemon(
        &socket,
        &spawner,
        &sleeper,
        &clock,
        &sock_path,
        &log_path,
        ensure_options(),
    )
    .await
    .expect("daemon should auto-restart against the same persistent profile dir");

    let navigate_result_2 = client::call(
        &socket,
        &sock_path,
        "stapler_browser_navigate",
        Some(json!({ "url": site_url })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_navigate should succeed after restart");
    let session_id_2 = navigate_result_2["sessionId"]
        .as_str()
        .expect("sessionId present")
        .to_string();

    let cookie_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_evaluate",
        Some(json!({
            "sessionId": session_id_2,
            "function": "() => document.cookie",
        })),
        Duration::from_secs(10),
    )
    .await
    .expect("reading the cookie after restart should succeed");
    let cookie = cookie_result["result"].as_str().unwrap_or("");
    assert!(
        cookie.contains("sticky=yes"),
        "expected the cookie set before restart to survive, got: {cookie_result:?}"
    );

    let _ = shutdown_site.send(());
    let _ = client::call(
        &socket,
        &sock_path,
        "shutdown",
        None,
        Duration::from_secs(2),
    )
    .await;
}

/// Story 1.3.2: a second `NativeBrowser::launch(Some(dir))` call against the
/// same real, existing `dir`, made while a first `launch()` against that
/// `dir` is still held open (not a race between two simultaneous launches —
/// the first is fully awaited before the second starts, mirroring how a
/// real second daemon would find the lock already held) — the second
/// returns an `Err` whose message names both the profile dir and
/// `SingletonLock`, per `describe_launch_error`.
#[tokio::test]
#[ignore]
async fn second_launch_against_held_profile_dir_reports_singleton_lock_collision() {
    // `NativeBrowser` spawns a `!Send` idle-session reaper via
    // `tokio::task::spawn_local` at `launch()` time, so this test needs its
    // own `LocalSet`, same as `browser_session.rs`'s
    // `navigate_concurrent_should_not_exceed_max_open_sessions`.
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let dir = tempfile::tempdir().expect("profile tempdir");

            let first = NativeBrowser::launch(Some(dir.path().to_path_buf()))
                .await
                .expect("first launch against a fresh profile dir should succeed");

            let second = NativeBrowser::launch(Some(dir.path().to_path_buf())).await;
            let err = match second {
                Ok(_) => {
                    panic!("second launch against the still-held profile dir should fail")
                }
                Err(e) => e.to_string(),
            };
            assert!(
                err.contains("SingletonLock"),
                "expected the collision error to name SingletonLock, got: {err}"
            );
            assert!(
                err.contains("appears to be in use"),
                "expected the collision error to explain the likely cause, got: {err}"
            );
            assert!(
                err.contains(&dir.path().display().to_string()),
                "expected the collision error to name the profile dir, got: {err}"
            );

            drop(first);
        })
        .await;
}
