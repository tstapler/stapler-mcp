//! The stdio MCP server Claude Code actually launches. Holds no heavyweight
//! state itself — every tool call proxies to the shared daemon via
//! `stapler_mcp_core::client`, auto-starting it on first use.

use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerInfo};
use rmcp::{tool, tool_handler, tool_router, Json, ServerHandler};
use serde::de::DeserializeOwned;
use serde::Serialize;

use stapler_mcp_core::client::{self, EnsureOptions};
use stapler_mcp_core::paths;
use stapler_mcp_core::schema::{
    BraveSearchInput, BraveSearchOutput, DownloadWebsiteInput, DownloadWebsiteOutput, FetchPageInput,
    FetchPageOutput, IndexDocsInput, IndexDocsOutput, ListIndexedSourcesInput, ListIndexedSourcesOutput,
    ReadWebsiteInput, ReadWebsiteOutput, RemoveIndexedSourceInput, RemoveIndexedSourceOutput, SearchDocsInput,
    SearchDocsOutput,
};
use stapler_mcp_native::{NativeClock, NativeEnv, NativeSleeper, NativeSocketFactory, NativeSpawner};

const CALL_TIMEOUT: Duration = Duration::from_secs(120);

async fn call_daemon<In: Serialize, Out: DeserializeOwned>(tool: &str, input: In) -> Result<Out, String> {
    let env = NativeEnv;
    let socket = NativeSocketFactory;
    let spawner = NativeSpawner;
    let sleeper = NativeSleeper;
    let clock = NativeClock;

    let sock_path = paths::socket_path(&env);
    let log_path = paths::log_path(&env);

    client::ensure_daemon(
        &socket,
        &spawner,
        &sleeper,
        &clock,
        &sock_path,
        &log_path,
        EnsureOptions::default(),
    )
    .await
    .map_err(|e| format!("ensure daemon: {e}"))?;

    let params = serde_json::to_value(input).map_err(|e| e.to_string())?;
    let result = client::call(&socket, &sock_path, tool, Some(params), CALL_TIMEOUT)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_value(result).map_err(|e| e.to_string())
}

#[derive(Debug, Clone)]
pub struct ThinClient {
    // `#[tool_handler]` below reads this field from its macro-generated
    // `call_tool` impl, which rustc's dead-code analysis doesn't see through —
    // verified working end-to-end (tools/list and tools/call both dispatch
    // correctly), so the "never read" warning here is a known false positive.
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Self>,
}

impl ThinClient {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for ThinClient {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl ThinClient {
    #[tool(
        name = "fetch_page",
        description = "Render a URL in a headless browser and return its title and extracted text (optionally saving the rendered HTML to a local file). Backed by the shared stapler-mcp daemon's browser pool."
    )]
    async fn fetch_page(&self, params: Parameters<FetchPageInput>) -> Result<Json<FetchPageOutput>, String> {
        call_daemon("fetch_page", params.0).await.map(Json)
    }

    #[tool(
        name = "brave_web_search",
        description = "Search the web via the Brave Search API. Requires BRAVE_API_KEY in the daemon's environment."
    )]
    async fn brave_web_search(
        &self,
        params: Parameters<BraveSearchInput>,
    ) -> Result<Json<BraveSearchOutput>, String> {
        call_daemon("brave_web_search", params.0).await.map(Json)
    }

    #[tool(
        name = "read_website",
        description = "Fetch a URL (optionally crawling same-host links up to maxDepth/maxPages), extract the main content via Readability-style extraction, and return it as Markdown. Cached by URL on the daemon."
    )]
    async fn read_website(&self, params: Parameters<ReadWebsiteInput>) -> Result<Json<ReadWebsiteOutput>, String> {
        call_daemon("read_website", params.0).await.map(Json)
    }

    #[tool(
        name = "download_website",
        description = "Fetch a URL (optionally crawling same-host links up to maxDepth/maxPages) and save each page's raw HTML under saveDir."
    )]
    async fn download_website(
        &self,
        params: Parameters<DownloadWebsiteInput>,
    ) -> Result<Json<DownloadWebsiteOutput>, String> {
        call_daemon("download_website", params.0).await.map(Json)
    }

    #[tool(
        name = "stapler_index_docs",
        description = "Crawl a URL (reusing the same host-restricted, robots.txt-respecting crawler as read_website, up to maxDepth/maxPages) and build a local semantic search index over it under the given source name (or a name derived from the URL). Re-running stapler_index_docs on an already-indexed source fully re-indexes it in place."
    )]
    async fn index_docs(&self, params: Parameters<IndexDocsInput>) -> Result<Json<IndexDocsOutput>, String> {
        call_daemon("stapler_index_docs", params.0).await.map(Json)
    }

    #[tool(
        name = "stapler_search_docs",
        description = "Semantically search a previously stapler_index_docs'd source by name, returning the top-scoring text chunks ranked by relevance to query."
    )]
    async fn search_docs(&self, params: Parameters<SearchDocsInput>) -> Result<Json<SearchDocsOutput>, String> {
        call_daemon("stapler_search_docs", params.0).await.map(Json)
    }

    #[tool(
        name = "stapler_list_indexed_sources",
        description = "List every doc source currently indexed via stapler_index_docs, with page/chunk counts and when each was last indexed."
    )]
    async fn list_indexed_sources(
        &self,
        params: Parameters<ListIndexedSourcesInput>,
    ) -> Result<Json<ListIndexedSourcesOutput>, String> {
        call_daemon("stapler_list_indexed_sources", params.0).await.map(Json)
    }

    #[tool(
        name = "stapler_remove_indexed_source",
        description = "Permanently delete a previously indexed doc source and all its stored chunks. Use only if explicitly instructed — there is no undo."
    )]
    async fn remove_indexed_source(
        &self,
        params: Parameters<RemoveIndexedSourceInput>,
    ) -> Result<Json<RemoveIndexedSourceOutput>, String> {
        call_daemon("stapler_remove_indexed_source", params.0).await.map(Json)
    }
}

impl ThinClient {
    /// Test-only accessor exposing this router's registered tool metadata
    /// (name, description, `inputSchema`) — the same data `tools/list`
    /// serves — without needing a live stdio MCP client. Used by
    /// `tests/tool_schema.rs` to verify Story 5.2.1's `tools/list`
    /// acceptance criterion (all docs-index tools present with non-empty
    /// descriptions and schemas matching their `*Input` structs).
    #[cfg(test)]
    pub fn registered_tools() -> Vec<rmcp::model::Tool> {
        Self::tool_router().list_all()
    }
}

#[tool_handler]
impl ServerHandler for ThinClient {
    fn get_info(&self) -> ServerInfo {
        // `Implementation::from_build_env()` (the `ServerInfo::new` default)
        // expands `env!("CARGO_CRATE_NAME")` inside rmcp's own source, so it
        // reports rmcp's package metadata, not ours — must set this explicitly.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("stapler-mcp", env!("CARGO_PKG_VERSION")))
    }
}
