//! Real-Chrome check that a login wall surfaces as `PortError::AuthRequired`
//! (never empty content). `#[ignore]`d: needs a Chrome/Chromium binary, like
//! `browser_session.rs`. Run with:
//!   cargo test -p stapler-mcp --test sso_detection -- --ignored --test-threads=1
//! The headed login flow itself needs a display and a human; it is manual-only.

use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use stapler_mcp_core::ports::{BrowserDriver, PortError};
use stapler_mcp_native::NativeBrowser;

/// `/` 302s to `/login`, which is a thin page with a password field.
async fn spawn_login_wall() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 2048];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let resp = if req.starts_with("GET /login") {
                    let body = r#"<html><head><title>Sign in</title></head><body><input type="password"></body></html>"#;
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 302 Found\r\nLocation: /login\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://127.0.0.1:{}", addr.port())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "needs a real Chrome/Chromium binary"]
async fn navigate_and_extract_should_return_auth_required_for_login_wall() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let base = spawn_login_wall().await;
            let mut browser = NativeBrowser::launch(None).await.expect("launch");
            let url = format!("{base}/docs");
            let err = browser
                .navigate_and_extract(&url, Duration::from_secs(30))
                .await
                .err()
                .expect("login wall must not return content");
            match err {
                PortError::AuthRequired(info) => {
                    assert_eq!(info.requested_url, url);
                    assert!(info.final_url.ends_with("/login"), "{}", info.final_url);
                    assert_eq!(info.auth_host, "127.0.0.1");
                    assert!(info.hint.is_some());
                }
                other => panic!("expected AuthRequired, got {other}"),
            }
            browser.close().await;
        })
        .await;
}

/// `/docs` needs the `sid=1` cookie (else 302 -> `/signin`, a password page);
/// `/login` sets that cookie, standing in for a completed SSO login.
async fn spawn_cookie_gated_site() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let html = |body: &str, extra: &str| {
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                };
                let resp = if req.starts_with("GET /signin") {
                    html(
                        r#"<html><head><title>Sign in</title></head><body><input type="password"></body></html>"#,
                        "",
                    )
                } else if req.starts_with("GET /login") {
                    html(
                        "<html><head><title>ok</title></head><body>logged in</body></html>",
                        "Set-Cookie: sid=1; Path=/\r\n",
                    )
                } else if req.contains("sid=1") {
                    let body = format!(
                        "<html><head><title>Docs</title></head><body><p>{}</p></body></html>",
                        "private documentation ".repeat(60)
                    );
                    html(&body, "")
                } else {
                    "HTTP/1.1 302 Found\r\nLocation: /signin\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                };
                let _ = sock.write_all(resp.as_bytes()).await;
            });
        }
    });
    format!("http://127.0.0.1:{}", addr.port())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "needs a real Chrome/Chromium binary"]
async fn later_fetch_should_succeed_from_same_profile_after_login_without_new_auth_wall() {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async {
            let base = spawn_cookie_gated_site().await;
            let profile = std::env::temp_dir().join(format!("sso-reuse-{}", std::process::id()));
            let mut browser = NativeBrowser::launch(Some(profile.clone()))
                .await
                .expect("launch");
            let t = Duration::from_secs(30);
            let docs = format!("{base}/docs");

            let first = browser.navigate_and_extract(&docs, t).await;
            assert!(
                matches!(first, Err(PortError::AuthRequired(_))),
                "pre-login must be gated"
            );

            browser
                .navigate_and_extract(&format!("{base}/login"), t)
                .await
                .expect("login page");

            let after = browser
                .navigate_and_extract(&docs, t)
                .await
                .expect("post-login fetch");
            assert!(
                after.text.contains("private documentation"),
                "{}",
                after.text
            );
            browser.close().await;
            let _ = std::fs::remove_dir_all(profile);
        })
        .await;
}
