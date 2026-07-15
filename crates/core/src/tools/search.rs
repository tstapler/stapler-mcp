use serde::Deserialize;

use crate::ports::HttpClient;
use crate::schema::{BraveSearchInput, BraveSearchOutput, BraveSearchResult};

pub const DEFAULT_BASE_URL: &str = "https://api.search.brave.com/res/v1/web/search";

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
    let count = match input.count {
        None | Some(0) => 10,
        Some(c) if c > 20 => 20,
        Some(c) => c,
    };

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
