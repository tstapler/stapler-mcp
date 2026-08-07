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

use crate::ports::{AxNode, AxSnapshot, BrowserDriver, Locator, PortError, SessionId};
use crate::schema::{
    AxNodeOutput, AxSnapshotOutput, BrowserActionOutput, BrowserClickInput, BrowserNavigateInput,
    BrowserNavigateOutput, BrowserSnapshotInput, BrowserTypeInput,
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
            other => format!("snapshot {}: {other}", input.session_id),
        })?;

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
        calls: RefCell<Vec<&'static str>>,
    }

    impl FakeBrowserDriver {
        fn new() -> Self {
            FakeBrowserDriver {
                navigate_result: RefCell::new(None),
                click_result: RefCell::new(None),
                type_result: RefCell::new(None),
                snapshot_result: RefCell::new(None),
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

        let output = browser_navigate(&driver, navigate_input("https://example.com/"), NetworkPolicy::Enforce)
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
        let driver = FakeBrowserDriver::new()
            .with_type(Ok(sample_snapshot("https://example.com/", None)));

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

    // -- UX acceptance tests --------------------------------------------------

    #[tokio::test]
    async fn ux_ac2_session_not_found_error_should_name_session_id_and_corrective_call() {
        let driver = FakeBrowserDriver::new()
            .with_snapshot(Err(PortError::NotFound("whatever the driver says".to_string())));

        let err = browser_snapshot(
            &driver,
            BrowserSnapshotInput {
                session_id: "sess-bogus".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("not-found should surface as an error");

        assert!(err.contains("sess-bogus"), "message should name the session id: {err}");
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

        let note = output.note.expect("note should be set when click navigates");
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
            let driver =
                FakeBrowserDriver::new().with_snapshot(Err(PortError::NotFound("sess-1".to_string())));
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
            note: Some("click navigated to https://example.com/; previous element refs are now invalid".to_string()),
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
}
