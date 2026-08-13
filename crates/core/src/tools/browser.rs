//! Tool-layer functions for the four browser-automation MCP tools
//! (`stapler_browser_navigate`/`click`/`type`/`snapshot`). Each function
//! bridges a `BrowserDriver` port call to the wire-facing `schema` types and
//! maps `PortError` to a `String` the daemon can hand straight back as an
//! MCP tool error.
//!
//! Error-mapping convention (fixed during UX review, applied identically in
//! `browser_navigate`/`browser_click`/`browser_type`): `PortError::NotFound`
//! is already an actionable, driver-authored sentence (see
//! `crates/native/src/browser.rs`'s `not_found_message`), so its inner
//! message is passed through verbatim — never re-wrapped, and never routed
//! through `Display` (which would prepend a redundant "not found: "). Every
//! other variant is wrapped with `"{verb} {id}: {e}"` so the failing call and
//! target are always visible. `browser_snapshot` is the deliberate exception:
//! per `design/ux.md`'s Error 1, a snapshot against an unknown session always
//! reconstructs the canonical "no active browser session ..." sentence from
//! `input.session_id`, rather than trusting the driver's own `NotFound`
//! payload — see the `browser_snapshot` doc comment below for why.

use std::time::Duration;

use crate::ports::{
    AxNode, AxSnapshot, BrowserDriver, Locator, PortError, SessionId, TabAction, TabInfo,
    WaitCondition,
};
use crate::schema::{
    AxNodeOutput, AxSnapshotOutput, BrowserActionOutput, BrowserClickInput,
    BrowserCloseSessionInput, BrowserCloseSessionOutput, BrowserHoverInput, BrowserNavigateInput,
    BrowserNavigateOutput, BrowserPressKeyInput, BrowserSelectOptionInput, BrowserSnapshotInput,
    BrowserTabInfo, BrowserTabsAction, BrowserTabsInput, BrowserTabsOutput, BrowserTypeInput,
    BrowserWaitForInput,
};
use crate::tools::webcrawl::{blocked_host_reason, NetworkPolicy};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

fn resolve_timeout(timeout_seconds: Option<u32>) -> Duration {
    match timeout_seconds {
        Some(s) if s > 0 => Duration::from_secs(u64::from(s)),
        _ => DEFAULT_TIMEOUT,
    }
}

fn to_node_output(node: AxNode) -> AxNodeOutput {
    AxNodeOutput {
        node_ref: node.node_ref,
        role: node.role,
        name: node.name,
        value: node.value,
        children: node.children.into_iter().map(to_node_output).collect(),
    }
}

fn to_snapshot_output(snapshot: AxSnapshot) -> AxSnapshotOutput {
    AxSnapshotOutput {
        root: to_node_output(snapshot.root),
        url: snapshot.url,
        truncated: snapshot.truncated,
        navigated_from: snapshot.navigated_from,
    }
}

fn to_tab_info_output(tab: TabInfo) -> BrowserTabInfo {
    BrowserTabInfo {
        index: tab.index,
        url: tab.url,
        title: tab.title,
    }
}

/// Shared mapping for `navigate`/`click`/`type_text`: `NotFound`'s and
/// `SessionCrashed`'s inner messages are already actionable (see
/// `crates/native/src/browser.rs`'s `not_found_message`/`crashed_message`)
/// and passed through unchanged — `SessionCrashed` used to fall into the
/// generic `other` arm below, which double-prefixed its already-actionable
/// sentence with `"{verb} {id}: "`. Everything else is wrapped with the
/// failing verb and id so it's clear what was being attempted when the
/// driver failed.
fn map_error(verb: &str, id: &str, err: PortError) -> String {
    match err {
        PortError::NotFound(msg) | PortError::SessionCrashed(msg) => msg,
        other => format!("{verb} {id}: {other}"),
    }
}

pub async fn browser_navigate<B: BrowserDriver>(
    browser: &B,
    input: BrowserNavigateInput,
    policy: NetworkPolicy,
) -> Result<BrowserNavigateOutput, String> {
    if input.url.is_empty() {
        return Err("url must not be empty".to_string());
    }
    let parsed = url::Url::parse(&input.url).map_err(|e| format!("invalid url: {e}"))?;
    if let Some(reason) = blocked_host_reason(&parsed, policy) {
        // Deliberately no retry/recovery suggestion here (design/ux.md Error
        // 5a): the *target URL itself* is disallowed, so naming
        // `stapler_browser_navigate` as a next step would just invite the
        // same rejection again.
        return Err(format!("navigate blocked: {reason}"));
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = input.session_id.clone().map(SessionId);

    let result = browser
        .navigate(&input.url, session_id.as_ref(), timeout)
        .await
        .map_err(|e| map_error("navigate", &input.url, e))?;

    Ok(BrowserNavigateOutput {
        session_id: result.session_id.0,
        final_url: result.final_url,
        snapshot: to_snapshot_output(result.snapshot),
    })
}

pub async fn browser_click<B: BrowserDriver>(
    browser: &B,
    input: BrowserClickInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.ref_id.is_empty() {
        return Err("refId must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let locator = Locator(input.ref_id.clone());

    let snapshot = browser
        .click(&session_id, &locator, timeout)
        .await
        .map_err(|e| map_error("click", &input.session_id, e))?;

    let note = snapshot.navigated_from.as_ref().map(|_| {
        format!(
            "click navigated to {}; previous element refs are now invalid",
            snapshot.url
        )
    });

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note,
    })
}

pub async fn browser_type<B: BrowserDriver>(
    browser: &B,
    input: BrowserTypeInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.ref_id.is_empty() {
        return Err("refId must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let locator = Locator(input.ref_id.clone());

    let snapshot = browser
        .type_text(&session_id, &locator, &input.text, timeout)
        .await
        .map_err(|e| map_error("typing", &input.session_id, e))?;

    let note = snapshot.navigated_from.as_ref().map(|_| {
        format!(
            "typing navigated to {}; previous element refs are now invalid",
            snapshot.url
        )
    });

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note,
    })
}

/// Unlike `browser_click`/`browser_type`, a `NotFound` here always
/// reconstructs the canonical "no active browser session ..." sentence from
/// `input.session_id`, rather than forwarding the driver's own `NotFound`
/// payload verbatim: `snapshot` is the entry point most likely to be called
/// with a stale/typo'd id an agent invented itself (no prior driver call in
/// this same request to have produced a driver-authored message from), so
/// the id being echoed back and the recovery call being named matters more
/// here than trusting arbitrary driver-supplied text.
pub async fn browser_snapshot<B: BrowserDriver>(
    browser: &B,
    input: BrowserSnapshotInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());

    let snapshot = browser
        .snapshot(&session_id, timeout)
        .await
        .map_err(|e| match e {
            PortError::NotFound(_) => format!(
                "no active browser session named '{}'; call stapler_browser_navigate to start a new session",
                input.session_id
            ),
            PortError::SessionCrashed(msg) => msg,
            other => format!("snapshot {}: {other}", input.session_id),
        })?;

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note: None,
    })
}

/// Ends `input.session_id`'s entire browser session (all its tabs), unlike
/// `browser_tabs`' `close` action which only closes one tab within it.
pub async fn browser_close_session<B: BrowserDriver>(
    browser: &B,
    input: BrowserCloseSessionInput,
) -> Result<BrowserCloseSessionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    let session_id = SessionId(input.session_id.clone());

    browser
        .close_session(&session_id)
        .await
        .map_err(|e| map_error("close", &input.session_id, e))?;

    Ok(BrowserCloseSessionOutput { closed: true })
}

pub async fn browser_tabs<B: BrowserDriver>(
    browser: &B,
    input: BrowserTabsInput,
) -> Result<BrowserTabsOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    let action = match input.action {
        BrowserTabsAction::List => TabAction::List,
        BrowserTabsAction::New => TabAction::New {
            url: input.url.clone(),
        },
        BrowserTabsAction::Select => {
            let index = input
                .index
                .ok_or_else(|| "index is required for the select action".to_string())?;
            TabAction::Select { index }
        }
        BrowserTabsAction::Close => TabAction::Close { index: input.index },
    };
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());

    let result = browser
        .tabs(&session_id, action, timeout)
        .await
        .map_err(|e| map_error("tabs", &input.session_id, e))?;

    Ok(BrowserTabsOutput {
        tabs: result.tabs.into_iter().map(to_tab_info_output).collect(),
        active_index: result.active_index,
        snapshot: result.snapshot.map(to_snapshot_output),
    })
}

pub async fn browser_hover<B: BrowserDriver>(
    browser: &B,
    input: BrowserHoverInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.ref_id.is_empty() {
        return Err("refId must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let locator = Locator(input.ref_id.clone());

    let snapshot = browser
        .hover(&session_id, &locator, timeout)
        .await
        .map_err(|e| map_error("hover", &input.session_id, e))?;

    let note = snapshot.navigated_from.as_ref().map(|_| {
        format!(
            "hover navigated to {}; previous element refs are now invalid",
            snapshot.url
        )
    });

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note,
    })
}

pub async fn browser_select_option<B: BrowserDriver>(
    browser: &B,
    input: BrowserSelectOptionInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.ref_id.is_empty() {
        return Err("refId must not be empty".to_string());
    }
    if input.values.is_empty() {
        return Err("values must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let locator = Locator(input.ref_id.clone());

    let snapshot = browser
        .select_option(&session_id, &locator, &input.values, timeout)
        .await
        .map_err(|e| map_error("select", &input.session_id, e))?;

    let note = snapshot.navigated_from.as_ref().map(|_| {
        format!(
            "select navigated to {}; previous element refs are now invalid",
            snapshot.url
        )
    });

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note,
    })
}

pub async fn browser_press_key<B: BrowserDriver>(
    browser: &B,
    input: BrowserPressKeyInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.key.is_empty() {
        return Err("key must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let locator = input.ref_id.clone().map(Locator);

    let snapshot = browser
        .press_key(&session_id, &input.key, locator.as_ref(), timeout)
        .await
        .map_err(|e| map_error("press key", &input.session_id, e))?;

    let note = snapshot.navigated_from.as_ref().map(|_| {
        format!(
            "press key navigated to {}; previous element refs are now invalid",
            snapshot.url
        )
    });

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note,
    })
}

/// Exactly one of `text`/`textGone`/`timeMs` must be set — zero leaves the
/// driver with nothing to wait for, and more than one is ambiguous about
/// which condition should actually gate the return.
pub async fn browser_wait_for<B: BrowserDriver>(
    browser: &B,
    input: BrowserWaitForInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    let set_count = [
        input.text.is_some(),
        input.text_gone.is_some(),
        input.time_ms.is_some(),
    ]
    .into_iter()
    .filter(|set| *set)
    .count();
    if set_count == 0 {
        return Err("exactly one of text, textGone, or timeMs must be set".to_string());
    }
    if set_count > 1 {
        return Err("only one of text, textGone, or timeMs may be set".to_string());
    }
    let condition = if let Some(text) = input.text.clone() {
        WaitCondition::TextAppears(text)
    } else if let Some(text_gone) = input.text_gone.clone() {
        WaitCondition::TextDisappears(text_gone)
    } else {
        WaitCondition::TimeMs(input.time_ms.expect("time_ms checked set above"))
    };
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());

    let snapshot = browser
        .wait_for(&session_id, condition, timeout)
        .await
        .map_err(|e| map_error("wait for", &input.session_id, e))?;

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note: None,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::ports::PageExtract;

    struct FakeBrowserDriver {
        navigate_result: RefCell<Option<Result<crate::ports::NavigateResult, PortError>>>,
        click_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
        type_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
        snapshot_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
        close_session_result: RefCell<Option<Result<(), PortError>>>,
        hover_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
        select_option_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
        press_key_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
        wait_for_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
        /// Overrides whatever `tabs()` would otherwise compute from
        /// `tabs_state` — used to simulate driver-level failures (e.g. an
        /// unknown session) without disturbing the in-memory tab list.
        tabs_error: RefCell<Option<PortError>>,
        /// In-memory tab list `tabs()` operates on, seeded with a single tab
        /// by `new()` so `List`/`Close` have something realistic to act on.
        tabs_state: RefCell<Vec<TabInfo>>,
        active_tab_index: RefCell<usize>,
        calls: RefCell<Vec<&'static str>>,
    }

    impl FakeBrowserDriver {
        fn new() -> Self {
            FakeBrowserDriver {
                navigate_result: RefCell::new(None),
                click_result: RefCell::new(None),
                type_result: RefCell::new(None),
                snapshot_result: RefCell::new(None),
                close_session_result: RefCell::new(None),
                hover_result: RefCell::new(None),
                select_option_result: RefCell::new(None),
                press_key_result: RefCell::new(None),
                wait_for_result: RefCell::new(None),
                tabs_error: RefCell::new(None),
                tabs_state: RefCell::new(vec![TabInfo {
                    index: 0,
                    url: "https://example.com/".to_string(),
                    title: "Example".to_string(),
                }]),
                active_tab_index: RefCell::new(0),
                calls: RefCell::new(Vec::new()),
            }
        }

        fn with_navigate(self, result: Result<crate::ports::NavigateResult, PortError>) -> Self {
            *self.navigate_result.borrow_mut() = Some(result);
            self
        }

        fn with_click(self, result: Result<AxSnapshot, PortError>) -> Self {
            *self.click_result.borrow_mut() = Some(result);
            self
        }

        fn with_type(self, result: Result<AxSnapshot, PortError>) -> Self {
            *self.type_result.borrow_mut() = Some(result);
            self
        }

        fn with_snapshot(self, result: Result<AxSnapshot, PortError>) -> Self {
            *self.snapshot_result.borrow_mut() = Some(result);
            self
        }

        fn with_close_session(self, result: Result<(), PortError>) -> Self {
            *self.close_session_result.borrow_mut() = Some(result);
            self
        }

        fn with_hover(self, result: Result<AxSnapshot, PortError>) -> Self {
            *self.hover_result.borrow_mut() = Some(result);
            self
        }

        fn with_select_option(self, result: Result<AxSnapshot, PortError>) -> Self {
            *self.select_option_result.borrow_mut() = Some(result);
            self
        }

        fn with_press_key(self, result: Result<AxSnapshot, PortError>) -> Self {
            *self.press_key_result.borrow_mut() = Some(result);
            self
        }

        fn with_wait_for(self, result: Result<AxSnapshot, PortError>) -> Self {
            *self.wait_for_result.borrow_mut() = Some(result);
            self
        }

        fn with_tabs_error(self, err: PortError) -> Self {
            *self.tabs_error.borrow_mut() = Some(err);
            self
        }

        fn with_tabs(self, tabs: Vec<TabInfo>) -> Self {
            *self.tabs_state.borrow_mut() = tabs;
            *self.active_tab_index.borrow_mut() = 0;
            self
        }

        fn call_count(&self) -> usize {
            self.calls.borrow().len()
        }
    }

    impl BrowserDriver for FakeBrowserDriver {
        async fn navigate_and_extract(
            &self,
            _url: &str,
            _timeout: Duration,
        ) -> Result<PageExtract, PortError> {
            panic!("navigate_and_extract should never be called by the browser-automation tools");
        }

        async fn navigate(
            &self,
            _url: &str,
            _session_id: Option<&SessionId>,
            _timeout: Duration,
        ) -> Result<crate::ports::NavigateResult, PortError> {
            self.calls.borrow_mut().push("navigate");
            self.navigate_result
                .borrow_mut()
                .take()
                .expect("navigate result not configured")
        }

        async fn click(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            self.calls.borrow_mut().push("click");
            self.click_result
                .borrow_mut()
                .take()
                .expect("click result not configured")
        }

        async fn type_text(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _text: &str,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            self.calls.borrow_mut().push("type_text");
            self.type_result
                .borrow_mut()
                .take()
                .expect("type result not configured")
        }

        async fn snapshot(
            &self,
            _session_id: &SessionId,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            self.calls.borrow_mut().push("snapshot");
            self.snapshot_result
                .borrow_mut()
                .take()
                .expect("snapshot result not configured")
        }

        async fn close_session(&self, _session_id: &SessionId) -> Result<(), PortError> {
            self.calls.borrow_mut().push("close_session");
            self.close_session_result
                .borrow_mut()
                .take()
                .expect("close_session result not configured")
        }

        async fn tabs(
            &self,
            _session_id: &SessionId,
            action: crate::ports::TabAction,
            _timeout: Duration,
        ) -> Result<crate::ports::TabsResult, PortError> {
            self.calls.borrow_mut().push("tabs");
            if let Some(err) = self.tabs_error.borrow_mut().take() {
                return Err(err);
            }

            match action {
                TabAction::List => Ok(crate::ports::TabsResult {
                    tabs: self.tabs_state.borrow().clone(),
                    active_index: *self.active_tab_index.borrow(),
                    snapshot: None,
                }),
                TabAction::New { url } => {
                    let mut tabs = self.tabs_state.borrow_mut();
                    let new_index = tabs.len();
                    let tab_url = url.unwrap_or_default();
                    tabs.push(TabInfo {
                        index: new_index,
                        url: tab_url.clone(),
                        title: format!("Tab {new_index}"),
                    });
                    *self.active_tab_index.borrow_mut() = new_index;
                    Ok(crate::ports::TabsResult {
                        tabs: tabs.clone(),
                        active_index: new_index,
                        snapshot: Some(sample_snapshot(&tab_url, None)),
                    })
                }
                TabAction::Select { index } => {
                    let tabs = self.tabs_state.borrow();
                    if index >= tabs.len() {
                        return Err(PortError::NotFound(format!("no tab at index {index}")));
                    }
                    *self.active_tab_index.borrow_mut() = index;
                    Ok(crate::ports::TabsResult {
                        tabs: tabs.clone(),
                        active_index: index,
                        snapshot: Some(sample_snapshot(&tabs[index].url, None)),
                    })
                }
                TabAction::Close { index } => {
                    let mut tabs = self.tabs_state.borrow_mut();
                    let close_index = index.unwrap_or(*self.active_tab_index.borrow());
                    if close_index >= tabs.len() {
                        return Err(PortError::NotFound(format!(
                            "no tab at index {close_index}"
                        )));
                    }
                    if tabs.len() == 1 {
                        return Err(PortError::Other(
                            "cannot close the last remaining tab".to_string(),
                        ));
                    }
                    tabs.remove(close_index);
                    for (i, tab) in tabs.iter_mut().enumerate() {
                        tab.index = i;
                    }
                    let mut active = self.active_tab_index.borrow_mut();
                    if *active >= tabs.len() {
                        *active = tabs.len() - 1;
                    } else if close_index < *active {
                        *active -= 1;
                    }
                    Ok(crate::ports::TabsResult {
                        tabs: tabs.clone(),
                        active_index: *active,
                        snapshot: None,
                    })
                }
            }
        }

        async fn hover(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            self.calls.borrow_mut().push("hover");
            self.hover_result
                .borrow_mut()
                .take()
                .expect("hover result not configured")
        }

        async fn select_option(
            &self,
            _session_id: &SessionId,
            _locator: &Locator,
            _values: &[String],
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            self.calls.borrow_mut().push("select_option");
            self.select_option_result
                .borrow_mut()
                .take()
                .expect("select_option result not configured")
        }

        async fn press_key(
            &self,
            _session_id: &SessionId,
            _key: &str,
            _locator: Option<&Locator>,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            self.calls.borrow_mut().push("press_key");
            self.press_key_result
                .borrow_mut()
                .take()
                .expect("press_key result not configured")
        }

        async fn wait_for(
            &self,
            _session_id: &SessionId,
            _condition: crate::ports::WaitCondition,
            _timeout: Duration,
        ) -> Result<AxSnapshot, PortError> {
            self.calls.borrow_mut().push("wait_for");
            self.wait_for_result
                .borrow_mut()
                .take()
                .expect("wait_for result not configured")
        }
    }

    fn sample_node() -> AxNode {
        AxNode {
            node_ref: "e1".to_string(),
            role: "generic".to_string(),
            name: String::new(),
            value: None,
            children: Vec::new(),
        }
    }

    fn sample_snapshot(url: &str, navigated_from: Option<&str>) -> AxSnapshot {
        AxSnapshot {
            root: sample_node(),
            url: url.to_string(),
            truncated: false,
            navigated_from: navigated_from.map(|s| s.to_string()),
        }
    }

    fn navigate_input(url: &str) -> BrowserNavigateInput {
        BrowserNavigateInput {
            url: url.to_string(),
            session_id: None,
            timeout_seconds: None,
        }
    }

    // -- browser_navigate ---------------------------------------------------

    #[tokio::test]
    async fn browser_navigate_should_return_new_session_id_when_no_session_given() {
        let driver = FakeBrowserDriver::new().with_navigate(Ok(crate::ports::NavigateResult {
            session_id: SessionId("sess-1".to_string()),
            final_url: "https://example.com/".to_string(),
            snapshot: sample_snapshot("https://example.com/", None),
        }));

        let output = browser_navigate(
            &driver,
            navigate_input("https://example.com/"),
            NetworkPolicy::Enforce,
        )
        .await
        .expect("navigate should succeed");

        assert_eq!(output.session_id, "sess-1");
        assert_eq!(output.final_url, "https://example.com/");
    }

    #[tokio::test]
    async fn browser_navigate_should_return_err_when_url_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_navigate(&driver, navigate_input(""), NetworkPolicy::Enforce)
            .await
            .expect_err("empty url should be rejected");

        assert_eq!(err, "url must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_navigate_should_return_blocked_err_when_url_resolves_to_private_host() {
        let driver = FakeBrowserDriver::new();

        let err = browser_navigate(
            &driver,
            navigate_input("http://127.0.0.1:1/"),
            NetworkPolicy::Enforce,
        )
        .await
        .expect_err("private-host url should be blocked");

        assert!(err.contains("blocked"), "unexpected message: {err}");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_navigate_should_pass_not_found_message_through_verbatim() {
        let driver = FakeBrowserDriver::new().with_navigate(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
                .to_string(),
        )));

        let err = browser_navigate(
            &driver,
            BrowserNavigateInput {
                url: "https://example.com/".to_string(),
                session_id: Some("sess-9".to_string()),
                timeout_seconds: None,
            },
            NetworkPolicy::Enforce,
        )
        .await
        .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }

    #[tokio::test]
    async fn browser_navigate_should_pass_session_crashed_message_through_verbatim() {
        let driver = FakeBrowserDriver::new().with_navigate(Err(PortError::SessionCrashed(
            "browser session \"sess-9\" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session"
                .to_string(),
        )));

        let err = browser_navigate(
            &driver,
            BrowserNavigateInput {
                url: "https://example.com/".to_string(),
                session_id: Some("sess-9".to_string()),
                timeout_seconds: None,
            },
            NetworkPolicy::Enforce,
        )
        .await
        .expect_err("session-crashed should surface as an error");

        assert_eq!(
            err,
            "browser session \"sess-9\" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session"
        );
    }

    // -- browser_click --------------------------------------------------------

    #[tokio::test]
    async fn browser_click_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_click(
            &driver,
            BrowserClickInput {
                session_id: String::new(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_click_should_return_err_when_ref_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_click(
            &driver,
            BrowserClickInput {
                session_id: "sess-1".to_string(),
                ref_id: String::new(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty refId should be rejected");

        assert_eq!(err, "refId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_click_should_pass_not_found_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_click(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session".to_string(),
        )));

        let err = browser_click(
            &driver,
            BrowserClickInput {
                session_id: "sess-9".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }

    #[tokio::test]
    async fn browser_click_should_pass_session_crashed_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_click(Err(PortError::SessionCrashed(
            "browser session \"sess-9\" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session".to_string(),
        )));

        let err = browser_click(
            &driver,
            BrowserClickInput {
                session_id: "sess-9".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("session-crashed should surface as an error");

        assert_eq!(
            err,
            "browser session \"sess-9\" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session"
        );
    }

    #[tokio::test]
    async fn browser_click_should_wrap_non_not_found_errors_with_verb_and_session_id() {
        let driver = FakeBrowserDriver::new().with_click(Err(PortError::Timeout));

        let err = browser_click(
            &driver,
            BrowserClickInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("timeout should surface as an error");

        assert_eq!(err, "click sess-1: timed out");
    }

    #[tokio::test]
    async fn browser_click_should_set_note_when_click_causes_navigation() {
        let driver = FakeBrowserDriver::new().with_click(Ok(sample_snapshot(
            "https://example.com/thanks",
            Some("https://example.com/form"),
        )));

        let output = browser_click(
            &driver,
            BrowserClickInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect("click should succeed");

        assert_eq!(
            output.note,
            Some(
                "click navigated to https://example.com/thanks; previous element refs are now invalid"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn browser_click_should_leave_note_none_when_click_does_not_navigate() {
        let driver =
            FakeBrowserDriver::new().with_click(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_click(
            &driver,
            BrowserClickInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect("click should succeed");

        assert_eq!(output.note, None);
    }

    // -- browser_type -----------------------------------------------------------

    #[tokio::test]
    async fn browser_type_should_return_action_output_when_type_succeeds() {
        let driver =
            FakeBrowserDriver::new().with_type(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_type(
            &driver,
            BrowserTypeInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                text: "hello".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect("type should succeed");

        assert_eq!(output.snapshot.url, "https://example.com/");
        assert_eq!(output.note, None);
    }

    #[tokio::test]
    async fn browser_type_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_type(
            &driver,
            BrowserTypeInput {
                session_id: String::new(),
                ref_id: "e1".to_string(),
                text: "hello".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_type_should_return_err_when_ref_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_type(
            &driver,
            BrowserTypeInput {
                session_id: "sess-1".to_string(),
                ref_id: String::new(),
                text: "hello".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty refId should be rejected");

        assert_eq!(err, "refId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_type_should_set_note_naming_typing_when_type_causes_navigation() {
        let driver = FakeBrowserDriver::new().with_type(Ok(sample_snapshot(
            "https://example.com/results",
            Some("https://example.com/search"),
        )));

        let output = browser_type(
            &driver,
            BrowserTypeInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                text: "hello".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect("type should succeed");

        assert_eq!(
            output.note,
            Some(
                "typing navigated to https://example.com/results; previous element refs are now invalid"
                    .to_string()
            )
        );
    }

    // -- browser_snapshot ---------------------------------------------------

    #[tokio::test]
    async fn browser_snapshot_should_return_actionable_message_when_session_not_found() {
        let driver =
            FakeBrowserDriver::new().with_snapshot(Err(PortError::NotFound("sess-9".to_string())));

        let err = browser_snapshot(
            &driver,
            BrowserSnapshotInput {
                session_id: "sess-9".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }

    #[tokio::test]
    async fn browser_snapshot_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_snapshot(
            &driver,
            BrowserSnapshotInput {
                session_id: String::new(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_snapshot_should_pass_session_crashed_message_through_verbatim() {
        let driver = FakeBrowserDriver::new().with_snapshot(Err(PortError::SessionCrashed(
            "browser session \"sess-9\" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session"
                .to_string(),
        )));

        let err = browser_snapshot(
            &driver,
            BrowserSnapshotInput {
                session_id: "sess-9".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("session-crashed should surface as an error");

        assert_eq!(
            err,
            "browser session \"sess-9\" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session"
        );
    }

    // -- UX acceptance tests --------------------------------------------------

    #[tokio::test]
    async fn ux_ac2_session_not_found_error_should_name_session_id_and_corrective_call() {
        let driver = FakeBrowserDriver::new().with_snapshot(Err(PortError::NotFound(
            "whatever the driver says".to_string(),
        )));

        let err = browser_snapshot(
            &driver,
            BrowserSnapshotInput {
                session_id: "sess-bogus".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert!(
            err.contains("sess-bogus"),
            "message should name the session id: {err}"
        );
        assert!(
            err.contains("stapler_browser_navigate"),
            "message should name the corrective call: {err}"
        );
    }

    #[tokio::test]
    async fn ux_ac4_click_should_set_note_naming_new_url_when_click_causes_navigation() {
        let driver = FakeBrowserDriver::new().with_click(Ok(sample_snapshot(
            "https://example.com/dashboard",
            Some("https://example.com/login"),
        )));

        let output = browser_click(
            &driver,
            BrowserClickInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect("click should succeed");

        let note = output
            .note
            .expect("note should be set when click navigates");
        assert!(
            note.contains("https://example.com/dashboard"),
            "note should name the new url: {note}"
        );
    }

    /// Table test over the error shapes `design/ux.md`'s UX AC 5 calls out:
    /// every error should name a specific recovery call (`stapler_browser_navigate`
    /// or `stapler_browser_snapshot`) *except* the one case that's about the
    /// target URL itself being disallowed, where suggesting a retry would just
    /// invite the same rejection again.
    ///
    /// The three session/locator-scoped cases below rely on the driver's own
    /// `PortError` payload already carrying the recovery instruction (true in
    /// production — see `crates/native/src/browser.rs`'s `not_found_message`/
    /// `crashed_message`, and the SSRF-poisoned-session message in
    /// `design/ux.md`'s Error 5b) and on this module's `map_error`/`browser_snapshot`
    /// passing that text through rather than discarding it.
    #[tokio::test]
    async fn ux_ac5_every_error_variant_should_name_a_specific_recovery_call_except_ssrf_target_block(
    ) {
        async fn session_not_found_via_snapshot() -> String {
            let driver = FakeBrowserDriver::new()
                .with_snapshot(Err(PortError::NotFound("sess-1".to_string())));
            browser_snapshot(
                &driver,
                BrowserSnapshotInput {
                    session_id: "sess-1".to_string(),
                    timeout_seconds: None,
                },
            )
            .await
            .expect_err("should error")
        }

        async fn locator_not_found_via_click() -> String {
            let driver = FakeBrowserDriver::new().with_click(Err(PortError::NotFound(
                "no element with ref 'e9' in current snapshot (page: https://example.com/dashboard); call stapler_browser_snapshot for current refs".to_string(),
            )));
            browser_click(
                &driver,
                BrowserClickInput {
                    session_id: "sess-1".to_string(),
                    ref_id: "e9".to_string(),
                    timeout_seconds: None,
                },
            )
            .await
            .expect_err("should error")
        }

        async fn ssrf_poisoned_session_via_navigate() -> String {
            let driver = FakeBrowserDriver::new().with_navigate(Err(PortError::NotFound(
                "session 'sess-1' navigated to a blocked host '169.254.169.254' during the last action; call stapler_browser_navigate with this sessionId and a safe URL to recover it, or start a fresh session".to_string(),
            )));
            browser_navigate(
                &driver,
                BrowserNavigateInput {
                    url: "https://example.com/next".to_string(),
                    session_id: Some("sess-1".to_string()),
                    timeout_seconds: None,
                },
                NetworkPolicy::Enforce,
            )
            .await
            .expect_err("should error")
        }

        async fn ssrf_target_url_blocked_via_navigate() -> String {
            let driver = FakeBrowserDriver::new();
            browser_navigate(
                &driver,
                BrowserNavigateInput {
                    url: "http://127.0.0.1:1/".to_string(),
                    session_id: None,
                    timeout_seconds: None,
                },
                NetworkPolicy::Enforce,
            )
            .await
            .expect_err("should error")
        }

        let session_not_found = session_not_found_via_snapshot().await;
        assert!(
            session_not_found.contains("stapler_browser_navigate"),
            "{session_not_found}"
        );

        let locator_not_found = locator_not_found_via_click().await;
        assert!(
            locator_not_found.contains("stapler_browser_snapshot"),
            "{locator_not_found}"
        );

        let ssrf_poisoned_session = ssrf_poisoned_session_via_navigate().await;
        assert!(
            ssrf_poisoned_session.contains("stapler_browser_navigate"),
            "{ssrf_poisoned_session}"
        );

        let ssrf_target_blocked = ssrf_target_url_blocked_via_navigate().await;
        assert!(
            !ssrf_target_blocked.contains("stapler_browser_navigate")
                && !ssrf_target_blocked.contains("stapler_browser_snapshot"),
            "target-url-blocked case should omit a retry suggestion: {ssrf_target_blocked}"
        );
    }

    #[tokio::test]
    async fn ux_ac9_successful_response_should_never_contain_top_level_error_key_alongside_note() {
        let success = BrowserActionOutput {
            snapshot: to_snapshot_output(sample_snapshot("https://example.com/", None)),
            note: Some(
                "click navigated to https://example.com/; previous element refs are now invalid"
                    .to_string(),
            ),
        };
        let json = serde_json::to_value(&success).expect("serialize");
        assert!(json.get("error").is_none());
        assert!(json.get("note").is_some());

        // The failure path returns `Result::Err(String)` from the tool
        // function itself, never a `BrowserActionOutput` — so there is no
        // `BrowserActionOutput` value to serialize alongside an error at
        // all. Confirm that shape directly against a fake failing call.
        let driver =
            FakeBrowserDriver::new().with_snapshot(Err(PortError::NotFound("sess-1".to_string())));
        let result = browser_snapshot(
            &driver,
            BrowserSnapshotInput {
                session_id: "sess-1".to_string(),
                timeout_seconds: None,
            },
        )
        .await;
        assert!(result.is_err());
    }

    // -- browser_close_session ------------------------------------------------

    #[tokio::test]
    async fn browser_close_session_should_return_closed_true_when_close_succeeds() {
        let driver = FakeBrowserDriver::new().with_close_session(Ok(()));

        let output = browser_close_session(
            &driver,
            BrowserCloseSessionInput {
                session_id: "sess-1".to_string(),
            },
        )
        .await
        .expect("close should succeed");

        assert!(output.closed);
    }

    #[tokio::test]
    async fn browser_close_session_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_close_session(
            &driver,
            BrowserCloseSessionInput {
                session_id: String::new(),
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_close_session_should_pass_not_found_message_through_verbatim() {
        let driver = FakeBrowserDriver::new().with_close_session(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
                .to_string(),
        )));

        let err = browser_close_session(
            &driver,
            BrowserCloseSessionInput {
                session_id: "sess-9".to_string(),
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }

    // -- browser_tabs -----------------------------------------------------------

    fn tabs_input(action: BrowserTabsAction) -> BrowserTabsInput {
        BrowserTabsInput {
            session_id: "sess-1".to_string(),
            action,
            index: None,
            url: None,
            timeout_seconds: None,
        }
    }

    #[tokio::test]
    async fn browser_tabs_should_list_seeded_tab_when_list_action_given() {
        let driver = FakeBrowserDriver::new();

        let output = browser_tabs(&driver, tabs_input(BrowserTabsAction::List))
            .await
            .expect("list should succeed");

        assert_eq!(output.tabs.len(), 1);
        assert_eq!(output.tabs[0].url, "https://example.com/");
        assert_eq!(output.active_index, 0);
        assert!(output.snapshot.is_none());
    }

    #[tokio::test]
    async fn browser_tabs_should_append_tab_and_activate_it_when_new_action_given() {
        let driver = FakeBrowserDriver::new();
        let mut input = tabs_input(BrowserTabsAction::New);
        input.url = Some("https://example.com/new".to_string());

        let output = browser_tabs(&driver, input)
            .await
            .expect("new should succeed");

        assert_eq!(output.tabs.len(), 2);
        assert_eq!(output.tabs[1].url, "https://example.com/new");
        assert_eq!(output.active_index, 1);
        assert!(output.snapshot.is_some());
    }

    #[tokio::test]
    async fn browser_tabs_should_activate_selected_index_when_select_action_given() {
        let driver = FakeBrowserDriver::new().with_tabs(vec![
            TabInfo {
                index: 0,
                url: "https://example.com/a".to_string(),
                title: "A".to_string(),
            },
            TabInfo {
                index: 1,
                url: "https://example.com/b".to_string(),
                title: "B".to_string(),
            },
        ]);
        let mut input = tabs_input(BrowserTabsAction::Select);
        input.index = Some(1);

        let output = browser_tabs(&driver, input)
            .await
            .expect("select should succeed");

        assert_eq!(output.active_index, 1);
        assert!(output.snapshot.is_some());
    }

    #[tokio::test]
    async fn browser_tabs_should_return_err_when_select_index_out_of_range() {
        let driver = FakeBrowserDriver::new();
        let mut input = tabs_input(BrowserTabsAction::Select);
        input.index = Some(9);

        let err = browser_tabs(&driver, input)
            .await
            .expect_err("out-of-range index should be rejected");

        assert!(err.contains("no tab at index 9"), "unexpected message: {err}");
    }

    #[tokio::test]
    async fn browser_tabs_should_return_err_when_select_index_missing() {
        let driver = FakeBrowserDriver::new();

        let err = browser_tabs(&driver, tabs_input(BrowserTabsAction::Select))
            .await
            .expect_err("missing index should be rejected");

        assert_eq!(err, "index is required for the select action");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_tabs_should_close_named_tab_when_close_action_given_with_index() {
        let driver = FakeBrowserDriver::new().with_tabs(vec![
            TabInfo {
                index: 0,
                url: "https://example.com/a".to_string(),
                title: "A".to_string(),
            },
            TabInfo {
                index: 1,
                url: "https://example.com/b".to_string(),
                title: "B".to_string(),
            },
        ]);
        let mut input = tabs_input(BrowserTabsAction::Close);
        input.index = Some(0);

        let output = browser_tabs(&driver, input)
            .await
            .expect("close should succeed");

        assert_eq!(output.tabs.len(), 1);
        assert_eq!(output.tabs[0].url, "https://example.com/b");
    }

    #[tokio::test]
    async fn browser_tabs_should_return_err_when_close_index_out_of_range() {
        let driver = FakeBrowserDriver::new();
        let mut input = tabs_input(BrowserTabsAction::Close);
        input.index = Some(9);

        let err = browser_tabs(&driver, input)
            .await
            .expect_err("out-of-range index should be rejected");

        assert!(err.contains("no tab at index 9"), "unexpected message: {err}");
    }

    #[tokio::test]
    async fn browser_tabs_should_return_err_when_closing_the_last_remaining_tab() {
        let driver = FakeBrowserDriver::new();

        let err = browser_tabs(&driver, tabs_input(BrowserTabsAction::Close))
            .await
            .expect_err("closing the last tab should be rejected");

        assert!(
            err.contains("cannot close the last remaining tab"),
            "unexpected message: {err}"
        );
    }

    #[tokio::test]
    async fn browser_tabs_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();
        let mut input = tabs_input(BrowserTabsAction::List);
        input.session_id = String::new();

        let err = browser_tabs(&driver, input)
            .await
            .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_tabs_should_pass_not_found_message_through_verbatim_when_session_unknown() {
        let driver = FakeBrowserDriver::new().with_tabs_error(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
                .to_string(),
        ));

        let err = browser_tabs(&driver, tabs_input(BrowserTabsAction::List))
            .await
            .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }

    // -- browser_hover ----------------------------------------------------------

    #[tokio::test]
    async fn browser_hover_should_return_action_output_when_hover_succeeds() {
        let driver =
            FakeBrowserDriver::new().with_hover(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_hover(
            &driver,
            BrowserHoverInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect("hover should succeed");

        assert_eq!(output.snapshot.url, "https://example.com/");
        assert_eq!(output.note, None);
    }

    #[tokio::test]
    async fn browser_hover_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_hover(
            &driver,
            BrowserHoverInput {
                session_id: String::new(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_hover_should_return_err_when_ref_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_hover(
            &driver,
            BrowserHoverInput {
                session_id: "sess-1".to_string(),
                ref_id: String::new(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty refId should be rejected");

        assert_eq!(err, "refId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_hover_should_pass_not_found_message_through_verbatim() {
        let driver = FakeBrowserDriver::new().with_hover(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
                .to_string(),
        )));

        let err = browser_hover(
            &driver,
            BrowserHoverInput {
                session_id: "sess-9".to_string(),
                ref_id: "e1".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }

    // -- browser_select_option ---------------------------------------------------

    #[tokio::test]
    async fn browser_select_option_should_return_action_output_when_select_succeeds() {
        let driver = FakeBrowserDriver::new()
            .with_select_option(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_select_option(
            &driver,
            BrowserSelectOptionInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                values: vec!["opt-a".to_string()],
                timeout_seconds: None,
            },
        )
        .await
        .expect("select should succeed");

        assert_eq!(output.snapshot.url, "https://example.com/");
    }

    #[tokio::test]
    async fn browser_select_option_should_return_err_when_values_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_select_option(
            &driver,
            BrowserSelectOptionInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                values: vec![],
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty values should be rejected");

        assert_eq!(err, "values must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_select_option_should_pass_not_found_message_through_verbatim() {
        let driver = FakeBrowserDriver::new().with_select_option(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
                .to_string(),
        )));

        let err = browser_select_option(
            &driver,
            BrowserSelectOptionInput {
                session_id: "sess-9".to_string(),
                ref_id: "e1".to_string(),
                values: vec!["opt-a".to_string()],
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }

    // -- browser_press_key --------------------------------------------------------

    #[tokio::test]
    async fn browser_press_key_should_return_action_output_when_press_succeeds() {
        let driver = FakeBrowserDriver::new()
            .with_press_key(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_press_key(
            &driver,
            BrowserPressKeyInput {
                session_id: "sess-1".to_string(),
                key: "Enter".to_string(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect("press key should succeed");

        assert_eq!(output.snapshot.url, "https://example.com/");
    }

    #[tokio::test]
    async fn browser_press_key_should_return_err_when_key_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_press_key(
            &driver,
            BrowserPressKeyInput {
                session_id: "sess-1".to_string(),
                key: String::new(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty key should be rejected");

        assert_eq!(err, "key must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_press_key_should_pass_not_found_message_through_verbatim() {
        let driver = FakeBrowserDriver::new().with_press_key(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
                .to_string(),
        )));

        let err = browser_press_key(
            &driver,
            BrowserPressKeyInput {
                session_id: "sess-9".to_string(),
                key: "Enter".to_string(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }

    // -- browser_wait_for --------------------------------------------------------

    #[tokio::test]
    async fn browser_wait_for_should_return_action_output_when_text_condition_succeeds() {
        let driver =
            FakeBrowserDriver::new().with_wait_for(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_wait_for(
            &driver,
            BrowserWaitForInput {
                session_id: "sess-1".to_string(),
                text: Some("Loaded".to_string()),
                text_gone: None,
                time_ms: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect("wait for should succeed");

        assert_eq!(output.snapshot.url, "https://example.com/");
    }

    #[tokio::test]
    async fn browser_wait_for_should_return_err_when_zero_conditions_set() {
        let driver = FakeBrowserDriver::new();

        let err = browser_wait_for(
            &driver,
            BrowserWaitForInput {
                session_id: "sess-1".to_string(),
                text: None,
                text_gone: None,
                time_ms: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("zero conditions should be rejected");

        assert_eq!(err, "exactly one of text, textGone, or timeMs must be set");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_wait_for_should_return_err_when_multiple_conditions_set() {
        let driver = FakeBrowserDriver::new();

        let err = browser_wait_for(
            &driver,
            BrowserWaitForInput {
                session_id: "sess-1".to_string(),
                text: Some("Loaded".to_string()),
                text_gone: None,
                time_ms: Some(500),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("multiple conditions should be rejected");

        assert_eq!(err, "only one of text, textGone, or timeMs may be set");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_wait_for_should_pass_not_found_message_through_verbatim() {
        let driver = FakeBrowserDriver::new().with_wait_for(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
                .to_string(),
        )));

        let err = browser_wait_for(
            &driver,
            BrowserWaitForInput {
                session_id: "sess-9".to_string(),
                text: Some("Loaded".to_string()),
                text_gone: None,
                time_ms: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert_eq!(
            err,
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session"
        );
    }
}
