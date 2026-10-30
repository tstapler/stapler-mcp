//! The MCP tool router Claude Code actually launches (over stdio, and later
//! Streamable HTTP). Holds no heavyweight state itself — every tool call
//! proxies to the shared daemon via a `DaemonTransport`, auto-starting the
//! daemon on first use for the default `SocketTransport`.

use std::collections::HashSet;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, Json, ServerHandler};

use crate::transport::{DaemonTransport, SocketTransport};
use stapler_mcp_core::schema::{
    BraveSearchInput, BraveSearchOutput, BrowserActionOutput, BrowserClickInput,
    BrowserCloseAllSessionsInput, BrowserCloseAllSessionsOutput, BrowserCloseSessionInput,
    BrowserCloseSessionOutput, BrowserEvaluateInput, BrowserEvaluateOutput, BrowserFillFormInput,
    BrowserFindInput, BrowserFindOutput, BrowserGetHtmlInput, BrowserGetHtmlOutput,
    BrowserHistoryInput, BrowserHoverInput, BrowserListSessionsInput, BrowserListSessionsOutput,
    BrowserNavigateInput, BrowserNavigateOutput, BrowserNetworkRequestsInput,
    BrowserNetworkRequestsOutput, BrowserPdfInput, BrowserPdfOutput, BrowserPressKeyInput,
    BrowserResizeInput, BrowserScreenshotInput, BrowserScreenshotOutput, BrowserSelectOptionInput,
    BrowserSetCheckedInput, BrowserSnapshotInput, BrowserTabsInput, BrowserTabsOutput,
    BrowserTypeInput, BrowserTypeSecretInput, BrowserWaitForInput, DaemonStatusInput,
    DaemonStatusOutput, DownloadWebsiteInput, DownloadWebsiteOutput, FetchPageInput,
    FetchPageOutput, IndexDocsInput, IndexDocsOutput, ListIndexedSourcesInput,
    ListIndexedSourcesOutput, ReadSavedPageInput, ReadSavedPageOutput, ReadWebsiteInput,
    ReadWebsiteOutput, RemoveIndexedSourceInput, RemoveIndexedSourceOutput, SearchDocsInput,
    SearchDocsOutput,
};
/// `STAPLER_MCP_TOOL_PROFILE` (issue #18): a comma-separated list of
/// categories below, narrowing which tools a session's `tools/list` and
/// `tools/call` expose — unset, empty, or `all` keeps today's behavior
/// (every tool registered). Read via raw `std::env::var`, following
/// `STAPLER_MCP_HTTP_PORT`'s precedent (`main.rs`'s `run_daemon()`) rather
/// than the `EnvPort`-trait convention `STAPLER_MCP_BROWSER_PROFILE_DIR` uses.
const ENV_TOOL_PROFILE: &str = "STAPLER_MCP_TOOL_PROFILE";

/// Every registered tool name, grouped by category. A tool must appear in
/// exactly one group — `tools_outside_profile`'s tests catch a name that's
/// missing or duplicated against the real `#[tool]` list below.
const TOOL_CATEGORIES: &[(&str, &[&str])] = &[
    (
        "web",
        &[
            "fetch_page",
            "brave_web_search",
            "browser_web_search",
            "read_website",
            "read_saved_page",
            "download_website",
        ],
    ),
    (
        "browser-core",
        &[
            "stapler_browser_navigate",
            "stapler_browser_click",
            "stapler_browser_type",
            "stapler_browser_snapshot",
            "stapler_browser_close_session",
            "stapler_browser_close_all_sessions",
            "stapler_browser_list_sessions",
            "stapler_browser_tabs",
        ],
    ),
    (
        "browser-advanced",
        &[
            "stapler_browser_type_secret",
            "stapler_browser_hover",
            "stapler_browser_select_option",
            "stapler_browser_press_key",
            "stapler_browser_wait_for",
            "stapler_browser_screenshot",
            "stapler_browser_pdf",
            "stapler_browser_evaluate",
            "stapler_browser_get_html",
            "stapler_browser_network_requests",
            "stapler_browser_fill_form",
            "stapler_browser_set_checked",
            "stapler_browser_history",
            "stapler_browser_resize",
            "stapler_browser_find",
        ],
    ),
    (
        "docs",
        &[
            "stapler_index_docs",
            "stapler_search_docs",
            "stapler_list_indexed_sources",
            "stapler_remove_indexed_source",
        ],
    ),
    ("daemon", &["stapler_daemon_status"]),
];

/// Resolves `STAPLER_MCP_TOOL_PROFILE` into tool names to hide. `None`,
/// empty, or `all` (case-insensitive) disables nothing. An unknown token is
/// warned and skipped; if every token is unknown, falls back to "all" rather
/// than disabling everything (a typo'd env var shouldn't lock out every tool).
fn tools_outside_profile(spec: Option<&str>) -> Vec<&'static str> {
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    if spec.eq_ignore_ascii_case("all") {
        return Vec::new();
    }

    let mut wanted: HashSet<&'static str> = HashSet::new();
    for token in spec.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        match TOOL_CATEGORIES
            .iter()
            .find(|(category, _)| category.eq_ignore_ascii_case(token))
        {
            Some((category, _)) => {
                wanted.insert(category);
            }
            None => eprintln!(
                "stapler-mcp: {ENV_TOOL_PROFILE} has unknown category '{token}', ignoring it"
            ),
        }
    }
    if wanted.is_empty() {
        eprintln!(
            "stapler-mcp: {ENV_TOOL_PROFILE}='{spec}' matched no known category, falling back to all tools"
        );
        return Vec::new();
    }

    TOOL_CATEGORIES
        .iter()
        .filter(|(category, _)| !wanted.contains(category))
        .flat_map(|(_, names)| names.iter().copied())
        .collect()
}

/// Disables every tool `spec` excludes (see `tools_outside_profile`) on
/// `router` — shared by `McpRouter::new()` and `::with_transport()` so both
/// the stdio and per-HTTP-session construction paths respect the same
/// profile. Takes the resolved spec rather than reading the env var itself,
/// so tests can inject a value without mutating process-global state.
fn apply_tool_profile<S: Send + Sync + 'static>(
    router: &mut rmcp::handler::server::router::tool::ToolRouter<S>,
    spec: Option<&str>,
) {
    for name in tools_outside_profile(spec) {
        router.disable_route(name);
    }
}

#[derive(Debug, Clone)]
pub struct McpRouter<T: DaemonTransport = SocketTransport> {
    transport: T,
    // `#[tool_handler]` below reads this field from its macro-generated
    // `call_tool` impl, which rustc's dead-code analysis doesn't see through —
    // verified working end-to-end (tools/list and tools/call both dispatch
    // correctly), so the "never read" warning here is a known false positive.
    #[allow(dead_code)]
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl McpRouter<SocketTransport> {
    pub fn new() -> Self {
        let mut tool_router = Self::tool_router();
        apply_tool_profile(
            &mut tool_router,
            std::env::var(ENV_TOOL_PROFILE).ok().as_deref(),
        );
        Self {
            transport: SocketTransport,
            tool_router,
        }
    }
}

impl Default for McpRouter<SocketTransport> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: DaemonTransport + Send + Sync + 'static> McpRouter<T> {
    /// Constructs a router over a non-default transport — used by
    /// `http_server::build_mcp_service` to build a fresh
    /// `McpRouter<ChannelTransport>` per HTTP session.
    pub fn with_transport(transport: T) -> Self {
        let mut tool_router = Self::tool_router();
        apply_tool_profile(
            &mut tool_router,
            std::env::var(ENV_TOOL_PROFILE).ok().as_deref(),
        );
        Self {
            transport,
            tool_router,
        }
    }
}

#[tool_router]
impl<T: DaemonTransport + Send + Sync + 'static> McpRouter<T> {
    #[tool(
        name = "fetch_page",
        description = "Render a URL in a headless browser and return its title and extracted text (optionally saving the rendered HTML to a local file). Backed by the shared stapler-mcp daemon's browser pool."
    )]
    async fn fetch_page(
        &self,
        params: Parameters<FetchPageInput>,
    ) -> Result<Json<FetchPageOutput>, String> {
        let result = self
            .transport
            .call(
                "fetch_page",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "brave_web_search",
        description = "Search the web via the Brave Search API. Requires BRAVE_API_KEY in the daemon's environment."
    )]
    async fn brave_web_search(
        &self,
        params: Parameters<BraveSearchInput>,
    ) -> Result<Json<BraveSearchOutput>, String> {
        let result = self
            .transport
            .call(
                "brave_web_search",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "browser_web_search",
        description = "Keyless fallback for brave_web_search: renders a Bing search results page in a throwaway browser session and scrapes the organic results. Same input/output shape as brave_web_search. Opens and closes exactly one session per call."
    )]
    async fn browser_web_search(
        &self,
        params: Parameters<BraveSearchInput>,
    ) -> Result<Json<BraveSearchOutput>, String> {
        let result = self
            .transport
            .call(
                "browser_web_search",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "read_website",
        description = "Fetch a URL (optionally crawling same-host links up to maxDepth/maxPages), extract the main content via Readability-style extraction, and return it as Markdown. Cached by URL on the daemon. Once the combined Markdown across all pages returned passes ~60,000 characters (tunable via maxInlineChars, or force it for every page with alwaysSaveToFile), further pages come back as a short preview plus savedPath instead — use read_saved_page to search or page through the full content."
    )]
    async fn read_website(
        &self,
        params: Parameters<ReadWebsiteInput>,
    ) -> Result<Json<ReadWebsiteOutput>, String> {
        let result = self
            .transport
            .call(
                "read_website",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "read_saved_page",
        description = "Search or page through a page's full Markdown previously saved by read_website (its savedPath). With query set, returns every matching line (case-insensitive) plus surrounding context, like grep -n -C; without it, returns a line-numbered page of content starting at offset. Works even when you're not on the same machine as the daemon."
    )]
    async fn read_saved_page(
        &self,
        params: Parameters<ReadSavedPageInput>,
    ) -> Result<Json<ReadSavedPageOutput>, String> {
        let result = self
            .transport
            .call(
                "read_saved_page",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "download_website",
        description = "Fetch a URL (optionally crawling same-host links up to maxDepth/maxPages) and save each page's raw HTML under saveDir."
    )]
    async fn download_website(
        &self,
        params: Parameters<DownloadWebsiteInput>,
    ) -> Result<Json<DownloadWebsiteOutput>, String> {
        let result = self
            .transport
            .call(
                "download_website",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_navigate",
        description = "Navigate to a URL in a real browser session (creating a new session if sessionId is omitted, or reusing an existing one), returning the resolved session id, the final URL after any redirects, and an accessibility-tree snapshot of the resulting page. Part of the playwright-mcp-style browser automation tools backed by the shared daemon's browser pool."
    )]
    async fn browser_navigate(
        &self,
        params: Parameters<BrowserNavigateInput>,
    ) -> Result<Json<BrowserNavigateOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_navigate",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_click",
        description = "Click an element in an existing browser session, identified by a `ref` from a previous snapshot, and return the accessibility-tree snapshot of the page after the click."
    )]
    async fn browser_click(
        &self,
        params: Parameters<BrowserClickInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_click",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_type",
        description = "Type text into an element in an existing browser session, identified by a `ref` from a previous snapshot, and return the accessibility-tree snapshot of the page after typing."
    )]
    async fn browser_type(
        &self,
        params: Parameters<BrowserTypeInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_type",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_type_secret",
        description = "Use this instead of stapler_browser_type whenever a field is a password, TOTP/2FA code, or other secret you have a stored credential for. Types a credential resolved server-side from the daemon's configured vault into an element in an existing browser session, identified by a `ref` from a previous snapshot — the credential value never appears in this tool's request or in any returned accessibility-tree snapshot (a fixed [REDACTED] placeholder takes its place). Returns the accessibility-tree snapshot after typing, same as stapler_browser_type, with note set to confirm success since the visible value won't change to show it."
    )]
    async fn browser_type_secret(
        &self,
        params: Parameters<BrowserTypeSecretInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_type_secret",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_snapshot",
        description = "Capture a fresh accessibility-tree snapshot of an existing browser session's current page, without performing any action."
    )]
    async fn browser_snapshot(
        &self,
        params: Parameters<BrowserSnapshotInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_snapshot",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_close_session",
        description = "Close an existing browser session and release its resources (the underlying browser tab/context). Safe to call even if the session is already gone."
    )]
    async fn browser_close_session(
        &self,
        params: Parameters<BrowserCloseSessionInput>,
    ) -> Result<Json<BrowserCloseSessionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_close_session",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_list_sessions",
        description = "List every active browser session with its tab count and idle time in milliseconds."
    )]
    async fn browser_list_sessions(
        &self,
        params: Parameters<BrowserListSessionsInput>,
    ) -> Result<Json<BrowserListSessionsOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_list_sessions",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_close_all_sessions",
        description = "Close every active browser session, best-effort. A failure closing one session does not stop the others. Returns the ids that closed successfully and the ids that failed with their error messages."
    )]
    async fn browser_close_all_sessions(
        &self,
        params: Parameters<BrowserCloseAllSessionsInput>,
    ) -> Result<Json<BrowserCloseAllSessionsOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_close_all_sessions",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_tabs",
        description = "Manage tabs within an existing browser session via the `action` field: `list` returns every open tab with its index/url/title; `new` opens a tab (optionally navigating it to `url`) and switches to it; `select` switches the active tab to the given `index`; `close` closes the tab at `index` (or the active tab if omitted). Returns the current tab list, the active index, and a snapshot of the now-active page. Mirrors @playwright/mcp's browser_tabs."
    )]
    async fn browser_tabs(
        &self,
        params: Parameters<BrowserTabsInput>,
    ) -> Result<Json<BrowserTabsOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_tabs",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_hover",
        description = "Move the mouse pointer over an element in an existing browser session, identified by a `ref` from a previous snapshot (useful for triggering hover-revealed UI), and return the accessibility-tree snapshot of the page afterward."
    )]
    async fn browser_hover(
        &self,
        params: Parameters<BrowserHoverInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_hover",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_select_option",
        description = "Select one or more options in a <select> element in an existing browser session, identified by a `ref` from a previous snapshot, and return the accessibility-tree snapshot of the page after selecting."
    )]
    async fn browser_select_option(
        &self,
        params: Parameters<BrowserSelectOptionInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_select_option",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_press_key",
        description = "Press a single keyboard key (e.g. \"Enter\", \"Escape\", \"ArrowDown\") in an existing browser session, optionally focusing an element first via a `ref` from a previous snapshot, and return the accessibility-tree snapshot of the page afterward."
    )]
    async fn browser_press_key(
        &self,
        params: Parameters<BrowserPressKeyInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_press_key",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_wait_for",
        description = "Wait in an existing browser session until specified `text` appears, `textGone` disappears, and/or at least `timeMs` elapses, then return the accessibility-tree snapshot of the page at that point. Useful for waiting out async page updates before the next action."
    )]
    async fn browser_wait_for(
        &self,
        params: Parameters<BrowserWaitForInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_wait_for",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_screenshot",
        description = "Capture a pixel screenshot (PNG) of an existing browser session's current page — the current viewport by default, or the full scrollable page with fullPage: true. Returns base64-encoded image data, or saves it to savePath if given (omitting the inline data)."
    )]
    async fn browser_screenshot(
        &self,
        params: Parameters<BrowserScreenshotInput>,
    ) -> Result<Json<BrowserScreenshotOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_screenshot",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_pdf",
        description = "Save an existing browser session's current page as a PDF (CDP Page.printToPDF / Playwright page.pdf()). Returns base64-encoded PDF data, or saves it to savePath if given (omitting the inline data). Sibling to stapler_browser_screenshot for PDF output instead of PNG."
    )]
    async fn browser_pdf(
        &self,
        params: Parameters<BrowserPdfInput>,
    ) -> Result<Json<BrowserPdfOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_pdf",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_evaluate",
        description = "Run a JS function (e.g. \"() => document.title\") in an existing browser session's current page and return its result as JSON. Pass refId (a `ref` from a previous snapshot) to call the function with that element as its argument (e.g. \"(element) => element.value\") instead of running at page scope."
    )]
    async fn browser_evaluate(
        &self,
        params: Parameters<BrowserEvaluateInput>,
    ) -> Result<Json<BrowserEvaluateOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_evaluate",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_get_html",
        description = "Return the rendered HTML of an existing browser session's current page (document.documentElement.outerHTML), or of a single element's outerHTML when refId (a `ref` from a previous snapshot) is given. Complements stapler_browser_snapshot's accessibility-tree view when the exact markup is what's needed."
    )]
    async fn browser_get_html(
        &self,
        params: Parameters<BrowserGetHtmlInput>,
    ) -> Result<Json<BrowserGetHtmlOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_get_html",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_fill_form",
        description = "Fill multiple fields in an existing browser session in one call instead of one stapler_browser_type/stapler_browser_select_option call per field. Each field names a `ref` from a previous snapshot, a type (\"textbox\" or \"combobox\"), and a value. Returns the accessibility-tree snapshot after the last field is filled."
    )]
    async fn browser_fill_form(
        &self,
        params: Parameters<BrowserFillFormInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_fill_form",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_set_checked",
        description = "Set a checkbox or radio button in an existing browser session, identified by a `ref` from a previous snapshot, to an explicit checked state. Only clicks if the element's current state differs from the requested one. Returns the accessibility-tree snapshot afterward."
    )]
    async fn browser_set_checked(
        &self,
        params: Parameters<BrowserSetCheckedInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_set_checked",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_history",
        description = "Navigate an existing browser session's current tab back or forward through its history, or reload it in place, via the `action` field (\"back\", \"forward\", or \"reload\"). Returns the accessibility-tree snapshot after the navigation completes."
    )]
    async fn browser_history(
        &self,
        params: Parameters<BrowserHistoryInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_history",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_resize",
        description = "Resize an existing browser session's current tab's viewport to width x height (CSS pixels). Returns the accessibility-tree snapshot afterward, since a resize can change what's visible/laid out."
    )]
    async fn browser_resize(
        &self,
        params: Parameters<BrowserResizeInput>,
    ) -> Result<Json<BrowserActionOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_resize",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_network_requests",
        description = "List the network requests (url, method, status, contentType, text body, timingMs) an existing browser session made since its last navigation — useful for debugging a page that didn't load expected content, or reading data that only appears in an XHR/fetch response. Optional filterUrl keeps only URLs containing that substring; clear: true returns the entries and then empties the list; savePath also writes a HAR 1.2 file (no header data). Bodies are text only (binary types are null) and capped at maxBodyBytes (default and max 16384). At most 1000 entries per navigation."
    )]
    async fn browser_network_requests(
        &self,
        params: Parameters<BrowserNetworkRequestsInput>,
    ) -> Result<Json<BrowserNetworkRequestsOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_network_requests",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_browser_find",
        description = "Search an existing browser session's current page for accessibility-tree nodes whose name (visible/accessible text) contains query (case-insensitive), without capturing a full stapler_browser_snapshot. Returns each match's ref, role, name, and its role path from the tree root — cheaper than a full snapshot when you only need to locate one element's ref."
    )]
    async fn browser_find(
        &self,
        params: Parameters<BrowserFindInput>,
    ) -> Result<Json<BrowserFindOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_browser_find",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_index_docs",
        description = "Crawl a URL (reusing the same host-restricted, robots.txt-respecting crawler as read_website, up to maxDepth/maxPages) and build a local semantic search index over it under the given source name (or a name derived from the URL). Re-running stapler_index_docs on an already-indexed source fully re-indexes it in place."
    )]
    async fn index_docs(
        &self,
        params: Parameters<IndexDocsInput>,
    ) -> Result<Json<IndexDocsOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_index_docs",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_search_docs",
        description = "Semantically search a previously stapler_index_docs'd source by name, returning the top-scoring text chunks ranked by relevance to query."
    )]
    async fn search_docs(
        &self,
        params: Parameters<SearchDocsInput>,
    ) -> Result<Json<SearchDocsOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_search_docs",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_list_indexed_sources",
        description = "List every doc source currently indexed via stapler_index_docs, with page/chunk counts and when each was last indexed."
    )]
    async fn list_indexed_sources(
        &self,
        params: Parameters<ListIndexedSourcesInput>,
    ) -> Result<Json<ListIndexedSourcesOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_list_indexed_sources",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_remove_indexed_source",
        description = "Permanently delete a previously indexed doc source and all its stored chunks. Use only if explicitly instructed — there is no undo."
    )]
    async fn remove_indexed_source(
        &self,
        params: Parameters<RemoveIndexedSourceInput>,
    ) -> Result<Json<RemoveIndexedSourceOutput>, String> {
        let result = self
            .transport
            .call(
                "stapler_remove_indexed_source",
                serde_json::to_value(params.0).map_err(|e| e.to_string())?,
            )
            .await?;
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }

    #[tool(
        name = "stapler_daemon_status",
        description = "Check whether the shared stapler-mcp daemon is reachable and report daemon-wide status that isn't tied to any one browser session — currently the active browser profile persistence mode (\"ephemeral\" or \"persistent at <path>\") and, if the configured profile directory looks unsafe (inside a git repository or a cloud-synced folder), a warning naming the hazard. Use this to confirm STAPLER_MCP_BROWSER_PROFILE_DIR actually took effect rather than inferring it from whether cookies survived a restart."
    )]
    async fn daemon_status(
        &self,
        _params: Parameters<DaemonStatusInput>,
    ) -> Result<Json<DaemonStatusOutput>, String> {
        let result = match self
            .transport
            .call(stapler_mcp_core::protocol::PING_TOOL, serde_json::json!({}))
            .await
        {
            Ok(result) => result,
            Err(error) => {
                return Ok(Json(DaemonStatusOutput {
                    pong: false,
                    browser_profile_mode: None,
                    browser_profile_warning: None,
                    error: Some(error),
                }));
            }
        };
        serde_json::from_value(result)
            .map_err(|e| e.to_string())
            .map(Json)
    }
}

impl<T: DaemonTransport + Send + Sync + 'static> McpRouter<T> {
    /// Test-only accessor exercising the exact `daemon_status` tool method
    /// (and therefore the same `DaemonStatusOutput` deserialization path a
    /// real MCP client would go through) without a live stdio client. `pub`
    /// (not `cfg(test)`) so `tests/tool_schema.rs`, a separate crate, can call it.
    pub async fn call_daemon_status_for_test(&self) -> Result<DaemonStatusOutput, String> {
        self.daemon_status(Parameters(DaemonStatusInput {}))
            .await
            .map(|Json(output)| output)
    }
}

impl<T: DaemonTransport + Send + Sync + 'static> McpRouter<T> {
    /// Test-only accessor exposing this router's registered tool metadata
    /// (name, description, `inputSchema`) — the same data `tools/list`
    /// serves — without needing a live stdio MCP client. Used by
    /// `tests/tool_schema.rs` to verify Story 5.2.1's `tools/list`
    /// acceptance criterion (all docs-index tools present with non-empty
    /// descriptions and schemas matching their `*Input` structs).
    pub fn registered_tools() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }
}

#[tool_handler]
impl<T: DaemonTransport + Send + Sync + 'static> ServerHandler for McpRouter<T> {
    fn get_info(&self) -> ServerInfo {
        // `Implementation::from_build_env()` (the `ServerInfo::new` default)
        // expands `env!("CARGO_CRATE_NAME")` inside rmcp's own source, so it
        // reports rmcp's package metadata, not ours — must set this explicitly.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new("stapler-mcp", env!("CARGO_PKG_VERSION")),
        )
    }
}

#[cfg(test)]
mod tool_profile_tests {
    use super::*;

    /// Every `#[tool]` name above must appear in exactly one `TOOL_CATEGORIES`
    /// group, diffed against the real `registered_tools()` catalog so a
    /// missing or duplicated entry is caught the moment a tool is added or
    /// renamed.
    #[test]
    fn tool_categories_should_cover_every_registered_tool_exactly_once() {
        let tools = McpRouter::<SocketTransport>::registered_tools();
        let registered: HashSet<&str> = tools.iter().map(|t| t.name.as_ref()).collect();

        let mut seen = HashSet::new();
        for (category, names) in TOOL_CATEGORIES {
            for name in *names {
                assert!(
                    seen.insert(*name),
                    "'{name}' appears in more than one TOOL_CATEGORIES group"
                );
                assert!(
                    registered.contains(name),
                    "TOOL_CATEGORIES has '{name}' (category '{category}') but no such tool is registered"
                );
            }
        }
        assert_eq!(
            seen.len(),
            registered.len(),
            "TOOL_CATEGORIES is missing: {:?}",
            registered.difference(&seen).collect::<Vec<_>>()
        );
    }

    #[test]
    fn tools_outside_profile_should_disable_nothing_when_unset() {
        assert!(tools_outside_profile(None).is_empty());
    }

    #[test]
    fn tools_outside_profile_should_disable_nothing_when_empty_or_all() {
        assert!(tools_outside_profile(Some("")).is_empty());
        assert!(tools_outside_profile(Some("  ")).is_empty());
        assert!(tools_outside_profile(Some("all")).is_empty());
        assert!(tools_outside_profile(Some("ALL")).is_empty());
    }

    #[test]
    fn tools_outside_profile_should_keep_only_requested_categories_when_web_given() {
        let expected: HashSet<&str> = TOOL_CATEGORIES
            .iter()
            .filter(|(category, _)| *category != "web")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();

        let disabled: HashSet<&str> = tools_outside_profile(Some("web")).into_iter().collect();

        assert_eq!(disabled, expected);
    }

    #[test]
    fn tools_outside_profile_should_union_multiple_categories_when_comma_separated() {
        let expected: HashSet<&str> = TOOL_CATEGORIES
            .iter()
            .filter(|(category, _)| *category != "web" && *category != "docs")
            .flat_map(|(_, names)| names.iter().copied())
            .collect();

        let disabled: HashSet<&str> = tools_outside_profile(Some("web, docs"))
            .into_iter()
            .collect();

        assert_eq!(disabled, expected);
    }

    #[test]
    fn tools_outside_profile_should_ignore_unknown_tokens_but_keep_known_ones() {
        let disabled = tools_outside_profile(Some("web,not-a-real-category"));

        assert!(!disabled.contains(&"fetch_page"));
        assert!(disabled.contains(&"stapler_browser_navigate"));
    }

    #[test]
    fn tools_outside_profile_should_fall_back_to_all_when_every_token_is_unknown() {
        assert!(tools_outside_profile(Some("not-a-real-category")).is_empty());
    }

    #[test]
    fn apply_tool_profile_should_hide_profiled_out_tools_from_list_all_when_profile_given() {
        let mut router = McpRouter::<SocketTransport>::tool_router();
        apply_tool_profile(&mut router, Some("web"));

        let tools = router.list_all();
        let names: HashSet<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        assert!(names.contains("fetch_page"));
        assert!(!names.contains("stapler_browser_navigate"));
    }

    #[test]
    fn apply_tool_profile_should_keep_every_tool_when_spec_is_none() {
        let mut router = McpRouter::<SocketTransport>::tool_router();
        let before = router.list_all().len();
        apply_tool_profile(&mut router, None);
        assert_eq!(router.list_all().len(), before);
    }
}
