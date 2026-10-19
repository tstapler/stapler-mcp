//! Verifies Story 5.2.1's `tools/list` acceptance criterion (plan.md Epic
//! 5.2, tracked as REQ-15 in validation.md's "Gap" row): the four docs-index
//! tools (`index_docs`, `search_docs`, `list_indexed_sources`,
//! `remove_indexed_source`) must be registered on `McpRouter`'s `rmcp`
//! `ToolRouter` with a non-empty `description` and an `inputSchema` that
//! matches their respective `*Input` struct's `schemars`-derived JSON
//! Schema.
//!
//! Deliberately does NOT go through a live stdio MCP client or the daemon
//! socket (that's `docs_index_round_trip`'s job, and it's `#[ignore]`d
//! because it needs the real embedding model). Instead this inspects
//! `McpRouter`'s tool router directly via `rmcp::ToolRouter::list_all()` —
//! the same in-process metadata the `tools/list` MCP method serves — so it's
//! fast, hermetic, and safe to run on every `cargo test`.

use std::collections::HashSet;
use std::sync::Arc;

use rmcp::handler::server::common::schema_for_input;
use rmcp::model::JsonObject;

use stapler_mcp_core::schema::{
    BraveSearchInput, BrowserClickInput, BrowserCloseAllSessionsInput, BrowserCloseSessionInput,
    BrowserEvaluateInput, BrowserFillFormInput, BrowserFindInput, BrowserGetHtmlInput,
    BrowserHistoryInput, BrowserHoverInput, BrowserListSessionsInput, BrowserNavigateInput,
    BrowserNetworkRequestsInput, BrowserPdfInput, BrowserPressKeyInput, BrowserResizeInput,
    BrowserScreenshotInput, BrowserSelectOptionInput, BrowserSetCheckedInput, BrowserSnapshotInput,
    BrowserTabsInput, BrowserTypeInput, BrowserWaitForInput, DaemonStatusInput, IndexDocsInput,
    ListIndexedSourcesInput, RemoveIndexedSourceInput, SearchDocsInput,
};

#[tokio::test]
async fn should_list_four_new_tools_with_nonempty_descriptions_and_matching_input_schema_when_tools_list_called(
) {
    let tools = stapler_mcp::mcp_router::McpRouter::<stapler_mcp::transport::SocketTransport>::registered_tools();
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
        (
            "stapler_browser_screenshot",
            schema_for_input::<BrowserScreenshotInput>()
                .expect("BrowserScreenshotInput schemars schema"),
        ),
        (
            "stapler_browser_pdf",
            schema_for_input::<BrowserPdfInput>().expect("BrowserPdfInput schemars schema"),
        ),
        (
            "stapler_browser_evaluate",
            schema_for_input::<BrowserEvaluateInput>()
                .expect("BrowserEvaluateInput schemars schema"),
        ),
        (
            "stapler_browser_fill_form",
            schema_for_input::<BrowserFillFormInput>()
                .expect("BrowserFillFormInput schemars schema"),
        ),
        (
            "stapler_browser_set_checked",
            schema_for_input::<BrowserSetCheckedInput>()
                .expect("BrowserSetCheckedInput schemars schema"),
        ),
        (
            "stapler_browser_history",
            schema_for_input::<BrowserHistoryInput>().expect("BrowserHistoryInput schemars schema"),
        ),
        (
            "stapler_browser_resize",
            schema_for_input::<BrowserResizeInput>().expect("BrowserResizeInput schemars schema"),
        ),
        (
            "stapler_browser_find",
            schema_for_input::<BrowserFindInput>().expect("BrowserFindInput schemars schema"),
        ),
        (
            "stapler_browser_get_html",
            schema_for_input::<BrowserGetHtmlInput>().expect("BrowserGetHtmlInput schemars schema"),
        ),
        (
            "stapler_daemon_status",
            schema_for_input::<DaemonStatusInput>().expect("DaemonStatusInput schemars schema"),
        ),
        (
            "browser_web_search",
            schema_for_input::<BraveSearchInput>().expect("BraveSearchInput schemars schema"),
        ),
        (
            "stapler_browser_network_requests",
            schema_for_input::<BrowserNetworkRequestsInput>()
                .expect("BrowserNetworkRequestsInput schemars schema"),
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

/// Fakes the daemon's `ping` response so `stapler_daemon_status`'s
/// deserialization path can be exercised without a live socket.
struct FakePingTransport {
    response: serde_json::Value,
}

impl stapler_mcp::transport::DaemonTransport for FakePingTransport {
    async fn call(
        &self,
        _tool: &'static str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        Ok(self.response.clone())
    }
}

/// Exercises `McpRouter::call_daemon_status_for_test()` — and therefore the
/// exact `daemon_status()` tool method a real MCP client calls — against a
/// faked `ping` response carrying `run_daemon`'s actual camelCase wire keys
/// (`browserProfileMode`/`browserProfileWarning`), so a drift between
/// `run_daemon`'s `set_status_extra` JSON and `DaemonStatusOutput`'s
/// `#[serde(rename_all = "camelCase")]` fields fails this test rather than
/// silently deserializing to `None` (previously exercised nowhere: this
/// accessor existed but had no caller).
#[tokio::test]
async fn daemon_status_should_deserialize_run_daemons_camelcase_wire_keys() {
    let router = stapler_mcp::mcp_router::McpRouter::with_transport(FakePingTransport {
        response: serde_json::json!({
            "pong": true,
            "browserProfileMode": "persistent at /home/alice/.stapler-mcp/browser-profile",
            "browserProfileWarning": "looks unsafe",
        }),
    });

    let status = router
        .call_daemon_status_for_test()
        .await
        .expect("daemon_status should succeed against a well-formed ping response");

    assert!(status.pong);
    assert_eq!(
        status.browser_profile_mode.as_deref(),
        Some("persistent at /home/alice/.stapler-mcp/browser-profile")
    );
    assert_eq!(
        status.browser_profile_warning.as_deref(),
        Some("looks unsafe")
    );
}
