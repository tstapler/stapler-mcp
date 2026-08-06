# UX Design: browser-automation MCP tools

Source: `requirements.md`, `research/ux.md`, `implementation/plan.md`. This is API/CLI DX
for a non-human caller — the "user" is an LLM agent parsing JSON tool responses to decide
its next tool call; the secondary reader is a human scanning a transcript afterward.
"Screens" below are tool call/response pairs; "flows" are call sequences; "error states"
are error-response shapes the agent must recover from without a human in the loop.

All shapes below match the resolved types in `implementation/plan.md` (`AxNodeOutput` with
`#[serde(rename = "ref")]`, `Locator` = opaque ref string scoped to the latest snapshot,
`PortError::NotFound` for session/locator misses, `BrowserActionOutput { snapshot, note }`
shared by click/type/snapshot).

## Surfaces designed: 4 tool call/response pairs + 6 error-response shapes = 10

---

## Surface 1: `stapler_browser_navigate`

### Call

```json
{
  "tool": "stapler_browser_navigate",
  "input": {
    "url": "https://example.com/login",
    "sessionId": null,
    "timeoutSeconds": 30
  }
}
```

### Response (success — new session)

```json
{
  "sessionId": "sess-1946a2f0c31-1",
  "finalUrl": "https://example.com/login",
  "snapshot": {
    "url": "https://example.com/login",
    "truncated": false,
    "root": {
      "ref": "e1",
      "role": "generic",
      "name": "",
      "children": [
        {
          "ref": "e2",
          "role": "textbox",
          "name": "Email",
          "children": []
        },
        {
          "ref": "e3",
          "role": "textbox",
          "name": "Password",
          "children": []
        },
        {
          "ref": "e4",
          "role": "button",
          "name": "Sign in",
          "children": []
        }
      ]
    }
  }
}
```

### Interaction flow

1. Agent calls `navigate` with a URL and no `sessionId` → gets back a brand-new
   `sessionId` plus an inline snapshot in the *same* round trip. No separate `snapshot`
   call is needed to see the page for the first time.
2. Agent reads `ref`s directly from `navigate`'s response (`e2`, `e3`, `e4` above) to plan
   its next action — it never has to guess a CSS selector or role/name pair itself.
3. If the agent already holds a `sessionId` from a prior `navigate` (continuing a flow,
   e.g. re-navigating after a redirect didn't fire), it passes that `sessionId` back;
   the same session's `Page` is reused (`goto`), and *all prior `ref`s become invalid* —
   the response's fresh snapshot is the only valid source of `ref`s from that point on.

### Edge case: re-navigate invalidates old refs

Given a session that navigated once (`ref`s `e1`-`e4` from Response above), then the
agent calls `navigate` again with `sessionId: "sess-1946a2f0c31-1"` and a new URL — the
response's `snapshot.root` starts a fresh `ref` numbering from `e1`. Any tool call using
the old session's `e3` after this point hits Error 2 (locator not found), which is
handled below with a message that explicitly attributes the miss to navigation.

---

## Surface 2: `stapler_browser_click`

### Call

```json
{
  "tool": "stapler_browser_click",
  "input": {
    "sessionId": "sess-1946a2f0c31-1",
    "refId": "e4",
    "timeoutSeconds": 10
  }
}
```

### Response (success — same-page click, no navigation)

```json
{
  "snapshot": {
    "url": "https://example.com/login",
    "truncated": false,
    "root": {
      "ref": "e1",
      "role": "generic",
      "name": "",
      "children": [
        {
          "ref": "e2",
          "role": "alert",
          "name": "Please enter your password",
          "children": []
        }
      ]
    }
  },
  "note": null
}
```

### Response (success — click triggered navigation)

```json
{
  "snapshot": {
    "url": "https://example.com/dashboard",
    "truncated": false,
    "root": {
      "ref": "e1",
      "role": "generic",
      "name": "",
      "children": [
        { "ref": "e2", "role": "heading", "name": "Welcome back", "children": [] }
      ]
    }
  },
  "note": "click navigated to https://example.com/dashboard; previous element refs are now invalid"
}
```

### Interaction flow

1. Agent picks a `ref` from the most recent snapshot it holds (from `navigate`, or from
   the previous `click`/`type`/`snapshot` response — never a `ref` from more than one
   response ago).
2. `click` always returns a fresh snapshot inline. The agent does **not** need to make a
   separate `stapler_browser_snapshot` call after a click to see the result — this is the
   auto-snapshot-per-mutation convention from `research/ux.md` §2, and it is what makes
   the ref-based locator model tractable (every response re-establishes the only valid
   set of refs).
3. `note` is `null` on an ordinary same-page click (nothing for the agent to react to
   beyond the new snapshot). `note` is populated, in plain language, exactly when the
   click caused navigation — telling the agent up front, before it tries to reuse any
   `ref` from its own memory of the pre-click page, that those refs are now dead. This
   turns what would otherwise be a confusing "why did my next click fail" error into a
   piece of information delivered proactively, at the moment it becomes true.

---

## Surface 3: `stapler_browser_type`

### Call

```json
{
  "tool": "stapler_browser_type",
  "input": {
    "sessionId": "sess-1946a2f0c31-1",
    "refId": "e2",
    "text": "user@example.com",
    "timeoutSeconds": 10
  }
}
```

### Response (success)

```json
{
  "snapshot": {
    "url": "https://example.com/login",
    "truncated": false,
    "root": {
      "ref": "e1",
      "role": "generic",
      "name": "",
      "children": [
        {
          "ref": "e2",
          "role": "textbox",
          "name": "Email",
          "children": [],
          "value": "user@example.com"
        }
      ]
    }
  },
  "note": null
}
```

(Note: `value` on a textbox node reflects the typed content in the returned snapshot so
the agent can confirm the keystrokes landed, without a follow-up `snapshot` call — this
mirrors an accessible-value exposure real screen readers rely on for form controls.)

### Interaction flow: full navigate → click → type → snapshot sequence

```
1. navigate(url)               -> sessionId=S, refs e1..e4 (email, password, submit)
2. type(S, e2, "user@x.com")   -> fresh snapshot, e2.value confirms text landed
3. type(S, e3, "hunter2")      -> fresh snapshot, e3.value confirms text landed
4. click(S, e4)                -> note: "click navigated to .../dashboard; ..."
                                   fresh snapshot with NEW refs (old e1..e4 now dead)
5. (optional) snapshot(S)      -> only needed if the agent wants to re-inspect the
                                   current page without performing a new action —
                                   e.g. after some client-side async render settles
```

Every mutating call (`navigate`, `click`, `type`) is self-sufficient — the agent can
complete the whole login flow above using only `ref`s harvested from prior responses,
never issuing a standalone `stapler_browser_snapshot` call. `snapshot` exists as an
explicit tool specifically for the case where the agent needs to re-observe state without
having just performed a mutation (e.g. waiting on an async UI update, or re-orienting
after an error).

---

## Surface 4: `stapler_browser_snapshot`

### Call

```json
{
  "tool": "stapler_browser_snapshot",
  "input": {
    "sessionId": "sess-1946a2f0c31-1",
    "timeoutSeconds": 10
  }
}
```

### Response (success)

```json
{
  "snapshot": {
    "url": "https://example.com/dashboard",
    "truncated": false,
    "root": {
      "ref": "e1",
      "role": "generic",
      "name": "",
      "children": [
        { "ref": "e2", "role": "heading", "name": "Welcome back", "children": [] },
        { "ref": "e3", "role": "button", "name": "Log out", "children": [] }
      ]
    }
  },
  "note": null
}
```

### Response (success — large page, capped)

```json
{
  "snapshot": {
    "url": "https://example.com/catalog",
    "truncated": true,
    "root": { "ref": "e1", "role": "generic", "name": "", "children": [ "... 400 nodes ..." ] }
  },
  "note": null
}
```

### Interaction flow

`snapshot` is read-only — it never mutates the page or bumps any staleness boundary
beyond the session's idle-timeout `last_used` timer. It is the recovery tool: any error
below that tells the agent to "call `stapler_browser_snapshot`" is asking it to invoke
this exact surface to re-establish a valid set of `ref`s before retrying its intended
action.

`truncated: true` tells the agent the tree was capped for size (matching the
`SearchDocsOutput` truncation convention elsewhere in this repo) — this is a signal to
narrow scope (e.g. re-snapshot is not available to "get more," so the agent should
instead act on what's visible, or use `click`/`type` to navigate deeper into the page and
snapshot a smaller subtree implicitly via the resulting state), not a bare data-loss
warning with no next step.

---

## Error surfaces

### Error 1: session not found

Trigger: any of `click`/`type`/`snapshot` called with a `sessionId` that was never issued,
or was reaped by the idle-timeout, or belongs to a different (e.g. previously shut down)
daemon process.

```json
{
  "error": "no active browser session named 'sess-1946a2f0c31-1'; call stapler_browser_navigate to start a new session"
}
```

Agent's recovery path: call `stapler_browser_navigate` again (no `sessionId`) to start
over. There is no way to "resume" a reaped session — the message says so implicitly by
pointing at *starting a new one*, not at retrying the same session id.

### Error 2: locator (ref) not found in current snapshot

Trigger: `click`/`type` called with a `ref_id` that doesn't exist in the session's
*current* ref table — either hallucinated, or valid in a snapshot that's since been
superseded by a later `navigate`/`click`/`type` response.

```json
{
  "error": "no element with ref 'e9' in current snapshot (page: https://example.com/dashboard); call stapler_browser_snapshot for current refs"
}
```

Agent's recovery path: call `stapler_browser_snapshot` on the same `sessionId` to get a
fresh, authoritative ref table, then retry the intended action with a `ref` taken from
that response.

### Error 3: stale ref after navigation (attributed cause)

Trigger: same underlying condition as Error 2, specifically in the case where the ref
belonged to a snapshot from *before* an in-session navigation the agent may not have
directly observed's `note` (e.g. it used a `ref` from two responses back, skipping over
the response containing the navigation `note`).

```json
{
  "error": "no element with ref 'e3' in current snapshot (page: https://example.com/dashboard) — the current page has navigated since this ref was issued; call stapler_browser_snapshot for current refs"
}
```

This is a variant of Error 2's message with the causal clause added when the adapter can
tell the miss coincides with a navigation event, so the agent isn't left guessing whether
it mistyped a ref or missed a page change — same recovery action (`stapler_browser_snapshot`).

### Error 4: timeout waiting for navigation/element

Trigger: `navigate`'s page load, or `click`/`type`'s dispatch-and-settle, exceeds
`timeoutSeconds` (or the default if unset).

```json
{
  "error": "timeout after 30s waiting for navigation to https://example.com/login"
}
```

```json
{
  "error": "timeout after 10s waiting for click on ref 'e4' (page: https://example.com/login)"
}
```

Agent's recovery path: retry the same call with a larger `timeoutSeconds`, or call
`stapler_browser_snapshot` first to check whether the page actually did settle into a
new, valid state despite the timeout (e.g. a slow-loading page that completed just after
the deadline) before deciding whether to retry or proceed from the new state.

### Error 5: navigation blocked by SSRF guard

Trigger: `navigate`'s target URL (or an in-session redirect/`click`-triggered navigation
via `FrameNavigatedGuard`) resolves to a private/loopback/link-local host under
`NetworkPolicy::Enforce`.

```json
{
  "error": "navigate blocked: host '127.0.0.1' resolves to a private/loopback address and is not allowed (set STAPLER_MCP_ALLOW_PRIVATE_NETWORKS to override)"
}
```

For the `FrameNavigatedGuard` case (an in-page navigation triggered by a prior `click`
or `type` landed on a blocked host) — most often surfaced on the **same call** whose
dispatch caused the navigation (the click/type call itself fails, rather than returning
a snapshot of the blocked content); occasionally, for a navigation not synchronously tied
to a call's dispatch (e.g. a delayed redirect), only on the *next* call against that
session:

```json
{
  "error": "session 'sess-1946a2f0c31-1' navigated to a blocked host '169.254.169.254' during the last action; call stapler_browser_navigate with this sessionId and a safe URL to recover it, or start a fresh session"
}
```

Agent's recovery path: for the first shape, the target URL itself is disallowed — the
agent should not retry the same URL; it should either stop (if the URL came from user
intent to reach that exact private host — a policy decision, not a technical one) or
report the block upstream. For the second shape, the session is blocked but **not
discarded** — no further `click`/`type`/`snapshot` calls against it will succeed until it
is recovered, but calling `stapler_browser_navigate` with the *same* `sessionId` and a
safe URL clears the block and makes the session usable again (this is a supported,
deliberate recovery path, not an error-message inaccuracy — see `implementation/plan.md`
Task 3.4.2). Starting an entirely new session via `stapler_browser_navigate` without a
`sessionId` is also valid, but not required.

### Error 6: session crashed (tab/renderer died)

Trigger: any of `navigate` (session-reuse path)/`click`/`type`/`snapshot` called against a
`sessionId` whose underlying tab/renderer crashed (`Target.targetCrashed`, native; Playwright's
`page.on('crash')`, wasm) — detected by `PortError::SessionCrashed`
(`implementation/plan.md` Epic 2 Story 2.5). This is deliberately worded differently from
Error 1 ("session not found"): the session id is still recognized, but its underlying tab is
dead, not merely absent — the caller needs to know the *same* session id will never work
again, as distinct from a typo'd/reaped id it might otherwise be tempted to double-check.

```json
{
  "error": "session 'sess-1946a2f0c31-1' crashed and is no longer usable; call stapler_browser_navigate (without sessionId) to start a new session"
}
```

Agent's recovery path: identical in effect to Error 1 (start a fresh session via
`stapler_browser_navigate` with no `sessionId`) but the wording tells the agent *why* — the
tab died — rather than implying the id was never valid, and explicitly warns against
retrying the same `sessionId`, since (unlike a reaped/never-existed session, which the
lookup path treats as a plain miss) resending calls against a crashed session's id is not a
transient condition that might resolve on retry.

---

## UX acceptance criteria

1. **Chainable via refs alone.** An LLM caller can complete a
   `navigate → type → type → click → snapshot`-shaped sequence using only `ref` values
   taken from prior tool responses, without ever needing a role/name pair or CSS
   selector, and without a mandatory `stapler_browser_snapshot` call between each
   mutation (`click`/`type` always return a fresh snapshot inline).

2. **Self-correcting session errors.** Every "session not found" error names the invalid
   `sessionId` and states the exact corrective tool call (`call stapler_browser_navigate to
   start a new session`) — never a bare "session error" or "invalid session" with no next step.

3. **Self-correcting locator errors.** Every "locator not found" / stale-ref error names
   the invalid `ref` value and the page URL it was checked against, and states the exact
   corrective tool call (`call stapler_browser_snapshot for current refs`) — an agent should
   never have to guess whether to retry, re-navigate, or re-snapshot.

4. **Navigation is announced, not silently discovered.** Any mutating call
   (`click`/`type`) whose action caused the page to navigate returns a non-null `note`
   naming the new URL and stating that previous refs are now invalid, in the *same*
   response as the navigation happened — an agent should never learn about a navigation
   only via a subsequent locator-not-found error on its next call.

5. **No dead ends.** Every error response (session not found, locator not found, stale
   ref, timeout, SSRF-blocked, session crashed) names a specific next tool call the agent
   can make to either retry or recover (`stapler_browser_navigate` or
   `stapler_browser_snapshot`) — with the sole,
   explicitly-flagged exception of "target URL itself is blocked," where the correct
   agent behavior is to stop rather than retry, and the error message says so by omitting
   any retry suggestion and only offering the escape hatch (env var override) as
   information, not instruction.

6. **Truncation is legible, not silent data loss.** Any snapshot capped for size sets
   `truncated: true` in the same response, so an agent comparing "what I expected to see"
   against "what came back" can attribute a missing element to truncation rather than
   assuming the element doesn't exist or that its `ref` guess was wrong.

7. **Accessible role+name fidelity, not raw tag dump.** `snapshot` output (and the inline
   snapshots returned by `navigate`/`click`/`type`) must label nodes by resolved ARIA
   role and computed accessible name — including implicit roles from plain HTML
   (`<button>`, `<a href>`, `<input type="checkbox">`) and name computed via the
   `aria-labelledby` > `aria-label` > native-labeling > `title` fallback order, not raw
   tag names (`div`, `span`) or DOM attributes — so the tool functions correctly against
   real-world pages that use plain HTML semantics rather than exhaustive ARIA annotation.
   Testable via the native adapter's AX-tree walk unit test (Task 3.1.1's AC: a button
   with no explicit `role` attribute still surfaces as `role: "button"`).

8. **Hidden/non-interactive nodes are pruned.** Nodes marked `aria-hidden`,
   `display:none`, or CDP's own `ignored: true` do not appear in any snapshot — an agent
   should never be offered a `ref` for an element it cannot actually interact with.

9. **Error vs. informational note are visually distinguishable in the response shape.**
   A `note` field on a successful `BrowserActionOutput` (informational — page navigated,
   proceed with new refs) is never confusable with a top-level `error` string (the call
   failed, no snapshot was produced) — an agent parsing the response can branch on
   presence of `error` alone without needing to inspect `note`'s contents to know whether
   the call succeeded.

10. **Idle-session failure is attributed to timing, not treated as a bug.** A "session
    not found" error caused by the 5-minute idle reaper is textually identical to one
    caused by a typo'd `sessionId` (both point at `browser_navigate`) — this is
    deliberate: the agent's correct response is the same in both cases (start a new
    session), so the error shape does not need to (and should not attempt to)
    distinguish "reaped" from "never existed," avoiding a class of error the agent cannot
    act on differently anyway.
