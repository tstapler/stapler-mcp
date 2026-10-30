# Plan: sso-auth-detection

**Date**: 2026-10-29
**Depends on**: browser-profile-persistence (complete — `STAPLER_MCP_BROWSER_PROFILE_DIR`)

## Architecture decision

Three-layer implementation:

1. **Detection** (`crates/native/src/browser.rs` — `navigate_and_extract`): after
   navigation resolves, inspect `final_url` vs `url` (host-change) and the
   rendered DOM (login-form indicators) to decide whether auth is required.

2. **Signal** (`crates/core/src/ports.rs`): new `PortError::AuthRequired` variant;
   new `AuthRequiredInfo` struct in `schema.rs`; `ReadWebsiteOutput` gets an
   `auth_required_pages` field; `FetchPageOutput` propagates `PortError::AuthRequired`
   as a structured tool error.

3. **Headed escape hatch** (`crates/native/src/browser.rs` — new
   `navigate_with_headed_auth` path): opt-in via `STAPLER_MCP_HEADED=1`; polls for
   auth completion; re-fetches original URL; degrades gracefully when no display.

## Epics and tasks

### Epic 1 — Detection heuristics (core, no UI)

**Story 1.1 — Host-change detection in `navigate_and_extract`**
- After `page.url()` resolves, compare `final_url`'s `host_str()` to the
  requested `url`'s `host_str()`.
- If they differ, construct a candidate `auth_required` signal. Do not return it
  yet — pass it through the composite check in Story 1.2.
- File: `crates/native/src/browser.rs` — `navigate_and_extract` body.

**Story 1.2 — Login-form DOM indicator check**
- After host-change is suspected (Story 1.1) _or_ as a secondary check on
  same-host navigations with near-empty content (< 200 chars of `text`):
  evaluate a JS snippet against the page DOM that checks for
  `input[type="password"]`, `<title>` containing "sign in"/"login"/"authenticate",
  and a `<form>` with action URL fragment containing "login"/"auth"/"sso".
- Return `true` (auth wall confirmed) or `false` (normal page).
- File: `crates/native/src/browser.rs` — new helper `detect_auth_wall`.

**Story 1.3 — Unit tests for detection logic**
- Pure-function tests for the host-change check (same host → false, different
  host → true, parse failures → false/safe).
- Mock-DOM tests for `detect_auth_wall` JS string using `invoke_on_node`'s
  existing test harness (or a direct JS-string test via a real page in
  integration tests).
- File: `crates/native/src/browser.rs` unit tests section; optionally
  `crates/cli/tests/webcrawl.rs` integration.

### Epic 2 — Signal: schema and port error changes

**Story 2.1 — `PortError::AuthRequired` variant**
- Add `AuthRequired { requested_url: String, auth_url: String, auth_host: String }`
  to `PortError` in `crates/core/src/ports.rs`.
- Update `Display` impl.
- File: `crates/core/src/ports.rs`.

**Story 2.2 — `AuthRequiredInfo` schema struct**
- Add `AuthRequiredInfo { requested_url, auth_url, auth_host }` to `schema.rs`
  (derive `JsonSchema`, `Serialize`, `Deserialize`).
- Add `auth_required: Option<AuthRequiredInfo>` to `FetchPageOutput`.
- Add `auth_required_urls: Vec<AuthRequiredInfo>` to `ReadWebsiteOutput` (a
  separate, parallel list alongside `pages` — not a field on `ReadWebsitePage`,
  because the auth-required URL has no page content to attach it to).
- File: `crates/core/src/schema.rs`.

**Story 2.3 — Surface auth_required in `fetch_page` tool handler**
- When `navigate_and_extract` returns `PortError::AuthRequired`, the tool handler
  returns a structured `FetchPageOutput` with `auth_required` set (and
  `title`/`text`/`final_url` empty or omitted) rather than a tool-level error
  string — so the LLM caller can inspect it programmatically rather than parsing
  an error message.
- File: `crates/cli/src/main.rs` (or wherever `fetch_page` tool handler lives).

**Story 2.4 — Surface auth_required in `read_website` crawler**
- In `webcrawl.rs` `fetch_and_expand`, when a fetch returns
  `PortError::AuthRequired`, append to the crawler's `auth_required_urls`
  accumulator rather than silently continuing. Return these in
  `ReadWebsiteOutput::auth_required_urls`.
- File: `crates/core/src/tools/webcrawl.rs`.

### Epic 3 — Headed escape hatch

**Story 3.1 — `STAPLER_MCP_HEADED` env-var and display detection**
- At `NativeBrowser::launch` time: if `STAPLER_MCP_HEADED=1`, check for
  `$DISPLAY` or `$WAYLAND_DISPLAY`. If present, launch Chrome with
  `.with_head()` in `BrowserConfig`. If absent, log a warning and launch
  headless (headed mode unavailable, will degrade gracefully per Story 3.3).
- Store a `headed: bool` field on `NativeBrowser`.
- File: `crates/native/src/browser.rs`.

**Story 3.2 — `navigate_and_extract` headed auth flow**
- When `detect_auth_wall` returns true AND `self.headed` is true:
  1. Keep the tab open (don't drop it).
  2. Navigate it to the auth URL (already there from the redirect).
  3. Spawn a poll loop (500ms interval) that checks `page.url()` and
     `page.evaluate("document.body.innerText.length")`. When URL leaves the
     auth host AND content length > 200, auth is complete.
  4. Wrap the poll in a `tokio::time::timeout(auth_wait_timeout)` (default 300s,
     `STAPLER_MCP_AUTH_WAIT_SECS` override).
  5. Re-run `page.goto(original_url)` + content extraction.
  6. Return the extracted content.
- File: `crates/native/src/browser.rs` — `navigate_and_extract`.

**Story 3.3 — Graceful degradation: no display or auth timeout**
- If headed mode is unavailable (headless daemon): return
  `PortError::AuthRequired` with a hint that headed mode requires `$DISPLAY` and
  `STAPLER_MCP_HEADED=1`.
- If the auth wait timeout expires: return `PortError::AuthRequired` with a hint
  that the login was not completed in time; the user can try again.
- Never hang indefinitely.
- File: `crates/native/src/browser.rs`.

**Story 3.4 — Integration test: headed login smoke test**
- A test that launches a local HTTP server serving a login-wall page (redirect to
  `/login`, which has `<input type="password">`), verifies that `navigate_and_extract`
  returns `PortError::AuthRequired` with the correct `auth_host`.
- (Headed-mode full-flow testing is manual-only — requires a real display and
  a real human; document this limitation.)
- File: `crates/cli/tests/webcrawl.rs` or a new
  `crates/cli/tests/sso_detection.rs`.

### Epic 4 — Documentation and observability

**Story 4.1 — Daemon status / ping RPC**
- Surface `headed_mode: bool` and `auth_wait_timeout_secs: u64` in the
  `stapler_daemon_status` tool response (alongside the existing
  `unsafe_profile_warning`).
- File: `crates/cli/src/main.rs`, `crates/core/src/schema.rs` (status schema).

**Story 4.2 — Tool description updates**
- Update `read_website` and `fetch_page` tool descriptions to document:
  - That auth-gated URLs return `auth_required` instead of empty pages.
  - How to opt into headed mode.
  - The dependency on `STAPLER_MCP_BROWSER_PROFILE_DIR` for durable login.

## Sequencing

```
Epic 1 (detection) → Epic 2 (signal) → Epic 3 (headed) → Epic 4 (docs)
           └─────── can parallelize 1.1+1.2 ───────────┘
                    2.1+2.2 can parallel
                    2.3+2.4 depend on 2.1+2.2
                    3.1 can start in parallel with Epic 2
```

## Rabbit Holes

- **Multi-step SSO chains**: the auth redirect may go
  `site.com → corp-sso.com → okta.com → site.com`. Tracking the first
  off-host redirect as `auth_host` is sufficient for the signal; headed mode
  works because Chrome follows the whole chain. Do not try to enumerate every
  intermediate host.
- **`navigate_and_extract` tab lifetime**: currently drops the page after
  extraction. Story 3.2 reuses it for the headed flow. If the headed path
  aborts early (timeout, crash), the tab must be explicitly closed — otherwise
  the daemon leaks a tab that the idle reaper will eventually clean up, but in
  a confusing state with no session ID.
- **Race: reaper evicts the auth tab**: the tab used for headed login has no
  session ID, so the session reaper (`reap_expired`) won't touch it (it's not
  in `self.sessions`). No race here — but the tab must still be explicitly
  closed on all exit paths (success, timeout, panic).
- **wasm adapter parity**: `navigate_and_extract` has a wasm counterpart
  (`crates/wasm/`). Auth detection and headed mode apply only to the native
  adapter for this pass — document that the wasm adapter always returns
  `PortError::Other` for auth-gated URLs (no change) and note it as future work.

## Alternatives Considered

- **Per-call `headed: bool` parameter on `fetch_page`/`read_website`**: rejected
  because headed mode is a browser-launch-time decision (Chrome can't flip
  headless/headed mid-process). A per-call flag that silently does nothing when
  the daemon is in headless mode is confusing.
- **Return auth_required as a `ReadWebsitePage` entry with a special marker**:
  rejected in favor of a separate `auth_required_urls` list in
  `ReadWebsiteOutput` — a page entry with no content and a flag is a less clean
  schema than a parallel list of auth-gated URLs.
- **Poll using `page.wait_for_navigation()`** instead of a manual poll loop:
  `wait_for_navigation` only fires once; a multi-step SSO chain triggers multiple
  navigations. The manual poll is more robust.

## Adversarial review

**Concern A**: "host-change detection will fire on any redirect — CDNs, CNAME
aliases, canonical redirects (http → https)."
**Response**: the composite check (host-change AND login-form DOM indicators)
reduces this. http→https on the same host is same-host, not a host change.
CDN/CNAME redirects land on the same logical host. The only realistic false
positive is a service that redirects to a third-party login on the public path
(e.g. a Netlify-hosted doc site behind Netlify Identity). Acceptable: the user
sees `auth_required` rather than empty pages — both are honest, and the auth URL
is surfaced.

**Concern B**: "headed mode adds a new launch-path with its own failure modes
(display unavailable, Chrome fails to start headed) that aren't covered by the
existing tests."
**Response**: Story 3.1 adds explicit display detection at launch so the failure
is deterministic and diagnosed immediately, not buried in a cryptic Chrome error.
Story 3.4 adds a smoke test for the detection path.

**Concern C**: "the wasm adapter has no parity here — a user on the wasm path
will still get silent failures."
**Response**: acknowledged and explicitly documented as out of scope (Rabbit Holes
section). The native adapter covers the primary daemon use case.
