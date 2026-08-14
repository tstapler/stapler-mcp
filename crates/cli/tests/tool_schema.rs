//! Verifies Story 5.2.1's `tools/list` acceptance criterion (plan.md Epic
//! 5.2, tracked as REQ-15 in validation.md's "Gap" row): the four docs-index
//! tools (`index_docs`, `search_docs`, `list_indexed_sources`,
//! `remove_indexed_source`) must be registered on `ThinClient`'s `rmcp`
//! `ToolRouter` with a non-empty `description` and an `inputSchema` that
//! matches their respective `*Input` struct's `schemars`-derived JSON
//! Schema.
//!
//! Deliberately does NOT go through a live stdio MCP client or the daemon
//! socket (that's `docs_index_round_trip`'s job, and it's `#[ignore]`d
//! because it needs the real embedding model). Instead this inspects
//! `ThinClient`'s tool router directly via `rmcp::ToolRouter::list_all()` —
//! the same in-process metadata the `tools/list` MCP method serves — so it's
//! fast, hermetic, and safe to run on every `cargo test`.
//!
//! `crates/cli` is a bin-only crate (no `[lib]` target), so `thin_client.rs`
//! isn't reachable from `tests/` via a normal `use`. `#[path]` pulls the
//! module in directly; `ThinClient::registered_tools()` (a small
//! `#[cfg(test)]`-gated accessor added alongside `ThinClient` itself) then
//! exposes the macro-generated tool router's metadata without a live stdio
//! MCP client.

use std::collections::HashSet;
use std::sync::Arc;

use rmcp::handler::server::common::schema_for_input;
use rmcp::model::JsonObject;

use stapler_mcp_core::schema::{
    BrowserClickInput, BrowserCloseAllSessionsInput, BrowserCloseSessionInput, BrowserHoverInput,
    BrowserListSessionsInput, BrowserNavigateInput, BrowserPressKeyInput, BrowserSelectOptionInput,
    BrowserSnapshotInput, BrowserTabsInput, BrowserTypeInput, BrowserWaitForInput, IndexDocsInput,
    ListIndexedSourcesInput, RemoveIndexedSourceInput, SearchDocsInput,
};

#[path = "../src/thin_client.rs"]
mod thin_client;

#[tokio::test]
async fn should_list_four_new_tools_with_nonempty_descriptions_and_matching_input_schema_when_tools_list_called(
) {
    let tools = thin_client::ThinClient::registered_tools();
    let tool_names: HashSet<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    assert_eq!(
        tool_names.len(),
        tools.len(),
        "expected no duplicate tool names registered, got: {tool_names:?}"
    );

    let expected: Vec<(&str, Arc<JsonObject>)> = vec![
        (
            "stapler_index_docs",
            schema_for_input::<IndexDocsInput>().expect("IndexDocsInput schemars schema"),
        ),
        (
            "stapler_search_docs",
            schema_for_input::<SearchDocsInput>().expect("SearchDocsInput schemars schema"),
        ),
        (
            "stapler_list_indexed_sources",
            schema_for_input::<ListIndexedSourcesInput>()
                .expect("ListIndexedSourcesInput schemars schema"),
        ),
        (
            "stapler_remove_indexed_source",
            schema_for_input::<RemoveIndexedSourceInput>()
                .expect("RemoveIndexedSourceInput schemars schema"),
        ),
        (
            "stapler_browser_navigate",
            schema_for_input::<BrowserNavigateInput>()
                .expect("BrowserNavigateInput schemars schema"),
        ),
        (
            "stapler_browser_click",
            schema_for_input::<BrowserClickInput>().expect("BrowserClickInput schemars schema"),
        ),
        (
            "stapler_browser_type",
            schema_for_input::<BrowserTypeInput>().expect("BrowserTypeInput schemars schema"),
        ),
        (
            "stapler_browser_snapshot",
            schema_for_input::<BrowserSnapshotInput>()
                .expect("BrowserSnapshotInput schemars schema"),
        ),
        (
            "stapler_browser_close_session",
            schema_for_input::<BrowserCloseSessionInput>()
                .expect("BrowserCloseSessionInput schemars schema"),
        ),
        (
            "stapler_browser_tabs",
            schema_for_input::<BrowserTabsInput>().expect("BrowserTabsInput schemars schema"),
        ),
        (
            "stapler_browser_hover",
            schema_for_input::<BrowserHoverInput>().expect("BrowserHoverInput schemars schema"),
        ),
        (
            "stapler_browser_select_option",
            schema_for_input::<BrowserSelectOptionInput>()
                .expect("BrowserSelectOptionInput schemars schema"),
        ),
        (
            "stapler_browser_press_key",
            schema_for_input::<BrowserPressKeyInput>()
                .expect("BrowserPressKeyInput schemars schema"),
        ),
        (
            "stapler_browser_wait_for",
            schema_for_input::<BrowserWaitForInput>().expect("BrowserWaitForInput schemars schema"),
        ),
        (
            "stapler_browser_list_sessions",
            schema_for_input::<BrowserListSessionsInput>()
                .expect("BrowserListSessionsInput schemars schema"),
        ),
        (
            "stapler_browser_close_all_sessions",
            schema_for_input::<BrowserCloseAllSessionsInput>()
                .expect("BrowserCloseAllSessionsInput schemars schema"),
        ),
    ];

    for (name, expected_schema) in expected {
        let tool = tools.iter().find(|t| t.name == name).unwrap_or_else(|| {
            panic!(
                "tool `{name}` not found in tools/list; registered tools: {:?}",
                tools.iter().map(|t| t.name.clone()).collect::<Vec<_>>()
            )
        });

        let description = tool.description.as_deref().unwrap_or("");
        assert!(
            !description.is_empty(),
            "tool `{name}` has an empty/missing description"
        );

        assert_eq!(
            tool.input_schema, expected_schema,
            "tool `{name}`'s inputSchema does not match `{name}`'s Input struct's schemars-derived JSON Schema"
        );
    }
}
