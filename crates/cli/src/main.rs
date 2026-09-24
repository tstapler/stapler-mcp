mod http_server;
mod mcp_router;
mod transport;

use std::rc::Rc;
use std::time::Duration;

use rmcp::{transport::stdio, ServiceExt};

use stapler_mcp_core::daemon::{json_handler, Daemon, Handler};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::{EnvPort, LockError, LockGuard, ProcessLock};
use stapler_mcp_core::schema::{
    BraveSearchInput, BrowserClickInput, BrowserCloseAllSessionsInput, BrowserCloseSessionInput,
    BrowserEvaluateInput, BrowserFillFormInput, BrowserFindInput, BrowserGetHtmlInput,
    BrowserHistoryInput, BrowserHoverInput, BrowserListSessionsInput, BrowserNavigateInput,
    BrowserPressKeyInput, BrowserResizeInput, BrowserScreenshotInput, BrowserSelectOptionInput,
    BrowserSetCheckedInput, BrowserSnapshotInput, BrowserTabsInput, BrowserTypeInput,
    BrowserTypeSecretInput, BrowserWaitForInput, DaemonStatusOutput, DownloadWebsiteInput,
    FetchPageInput, IndexDocsInput, ListIndexedSourcesInput, ReadSavedPageInput, ReadWebsiteInput,
    RemoveIndexedSourceInput, SearchDocsInput,
};
use stapler_mcp_core::tools::{browser, credential, docs, fetch, search, webcrawl};
use stapler_mcp_native::{
    NativeBrowser, NativeClock, NativeCredentialStore, NativeEmbedder, NativeEnv, NativeFs,
    NativeHttp, NativeLock, NativeSocketFactory, NativeSpawner,
};

/// Epic 6.1 AC: verbatim, so a caller sees exactly this string and a unit
/// test (`credential_wiring_tests`, below) can assert against the same
/// constant rather than a duplicated literal.
const VAULT_NOT_CONFIGURED_MESSAGE: &str =
    "vault not configured: set OP_SERVICE_ACCOUNT_TOKEN in the daemon's environment and restart";

/// The opt-out branch's handler: never references `browser` or constructs a
/// `NativeCredentialStore`, so registering it (the `OP_SERVICE_ACCOUNT_TOKEN`
/// -unset path) is structurally incapable of touching either — the AC this
/// function exists to make independently unit-testable without a live
/// `NativeBrowser` (which needs a real Chromium binary to construct at all).
fn vault_not_configured_handler() -> Handler {
    json_handler(|_input: BrowserTypeSecretInput| async {
        Err::<stapler_mcp_core::schema::BrowserActionOutput, String>(
            VAULT_NOT_CONFIGURED_MESSAGE.to_string(),
        )
    })
}

/// The opt-in branch's handler: `browser` already carries the injected
/// `NativeCredentialStore` (set once at startup below), so this reaches
/// `NativeBrowser::type_secret` -> its own injected store's `resolve()` —
/// this function itself never calls `CredentialStore::resolve` directly
/// (Story 5.2.1).
fn type_secret_handler(browser: Rc<NativeBrowser>) -> Handler {
    json_handler(move |input: BrowserTypeSecretInput| {
        let browser = browser.clone();
        async move { credential::browser_type_secret(&*browser, input).await }
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // `--status`/`--print-config` are checked before `--daemon` deliberately:
    // both are read-only diagnostics meant to run from an ordinary shell
    // against an already-running (often systemd/launchd-started) daemon, so
    // neither may spawn one as a side effect.
    let is_status = args.iter().any(|a| a == "--status");
    let is_print_config = args.iter().any(|a| a == "--print-config");
    let is_daemon = args.iter().any(|a| a == "--daemon");

    // Deliberately single-threaded: this daemon's work is I/O-bound, not
    // CPU-bound, and a `current_thread` runtime + `LocalSet` lets the exact
    // same core (no `Send` bounds anywhere) also satisfy a `!Send`
    // wasm-bindgen adapter later, without ever needing `async-trait`.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    let local = tokio::task::LocalSet::new();

    if is_status {
        let code = local.block_on(&rt, run_status());
        std::process::exit(code);
    } else if is_print_config {
        let code = local.block_on(&rt, run_print_config());
        std::process::exit(code);
    } else if is_daemon {
        local.block_on(&rt, run_daemon());
        // `run_daemon`'s own `tokio::join!` already bounds the accept loop,
        // bridge consumer, and HTTP server tasks individually via
        // `await_with_shutdown_grace` — but giving up on a `JoinHandle`
        // doesn't abort the underlying `tokio::spawn`ed task (e.g. the HTTP
        // server can still be draining a lingering keep-alive connection).
        // Dropping `rt` normally (the `else` branches below fall off the end
        // of `main` and do exactly that) blocks *indefinitely* on any such
        // straggler, since `Runtime`'s destructor waits for every spawned
        // task with no timeout. `shutdown_timeout` bounds that final wait and
        // force-cancels stragglers past it, so SIGTERM/`SHUTDOWN_TOOL` always
        // produce a bounded process exit.
        rt.shutdown_timeout(SHUTDOWN_GRACE_TIMEOUT);
    } else {
        local.block_on(&rt, run_thin_client());
    }
}

/// Short timeout for `--status`'s liveness ping — this must never trigger
/// `ensure_daemon`'s auto-spawn behavior, so it calls `client::ping` directly
/// rather than going through the thin client's usual `ensure_daemon` path.
const STATUS_PING_TIMEOUT: Duration = Duration::from_secs(2);
const STATUS_TCP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// `stapler-mcp --status`: reports whether the shared daemon is reachable
/// and, separately, whether its optional HTTP transport is configured and
/// listening. Read-only — never spawns a daemon. Returns the process exit
/// code (0 if the daemon itself is running, regardless of HTTP state).
async fn run_status() -> i32 {
    let env = NativeEnv;
    let socket = NativeSocketFactory;
    let sock_path = paths::socket_path(&env);

    let ping_result = stapler_mcp_core::client::call(
        &socket,
        &sock_path,
        stapler_mcp_core::protocol::PING_TOOL,
        None,
        STATUS_PING_TIMEOUT,
    )
    .await;
    let Ok(ping_result) = ping_result else {
        println!("daemon: not running");
        println!("try: systemctl --user start stapler-mcp   (or: stapler-mcp --daemon)");
        return 1;
    };

    print_daemon_pid(&env);
    print_browser_profile_status(&ping_result);
    print_http_status(&env).await;
    0
}

/// Computes the `browser profile: ...` line (and, if present, a hazard
/// warning line) from a `DaemonStatusOutput` — the same typed struct
/// `stapler_daemon_status`'s `daemon_status()` deserializes from `ping`'s
/// response (`Daemon::set_status_extra`, wired in `run_daemon`). Going
/// through the shared type rather than raw `serde_json::Value::get(...)`
/// calls means a wire-key mismatch is a deserialization miss on a named
/// field, not two independently-typed string literals that could silently
/// drift apart.
fn browser_profile_status_lines(status: &DaemonStatusOutput) -> Vec<String> {
    let mode = status
        .browser_profile_mode
        .as_deref()
        .unwrap_or("ephemeral");
    let mut lines = vec![format!("browser profile: {mode}")];
    if let Some(warning) = &status.browser_profile_warning {
        lines.push(format!("warning: {warning}"));
    }
    lines
}

/// `ping_result` is `--status`'s raw `client::call` response; deserializing
/// it into `DaemonStatusOutput` here (rather than reading `.get("...")`
/// keys directly) fails to `None` fields, not silently wrong values, if
/// `run_daemon`'s wire format ever drifts from this struct.
fn print_browser_profile_status(ping_result: &serde_json::Value) {
    let status: DaemonStatusOutput =
        serde_json::from_value(ping_result.clone()).unwrap_or(DaemonStatusOutput {
            pong: false,
            browser_profile_mode: None,
            browser_profile_warning: None,
        });
    for line in browser_profile_status_lines(&status) {
        println!("{line}");
    }
}

#[cfg(test)]
mod browser_profile_status_tests {
    use super::*;

    #[test]
    fn should_default_to_ephemeral_when_mode_absent() {
        let status = DaemonStatusOutput {
            pong: true,
            browser_profile_mode: None,
            browser_profile_warning: None,
        };
        assert_eq!(
            browser_profile_status_lines(&status),
            vec!["browser profile: ephemeral".to_string()]
        );
    }

    #[test]
    fn should_print_persistent_mode_and_warning_when_present() {
        let status = DaemonStatusOutput {
            pong: true,
            browser_profile_mode: Some(
                "persistent at /home/alice/.stapler-mcp/browser-profile".to_string(),
            ),
            browser_profile_warning: Some("looks unsafe".to_string()),
        };
        assert_eq!(
            browser_profile_status_lines(&status),
            vec![
                "browser profile: persistent at /home/alice/.stapler-mcp/browser-profile"
                    .to_string(),
                "warning: looks unsafe".to_string(),
            ]
        );
    }
}

/// Prints the `daemon: running (pid ...)` line, reading the PID directly out
/// of the lock file (`write_pid`'s only externally-visible form — flock is
/// advisory, so reading it without acquiring the lock is safe).
fn print_daemon_pid(env: &NativeEnv) {
    let lock_path = paths::lock_path(env);
    match std::fs::read_to_string(&lock_path)
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
    {
        Some(pid) => println!("daemon: running (pid {pid})"),
        None => println!("daemon: running (pid unknown)"),
    }
}

/// Prints the `http: ...` status line, sourced from the persisted
/// `http-port` file rather than this process's own environment — the
/// systemd/launchd-started daemon case never has `STAPLER_MCP_HTTP_PORT` set
/// in the shell that later runs `--status`.
async fn print_http_status(env: &NativeEnv) {
    let http_port_path = paths::http_port_path(env);
    let Some(port) = std::fs::read_to_string(&http_port_path)
        .ok()
        .and_then(|s| s.trim().parse::<u16>().ok())
    else {
        println!("http: not configured (STAPLER_MCP_HTTP_PORT not set on last daemon start)");
        return;
    };

    let addr = format!("127.0.0.1:{port}");
    let reachable = matches!(
        tokio::time::timeout(
            STATUS_TCP_PROBE_TIMEOUT,
            tokio::net::TcpStream::connect(&addr)
        )
        .await,
        Ok(Ok(_))
    );
    if reachable {
        println!("http: listening on 127.0.0.1:{port}");
    } else {
        println!("http: not listening on 127.0.0.1:{port} (configured but unreachable)");
    }
}

/// `stapler-mcp --print-config`: prints an MCP client config block for the
/// HTTP transport, sourced entirely from files the daemon itself already
/// persisted (`http-token`/`http-port`) — never generates a token or port
/// itself. Purely deterministic reads, so running this twice in a row
/// against unchanged state produces byte-identical output.
async fn run_print_config() -> i32 {
    let env = NativeEnv;
    let token = read_nonempty_file(&paths::http_token_path(&env));
    let port = read_nonempty_file(&paths::http_port_path(&env));

    match (token, port) {
        (Some(token), Some(port)) => {
            println!("{}", print_config_json(&token, &port));
            0
        }
        (None, _) => {
            println!(
                "stapler-mcp: the daemon hasn't started with HTTP enabled yet. Start it with: STAPLER_MCP_HTTP_PORT=<port> stapler-mcp --daemon"
            );
            1
        }
        (Some(_), None) => {
            println!(
                "stapler-mcp: HTTP isn't currently enabled on the running daemon (its last startup had STAPLER_MCP_HTTP_PORT unset). Restart it with STAPLER_MCP_HTTP_PORT=<port> set to enable HTTP."
            );
            1
        }
    }
}

fn read_nonempty_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn print_config_json(token: &str, port: &str) -> String {
    let config = serde_json::json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{port}/mcp"),
        "headers": {
            "Authorization": format!("Bearer {token}")
        }
    });
    serde_json::to_string(&config).expect("serialize print-config JSON")
}

/// Grace period each of the accept loop / bridge consumer / HTTP server gets
/// to notice cancellation and finish its own in-flight work before shutdown
/// cleanup proceeds regardless. See `run_daemon`'s `tokio::join!` below.
const SHUTDOWN_GRACE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn run_thin_client() {
    let client = mcp_router::McpRouter::new();
    let service = match client.serve(stdio()).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("stapler-mcp: failed to start stdio transport: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = service.waiting().await {
        eprintln!("stapler-mcp: stdio transport error: {e}");
        std::process::exit(1);
    }
}

async fn run_daemon() {
    let env = NativeEnv;
    let base = paths::base_dir(&env);
    let sock_path = paths::socket_path(&env);
    let lock_path = paths::lock_path(&env);

    if let Err(e) = std::fs::create_dir_all(&base) {
        eprintln!("stapler-mcp: failed to create state dir {base}: {e}");
        std::process::exit(1);
    }

    // `STAPLER_MCP_HTTP_PORT` records configuration *intent*, not liveness —
    // written unconditionally here regardless of whether the bind below
    // later succeeds, so a `--status`/`--print-config` command can report
    // what this invocation was asked to do. Stale from a prior HTTP-enabled
    // run, it's removed so a later non-HTTP invocation doesn't leak it.
    let http_port: Option<u16> = std::env::var("STAPLER_MCP_HTTP_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok());
    let http_port_path = paths::http_port_path(&env);
    match http_port {
        Some(port) => {
            if let Err(e) = std::fs::write(&http_port_path, port.to_string()) {
                eprintln!("stapler-mcp: failed to write HTTP port file {http_port_path}: {e}");
            }
        }
        None => {
            if let Err(e) = std::fs::remove_file(&http_port_path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!(
                        "stapler-mcp: failed to remove stale HTTP port file {http_port_path}: {e}"
                    );
                }
            }
        }
    }

    let lock = NativeLock;
    let mut guard = match lock.acquire_exclusive(&lock_path).await {
        Ok(g) => g,
        Err(LockError::AlreadyRunning) => {
            // Losing side of the flock race — clean, expected exit, not a failure.
            eprintln!("stapler-mcp: daemon already running");
            return;
        }
        Err(LockError::Other(e)) => {
            eprintln!("stapler-mcp: failed to acquire lock: {e}");
            std::process::exit(1);
        }
    };
    guard.write_pid(std::process::id());

    let http = Rc::new(NativeHttp::new());
    let fs = Rc::new(NativeFs);
    // Opt-in, daemon-startup-only: read once here, not per-call, since
    // `user_data_dir` is consumed once at `Browser::launch()` — a
    // `stapler_browser_navigate` tool parameter would silently no-op after
    // the daemon's first `navigate`. See `paths::browser_profile_dir`.
    let persistent_profile_dir = paths::browser_profile_dir(&env).map(std::path::PathBuf::from);
    let browser_profile_mode = match &persistent_profile_dir {
        Some(dir) => format!("persistent at {}", dir.display()),
        None => "ephemeral".to_string(),
    };
    match &persistent_profile_dir {
        Some(dir) => eprintln!(
            "stapler-mcp: browser profile: persistent at {}",
            dir.display()
        ),
        None => eprintln!(
            "stapler-mcp: browser profile: ephemeral (temp dir, does not survive daemon restart)"
        ),
    }
    let browser = match NativeBrowser::launch(persistent_profile_dir).await {
        Ok(b) => Rc::new(b),
        Err(e) => {
            eprintln!("stapler-mcp: failed to launch browser: {e}");
            std::process::exit(1);
        }
    };
    // Epic 6.1: infrastructure-level opt-in per requirements.md's Risk
    // Control section — a `NativeCredentialStore` is only ever constructed
    // (and injected into `browser`) when the token is present. Reading
    // `OP_SERVICE_ACCOUNT_TOKEN` via `EnvPort` rather than `std::env`
    // directly matches every other env read in this file (see
    // `network_policy` below).
    let credential_store_present = EnvPort::var(&env, "OP_SERVICE_ACCOUNT_TOKEN")
        .map(|token| {
            browser.set_credential_store(Rc::new(NativeCredentialStore::new(NativeSpawner, token)));
        })
        .is_some();

    let embedder = Rc::new(NativeEmbedder::new(paths::embedding_cache_dir(&env)));
    let clock = Rc::new(NativeClock);
    let source_locks = Rc::new(docs::SourceLocks::new());
    let docs_index_dir = paths::docs_index_dir(&env);

    // `Rc`-wrapped from construction: registration below only needs `&self`
    // (works fine through `Deref`), and the bridge consumer spawned after
    // registration needs its own clone of the same daemon.
    let daemon = Rc::new(Daemon::new());
    // Echoed on every `ping` (and therefore `stapler_daemon_status` /
    // `--status`) response so an operator/agent can positively verify
    // `STAPLER_MCP_BROWSER_PROFILE_DIR` took effect, rather than inferring it
    // from whether cookies survived a restart.
    daemon.set_status_extra(serde_json::json!({
        "browserProfileMode": browser_profile_mode,
        "browserProfileWarning": browser.unsafe_profile_warning(),
    }));

    daemon.register(
        "fetch_page",
        json_handler({
            let browser = browser.clone();
            let fs = fs.clone();
            move |input: FetchPageInput| {
                let browser = browser.clone();
                let fs = fs.clone();
                async move { fetch::fetch_page(&*browser, &*fs, input).await }
            }
        }),
    );

    daemon.register(
        "brave_web_search",
        json_handler({
            let http = http.clone();
            move |input: BraveSearchInput| {
                let http = http.clone();
                async move {
                    let api_key = std::env::var("BRAVE_API_KEY").unwrap_or_default();
                    let base_url = std::env::var("BRAVE_API_BASE_URL")
                        .unwrap_or_else(|_| search::DEFAULT_BASE_URL.to_string());
                    search::brave_web_search(&*http, &api_key, &base_url, input).await
                }
            }
        }),
    );

    let cache_dir = paths::cache_dir(&env);
    // Only ever set by this crate's own integration tests, to let the
    // crawler reach a `127.0.0.1` mock server — see `NetworkPolicy`'s doc
    // comment. Read once via `EnvPort` rather than `std::env` directly so
    // it's still exercised through the same seam as everything else in
    // `crates/core`.
    let network_policy = webcrawl::NetworkPolicy::from_env(
        stapler_mcp_core::ports::EnvPort::var(&env, "STAPLER_MCP_ALLOW_PRIVATE_NETWORKS"),
        stapler_mcp_core::ports::EnvPort::var(&env, "STAPLER_MCP_ALLOWED_PRIVATE_HOSTS"),
    );
    daemon.register(
        "read_website",
        json_handler({
            let http = http.clone();
            let fs = fs.clone();
            let cache_dir = cache_dir.clone();
            let network_policy = network_policy.clone();
            move |input: ReadWebsiteInput| {
                let http = http.clone();
                let fs = fs.clone();
                let cache_dir = cache_dir.clone();
                let network_policy = network_policy.clone();
                async move {
                    webcrawl::read_website(&*http, &*fs, &cache_dir, input, network_policy).await
                }
            }
        }),
    );

    daemon.register(
        "read_saved_page",
        json_handler({
            let fs = fs.clone();
            let cache_dir = cache_dir.clone();
            move |input: ReadSavedPageInput| {
                let fs = fs.clone();
                let cache_dir = cache_dir.clone();
                async move { webcrawl::read_saved_page(&*fs, &cache_dir, input).await }
            }
        }),
    );

    daemon.register(
        "download_website",
        json_handler({
            let http = http.clone();
            let fs = fs.clone();
            let network_policy = network_policy.clone();
            move |input: DownloadWebsiteInput| {
                let http = http.clone();
                let fs = fs.clone();
                let network_policy = network_policy.clone();
                async move { webcrawl::download_website(&*http, &*fs, input, network_policy).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_navigate",
        json_handler({
            let browser = browser.clone();
            let network_policy = network_policy.clone();
            move |input: BrowserNavigateInput| {
                let browser = browser.clone();
                let network_policy = network_policy.clone();
                async move { browser::browser_navigate(&*browser, input, network_policy).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_click",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserClickInput| {
                let browser = browser.clone();
                async move { browser::browser_click(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_type",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserTypeInput| {
                let browser = browser.clone();
                async move { browser::browser_type(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_snapshot",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserSnapshotInput| {
                let browser = browser.clone();
                async move { browser::browser_snapshot(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_close_session",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserCloseSessionInput| {
                let browser = browser.clone();
                async move { browser::browser_close_session(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_list_sessions",
        json_handler({
            let browser = browser.clone();
            move |_input: BrowserListSessionsInput| {
                let browser = browser.clone();
                async move { browser::browser_list_sessions(&*browser).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_close_all_sessions",
        json_handler({
            let browser = browser.clone();
            move |_input: BrowserCloseAllSessionsInput| {
                let browser = browser.clone();
                async move { browser::browser_close_all_sessions(&*browser).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_tabs",
        json_handler({
            let browser = browser.clone();
            let network_policy = network_policy.clone();
            move |input: BrowserTabsInput| {
                let browser = browser.clone();
                let network_policy = network_policy.clone();
                async move { browser::browser_tabs(&*browser, input, network_policy).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_hover",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserHoverInput| {
                let browser = browser.clone();
                async move { browser::browser_hover(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_select_option",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserSelectOptionInput| {
                let browser = browser.clone();
                async move { browser::browser_select_option(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_press_key",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserPressKeyInput| {
                let browser = browser.clone();
                async move { browser::browser_press_key(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_wait_for",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserWaitForInput| {
                let browser = browser.clone();
                async move { browser::browser_wait_for(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_screenshot",
        json_handler({
            let browser = browser.clone();
            let fs = fs.clone();
            move |input: BrowserScreenshotInput| {
                let browser = browser.clone();
                let fs = fs.clone();
                async move { browser::browser_screenshot(&*browser, &*fs, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_evaluate",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserEvaluateInput| {
                let browser = browser.clone();
                async move { browser::browser_evaluate(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_get_html",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserGetHtmlInput| {
                let browser = browser.clone();
                async move { browser::browser_get_html(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_fill_form",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserFillFormInput| {
                let browser = browser.clone();
                async move { browser::browser_fill_form(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_set_checked",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserSetCheckedInput| {
                let browser = browser.clone();
                async move { browser::browser_set_checked(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_history",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserHistoryInput| {
                let browser = browser.clone();
                async move { browser::browser_history(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_resize",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserResizeInput| {
                let browser = browser.clone();
                async move { browser::browser_resize(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_find",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserFindInput| {
                let browser = browser.clone();
                async move { browser::browser_find(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_type_secret",
        if credential_store_present {
            type_secret_handler(browser.clone())
        } else {
            vault_not_configured_handler()
        },
    );

    daemon.register(
        "stapler_index_docs",
        json_handler({
            let http = http.clone();
            let fs = fs.clone();
            let embedder = embedder.clone();
            let clock = clock.clone();
            let source_locks = source_locks.clone();
            let docs_index_dir = docs_index_dir.clone();
            let network_policy = network_policy.clone();
            move |input: IndexDocsInput| {
                let http = http.clone();
                let fs = fs.clone();
                let embedder = embedder.clone();
                let clock = clock.clone();
                let source_locks = source_locks.clone();
                let docs_index_dir = docs_index_dir.clone();
                let network_policy = network_policy.clone();
                async move {
                    docs::index_source(
                        &*http,
                        &*fs,
                        &*embedder,
                        &*clock,
                        &source_locks,
                        &docs_index_dir,
                        input,
                        network_policy,
                    )
                    .await
                }
            }
        }),
    );

    daemon.register(
        "stapler_search_docs",
        json_handler({
            let fs = fs.clone();
            let embedder = embedder.clone();
            let docs_index_dir = docs_index_dir.clone();
            move |input: SearchDocsInput| {
                let fs = fs.clone();
                let embedder = embedder.clone();
                let docs_index_dir = docs_index_dir.clone();
                async move { docs::search_docs(&*fs, &*embedder, &docs_index_dir, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_list_indexed_sources",
        json_handler({
            let fs = fs.clone();
            let docs_index_dir = docs_index_dir.clone();
            move |input: ListIndexedSourcesInput| {
                let fs = fs.clone();
                let docs_index_dir = docs_index_dir.clone();
                async move { docs::list_indexed_sources(&*fs, &docs_index_dir, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_remove_indexed_source",
        json_handler({
            let fs = fs.clone();
            let source_locks = source_locks.clone();
            let docs_index_dir = docs_index_dir.clone();
            move |input: RemoveIndexedSourceInput| {
                let fs = fs.clone();
                let source_locks = source_locks.clone();
                let docs_index_dir = docs_index_dir.clone();
                async move {
                    docs::remove_indexed_source(&*fs, &source_locks, &docs_index_dir, input).await
                }
            }
        }),
    );

    // Bridges the daemon's `!Send` core to a `Send`-bounded in-process
    // channel, so the HTTP server below can reach it without ever touching
    // `Rc<RefCell<...>>` state directly.
    let (bridge_tx, bridge_rx) =
        tokio::sync::mpsc::channel::<transport::BridgeMessage>(transport::BRIDGE_CHANNEL_CAPACITY);

    // `tokio::task::spawn`, not `spawn_local`: the HTTP server side must stay
    // `Send`-capable (the whole point of the bridge-channel architecture) —
    // it only ever touches the `Send` `bridge_tx` and axum/tokio types, never
    // the `!Send` `Rc<Daemon>` directly. `spawn` works fine from this
    // `current_thread` runtime too. The `JoinHandle` is joined into the
    // shutdown `tokio::join!` below.
    let http_server_handle = match http_port {
        Some(port) => {
            let http_token_path = paths::http_token_path(&env);
            match stapler_mcp_native::http_token::generate_or_load(&http_token_path).await {
                Ok((token, freshly_generated)) => {
                    if freshly_generated {
                        eprintln!(
                            "stapler-mcp: generated new HTTP bearer token at {http_token_path} (0600)"
                        );
                    }
                    eprintln!("stapler-mcp: HTTP transport listening on 127.0.0.1:{port}/mcp");
                    Some(tokio::task::spawn(http_server::run_http_server(
                        bridge_tx.clone(),
                        port,
                        token,
                        daemon.cancellation_token(),
                    )))
                }
                Err(e) => {
                    eprintln!(
                        "stapler-mcp: failed to generate/load HTTP bearer token at {http_token_path}: {e} — continuing with stdio/socket transport only"
                    );
                    None
                }
            }
        }
        None => {
            eprintln!("stapler-mcp: HTTP transport disabled (STAPLER_MCP_HTTP_PORT not set)");
            None
        }
    };

    // Runs on the `LocalSet` (not `tokio::spawn`) because it needs an
    // `Rc<Daemon>` clone alive across an `.await`. It does no cleanup itself
    // — just flips the shutdown flag/cancels the token — and completes in
    // the very same poll that fires cancellation (no `.await` after
    // `request_shutdown()`), so on this single-threaded cooperative runtime
    // it's guaranteed to finish before any of the three loops joined below
    // (each of which needs at least one more wakeup to notice cancellation)
    // resolve their own handles. That's why its own handle isn't explicitly
    // joined into the `tokio::join!` below.
    let sigterm_daemon = daemon.clone();
    tokio::task::spawn_local(async move {
        let mut sigterm =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("stapler-mcp: failed to install SIGTERM handler: {e}");
                    return;
                }
            };
        sigterm.recv().await;
        eprintln!("stapler-mcp: received SIGTERM, shutting down");
        sigterm_daemon.request_shutdown();
    });

    let bridge_daemon = daemon.clone();
    let bridge_cancel = bridge_daemon.cancellation_token();
    let bridge_consumer_handle = tokio::task::spawn_local(transport::run_bridge_consumer(
        bridge_daemon,
        bridge_rx,
        bridge_cancel,
    ));

    let socket = NativeSocketFactory;
    let cancel_token = daemon.cancellation_token();

    // One joined shutdown sequence for both shutdown triggers (SIGTERM and
    // the `SHUTDOWN_TOOL` RPC): both ultimately call `daemon.request_shutdown()`,
    // which cancels `cancel_token` and unblocks all three of these
    // concurrently. Each is raced against `await_with_shutdown_grace`, which
    // only starts its `SHUTDOWN_GRACE_TIMEOUT` countdown once `cancel_token`
    // actually fires — these three futures otherwise run for the daemon's
    // entire normal (potentially hours-long) operating lifetime, so a flat
    // `tokio::time::timeout` around each one would wrongly cut that lifetime
    // short instead of bounding only the post-shutdown wind-down. On a
    // timeout the underlying task is left running (not aborted) rather than
    // risking a partial-state abort mid-request — an accepted, documented
    // residual risk (see the timeout branches inside the helper).
    let (run_result, _bridge_result, _http_result) = tokio::join!(
        async {
            await_with_shutdown_grace(
                daemon.run_cancellable(&socket, &sock_path),
                &cancel_token,
                "accept loop",
            )
            .await
            .unwrap_or(Ok(()))
        },
        await_with_shutdown_grace(bridge_consumer_handle, &cancel_token, "bridge consumer"),
        async {
            if let Some(handle) = http_server_handle {
                await_with_shutdown_grace(handle, &cancel_token, "HTTP server").await;
            }
        },
    );

    shutdown_cleanup(daemon, browser).await;

    if let Err(e) = run_result {
        eprintln!("stapler-mcp: daemon run error: {e}");
        std::process::exit(1);
    }
}

/// Awaits `fut` to completion, unless `cancel` fires first and then
/// `SHUTDOWN_GRACE_TIMEOUT` elapses without `fut` resolving — in which case
/// this returns `None` and logs which task exceeded its grace period.
/// Bounds only the wind-down phase *after* shutdown is requested; before
/// `cancel` fires, `fut` is awaited with no timeout at all, since these
/// tasks are meant to run for the daemon's whole normal lifetime.
async fn await_with_shutdown_grace<F: std::future::Future>(
    fut: F,
    cancel: &tokio_util::sync::CancellationToken,
    label: &str,
) -> Option<F::Output> {
    tokio::pin!(fut);
    tokio::select! {
        result = &mut fut => Some(result),
        _ = async {
            cancel.cancelled().await;
            tokio::time::sleep(SHUTDOWN_GRACE_TIMEOUT).await;
        } => {
            eprintln!(
                "stapler-mcp: shutdown grace period ({SHUTDOWN_GRACE_TIMEOUT:?}) exceeded waiting on {label}, proceeding to cleanup anyway"
            );
            None
        }
    }
}

/// Post-shutdown cleanup, run exactly once after `run_daemon`'s joined
/// shutdown sequence completes, regardless of which of SIGTERM/`SHUTDOWN_TOOL`
/// triggered it. Drops `daemon` (and the `Rc<NativeBrowser>` clones its
/// handler closures held) so `browser` becomes the sole remaining reference,
/// making `Rc::get_mut` succeed — then explicitly closes it. Without this,
/// the Chrome subprocess and its CDP connection keep the process alive
/// forever after a clean shutdown.
async fn shutdown_cleanup(daemon: Rc<Daemon>, mut browser: Rc<NativeBrowser>) {
    drop(daemon);
    if let Some(inner) = Rc::get_mut(&mut browser) {
        // Abort the session-idle reaper before closing the browser: a
        // still-running reaper mid-scan could otherwise race
        // `NativeBrowser::close()`. `handle.await`ing an aborted task
        // returns `Err` with `is_cancelled() == true`; any other `Err` means
        // the reaper panicked rather than being cleanly cancelled, which is
        // worth logging since it would otherwise go unnoticed (Story 2.4).
        let reaper_handle = inner.reaper.borrow_mut().take();
        if let Some(handle) = reaper_handle {
            handle.abort();
            if let Err(e) = handle.await {
                if !e.is_cancelled() {
                    eprintln!("stapler-mcp: session idle reaper task panicked: {e}");
                }
            }
        }
        inner.close().await;
    } else {
        // Every task that could hold an `Rc<NativeBrowser>` clone has already
        // exited by the time the shutdown `tokio::join!` completes (see this
        // function's doc comment), so this should be unreachable — logged
        // rather than silently skipped in case that invariant ever breaks.
        eprintln!(
            "stapler-mcp: shutdown_cleanup: browser still has {} references, skipping close()",
            Rc::strong_count(&browser)
        );
    }
}

/// Epic 6.1 / Story 6.1.1: validation.md's
/// `daemon_should_return_vault_not_configured_error_when_op_service_account_token_unset`
/// row is classified Unit — reachable here without a live `NativeBrowser`
/// (which needs a real Chromium binary to construct at all) because
/// `vault_not_configured_handler` never references `browser`. This routes
/// the handler through a real `Daemon` (not called directly) so the
/// assertion also proves the tool is actually *registered*, not merely
/// constructible. The token-present path (`NativeBrowser::type_secret`
/// reaching the injected `NativeCredentialStore::resolve`) needs a real
/// daemon subprocess + Chromium + the `op` CLI, so it lives in
/// `crates/cli/tests/browser_session.rs` instead, alongside this file's
/// other `#[ignore]`d real-daemon integration tests.
#[cfg(test)]
mod credential_wiring_tests {
    use super::*;

    #[tokio::test]
    async fn daemon_should_return_vault_not_configured_error_when_op_service_account_token_unset() {
        let daemon = Daemon::new();
        daemon.register(
            "stapler_browser_type_secret",
            vault_not_configured_handler(),
        );

        let request = serde_json::json!({
            "tool": "stapler_browser_type_secret",
            "params": {
                "sessionId": "sess-1",
                "refId": "e1",
                "credential": { "domain": "example.com", "field": "password" }
            }
        });
        let bytes = daemon
            .handle_request_bytes(request.to_string().as_bytes())
            .await;
        let resp: serde_json::Value =
            serde_json::from_slice(&bytes).expect("daemon response should be valid JSON");

        assert_eq!(
            resp["error"].as_str(),
            Some(VAULT_NOT_CONFIGURED_MESSAGE),
            "got: {resp:?}"
        );
        assert!(
            resp.get("result").is_none() || resp["result"].is_null(),
            "got: {resp:?}"
        );
    }
}
