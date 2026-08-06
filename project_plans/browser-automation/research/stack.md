# Stack Research: browser-automation

## Current codebase state (VERIFIED — read directly)

- `crates/core/src/ports.rs:107-128` — `BrowserDriver` trait has exactly one method today: `navigate_and_extract(url, timeout) -> PageExtract` (title/html/text/final_url). No session concept exists in the port at all; both adapters implement only this one method.
- `crates/native/Cargo.toml` — `chromiumoxide = "0.9"` (unpinned patch), plus `tokio`, `futures`, `reqwest`, `fastembed`. No other browser-related crate.
- `crates/native/src/browser.rs` — `NativeBrowser` holds a single `Browser` for the daemon's lifetime; `navigate_and_extract` creates a **fresh `Page` per call** ("fresh tab per call", explicit comment at line 9-11) and never keeps it around. There is no page registry/session map today — this is the main structural gap for the new feature (needs a `HashMap<SessionId, Page>` or similar, keyed and reaped).
- `crates/wasm/Cargo.toml` — `wasm-bindgen`, `wasm-bindgen-futures`, `js-sys`, `serde`/`serde_json`, `schemars`. No JS/npm dependency is declared here (wasm crate itself has no npm deps — those live in `npm/package.json`).
- `crates/wasm/src/browser.rs` — thin `extern "C"` bindings to a hand-written JS glue module at `crates/wasm/src/glue/browser.js`, calling exactly two JS functions: `jsNavigateAndExtract` and `jsCloseBrowser`.
- `crates/wasm/src/glue/browser.js` — **`playwright-core` is already required and used** (`const { chromium } = require("playwright-core")`), with a single lazily-launched shared `chromium.launch({ headless: true, channel: "chrome" })` browser instance for the daemon's lifetime. `jsNavigateAndExtract` does `browser.newPage()` → `goto` → read `title`/`content()`/`innerText` → `page.close()` (also fresh-page-per-call, no session persistence).
- `npm/package.json` — dependencies are `@modelcontextprotocol/sdk@^1.29.0` and **`playwright-core@^1.61.1`**. This is the only npm dependency relevant to browser automation; it is already present, so no *new* npm package is needed for the wasm side — only new JS glue functions and (optionally) a version bump.

**Conclusion: no new Cargo or npm dependencies are required.** Both `chromiumoxide` (native) and `playwright-core` (wasm) are already in the tree and both expose the APIs this feature needs. The work is entirely in the port trait shape, the native page-registry, and new JS glue functions — not dependency acquisition.

## External research findings

### chromiumoxide (native adapter)
- Current crates.io version: **0.9.1** (`crates/native/Cargo.toml` pins `"0.9"`, which resolves to 0.9.1 — no bump needed for baseline compatibility). [crates.io/crates/chromiumoxide](https://crates.io/crates/chromiumoxide)
- The crate code-generates CDP bindings directly from Chrome's PDL protocol files, so `Accessibility.getFullAXTree` and `Accessibility.queryAXTree` **are represented as typed Rust bindings**: `GetFullAxTreeParams`/`GetFullAxTreeReturns` and `QueryAxTreeParams`/`QueryAxTreeReturns` in `cdp::browser_protocol::accessibility`. This confirms the port's planned `snapshot`/click-locator work is achievable without dropping to raw CDP JSON.
- **Behavioral gotchas to account for in the plan** (apply regardless of client library, so they affect the native adapter directly):
  - `getFullAXTree` on recent Chrome now requires an explicit `frameId` — omitting it returns only the root `RootWebArea` node instead of the full tree.
  - `queryAXTree` similarly needs explicit `backendNodeId` scoping to the document root; unscoped queries can return empty results even when matching nodes exist.
  - Chrome builds its accessibility tree **lazily** — the first `queryAXTree`/`getFullAXTree` call on a freshly navigated page can return an empty/partial tree because the tree hasn't been constructed yet. Common workaround: issue a priming `getFullAXTree` call (and/or wait for a load event) before the first real query. This directly affects the `navigate` → `snapshot` sequence in scope — the initial snapshot returned by `navigate` needs this priming step or it may come back empty.
  - No documented chromiumoxide-specific regressions against the new (non-legacy) Chrome headless mode were found beyond generic ecosystem headless-mode flux; treat this as a residual risk to validate empirically in phase 4/5 (spin up a persistent `Page`, hold it across two calls, confirm CDP session doesn't get torn down by Chrome's target-lifecycle changes).

### playwright-core (wasm adapter)
- Current npm version: **1.62.1** (repo pins `^1.61.1`, so `npm install` today would already resolve to the 1.62.x line under that caret range — effectively already current; no Cargo/package.json edit strictly required, though pinning the caret base up to `^1.62.1` is a reasonable hygiene bump). [npmjs.com/package/playwright-core](https://www.npmjs.com/package/playwright-core)
- **API shape has moved on from `page.accessibility.snapshot()`**: that legacy API is effectively superseded. The current, actively-developed surface is **ARIA snapshots**: `page.ariaSnapshot()` / `locator.ariaSnapshot()`, returning a YAML-ish structured representation of the accessibility tree (role, accessible name, headings, form fields, `/children`, `/url` properties). This is the API the wasm glue's new `snapshot` operation should call, not the deprecated `page.accessibility` namespace.
- Locators built from role+name (matching the requirement's "role+name, matching playwright's model") map directly onto Playwright's existing `page.getByRole(role, { name })` — this is already the idiomatic Playwright locator API and requires no new dependency, just new glue functions (`jsClick`, `jsType`, `jsSnapshot`, session-aware `jsNavigate` that returns a `sessionId` and keeps the `Page` object alive keyed by that id, mirroring what the native side needs to do).
- No evidence found of `playwright-core`'s wasm/Node API withholding persistent-session/page access — `browser.newPage()` returns a normal `Page` object that can simply be retained in a JS-side `Map<sessionId, Page>` for the lifetime of the session, closed by the reaper. This is a plain Node-side data-structure addition, not a wasm-bindgen capability gap — de-risks the "wasm-bindgen JS glue may not expose persistent-session APIs" feasibility risk called out in requirements.md.

### chromiumoxide vs. modern headless Chrome — general risk note
- Community consensus (comparison articles, Chromium issue tracker threads) treats `chromiumoxide` as functional but less mature/battle-tested than Playwright/Puppeteer for multi-session or long-lived-instance scenarios. No specific confirmed regression was found for persistent multi-tab sessions against current Chrome, but Chrome's headless-mode architecture change (old separate headless binary → new headless mode built on the standard binary) is a known general source of CDP behavioral drift across all CDP clients (chromiumoxide, puppeteer, playwright alike). Recommend a smoke test early in implementation: launch, open two persistent `Page`s concurrently, hold across an `await` boundary spanning multiple tool calls, confirm no CDP session/target teardown.

## Recommended versions / dependency actions
| Component | Current pin | Latest available | Action |
|---|---|---|---|
| `chromiumoxide` (native) | `"0.9"` (`crates/native/Cargo.toml`) | 0.9.1 | No change needed; already resolves current. |
| `playwright-core` (npm) | `^1.61.1` (`npm/package.json`) | 1.62.1 | Optional bump of the caret base to `^1.62.1` for hygiene; not required for the APIs this feature uses. |
| New Cargo deps | — | — | None required. |
| New npm deps | — | — | None required — `playwright-core` already present. |

## Patterns to follow
- Session registry: native side needs an internal `Mutex<HashMap<String, Page>>` (or similar) inside `NativeBrowser`, generating a `session_id` (e.g. `uuid` — **not currently a dependency**; either add the `uuid` crate or generate IDs via an existing hashing/random primitive already in the workspace — check `Cargo.lock` for an existing `uuid`/`rand` transitively before adding a new direct dependency).
- Wasm side: mirror with a JS `Map` keyed by the same `session_id` scheme in `crates/wasm/src/glue/browser.js`, new exported `jsNavigate`/`jsClick`/`jsType`/`jsSnapshot`/functions alongside the existing `jsNavigateAndExtract`/`jsCloseBrowser`, following the existing `#[wasm_bindgen(module = "/src/glue/browser.js")] extern "C"` binding pattern in `crates/wasm/src/browser.rs`.
- Idle-timeout reaper: background `tokio::spawn` task on native side (pattern already exists for the CDP event-stream drain in `NativeBrowser::launch`, `crates/native/src/browser.rs:27`) sweeping the session map on a timer; wasm side has no persistent background-task primitive today (JS glue is only called reactively from Rust) — reaper timing will likely need to live on the Rust side of the wasm crate (a `setTimeout`-driven JS reaper, or a Rust-side timer that calls into JS to close idle sessions) since wasm-bindgen modules don't have their own independent async runtime distinct from what the host (Node) drives.
