# UX Research: browser-automation MCP tools

Audience framing: the "user" of this interface is an LLM agent making tool calls; the
secondary audience is a human reading the tool-call transcript afterward. This is API/CLI
DX, not GUI UX — clarity, low ambiguity, and token economy matter more than aesthetics.

## 1. Comparable interaction pattern: `@playwright/mcp` (microsoft/playwright-mcp)

VERIFIED via WebSearch (playwright.dev/mcp/snapshots, playwright.dev/mcp/introduction,
github.com/microsoft/playwright-mcp, issue #1639).

`browser_snapshot` returns a YAML-like accessibility tree, not raw HTML or a flat element
list. Example shape:

```yaml
- generic [ref=e1]:
  - banner [ref=e2]:
    - link "QASkills" [ref=e3]
    - navigation "Main" [ref=e4]:
      - link "Skills" [ref=e5]
```

Each node is `<aria-role> "<accessible-name>" [ref=eN]`, nested to reflect DOM/AOM
hierarchy. `ref` ids are opaque, stable *only within one snapshot*, and are the handle
that `browser_click { ref: "e5" }` / `browser_type { ref: "e5", text: ... }` reference back
to — the agent never needs a CSS selector or XPath.

Why this format works better than alternatives for an LLM caller:
- **Signal-to-noise**: raw HTML carries `<div>` soup, class names, inline styles, script
  tags — mostly irrelevant to "what can I click and what does it do." The accessibility
  tree already encodes *semantic* role and name, which is exactly the information an LLM
  needs to decide the next action, at a fraction of the token cost of serialized DOM.
- **Structure without ambiguity**: a flat list of elements ("button: Submit", "input:
  email") loses parent/child and grouping context — an LLM can't tell which "Submit" button
  belongs to which form. Indentation-based YAML nesting preserves that for free, in a
  format LLMs already parse well from pretraining on YAML/config files.
- **Stable-but-scoped refs > selectors**: refs are cheap to generate, guaranteed unique
  within the snapshot, and immune to selector fragility (a `nth-child` or CSS class that
  breaks on redesign). The explicit staleness boundary (refs die at the next snapshot) is
  itself a feature — it forces the agent to re-observe state rather than act on stale
  assumptions, which is the single most common class of browser-automation bug.
- **Token efficiency**: dropping non-accessible-tree DOM attributes is a deliberate
  optimization documented by the Playwright team specifically to reduce prompt length.

## 2. Mental model: navigate → click → type → snapshot

VERIFIED (playwright.dev/mcp/snapshots + community sources): playwright-mcp's default
behavior is that **mutating actions return an updated snapshot automatically** —
"Most tools also return a snapshot automatically after each action, so the LLM always has
up-to-date page state." This is configurable (`PLAYWRIGHT_MCP_SNAPSHOT_MODE=none`), and a
still-open feature request (#1639) shows the ecosystem is still tuning *how* that snapshot
is delivered (inline text vs. a file reference for large pages).

Tradeoff, spelled out:
- **Auto-snapshot-per-mutation** (recommended default): every `click`/`type` response
  includes the resulting tree. Pro: agent never acts on stale refs, no extra round trip,
  matches how a human would naturally verify "did that work" after every interaction. Con:
  pays the token cost of a full snapshot on every single call, even trivial ones (e.g.
  typing one character, or a click that provably didn't change the DOM).
- **Explicit follow-up `snapshot` call required**: cheaper on average token spend, but
  pushes a burden onto the agent to remember to re-snapshot before its next locator-based
  action, and every skipped re-snapshot risks a stale-ref click that fails opaquely, or
  worse, silently succeeds against the wrong element post-navigation.

Given that `stapler-mcp` is a thin-client/daemon architecture already optimizing for
context economy in `docs.rs`/`webcrawl.rs` (chunking, `MAX_CHUNKS_PER_SOURCE`, truncation
flags), recommend **playwright-mcp's default**: auto-return a snapshot after `click`/`type`,
but make it a **diff-oriented or truncated** snapshot when possible (e.g. only emit if the
tree actually changed, or cap depth/nodes with a `truncated: true` flag mirroring the
existing `SearchDocsOutput`/`fetch.rs` truncation conventions in this repo) rather than a
full untruncated tree every time. This keeps the single-round-trip ergonomics without
paying full snapshot cost on trivial or no-op mutations.

## 3. Error UX

This repo's established convention (see `crates/core/src/tools/fetch.rs` and
`crates/core/src/tools/docs.rs`) is: plain `String` errors via `Result<T, String>`,
lower-case, no trailing punctuation beyond periods for a full sentence, actionable —
naming the bad input and telling the caller what to do next. Concretely:

- `fetch.rs:14` — `"url must not be empty"` (precondition, terse).
- `fetch.rs:24` — `format!("render {}: {e}", input.url)` (operation + target + underlying
  cause, colon-separated).
- `docs.rs:750-753` — `unknown_source_error`: names the bad value, **lists the valid
  alternatives** (`currently indexed: {names}`), and tells the caller which tool call fixes
  it (`Call list_indexed_sources ... or index_docs ...`). This is the strongest precedent:
  errors that are self-correcting without a second round trip to a "list" tool.

Applying that convention to browser-automation error cases:

- **Session not found/expired**:
  `"no active browser session named '{id}'; call browser_navigate to start a new session"`
  — mirrors `unknown_source_error`'s "name it + tell them the fix" shape. If multiple
  sessions can be open, list live session ids the same way `currently indexed: {names}` does.
- **Locator not found on current page**:
  `"no element matching role={role} name={name} in current snapshot (page: {url}); call
  browser_snapshot to get current refs before retrying"` — critical to name *which*
  locator failed (role+name or ref id) and point back at the recovery action, since this is
  the single most common agent mistake (stale or hallucinated ref).
- **Click triggers navigation, old snapshot now stale**: this should not surface as a bare
  error — it's the expected/successful case. The response should say so explicitly, e.g.
  `"click navigated to {new_url}; previous element refs are now invalid"` bundled with the
  fresh snapshot, so the agent doesn't try to reuse old refs. If a `ref` from the pre-nav
  snapshot is used *after* this point, that's the "locator not found" error above, and
  should specifically call out the likely cause: `"...; the current page has navigated
  since this ref was issued — call browser_snapshot for current refs"`.
- **Timeout waiting for element/navigation**: `format!("timeout after {secs}s waiting for
  {what} on {url}")` mirroring `fetch.rs`'s `render {url}: {e}` pattern (operation + target
  + duration), with `{what}` naming the specific locator or navigation condition being
  waited on — not a bare "timed out."

General rule carried over from `docs.rs`: **never return an error that just says something
failed without naming the specific input and a next action** — every error above should be
answerable by the agent without a human in the loop.

## 4. Accessibility-tree correctness (not compliance)

Because locators are role+name based, the design must get **accessible name computation**
right against real-world (often poorly-marked-up) pages, or locators will silently fail on
exactly the pages agents are most likely to be sent to (marketing sites, legacy internal
tools). Relevant WCAG/ARIA conventions, applied for *functional correctness*:

- **Accessible name computation order** (per the W3C Accessible Name and Description
  Computation spec, which is what real browsers' accessibility trees already implement,
  and which Playwright's own `ariaSnapshot()`/CDP-backed tree inherits for free):
  `aria-labelledby` > `aria-label` > native host-language labeling (`<label for>`,
  `<img alt>`, button/link text content) > `title` attribute. Note the order is
  labelledby-before-label, not the reverse — get this backwards and role/name locators
  will pick the wrong element on any page using both.
- **Implicit ARIA roles**: many real pages use plain HTML with no explicit `role`
  attribute at all (`<button>`, `<a href>`, `<input type="checkbox">`, `<nav>`, `<main>`).
  The locator/snapshot layer must resolve implicit roles from the HTML element+attribute
  mapping (the [ARIA in HTML](https://www.w3.org/TR/html-aria/) spec), not require explicit
  `role="button"` — otherwise the tool only works on well-annotated pages, which defeats
  the purpose of using the accessibility tree over raw HTML in the first place.
- **Name computed on the innerText/content fallback**: for elements with no ARIA label of
  any kind, fall back to visible text content (trimmed, collapsed whitespace) — this is
  the common case for `<button>Submit</button>`-style markup with zero ARIA authoring.
- **Duplicate/ambiguous names**: real pages routinely have multiple elements with the same
  accessible name (e.g. several "Delete" buttons in a list). The snapshot's `ref` mechanism
  sidesteps this — the agent should be steered toward using `ref` from a fresh snapshot for
  disambiguation rather than role+name alone once a "click" error reports "N elements match
  role={role} name={name}; ambiguous" — a good additional error case to add to §3.
- **Hidden/non-interactive nodes should be pruned** from the snapshot (`aria-hidden="true"`,
  `display:none`, `visibility:hidden`, `inert`) — matching what real assistive tech exposes
  — both to keep the tree accurate for the agent's decision-making and to save tokens.

## 5. Jobs-to-be-done: browser-automation vs. `fetch_page`

`fetch_page` (this repo, `crates/core/src/tools/fetch.rs`) is a one-shot, stateless
navigate-and-extract: give it a URL, get back title/text/final_url. It has no session, no
ability to act on the page, and no loop.

The job an agent reaches for browser-automation to do is specifically: **"I need to change
page state through interaction, then observe the *new* state, possibly repeating this
several times, because the content I need isn't reachable by a single GET-and-render."**
Concretely:
- Logging in (fill credential fields, submit, land on an authenticated page `fetch_page`
  could never reach cold).
- Paginating through client-rendered results (click "next," re-snapshot, repeat) where the
  underlying data isn't in the initial HTML/DOM at all (SPA, infinite scroll, JS-gated).
  content).
- Multi-step forms/wizards where each step's fields depend on the previous step's submission.
- Any UI whose relevant state is **behind a JS event handler** rather than encoded in the
  URL — `fetch_page` renders once and extracts; it cannot dispatch a click or keystroke and
  wait for a resulting DOM mutation.

The dividing line for choosing between the two tools, worth stating explicitly to callers
(e.g. in tool descriptions the LLM sees): if the target content is reachable by loading one
URL, use `fetch_page` — it's cheaper (no session lifecycle, no snapshot bookkeeping). Reach
for browser-automation only when the task requires *causing* a state change and observing
its result, not just reading a page.
