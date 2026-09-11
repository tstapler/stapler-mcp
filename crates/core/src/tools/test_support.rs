//! Shared `#[cfg(test)]`-only `BrowserDriver` test double for
//! `crates/core/src/tools`. `browser.rs`'s own tests need every
//! `BrowserDriver` method faked (all twelve browser-automation tools go
//! through it); `credential.rs`'s tests only need `type_secret`. Both used to
//! keep separate, near-identical copies of this fake — this one shared
//! definition (A4 code review fix) is what `credential.rs` now uses too, so
//! adding a new `BrowserDriver` method can't silently leave one test module's
//! fake out of sync with the other's.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Duration;

use crate::ports::{
    AxNode, AxSnapshot, BrowserDriver, CredentialRef, Locator, PageExtract, PortError, SessionId,
    TabAction, TabInfo,
};

pub(crate) struct FakeBrowserDriver {
    navigate_result: RefCell<Option<Result<crate::ports::NavigateResult, PortError>>>,
    click_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    type_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    type_secret_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    snapshot_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    /// Queue of results consumed in order, one per `close_session` call — a
    /// queue (rather than a single `Option`, like every other `*_result`
    /// field on this fake) so `browser_close_all_sessions` tests can drive
    /// distinct outcomes per session id.
    close_session_results: RefCell<VecDeque<Result<(), PortError>>>,
    list_sessions_result: RefCell<Option<Result<Vec<crate::ports::SessionSummary>, PortError>>>,
    hover_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    select_option_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    press_key_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    wait_for_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    screenshot_result: RefCell<Option<Result<Vec<u8>, PortError>>>,
    evaluate_result: RefCell<Option<Result<serde_json::Value, PortError>>>,
    /// Records the `(function, locator)` args each `evaluate()` call
    /// actually received, so tests can verify which branch a caller (e.g.
    /// `browser_get_html`) took instead of only checking the mock's
    /// pre-programmed return value. `pub(crate)`: `browser.rs`'s tests read
    /// this directly rather than through an accessor.
    pub(crate) evaluate_calls: RefCell<Vec<(String, Option<Locator>)>>,
    history_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    resize_result: RefCell<Option<Result<AxSnapshot, PortError>>>,
    /// Overrides whatever `tabs()` would otherwise compute from
    /// `tabs_state` — used to simulate driver-level failures (e.g. an
    /// unknown session) without disturbing the in-memory tab list.
    tabs_error: RefCell<Option<PortError>>,
    /// In-memory tab list `tabs()` operates on, seeded with a single tab by
    /// `new()` so `List`/`Close` have something realistic to act on.
    tabs_state: RefCell<Vec<TabInfo>>,
    active_tab_index: RefCell<usize>,
    /// `pub(crate)`: `browser.rs`'s tests assert on the exact call sequence
    /// directly rather than through an accessor.
    pub(crate) calls: RefCell<Vec<&'static str>>,
}

impl FakeBrowserDriver {
    pub(crate) fn new() -> Self {
        FakeBrowserDriver {
            navigate_result: RefCell::new(None),
            click_result: RefCell::new(None),
            type_result: RefCell::new(None),
            type_secret_result: RefCell::new(None),
            snapshot_result: RefCell::new(None),
            close_session_results: RefCell::new(VecDeque::new()),
            list_sessions_result: RefCell::new(None),
            hover_result: RefCell::new(None),
            select_option_result: RefCell::new(None),
            press_key_result: RefCell::new(None),
            wait_for_result: RefCell::new(None),
            screenshot_result: RefCell::new(None),
            evaluate_result: RefCell::new(None),
            evaluate_calls: RefCell::new(Vec::new()),
            history_result: RefCell::new(None),
            resize_result: RefCell::new(None),
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

    pub(crate) fn with_navigate(
        self,
        result: Result<crate::ports::NavigateResult, PortError>,
    ) -> Self {
        *self.navigate_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_click(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.click_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_type(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.type_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_type_secret(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.type_secret_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_snapshot(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.snapshot_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_close_session(self, result: Result<(), PortError>) -> Self {
        self.close_session_results.borrow_mut().push_back(result);
        self
    }

    pub(crate) fn with_list_sessions(
        self,
        result: Result<Vec<crate::ports::SessionSummary>, PortError>,
    ) -> Self {
        *self.list_sessions_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_hover(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.hover_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_select_option(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.select_option_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_press_key(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.press_key_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_wait_for(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.wait_for_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_screenshot(self, result: Result<Vec<u8>, PortError>) -> Self {
        *self.screenshot_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_evaluate(self, result: Result<serde_json::Value, PortError>) -> Self {
        *self.evaluate_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_history(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.history_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_resize(self, result: Result<AxSnapshot, PortError>) -> Self {
        *self.resize_result.borrow_mut() = Some(result);
        self
    }

    pub(crate) fn with_tabs_error(self, err: PortError) -> Self {
        *self.tabs_error.borrow_mut() = Some(err);
        self
    }

    pub(crate) fn with_tabs(self, tabs: Vec<TabInfo>) -> Self {
        *self.tabs_state.borrow_mut() = tabs;
        *self.active_tab_index.borrow_mut() = 0;
        self
    }

    pub(crate) fn call_count(&self) -> usize {
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

    async fn type_secret(
        &self,
        _session_id: &SessionId,
        _locator: &Locator,
        _credential_ref: &CredentialRef,
        _timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        self.calls.borrow_mut().push("type_secret");
        self.type_secret_result
            .borrow_mut()
            .take()
            .expect("type_secret result not configured")
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
        self.close_session_results
            .borrow_mut()
            .pop_front()
            .expect("close_session result not configured")
    }

    async fn list_sessions(&self) -> Result<Vec<crate::ports::SessionSummary>, PortError> {
        self.calls.borrow_mut().push("list_sessions");
        self.list_sessions_result
            .borrow_mut()
            .take()
            .expect("list_sessions result not configured")
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

    async fn screenshot(
        &self,
        _session_id: &SessionId,
        _full_page: bool,
        _timeout: Duration,
    ) -> Result<Vec<u8>, PortError> {
        self.calls.borrow_mut().push("screenshot");
        self.screenshot_result
            .borrow_mut()
            .take()
            .expect("screenshot result not configured")
    }

    async fn evaluate(
        &self,
        _session_id: &SessionId,
        function: &str,
        locator: Option<&Locator>,
        _timeout: Duration,
    ) -> Result<serde_json::Value, PortError> {
        self.calls.borrow_mut().push("evaluate");
        self.evaluate_calls
            .borrow_mut()
            .push((function.to_string(), locator.cloned()));
        self.evaluate_result
            .borrow_mut()
            .take()
            .expect("evaluate result not configured")
    }

    async fn history(
        &self,
        _session_id: &SessionId,
        _action: crate::ports::HistoryAction,
        _timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        self.calls.borrow_mut().push("history");
        self.history_result
            .borrow_mut()
            .take()
            .expect("history result not configured")
    }

    async fn resize(
        &self,
        _session_id: &SessionId,
        _width: u32,
        _height: u32,
        _timeout: Duration,
    ) -> Result<AxSnapshot, PortError> {
        self.calls.borrow_mut().push("resize");
        self.resize_result
            .borrow_mut()
            .take()
            .expect("resize result not configured")
    }
}

pub(crate) fn sample_node() -> AxNode {
    AxNode {
        node_ref: "e1".to_string(),
        role: "generic".to_string(),
        name: String::new(),
        value: None,
        children: Vec::new(),
    }
}

pub(crate) fn sample_snapshot(url: &str, navigated_from: Option<&str>) -> AxSnapshot {
    AxSnapshot {
        root: sample_node(),
        url: url.to_string(),
        truncated: false,
        navigated_from: navigated_from.map(|s| s.to_string()),
    }
}
