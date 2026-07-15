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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexDocsInput {
    /// The seed URL to fetch (and optionally crawl from).
    pub url: String,
    /// Optional human-readable name/slug for this source; if omitted, one is
    /// derived from the URL's host+path (e.g. `tokio.rs/tokio/tutorial` →
    /// `tokio-tutorial`). Use this name in later `search_docs` /
    /// `remove_indexed_source` calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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
pub struct IndexDocsOutput {
    /// The resolved source name — pass this to `search_docs` or
    /// `remove_indexed_source`.
    pub source_name: String,
    /// The stable identifier this source is stored under on disk.
    pub source_id: String,
    /// Number of pages fetched and indexed in this call.
    pub pages_indexed: u32,
    /// URLs of previously-indexed pages under this source that were removed
    /// because they were no longer reachable from the seed on this crawl.
    pub pages_removed: Vec<String>,
    /// Number of chunks written to the index for this source.
    pub chunks_indexed: u32,
    /// The embedding model used to embed the indexed chunks.
    pub embedding_model: String,
    /// `true` when `MAX_CHUNKS_PER_SOURCE` was hit and indexing stopped
    /// early (possibly mid-page).
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchDocsInput {
    /// The source name to search — from `index_docs`'s output or
    /// `list_indexed_sources`.
    pub source: String,
    /// The search query.
    pub query: String,
    /// Maximum number of results to return, defaults to 5.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocsSearchResult {
    /// The matched chunk's text.
    pub text: String,
    /// Cosine similarity to the query, higher is more relevant (range
    /// roughly -1.0 to 1.0).
    pub score: f32,
    /// The specific sub-page URL this chunk came from (not necessarily the
    /// seed URL).
    pub source_url: String,
    /// The nearest enclosing section heading for this chunk, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
    /// The `<title>` of the page this chunk came from.
    pub source_title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchDocsOutput {
    pub results: Vec<DocsSearchResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListIndexedSourcesInput {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSourceSummary {
    /// The source's human-readable name/slug.
    pub source_name: String,
    /// The stable identifier this source is stored under on disk.
    pub source_id: String,
    /// The original seed URL passed to `index_docs`.
    pub seed_url: String,
    /// Number of pages indexed under this source.
    pub page_count: u32,
    /// Number of chunks indexed under this source.
    pub chunk_count: u32,
    /// When this source was last indexed, in milliseconds since the Unix epoch.
    pub indexed_at_millis: u64,
    /// The embedding model used to embed this source's chunks.
    pub embedding_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ListIndexedSourcesOutput {
    pub sources: Vec<IndexedSourceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoveIndexedSourceInput {
    /// The source name to remove — from `index_docs`'s output or
    /// `list_indexed_sources`.
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RemoveIndexedSourceOutput {
    /// `true` if a source matching the requested name was found and removed.
    pub removed: bool,
    /// The name of the source that was removed.
    pub source_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_serialize_camel_case_with_source_field_when_index_docs_input_given() {
        let input = IndexDocsInput {
            url: "https://tokio.rs/tokio/tutorial".into(),
            source: Some("tokio-tutorial".into()),
            max_depth: Some(2),
            max_pages: Some(20),
        };

        let value = serde_json::to_value(&input).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "url": "https://tokio.rs/tokio/tutorial",
                "source": "tokio-tutorial",
                "maxDepth": 2,
                "maxPages": 20,
            })
        );
    }

    #[test]
    fn should_serialize_camel_case_when_index_docs_output_given() {
        let output = IndexDocsOutput {
            source_name: "tokio-tutorial".into(),
            source_id: "tokio-tutorial".into(),
            pages_indexed: 12,
            pages_removed: vec![],
            chunks_indexed: 340,
            embedding_model: "all-MiniLM-L6-v2".into(),
            truncated: false,
        };

        let value = serde_json::to_value(&output).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "sourceName": "tokio-tutorial",
                "sourceId": "tokio-tutorial",
                "pagesIndexed": 12,
                "pagesRemoved": [],
                "chunksIndexed": 340,
                "embeddingModel": "all-MiniLM-L6-v2",
                "truncated": false,
            })
        );
    }

    #[test]
    fn should_serialize_camel_case_when_search_docs_input_and_output_given() {
        let input = SearchDocsInput {
            source: "tokio-tutorial".into(),
            query: "how do I spawn a task".into(),
            limit: Some(3),
        };
        let input_value = serde_json::to_value(&input).unwrap();
        assert_eq!(
            input_value,
            serde_json::json!({
                "source": "tokio-tutorial",
                "query": "how do I spawn a task",
                "limit": 3,
            })
        );

        let output = SearchDocsOutput {
            results: vec![DocsSearchResult {
                text: "tokio::spawn creates a new asynchronous task".into(),
                score: 0.87,
                source_url: "https://tokio.rs/tokio/tutorial/spawning".into(),
                heading: Some("Spawning".into()),
                source_title: "Spawning - Tokio".into(),
            }],
        };
        let output_value = serde_json::to_value(&output).unwrap();
        let result = &output_value.get("results").unwrap()[0];
        assert_eq!(
            result.get("text").unwrap(),
            "tokio::spawn creates a new asynchronous task"
        );
        assert!((result.get("score").unwrap().as_f64().unwrap() - 0.87).abs() < 1e-6);
        assert_eq!(
            result.get("sourceUrl").unwrap(),
            "https://tokio.rs/tokio/tutorial/spawning"
        );
        assert_eq!(result.get("heading").unwrap(), "Spawning");
        assert_eq!(result.get("sourceTitle").unwrap(), "Spawning - Tokio");
    }

    #[test]
    fn should_serialize_empty_object_when_list_indexed_sources_input_given() {
        let input = ListIndexedSourcesInput {};
        let value = serde_json::to_value(&input).unwrap();
        assert_eq!(value, serde_json::json!({}));
    }

    #[test]
    fn should_serialize_camel_case_when_list_indexed_sources_output_given() {
        let output = ListIndexedSourcesOutput {
            sources: vec![
                IndexedSourceSummary {
                    source_name: "tokio-tutorial".into(),
                    source_id: "tokio-tutorial".into(),
                    seed_url: "https://tokio.rs/tokio/tutorial".into(),
                    page_count: 12,
                    chunk_count: 340,
                    indexed_at_millis: 1_700_000_000_000,
                    embedding_model: "all-MiniLM-L6-v2".into(),
                },
                IndexedSourceSummary {
                    source_name: "serde-guide".into(),
                    source_id: "serde-guide".into(),
                    seed_url: "https://serde.rs/".into(),
                    page_count: 8,
                    chunk_count: 150,
                    indexed_at_millis: 1_700_000_100_000,
                    embedding_model: "all-MiniLM-L6-v2".into(),
                },
            ],
        };

        let value = serde_json::to_value(&output).unwrap();
        let sources = value.get("sources").unwrap().as_array().unwrap();
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].get("sourceName").unwrap(), "tokio-tutorial");
        assert_eq!(
            sources[0].get("seedUrl").unwrap(),
            "https://tokio.rs/tokio/tutorial"
        );
        assert_eq!(sources[1].get("sourceName").unwrap(), "serde-guide");
        assert_eq!(sources[1].get("seedUrl").unwrap(), "https://serde.rs/");
    }

    #[test]
    fn should_serialize_camel_case_when_remove_indexed_source_input_and_output_given() {
        let input = RemoveIndexedSourceInput {
            source: "tokio-tutorial".into(),
        };
        let input_value = serde_json::to_value(&input).unwrap();
        assert_eq!(input_value, serde_json::json!({"source": "tokio-tutorial"}));

        let output = RemoveIndexedSourceOutput {
            removed: true,
            source_name: "tokio-tutorial".into(),
        };
        let output_value = serde_json::to_value(&output).unwrap();
        assert_eq!(
            output_value,
            serde_json::json!({
                "removed": true,
                "sourceName": "tokio-tutorial",
            })
        );
    }
}
