//! A SIGKILLed Chrome child must not wedge the daemon: `fetch_page` relaunches
//! the browser, `stapler_daemon_status`/`ping` reports `browserAlive`, and a
//! failed relaunch yields a fast "browser unavailable" error.
//!
//! Needs a real Chrome, so `#[ignore]`d; run with:
//!   cargo test -p stapler-mcp-daemon --test browser_child_kill_respawn -- --ignored --test-threads=1

mod common;

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use stapler_mcp_core::client;
use stapler_mcp_native::NativeSocketFactory;

use common::Harness;

async fn spawn_mock_site() -> String {
    let body =
        "<html><head><title>Kill Respawn Fixture</title></head><body><p>hello</p></body></html>";
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = sock.read(&mut buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/")
}

fn chrome_child_pid(daemon_pid: i32) -> i32 {
    let out = std::process::Command::new("pgrep")
        .args(["-P", &daemon_pid.to_string()])
        .output()
        .expect("pgrep");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse().ok())
        .expect("daemon should have a child process (Chrome)")
}

async fn call(
    h: &Harness,
    tool: &str,
    params: Option<Value>,
    t: Duration,
) -> Result<Value, String> {
    client::call(&NativeSocketFactory, &h.sock_path, tool, params, t)
        .await
        .map_err(|e| e.to_string())
}

async fn start() -> (Harness, String) {
    let h = Harness::new();
    std::env::set_var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS", "1");
    // STAPLER_TEST_URL=https://example.com/ runs the same sequence against a live site.
    let site = match std::env::var("STAPLER_TEST_URL") {
        Ok(u) => {
            std::env::remove_var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS");
            u
        }
        Err(_) => spawn_mock_site().await,
    };
    h.ensure(Duration::from_secs(20))
        .await
        .expect("daemon start");
    (h, site)
}

fn kill_chrome_and_wait(h: &Harness) {
    let chrome = chrome_child_pid(h.daemon_pid());
    assert!(common::signal(chrome, libc::SIGKILL));
    // Zombie counts as dead; the daemon has not necessarily reaped it yet.
    assert!(common::wait_for_exit(chrome, Duration::from_secs(5)));
}

#[tokio::test]
#[ignore = "needs a real Chrome"]
async fn fetch_page_should_relaunch_browser_after_chrome_child_killed() {
    let (h, site) = start().await;
    kill_chrome_and_wait(&h);

    let started = Instant::now();
    let out = call(
        &h,
        "fetch_page",
        Some(json!({"url": site})),
        Duration::from_secs(30),
    )
    .await
    .expect("fetch_page should succeed via relaunch");
    println!(
        "RESULT title={:?} botChallenge={:?} elapsed={:?}",
        out["title"],
        out["botChallenge"],
        started.elapsed()
    );
    assert!(!out["title"].as_str().unwrap_or("").is_empty(), "{out}");
    assert!(
        started.elapsed() < Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
#[ignore = "needs a real Chrome"]
async fn status_should_report_browser_alive_false_after_kill_and_true_after_relaunch() {
    let (h, site) = start().await;
    let up = call(&h, "ping", None, Duration::from_secs(5))
        .await
        .unwrap();
    println!("RESULT before kill: {up}");
    assert_eq!(up["browserAlive"], true, "{up}");

    kill_chrome_and_wait(&h);
    let down = call(&h, "ping", None, Duration::from_secs(5))
        .await
        .unwrap();
    println!("RESULT after kill, before relaunch: {down}");
    assert_eq!(down["browserAlive"], false, "{down}");

    call(
        &h,
        "fetch_page",
        Some(json!({"url": site})),
        Duration::from_secs(30),
    )
    .await
    .expect("relaunch");
    let again = call(&h, "ping", None, Duration::from_secs(5))
        .await
        .unwrap();
    println!("RESULT after fetch_page relaunch: {again}");
    assert_eq!(again["browserAlive"], true, "{again}");
}

#[tokio::test]
#[ignore = "needs a real Chrome"]
async fn fetch_page_should_fail_fast_with_browser_unavailable_when_relaunch_fails() {
    let (h, site) = start().await;
    let chrome = chrome_child_pid(h.daemon_pid());
    let cmdline = std::fs::read_to_string(format!("/proc/{chrome}/cmdline")).unwrap();
    let dir = cmdline
        .split(['\0', ' '])
        .find_map(|a| a.strip_prefix("--user-data-dir="))
        .unwrap_or_else(|| panic!("user-data-dir arg in {cmdline:?}"))
        .to_string();
    kill_chrome_and_wait(&h);
    // A regular file where the profile dir was makes the relaunch fail at once.
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::write(&dir, b"not a dir").unwrap();

    let started = Instant::now();
    let err = call(
        &h,
        "fetch_page",
        Some(json!({"url": site})),
        Duration::from_secs(30),
    )
    .await
    .expect_err("relaunch cannot succeed");
    let _ = std::fs::remove_file(&dir);
    println!("RESULT err={err:?} elapsed={:?}", started.elapsed());
    assert!(err.contains("browser unavailable"), "{err}");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
