# Requirements: browser-network-capture

**Date**: 2026-10-10
**Type**: feature addition
**Complexity**: 3 — moderate cross-cutting addition to an existing hexagonal-architecture feature

## Problem Statement

When an LLM agent drives a browser session via `stapler_browser_*` tools, there is no way to
inspect the underlying network activity that occurred — no request/response metadata, no
headers, no status codes, no captured XHR/fetch payloads. This limits two distinct use cases:

1. **Debugging**: when a page doesn't load expected content, the agent can't tell whether the
   page returned a 404, a redirect, or a 200 with empty data.
2. **Data extraction**: some applications expose data only via XHR/fetch API responses (not
   rendered into the DOM), so an accessibility-tree snapshot is insufficient; the agent needs
   to read the JSON body of a specific API call.

The item notes that `vercel-labs/agent-browser` includes HAR capture as one of its `network`
tool-profile capabilities, suggesting this is an established pattern in the space.

## Baseline

Today's `BrowserDriver` port (`crates/core/src/ports.rs`) has no method for capturing network
events. The `chromiumoxide` native adapter exposes the full Chrome DevTools Protocol, which
includes a `Network` domain that can be enabled to record all requests and responses. The wasm
adapter delegates to `playwright-core`, which exposes `page.on('request')`/`page.on('response')`
and `page.request.allHeaders()`. Neither adapter hooks into these APIs today.

The existing browser session model (persistent `Page` per `SessionId`, `SessionRegistry`)
provides the right attachment point: per-session request lists can be accumulated into the
session's in-memory state (alongside `last_used`, `latest_snapshot`) and exposed on demand.

## Users / Consumers

LLM agents (via MCP tool calls) that:
- need to debug why a page didn't render expected content (check HTTP status codes and
  response bodies for key requests)
- need to extract data that only appears in XHR/fetch response bodies (never rendered into DOM)
- want to understand the network footprint of a page load for analysis

## Success Metrics

- A new `stapler_browser_network_requests` MCP tool exists, registered on both native
  and wasm daemons, that returns an ordered list of network request records captured
  during a session since the last `navigate` (or explicit clear), including at minimum:
  URL, method, status code, response content-type, and response body (truncated past a
  cap).
- An optional `filter_url` parameter narrows results to URLs containing a given substring,
  avoiding large payloads when the caller only cares about one API endpoint.
- An optional `save_har` parameter causes a spec-compliant HAR 1.2 JSON document to be
  written to a caller-supplied path for offline analysis.
- A session's accumulated request list is automatically cleared on each `navigate` call
  (so a fresh navigation starts with a clean slate), while remaining available until the
  next navigate or explicit clear call.
- Native adapter: request capture works on real Chromium via CDP's `Network` domain —
  verified by an `#[ignore]`d integration test that navigates to a test page making an
  XHR call and confirms the `network_requests` output includes the XHR's URL and status.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings`
  both pass with no new warnings.

## Appetite

Small–medium (1–2 weeks). This is additive: one new `BrowserDriver` method, one new
tool function, per-session accumulation storage. The hard parts are:
1. Deciding what subset of CDP `Network` events to capture (not all 30+ events are
   useful to an LLM agent).
2. Response body retrieval — CDP and playwright-core have subtly different APIs for
   reading response bodies, and some responses (streaming, gzipped) need care.
3. Memory management — unbounded accumulation in long-lived sessions would be a memory
   leak; a per-session cap is necessary.

## Constraints

- Must preserve all existing `BrowserDriver` methods and their semantics unchanged.
- The new tool must not be registered by default if a tool-profile gating mechanism
  exists (per the item's description — "gated behind the same tool-profile mechanism
  proposed for selectable MCP tool profiles so it doesn't bloat the default tool schema").
  If no such mechanism exists yet, it is registered unconditionally for now with a note
  that it should be gated when that feature lands.
- Native and wasm adapters must both implement the new `BrowserDriver::network_requests`
  method (existing convention: every port method has at least a wasm implementation,
  per `ports.rs:139-145`'s Embedder note being the deliberate sole exception).
- Response body capture is best-effort: bodies unavailable through the adapter (e.g.
  streaming responses, or responses whose body was never requested by the page) should
  appear in the record with `body: null` rather than causing an error.

## Non-functional Requirements

- **Memory**: each session's accumulated request list must be bounded (hard cap:
  1000 entries; truncation flag set when exceeded). Response bodies are truncated at
  a separate cap (e.g. 64 KB per body) to limit per-session memory.
- **Performance**: request recording must not block the page's network activity — CDP's
  Network events are fired asynchronously; recording is write-once into a per-session
  `Vec`, not a synchronous interception that can delay page loading.
- **Security classification**: same as existing browser tools — local daemon, no
  persisted state, trust model identical to `fetch_page`.

## Scope

### In Scope
- New `BrowserDriver::network_requests` method returning a `NetworkRequestsResult`
  (list of `NetworkEntry` records, plus `truncated: bool`).
- New `NetworkEntry` port type: `url`, `method`, `status: Option<u16>`,
  `content_type: Option<String>`, `body: Option<String>` (UTF-8, truncated at cap),
  `body_truncated: bool`, `timing_ms: Option<u64>` (total request-to-response time).
- Optional `save_har` output path (writes a HAR 1.2 JSON document to disk).
- Optional `filter_url` substring filter applied before returning.
- Optional `clear` flag to reset the session's accumulated list without returning entries.
- Per-session accumulation: the native adapter subscribes to CDP `Network.requestWillBeSent`
  and `Network.responseReceived` / `Network.loadingFinished` events; the wasm adapter
  uses `page.on('request')`/`page.on('response')`.
- Automatic clear on `navigate` (new session starts clean; re-navigate on existing session
  resets the list).
- New MCP tool schema types: `BrowserNetworkRequestsInput`, `BrowserNetworkRequestsOutput`,
  `BrowserNetworkEntry` (following `FetchPageInput`/`FetchPageOutput`'s camelCase + schemars
  pattern).
- Unit tests (fake driver) and one `#[ignore]`d integration test (native adapter against
  a real Chromium with a test page making an XHR call).

### Out of Scope
- Request interception / modification (read-only capture only).
- WebSocket frame capture.
- Network throttling or offline simulation.
- Tool-profile gating mechanism itself (that's a separate feature; this item notes it as
  the intended long-term home for this tool).
- HAR viewer UI.

## Rabbit Holes

- **CDP response body retrieval timing**: `Network.getResponseBody` must be called
  *before* the response is garbage-collected by Chrome. For large pages with many
  resources this can be tricky — the safest approach is to capture bodies in the
  `loadingFinished` event handler synchronously, not lazily on demand. Budget time
  for this.
- **wasm `playwright-core` response body access**: `playwright-core`'s Node API
  (`response.body()`) returns a `Buffer`; wasm-bindgen can't directly pass Node
  `Buffer` objects across the boundary, so the JS glue will need to convert to
  `base64` or `Uint8Array`. Verify the actual wasm glue's JS-Rust boundary shape before
  committing to an approach.
- **Memory pressure from response body accumulation**: a single SPA page load can
  easily fire 100+ network requests. At 64 KB per body cap, that's 6.4 MB per session
  just for response bodies. With `MAX_OPEN_SESSIONS = 20` and all sessions busy, worst
  case is ~128 MB. This is acceptable for a local single-user daemon but needs explicit
  documentation and may warrant a lower default cap.

## Alternatives Considered

- **Return network data via `evaluate` (JS injection)**: callers could inject JS to
  intercept `fetch` and `XMLHttpRequest`. Rejected: requires page-level injection that
  doesn't capture all requests (e.g. CSS, images, redirects before JS runs), is fragile
  against CSP, and adds caller complexity — a dedicated port method is cleaner.
- **Playwright-mcp's `browser_network_requests` tool as the model**: the item's description
  explicitly references `vercel-labs/agent-browser` as prior art. Adopting its interface
  shape (URL list + optional filter + HAR export) as the model reduces design work.
- **Passive only (no body capture)**: capturing URL + status only without bodies was
  considered as a scope reduction. Rejected: the item's primary motivation for data
  extraction requires response bodies; a URL+status-only tool serves debugging but not
  data extraction.

## Open Questions

- Should `network_requests` accept a `since_navigate` boolean (only entries since the
  most recent navigate within the session) vs. always returning all entries since the
  last auto-clear? The auto-clear-on-navigate behavior probably makes this moot, but
  multi-tab sessions may need clarification.
- What is the right per-body cap? 64 KB is a reasonable default for LLM consumption,
  but an API returning a large JSON blob may legitimately exceed it. Consider making
  it a configurable input parameter (`max_body_bytes`).
- Tool-profile gating: is the mechanism from the referenced item available yet? If so,
  this tool should be placed in a non-default profile from day one.
