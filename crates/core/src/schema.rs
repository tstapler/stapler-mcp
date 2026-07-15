//! Tool input/output shapes, shared by the native `rmcp` registration and
//! (later) the wasm-exported schema strings for the Node/TS side — one
//! authored definition, not two.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FetchPageInput {
    /// The URL to fetch and render.
    pub url: String,
    /// Optional local file path to save the rendered HTML to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_path: Option<String>,
    /// Navigation timeout in seconds, defaults to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FetchPageOutput {
    /// The page's `<title>`.
    pub title: String,
    /// Visible text content extracted from the rendered page.
    pub text: String,
    /// Local path the HTML was saved to, if `savePath` was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_to: Option<String>,
    /// The URL after any redirects.
    pub final_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BraveSearchInput {
    /// The search query.
    pub query: String,
    /// Number of results to return, defaults to 10, max 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BraveSearchResult {
    pub title: String,
    pub url: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BraveSearchOutput {
    pub results: Vec<BraveSearchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadWebsiteInput {
    /// The seed URL to fetch (and optionally crawl from).
    pub url: String,
    /// How many link-hops to follow from the seed page, defaults to 1.
    /// Crawling only follows links same-host as the seed and only from
    /// freshly-fetched pages (a cache hit doesn't re-discover links).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Maximum number of pages to fetch across the whole crawl, defaults to
    /// 10, capped at 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadWebsitePage {
    pub url: String,
    pub title: String,
    /// Main content extracted via Readability-style extraction, converted to Markdown.
    pub markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReadWebsiteOutput {
    pub pages: Vec<ReadWebsitePage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadWebsiteInput {
    /// The seed URL to fetch (and optionally crawl from).
    pub url: String,
    /// Local directory to save raw HTML pages under (one file per page,
    /// paths derived from each page's URL path).
    pub save_dir: String,
    /// How many link-hops to follow from the seed page, defaults to 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Maximum number of pages to fetch across the whole crawl, defaults to
    /// 10, capped at 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_pages: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadedPage {
    pub url: String,
    /// Local path the raw HTML was saved to.
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DownloadWebsiteOutput {
    pub pages: Vec<DownloadedPage>,
}
