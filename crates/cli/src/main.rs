mod thin_client;

use std::rc::Rc;

use rmcp::{transport::stdio, ServiceExt};

use stapler_mcp_core::daemon::{json_handler, Daemon};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::{LockError, LockGuard, ProcessLock};
use stapler_mcp_core::schema::{
    BraveSearchInput, BrowserClickInput, BrowserCloseAllSessionsInput, BrowserCloseSessionInput,
    BrowserHoverInput, BrowserListSessionsInput, BrowserNavigateInput, BrowserPressKeyInput,
    BrowserSelectOptionInput, BrowserSnapshotInput, BrowserTabsInput, BrowserTypeInput,
    BrowserWaitForInput, DownloadWebsiteInput, FetchPageInput, IndexDocsInput,
    ListIndexedSourcesInput, ReadWebsiteInput, RemoveIndexedSourceInput, SearchDocsInput,
};
use stapler_mcp_core::tools::{browser, docs, fetch, search, webcrawl};
use stapler_mcp_native::{
    NativeBrowser, NativeClock, NativeEmbedder, NativeEnv, NativeFs, NativeHttp, NativeLock,
    NativeSocketFactory,
};

fn main() {
    let is_daemon = std::env::args().any(|a| a == "--daemon");

    // Deliberately single-threaded: this daemon's work is I/O-bound, not
    // CPU-bound, and a `current_thread` runtime + `LocalSet` lets the exact
    // same core (no `Send` bounds anywhere) also satisfy a `!Send`
    // wasm-bindgen adapter later, without ever needing `async-trait`.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime");
    let local = tokio::task::LocalSet::new();

    if is_daemon {
        local.block_on(&rt, run_daemon());
    } else {
        local.block_on(&rt, run_thin_client());
    }
}

async fn run_thin_client() {
    let client = thin_client::ThinClient::new();
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
    let mut browser = match NativeBrowser::launch().await {
        Ok(b) => Rc::new(b),
        Err(e) => {
            eprintln!("stapler-mcp: failed to launch browser: {e}");
            std::process::exit(1);
        }
    };
    let embedder = Rc::new(NativeEmbedder::new(paths::embedding_cache_dir(&env)));
    let clock = Rc::new(NativeClock);
    let source_locks = Rc::new(docs::SourceLocks::new());
    let docs_index_dir = paths::docs_index_dir(&env);

    let daemon = Daemon::new();

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
    let network_policy = webcrawl::NetworkPolicy::from_env(stapler_mcp_core::ports::EnvPort::var(
        &env,
        "STAPLER_MCP_ALLOW_PRIVATE_NETWORKS",
    ));
    daemon.register(
        "read_website",
        json_handler({
            let http = http.clone();
            let fs = fs.clone();
            move |input: ReadWebsiteInput| {
                let http = http.clone();
                let fs = fs.clone();
                let cache_dir = cache_dir.clone();
                async move {
                    webcrawl::read_website(&*http, &*fs, &cache_dir, input, network_policy).await
                }
            }
        }),
    );

    daemon.register(
        "download_website",
        json_handler({
            let http = http.clone();
            let fs = fs.clone();
            move |input: DownloadWebsiteInput| {
                let http = http.clone();
                let fs = fs.clone();
                async move { webcrawl::download_website(&*http, &*fs, input, network_policy).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_navigate",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserNavigateInput| {
                let browser = browser.clone();
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
            move |input: BrowserTabsInput| {
                let browser = browser.clone();
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
        "stapler_index_docs",
        json_handler({
            let http = http.clone();
            let fs = fs.clone();
            let embedder = embedder.clone();
            let clock = clock.clone();
            let source_locks = source_locks.clone();
            let docs_index_dir = docs_index_dir.clone();
            move |input: IndexDocsInput| {
                let http = http.clone();
                let fs = fs.clone();
                let embedder = embedder.clone();
                let clock = clock.clone();
                let source_locks = source_locks.clone();
                let docs_index_dir = docs_index_dir.clone();
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

    let socket = NativeSocketFactory;
    let run_result = daemon.run(&socket, &sock_path).await;

    // Drop the daemon (and the `Rc<NativeBrowser>` clones its handler
    // closures held) so `browser` becomes the sole remaining reference,
    // making `Rc::get_mut` succeed — then explicitly close it. Without this,
    // the Chrome subprocess and its CDP connection keep the process alive
    // forever after a clean `shutdown`, whatever `daemon.run` returned.
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
    }

    if let Err(e) = run_result {
        eprintln!("stapler-mcp: daemon run error: {e}");
        std::process::exit(1);
    }
}
