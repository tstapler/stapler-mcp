//! Regression for backlog 7accd0b9 / issue #55: "ensure daemon: Connection
//! refused". Real daemon binary, isolated `STAPLER_MCP_HOME`.
//!
//! Single `#[tokio::test]`: `std::env::set_var` is process-global.

use std::time::Duration;

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::EnvPort;
use stapler_mcp_native::{NativeClock, NativeSleeper, NativeSocketFactory, NativeSpawner};
use tokio::io::AsyncWriteExt;

struct TestEnv {
    home: String,
}

impl EnvPort for TestEnv {
    fn var(&self, key: &str) -> Option<String> {
        (key == "STAPLER_MCP_HOME").then(|| self.home.clone())
    }

    fn home_dir(&self) -> Option<String> {
        Some(self.home.clone())
    }
}

#[tokio::test]
async fn ensure_daemon_should_survive_parallel_cold_start_sigkill_and_client_hangups() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_string_lossy().to_string();
    // Env is snapshotted at spawn time, so set it before any daemon starts.
    std::env::set_var("STAPLER_MCP_HOME", &home);
    let env = TestEnv { home };
    std::fs::create_dir_all(paths::base_dir(&env)).unwrap();

    let sock_path = paths::socket_path(&env);
    let lock_path = paths::lock_path(&env);
    let log_path = paths::log_path(&env);
    let exe = env!("CARGO_BIN_EXE_stapler-mcp-daemon").to_string();
    let opts = || EnsureOptions {
        startup_timeout: Some(Duration::from_secs(60)),
        exe_hint: Some(exe.clone()),
    };
    let ensure = || async {
        client::ensure_daemon(
            &NativeSocketFactory,
            &NativeSpawner,
            &NativeSleeper,
            &NativeClock,
            &sock_path,
            &log_path,
            opts(),
        )
        .await
    };
    let ping = || client::ping(&NativeSocketFactory, &sock_path, Duration::from_secs(2));

    // AC1: 6 parallel callers against a cold start all succeed.
    let results = futures::future::join_all((0..6).map(|_| ensure())).await;
    for (i, r) in results.iter().enumerate() {
        assert!(r.is_ok(), "parallel caller {i} failed: {r:?}");
    }
    ping().await.expect("daemon reachable after cold start");

    // Root cause of #55: clients hanging up mid-response used to end the
    // daemon's accept loop while the process (and lock) lived on.
    for _ in 0..50 {
        let mut s = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
        s.write_all(b"{\"tool\":\"ping\"}\n").await.unwrap();
        drop(s);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    ping().await.expect("daemon must keep serving after client hang-ups");

    // AC0: SIGKILL the daemon, then the next caller respawns it.
    let pid = std::fs::read_to_string(&lock_path).unwrap().trim().to_string();
    assert!(std::process::Command::new("kill")
        .args(["-9", &pid])
        .status()
        .unwrap()
        .success());
    for _ in 0..50 {
        if ping().await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ping().await.is_err(), "daemon should be down after SIGKILL");
    ensure().await.expect("ensure_daemon should respawn a killed daemon");
    ping().await.expect("respawned daemon reachable");

    let _ = client::call(
        &NativeSocketFactory,
        &sock_path,
        "shutdown",
        None,
        Duration::from_secs(5),
    )
    .await;
}
