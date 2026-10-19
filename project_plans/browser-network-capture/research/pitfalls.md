# Research: Pitfalls

**Date**: 2026-10-10

## P1: CDP response body retrieval timing

`Network.getResponseBody` can only retrieve a response body while Chrome still holds
it in memory. Chrome evicts response bodies from memory as part of its own GC, independently
of the CDP session. On a page with many resources (images, fonts, scripts), the bodies
of early responses may be evicted before a deferred `getResponseBody` call reaches them.

**Mitigation**: call `getResponseBody` synchronously (immediately) inside the
`EventLoadingFinished` handler, not lazily on-demand. This means bodies are retrieved at
capture time and stored in the per-session `Vec<NetworkEntry>`. The trade-off is memory
(bodies stored in-process) vs. reliability (bodies available when caller requests them).
The per-body cap (64 KB default) and per-session entry cap (1000 entries) bound the memory.

## P2: CDP Network.enable scope and session creation race

CDP's `Network.enable` must be called before any requests are made. If `Network.enable`
is called *after* `page.goto()`, requests made during the initial load are not captured.

**Mitigation**: call `page.enable_network()` immediately after the `Page` is created
(before the first `goto` call), not inside `navigate()`. The `BrowserSession` constructor
(`NativeBrowser::open_session`) is the right place — same location where `EventTargetCrashed`
and `EventFrameNavigated` subscriptions are set up.

## P3: wasm response body retrieval

`playwright-core`'s `response.body()` method returns a Node.js `Buffer`. The wasm-bindgen
boundary only handles primitive types and `Uint8Array`/`ArrayBuffer` — it cannot pass
a `Buffer` directly.

**Mitigation**: in the JS glue, convert via `Buffer.from(await response.body()).toString('base64')`
and decode in Rust, or use `Buffer.from(await response.body()).toString('utf-8')` for text
responses directly. Wrap in try/catch since `response.body()` can throw for responses whose
bodies were never buffered (e.g. streamed responses the page never consumed).

## P4: memory pressure from body accumulation

A typical SPA page load fires 50–200 network requests. With 64 KB per body cap and 1000
entry cap per session, worst case per-session memory is 64 MB. With `MAX_OPEN_SESSIONS = 20`,
worst case total is 1.28 GB — unacceptable.

**Mitigation**: Apply a stricter default cap for body storage:
- Default `max_body_bytes = 16 KB` per response (caller can override per call)
- Hard entry cap: 1000 per session (truncation flag set)
- Bodies for resource types unlikely to contain useful data (images, fonts, binary) should
  be skipped by default — detect via `content-type`: if `content_type` starts with
  `image/`, `font/`, `audio/`, `video/`, store `body: null` unconditionally.

With this mitigation, worst-case per-session memory for body storage drops to ~16 MB
(1000 entries × 16 KB), and total across 20 sessions to ~320 MB — still large but bounded.

## P5: `navigate` auto-clear vs. multi-tab sessions

The requirements say "automatically cleared on each `navigate` call." But sessions now
support multiple tabs. If the caller navigates tab 2 while tab 1's entries are still
needed, the auto-clear should apply only to the currently active tab's entries, not
the whole session's list.

**Design decision**: for v1, maintain one flat `network_entries` list per session
(not per-tab). Auto-clear on any navigate within the session. This is the simpler model
and matches the single-tab-per-session recommendation in the browser-automation requirements.
Document this limitation explicitly: callers that need to preserve entries across a tab
navigate should call `network_requests` before navigating.

## P6: SSRF and response body leakage

If a page is navigated to a blocked host via an in-page redirect (after `FrameNavigated`
fires the SSRF guard), the `BlockedFlag` is set and subsequent tool calls fail. However,
network requests that fired *before* the SSRF guard tripped may have accumulated in the
session's `network_entries`. These entries could include request/response data from the
blocked host.

**Mitigation**: when `blocked = true` is set on a session, the `network_requests` call
should return `PortError::NotFound` (the same "session is blocked, re-navigate" error
as every other call on a blocked session). This prevents leaking data from blocked hosts
via the network entries list.
