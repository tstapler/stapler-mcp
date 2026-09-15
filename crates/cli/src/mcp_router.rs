//! The MCP tool router Claude Code actually launches (over stdio, and later
//! Streamable HTTP). Holds no heavyweight state itself — every tool call
//! proxies to the shared daemon via a `DaemonTransport`, auto-starting the
//! daemon on first use for the default `SocketTransport`.

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
    BrowserNavigateInput, BrowserNavigateOutput, BrowserPressKeyInput, BrowserResizeInput,
    BrowserScreenshotInput, BrowserScreenshotOutput, BrowserSelectOptionInput,
    BrowserSetCheckedInput, BrowserSnapshotInput, BrowserTabsInput, BrowserTabsOutput,
    BrowserTypeInput, BrowserTypeSecretInput, BrowserWaitForInput, DownloadWebsiteInput,
    DownloadWebsiteOutput, FetchPageInput, FetchPageOutput, IndexDocsInput, IndexDocsOutput,
    ListIndexedSourcesInput, ListIndexedSourcesOutput, ReadSavedPageInput, ReadSavedPageOutput,
    ReadWebsiteInput, ReadWebsiteOutput, RemoveIndexedSourceInput, RemoveIndexedSourceOutput,
    SearchDocsInput, SearchDocsOutput,
};
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
        Self {
            transport: SocketTransport,
            tool_router: Self::tool_router(),
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
    // `tests/tool_schema.rs` pulls this file in via `#[path]` into a
    // separate compilation unit that only calls `registered_tools()`, where
    // dead-code analysis can't see `http_server.rs`'s real call site.
    #[allow(dead_code)]
    pub fn with_transport(transport: T) -> Self {
        Self {
            transport,
            tool_router: Self::tool_router(),
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
            .call("fetch_page", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("brave_web_search", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("read_website", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("read_saved_page", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("download_website", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_navigate", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_click", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_type", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_snapshot", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_close_session", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_list_sessions", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_close_all_sessions", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_tabs", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_hover", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_select_option", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_press_key", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_wait_for", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_screenshot", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_evaluate", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_fill_form", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_set_checked", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_history", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_resize", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_browser_find", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_index_docs", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_search_docs", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_list_indexed_sources", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
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
            .call("stapler_remove_indexed_source", serde_json::to_value(params.0).map_err(|e| e.to_string())?)
            .await?;
        serde_json::from_value(result).map_err(|e| e.to_string()).map(Json)
    }
}

impl<T: DaemonTransport + Send + Sync + 'static> McpRouter<T> {
    /// Test-only accessor exposing this router's registered tool metadata
    /// (name, description, `inputSchema`) — the same data `tools/list`
    /// serves — without needing a live stdio MCP client. Used by
    /// `tests/tool_schema.rs` to verify Story 5.2.1's `tools/list`
    /// acceptance criterion (all docs-index tools present with non-empty
    /// descriptions and schemas matching their `*Input` structs).
    ///
    /// Only called from that separate integration-test binary (which pulls
    /// this file in via `#[path]`, since `crates/cli` has no `[lib]`
    /// target) — invisible to dead-code analysis of *this* compilation
    /// unit, hence the explicit allow.
    #[cfg(test)]
    #[allow(dead_code)]
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
