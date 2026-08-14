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
    /// Combined size, in bytes, of this source's `chunks.jsonl` and
    /// `meta.json` files on disk.
    pub bytes_on_disk: u64,
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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AxNodeOutput {
    /// `ref` is a Rust keyword and cannot be a field name, hence the rename.
    #[serde(rename = "ref")]
    pub node_ref: String,
    pub role: String,
    pub name: String,
    /// Omitted from JSON entirely for non-form-control nodes rather than
    /// serialized as `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub children: Vec<AxNodeOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AxSnapshotOutput {
    pub root: AxNodeOutput,
    pub url: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub navigated_from: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserNavigateInput {
    /// The URL to navigate to.
    pub url: String,
    /// An existing session to reuse; omit to start a new session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Navigation timeout in seconds, defaults to 30.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserNavigateOutput {
    pub session_id: String,
    pub final_url: String,
    pub snapshot: AxSnapshotOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserClickInput {
    pub session_id: String,
    /// A `ref` from a previous `AxSnapshotOutput`.
    pub ref_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTypeInput {
    pub session_id: String,
    /// A `ref` from a previous `AxSnapshotOutput`.
    pub ref_id: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSnapshotInput {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

/// Shared by click/type/snapshot responses.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserActionOutput {
    pub snapshot: AxSnapshotOutput,
    /// Carries informational messages such as "click navigated to {url};
    /// previous refs are now invalid" — never present alongside a top-level
    /// error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCloseSessionInput {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCloseSessionOutput {
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum BrowserTabsAction {
    List,
    New,
    Select,
    Close,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTabsInput {
    pub session_id: String,
    pub action: BrowserTabsAction,
    /// Which tab to act on for `select`/`close`; omit `close` to close the
    /// current tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    /// URL to open in the new tab for `new`; omit for a blank tab.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTabInfo {
    pub index: usize,
    pub url: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserTabsOutput {
    pub tabs: Vec<BrowserTabInfo>,
    pub active_index: usize,
    /// Populated only for `new`/`select`; `None` for `list`/`close`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<AxSnapshotOutput>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserHoverInput {
    pub session_id: String,
    /// A `ref` from a previous `AxSnapshotOutput`.
    pub ref_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSelectOptionInput {
    pub session_id: String,
    /// A `ref` from a previous `AxSnapshotOutput`, identifying the
    /// `<select>`-like control.
    pub ref_id: String,
    /// Option value(s) to select; multiple entries select multiple options
    /// in a multi-select control.
    pub values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPressKeyInput {
    pub session_id: String,
    /// Key name as understood by the underlying browser automation (e.g.
    /// `"Enter"`, `"ArrowDown"`).
    pub key: String,
    /// A `ref` from a previous `AxSnapshotOutput`; omit to send the key to
    /// the page's currently focused element.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserWaitForInput {
    pub session_id: String,
    /// Wait until this text appears on the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Wait until this text disappears from the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_gone: Option<String>,
    /// Wait a fixed delay in milliseconds instead of polling for text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_ms: Option<u64>,
    /// Bounds the whole wait; not meaningful together with `timeMs` beyond
    /// it, since `timeMs` is itself the delay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserListSessionsInput {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSessionSummary {
    pub session_id: String,
    pub tab_count: usize,
    /// Milliseconds since this session's last activity.
    pub idle_ms: u64,
    /// `true` if the session is stuck on a blocked host and needs
    /// re-navigating before it's usable again.
    pub blocked: bool,
    /// `true` if the session's underlying page has crashed and the session
    /// is no longer usable.
    pub crashed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserListSessionsOutput {
    pub sessions: Vec<BrowserSessionSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCloseAllSessionsInput {}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCloseSessionFailure {
    pub session_id: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BrowserCloseAllSessionsOutput {
    /// Session ids that were closed successfully.
    pub closed: Vec<String>,
    /// Session ids that failed to close, paired with the error message.
    pub failed: Vec<BrowserCloseSessionFailure>,
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
                    bytes_on_disk: 4_096,
                },
                IndexedSourceSummary {
                    source_name: "serde-guide".into(),
                    source_id: "serde-guide".into(),
                    seed_url: "https://serde.rs/".into(),
                    page_count: 8,
                    chunk_count: 150,
                    indexed_at_millis: 1_700_000_100_000,
                    embedding_model: "all-MiniLM-L6-v2".into(),
                    bytes_on_disk: 1_024,
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

    #[test]
    fn browser_navigate_input_should_omit_session_id_when_none_and_use_camelcase() {
        let input = BrowserNavigateInput {
            url: "https://example.com".into(),
            session_id: None,
            timeout_seconds: None,
        };

        let json = serde_json::to_string(&input).unwrap();

        assert!(json.contains("\"url\":\"https://example.com\""));
        assert!(!json.contains("sessionId"));
    }

    #[test]
    fn browser_type_input_should_round_trip_when_deserialized_from_camelcase_json() {
        let json = r#"{"sessionId":"sess-1","refId":"e5","text":"hello"}"#;

        let input: BrowserTypeInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id, "sess-1");
        assert_eq!(input.ref_id, "e5");
        assert_eq!(input.text, "hello");
        assert_eq!(input.timeout_seconds, None);

        let round_tripped = serde_json::to_value(&input).unwrap();
        assert_eq!(
            round_tripped,
            serde_json::json!({
                "sessionId": "sess-1",
                "refId": "e5",
                "text": "hello",
            })
        );
    }

    #[test]
    fn ax_node_output_should_use_ref_key_and_omit_value_when_non_form_control_node_given() {
        let node = AxNodeOutput {
            node_ref: "e3".into(),
            role: "button".into(),
            name: "Submit".into(),
            value: None,
            children: vec![],
        };

        let value = serde_json::to_value(&node).unwrap();

        assert_eq!(value.get("ref").unwrap(), "e3");
        assert!(value.get("nodeRef").is_none());
        assert!(value.get("node_ref").is_none());
        assert!(!value.as_object().unwrap().contains_key("value"));
    }

    #[test]
    fn ax_node_output_should_include_value_key_when_textbox_node_given() {
        let node = AxNodeOutput {
            node_ref: "e2".into(),
            role: "textbox".into(),
            name: "Email".into(),
            value: Some("user@example.com".into()),
            children: vec![],
        };

        let value = serde_json::to_value(&node).unwrap();

        assert_eq!(value.get("value").unwrap(), "user@example.com");
    }

    #[test]
    fn browser_close_session_input_should_round_trip_when_deserialized_from_camelcase_json() {
        let json = r#"{"sessionId":"sess-1"}"#;

        let input: BrowserCloseSessionInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id, "sess-1");

        let round_tripped = serde_json::to_value(&input).unwrap();
        assert_eq!(round_tripped, serde_json::json!({"sessionId": "sess-1"}));
    }

    #[test]
    fn browser_close_session_output_should_serialize_camel_case_when_given() {
        let output = BrowserCloseSessionOutput { closed: true };

        let value = serde_json::to_value(&output).unwrap();

        assert_eq!(value, serde_json::json!({"closed": true}));
    }

    #[test]
    fn browser_tabs_action_should_serialize_lowercase_when_each_variant_given() {
        assert_eq!(
            serde_json::to_value(BrowserTabsAction::List).unwrap(),
            serde_json::json!("list")
        );
        assert_eq!(
            serde_json::to_value(BrowserTabsAction::New).unwrap(),
            serde_json::json!("new")
        );
        assert_eq!(
            serde_json::to_value(BrowserTabsAction::Select).unwrap(),
            serde_json::json!("select")
        );
        assert_eq!(
            serde_json::to_value(BrowserTabsAction::Close).unwrap(),
            serde_json::json!("close")
        );
    }

    #[test]
    fn browser_tabs_input_should_round_trip_when_deserialized_from_camelcase_json() {
        let json = r#"{"sessionId":"sess-1","action":"select","index":2}"#;

        let input: BrowserTabsInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id, "sess-1");
        assert_eq!(input.action, BrowserTabsAction::Select);
        assert_eq!(input.index, Some(2));
        assert_eq!(input.url, None);

        let round_tripped = serde_json::to_value(&input).unwrap();
        assert_eq!(
            round_tripped,
            serde_json::json!({
                "sessionId": "sess-1",
                "action": "select",
                "index": 2,
            })
        );
    }

    #[test]
    fn browser_tabs_input_should_omit_index_and_url_when_none_and_action_list_given() {
        let input = BrowserTabsInput {
            session_id: "sess-1".into(),
            action: BrowserTabsAction::List,
            index: None,
            url: None,
            timeout_seconds: None,
        };

        let json = serde_json::to_string(&input).unwrap();

        assert!(!json.contains("index"));
        assert!(!json.contains("url"));
        assert!(!json.contains("timeoutSeconds"));
    }

    #[test]
    fn browser_tabs_output_should_serialize_camel_case_and_omit_snapshot_when_list_given() {
        let output = BrowserTabsOutput {
            tabs: vec![BrowserTabInfo {
                index: 0,
                url: "https://example.com".into(),
                title: "Example".into(),
            }],
            active_index: 0,
            snapshot: None,
        };

        let value = serde_json::to_value(&output).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "tabs": [{"index": 0, "url": "https://example.com", "title": "Example"}],
                "activeIndex": 0,
            })
        );
    }

    #[test]
    fn browser_hover_input_should_round_trip_when_deserialized_from_camelcase_json() {
        let json = r#"{"sessionId":"sess-1","refId":"e5"}"#;

        let input: BrowserHoverInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id, "sess-1");
        assert_eq!(input.ref_id, "e5");
        assert_eq!(input.timeout_seconds, None);

        let round_tripped = serde_json::to_value(&input).unwrap();
        assert_eq!(
            round_tripped,
            serde_json::json!({"sessionId": "sess-1", "refId": "e5"})
        );
    }

    #[test]
    fn browser_select_option_input_should_round_trip_when_deserialized_from_camelcase_json() {
        let json = r#"{"sessionId":"sess-1","refId":"e5","values":["a","b"]}"#;

        let input: BrowserSelectOptionInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id, "sess-1");
        assert_eq!(input.ref_id, "e5");
        assert_eq!(input.values, vec!["a".to_string(), "b".to_string()]);

        let round_tripped = serde_json::to_value(&input).unwrap();
        assert_eq!(
            round_tripped,
            serde_json::json!({
                "sessionId": "sess-1",
                "refId": "e5",
                "values": ["a", "b"],
            })
        );
    }

    #[test]
    fn browser_press_key_input_should_omit_ref_id_when_none_and_use_camelcase() {
        let input = BrowserPressKeyInput {
            session_id: "sess-1".into(),
            key: "Enter".into(),
            ref_id: None,
            timeout_seconds: None,
        };

        let json = serde_json::to_string(&input).unwrap();

        assert!(json.contains("\"key\":\"Enter\""));
        assert!(!json.contains("refId"));
    }

    #[test]
    fn browser_press_key_input_should_round_trip_when_deserialized_from_camelcase_json() {
        let json = r#"{"sessionId":"sess-1","key":"ArrowDown","refId":"e5"}"#;

        let input: BrowserPressKeyInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id, "sess-1");
        assert_eq!(input.key, "ArrowDown");
        assert_eq!(input.ref_id, Some("e5".to_string()));

        let round_tripped = serde_json::to_value(&input).unwrap();
        assert_eq!(
            round_tripped,
            serde_json::json!({
                "sessionId": "sess-1",
                "key": "ArrowDown",
                "refId": "e5",
            })
        );
    }

    #[test]
    fn browser_wait_for_input_should_round_trip_when_deserialized_from_camelcase_json() {
        let json = r#"{"sessionId":"sess-1","text":"Loaded","timeoutSeconds":10}"#;

        let input: BrowserWaitForInput = serde_json::from_str(json).unwrap();
        assert_eq!(input.session_id, "sess-1");
        assert_eq!(input.text, Some("Loaded".to_string()));
        assert_eq!(input.text_gone, None);
        assert_eq!(input.time_ms, None);
        assert_eq!(input.timeout_seconds, Some(10));

        let round_tripped = serde_json::to_value(&input).unwrap();
        assert_eq!(
            round_tripped,
            serde_json::json!({
                "sessionId": "sess-1",
                "text": "Loaded",
                "timeoutSeconds": 10,
            })
        );
    }

    #[test]
    fn browser_wait_for_input_should_omit_optional_fields_when_none_given() {
        let input = BrowserWaitForInput {
            session_id: "sess-1".into(),
            text: None,
            text_gone: None,
            time_ms: Some(500),
            timeout_seconds: None,
        };

        let json = serde_json::to_string(&input).unwrap();

        assert!(!json.contains("\"text\""));
        assert!(!json.contains("textGone"));
        assert!(json.contains("\"timeMs\":500"));
        assert!(!json.contains("timeoutSeconds"));
    }

    #[test]
    fn should_serialize_empty_object_when_browser_list_sessions_input_given() {
        let input = BrowserListSessionsInput {};
        let value = serde_json::to_value(&input).unwrap();
        assert_eq!(value, serde_json::json!({}));
    }

    #[test]
    fn should_serialize_camel_case_when_browser_list_sessions_output_given() {
        let output = BrowserListSessionsOutput {
            sessions: vec![BrowserSessionSummary {
                session_id: "sess-1".into(),
                tab_count: 2,
                idle_ms: 1_500,
                blocked: false,
                crashed: false,
            }],
        };

        let value = serde_json::to_value(&output).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "sessions": [{
                    "sessionId": "sess-1",
                    "tabCount": 2,
                    "idleMs": 1_500,
                    "blocked": false,
                    "crashed": false,
                }],
            })
        );
    }

    #[test]
    fn should_serialize_empty_object_when_browser_close_all_sessions_input_given() {
        let input = BrowserCloseAllSessionsInput {};
        let value = serde_json::to_value(&input).unwrap();
        assert_eq!(value, serde_json::json!({}));
    }

    #[test]
    fn should_serialize_camel_case_when_browser_close_all_sessions_output_given() {
        let output = BrowserCloseAllSessionsOutput {
            closed: vec!["sess-1".into()],
            failed: vec![BrowserCloseSessionFailure {
                session_id: "sess-2".into(),
                error: "session sess-2 not found".into(),
            }],
        };

        let value = serde_json::to_value(&output).unwrap();

        assert_eq!(
            value,
            serde_json::json!({
                "closed": ["sess-1"],
                "failed": [{
                    "sessionId": "sess-2",
                    "error": "session sess-2 not found",
                }],
            })
        );
    }
}
