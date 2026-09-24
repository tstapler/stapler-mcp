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

use base64::Engine;

use crate::ports::{
    AxNode, AxSnapshot, BrowserDriver, FileStore, HistoryAction, Locator, PortError, SessionId,
    TabAction, TabInfo, WaitCondition,
};
use crate::schema::{
    AxNodeOutput, AxSnapshotOutput, BrowserActionOutput, BrowserClickInput,
    BrowserCloseAllSessionsOutput, BrowserCloseSessionFailure, BrowserCloseSessionInput,
    BrowserCloseSessionOutput, BrowserEvaluateInput, BrowserEvaluateOutput, BrowserFillFormInput,
    BrowserFindInput, BrowserFindMatch, BrowserFindOutput, BrowserFormFieldType,
    BrowserGetHtmlInput, BrowserGetHtmlOutput, BrowserHistoryAction, BrowserHistoryInput,
    BrowserHoverInput, BrowserListSessionsOutput, BrowserNavigateInput, BrowserNavigateOutput,
    BrowserPressKeyInput, BrowserResizeInput, BrowserScreenshotInput, BrowserScreenshotOutput,
    BrowserSelectOptionInput, BrowserSessionSummary, BrowserSetCheckedInput, BrowserSnapshotInput,
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

pub(crate) fn to_snapshot_output(snapshot: AxSnapshot) -> AxSnapshotOutput {
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

pub async fn browser_list_sessions<B: BrowserDriver>(
    browser: &B,
) -> Result<BrowserListSessionsOutput, String> {
    let sessions = browser
        .list_sessions()
        .await
        .map_err(|e| format!("list sessions: {e}"))?;

    Ok(BrowserListSessionsOutput {
        sessions: sessions
            .into_iter()
            .map(|s| BrowserSessionSummary {
                session_id: s.session_id,
                tab_count: s.tab_count,
                idle_ms: s.idle_ms,
                blocked: s.blocked,
                crashed: s.crashed,
            })
            .collect(),
    })
}

/// Best-effort: closes every currently live session concurrently (not one at
/// a time — a single wedged `close_session` call must never delay every
/// other session's close). A failure closing one session never aborts the
/// rest — every id is attempted and its outcome (closed or
/// failed-with-message) is reported back.
pub async fn browser_close_all_sessions<B: BrowserDriver>(
    browser: &B,
) -> Result<BrowserCloseAllSessionsOutput, String> {
    let sessions = browser
        .list_sessions()
        .await
        .map_err(|e| format!("list sessions: {e}"))?;

    let results = futures::future::join_all(sessions.into_iter().map(|session| async move {
        let session_id = SessionId(session.session_id.clone());
        (session.session_id, browser.close_session(&session_id).await)
    }))
    .await;

    let mut closed = Vec::new();
    let mut failed = Vec::new();
    for (session_id, result) in results {
        match result {
            Ok(()) => closed.push(session_id),
            Err(e) => {
                let error = map_error("close", &session_id, e);
                failed.push(BrowserCloseSessionFailure { session_id, error });
            }
        }
    }

    Ok(BrowserCloseAllSessionsOutput { closed, failed })
}

pub async fn browser_tabs<B: BrowserDriver>(
    browser: &B,
    input: BrowserTabsInput,
    policy: NetworkPolicy,
) -> Result<BrowserTabsOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    let action = match input.action {
        BrowserTabsAction::List => TabAction::List,
        BrowserTabsAction::New => {
            // Same preflight as `browser_navigate` (design/ux.md Error 5a):
            // without it, a literally-known-blocked URL (private IP,
            // metadata endpoint) would fire its real network request via
            // the driver's `page.goto` before any block detection runs,
            // unlike navigating an existing tab.
            if let Some(url) = input.url.as_deref().filter(|u| !u.is_empty()) {
                let parsed = url::Url::parse(url).map_err(|e| format!("invalid url: {e}"))?;
                if let Some(reason) = blocked_host_reason(&parsed, policy) {
                    return Err(format!("navigate blocked: {reason}"));
                }
            }
            TabAction::New {
                url: input.url.clone(),
            }
        }
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

pub async fn browser_screenshot<B: BrowserDriver, F: FileStore>(
    browser: &B,
    fs: &F,
    input: BrowserScreenshotInput,
) -> Result<BrowserScreenshotOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let full_page = input.full_page.unwrap_or(false);

    let png = browser
        .screenshot(&session_id, full_page, timeout)
        .await
        .map_err(|e| map_error("screenshot", &input.session_id, e))?;

    let mut out = BrowserScreenshotOutput {
        data_base64: None,
        saved_to: None,
        mime_type: "image/png".to_string(),
    };

    // Same save-xor-inline tradeoff as `fetch_page`'s `savePath`: a large
    // screenshot doesn't need to also be inlined as base64 into the tool
    // response once it's written to disk.
    if let Some(save_path) = input.save_path {
        fs.write_file(&save_path, &png)
            .await
            .map_err(|e| e.to_string())?;
        out.saved_to = Some(save_path);
    } else {
        out.data_base64 = Some(base64::engine::general_purpose::STANDARD.encode(&png));
    }

    Ok(out)
}

pub async fn browser_evaluate<B: BrowserDriver>(
    browser: &B,
    input: BrowserEvaluateInput,
) -> Result<BrowserEvaluateOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.function.is_empty() {
        return Err("function must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let locator = input.ref_id.clone().map(Locator);

    let result = browser
        .evaluate(&session_id, &input.function, locator.as_ref(), timeout)
        .await
        .map_err(|e| map_error("evaluate", &input.session_id, e))?;

    Ok(BrowserEvaluateOutput { result })
}

/// Complementary to `browser_snapshot`'s accessibility-tree view: returns the
/// page's (or, with `refId`, one element's) actual rendered HTML. Built on
/// top of `evaluate` rather than a new driver method — `outerHTML` is just
/// another JS expression, so there's no CDP call this needs that `evaluate`
/// doesn't already make.
pub async fn browser_get_html<B: BrowserDriver>(
    browser: &B,
    input: BrowserGetHtmlInput,
) -> Result<BrowserGetHtmlOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let locator = input.ref_id.clone().map(Locator);
    let function = if locator.is_some() {
        "(element) => element.outerHTML"
    } else {
        "() => document.documentElement.outerHTML"
    };

    let result = browser
        .evaluate(&session_id, function, locator.as_ref(), timeout)
        .await
        .map_err(|e| map_error("get html", &input.session_id, e))?;

    let html = result.as_str().map(str::to_string).ok_or_else(|| {
        format!(
            "get html {}: expected outerHTML to be a string, got {result}",
            input.session_id
        )
    })?;

    Ok(BrowserGetHtmlOutput { html })
}

/// Batch convenience over calling `stapler_browser_type`/
/// `stapler_browser_select_option` once per field — not a single atomic
/// driver call. Fields are filled in order; if one fails, earlier fields
/// remain filled and the error names which field failed.
pub async fn browser_fill_form<B: BrowserDriver>(
    browser: &B,
    input: BrowserFillFormInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.fields.is_empty() {
        return Err("fields must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());

    let mut snapshot = None;
    for field in &input.fields {
        if field.ref_id.is_empty() {
            return Err("refId must not be empty".to_string());
        }
        let locator = Locator(field.ref_id.clone());
        let result = match field.r#type {
            BrowserFormFieldType::Textbox => {
                browser
                    .type_text(&session_id, &locator, &field.value, timeout)
                    .await
            }
            BrowserFormFieldType::Combobox => {
                browser
                    .select_option(
                        &session_id,
                        &locator,
                        std::slice::from_ref(&field.value),
                        timeout,
                    )
                    .await
            }
            BrowserFormFieldType::Checkbox => {
                let checked = parse_checked_value(&field.value)
                    .map_err(|e| format!("field '{}': {e}", field.ref_id))?;
                set_checked(browser, &session_id, &locator, checked, timeout).await
            }
        };
        snapshot = Some(result.map_err(|e| {
            map_error(
                &format!("fill form field '{}'", field.ref_id),
                &input.session_id,
                e,
            )
        })?);
    }

    let snapshot = snapshot.expect("fields is non-empty, checked above");
    let note = snapshot.navigated_from.as_ref().map(|_| {
        format!(
            "fill form navigated to {}; previous element refs are now invalid",
            snapshot.url
        )
    });

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note,
    })
}

fn parse_checked_value(value: &str) -> Result<bool, String> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        other => Err(format!(
            "checkbox value must be \"true\" or \"false\", got \"{other}\""
        )),
    }
}

/// Reads `locator`'s current checked state via `evaluate` (checking `.checked`
/// for a native `<input type=checkbox/radio>` or `aria-checked` for a
/// custom-element checkbox role), then only dispatches a `click` if that
/// differs from `checked` — composing two existing `BrowserDriver` calls
/// rather than needing a new adapter-level primitive, since clicking an
/// already-`checked` checkbox would toggle it *off*. Returns a fresh
/// `snapshot()` when no click was needed, so the caller always gets a
/// current snapshot either way.
async fn set_checked<B: BrowserDriver>(
    browser: &B,
    session_id: &SessionId,
    locator: &Locator,
    checked: bool,
    timeout: Duration,
) -> Result<AxSnapshot, PortError> {
    const CHECKED_STATE_JS: &str =
        "(el) => el.checked === true || el.getAttribute('aria-checked') === 'true'";

    let current = browser
        .evaluate(session_id, CHECKED_STATE_JS, Some(locator), timeout)
        .await?;
    let already_checked = current.as_bool().unwrap_or(false);

    if already_checked == checked {
        browser.snapshot(session_id, timeout).await
    } else {
        browser.click(session_id, locator, timeout).await
    }
}

pub async fn browser_set_checked<B: BrowserDriver>(
    browser: &B,
    input: BrowserSetCheckedInput,
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

    let snapshot = set_checked(browser, &session_id, &locator, input.checked, timeout)
        .await
        .map_err(|e| map_error("set checked", &input.session_id, e))?;

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note: None,
    })
}

pub async fn browser_history<B: BrowserDriver>(
    browser: &B,
    input: BrowserHistoryInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());
    let action = match input.action {
        BrowserHistoryAction::Back => HistoryAction::Back,
        BrowserHistoryAction::Forward => HistoryAction::Forward,
        BrowserHistoryAction::Reload => HistoryAction::Reload,
    };

    let snapshot = browser
        .history(&session_id, action, timeout)
        .await
        .map_err(|e| map_error("history", &input.session_id, e))?;

    let note = snapshot.navigated_from.as_ref().map(|_| {
        format!(
            "history navigated to {}; previous element refs are now invalid",
            snapshot.url
        )
    });

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note,
    })
}

pub async fn browser_resize<B: BrowserDriver>(
    browser: &B,
    input: BrowserResizeInput,
) -> Result<BrowserActionOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.width == 0 || input.height == 0 {
        return Err("width and height must both be greater than 0".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());

    let snapshot = browser
        .resize(&session_id, input.width, input.height, timeout)
        .await
        .map_err(|e| map_error("resize", &input.session_id, e))?;

    Ok(BrowserActionOutput {
        snapshot: to_snapshot_output(snapshot),
        note: None,
    })
}

/// Caps how many matches are returned in one call — a broad query against a
/// large page shouldn't dump most of the tree back, defeating the point of
/// `find` being cheaper than a full `snapshot`.
const MAX_FIND_MATCHES: usize = 20;

pub async fn browser_find<B: BrowserDriver>(
    browser: &B,
    input: BrowserFindInput,
) -> Result<BrowserFindOutput, String> {
    if input.session_id.is_empty() {
        return Err("sessionId must not be empty".to_string());
    }
    if input.query.is_empty() {
        return Err("query must not be empty".to_string());
    }
    let timeout = resolve_timeout(input.timeout_seconds);
    let session_id = SessionId(input.session_id.clone());

    let snapshot = browser
        .snapshot(&session_id, timeout)
        .await
        .map_err(|e| map_error("find", &input.session_id, e))?;

    let query_lower = input.query.to_lowercase();
    let mut matches = Vec::new();
    let mut truncated = false;
    collect_find_matches(
        &snapshot.root,
        &query_lower,
        "",
        &mut matches,
        &mut truncated,
    );

    Ok(BrowserFindOutput { matches, truncated })
}

/// Walks the whole tree (not stopping at `MAX_FIND_MATCHES`) so `truncated`
/// accurately reflects whether more matches exist beyond the cap, matching
/// how `AxSnapshotOutput::truncated` is reported elsewhere in this file.
fn collect_find_matches(
    node: &AxNode,
    query_lower: &str,
    parent_path: &str,
    matches: &mut Vec<BrowserFindMatch>,
    truncated: &mut bool,
) {
    let path = if parent_path.is_empty() {
        node.role.clone()
    } else {
        format!("{parent_path} > {}", node.role)
    };

    if node.name.to_lowercase().contains(query_lower) {
        if matches.len() >= MAX_FIND_MATCHES {
            *truncated = true;
        } else {
            matches.push(BrowserFindMatch {
                node_ref: node.node_ref.clone(),
                role: node.role.clone(),
                name: node.name.clone(),
                path: path.clone(),
            });
        }
    }

    for child in &node.children {
        collect_find_matches(child, query_lower, &path, matches, truncated);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::ports::{CredentialField, CredentialRef};
    use crate::schema::BrowserFormField;
    use crate::tools::test_support::{sample_snapshot, FakeBrowserDriver};

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
            navigate_input("http://169.254.169.254/"),
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

    // -- type_secret (FakeBrowserDriver direct — no tool-layer wrapper yet,
    // added in Phase 5) --------------------------------------------------

    #[tokio::test]
    async fn fake_browser_driver_should_return_configured_result_when_type_secret_called() {
        let driver = FakeBrowserDriver::new().with_type_secret(Err(PortError::CredentialExpired(
            "TOTP code for example.com expired".to_string(),
        )));

        let err = driver
            .type_secret(
                &SessionId("sess-1".to_string()),
                &Locator("e1".to_string()),
                &CredentialRef {
                    domain: "example.com".to_string(),
                    field: CredentialField::Totp,
                },
                Duration::from_secs(5),
            )
            .await
            .expect_err("configured type_secret result should be an error");

        assert!(matches!(err, PortError::CredentialExpired(_)));
        assert_eq!(driver.call_count(), 1);
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
                    url: "http://169.254.169.254/".to_string(),
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

    // -- browser_list_sessions ---------------------------------------------

    #[tokio::test]
    async fn browser_list_sessions_should_return_sessions_when_sessions_exist() {
        let driver =
            FakeBrowserDriver::new().with_list_sessions(Ok(vec![crate::ports::SessionSummary {
                session_id: "sess-1".to_string(),
                tab_count: 2,
                idle_ms: 1_500,
                blocked: false,
                crashed: false,
            }]));

        let output = browser_list_sessions(&driver)
            .await
            .expect("list_sessions should succeed");

        assert_eq!(output.sessions.len(), 1);
        assert_eq!(output.sessions[0].session_id, "sess-1");
        assert_eq!(output.sessions[0].tab_count, 2);
        assert_eq!(output.sessions[0].idle_ms, 1_500);
        assert!(!output.sessions[0].blocked);
        assert!(!output.sessions[0].crashed);
    }

    #[tokio::test]
    async fn browser_list_sessions_should_surface_blocked_and_crashed_flags() {
        let driver =
            FakeBrowserDriver::new().with_list_sessions(Ok(vec![crate::ports::SessionSummary {
                session_id: "sess-1".to_string(),
                tab_count: 1,
                idle_ms: 0,
                blocked: true,
                crashed: true,
            }]));

        let output = browser_list_sessions(&driver)
            .await
            .expect("list_sessions should succeed");

        assert!(output.sessions[0].blocked);
        assert!(output.sessions[0].crashed);
    }

    #[tokio::test]
    async fn browser_list_sessions_should_propagate_error_when_driver_fails() {
        let driver =
            FakeBrowserDriver::new().with_list_sessions(Err(PortError::Other("boom".to_string())));

        let err = browser_list_sessions(&driver)
            .await
            .expect_err("driver failure should surface as an error");

        assert_eq!(err, "list sessions: boom");
    }

    #[tokio::test]
    async fn browser_list_sessions_should_return_empty_when_no_sessions_exist() {
        let driver = FakeBrowserDriver::new().with_list_sessions(Ok(vec![]));

        let output = browser_list_sessions(&driver)
            .await
            .expect("list_sessions should succeed");

        assert!(output.sessions.is_empty());
    }

    // -- browser_close_all_sessions -----------------------------------------

    #[tokio::test]
    async fn browser_close_all_sessions_should_report_closed_and_failed_when_one_of_each() {
        let driver = FakeBrowserDriver::new()
            .with_list_sessions(Ok(vec![
                crate::ports::SessionSummary {
                    session_id: "sess-1".to_string(),
                    tab_count: 1,
                    idle_ms: 0,
                    blocked: false,
                    crashed: false,
                },
                crate::ports::SessionSummary {
                    session_id: "sess-2".to_string(),
                    tab_count: 1,
                    idle_ms: 0,
                    blocked: false,
                    crashed: false,
                },
            ]))
            .with_close_session(Ok(()))
            .with_close_session(Err(PortError::NotFound(
                "no active browser session named 'sess-2'; call stapler_browser_navigate to start a new session"
                    .to_string(),
            )));

        let output = browser_close_all_sessions(&driver)
            .await
            .expect("close_all_sessions should succeed");

        assert_eq!(output.closed, vec!["sess-1".to_string()]);
        assert_eq!(output.failed.len(), 1);
        assert_eq!(output.failed[0].session_id, "sess-2");
        assert_eq!(
            output.failed[0].error,
            "no active browser session named 'sess-2'; call stapler_browser_navigate to start a new session"
        );
    }

    #[tokio::test]
    async fn browser_close_all_sessions_should_return_empty_when_no_sessions_exist() {
        let driver = FakeBrowserDriver::new().with_list_sessions(Ok(vec![]));

        let output = browser_close_all_sessions(&driver)
            .await
            .expect("close_all_sessions should succeed");

        assert!(output.closed.is_empty());
        assert!(output.failed.is_empty());
    }

    #[tokio::test]
    async fn browser_close_all_sessions_should_propagate_error_when_list_sessions_fails() {
        let driver =
            FakeBrowserDriver::new().with_list_sessions(Err(PortError::Other("boom".to_string())));

        let err = browser_close_all_sessions(&driver)
            .await
            .expect_err("driver failure should surface as an error");

        assert_eq!(err, "list sessions: boom");
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

        let output = browser_tabs(
            &driver,
            tabs_input(BrowserTabsAction::List),
            NetworkPolicy::Enforce,
        )
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

        let output = browser_tabs(&driver, input, NetworkPolicy::Enforce)
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

        let output = browser_tabs(&driver, input, NetworkPolicy::Enforce)
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

        let err = browser_tabs(&driver, input, NetworkPolicy::Enforce)
            .await
            .expect_err("out-of-range index should be rejected");

        assert!(
            err.contains("no tab at index 9"),
            "unexpected message: {err}"
        );
    }

    #[tokio::test]
    async fn browser_tabs_should_return_err_when_select_index_missing() {
        let driver = FakeBrowserDriver::new();

        let err = browser_tabs(
            &driver,
            tabs_input(BrowserTabsAction::Select),
            NetworkPolicy::Enforce,
        )
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

        let output = browser_tabs(&driver, input, NetworkPolicy::Enforce)
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

        let err = browser_tabs(&driver, input, NetworkPolicy::Enforce)
            .await
            .expect_err("out-of-range index should be rejected");

        assert!(
            err.contains("no tab at index 9"),
            "unexpected message: {err}"
        );
    }

    #[tokio::test]
    async fn browser_tabs_should_return_err_when_closing_the_last_remaining_tab() {
        let driver = FakeBrowserDriver::new();

        let err = browser_tabs(
            &driver,
            tabs_input(BrowserTabsAction::Close),
            NetworkPolicy::Enforce,
        )
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

        let err = browser_tabs(&driver, input, NetworkPolicy::Enforce)
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

        let err = browser_tabs(
            &driver,
            tabs_input(BrowserTabsAction::List),
            NetworkPolicy::Enforce,
        )
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
        let driver = FakeBrowserDriver::new()
            .with_wait_for(Ok(sample_snapshot("https://example.com/", None)));

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

    // -- browser_screenshot ------------------------------------------------------

    /// Captures the last `write_file` call's path/bytes — `browser_screenshot`
    /// only ever calls `write_file`, never `read_file`/`delete_file`, so
    /// that's all this needs (unlike `docs.rs`'s fuller `InMemoryFileStore`).
    struct FakeFileStore {
        last_write: RefCell<Option<(String, Vec<u8>)>>,
    }

    impl FakeFileStore {
        fn new() -> Self {
            FakeFileStore {
                last_write: RefCell::new(None),
            }
        }
    }

    impl crate::ports::FileStore for FakeFileStore {
        async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
            *self.last_write.borrow_mut() = Some((path.to_string(), bytes.to_vec()));
            Ok(())
        }

        async fn read_file(&self, _path: &str) -> Result<Option<Vec<u8>>, PortError> {
            panic!("not exercised by this test");
        }

        async fn delete_file(&self, _path: &str) -> Result<(), PortError> {
            panic!("not exercised by this test");
        }
    }

    #[tokio::test]
    async fn browser_screenshot_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();
        let fs = FakeFileStore::new();

        let err = browser_screenshot(
            &driver,
            &fs,
            BrowserScreenshotInput {
                session_id: String::new(),
                full_page: None,
                save_path: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_screenshot_should_return_base64_data_when_save_path_omitted() {
        let driver = FakeBrowserDriver::new().with_screenshot(Ok(vec![1, 2, 3, 4]));
        let fs = FakeFileStore::new();

        let output = browser_screenshot(
            &driver,
            &fs,
            BrowserScreenshotInput {
                session_id: "sess-1".to_string(),
                full_page: None,
                save_path: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect("screenshot should succeed");

        assert_eq!(output.data_base64.as_deref(), Some("AQIDBA=="));
        assert_eq!(output.saved_to, None);
        assert_eq!(output.mime_type, "image/png");
        assert!(fs.last_write.borrow().is_none());
    }

    #[tokio::test]
    async fn browser_screenshot_should_save_to_file_and_omit_data_when_save_path_given() {
        let driver = FakeBrowserDriver::new().with_screenshot(Ok(vec![9, 9, 9]));
        let fs = FakeFileStore::new();

        let output = browser_screenshot(
            &driver,
            &fs,
            BrowserScreenshotInput {
                session_id: "sess-1".to_string(),
                full_page: Some(true),
                save_path: Some("/tmp/shot.png".to_string()),
                timeout_seconds: None,
            },
        )
        .await
        .expect("screenshot should succeed");

        assert_eq!(output.data_base64, None);
        assert_eq!(output.saved_to.as_deref(), Some("/tmp/shot.png"));
        assert_eq!(
            *fs.last_write.borrow(),
            Some(("/tmp/shot.png".to_string(), vec![9, 9, 9]))
        );
    }

    #[tokio::test]
    async fn browser_screenshot_should_pass_not_found_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_screenshot(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session".to_string(),
        )));
        let fs = FakeFileStore::new();

        let err = browser_screenshot(
            &driver,
            &fs,
            BrowserScreenshotInput {
                session_id: "sess-9".to_string(),
                full_page: None,
                save_path: None,
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

    // -- browser_evaluate ---------------------------------------------------------

    #[tokio::test]
    async fn browser_evaluate_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_evaluate(
            &driver,
            BrowserEvaluateInput {
                session_id: String::new(),
                function: "() => 1".to_string(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_evaluate_should_return_err_when_function_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_evaluate(
            &driver,
            BrowserEvaluateInput {
                session_id: "sess-1".to_string(),
                function: String::new(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty function should be rejected");

        assert_eq!(err, "function must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_evaluate_should_return_driver_result_as_output() {
        let driver = FakeBrowserDriver::new().with_evaluate(Ok(serde_json::json!({"n": 42})));

        let output = browser_evaluate(
            &driver,
            BrowserEvaluateInput {
                session_id: "sess-1".to_string(),
                function: "() => ({n: 42})".to_string(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect("evaluate should succeed");

        assert_eq!(output.result, serde_json::json!({"n": 42}));
    }

    #[tokio::test]
    async fn browser_evaluate_should_pass_not_found_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_evaluate(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session".to_string(),
        )));

        let err = browser_evaluate(
            &driver,
            BrowserEvaluateInput {
                session_id: "sess-9".to_string(),
                function: "() => 1".to_string(),
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

    // -- browser_get_html ---------------------------------------------------------

    #[tokio::test]
    async fn browser_get_html_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_get_html(
            &driver,
            BrowserGetHtmlInput {
                session_id: String::new(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_get_html_should_return_whole_page_html_when_ref_id_is_omitted() {
        let driver =
            FakeBrowserDriver::new().with_evaluate(Ok(serde_json::json!("<html>page</html>")));

        let output = browser_get_html(
            &driver,
            BrowserGetHtmlInput {
                session_id: "sess-1".to_string(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect("get_html should succeed");

        assert_eq!(output.html, "<html>page</html>");
        assert_eq!(*driver.calls.borrow(), vec!["evaluate"]);
        assert_eq!(
            *driver.evaluate_calls.borrow(),
            vec![("() => document.documentElement.outerHTML".to_string(), None)]
        );
    }

    #[tokio::test]
    async fn browser_get_html_should_return_element_outer_html_when_ref_id_is_given() {
        let driver =
            FakeBrowserDriver::new().with_evaluate(Ok(serde_json::json!("<button>Go</button>")));

        let output = browser_get_html(
            &driver,
            BrowserGetHtmlInput {
                session_id: "sess-1".to_string(),
                ref_id: Some("ref-1".to_string()),
                timeout_seconds: None,
            },
        )
        .await
        .expect("get_html should succeed");

        assert_eq!(output.html, "<button>Go</button>");
        assert_eq!(
            *driver.evaluate_calls.borrow(),
            vec![(
                "(element) => element.outerHTML".to_string(),
                Some(Locator("ref-1".to_string()))
            )]
        );
    }

    #[tokio::test]
    async fn browser_get_html_should_format_timeout_error_with_verb_and_session_id() {
        let driver = FakeBrowserDriver::new().with_evaluate(Err(PortError::Timeout));

        let err = browser_get_html(
            &driver,
            BrowserGetHtmlInput {
                session_id: "sess-1".to_string(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("timeout should surface as an error");

        assert_eq!(err, "get html sess-1: timed out");
    }

    #[tokio::test]
    async fn browser_get_html_should_return_err_when_evaluate_result_is_not_a_string() {
        let driver = FakeBrowserDriver::new().with_evaluate(Ok(serde_json::json!(null)));

        let err = browser_get_html(
            &driver,
            BrowserGetHtmlInput {
                session_id: "sess-1".to_string(),
                ref_id: None,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("non-string evaluate result should be rejected");

        assert_eq!(
            err,
            "get html sess-1: expected outerHTML to be a string, got null"
        );
    }

    #[tokio::test]
    async fn browser_get_html_should_pass_not_found_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_evaluate(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session".to_string(),
        )));

        let err = browser_get_html(
            &driver,
            BrowserGetHtmlInput {
                session_id: "sess-9".to_string(),
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

    // -- browser_fill_form ---------------------------------------------------------

    fn form_field(ref_id: &str, kind: BrowserFormFieldType, value: &str) -> BrowserFormField {
        BrowserFormField {
            ref_id: ref_id.to_string(),
            r#type: kind,
            value: value.to_string(),
        }
    }

    #[tokio::test]
    async fn browser_fill_form_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_fill_form(
            &driver,
            BrowserFillFormInput {
                session_id: String::new(),
                fields: vec![form_field("e1", BrowserFormFieldType::Textbox, "hi")],
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_fill_form_should_return_err_when_fields_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_fill_form(
            &driver,
            BrowserFillFormInput {
                session_id: "sess-1".to_string(),
                fields: vec![],
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty fields should be rejected");

        assert_eq!(err, "fields must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_fill_form_should_return_err_when_a_field_ref_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_fill_form(
            &driver,
            BrowserFillFormInput {
                session_id: "sess-1".to_string(),
                fields: vec![form_field("", BrowserFormFieldType::Textbox, "hi")],
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty refId should be rejected");

        assert_eq!(err, "refId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_fill_form_should_call_type_text_and_select_option_for_mixed_fields() {
        let driver = FakeBrowserDriver::new()
            .with_type(Ok(sample_snapshot("https://example.com/", None)))
            .with_select_option(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_fill_form(
            &driver,
            BrowserFillFormInput {
                session_id: "sess-1".to_string(),
                fields: vec![
                    form_field("e1", BrowserFormFieldType::Textbox, "hello"),
                    form_field("e2", BrowserFormFieldType::Combobox, "opt-a"),
                ],
                timeout_seconds: None,
            },
        )
        .await
        .expect("fill form should succeed");

        assert_eq!(*driver.calls.borrow(), vec!["type_text", "select_option"]);
        assert_eq!(output.snapshot.url, "https://example.com/");
    }

    #[tokio::test]
    async fn browser_fill_form_should_wrap_field_error_with_ref_id() {
        let driver = FakeBrowserDriver::new().with_type(Err(PortError::Timeout));

        let err = browser_fill_form(
            &driver,
            BrowserFillFormInput {
                session_id: "sess-1".to_string(),
                fields: vec![form_field("e1", BrowserFormFieldType::Textbox, "hello")],
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("timeout should surface as an error");

        assert_eq!(err, "fill form field 'e1' sess-1: timed out");
    }

    // -- browser_set_checked ---------------------------------------------------

    #[tokio::test]
    async fn browser_set_checked_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_set_checked(
            &driver,
            BrowserSetCheckedInput {
                session_id: String::new(),
                ref_id: "e1".to_string(),
                checked: true,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_set_checked_should_return_err_when_ref_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_set_checked(
            &driver,
            BrowserSetCheckedInput {
                session_id: "sess-1".to_string(),
                ref_id: String::new(),
                checked: true,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty refId should be rejected");

        assert_eq!(err, "refId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_set_checked_should_click_when_current_state_differs() {
        let driver = FakeBrowserDriver::new()
            .with_evaluate(Ok(serde_json::json!(false)))
            .with_click(Ok(sample_snapshot("https://example.com/", None)));

        browser_set_checked(
            &driver,
            BrowserSetCheckedInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                checked: true,
                timeout_seconds: None,
            },
        )
        .await
        .expect("set_checked should succeed");

        assert_eq!(*driver.calls.borrow(), vec!["evaluate", "click"]);
    }

    #[tokio::test]
    async fn browser_set_checked_should_not_click_when_current_state_already_matches() {
        let driver = FakeBrowserDriver::new()
            .with_evaluate(Ok(serde_json::json!(true)))
            .with_snapshot(Ok(sample_snapshot("https://example.com/", None)));

        browser_set_checked(
            &driver,
            BrowserSetCheckedInput {
                session_id: "sess-1".to_string(),
                ref_id: "e1".to_string(),
                checked: true,
                timeout_seconds: None,
            },
        )
        .await
        .expect("set_checked should succeed");

        assert_eq!(*driver.calls.borrow(), vec!["evaluate", "snapshot"]);
    }

    #[tokio::test]
    async fn browser_set_checked_should_pass_not_found_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_evaluate(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session".to_string(),
        )));

        let err = browser_set_checked(
            &driver,
            BrowserSetCheckedInput {
                session_id: "sess-9".to_string(),
                ref_id: "e1".to_string(),
                checked: true,
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

    // -- browser_fill_form (checkbox) --------------------------------------------

    #[tokio::test]
    async fn browser_fill_form_should_return_err_when_checkbox_value_is_not_true_or_false() {
        let driver = FakeBrowserDriver::new();

        let err = browser_fill_form(
            &driver,
            BrowserFillFormInput {
                session_id: "sess-1".to_string(),
                fields: vec![form_field("e1", BrowserFormFieldType::Checkbox, "yes")],
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("non-boolean checkbox value should be rejected");

        assert_eq!(
            err,
            "field 'e1': checkbox value must be \"true\" or \"false\", got \"yes\""
        );
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_fill_form_should_set_checkbox_via_evaluate_and_click() {
        let driver = FakeBrowserDriver::new()
            .with_evaluate(Ok(serde_json::json!(false)))
            .with_click(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_fill_form(
            &driver,
            BrowserFillFormInput {
                session_id: "sess-1".to_string(),
                fields: vec![form_field("e1", BrowserFormFieldType::Checkbox, "true")],
                timeout_seconds: None,
            },
        )
        .await
        .expect("fill form should succeed");

        assert_eq!(*driver.calls.borrow(), vec!["evaluate", "click"]);
        assert_eq!(output.snapshot.url, "https://example.com/");
    }

    // -- browser_history ----------------------------------------------------------

    #[tokio::test]
    async fn browser_history_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_history(
            &driver,
            BrowserHistoryInput {
                session_id: String::new(),
                action: BrowserHistoryAction::Back,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_history_should_set_note_when_history_action_navigates() {
        let driver = FakeBrowserDriver::new().with_history(Ok(sample_snapshot(
            "https://example.com/previous",
            Some("https://example.com/current"),
        )));

        let output = browser_history(
            &driver,
            BrowserHistoryInput {
                session_id: "sess-1".to_string(),
                action: BrowserHistoryAction::Back,
                timeout_seconds: None,
            },
        )
        .await
        .expect("history should succeed");

        assert_eq!(
            output.note,
            Some(
                "history navigated to https://example.com/previous; previous element refs are now invalid"
                    .to_string()
            )
        );
    }

    #[tokio::test]
    async fn browser_history_should_pass_not_found_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_history(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session".to_string(),
        )));

        let err = browser_history(
            &driver,
            BrowserHistoryInput {
                session_id: "sess-9".to_string(),
                action: BrowserHistoryAction::Reload,
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

    // -- browser_resize ------------------------------------------------------------

    #[tokio::test]
    async fn browser_resize_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_resize(
            &driver,
            BrowserResizeInput {
                session_id: String::new(),
                width: 1024,
                height: 768,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_resize_should_return_err_when_width_or_height_is_zero() {
        let driver = FakeBrowserDriver::new();

        let err = browser_resize(
            &driver,
            BrowserResizeInput {
                session_id: "sess-1".to_string(),
                width: 0,
                height: 768,
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("zero width should be rejected");

        assert_eq!(err, "width and height must both be greater than 0");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_resize_should_return_action_output_when_resize_succeeds() {
        let driver =
            FakeBrowserDriver::new().with_resize(Ok(sample_snapshot("https://example.com/", None)));

        let output = browser_resize(
            &driver,
            BrowserResizeInput {
                session_id: "sess-1".to_string(),
                width: 1024,
                height: 768,
                timeout_seconds: None,
            },
        )
        .await
        .expect("resize should succeed");

        assert_eq!(output.snapshot.url, "https://example.com/");
        assert_eq!(output.note, None);
    }

    #[tokio::test]
    async fn browser_resize_should_pass_not_found_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_resize(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session".to_string(),
        )));

        let err = browser_resize(
            &driver,
            BrowserResizeInput {
                session_id: "sess-9".to_string(),
                width: 1024,
                height: 768,
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

    // -- browser_find ---------------------------------------------------------------

    fn node(role: &str, name: &str, node_ref: &str, children: Vec<AxNode>) -> AxNode {
        AxNode {
            node_ref: node_ref.to_string(),
            role: role.to_string(),
            name: name.to_string(),
            value: None,
            children,
        }
    }

    #[tokio::test]
    async fn browser_find_should_return_err_when_session_id_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_find(
            &driver,
            BrowserFindInput {
                session_id: String::new(),
                query: "submit".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty sessionId should be rejected");

        assert_eq!(err, "sessionId must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_find_should_return_err_when_query_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_find(
            &driver,
            BrowserFindInput {
                session_id: "sess-1".to_string(),
                query: String::new(),
                timeout_seconds: None,
            },
        )
        .await
        .expect_err("empty query should be rejected");

        assert_eq!(err, "query must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_find_should_return_matching_nodes_case_insensitively_with_path() {
        let tree = node(
            "generic",
            "",
            "e1",
            vec![node(
                "list",
                "",
                "e2",
                vec![node(
                    "listitem",
                    "",
                    "e3",
                    vec![node("link", "Submit Order", "e4", vec![])],
                )],
            )],
        );
        let snapshot = AxSnapshot {
            root: tree,
            url: "https://example.com/".to_string(),
            truncated: false,
            navigated_from: None,
        };
        let driver = FakeBrowserDriver::new().with_snapshot(Ok(snapshot));

        let output = browser_find(
            &driver,
            BrowserFindInput {
                session_id: "sess-1".to_string(),
                query: "submit".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect("find should succeed");

        assert_eq!(output.matches.len(), 1);
        assert_eq!(output.matches[0].node_ref, "e4");
        assert_eq!(output.matches[0].name, "Submit Order");
        assert_eq!(output.matches[0].path, "generic > list > listitem > link");
        assert!(!output.truncated);
    }

    #[tokio::test]
    async fn browser_find_should_truncate_and_set_truncated_true_when_matches_exceed_cap() {
        let children: Vec<AxNode> = (0..(MAX_FIND_MATCHES + 5))
            .map(|i| node("button", "Click me", &format!("e{i}"), vec![]))
            .collect();
        let snapshot = AxSnapshot {
            root: node("generic", "", "root", children),
            url: "https://example.com/".to_string(),
            truncated: false,
            navigated_from: None,
        };
        let driver = FakeBrowserDriver::new().with_snapshot(Ok(snapshot));

        let output = browser_find(
            &driver,
            BrowserFindInput {
                session_id: "sess-1".to_string(),
                query: "click".to_string(),
                timeout_seconds: None,
            },
        )
        .await
        .expect("find should succeed");

        assert_eq!(output.matches.len(), MAX_FIND_MATCHES);
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn browser_find_should_pass_not_found_error_through_unchanged() {
        let driver = FakeBrowserDriver::new().with_snapshot(Err(PortError::NotFound(
            "no active browser session named 'sess-9'; call stapler_browser_navigate to start a new session".to_string(),
        )));

        let err = browser_find(
            &driver,
            BrowserFindInput {
                session_id: "sess-9".to_string(),
                query: "submit".to_string(),
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
