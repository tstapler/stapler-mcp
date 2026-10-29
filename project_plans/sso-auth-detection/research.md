# Research: sso-auth-detection

**Date**: 2026-10-29

## 1. Codebase orientation

### `navigate_and_extract` — the path both affected tools share

`NativeBrowser::navigate_and_extract` (`crates/native/src/browser.rs:1568-1616`)
is the single code path that both `read_website` (via the `WebCrawler` in
`crates/core/src/tools/webcrawl.rs`) and `fetch_page` ultimately call.

It does:
1. `browser.new_page(url)` — creates an ephemeral tab
2. `page.wait_for_navigation()`
3. Extracts `title`, `text` (via `innerText`), `html` (via `content()`), and
   `final_url` (via `page.url()`)
4. Drops the page (no session registered, no event listeners)

**Key gap**: no inspection of `final_url` vs `url`, no check for login-form
indicators, no auth signal of any kind.

### How silent empty pages happen

In `webcrawl.rs`, `fetch_and_expand` wraps the underlying fetch in an `Ok(...)
else { continue }` arm (line 508 and 706). Any `PortError` from
`navigate_and_extract` is silently discarded and the page is simply absent from
the results. A `PortError::Timeout` (rendered as "timed out") hits the same
suppression. The caller sees `{"pages":[]}` with no indication of why.

### `FetchPageOutput` gap

`fetch_page` (`schema.rs:21-33`) returns `FetchPageOutput { title, text, saved_to,
final_url }`. A failure propagates as a tool-level error string (not a structured
result), but the current `navigate_and_extract` doesn't return any auth-specific
error — it returns either the extracted content or a generic `PortError::Other` /
`PortError::Timeout`.

### Existing `PortError` variants

From `crates/core/src/ports.rs:16+`:

```
PortError::Io(String)
PortError::Timeout
PortError::Other(String)
PortError::NotFound(String)
PortError::SessionCrashed(String)
PortError::NotActionable(String)
PortError::CredentialDomainMismatch(String)
PortError::CredentialAmbiguous(String)
PortError::CredentialUnauthenticated(String)
PortError::CredentialRateLimited(String)
PortError::CredentialExpired(String)
```

There is no `AuthRequired` variant. Adding one follows the same pattern as the
existing `Credential*` variants.

### Browser launch — headed mode

`NativeBrowser::launch(persistent_profile_dir: Option<PathBuf>)`
(`browser.rs:554-604`) builds a `BrowserConfig` that defaults to headless. To
launch headed, `BrowserConfig::builder()` must not call `.with_head()` removed or
conversely must call `.with_head()` — chromiumoxide's `BrowserConfigBuilder`
exposes `.with_head()` for a non-headless launch. This is a launch-time flag: it
cannot be toggled per-call.

### Persistent profile — prerequisite for durable login

`browser-profile-persistence` is complete: `STAPLER_MCP_BROWSER_PROFILE_DIR`
env var sets a durable `user_data_dir` at launch. Without it, a headed login
session only survives for the current daemon process lifetime. The SSO feature
should document this dependency: headed login is most useful when combined with a
persistent profile.

## 2. SSO/auth-redirect detection strategies

### Strategy A: host-change redirect (strongest signal, fewest false positives)

After navigation, compare `final_url`'s host to the requested URL's host. If they
differ, the user was redirected to a different domain — a strong indicator of an
OAuth/SAML/SSO redirect (e.g., `docs.example.com` → `sso.corp.example.com/login`).

**Limitations**: same-host login walls (`docs.example.com/login?next=/docs/...`)
are missed. Multi-step SSO chains (the final URL may be on a third-party IdP)
need to be traced back to the original intent.

### Strategy B: login-form DOM indicators

Inspect the rendered HTML for patterns associated with a login page:
- `<input type="password">` in the DOM
- `<form>` with `action` containing "login", "auth", "sso", "signin"
- Page `<title>` containing "Sign in", "Login", "Authenticate"
- `<meta>` redirect-to-login headers (some SSO systems include these)

**Limitations**: requires HTML inspection after render; more false positives if
a legitimate page happens to embed a login widget in a sidebar.

### Strategy C: near-empty content after navigation

If the rendered `text` (innerText) is under a threshold (e.g., 200 characters)
and `final_url` differs from the requested URL, this is a strong composite
signal.

**Best practice synthesis**: combine A + B:
- Host-change redirect → `auth_required` immediately
- Same-host + login-form DOM indicators → `auth_required`
- Neither → return content normally

This matches what Playwright-based scraping libraries (e.g., Crawlee, Apify) do
for auth detection.

## 3. Headed mode mechanics

### chromiumoxide headed launch

`BrowserConfig::builder().with_head()` launches Chrome in non-headless mode
(verified in chromiumoxide source: `BrowserConfig::builder()` defaults headless
to `true`; `.with_head()` clears it). No other changes needed for a headed
window to appear — the OS display stack handles visibility.

### No-display fallback

If `$DISPLAY` (X11) or `$WAYLAND_DISPLAY` is unset on Linux, a headed Chrome
launch will fail immediately (Chrome exits with "cannot open display"). The
daemon should detect this at launch time or at `navigate_and_extract` invocation
time: if headed mode is requested but no display is available, skip the headed
attempt and return `auth_required` with the auth URL.

### Wait-for-auth-complete

The daemon needs to detect when the user has finished logging in. The simplest
reliable approach: poll the tab's current URL every 500ms; when it no longer
matches the auth host (and page content is non-trivial), auth is complete.
A configurable timeout (default: 5 minutes, env-var-overridable) applies.

### Sequence for headed mode

1. `navigate_and_extract(url)` runs; `final_url` lands on auth host.
2. `auth_required` is detected.
3. If headed mode and display available: reuse (or open) the existing browser
   tab in visible mode; navigate back to the auth URL and let the user proceed.
4. Poll for completion (URL leaves auth host + page has content).
5. Re-run `navigate_and_extract` against the original URL.
6. Return content.

**Note**: headed mode re-uses the same `NativeBrowser` browser process. The tab
opened for step 3 should be a named session (so the idle reaper doesn't close it
while the user is logging in) or the poll loop should hold a reference to it.

## 4. Schema changes

### Option A: new `PortError::AuthRequired`

Add `AuthRequired { requested_url: String, auth_url: String, auth_host: String }`
to `PortError`. Surface it as a tool-level error in `fetch_page` (with a human-
readable message). In `read_website`, surface it per-page rather than silently
skipping — add an `auth_required_pages: Vec<AuthRequiredPage>` to
`ReadWebsiteOutput` or add an `auth_required: Option<AuthRequired>` field to
`ReadWebsitePage`.

### Option B: structured field on output types

Add `auth_required: Option<AuthRequiredInfo>` to `FetchPageOutput` and a
per-page indicator to `ReadWebsitePage`. This makes auth detection observable
without requiring callers to catch a tool-level error.

**Recommendation**: Option A for `fetch_page` (single-URL, expect a result or an
error) + Option B for `read_website` (multi-page crawler, need per-page
granularity without stopping the whole crawl).

## 5. Dependency map

```
sso-auth-detection
  ├── depends on: browser-profile-persistence (for durable post-login sessions)
  ├── complements: credential-vault (vault-backed login, different use case)
  └── new PortError variant (core, no existing dependency)
```

## 6. Risk assessment

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| False-positive auth detection on pages with login widgets | Medium | Medium | Use composite heuristic (host-change OR strong DOM signal), not DOM alone |
| Headed Chrome fails on headless servers (no $DISPLAY) | High on CI | Low | Detect at startup, degrade gracefully to auth_required signal |
| User never completes login (poll timeout) | Medium | Low | Configurable timeout, return auth_required with original URL after timeout |
| Profile not persistent — login state lost on restart | High | Medium | Document: headed login most useful with STAPLER_MCP_BROWSER_PROFILE_DIR |
| Multi-step SSO chain — intermediate host mistaken as auth_url | Low | Medium | Track full redirect chain, report original auth redirect host |
