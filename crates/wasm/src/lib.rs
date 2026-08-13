mod browser;
mod clock;
mod env;
mod fs;
mod http;
mod js_util;
mod lock;
mod process;
mod socket;

use std::rc::Rc;
use std::time::Duration;

use wasm_bindgen::prelude::*;

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::daemon::{json_handler, Daemon};
use stapler_mcp_core::paths;
use stapler_mcp_core::ports::{EnvPort, LockError, LockGuard, ProcessLock};
use stapler_mcp_core::schema::{
    BraveSearchInput, BraveSearchOutput, BrowserActionOutput, BrowserClickInput,
    BrowserCloseSessionInput, BrowserCloseSessionOutput, BrowserHoverInput,
    BrowserNavigateInput, BrowserNavigateOutput, BrowserPressKeyInput, BrowserSelectOptionInput,
    BrowserSnapshotInput, BrowserTabsInput, BrowserTabsOutput, BrowserTypeInput,
    BrowserWaitForInput, DownloadWebsiteInput, DownloadWebsiteOutput, FetchPageInput,
    FetchPageOutput, ReadWebsiteInput, ReadWebsiteOutput,
};
use stapler_mcp_core::tools::{browser as browser_tools, fetch, search, webcrawl};

#[wasm_bindgen]
pub async fn run_daemon() -> Result<(), JsValue> {
    let env = env::WasmEnv;
    let base = paths::base_dir(&env);
    let sock_path = paths::socket_path(&env);
    let lock_path = paths::lock_path(&env);

    fs::js_ensure_dir(&base);

    let lock = lock::WasmLock;
    let mut guard = match lock.acquire_exclusive(&lock_path).await {
        Ok(g) => g,
        Err(LockError::AlreadyRunning) => return Ok(()),
        Err(LockError::Other(e)) => return Err(JsValue::from_str(&e)),
    };
    guard.write_pid(0);

    let http = Rc::new(http::WasmHttp);
    let fsstore = Rc::new(fs::WasmFs);
    let browser = Rc::new(browser::WasmBrowser);

    let daemon = Daemon::new();

    daemon.register(
        "fetch_page",
        json_handler({
            let browser = browser.clone();
            let fsstore = fsstore.clone();
            move |input: FetchPageInput| {
                let browser = browser.clone();
                let fsstore = fsstore.clone();
                async move { fetch::fetch_page(&*browser, &*fsstore, input).await }
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
                    let env = env::WasmEnv;
                    let api_key = env.var("BRAVE_API_KEY").unwrap_or_default();
                    let base_url = env
                        .var("BRAVE_API_BASE_URL")
                        .unwrap_or_else(|| search::DEFAULT_BASE_URL.to_string());
                    search::brave_web_search(&*http, &api_key, &base_url, input).await
                }
            }
        }),
    );

    let cache_dir = paths::cache_dir(&env);
    // Only ever set by this crate's own tests — see `NetworkPolicy`'s doc
    // comment in `crates/core`.
    let network_policy =
        webcrawl::NetworkPolicy::from_env(env.var("STAPLER_MCP_ALLOW_PRIVATE_NETWORKS"));
    daemon.register(
        "read_website",
        json_handler({
            let http = http.clone();
            let fsstore = fsstore.clone();
            move |input: ReadWebsiteInput| {
                let http = http.clone();
                let fsstore = fsstore.clone();
                let cache_dir = cache_dir.clone();
                async move {
                    webcrawl::read_website(&*http, &*fsstore, &cache_dir, input, network_policy)
                        .await
                }
            }
        }),
    );

    daemon.register(
        "download_website",
        json_handler({
            let http = http.clone();
            let fsstore = fsstore.clone();
            move |input: DownloadWebsiteInput| {
                let http = http.clone();
                let fsstore = fsstore.clone();
                async move {
                    webcrawl::download_website(&*http, &*fsstore, input, network_policy).await
                }
            }
        }),
    );

    daemon.register(
        "stapler_browser_navigate",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserNavigateInput| {
                let browser = browser.clone();
                async move {
                    browser_tools::browser_navigate(&*browser, input, network_policy).await
                }
            }
        }),
    );

    daemon.register(
        "stapler_browser_click",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserClickInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_click(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_type",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserTypeInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_type(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_snapshot",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserSnapshotInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_snapshot(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_close_session",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserCloseSessionInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_close_session(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_tabs",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserTabsInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_tabs(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_hover",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserHoverInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_hover(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_select_option",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserSelectOptionInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_select_option(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_press_key",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserPressKeyInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_press_key(&*browser, input).await }
            }
        }),
    );

    daemon.register(
        "stapler_browser_wait_for",
        json_handler({
            let browser = browser.clone();
            move |input: BrowserWaitForInput| {
                let browser = browser.clone();
                async move { browser_tools::browser_wait_for(&*browser, input).await }
            }
        }),
    );

    let socket = socket::WasmSocketFactory;
    let run_result = daemon.run(&socket, &sock_path).await;
    // Always close the browser on the way out, even on error — otherwise the
    // Chrome subprocess (and Node's CDP connection to it) keeps the event
    // loop alive forever, whatever `daemon.run` returned.
    browser.close().await;
    run_result.map_err(|e| JsValue::from_str(&e.to_string()))
}

#[wasm_bindgen]
pub async fn ensure_daemon_and_call(
    tool: String,
    params_json: String,
    exe_hint: Option<String>,
) -> Result<String, JsValue> {
    let env = env::WasmEnv;
    let socket = socket::WasmSocketFactory;
    let spawner = process::WasmSpawner;
    let sleeper = clock::WasmSleeper;
    let clock = clock::WasmClock;

    let sock_path = paths::socket_path(&env);
    let log_path = paths::log_path(&env);

    client::ensure_daemon(
        &socket,
        &spawner,
        &sleeper,
        &clock,
        &sock_path,
        &log_path,
        EnsureOptions {
            startup_timeout: None,
            exe_hint,
        },
    )
    .await
    .map_err(|e| JsValue::from_str(&format!("ensure daemon: {e}")))?;

    let params: Option<serde_json::Value> = if params_json.is_empty() {
        None
    } else {
        Some(serde_json::from_str(&params_json).map_err(|e| JsValue::from_str(&e.to_string()))?)
    };

    let result = client::call(&socket, &sock_path, &tool, params, Duration::from_secs(120))
        .await
        .map_err(|e| JsValue::from_str(&e.to_string()))?;
    serde_json::to_string(&result).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolDescriptor {
    name: &'static str,
    description: &'static str,
    input_schema: serde_json::Value,
    output_schema: serde_json::Value,
}

/// One authored schema (via `schemars` on the shared core types) reused here
/// instead of hand-authoring a second definition on the Node/TS side.
#[wasm_bindgen]
pub fn list_tools_json() -> Result<String, JsValue> {
    let tools = vec![
        ToolDescriptor {
            name: "fetch_page",
            description: "Render a URL in a headless browser and return its title and extracted text (optionally saving the rendered HTML to a local file). Backed by the shared stapler-mcp daemon's browser pool.",
            input_schema: serde_json::to_value(schemars::schema_for!(FetchPageInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(FetchPageOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "brave_web_search",
            description: "Search the web via the Brave Search API. Requires BRAVE_API_KEY in the daemon's environment.",
            input_schema: serde_json::to_value(schemars::schema_for!(BraveSearchInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BraveSearchOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "read_website",
            description: "Fetch a URL (optionally crawling same-host links up to maxDepth/maxPages), extract the main content via Readability-style extraction, and return it as Markdown. Cached by URL on the daemon.",
            input_schema: serde_json::to_value(schemars::schema_for!(ReadWebsiteInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(ReadWebsiteOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "download_website",
            description: "Fetch a URL (optionally crawling same-host links up to maxDepth/maxPages) and save each page's raw HTML under saveDir.",
            input_schema: serde_json::to_value(schemars::schema_for!(DownloadWebsiteInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(DownloadWebsiteOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_navigate",
            description: "Navigate a persistent browser session (starting a new one if sessionId is omitted) and return an accessibility-tree snapshot of the resulting page.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserNavigateInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserNavigateOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_click",
            description: "Click an element (identified by a ref from a previous snapshot) in an existing browser session and return the resulting accessibility-tree snapshot.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserClickInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserActionOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_type",
            description: "Type text into an element (identified by a ref from a previous snapshot) in an existing browser session and return the resulting accessibility-tree snapshot.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserTypeInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserActionOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_snapshot",
            description: "Capture a fresh accessibility-tree snapshot of an existing browser session's current page without mutating it.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserSnapshotInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserActionOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_close_session",
            description: "Close an existing browser session and release its resources. Idempotent-in-effect: closing an already-closed session still reports success.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserCloseSessionInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserCloseSessionOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_tabs",
            description: "List, open, switch to, or close tabs in an existing browser session, via the `action` field (list/new/select/close). `select`/`close` take a tab `index`; `new` optionally takes a `url` to navigate the new tab to. Mirrors @playwright/mcp's browser_tabs.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserTabsInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserTabsOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_hover",
            description: "Hover the pointer over an element (identified by a ref from a previous snapshot) in an existing browser session and return the resulting accessibility-tree snapshot.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserHoverInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserActionOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_select_option",
            description: "Select one or more options in a <select> element (identified by a ref from a previous snapshot) in an existing browser session and return the resulting accessibility-tree snapshot.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserSelectOptionInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserActionOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_press_key",
            description: "Press a keyboard key (optionally focusing an element first via a ref from a previous snapshot) in an existing browser session and return the resulting accessibility-tree snapshot.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserPressKeyInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserActionOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
        ToolDescriptor {
            name: "stapler_browser_wait_for",
            description: "Wait in an existing browser session until given text appears, given text disappears, and/or a minimum time elapses, then return the resulting accessibility-tree snapshot.",
            input_schema: serde_json::to_value(schemars::schema_for!(BrowserWaitForInput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
            output_schema: serde_json::to_value(schemars::schema_for!(BrowserActionOutput))
                .map_err(|e| JsValue::from_str(&e.to_string()))?,
        },
    ];
    serde_json::to_string(&tools).map_err(|e| JsValue::from_str(&e.to_string()))
}

#[cfg(test)]
mod list_tools_tests {
    use super::list_tools_json;

    #[test]
    fn list_tools_json_should_contain_ten_browser_tool_descriptors_when_called() {
        let json = list_tools_json().expect("list_tools_json should succeed");
        let tools: serde_json::Value =
            serde_json::from_str(&json).expect("list_tools_json output should be valid JSON");
        let names: Vec<&str> = tools
            .as_array()
            .expect("tools should be a JSON array")
            .iter()
            .map(|t| t["name"].as_str().expect("name should be a string"))
            .collect();

        let browser_tool_names = [
            "stapler_browser_navigate",
            "stapler_browser_click",
            "stapler_browser_type",
            "stapler_browser_snapshot",
            "stapler_browser_close_session",
            "stapler_browser_tabs",
            "stapler_browser_hover",
            "stapler_browser_select_option",
            "stapler_browser_press_key",
            "stapler_browser_wait_for",
        ];
        let matched: Vec<&&str> = browser_tool_names
            .iter()
            .filter(|name| names.contains(*name))
            .collect();

        assert_eq!(
            matched.len(),
            10,
            "expected exactly 10 browser tool descriptors, found {matched:?} in {names:?}"
        );
    }
}
