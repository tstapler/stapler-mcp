//! Merges what were two separate third-party MCP servers the user ran
//! before this project existed: a Readability/Markdown extractor
//! (`read_website`) and a raw-page downloader (`download_website`). One
//! shared fetch/crawl/`robots.txt` implementation, two output modes — not
//! two crawlers. Only touches the `HttpClient`/`FileStore` ports already
//! established by `fetch_page`/`brave_web_search` — no new port trait.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;

use dom_smoothie::Readability;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

use crate::ports::{HttpClient, HttpResponse};
use crate::schema::{
    DownloadWebsiteInput, DownloadWebsiteOutput, DownloadedPage, ReadSavedPageInput,
    ReadSavedPageOutput, ReadWebsiteInput, ReadWebsiteOutput, ReadWebsitePage,
};

const DEFAULT_MAX_DEPTH: u32 = 1;
const DEFAULT_MAX_PAGES: u32 = 10;
const MAX_PAGES_CEILING: u32 = 50;
const MAX_DEPTH_CEILING: u32 = 5;
// A caller-supplied `maxInlineChars` above this is clamped down to it —
// same defense-in-depth reasoning as `MAX_PAGES_CEILING`/`MAX_DEPTH_CEILING`:
// `read_website`'s caller is an LLM agent that may itself be steered by
// previously-fetched content, so an absurdly large value can't fully defeat
// `INLINE_MARKDOWN_BUDGET`'s point.
const MAX_INLINE_CHARS_CEILING: usize = 200_000;
const USER_AGENT: &str = "stapler-mcp/0.1 (+https://github.com/tstapler/stapler-mcp)";

pub(crate) fn resolve_limits(max_depth: Option<u32>, max_pages: Option<u32>) -> (u32, u32) {
    let depth = max_depth
        .unwrap_or(DEFAULT_MAX_DEPTH)
        .min(MAX_DEPTH_CEILING);
    let pages = max_pages
        .unwrap_or(DEFAULT_MAX_PAGES)
        .clamp(1, MAX_PAGES_CEILING);
    (depth, pages)
}

/// `always_save_to_file` wins outright (an explicit ask to skip inlining
/// entirely reads clearer than asking the caller to know `0` means the
/// same thing); otherwise `max_inline_chars` overrides the default budget,
/// clamped to `MAX_INLINE_CHARS_CEILING`.
fn resolve_inline_budget(
    max_inline_chars: Option<usize>,
    always_save_to_file: Option<bool>,
) -> usize {
    if always_save_to_file.unwrap_or(false) {
        return 0;
    }
    max_inline_chars
        .unwrap_or(INLINE_MARKDOWN_BUDGET)
        .min(MAX_INLINE_CHARS_CEILING)
}

pub(crate) fn cache_key_for(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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

pub(crate) fn extract_title_and_markdown(
    html: &str,
    url: &str,
) -> Result<(String, String), String> {
    let mut readability =
        Readability::new(html, Some(url), None).map_err(|e| format!("readability: {e}"))?;
    let article = readability
        .parse()
        .map_err(|e| format!("readability: {e}"))?;
    let markdown =
        htmd::convert(article.content.as_ref()).map_err(|e| format!("html-to-markdown: {e}"))?;
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

/// Dual-purpose exact-host-equality check: the SSRF guard's own-host
/// crawl-boundary test (does a discovered link stay on the seed's host?),
/// and, per ADR-002, the credential-domain guard's exact host-equality check
/// in `crates/native/src/vault.rs`'s `lookup_domain` (Epic 3.3) — both
/// intentionally share this one implementation rather than maintaining
/// independent copies that could silently diverge. `pub` (not `pub(crate)`)
/// so the `native` crate can call it, mirroring `blocked_host_reason`'s and
/// `NetworkPolicy`'s existing cross-crate visibility below.
pub fn same_host(a: &Url, b: &Url) -> bool {
    a.host_str().is_some() && a.host_str() == b.host_str()
}

/// Controls whether the crawler refuses to fetch private/link-local
/// addresses (an SSRF guard). Loopback (`127.0.0.0/8`, `::1`, `localhost`)
/// is always allowed, under every variant here — it's the common dev-loop
/// case of pointing a browser tool at a local daemon or test fixture, and
/// isn't the actual SSRF risk this guard exists for (a LAN host or the cloud
/// metadata endpoint reachable from wherever this binary runs). Deliberately
/// **not** exposed on any MCP tool schema
/// (`ReadWebsiteInput`/`DownloadWebsiteInput`/`IndexDocsInput`): the caller
/// of these tools is an LLM agent that may itself be steered by
/// previously-fetched content, so the choice of whether to allow private
/// targets beyond loopback — including which hosts, under
/// `EnforceWithAllowlist` — must be made by the trusted binary at its own
/// call sites, not by anything a tool-call argument can influence.
/// `AllowPrivateNetworks` exists purely so this crate's own tests can crawl
/// a private-range mock server.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum NetworkPolicy {
    Enforce,
    AllowPrivateNetworks,
    /// Same fail-closed default as `Enforce`, except a host in this
    /// operator-declared set (exact hostname or unbracketed IP literal,
    /// matched case-insensitively) is also let through — e.g. a specific LAN
    /// service beyond the loopback default. Applied inside
    /// `blocked_host_reason` itself, so it also covers the redirect/in-page
    /// re-check (`frame_navigated_blocked_message`), not just the initial
    /// navigate.
    EnforceWithAllowlist(Arc<HashSet<String>>),
}

impl NetworkPolicy {
    /// `allow_private` is expected to come from the
    /// `STAPLER_MCP_ALLOW_PRIVATE_NETWORKS` env var, `allowed_hosts` from
    /// `STAPLER_MCP_ALLOWED_PRIVATE_HOSTS` (a comma-separated host list) —
    /// both read by the binary's own entry point, never from tool input.
    pub fn from_env(allow_private: Option<String>, allowed_hosts: Option<String>) -> Self {
        if allow_private.as_deref() == Some("1") {
            return NetworkPolicy::AllowPrivateNetworks;
        }
        let hosts: HashSet<String> = allowed_hosts
            .unwrap_or_default()
            .split(',')
            .map(|h| h.trim().to_ascii_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        if hosts.is_empty() {
            NetworkPolicy::Enforce
        } else {
            NetworkPolicy::EnforceWithAllowlist(Arc::new(hosts))
        }
    }
}

fn is_loopback_ipv4(ip: std::net::Ipv4Addr) -> bool {
    // `is_unspecified()` only matches the exact address `0.0.0.0`, but the
    // entire `0.0.0.0/8` range is "this network" (RFC 791 §3.2) and, on most
    // OSes, resolves to `127.0.0.1`/local interfaces just like loopback —
    // `http://0.1.2.3/` is loopback-equivalent exactly like `http://0.0.0.0/`.
    ip.is_loopback() || ip.octets()[0] == 0
}

fn is_private_or_link_local_ipv4(ip: std::net::Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local()
}

fn is_loopback_ipv6(ip: std::net::Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return true;
    }
    // `to_ipv4_mapped()` only unwraps the `::ffff:a.b.c.d/96` form. The
    // legacy IPv4-compatible form `::a.b.c.d` (RFC 4291 §2.5.5.1, deprecated
    // but still parsed by `Ipv6Addr::from_str`) has an all-zero 96-bit
    // prefix instead of the `ffff` prefix and would otherwise sail through
    // this check unrecognized — `to_ipv4()` recognizes both forms.
    ip.to_ipv4().map(is_loopback_ipv4).unwrap_or(false)
}

fn is_private_or_link_local_ipv6(ip: std::net::Ipv6Addr) -> bool {
    let seg0 = ip.segments()[0];
    let is_unique_local = (seg0 & 0xfe00) == 0xfc00; // fc00::/7
    let is_link_local = (seg0 & 0xffc0) == 0xfe80; // fe80::/10
    if is_unique_local || is_link_local {
        return true;
    }
    ip.to_ipv4()
        .map(is_private_or_link_local_ipv4)
        .unwrap_or(false)
}

/// Best-effort literal check: `crates/core` has no DNS-resolution port (see
/// `ports.rs`), so this only catches an IP-literal or well-known loopback
/// hostname written directly in the URL. It does not catch a public hostname
/// that resolves to a private address at request time (DNS rebinding), nor a
/// redirect an `HttpClient` adapter follows internally — both would need a
/// check inside the adapter's own connect path to close.
pub fn blocked_host_reason(url: &Url, policy: NetworkPolicy) -> Option<String> {
    if policy == NetworkPolicy::AllowPrivateNetworks {
        return None;
    }
    let reason = match url.host()? {
        // Loopback is allowed under every policy variant — see
        // `NetworkPolicy`'s doc comment.
        url::Host::Ipv4(ip) if is_loopback_ipv4(ip) => return None,
        url::Host::Ipv6(ip) if is_loopback_ipv6(ip) => return None,
        url::Host::Domain(d)
            if d.eq_ignore_ascii_case("localhost")
                || d.to_ascii_lowercase().ends_with(".localhost") =>
        {
            return None;
        }
        url::Host::Ipv4(ip) if is_private_or_link_local_ipv4(ip) => {
            Some(format!("{ip} is a private/link-local address"))
        }
        url::Host::Ipv6(ip) if is_private_or_link_local_ipv6(ip) => {
            Some(format!("{ip} is a private/link-local address"))
        }
        _ => None,
    }?;

    if let NetworkPolicy::EnforceWithAllowlist(allowed) = &policy {
        // `Url::host_str` brackets an IPv6 literal (`"[fe80::1]"`), which
        // would never match an allowlist entry written the normal way —
        // re-derive the comparison key from `Host`'s own `Display` instead.
        let host = match url.host()? {
            url::Host::Ipv4(ip) => ip.to_string(),
            url::Host::Ipv6(ip) => ip.to_string(),
            url::Host::Domain(d) => d.to_ascii_lowercase(),
        };
        if allowed.contains(&host) {
            return None;
        }
    }

    // Auditable deny event (issue #40): a silent `Option::None`-shaped
    // refusal leaves no record an operator can distinguish from "nothing
    // tried to go there" — every actual denial gets one line here, on the
    // same stderr channel as this crate's other diagnostics (stdout carries
    // the MCP JSON-RPC stream and can't be used for this).
    eprintln!("stapler-mcp: SSRF guard denied navigation to '{url}': {reason}");

    Some(reason)
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
    policy: NetworkPolicy,
    visited: HashSet<String>,
    queue: VecDeque<(Url, u32)>,
}

impl<'a, H: HttpClient> Crawler<'a, H> {
    pub(crate) async fn new(
        http: &'a H,
        seed: Url,
        max_depth: u32,
        max_pages: u32,
        policy: NetworkPolicy,
    ) -> Result<Self, String> {
        if let Some(reason) = blocked_host_reason(&seed, policy.clone()) {
            return Err(format!("refusing to crawl {seed}: {reason}"));
        }
        let robot = fetch_robots(http, &seed).await;
        let mut visited = HashSet::new();
        visited.insert(seed.to_string());
        let mut queue = VecDeque::new();
        queue.push_back((seed.clone(), 0u32));
        Ok(Crawler {
            http,
            robot,
            seed,
            max_depth,
            max_pages,
            policy,
            visited,
            queue,
        })
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
                if same_host(&link, &self.seed)
                    && !self.visited.contains(&link_str)
                    && blocked_host_reason(&link, self.policy.clone()).is_none()
                {
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

/// Ceiling, across every page a single `read_website` call returns, on how
/// much Markdown comes back inline — sized in characters as a conservative
/// stand-in for tokens (roughly 4 chars/token for English prose, so this
/// stays well clear even on denser text). Chosen well under Claude Code's
/// own ~25,000-token default cap on one MCP tool response
/// (`MAX_MCP_OUTPUT_TOKENS`): one long article, or a several-page crawl
/// where every page is individually modest but the total isn't, would
/// otherwise crowd out the rest of the caller's context — or hit that cap
/// outright. A page that doesn't fit what's left of the budget is saved to
/// disk instead (`ReadWebsitePage::saved_path`), with only a short preview
/// (`DIVERTED_PAGE_PREVIEW_LEN`, not charged against the budget — capped
/// small enough that even `MAX_PAGES_CEILING` diverted pages can't
/// meaningfully add to it) returned in its place. The file is still plain
/// Markdown, so an ordinary file tool can read or grep the full page
/// afterward.
const INLINE_MARKDOWN_BUDGET: usize = 60_000;

/// Preview length left in `markdown` for a page diverted to disk — just
/// enough to judge relevance, not counted against `INLINE_MARKDOWN_BUDGET`.
const DIVERTED_PAGE_PREVIEW_LEN: usize = 300;

/// Diverts `markdown` to a saved file (returning a preview in its place) if
/// it doesn't fit what's left of `remaining_budget`, otherwise returns it
/// unchanged and deducts its length from `remaining_budget` — shared by
/// `read_website`'s cache-hit and fresh-fetch paths so both draw from the
/// same running budget across the whole call.
async fn divert_large_markdown<F: crate::ports::FileStore>(
    fs: &F,
    cache_dir: &str,
    url: &Url,
    markdown: String,
    remaining_budget: &mut usize,
) -> (String, Option<String>) {
    if markdown.len() <= *remaining_budget {
        *remaining_budget -= markdown.len();
        return (markdown, None);
    }

    let saved_path = format!(
        "{cache_dir}/read-website/output/{}.md",
        cache_key_for(url.as_str())
    );
    if fs
        .write_file(&saved_path, markdown.as_bytes())
        .await
        .is_err()
    {
        // Best-effort, same as the fetch cache write below: a save failure
        // shouldn't lose the content, so fall back to returning it inline
        // regardless of budget.
        return (markdown, None);
    }

    (build_preview(&markdown, &saved_path), Some(saved_path))
}

/// Char-boundary-safe: `str` indexing panics mid-UTF-8-sequence, and
/// `DIVERTED_PAGE_PREVIEW_LEN` bytes can land inside a multi-byte character.
fn build_preview(markdown: &str, saved_path: &str) -> String {
    let preview_end = markdown
        .char_indices()
        .nth(DIVERTED_PAGE_PREVIEW_LEN)
        .map(|(i, _)| i)
        .unwrap_or(markdown.len());
    let remaining_chars = markdown.len() - preview_end;
    format!(
        "{}\n\n... [{remaining_chars} more characters truncated — full page saved to {saved_path}]",
        &markdown[..preview_end]
    )
}

pub async fn read_website<H, F>(
    http: &H,
    fs: &F,
    cache_dir: &str,
    input: ReadWebsiteInput,
    policy: NetworkPolicy,
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
    let mut remaining_budget =
        resolve_inline_budget(input.max_inline_chars, input.always_save_to_file);

    let mut crawler = Crawler::new(http, seed, max_depth, max_pages, policy).await?;
    let mut pages = Vec::new();

    while let Some((url, depth)) = crawler.next_url(pages.len()) {
        let cache_path = format!(
            "{cache_dir}/read-website/{}.json",
            cache_key_for(url.as_str())
        );

        if let Ok(Some(bytes)) = fs.read_file(&cache_path).await {
            if let Ok(cached) = serde_json::from_slice::<CachedPage>(&bytes) {
                // Cache hit: skip the network fetch entirely (that's the
                // whole point of caching), at the cost of not expanding this
                // page's links further — see `fetch_and_expand`'s doc comment.
                let (markdown, saved_path) = divert_large_markdown(
                    fs,
                    cache_dir,
                    &url,
                    cached.markdown,
                    &mut remaining_budget,
                )
                .await;
                pages.push(ReadWebsitePage {
                    url: url.to_string(),
                    title: cached.title,
                    markdown,
                    saved_path,
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

        let (markdown, saved_path) =
            divert_large_markdown(fs, cache_dir, &url, markdown, &mut remaining_budget).await;
        pages.push(ReadWebsitePage {
            url: url.to_string(),
            title,
            markdown,
            saved_path,
        });
    }

    Ok(ReadWebsiteOutput { pages })
}

const DEFAULT_SAVED_PAGE_LINES: usize = 500;
const MAX_SAVED_PAGE_LINES: usize = 2_000;
const DEFAULT_SAVED_PAGE_CONTEXT_LINES: usize = 3;
const MAX_SAVED_PAGE_CONTEXT_LINES: usize = 50;

/// `saved_path` is only ever safe to read back if it's exactly a path
/// `divert_large_markdown` itself could have produced — an allowlist match
/// on the whole shape (`{cache_dir}/read-website/output/<64 lowercase hex
/// chars>.md`) rather than a traversal/canonicalization denylist check,
/// since `read_saved_page`'s caller is the same LLM agent the SSRF guard's
/// own doc comment warns may be steered by previously-fetched content —
/// this must never become a way to read an arbitrary local file.
fn is_valid_saved_page_path(cache_dir: &str, path: &str) -> bool {
    let prefix = format!("{cache_dir}/read-website/output/");
    let Some(rest) = path.strip_prefix(&prefix) else {
        return false;
    };
    let Some(hex) = rest.strip_suffix(".md") else {
        return false;
    };
    hex.len() == 64 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Reads back a page `divert_large_markdown` previously saved to disk —
/// either a plain paginated read (line-numbered, like the `Read` tool), or,
/// with `query` set, every matching line plus surrounding context (like
/// `grep -n -C`) — so a caller can find or page through the full content
/// without pulling all of it into its own context, and so the content
/// remains reachable even when the caller isn't on the same machine as this
/// daemon (this file's own local path may not be readable from there).
pub async fn read_saved_page<F: crate::ports::FileStore>(
    fs: &F,
    cache_dir: &str,
    input: ReadSavedPageInput,
) -> Result<ReadSavedPageOutput, String> {
    if !is_valid_saved_page_path(cache_dir, &input.saved_path) {
        return Err(
            "savedPath must be a path previously returned by read_website's savedPath field"
                .to_string(),
        );
    }
    let bytes = fs
        .read_file(&input.saved_path)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{}: not found", input.saved_path))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().collect();

    Ok(match input.query.filter(|q| !q.is_empty()) {
        Some(query) => search_saved_page(&lines, &query, input.context_lines),
        None => paginate_saved_page(&lines, input.offset, input.limit),
    })
}

fn search_saved_page(
    lines: &[&str],
    query: &str,
    context_lines: Option<usize>,
) -> ReadSavedPageOutput {
    let context = context_lines
        .unwrap_or(DEFAULT_SAVED_PAGE_CONTEXT_LINES)
        .min(MAX_SAVED_PAGE_CONTEXT_LINES);
    ReadSavedPageOutput {
        content: grep_with_context(lines, &query.to_lowercase(), context),
        total_lines: lines.len(),
        truncated: false,
    }
}

fn paginate_saved_page(
    lines: &[&str],
    offset: Option<usize>,
    limit: Option<usize>,
) -> ReadSavedPageOutput {
    let total_lines = lines.len();
    let offset = offset.unwrap_or(1).max(1) - 1;
    let limit = limit
        .unwrap_or(DEFAULT_SAVED_PAGE_LINES)
        .min(MAX_SAVED_PAGE_LINES);
    let start = offset.min(total_lines);
    let end = (offset + limit).min(total_lines);
    ReadSavedPageOutput {
        content: numbered_lines(&lines[start..end], start),
        total_lines,
        truncated: end < total_lines,
    }
}

/// Formats `lines` as `"<1-based line number>: <line>"`, one per line,
/// `first_line_index` being `lines[0]`'s 0-based position in the full file.
fn numbered_lines(lines: &[&str], first_line_index: usize) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{}: {line}", first_line_index + i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every line containing `query_lower` (case-insensitive), each with
/// `context` lines of surrounding context, formatted like `grep -n -C`:
/// line-numbered, with a `"--"` separator between non-adjacent/non-
/// overlapping match blocks (merged into one block otherwise, matching
/// grep's own behavior of never repeating a shared context line).
fn grep_with_context(lines: &[&str], query_lower: &str, context: usize) -> String {
    let mut blocks: Vec<(usize, usize)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !line.to_lowercase().contains(query_lower) {
            continue;
        }
        let start = i.saturating_sub(context);
        let end = (i + context).min(lines.len().saturating_sub(1));
        match blocks.last_mut() {
            Some((_, last_end)) if start <= *last_end + 1 => *last_end = end,
            _ => blocks.push((start, end)),
        }
    }

    blocks
        .into_iter()
        .map(|(start, end)| numbered_lines(&lines[start..=end], start))
        .collect::<Vec<_>>()
        .join("\n--\n")
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
    policy: NetworkPolicy,
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

    let mut crawler = Crawler::new(http, seed, max_depth, max_pages, policy).await?;
    let mut pages = Vec::new();

    while let Some((url, depth)) = crawler.next_url(pages.len()) {
        let Ok((html, _final_url)) = crawler.fetch_and_expand(&url, depth).await else {
            continue;
        };
        let path = save_path_for(&input.save_dir, &url);
        fs.write_file(&path, html.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        pages.push(DownloadedPage {
            url: url.to_string(),
            path,
        });
    }

    Ok(DownloadWebsiteOutput { pages })
}

#[cfg(test)]
mod ssrf_guard_tests {
    use super::*;

    fn blocked(url: &str) -> bool {
        blocked_host_reason(&Url::parse(url).unwrap(), NetworkPolicy::Enforce).is_some()
    }

    #[test]
    fn should_allow_loopback_ipv4_literal_when_enforcing() {
        assert!(!blocked("http://127.0.0.1/"));
    }

    #[test]
    fn should_block_link_local_metadata_ipv4_when_enforcing() {
        assert!(blocked("http://169.254.169.254/latest/meta-data/"));
    }

    #[test]
    fn should_block_private_ipv4_ranges_when_enforcing() {
        assert!(blocked("http://10.0.0.1/"));
        assert!(blocked("http://172.16.0.1/"));
        assert!(blocked("http://192.168.1.1/"));
    }

    #[test]
    fn should_allow_localhost_hostname_case_insensitively_when_enforcing() {
        assert!(!blocked("http://localhost/"));
        assert!(!blocked("http://LOCALHOST/"));
        assert!(!blocked("http://foo.localhost/"));
    }

    #[test]
    fn should_allow_loopback_but_block_private_and_link_local_ipv6_when_enforcing() {
        assert!(!blocked("http://[::1]/"));
        assert!(blocked("http://[fc00::1]/"));
        assert!(blocked("http://[fe80::1]/"));
    }

    #[test]
    fn should_allow_ipv4_mapped_loopback_ipv6_when_enforcing() {
        assert!(!blocked("http://[::ffff:127.0.0.1]/"));
    }

    #[test]
    fn should_block_ipv4_mapped_private_ipv6_when_enforcing() {
        assert!(blocked("http://[::ffff:10.0.0.1]/"));
    }

    #[test]
    fn should_allow_entire_0_0_0_0_slash_8_range_when_enforcing() {
        // Not just the exact address `0.0.0.0` — the whole "this network"
        // range resolves like loopback on most OSes, so it's allowed the
        // same way.
        assert!(!blocked("http://0.0.0.0/"));
        assert!(!blocked("http://0.1.2.3/"));
    }

    #[test]
    fn should_allow_ipv4_compatible_loopback_ipv6_but_block_private_when_enforcing() {
        // `::a.b.c.d` (RFC 4291 IPv4-compatible form, distinct from the
        // `::ffff:a.b.c.d` mapped form) resolves the same way as its
        // embedded IPv4 address.
        assert!(blocked("http://[::10.0.0.1]/"));
        assert!(!blocked("http://[::127.0.0.1]/"));
    }

    #[test]
    fn should_allow_public_url_when_enforcing() {
        assert!(!blocked("https://example.com/"));
        assert!(!blocked("https://8.8.8.8/"));
    }

    #[test]
    fn should_allow_every_url_when_policy_allows_private_networks() {
        let seed = Url::parse("http://10.0.0.1/").unwrap();
        assert!(blocked_host_reason(&seed, NetworkPolicy::AllowPrivateNetworks).is_none());
    }

    #[test]
    fn should_allow_only_the_allowlisted_private_host_when_enforcing_with_allowlist() {
        let policy = NetworkPolicy::from_env(None, Some("192.168.1.50".to_string()));
        assert!(
            blocked_host_reason(&Url::parse("http://192.168.1.50/").unwrap(), policy.clone())
                .is_none()
        );
        assert!(
            blocked_host_reason(&Url::parse("http://192.168.1.51/").unwrap(), policy).is_some(),
            "hosts outside the allowlist must still be blocked"
        );
    }

    #[test]
    fn should_match_allowlisted_hosts_case_insensitively() {
        let policy = NetworkPolicy::from_env(None, Some("FE80::1".to_string()));
        assert!(blocked_host_reason(&Url::parse("http://[fe80::1]/").unwrap(), policy).is_none());
    }

    #[test]
    fn should_prefer_allow_private_networks_over_allowlist_when_both_set() {
        let policy = NetworkPolicy::from_env(Some("1".to_string()), Some("nope".to_string()));
        assert_eq!(policy, NetworkPolicy::AllowPrivateNetworks);
    }

    #[test]
    fn should_enforce_with_no_allowlist_when_env_vars_are_unset() {
        assert_eq!(NetworkPolicy::from_env(None, None), NetworkPolicy::Enforce);
    }
}

#[cfg(test)]
mod read_website_output_tests {
    use super::*;
    use crate::ports::{FileStore, PortError};
    use std::cell::RefCell;

    /// Captures the last `write_file` call's path/bytes, like `browser.rs`'s
    /// own `FakeFileStore` — `divert_large_markdown` only ever calls
    /// `write_file`. `fail_writes` exercises the best-effort fallback path.
    struct FakeFileStore {
        last_write: RefCell<Option<(String, Vec<u8>)>>,
        fail_writes: bool,
    }

    impl FakeFileStore {
        fn new() -> Self {
            FakeFileStore {
                last_write: RefCell::new(None),
                fail_writes: false,
            }
        }

        fn failing() -> Self {
            FakeFileStore {
                last_write: RefCell::new(None),
                fail_writes: true,
            }
        }
    }

    impl FileStore for FakeFileStore {
        async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<(), PortError> {
            if self.fail_writes {
                return Err(PortError::Io("simulated write failure".to_string()));
            }
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
    async fn should_return_markdown_inline_and_deduct_from_budget_when_under_budget() {
        let fs = FakeFileStore::new();
        let url = Url::parse("https://example.com/page").unwrap();
        let small = "hello world".to_string();
        let mut budget = 1_000;

        let (markdown, saved_path) =
            divert_large_markdown(&fs, "/cache", &url, small.clone(), &mut budget).await;

        assert_eq!(markdown, small);
        assert!(saved_path.is_none());
        assert!(fs.last_write.borrow().is_none());
        assert_eq!(budget, 1_000 - small.len());
    }

    #[tokio::test]
    async fn should_save_to_file_and_return_preview_when_over_budget() {
        let fs = FakeFileStore::new();
        let url = Url::parse("https://example.com/page").unwrap();
        let big = "a".repeat(1_000);
        let mut budget = 500;

        let (markdown, saved_path) =
            divert_large_markdown(&fs, "/cache", &url, big.clone(), &mut budget).await;

        let saved_path = saved_path.expect("saved_path should be set");
        assert!(saved_path.starts_with("/cache/read-website/output/"));
        assert!(saved_path.ends_with(".md"));
        assert!(
            markdown.len() < big.len(),
            "returned markdown should be a preview, not the full page"
        );
        assert!(markdown.contains(&saved_path));
        assert_eq!(
            fs.last_write.borrow().as_ref(),
            Some(&(saved_path, big.into_bytes()))
        );
        assert_eq!(
            budget, 500,
            "a diverted page's preview isn't charged against the budget"
        );
    }

    #[tokio::test]
    async fn should_fall_back_to_inline_when_save_fails() {
        let fs = FakeFileStore::failing();
        let url = Url::parse("https://example.com/page").unwrap();
        let big = "a".repeat(1_000);
        let mut budget = 500;

        let (markdown, saved_path) =
            divert_large_markdown(&fs, "/cache", &url, big.clone(), &mut budget).await;

        assert_eq!(markdown, big, "save failure shouldn't lose content");
        assert!(saved_path.is_none());
    }

    #[tokio::test]
    async fn should_not_panic_on_multibyte_char_at_the_preview_boundary() {
        // Every character is 3 UTF-8 bytes, so a byte-offset slice at
        // `DIVERTED_PAGE_PREVIEW_LEN` would land mid-character.
        let fs = FakeFileStore::new();
        let url = Url::parse("https://example.com/page").unwrap();
        let big: String = "世".repeat(DIVERTED_PAGE_PREVIEW_LEN + 500);
        let mut budget = 0;

        let (_, saved_path) = divert_large_markdown(&fs, "/cache", &url, big, &mut budget).await;

        assert!(saved_path.is_some());
    }

    #[test]
    fn should_use_default_budget_when_no_input_options_given() {
        assert_eq!(resolve_inline_budget(None, None), INLINE_MARKDOWN_BUDGET);
    }

    #[test]
    fn should_override_budget_with_max_inline_chars() {
        assert_eq!(resolve_inline_budget(Some(5_000), None), 5_000);
    }

    #[test]
    fn should_clamp_max_inline_chars_to_ceiling() {
        assert_eq!(
            resolve_inline_budget(Some(MAX_INLINE_CHARS_CEILING + 1_000), None),
            MAX_INLINE_CHARS_CEILING
        );
    }

    #[test]
    fn should_zero_budget_when_always_save_to_file_is_set() {
        // Wins even over an explicit maxInlineChars — an intentional
        // "skip inlining entirely" beats a numeric override that happens
        // to also be nonzero.
        assert_eq!(resolve_inline_budget(Some(5_000), Some(true)), 0);
    }
}

#[cfg(test)]
mod read_saved_page_tests {
    use super::*;
    use crate::ports::{FileStore, PortError};
    use std::collections::HashMap;

    const CACHE_DIR: &str = "/cache";
    const VALID_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd12";

    fn valid_path() -> String {
        format!("{CACHE_DIR}/read-website/output/{VALID_HEX}.md")
    }

    struct InMemoryFileStore {
        files: HashMap<String, Vec<u8>>,
    }

    impl InMemoryFileStore {
        fn seeded(path: &str, content: &str) -> Self {
            InMemoryFileStore {
                files: HashMap::from([(path.to_string(), content.as_bytes().to_vec())]),
            }
        }
    }

    impl FileStore for InMemoryFileStore {
        async fn write_file(&self, _path: &str, _bytes: &[u8]) -> Result<(), PortError> {
            panic!("not exercised by this test");
        }

        async fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, PortError> {
            Ok(self.files.get(path).cloned())
        }

        async fn delete_file(&self, _path: &str) -> Result<(), PortError> {
            panic!("not exercised by this test");
        }
    }

    fn input(saved_path: &str) -> ReadSavedPageInput {
        ReadSavedPageInput {
            saved_path: saved_path.to_string(),
            query: None,
            context_lines: None,
            offset: None,
            limit: None,
        }
    }

    #[test]
    fn should_accept_a_well_formed_output_path() {
        assert!(is_valid_saved_page_path(CACHE_DIR, &valid_path()));
    }

    #[test]
    fn should_reject_paths_outside_the_output_directory() {
        assert!(!is_valid_saved_page_path(
            CACHE_DIR,
            &format!("{CACHE_DIR}/read-website/{VALID_HEX}.json")
        ));
        assert!(!is_valid_saved_page_path(CACHE_DIR, "/etc/passwd"));
    }

    #[test]
    fn should_reject_a_traversal_attempt() {
        assert!(!is_valid_saved_page_path(
            CACHE_DIR,
            &format!("{CACHE_DIR}/read-website/output/../../../etc/passwd")
        ));
    }

    #[test]
    fn should_reject_wrong_length_or_non_hex_or_wrong_extension() {
        assert!(!is_valid_saved_page_path(
            CACHE_DIR,
            &format!("{CACHE_DIR}/read-website/output/abcd.md")
        ));
        assert!(!is_valid_saved_page_path(
            CACHE_DIR,
            &format!(
                "{CACHE_DIR}/read-website/output/{}.md",
                "g".repeat(64) // not hex
            )
        ));
        assert!(!is_valid_saved_page_path(
            CACHE_DIR,
            &format!("{CACHE_DIR}/read-website/output/{VALID_HEX}.txt")
        ));
    }

    #[tokio::test]
    async fn should_reject_a_path_not_matching_the_expected_shape() {
        let fs = InMemoryFileStore::seeded(&valid_path(), "irrelevant");

        let err = read_saved_page(&fs, CACHE_DIR, input("/etc/passwd"))
            .await
            .expect_err("arbitrary path should be rejected");

        assert!(err.contains("savedPath"), "unexpected message: {err}");
    }

    #[tokio::test]
    async fn should_return_error_when_saved_file_is_missing() {
        let fs = InMemoryFileStore::seeded("/cache/read-website/output/other.md", "content");

        let err = read_saved_page(&fs, CACHE_DIR, input(&valid_path()))
            .await
            .expect_err("missing file should error");

        assert!(err.contains("not found"), "unexpected message: {err}");
    }

    #[tokio::test]
    async fn should_paginate_with_line_numbers_and_report_truncated() {
        let content = "line1\nline2\nline3\nline4\nline5";
        let fs = InMemoryFileStore::seeded(&valid_path(), content);

        let output = read_saved_page(
            &fs,
            CACHE_DIR,
            ReadSavedPageInput {
                offset: Some(2),
                limit: Some(2),
                ..input(&valid_path())
            },
        )
        .await
        .expect("read should succeed");

        assert_eq!(output.content, "2: line2\n3: line3");
        assert_eq!(output.total_lines, 5);
        assert!(output.truncated);
    }

    #[tokio::test]
    async fn should_not_be_truncated_when_the_last_page_is_reached() {
        let content = "line1\nline2\nline3";
        let fs = InMemoryFileStore::seeded(&valid_path(), content);

        let output = read_saved_page(&fs, CACHE_DIR, input(&valid_path()))
            .await
            .expect("read should succeed");

        assert_eq!(output.content, "1: line1\n2: line2\n3: line3");
        assert!(!output.truncated);
    }

    #[tokio::test]
    async fn should_search_case_insensitively_with_surrounding_context() {
        let content = "a\nb\nNEEDLE\nc\nd\ne\nf\nneedle again\ng";
        let fs = InMemoryFileStore::seeded(&valid_path(), content);

        let output = read_saved_page(
            &fs,
            CACHE_DIR,
            ReadSavedPageInput {
                query: Some("needle".to_string()),
                context_lines: Some(1),
                ..input(&valid_path())
            },
        )
        .await
        .expect("search should succeed");

        // Two separate matches (lines 3 and 8), far enough apart that their
        // 1-line context windows don't touch — two blocks, "--" separated.
        assert_eq!(
            output.content,
            "2: b\n3: NEEDLE\n4: c\n--\n7: f\n8: needle again\n9: g"
        );
        assert!(!output.truncated);
    }

    #[tokio::test]
    async fn should_merge_overlapping_match_context_into_one_block() {
        let content = "a\nneedle\nb\nneedle\nc";
        let fs = InMemoryFileStore::seeded(&valid_path(), content);

        let output = read_saved_page(
            &fs,
            CACHE_DIR,
            ReadSavedPageInput {
                query: Some("needle".to_string()),
                context_lines: Some(2),
                ..input(&valid_path())
            },
        )
        .await
        .expect("search should succeed");

        assert!(
            !output.content.contains("--"),
            "overlapping context windows should merge into a single block: {}",
            output.content
        );
        assert_eq!(output.content, "1: a\n2: needle\n3: b\n4: needle\n5: c");
    }

    #[tokio::test]
    async fn should_return_empty_content_when_search_has_no_matches() {
        let fs = InMemoryFileStore::seeded(&valid_path(), "a\nb\nc");

        let output = read_saved_page(
            &fs,
            CACHE_DIR,
            ReadSavedPageInput {
                query: Some("nope".to_string()),
                ..input(&valid_path())
            },
        )
        .await
        .expect("search should succeed");

        assert_eq!(output.content, "");
    }
}

#[cfg(test)]
mod same_host_tests {
    // Fully-qualified crate path rather than `use super::same_host` —
    // exercises the same call shape `crates/native/src/vault.rs`'s
    // `lookup_domain` (Epic 3.3, ADR-002) now uses, now that `same_host` is
    // `pub`.
    use crate::tools::webcrawl::same_host;
    use url::Url;

    #[test]
    fn same_host_should_return_true_when_hosts_match_across_module_boundary() {
        let a = Url::parse("https://example.com/login").unwrap();
        let b = Url::parse("https://example.com/").unwrap();
        assert!(same_host(&a, &b));
    }

    #[test]
    fn same_host_should_normalize_ipv6_brackets_when_comparing_hosts() {
        // `Url::host_str()` brackets IPv6 literals (`"[::1]"`); the two
        // inputs below are the same address in compressed vs. fully expanded
        // form, so this also confirms `url::Url` normalizes IPv6 hosts
        // before `same_host` ever compares them — see the analogous
        // allowlist comparison bug this guards against at
        // `blocked_host_reason`'s `EnforceWithAllowlist` arm above.
        let a = Url::parse("https://[::1]/login").unwrap();
        let b = Url::parse("https://[0:0:0:0:0:0:0:1]/").unwrap();
        assert!(same_host(&a, &b));
    }
}
