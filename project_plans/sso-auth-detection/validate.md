# Validation: sso-auth-detection

**Date**: 2026-10-29

## Adversarial review pass

### Risk 1: False-positive auth detection breaks non-SSO URLs

**Claim**: the composite detection (host-change + login-form DOM) will fire on
benign redirects (CDN, http→https canonical, marketing landing page with
login widget in navbar).

**Assessment**: PLAUSIBLE but mitigated.

- http→https is a same-host redirect — passes the host-change check with no
  false positive (same `host_str()`).
- CDN/CNAME: e.g. `docs.company.com` → `company.netlify.app` — this IS a
  host-change. The second-stage DOM check mitigates: a real CDN redirect lands
  on the actual content page (no `<input type="password">`). The composite
  requires BOTH signals for a definitive `auth_required` on same-type-but-not-sso
  redirects.
- A page with a navbar login widget: same host, non-trivial content, `<input
  type="password">` in the DOM. This is a possible false positive. Mitigation:
  weight by content length — if `text.len() > 1000` (real page content exists),
  don't flag as auth-required even if a password field is present.

**Verdict**: refinement needed on the DOM-only path. Add content-length guard
(`text.len() < 500`) to the same-host login-form branch. Host-change without
any DOM check is safe on its own for the primary use case.

### Risk 2: Headed mode races the idle reaper

**Claim**: the tab kept open for headed auth has no session ID and isn't in
`self.sessions` — the reaper won't touch it, but the tab might be closed if
`NativeBrowser::close()` is called during auth wait.

**Assessment**: CONFIRMED design gap. `NativeBrowser::close()` calls
`self.browser.close()`, which tears down the whole browser process including all
open pages. If the daemon shuts down while a headed auth is in progress, the tab
and the poll loop are torn down with it. This is acceptable (auth is interactive,
a daemon restart is obvious to the user) but the poll loop's `tokio::select!`
must handle the future being dropped gracefully — it already does because
`tokio::time::timeout` produces an `Err(Elapsed)` that Story 3.3 handles.

**Verdict**: no additional mitigation needed; document in code comment.

### Risk 3: `auth_required_urls` in `ReadWebsiteOutput` is a schema break

**Claim**: adding `auth_required_urls: Vec<AuthRequiredInfo>` to
`ReadWebsiteOutput` changes the JSON schema MCP clients may have cached.

**Assessment**: PLAUSIBLE. The field is additive (new optional array). MCP
clients that ignore unknown fields (all major ones) are unaffected. Claude's own
tool-use JSON parsing is tolerant of extra fields. Clients that do strict
schema validation (rare, but possible) may fail.

**Mitigation**: use `#[serde(default, skip_serializing_if = "Vec::is_empty")]`
on `auth_required_urls` so it's absent from the JSON response when empty —
existing clients see no change. Only clients that explicitly look for the new
field observe it.

**Verdict**: low risk with the skip-serializing guard.

### Risk 4: `STAPLER_MCP_HEADED` on a headless CI server silently does nothing

**Claim**: if someone sets `STAPLER_MCP_HEADED=1` but runs on a machine with no
display, the behavior is undefined (or Chrome crashes at launch).

**Assessment**: Story 3.1 addresses this explicitly: detect `$DISPLAY`/
`$WAYLAND_DISPLAY` at launch. If headed is requested but no display is available,
log a clear warning to stderr and fall back to headless. The tool's behavior
(returning `auth_required` for SSO-gated URLs without opening a window) is
correct and documented.

**Verdict**: resolved by Story 3.1 design.

### Risk 5: Wasm adapter has no equivalent path

**Claim**: the wasm adapter's `navigate_and_extract` (used in Claude.ai /
browser-based contexts) will still silently fail for SSO-gated URLs.

**Assessment**: CONFIRMED gap, acknowledged as out of scope in `plan.md`. The
wasm adapter can be updated in a follow-on pass (add the same JS-based detection
heuristics; headed mode is not applicable in a headless browser context).

**Verdict**: document, defer to follow-on.

## AC validation

| AC | Satisfied by | Notes |
|---|---|---|
| 1. auth_required signal, never silent empty pages | Epic 2, Story 2.4 (webcrawl) + Story 2.3 (fetch_page) | Replaces silent `continue` with accumulator |
| 2. signal includes requested URL, final URL, auth host | Story 2.1 — `PortError::AuthRequired` struct | All three fields present |
| 3. headed mode opens window; user completes login; fetch completes | Story 3.2 | Manual-only verification for the full flow |
| 4. subsequent fetch succeeds without prompting (same profile) | Depends on STAPLER_MCP_BROWSER_PROFILE_DIR | Document dependency clearly |
| 5. no display → auth_required + auth URL, not crash | Story 3.1 (display check) + Story 3.3 (graceful degrade) | Covered |
| 6. credentials never read/logged | No credential-reading code in any story | Headed window is opaque to the daemon |
| 7. common-path perf unchanged | Detection only runs after navigation; no overhead for non-SSO pages | Composite check is two JS evals post-navigation |

## Pre-implementation checklist

- [ ] Verify `BrowserConfig::builder().with_head()` compiles and launches
  Chrome visibly on Linux/macOS (chromiumoxide version check).
- [ ] Confirm `detect_auth_wall` JS snippet against three real-world SSO login
  pages (Okta, Google Workspace, GitHub Enterprise) — ensure password field
  is present in AX tree / DOM before the composite heuristic is finalized.
- [ ] Confirm `page.url()` after a `wait_for_navigation` returns the committed
  URL including SSO redirect destination (not the original request URL).
- [ ] Review `skip_serializing_if = "Vec::is_empty"` behavior in the thin-client
  schema serialization path — ensure it doesn't break the wasm schema export.
