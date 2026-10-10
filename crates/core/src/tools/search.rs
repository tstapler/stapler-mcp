use std::time::Duration;

use serde::Deserialize;

use crate::ports::{BrowserDriver, HttpClient, SessionId};
use crate::schema::{BraveSearchInput, BraveSearchOutput, BraveSearchResult};
use crate::tools::browser::map_error;

pub const DEFAULT_BASE_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// Shared by `brave_web_search` and `browser_web_search`: default to 10,
/// cap at 20.
fn clamp_search_count(count: Option<u32>) -> u32 {
    match count {
        None | Some(0) => 10,
        Some(c) if c > 20 => 20,
        Some(c) => c,
    }
}

#[derive(Deserialize)]
struct ApiResult {
    title: String,
    url: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize, Default)]
struct ApiWeb {
    #[serde(default)]
    results: Vec<ApiResult>,
}

#[derive(Deserialize)]
struct ApiResponse {
    #[serde(default)]
    web: Option<ApiWeb>,
}

pub async fn brave_web_search<H: HttpClient>(
    http: &H,
    api_key: &str,
    base_url: &str,
    input: BraveSearchInput,
) -> Result<BraveSearchOutput, String> {
    if api_key.is_empty() {
        return Err("BRAVE_API_KEY is not set".to_string());
    }
    if input.query.is_empty() {
        return Err("query must not be empty".to_string());
    }
    let count = clamp_search_count(input.count);

    let url = build_search_url(base_url, &input.query, count);
    let headers = [
        ("Accept".to_string(), "application/json".to_string()),
        ("X-Subscription-Token".to_string(), api_key.to_string()),
    ];

    let resp = http.get(&url, &headers).await.map_err(|e| e.to_string())?;
    if resp.status != 200 {
        let body = String::from_utf8_lossy(&resp.body);
        return Err(format!("brave search: HTTP {}: {body}", resp.status));
    }

    let parsed: ApiResponse = serde_json::from_slice(&resp.body).map_err(|e| e.to_string())?;
    let results = parsed
        .web
        .unwrap_or_default()
        .results
        .into_iter()
        .map(|r| BraveSearchResult {
            title: r.title,
            url: r.url,
            description: r.description,
        })
        .collect();
    Ok(BraveSearchOutput { results })
}

fn build_search_url(base_url: &str, query: &str, count: u32) -> String {
    let qs: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("q", query)
        .append_pair("count", &count.to_string())
        .finish();
    format!("{base_url}?{qs}")
}

const BROWSER_SEARCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Bing, not DuckDuckGo as issue #46 originally suggested: DuckDuckGo's bot
/// challenge blocked headless/headed Chrome and plain `curl` alike from this
/// environment, while Bing returned real results every time — see the PR
/// description for the empirical comparison.
const SEARCH_BASE_URL: &str = "https://www.bing.com/search";

fn build_bing_search_url(query: &str) -> String {
    let qs: String = form_urlencoded::Serializer::new(String::new())
        .append_pair("q", query)
        .finish();
    format!("{SEARCH_BASE_URL}?{qs}")
}

/// Bing wraps each result's link in a `bing.com/ck/a` click-tracking
/// redirect; the real target is base64url-encoded in its `u` query param
/// (prefixed with a literal `"a1"`), decoded here so callers get the actual
/// destination rather than a tracking link.
const EXTRACT_BING_RESULTS_JS: &str = r#"() => {
    function decodeBingUrl(href) {
        try {
            const params = new URL(href).searchParams;
            const u = params.get('u');
            if (u && u.startsWith('a1')) {
                let b64 = u.slice(2).replace(/-/g, '+').replace(/_/g, '/');
                while (b64.length % 4) {
                    b64 += '=';
                }
                return atob(b64);
            }
        } catch (e) {}
        return href;
    }
    return Array.from(document.querySelectorAll('li.b_algo')).map((el) => {
        const a = el.querySelector('h2 a');
        const caption = el.querySelector('.b_caption, .b_snippet');
        return {
            title: a ? a.textContent.trim() : '',
            url: a ? decodeBingUrl(a.href) : '',
            description: caption ? caption.textContent.trim().replace(/\s+/g, ' ') : '',
        };
    }).filter((r) => r.url);
}"#;

#[derive(Deserialize)]
struct RawBrowserResult {
    title: String,
    url: String,
    #[serde(default)]
    description: String,
}

/// Keyless fallback for `brave_web_search` (issue #46): scrapes Bing's
/// results page in a throwaway browser session instead of calling an API.
/// Mirrors `BraveSearchInput`/`BraveSearchOutput` so callers can route
/// through either tool interchangeably.
pub async fn browser_web_search<B: BrowserDriver>(
    browser: &B,
    input: BraveSearchInput,
) -> Result<BraveSearchOutput, String> {
    if input.query.is_empty() {
        return Err("query must not be empty".to_string());
    }
    let count = clamp_search_count(input.count) as usize;

    let value = run_search_session(browser, &input.query).await?;
    let raw: Vec<RawBrowserResult> = serde_json::from_value(value)
        .map_err(|e| format!("browser web search: unexpected result shape: {e}"))?;

    Ok(BraveSearchOutput {
        results: raw.into_iter().take(count).map(to_brave_result).collect(),
    })
}

fn to_brave_result(r: RawBrowserResult) -> BraveSearchResult {
    BraveSearchResult {
        title: r.title,
        url: r.url,
        description: r.description,
    }
}

/// Opens one session, extracts results, and closes the session again —
/// closing runs even when `evaluate` fails, so a search never leaves
/// residue in `stapler_browser_list_sessions`. A `navigate` failure needs
/// no such cleanup: no session exists yet in that case.
async fn run_search_session<B: BrowserDriver>(
    browser: &B,
    query: &str,
) -> Result<serde_json::Value, String> {
    let url = build_bing_search_url(query);
    let nav = browser
        .navigate(&url, None, BROWSER_SEARCH_TIMEOUT)
        .await
        .map_err(|e| map_error("browser web search", &url, e))?;

    let eval_result = browser
        .evaluate(
            &nav.session_id,
            EXTRACT_BING_RESULTS_JS,
            None,
            BROWSER_SEARCH_TIMEOUT,
        )
        .await;
    close_search_session(browser, &nav.session_id).await;

    eval_result.map_err(|e| map_error("browser web search", &nav.session_id.0, e))
}

/// Best-effort: a failure to close the per-search session is never surfaced
/// as a tool error (the search itself may have already succeeded), but it
/// would leave residue in `stapler_browser_list_sessions` — logged to stderr
/// so it's at least visible to the daemon operator.
async fn close_search_session<B: BrowserDriver>(browser: &B, session_id: &SessionId) {
    if let Err(e) = browser.close_session(session_id).await {
        eprintln!("stapler-mcp: browser web search: failed to close session: {e}");
    }
}

#[cfg(test)]
mod browser_web_search_tests {
    use super::*;
    use crate::ports::{NavigateResult, PortError};
    use crate::tools::test_support::{sample_snapshot, FakeBrowserDriver};

    fn input(query: &str, count: Option<u32>) -> BraveSearchInput {
        BraveSearchInput {
            query: query.to_string(),
            count,
        }
    }

    fn navigated(session_id: &str) -> Result<NavigateResult, PortError> {
        Ok(NavigateResult {
            session_id: SessionId(session_id.to_string()),
            final_url: "https://www.bing.com/search?q=rust".to_string(),
            snapshot: sample_snapshot("https://www.bing.com/search?q=rust", None),
        })
    }

    fn results_json(n: usize) -> serde_json::Value {
        let items: Vec<_> = (0..n)
            .map(|i| {
                serde_json::json!({
                    "title": format!("Result {i}"),
                    "url": format!("https://example.com/{i}"),
                    "description": format!("Description {i}"),
                })
            })
            .collect();
        serde_json::Value::Array(items)
    }

    #[test]
    fn build_bing_search_url_should_url_encode_query_and_keep_fixed_host() {
        let url = build_bing_search_url("rust tokio");

        assert_eq!(url, "https://www.bing.com/search?q=rust+tokio");
    }

    #[tokio::test]
    async fn browser_web_search_should_return_err_when_query_is_empty() {
        let driver = FakeBrowserDriver::new();

        let err = browser_web_search(&driver, input("", None))
            .await
            .expect_err("empty query should be rejected");

        assert_eq!(err, "query must not be empty");
        assert_eq!(driver.call_count(), 0);
    }

    #[tokio::test]
    async fn browser_web_search_should_return_shaped_results_when_search_succeeds() {
        let driver = FakeBrowserDriver::new()
            .with_navigate(navigated("sess-1"))
            .with_evaluate(Ok(results_json(3)))
            .with_close_session(Ok(()));

        let output = browser_web_search(&driver, input("rust tokio", None))
            .await
            .expect("search should succeed");

        assert_eq!(output.results.len(), 3);
        assert_eq!(output.results[0].title, "Result 0");
        assert_eq!(output.results[0].url, "https://example.com/0");
        assert_eq!(output.results[0].description, "Description 0");
        assert_eq!(
            driver.calls.borrow().as_slice(),
            &["navigate", "evaluate", "close_session"]
        );
    }

    #[tokio::test]
    async fn browser_web_search_should_clamp_count_to_20_when_input_count_exceeds_max() {
        let driver = FakeBrowserDriver::new()
            .with_navigate(navigated("sess-1"))
            .with_evaluate(Ok(results_json(30)))
            .with_close_session(Ok(()));

        let output = browser_web_search(&driver, input("rust", Some(99)))
            .await
            .expect("search should succeed");

        assert_eq!(output.results.len(), 20);
    }

    #[tokio::test]
    async fn browser_web_search_should_default_count_to_10_when_input_count_is_zero_or_omitted() {
        let driver = FakeBrowserDriver::new()
            .with_navigate(navigated("sess-1"))
            .with_evaluate(Ok(results_json(30)))
            .with_close_session(Ok(()));

        let output = browser_web_search(&driver, input("rust", Some(0)))
            .await
            .expect("search should succeed");

        assert_eq!(output.results.len(), 10);
    }

    #[tokio::test]
    async fn browser_web_search_should_propagate_navigate_error_without_calling_close_session() {
        let driver =
            FakeBrowserDriver::new().with_navigate(Err(PortError::Other("boom".to_string())));

        let err = browser_web_search(&driver, input("rust", None))
            .await
            .expect_err("navigate failure should surface as an error");

        assert!(err.contains("boom"), "unexpected message: {err}");
        assert_eq!(driver.calls.borrow().as_slice(), &["navigate"]);
    }

    #[tokio::test]
    async fn browser_web_search_should_close_session_and_propagate_error_when_evaluate_fails() {
        let driver = FakeBrowserDriver::new()
            .with_navigate(navigated("sess-1"))
            .with_evaluate(Err(PortError::Timeout))
            .with_close_session(Ok(()));

        let err = browser_web_search(&driver, input("rust", None))
            .await
            .expect_err("evaluate failure should surface as an error");

        assert!(err.contains("timed out"), "unexpected message: {err}");
        assert_eq!(
            driver.calls.borrow().as_slice(),
            &["navigate", "evaluate", "close_session"],
            "the per-search session must still be closed even when evaluate fails"
        );
    }

    #[tokio::test]
    async fn browser_web_search_should_return_ok_results_when_close_session_fails_after_successful_evaluate(
    ) {
        let driver = FakeBrowserDriver::new()
            .with_navigate(navigated("sess-1"))
            .with_evaluate(Ok(results_json(2)))
            .with_close_session(Err(PortError::Other("boom".to_string())));

        let output = browser_web_search(&driver, input("rust", None))
            .await
            .expect("a close_session failure must not fail the search itself");

        assert_eq!(output.results.len(), 2);
    }

    #[tokio::test]
    async fn browser_web_search_should_return_err_when_evaluate_result_has_unexpected_shape() {
        let driver = FakeBrowserDriver::new()
            .with_navigate(navigated("sess-1"))
            .with_evaluate(Ok(serde_json::json!({"not": "an array"})))
            .with_close_session(Ok(()));

        let err = browser_web_search(&driver, input("rust", None))
            .await
            .expect_err("malformed result shape should surface as an error");

        assert!(
            err.contains("unexpected result shape"),
            "unexpected message: {err}"
        );
    }

    #[tokio::test]
    async fn browser_web_search_should_return_empty_results_when_no_matches_found() {
        let driver = FakeBrowserDriver::new()
            .with_navigate(navigated("sess-1"))
            .with_evaluate(Ok(results_json(0)))
            .with_close_session(Ok(()));

        let output = browser_web_search(&driver, input("asdkjaslkdjaslkdj", None))
            .await
            .expect("empty results should still be a success");

        assert!(output.results.is_empty());
    }
}
