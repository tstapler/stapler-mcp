//! Pre-mortem P1: while the (serial) daemon is busy with a slow request,
//! parallel callers' `ensure_daemon` pings time out; that must count as
//! "alive" so each caller proceeds and its real call is served afterwards.

mod common;

use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use stapler_mcp_core::client;
use stapler_mcp_native::NativeSocketFactory;

const SLOW_MS: u64 = 5_000;
const N: usize = 8;

/// Mock Brave API that holds each response for `SLOW_MS`.
async fn spawn_slow_mock() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf).await;
                tokio::time::sleep(Duration::from_millis(SLOW_MS)).await;
                let body = r#"{"web":{"results":[]}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://{addr}/res/v1/web/search")
}

#[tokio::test]
async fn callers_should_all_succeed_while_the_daemon_is_busy_with_a_slow_request() {
    let base_url = spawn_slow_mock().await;
    let h = common::Harness::new();
    std::env::set_var("BRAVE_API_KEY", "test-key");
    std::env::set_var("BRAVE_API_BASE_URL", &base_url);
    h.ensure(Duration::from_secs(45))
        .await
        .expect("daemon start");
    let pid = h.daemon_pid();

    let sock = h.sock_path.clone();
    let slow = tokio::spawn(async move {
        client::call(
            &NativeSocketFactory,
            &sock,
            "brave_web_search",
            Some(json!({"query": "slow"})),
            Duration::from_secs(60),
        )
        .await
        .map_err(|e| e.to_string())
    });
    // Let the daemon pick up the slow request before the fan-out arrives.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut callers = tokio::task::JoinSet::new();
    for _ in 0..N {
        let (sock, log) = (h.sock_path.clone(), h.log_path.clone());
        callers.spawn(async move {
            common::ensure(&sock, &log, Duration::from_secs(45))
                .await
                .map_err(|e| e.to_string())?;
            client::ping(&NativeSocketFactory, &sock, Duration::from_secs(60))
                .await
                .map_err(|e| e.to_string())
        });
    }
    while let Some(joined) = callers.join_next().await {
        let result = joined.expect("task panicked");
        assert!(
            result.is_ok(),
            "caller failed while daemon was busy: {result:?}"
        );
    }
    assert!(
        slow.await.expect("slow task").is_ok(),
        "the slow request itself failed"
    );
    assert_eq!(
        h.daemon_pid(),
        pid,
        "no replacement daemon should have started"
    );
}
