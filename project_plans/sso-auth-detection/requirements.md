# Requirements: sso-auth-detection

**item_id**: d57de33c-b8dd-4884-867c-08fe2ece5806
**Date**: 2026-10-29
**Type**: feature addition
**Complexity**: 3 — medium, builds on existing browser infrastructure

## Problem Statement

`read_website` and `fetch_page` cannot distinguish between a page that rendered
successfully and a page that silently redirected to an SSO/login wall. In a real
session, `read_website` on an SSO-protected URL returned `{"pages":[]}` with no
error and no indication that authentication was the cause. The agent inferred the
cause from external context; the tool gave no signal. A `fetch_page` call on a
large public page returned `render ...: timed out`, which looks identical to a
timeout from other causes.

The daemon's Chrome browser (`NativeBrowser`) already opens tabs, watches
navigation events, and captures `final_url` from `navigate_and_extract`
(`browser.rs:1596-1603`). The session-based tools already track SSRF redirects
via `frameNavigated`/`frameRequestedNavigation` listeners. Auth-redirect detection
is the same class of problem: notice that the page's committed URL (or the page
body itself) signals "login required" and return an explicit, named result rather
than silently empty pages or a generic timeout.

## Ground Truth (verified against codebase, 2026-10-29)

- `navigate_and_extract` (`browser.rs:1568-1616`) opens a new ephemeral tab,
  navigates, and returns `PageExtract { title, html, text, final_url }`. It does
  **not** inspect `final_url` for auth-redirect signals.
- `ReadWebsiteOutput` (`schema.rs:155-157`) is `{ pages: Vec<ReadWebsitePage> }`.
  A failed fetch silently returns no entry for that URL (webcrawl
  `fetch_and_expand` error arm: `let Ok(...) = ... else { continue }`,
  `webcrawl.rs:508`). The caller sees an empty pages slice with no explanation.
- `FetchPageOutput` (`schema.rs:21-33`) has `title`, `text`, `saved_to`,
  `final_url` — no auth-indication field.
- `PortError` (`ports.rs:16+`) has no `AuthRequired` variant. The existing
  credential-adjacent variants (`CredentialDomainMismatch`, `CredentialUnauthenticated`,
  etc.) are for vault/credential-store usage, not for detecting a browser redirect
  to a login page.
- Profile persistence (issue #25, `project_plans/browser-profile-persistence/`)
  is done: `STAPLER_MCP_BROWSER_PROFILE_DIR` opt-in already exists. A persistent
  profile is the mechanism that allows SSO session cookies to survive daemon
  restarts — this feature builds on it.
- The browser currently runs headless (default `BrowserConfig::builder()`). There
  is no existing headed-mode launch path in the codebase.

## Revised Problem Framing

Three separable concerns, all required by the acceptance criteria:

1. **Detect**: Identify when `navigate_and_extract` (used by `read_website` /
   `fetch_page`) lands on a login/SSO wall instead of the requested content.
2. **Signal**: Return an explicit `auth_required` result (never silent empty pages)
   with the request URL, the landing URL that demanded auth, and the auth host.
3. **Headless-to-headed escape hatch**: When a display is available, open the
   browser in headed mode so the user can log in interactively, then re-run the
   original fetch in the same profile once auth completes.

## Constraints from Current Architecture

- `navigate_and_extract` creates an **ephemeral tab** (not a named session) —
  unlike `navigate()`'s session-based path, it uses `browser.new_page(url)` with
  no event listeners. Auth detection added here must not break the no-listener,
  fast-path design.
- `BrowserConfig` is a launch-time setting. Switching from headless to headed
  requires relaunching the browser process or supporting two processes side by side.
  The simplest model is: headed mode is an opt-in that applies to the entire daemon
  (env var at startup), not a per-call toggle.
- A persistent profile (`STAPLER_MCP_BROWSER_PROFILE_DIR`) is the only mechanism
  for SSO session cookies to survive a daemon restart. Without it, heading the
  browser for login works for the current daemon lifetime only.

## In Scope

- SSO/auth-redirect detection heuristics for `navigate_and_extract` (and by
  extension `read_website` / `fetch_page`).
- A new `PortError::AuthRequired` variant (or equivalent tool-level output field)
  that carries: requested URL, landing URL, auth host, and a human-readable hint.
- `ReadWebsiteOutput` and `FetchPageOutput` schema changes to surface
  `auth_required` instead of silent empty/timeout.
- Headed mode: a `STAPLER_MCP_HEADED` env-var opt-in that launches Chrome visibly;
  the daemon waits for auth to complete (configurable timeout) before completing
  the fetch.
- No-display fallback: when no display is available (headless-only environment),
  return `auth_required` with the auth URL for the user to open elsewhere, rather
  than hanging or crashing.
- Reusing the persistent profile post-login: a second `navigate_and_extract` (or
  the same call's continuation) against the same profile must succeed without
  prompting while the session is valid.

## Out of Scope

- Storing, reading, or passing credentials programmatically (see `credential-vault`,
  issue #26 — that project handles vault-backed login; this one handles interactive
  human login).
- Per-session headed mode (a session-level `headed: true` parameter on
  `stapler_browser_navigate`) — the launch-time constraint makes this infeasible
  without a second browser process.
- SSO token injection or cookie planting via the MCP tools — the user authenticates
  in the headed window; the tool never reads the resulting cookies.
- `stapler_browser_navigate` / session-based tools — those tools already surface
  navigation failures explicitly and have separate event-listener infrastructure.

## Acceptance Criteria

1. A `read_website` or `fetch_page` call whose navigation lands on a
   login/SSO page returns an explicit `auth_required` signal (never silent
   `{"pages":[]}` and never a generic timeout error).
2. The `auth_required` signal includes: the originally requested URL, the final
   (redirected) URL, and the hostname that demanded authentication.
3. In headed mode (opt-in), a browser window appears; the user completes login
   (including MFA/hardware key); the original fetch then completes and returns
   the page content.
4. After a successful headed login, a subsequent `read_website`/`fetch_page` call
   to the same host — in the same daemon process using the same persistent profile
   — succeeds without opening a new window or prompting again.
5. When no display is available and headed mode is requested, the tool returns
   `auth_required` with the auth URL (not a crash or hang) so the user can
   authenticate via another path.
6. Credentials (cookies, tokens, passwords) are never read, logged, or forwarded
   by the tool; the user authenticates in the headed window.
7. The headless default behavior for non-SSO-gated URLs is unchanged — no
   performance regression on the common path.

## Open Questions

- Detection heuristics: redirect to a different host, known login-form DOM
  shape, or near-empty content after navigation? Redirect-to-different-host is
  the most reliable signal; DOM-shape detection reduces false negatives for
  same-host login walls (e.g. `/login?next=...`).
- Timeout while waiting for the user in headed mode: what is the right default,
  and should it be configurable via an env var or a tool parameter?
- What constitutes "auth complete"? The simplest check: the current URL no longer
  matches the auth host and the page has non-trivial content. More robust: the
  URL matches the originally requested URL or its expected host.
- How to handle multi-step SSO flows (SAML, OAuth with external IdP): the auth
  redirect may chain through several hosts before landing back on the target.

## Priority

`medium` — blocks practical use of `read_website`/`fetch_page` for any internal
or enterprise documentation behind SSO; the silent empty-pages failure is
actively misleading.
