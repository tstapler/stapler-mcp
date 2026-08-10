# Build vs. Buy: browser-automation

Scope note: "keep using playwright-mcp as-is" (status quo) is already rejected per
requirements.md — not re-litigated here. This assesses build-vs-buy for the four
*sub-components* of the native/wasm implementation.

## 1. AX-tree walking on native (chromiumoxide)

Current state: `crates/native/src/browser.rs` uses `chromiumoxide` 0.9 directly against
raw CDP domains (`page.evaluate`, `page.content`, etc.) — no AX-tree helper today.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| Hand-roll AX walking on `chromiumoxide_cdp::cdp::browser_protocol::accessibility` (as requirements.md proposes) | Full control; `chromiumoxide_cdp` already generates typed Rust structs for every `Accessibility` domain command/event (`GetFullAxTree`, `AxNode`, `AxValue`, etc.) — the wire-format parsing is already solved, only the tree-walk/role-name logic is new | Role/name *resolution* (accname algorithm) is not free — CDP's `AXNode.name`/`AXNode.role` fields already carry Chromium's own accname-computed values per node, so "hand-rolling" here is mostly tree traversal + filtering ignored/hidden nodes, not reimplementing accname from scratch (see item 4) | **Recommended** |
| Adopt a dedicated "CDP accessibility tree" crate | N/A | Searched crates.io/GitHub for "chrome accessibility tree rust", "CDP AX tree crate": no crate found that wraps CDP's `Accessibility` domain into ready role/name locators. Closest hits (`accessibility-tree`, `accesskit`) are platform-native (Windows UIA/macOS NSAccessibility/AT-SPI) accessibility-tree libraries unrelated to Chrome DevTools Protocol — wrong abstraction layer entirely | **Not recommended** — doesn't exist |
| Adopt/fork `playwright-rust` and reuse its AX code | N/A | No maintained `playwright-rust` exists that reimplements Playwright's engine in Rust; searches turn up only thin CDP wrapper crates (chromiumoxide, headless_chrome) or bindings that shell out to the Node Playwright driver, not a Rust port of Playwright's snapshot/locator logic | **Not recommended** — doesn't exist |

**Bottom line**: nothing to buy here. The real work is walking the tree CDP already
returns (via `chromiumoxide_cdp`'s generated types) and mapping it into stable
locators — closer to "assemble from typed primitives" than "invent an algorithm."

## 2. Native browser driver library choice

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| **Keep `chromiumoxide`** (status quo, already used for `fetch_page`) | Zero switching cost — `crates/native/src/browser.rs` and `Cargo.toml` already depend on it; async/tokio-native (matches the daemon's async runtime, per file header comment "tokio + fs4 + reqwest + chromiumoxide"); actively maintained; exposes the raw CDP `Accessibility` domain needed for item 1; supports persistent `Page` objects and multiple tabs on one `Browser` (needed for session-scoped navigate/click/type) | Lower-level API than Playwright — no built-in locator/waiting ergonomics (expected, since that's exactly the gap requirements.md scopes in) | **Recommended** |
| `headless_chrome` (rust-headless-chrome) | Higher-level, Puppeteer-like API | Synchronous API built on threads, not async/tokio — would require bridging into the daemon's async runtime, adding complexity rather than removing it; still CDP/Chrome-only so doesn't solve the AX-tree gap either; switching cost (rewrite `fetch_page` + new session code) with no offsetting capability gain | **Not recommended** |
| `fantoccini` | Cross-browser via WebDriver; mature/battle-tested; async | WebDriver protocol has no equivalent to CDP's `Accessibility` domain (no raw AX-tree access) — would make item 1 *harder*, not easier, defeating the point of this feature; requires a WebDriver server (geckodriver/chromedriver) as an extra process, working against the "single native daemon" goal | **Not recommended** |
| A maintained `playwright-rust` binding | Would directly reuse Playwright's own accname/locator engine | Does not exist as a maintained project (see item 1) | **Not viable** — doesn't exist |

**Bottom line**: keep `chromiumoxide`. Switching cost (rewriting the existing
`fetch_page` adapter plus building new session/AX code twice) is real and none of the
alternatives close the AX-tree gap better — `fantoccini` is actively worse for it since
WebDriver has no CDP Accessibility domain equivalent.

## 3. Wasm/playwright-core wiring

Current state: `crates/wasm/src/browser.rs` already calls into a hand-written JS glue
module (`src/glue/browser.js`) via `wasm_bindgen`, which presumably wraps a Node-hosted
Playwright browser. Extending this is adding more glue functions, not inventing a new
bridging mechanism.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| Write thin custom glue directly against `playwright-core`'s public API (`page.accessibility.snapshot()`, `page.click()`, `page.type()`, session/context management) | `playwright-core` is the same npm package already implied by the wasm adapter's existing JS glue pattern; its accessibility snapshot API is public, documented, and stable; matches the existing `js_navigate_and_extract`/`js_close_browser` glue-function pattern in `browser.js` — additive, no architectural change | Still requires writing/maintaining the session-lifecycle glue (tracking Page objects, idle timeout hooks) in JS | **Recommended** |
| Extract/adapt `@playwright/mcp`'s internal browser-session-management modules | `@playwright/mcp` (microsoft/playwright-mcp, Apache-2.0) is open source and solves the identical problem (persistent sessions, incremental AX snapshots, ref-based locators) | It's a full standalone MCP server (its own tool-dispatch, config, CLI, protocol framing), not a library with a stable importable API; its internals aren't published to npm as a separate package and are coupled to its own server lifecycle/config surface — extracting specific files means forking+maintaining someone else's internal module boundary, a worse ongoing cost than writing ~50-100 lines of glue directly against `playwright-core`'s stable public API | **Not recommended** — attractive on paper, higher long-run cost than direct API glue |
| Fork `@playwright/mcp` wholesale | N/A | Explicitly out of scope per requirements.md's own framing (it's a full MCP server, wrong shape) | **Not recommended** (already excluded) |

**Bottom line**: write glue directly against `playwright-core`, following the existing
`browser.js` pattern. `@playwright/mcp`'s source is worth *reading* for design ideas
(e.g., how it handles ref staleness / incremental snapshots) but not worth extracting
as code.

## 4. LLM-generated vs. battle-tested: AX-tree-to-locator resolution

Key finding that changes the framing: CDP's `Accessibility.getFullAXTree` /
`Accessibility.getAXNodeAndAncestors` already return nodes with `role` and `name`
fields **pre-computed by Chromium's own accname implementation** — the browser has
already run the W3C accname algorithm before the data ever reaches Rust. This is
different from, say, writing an accname computer over raw HTML/ARIA from scratch.

| Option | Pros | Cons | Verdict |
|---|---|---|---|
| Consume CDP's pre-computed `role`/`name` fields directly, write only the tree-walk + locator-matching logic in Rust (fuzzy/exact match against `name`, filter by `role`, handle ignored/hidden nodes) | No accname algorithm to hand-write or port — Chromium did that part; matching logic (string compare + role filter + tree traversal) is simple, well-testable, low corner-case surface; matches "only used for locator-matching, not full a11y compliance" scoping in requirements.md | Still need to handle CDP-specific quirks (ignored nodes, `AXValueType` variants, nodes without a `backendDOMNodeId`) — a smaller but real edge-case set, distinct from full accname | **Recommended for v1** |
| Hand-write/port a full W3C accname algorithm from scratch in Rust | Would be needed only if working from raw DOM/ARIA instead of CDP's AX domain | Unnecessary — duplicates work Chromium already did; higher risk of subtle divergence from browser behavior (the exact problem this option is meant to avoid); no existing accname Rust crate to build on (confirmed via search — closest is `accesskit`, which targets platform-native accessibility APIs, not accname-from-HTML) | **Not recommended** — solves a problem that doesn't exist given the CDP path |
| Adopt/port an existing accname implementation from another language (e.g. axe-core's or a browser engine's) | Battle-tested edge-case handling if truly needed | No packaged, embeddable Rust port exists; porting axe-core's (JS) accname logic is itself nontrivial LLM-generated-from-scratch work — doesn't actually buy anything over option 1 given CDP already supplies computed names | **Not recommended** |

**Bottom line**: this isn't really a "hand-write a well-specified W3C algorithm" risk
in practice, because Chromium's CDP `Accessibility` domain already exposes
accname-computed `name`/`role` per node. The actual novel code is tree traversal and
locator matching over already-resolved values — a much smaller, lower-risk surface
than full accname computation. Scope v1's hand-written portion to that traversal/match
logic only, and treat "port accname" as unnecessary rather than deferred.

## Summary of recommendations

1. **AX-tree walking**: no crate exists to buy; build tree-walk/locator-match logic on
   top of `chromiumoxide_cdp`'s already-generated CDP `Accessibility` domain types.
2. **Native driver**: keep `chromiumoxide` — no alternative (`fantoccini`,
   `headless_chrome`) improves the AX-tree story, and switching cost is real.
3. **Wasm wiring**: write thin custom glue directly against `playwright-core`'s public
   `page.accessibility.snapshot()` API, following the existing `browser.js` pattern —
   don't attempt to extract `@playwright/mcp`'s internals (it's a server, not a
   library).
4. **Accname risk**: lower than requirements.md implies — CDP already returns
   Chromium's own accname-computed `role`/`name` per node, so v1's hand-written code is
   traversal/matching over pre-resolved values, not a from-scratch accname port.
