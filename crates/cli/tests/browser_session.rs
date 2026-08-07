//! Real-daemon integration tests for the four browser-automation MCP tools
//! (plan.md Epic 6 / Story 6.2 and `project_plans/browser-automation/implementation/validation.md`'s
//! requirement-mapping table). The flagship test,
//! `browser_navigate_click_type_snapshot_round_trip`, drives
//! `stapler_browser_navigate` -> `stapler_browser_click` ->
//! `stapler_browser_type` -> `stapler_browser_snapshot` in sequence against a
//! single session over the real Unix-socket daemon protocol, mirroring
//! `docs_index.rs`'s `docs_index_round_trip` structure (`spawn_mock_site` +
//! `client::call`). The rest of this file's tests isolate individual
//! requirement rows from validation.md's table that the round trip alone
//! doesn't name-for-name cover (non-empty `sessionId`, the click/type
//! real-page assertions, the lazy-AX-tree race, reaper shutdown ordering, and
//! `FrameNavigatedGuard` recovery), reusing the same harness.
//!
//! Native adapter only, per requirements.md — the wasm adapter gets a
//! lighter smoke test (already covered in Epic 4's own scope), not this full
//! round trip.
//!
//! Requires a real Chrome/Chromium binary for `chromiumoxide::Browser::launch`
//! to succeed, so — exactly like `docs_index_round_trip`'s own justification
//! for being `#[ignore]`d (a real dependency `cargo test`'s default run can't
//! assume) — this is opted out of the default run. Run explicitly with:
//!   cargo test -p stapler-mcp --test browser_session -- --ignored
//!
//! **Concurrency note**: several tests here mutate the process-global
//! `STAPLER_MCP_HOME`/`STAPLER_MCP_ALLOW_PRIVATE_NETWORKS` env vars just
//! before spawning their own daemon subprocess (which inherits them at spawn
//! time), and one test (`navigate_should_clear_blocked_flag_when_renavigating_previously_blocked_session_to_safe_url`)
//! specifically needs `STAPLER_MCP_ALLOW_PRIVATE_NETWORKS` *unset* while the
//! others need it set to `"1"` to reach their own `127.0.0.1` mock site. Run
//! this file's `--ignored` tests with `--test-threads=1` to avoid one test's
//! env mutation racing another's daemon spawn.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};
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

/// Hand-rolled single-route HTTP mock, same pattern as `docs_index.rs`'s
/// `spawn_mock_site`: serves one HTML page with a `<button id="go">Go</button>`
/// and an `<input type="text" id="name">`, where clicking the button appends
/// `<p id="result">clicked!</p>` via inline `<script>` — enough surface for
/// `browser_navigate`/`click`/`type`/`snapshot` to exercise a same-page click
/// (no navigation) and a text-input round trip.
async fn spawn_mock_site() -> (String, tokio::sync::oneshot::Sender<()>) {
    let page = "<html><head><title>Browser Session Fixture</title></head><body>\
                <button id=\"go\">Go</button>\
                <input type=\"text\" id=\"name\">\
                <script>\
                document.getElementById('go').addEventListener('click', function () {\
                  var p = document.createElement('p');\
                  p.id = 'result';\
                  p.textContent = 'clicked!';\
                  document.body.appendChild(p);\
                });\
                </script>\
                </body></html>";
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

/// Depth-first search over an `AxSnapshotOutput`-shaped JSON tree (root has
/// `role`/`name`/`ref`/`children`) for the first node matching `pred`.
fn find_node<'a>(node: &'a Value, pred: &dyn Fn(&Value) -> bool) -> Option<&'a Value> {
    if pred(node) {
        return Some(node);
    }
    for child in node["children"].as_array().into_iter().flatten() {
        if let Some(found) = find_node(child, pred) {
            return Some(found);
        }
    }
    None
}

/// True if `node` or any descendant's `name` contains `needle` — used to
/// confirm the post-click DOM mutation ("clicked!") shows up somewhere in
/// the accessibility tree, regardless of which node the text's accessible
/// name ends up attached to.
fn tree_contains_name(node: &Value, needle: &str) -> bool {
    if node["name"].as_str().is_some_and(|n| n.contains(needle)) {
        return true;
    }
    node["children"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|child| tree_contains_name(child, needle))
}

/// Shared boilerplate for every test below: fresh `STAPLER_MCP_HOME`,
/// `STAPLER_MCP_ALLOW_PRIVATE_NETWORKS` set per `allow_private_networks`
/// (must be `true` for any test whose daemon needs to reach its own
/// `127.0.0.1` mock site; must be `false` for the one test that needs the
/// real SSRF guard's `NetworkPolicy::Enforce` active), then `ensure_daemon`.
/// Returns the still-live `TempDir` (drop order matters: it must outlive the
/// daemon subprocess using it as `STAPLER_MCP_HOME`, so the caller must hold
/// it for the rest of the test) plus what every `client::call` needs.
async fn start_daemon(allow_private_networks: bool) -> (tempfile::TempDir, NativeSocketFactory, String) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_string_lossy().to_string();
    std::env::set_var("STAPLER_MCP_HOME", &home);
    if allow_private_networks {
        std::env::set_var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS", "1");
    } else {
        std::env::remove_var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS");
    }
    let env = TestEnv { home };
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
            // Generous: first run may need to launch a fresh Chromium
            // process in addition to the daemon itself.
            startup_timeout: Some(Duration::from_secs(60)),
            exe_hint: Some(exe),
        },
    )
    .await
    .expect("daemon should auto-start");

    (tmp, socket, sock_path)
}

#[tokio::test]
#[ignore]
async fn browser_navigate_click_type_snapshot_round_trip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().to_string_lossy().to_string();
    std::env::set_var("STAPLER_MCP_HOME", &home);
    // The mock site below binds a real 127.0.0.1 listener; opt the daemon
    // subprocess out of the SSRF guard so it can reach it — same rationale
    // as `docs_index.rs`/`webcrawl.rs`.
    std::env::set_var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS", "1");
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
            // Generous: first run may need to launch a fresh Chromium
            // process in addition to the daemon itself.
            startup_timeout: Some(Duration::from_secs(60)),
            exe_hint: Some(exe),
        },
    )
    .await
    .expect("daemon should auto-start");

    // 1. navigate: start a fresh session against the mock page.
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
    assert!(!session_id.is_empty(), "got: {navigate_result:?}");

    let go_button = find_node(&navigate_result["snapshot"]["root"], &|n| {
        n["role"] == "button" && n["name"].as_str().is_some_and(|s| s.contains("Go"))
    })
    .unwrap_or_else(|| panic!("expected a 'Go' button node, got: {navigate_result:?}"));
    let go_ref = go_button["ref"].as_str().expect("button ref").to_string();

    let name_input = find_node(&navigate_result["snapshot"]["root"], &|n| {
        n["role"] == "textbox"
    })
    .unwrap_or_else(|| panic!("expected a textbox node, got: {navigate_result:?}"));
    let name_ref = name_input["ref"].as_str().expect("textbox ref").to_string();

    // 2. click: clicking "Go" mutates the DOM in place — no navigation, so
    //    the returned `note` must be absent, and the tree must now contain
    //    the "clicked!" text the inline script appended.
    let click_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_click",
        Some(json!({ "sessionId": session_id, "refId": go_ref })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_click should succeed");

    assert!(
        click_result.get("note").is_none() || click_result["note"].is_null(),
        "same-page click should not set a navigation note, got: {click_result:?}"
    );
    assert!(
        tree_contains_name(&click_result["snapshot"]["root"], "clicked!"),
        "expected 'clicked!' somewhere in the post-click snapshot, got: {click_result:?}"
    );

    // 3. type: type into the input, then confirm a follow-up snapshot
    //    reflects the typed value.
    client::call(
        &socket,
        &sock_path,
        "stapler_browser_type",
        Some(json!({ "sessionId": session_id, "refId": name_ref, "text": "Ada" })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_type should succeed");

    let snapshot_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_snapshot",
        Some(json!({ "sessionId": session_id })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_snapshot should succeed");

    let typed_input = find_node(&snapshot_result["snapshot"]["root"], &|n| {
        n["role"] == "textbox"
    })
    .unwrap_or_else(|| panic!("expected a textbox node, got: {snapshot_result:?}"));
    assert_eq!(
        typed_input["value"].as_str(),
        Some("Ada"),
        "expected the typed value to be reflected, got: {snapshot_result:?}"
    );

    let _ = shutdown_site.send(());

    client::call(
        &socket,
        &sock_path,
        "shutdown",
        None,
        Duration::from_secs(2),
    )
    .await
    .expect("shutdown call should succeed");
}

/// REQ-1a integration (validation.md): real daemon + mock site, assert the
/// response JSON has a non-empty `sessionId` (Task 5.2.1 AC). Deliberately
/// narrower than the full round trip above — this only exercises the first
/// `navigate` call's own contract.
#[tokio::test]
#[ignore]
async fn stapler_browser_navigate_should_return_nonempty_session_id_over_real_daemon() {
    let (_tmp, socket, sock_path) = start_daemon(true).await;
    let (site_url, shutdown_site) = spawn_mock_site().await;

    let navigate_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_navigate",
        Some(json!({ "url": site_url })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_navigate should succeed");

    let session_id = navigate_result["sessionId"].as_str();
    assert!(
        session_id.is_some_and(|s| !s.is_empty()),
        "expected a non-empty sessionId, got: {navigate_result:?}"
    );

    let _ = shutdown_site.send(());
    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
}

/// REQ-1b integration (validation.md, Task 3.3.1 AC): real mock page with
/// `<button id="go">`, click it, assert the `"clicked!"` node the page's own
/// inline script appends shows up in the post-click snapshot.
#[tokio::test]
#[ignore]
async fn click_should_show_clicked_text_when_button_clicked_on_real_page() {
    let (_tmp, socket, sock_path) = start_daemon(true).await;
    let (site_url, shutdown_site) = spawn_mock_site().await;

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
    let go_button = find_node(&navigate_result["snapshot"]["root"], &|n| {
        n["role"] == "button" && n["name"].as_str().is_some_and(|s| s.contains("Go"))
    })
    .unwrap_or_else(|| panic!("expected a 'Go' button node, got: {navigate_result:?}"));
    let go_ref = go_button["ref"].as_str().expect("button ref").to_string();

    let click_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_click",
        Some(json!({ "sessionId": session_id, "refId": go_ref })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_click should succeed");

    assert!(
        tree_contains_name(&click_result["snapshot"]["root"], "clicked!"),
        "expected 'clicked!' somewhere in the post-click snapshot, got: {click_result:?}"
    );

    let _ = shutdown_site.send(());
    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
}

/// REQ-1c integration (validation.md, Task 3.3.2 AC): type into
/// `<input id="name">`, then a follow-up `stapler_browser_snapshot` call
/// shows the typed value reflected in the accessible `value`.
#[tokio::test]
#[ignore]
async fn type_text_should_reflect_typed_value_in_accessible_value_when_real_input_used() {
    let (_tmp, socket, sock_path) = start_daemon(true).await;
    let (site_url, shutdown_site) = spawn_mock_site().await;

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
    let name_input = find_node(&navigate_result["snapshot"]["root"], &|n| {
        n["role"] == "textbox"
    })
    .unwrap_or_else(|| panic!("expected a textbox node, got: {navigate_result:?}"));
    let name_ref = name_input["ref"].as_str().expect("textbox ref").to_string();

    client::call(
        &socket,
        &sock_path,
        "stapler_browser_type",
        Some(json!({ "sessionId": session_id, "refId": name_ref, "text": "hello" })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_type should succeed");

    let snapshot_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_snapshot",
        Some(json!({ "sessionId": session_id })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_snapshot should succeed");

    let typed_input = find_node(&snapshot_result["snapshot"]["root"], &|n| {
        n["role"] == "textbox"
    })
    .unwrap_or_else(|| panic!("expected a textbox node, got: {snapshot_result:?}"));
    assert_eq!(
        typed_input["value"].as_str(),
        Some("hello"),
        "expected the typed value to be reflected, got: {snapshot_result:?}"
    );

    let _ = shutdown_site.send(());
    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
}

/// REQ-2 integration (validation.md, Task 3.2.1 AC): "exercises the
/// lazy-AX-tree race the synthetic unit test cannot" — a real Chromium
/// accessibility tree isn't necessarily ready the instant navigation
/// completes, so `navigate`'s own `wait_and_capture` retry loop (native/ax.rs)
/// only ever gets exercised for real against a live page, never a fake one.
/// Asserts the returned snapshot is non-empty (has a role and at least one
/// child), which would fail outright if the retry loop didn't wait for the
/// tree to materialize.
#[tokio::test]
#[ignore]
async fn navigate_should_return_nonempty_snapshot_when_axtree_built_lazily_on_real_page() {
    let (_tmp, socket, sock_path) = start_daemon(true).await;
    let (site_url, shutdown_site) = spawn_mock_site().await;

    let navigate_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_navigate",
        Some(json!({ "url": site_url })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_navigate should succeed");

    let root = &navigate_result["snapshot"]["root"];
    assert!(
        root["role"].as_str().is_some_and(|r| !r.is_empty()),
        "expected a non-empty root role, got: {navigate_result:?}"
    );
    assert!(
        root["children"]
            .as_array()
            .is_some_and(|children| !children.is_empty()),
        "expected the lazily-built AX tree to have at least one child node, got: {navigate_result:?}"
    );

    let session_id = navigate_result["sessionId"]
        .as_str()
        .expect("sessionId present")
        .to_string();
    let _ = shutdown_site.send(());
    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
    let _ = session_id; // kept only for readability of the call sequence above
}

/// REQ-3 integration (validation.md, Task 2.4.1 AC): a `"shutdown"` call with
/// one open session completes without hanging. Exercises `crates/cli/src/main.rs`'s
/// production shutdown sequence — abort-and-await the `SessionIdleReaper`
/// *before* `NativeBrowser::close()` — end to end, something
/// `spawn_reaper`/`reap_expired`'s own unit tests (which only ever construct
/// a `FakeSession`, never a real `Browser`/daemon process) can't reach.
/// Doesn't assert ordering directly (no lifecycle hook exposes that) — per
/// the AC, "completes without hanging" is the observable contract, enforced
/// here with an explicit wrapping timeout so a regression that reintroduces
/// the abort-after-close race (which could deadlock waiting on a reaper that
/// never wakes) fails loudly instead of hanging the test suite forever.
#[tokio::test]
#[ignore]
async fn shutdown_should_abort_reaper_before_browser_close_when_session_open() {
    let (_tmp, socket, sock_path) = start_daemon(false).await;

    // A `data:` URL needs no mock HTTP server (and has no `host()`, so it's
    // never subject to the SSRF guard regardless of policy) — enough to open
    // one live session for `"shutdown"` to have something to close.
    let navigate_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_navigate",
        Some(json!({ "url": "data:text/html,<html><body>open session</body></html>" })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_navigate should succeed");
    assert!(navigate_result["sessionId"].as_str().is_some_and(|s| !s.is_empty()));

    let shutdown = tokio::time::timeout(
        Duration::from_secs(15),
        client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(10)),
    )
    .await;

    assert!(
        shutdown.is_ok(),
        "shutdown call with one open session hung past the 15s test-level timeout"
    );
    shutdown
        .unwrap()
        .expect("shutdown call should succeed with an open session");
}

/// REQ-6 integration (validation.md, Task 3.4.2's second AC): re-navigating a
/// `blocked`-flagged session to a safe URL succeeds and clears the flag.
///
/// Deliberately does **not** set `STAPLER_MCP_ALLOW_PRIVATE_NETWORKS` — this
/// test needs the real SSRF guard (`NetworkPolicy::Enforce`) active so an
/// in-page navigation to a link-local address actually trips the
/// `FrameNavigatedGuard` (Task 3.4.2's first AC, covered by this file's
/// sibling unit test in `crates/native/src/browser.rs`). Both `navigate`
/// targets below are `data:` URLs — `url::Url::host()` returns `None` for
/// them, so `blocked_host_reason`'s pre-flight check (which only looks at
/// the *navigate call's own target*) never rejects either call; only the
/// mock page's own in-page link click — which the pre-flight check has no
/// visibility into — is meant to trip the guard, exactly the gap
/// `FrameNavigatedGuard` exists to close.
#[tokio::test]
#[ignore]
async fn navigate_should_clear_blocked_flag_when_renavigating_previously_blocked_session_to_safe_url(
) {
    let (_tmp, socket, sock_path) = start_daemon(false).await;

    let poisoned_page = "data:text/html,<html><body><a id=\"go\" href=\"http://169.254.169.254/\">go</a></body></html>";
    let navigate_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_navigate",
        Some(json!({ "url": poisoned_page })),
        Duration::from_secs(30),
    )
    .await
    .expect("initial navigate to a safe data: URL should succeed");
    let session_id = navigate_result["sessionId"]
        .as_str()
        .expect("sessionId present")
        .to_string();
    let go_link = find_node(&navigate_result["snapshot"]["root"], &|n| {
        n["name"].as_str().is_some_and(|s| s.contains("go"))
    })
    .unwrap_or_else(|| panic!("expected a 'go' link node, got: {navigate_result:?}"));
    let go_ref = go_link["ref"].as_str().expect("link ref").to_string();

    // Click the in-page link to a link-local address. The block may surface
    // directly on this call (if `FrameNavigatedGuard` fires within
    // `dispatch_action`'s grace period) or only on the *next* call — both are
    // the documented fallback (Task 3.4.2) — so accept either shape and fall
    // through to an explicit snapshot call to force the assertion if needed.
    let click_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_click",
        Some(json!({ "sessionId": session_id, "refId": go_ref })),
        Duration::from_secs(30),
    )
    .await;
    let blocked_seen_on_click = matches!(&click_result, Err(e) if e.to_string().contains("blocked"));

    if !blocked_seen_on_click {
        let snapshot_result = client::call(
            &socket,
            &sock_path,
            "stapler_browser_snapshot",
            Some(json!({ "sessionId": session_id })),
            Duration::from_secs(30),
        )
        .await;
        let err = snapshot_result.expect_err(
            "a session poisoned by an in-page navigation to a link-local address should error on the next call",
        );
        assert!(
            err.to_string().contains("blocked"),
            "expected a 'blocked' error, got: {err}"
        );
    }

    // Recovery: re-navigating the same session to a safe URL must clear the
    // `blocked` flag and succeed.
    let safe_page = "data:text/html,<html><body><p id=\"ok\">recovered</p></body></html>";
    let recovery_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_navigate",
        Some(json!({ "sessionId": session_id, "url": safe_page })),
        Duration::from_secs(30),
    )
    .await
    .expect("re-navigating a blocked session to a safe URL should succeed and clear the flag");
    assert_eq!(recovery_result["sessionId"].as_str(), Some(session_id.as_str()));

    // Confirm the flag is really cleared: a follow-up snapshot on the same
    // session must succeed, not repeat the stale "blocked" error.
    client::call(
        &socket,
        &sock_path,
        "stapler_browser_snapshot",
        Some(json!({ "sessionId": session_id })),
        Duration::from_secs(30),
    )
    .await
    .expect("snapshot after recovery should succeed, confirming the blocked flag was cleared");

    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
}

/// UX AC1 (design/ux.md, validation.md's UX table): call `navigate`; take
/// `ref`s from that one response only; then call `type`, `type`, `click`,
/// `snapshot` in sequence, never issuing a role/name pair or CSS selector to
/// the tools themselves (locating nodes by role/name in the *client's own*
/// JSON parsing, as done here via `find_node`, is how an agent reads a
/// response — it's issuing a `ref` id, not a selector, that the criterion is
/// about) — confirming no step needs a value absent from a prior response.
#[tokio::test]
#[ignore]
async fn ux_ac1_agent_should_complete_navigate_type_type_click_snapshot_using_only_response_refs()
{
    let (_tmp, socket, sock_path) = start_daemon(true).await;
    let (site_url, shutdown_site) = spawn_mock_site().await;

    // Step 1: navigate. Every ref used below comes from this one response.
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
    let go_button = find_node(&navigate_result["snapshot"]["root"], &|n| {
        n["role"] == "button" && n["name"].as_str().is_some_and(|s| s.contains("Go"))
    })
    .unwrap_or_else(|| panic!("expected a 'Go' button node, got: {navigate_result:?}"));
    let go_ref = go_button["ref"].as_str().expect("button ref").to_string();
    let name_input = find_node(&navigate_result["snapshot"]["root"], &|n| {
        n["role"] == "textbox"
    })
    .unwrap_or_else(|| panic!("expected a textbox node, got: {navigate_result:?}"));
    let name_ref = name_input["ref"].as_str().expect("textbox ref").to_string();

    // Step 2: type (first). Only `session_id`/`name_ref` from step 1's
    // response are used — no selector, no re-derivation.
    client::call(
        &socket,
        &sock_path,
        "stapler_browser_type",
        Some(json!({ "sessionId": session_id, "refId": name_ref, "text": "Ada" })),
        Duration::from_secs(30),
    )
    .await
    .expect("first browser_type should succeed");

    // Step 3: type (second) — same ref, still valid because nothing here
    // navigated (a fresh nav_generation would invalidate it).
    client::call(
        &socket,
        &sock_path,
        "stapler_browser_type",
        Some(json!({ "sessionId": session_id, "refId": name_ref, "text": "Lovelace" })),
        Duration::from_secs(30),
    )
    .await
    .expect("second browser_type should succeed");

    // Step 4: click, using the button ref from step 1's response.
    let click_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_click",
        Some(json!({ "sessionId": session_id, "refId": go_ref })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_click should succeed");
    assert!(
        tree_contains_name(&click_result["snapshot"]["root"], "clicked!"),
        "expected 'clicked!' somewhere in the post-click snapshot, got: {click_result:?}"
    );

    // Step 5: snapshot, using only `session_id` from step 1's response.
    let snapshot_result = client::call(
        &socket,
        &sock_path,
        "stapler_browser_snapshot",
        Some(json!({ "sessionId": session_id })),
        Duration::from_secs(30),
    )
    .await
    .expect("browser_snapshot should succeed");
    let typed_input = find_node(&snapshot_result["snapshot"]["root"], &|n| {
        n["role"] == "textbox"
    })
    .unwrap_or_else(|| panic!("expected a textbox node, got: {snapshot_result:?}"));
    assert_eq!(
        typed_input["value"].as_str(),
        Some("Lovelace"),
        "expected the second type call's value to be reflected, got: {snapshot_result:?}"
    );

    let _ = shutdown_site.send(());
    client::call(&socket, &sock_path, "shutdown", None, Duration::from_secs(2))
        .await
        .expect("shutdown call should succeed");
}
