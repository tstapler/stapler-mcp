//! Exercises the merged webcrawl tool end-to-end over the real daemon/socket:
//! BFS depth-limiting, `robots.txt` respecting, and — the actual point of
//! caching — that a cache hit skips the network fetch entirely (proven by
//! shutting the mock server down between calls).

use std::collections::HashMap;
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

const FILLER: &str = "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor \
incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation \
ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in \
voluptate velit esse cillum dolore eu fugiat nulla pariatur.";

/// Hand-rolled multi-route HTTP mock — parses just the request line's path,
/// same pattern as the daemon_ping test's single-route mock, extended to
/// route by path so this can serve a small synthetic multi-page site plus a
/// `robots.txt`. Returns a shutdown handle (dropping the listener) so a test
/// can prove a later call used the cache, not the network.
async fn spawn_mock_site() -> (String, tokio::sync::oneshot::Sender<()>) {
    let routes: HashMap<&str, String> = HashMap::from([
        (
            "/robots.txt",
            "User-agent: *\nDisallow: /private\n".to_string(),
        ),
        (
            "/",
            format!(
                "<html><head><title>Index</title></head><body><p>{FILLER}</p><a href=\"/page2\">Page 2</a> <a href=\"/private\">Private</a></body></html>"
            ),
        ),
        (
            "/page2",
            format!(
                "<html><head><title>Page Two</title></head><body><p>{FILLER}</p><a href=\"/page3\">Page 3</a></body></html>"
            ),
        ),
        (
            "/page3",
            format!("<html><head><title>Page Three</title></head><body><p>{FILLER}</p></body></html>"),
        ),
        (
            "/private",
            format!("<html><head><title>Private</title></head><body><p>{FILLER}</p></body></html>"),
        ),
    ]);

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock site");
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

#[tokio::test]
async fn webcrawl_respects_robots_and_caches() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_string_lossy().to_string();
    std::env::set_var("STAPLER_MCP_HOME", &home);
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

    client::ensure_daemon(
        &socket,
        &spawner,
        &sleeper,
        &clock,
        &sock_path,
        &log_path,
        EnsureOptions {
            startup_timeout: Some(Duration::from_secs(15)),
            exe_hint: Some(exe),
        },
    )
    .await
    .expect("daemon should auto-start");

    // 1. BFS depth-2 crawl discovers index -> page2 -> page3, but never
    //    /private (robots.txt-disallowed).
    let result = client::call(
        &socket,
        &sock_path,
        "read_website",
        Some(json!({"url": site_url, "maxDepth": 2, "maxPages": 10})),
        Duration::from_secs(10),
    )
    .await
    .expect("read_website should succeed");

    let pages = result["pages"].as_array().expect("pages array");
    assert_eq!(pages.len(), 3, "expected index+page2+page3, got: {pages:?}");
    let titles: Vec<&str> = pages.iter().map(|p| p["title"].as_str().unwrap()).collect();
    assert!(titles.contains(&"Index"));
    assert!(titles.contains(&"Page Two"));
    assert!(titles.contains(&"Page Three"));
    assert!(
        !titles.contains(&"Private"),
        "robots.txt should have disallowed /private, got titles: {titles:?}"
    );

    // 2. `download_website` shares the same crawler — same depth/robots
    //    behavior, raw HTML saved to disk instead of extracted Markdown.
    let save_dir = tmp.path().join("downloaded");
    let download_result = client::call(
        &socket,
        &sock_path,
        "download_website",
        Some(json!({
            "url": site_url,
            "saveDir": save_dir.to_string_lossy(),
            "maxDepth": 2,
            "maxPages": 10,
        })),
        Duration::from_secs(10),
    )
    .await
    .expect("download_website should succeed");
    let downloaded = download_result["pages"].as_array().expect("pages array");
    assert_eq!(downloaded.len(), 3, "expected index+page2+page3, got: {downloaded:?}");
    for page in downloaded {
        let path = page["path"].as_str().expect("path");
        let contents = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        assert!(contents.contains("<html>"), "saved file should contain raw HTML: {path}");
    }

    // 3. Shut the mock site down, then call `read_website` again for just the
    //    seed URL. If this succeeds and returns the same cached page instead
    //    of erroring, the daemon used its cache rather than the network —
    //    the actual point of caching. (Only the seed page is cached-and-thus-
    //    returned: a cache hit deliberately skips link expansion, so this
    //    second call returns exactly 1 page, not 3 — see webcrawl.rs.)
    let _ = shutdown_site.send(());
    tokio::time::sleep(Duration::from_millis(100)).await;

    let cached_result = client::call(
        &socket,
        &sock_path,
        "read_website",
        Some(json!({"url": site_url, "maxDepth": 2, "maxPages": 10})),
        Duration::from_secs(10),
    )
    .await
    .expect("read_website should still succeed from cache with the server down");

    let cached_pages = cached_result["pages"].as_array().expect("pages array");
    assert_eq!(
        cached_pages.len(),
        1,
        "cache hit should skip link expansion, got: {cached_pages:?}"
    );
    assert_eq!(cached_pages[0]["title"], "Index");

    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
}
