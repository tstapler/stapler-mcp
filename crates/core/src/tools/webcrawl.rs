//! Merges what were two separate third-party MCP servers the user ran
//! before this project existed: a Readability/Markdown extractor
//! (`read_website`) and a raw-page downloader (`download_website`). One
//! shared fetch/crawl/`robots.txt` implementation, two output modes — not
//! two crawlers. Only touches the `HttpClient`/`FileStore` ports already
//! established by `fetch_page`/`brave_web_search` — no new port trait.

use std::collections::{HashSet, VecDeque};

use dom_smoothie::Readability;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::ports::{HttpClient, HttpResponse};
use crate::schema::{
    DownloadWebsiteInput, DownloadWebsiteOutput, DownloadedPage, ReadWebsiteInput, ReadWebsiteOutput,
    ReadWebsitePage,
};

const DEFAULT_MAX_DEPTH: u32 = 1;
const DEFAULT_MAX_PAGES: u32 = 10;
const MAX_PAGES_CEILING: u32 = 50;
const MAX_DEPTH_CEILING: u32 = 5;
const USER_AGENT: &str = "stapler-mcp/0.1 (+https://github.com/tstapler/stapler-mcp)";

pub(crate) fn resolve_limits(max_depth: Option<u32>, max_pages: Option<u32>) -> (u32, u32) {
    let depth = max_depth.unwrap_or(DEFAULT_MAX_DEPTH).min(MAX_DEPTH_CEILING);
    let pages = max_pages.unwrap_or(DEFAULT_MAX_PAGES).clamp(1, MAX_PAGES_CEILING);
    (depth, pages)
}

pub(crate) fn cache_key_for(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Extracts same-scheme `<a href>` links, resolved against `base`. Crawling
/// deliberately stays same-host (checked by the caller) to avoid wandering
/// off the seed site.
fn extract_links(html: &str, base: &Url) -> Vec<Url> {
    let doc = dom_query::Document::from(html);
    doc.select("a[href]")
        .iter()
        .filter_map(|node| node.attr("href"))
        .filter_map(|href| base.join(href.as_ref()).ok())
        .filter(|u| u.scheme() == "http" || u.scheme() == "https")
        .collect()
}

pub(crate) fn extract_title_and_markdown(html: &str, url: &str) -> Result<(String, String), String> {
    let mut readability =
        Readability::new(html, Some(url), None).map_err(|e| format!("readability: {e}"))?;
    let article = readability.parse().map_err(|e| format!("readability: {e}"))?;
    let markdown = htmd::convert(article.content.as_ref()).map_err(|e| format!("html-to-markdown: {e}"))?;
    Ok((article.title, markdown))
}

/// Best-effort: no `robots.txt`, an unreachable one, or a parse failure all
/// mean "treat as allow-all" rather than aborting the whole crawl.
async fn fetch_robots<H: HttpClient>(http: &H, seed: &Url) -> Option<texting_robots::Robot> {
    let robots_url = texting_robots::get_robots_url(seed.as_str()).ok()?;
    let resp = http.get(&robots_url, &[]).await.ok()?;
    if resp.status != 200 {
        return None;
    }
    texting_robots::Robot::new(USER_AGENT, &resp.body).ok()
}

/// `Err(Some(status))` for a non-200 response, `Err(None)` for a transport-level
/// failure (the request never got a response at all). Kept distinct (rather than
/// collapsing both into a single `None`, as the pre-`docs-index` version did) so
/// callers that need to report *why* a fetch failed (`index_source`'s seed-URL
/// error, Story 4.1.1) can do so; callers that don't care still just match `Err(_)`.
async fn fetch_ok<H: HttpClient>(http: &H, url: &str) -> Result<HttpResponse, Option<u16>> {
    let resp = http
        .get(url, &[("User-Agent".to_string(), USER_AGENT.to_string())])
        .await
        .map_err(|_| None)?;
    if resp.status == 200 {
        Ok(resp)
    } else {
        Err(Some(resp.status))
    }
}

fn same_host(a: &Url, b: &Url) -> bool {
    a.host_str().is_some() && a.host_str() == b.host_str()
}

/// A BFS crawl frontier shared by both tools: yields `(url, depth, html)` for
/// every page successfully fetched (skipping disallowed/failed pages rather
/// than aborting), stopping at `max_pages` fetched or `max_depth` hops.
pub(crate) struct Crawler<'a, H: HttpClient> {
    http: &'a H,
    robot: Option<texting_robots::Robot>,
    seed: Url,
    max_depth: u32,
    max_pages: u32,
    visited: HashSet<String>,
    queue: VecDeque<(Url, u32)>,
}

impl<'a, H: HttpClient> Crawler<'a, H> {
    pub(crate) async fn new(http: &'a H, seed: Url, max_depth: u32, max_pages: u32) -> Self {
        let robot = fetch_robots(http, &seed).await;
        let mut visited = HashSet::new();
        visited.insert(seed.to_string());
        let mut queue = VecDeque::new();
        queue.push_back((seed.clone(), 0u32));
        Crawler {
            http,
            robot,
            seed,
            max_depth,
            max_pages,
            visited,
            queue,
        }
    }

    fn allowed(&self, url: &Url) -> bool {
        match &self.robot {
            Some(robot) => robot.allowed(url.as_str()),
            None => true,
        }
    }

    /// Pops the next allowed URL from the frontier (silently skipping any
    /// `robots.txt`-disallowed ones), or `None` once `max_pages` already
    /// fetched is reached or the frontier is exhausted.
    pub(crate) fn next_url(&mut self, fetched: usize) -> Option<(Url, u32)> {
        if fetched >= self.max_pages as usize {
            return None;
        }
        while let Some((url, depth)) = self.queue.pop_front() {
            if self.allowed(&url) {
                return Some((url, depth));
            }
        }
        None
    }

    /// Fetches `url` and, if `depth` allows further expansion, enqueues its
    /// same-host links. Returns `Err` on fetch failure (skipped, not fatal
    /// to the whole crawl by most callers — see `fetch_ok`'s doc comment for
    /// what the `Err` payload carries) — separated from `next_url` so a
    /// cache hit (see `read_website`) can skip the network fetch entirely,
    /// at the cost of not discovering that page's links (an accepted,
    /// documented trade-off: crawl discovery only follows links from
    /// freshly-fetched pages). On success, returns `(html, final_url)` —
    /// `final_url` is the post-redirect URL actually served (see
    /// `HttpResponse::final_url`), threaded through for `docs-index`'s
    /// `ChunkRecord.source_url`/`SourceMeta.page_urls` (Epic 6.1).
    pub(crate) async fn fetch_and_expand(
        &mut self,
        url: &Url,
        depth: u32,
    ) -> Result<(String, String), Option<u16>> {
        let resp = fetch_ok(self.http, url.as_str()).await?;
        let final_url = resp.final_url.clone();
        let html = String::from_utf8_lossy(&resp.body).into_owned();

        if depth < self.max_depth {
            for link in extract_links(&html, url) {
                let link_str = link.to_string();
                if same_host(&link, &self.seed) && !self.visited.contains(&link_str) {
                    self.visited.insert(link_str);
                    self.queue.push_back((link, depth + 1));
                }
            }
        }

        Ok((html, final_url))
    }
}

#[derive(Serialize, Deserialize)]
struct CachedPage {
    title: String,
    markdown: String,
}

pub async fn read_website<H, F>(
    http: &H,
    fs: &F,
    cache_dir: &str,
    input: ReadWebsiteInput,
) -> Result<ReadWebsiteOutput, String>
where
    H: HttpClient,
    F: crate::ports::FileStore,
{
    if input.url.is_empty() {
        return Err("url must not be empty".to_string());
    }
    let seed = Url::parse(&input.url).map_err(|e| format!("invalid url: {e}"))?;
    let (max_depth, max_pages) = resolve_limits(input.max_depth, input.max_pages);

    let mut crawler = Crawler::new(http, seed, max_depth, max_pages).await;
    let mut pages = Vec::new();

    while let Some((url, depth)) = crawler.next_url(pages.len()) {
        let cache_path = format!("{cache_dir}/read-website/{}.json", cache_key_for(url.as_str()));

        if let Ok(Some(bytes)) = fs.read_file(&cache_path).await {
            if let Ok(cached) = serde_json::from_slice::<CachedPage>(&bytes) {
                // Cache hit: skip the network fetch entirely (that's the
                // whole point of caching), at the cost of not expanding this
                // page's links further — see `fetch_and_expand`'s doc comment.
                pages.push(ReadWebsitePage {
                    url: url.to_string(),
                    title: cached.title,
                    markdown: cached.markdown,
                });
                continue;
            }
        }

        let Ok((html, _final_url)) = crawler.fetch_and_expand(&url, depth).await else {
            continue;
        };
        let (title, markdown) = extract_title_and_markdown(&html, url.as_str())?;

        let cached = CachedPage {
            title: title.clone(),
            markdown: markdown.clone(),
        };
        if let Ok(bytes) = serde_json::to_vec(&cached) {
            // Best-effort: a cache write failure shouldn't fail the tool call.
            let _ = fs.write_file(&cache_path, &bytes).await;
        }

        pages.push(ReadWebsitePage {
            url: url.to_string(),
            title,
            markdown,
        });
    }

    Ok(ReadWebsiteOutput { pages })
}

/// Maps a URL to a filesystem path under `save_dir`. Sanitizes the URL's
/// path component against traversal (`..`/`.` segments are dropped
/// entirely) since this path is derived from a possibly-untrusted remote
/// page's URL.
fn save_path_for(save_dir: &str, url: &Url) -> String {
    let raw = url.path().trim_start_matches('/');
    let safe_rel: String = raw
        .split('/')
        .filter(|seg| !seg.is_empty() && *seg != "." && *seg != "..")
        .collect::<Vec<_>>()
        .join("/");
    let safe_rel = if safe_rel.is_empty() {
        "index.html".to_string()
    } else if raw.ends_with('/') {
        format!("{safe_rel}/index.html")
    } else {
        safe_rel
    };
    let host = url.host_str().unwrap_or("unknown-host");
    format!("{save_dir}/{host}/{safe_rel}")
}

pub async fn download_website<H, F>(
    http: &H,
    fs: &F,
    input: DownloadWebsiteInput,
) -> Result<DownloadWebsiteOutput, String>
where
    H: HttpClient,
    F: crate::ports::FileStore,
{
    if input.url.is_empty() {
        return Err("url must not be empty".to_string());
    }
    if input.save_dir.is_empty() {
        return Err("saveDir must not be empty".to_string());
    }
    let seed = Url::parse(&input.url).map_err(|e| format!("invalid url: {e}"))?;
    let (max_depth, max_pages) = resolve_limits(input.max_depth, input.max_pages);

    let mut crawler = Crawler::new(http, seed, max_depth, max_pages).await;
    let mut pages = Vec::new();

    while let Some((url, depth)) = crawler.next_url(pages.len()) {
        let Ok((html, _final_url)) = crawler.fetch_and_expand(&url, depth).await else {
            continue;
        };
        let path = save_path_for(&input.save_dir, &url);
        fs.write_file(&path, html.as_bytes()).await.map_err(|e| e.to_string())?;
        pages.push(DownloadedPage {
            url: url.to_string(),
            path,
        });
    }

    Ok(DownloadWebsiteOutput { pages })
}
