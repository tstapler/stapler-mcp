# Research: Architecture Fit

**Date**: 2026-10-10
**Source**: codebase inspection

## Existing session state model

`NativeBrowser` (`crates/native/src/browser.rs`) holds a `SessionRegistry`:
`Rc<RefCell<HashMap<SessionId, BrowserSession>>>`. Each `BrowserSession` currently stores:
- a `Page` handle (chromiumoxide)
- `last_used: u64` timestamp
- a `latest_snapshot` ref table (opaque `ref` → `BackendNodeId` map)
- a `blocked: bool` SSRF flag
- a `crashed: bool` flag
- active tab index and tab list

Adding a per-session `network_entries: Vec<NetworkEntry>` with a corresponding
`network_truncated: bool` fits cleanly into this struct. No new concurrency primitive
needed — the `RefCell` borrow discipline already in place covers it.

## CDP Network domain integration

`chromiumoxide` exposes the full CDP `Network` domain. To record network activity:

1. Call `page.enable_network()` (or `page.execute(EnableParams { ... })` for
   `Network.enable`) when the session is created — once per session, not per request.
2. Subscribe to `EventRequestWillBeSent` (request URL, method, initiator).
3. Subscribe to `EventResponseReceived` (status code, headers, MIME type).
4. Subscribe to `EventLoadingFinished` (to trigger body retrieval via
   `Network.getResponseBody`).

These events are already available through `page.event_listener()` (the same mechanism
used for `EventFrameNavigated` and `EventTargetCrashed` in the existing browser.rs).

Key constraint: `Network.getResponseBody` must be called while the response is still
in Chrome's memory (before the page's garbage collector clears it). The safest approach
is to call it synchronously inside the `EventLoadingFinished` handler.

## wasm adapter

The wasm adapter delegates to `playwright-core` via JS glue (`crates/wasm/src/glue/`).
`playwright-core` exposes `page.on('request', handler)` and `page.on('response', handler)`.
Response bodies: `await response.body()` returns a Node `Buffer`.

The wasm-bindgen boundary for returning response bodies needs care:
- `Buffer` cannot cross wasm-bindgen directly
- Must convert to `Uint8Array` or `base64` string in JS before passing to Rust

The JS glue should implement the same per-session `networkEntries` array that the native
adapter maintains in Rust, appending entries in response event handlers.

## Port trait extension

`BrowserDriver` in `crates/core/src/ports.rs` needs one new method:

```rust
async fn network_requests(
    &self,
    session_id: &SessionId,
    filter_url: Option<&str>,
    max_entries: Option<usize>,
    clear: bool,
    timeout: Duration,
) -> Result<NetworkRequestsResult, PortError>;
```

New types needed:
```rust
pub struct NetworkEntry {
    pub url: String,
    pub method: String,
    pub status: Option<u16>,
    pub content_type: Option<String>,
    pub body: Option<String>,
    pub body_truncated: bool,
    pub timing_ms: Option<u64>,
}

pub struct NetworkRequestsResult {
    pub entries: Vec<NetworkEntry>,
    pub truncated: bool,
}
```

The `StubBrowser` in `ports.rs`'s test module and `FakeBrowserDriver` in
`tools/browser.rs`'s test module must both grow `network_requests` implementations.

## Tool layer

Following the `browser_screenshot` pattern (`crates/core/src/tools/browser.rs`):
- `browser_network_requests(browser, fs, input)` — takes `FileStore` to support
  `save_har` path writing
- Maps `BrowserNetworkRequestsInput` → port call → `BrowserNetworkRequestsOutput`
- HAR assembly happens in the tool layer (not the port), using the entries returned
  by the port — pure JSON serialization, no adapter-specific code

## HAR 1.2 structure

A spec-compliant HAR 1.2 document for the port's `NetworkRequestsResult`:
```json
{
  "log": {
    "version": "1.2",
    "creator": { "name": "stapler-mcp", "version": "0.1" },
    "entries": [
      {
        "startedDateTime": "2026-10-10T00:00:00.000Z",
        "time": 42,
        "request": { "method": "GET", "url": "...", "headers": [], "queryString": [], "cookies": [], "headersSize": -1, "bodySize": -1 },
        "response": { "status": 200, "statusText": "OK", "headers": [], "content": { "size": -1, "mimeType": "application/json", "text": "..." }, "redirectURL": "", "headersSize": -1, "bodySize": -1 },
        "cache": {},
        "timings": { "send": 0, "wait": 42, "receive": 0 }
      }
    ]
  }
}
```

The port's `NetworkEntry` doesn't carry full headers (that would be too verbose for LLM
consumption) — HAR headers fields are emitted as empty arrays for compliance.

## Attach point summary

| Layer | Change |
|---|---|
| `crates/core/src/ports.rs` | Add `NetworkEntry`, `NetworkRequestsResult` types; add `network_requests` to `BrowserDriver` trait; add `network_requests` to `StubBrowser` test double |
| `crates/core/src/schema.rs` | Add `BrowserNetworkEntry`, `BrowserNetworkRequestsInput`, `BrowserNetworkRequestsOutput` schema types |
| `crates/core/src/tools/browser.rs` | Add `browser_network_requests` tool fn; add `network_requests` to `FakeBrowserDriver`; add unit tests |
| `crates/native/src/browser.rs` | Add `network_entries: Vec<NetworkEntry>` to `BrowserSession`; enable CDP Network domain on session creation; wire `EventRequestWillBeSent`/`EventResponseReceived`/`EventLoadingFinished` handlers; implement `BrowserDriver::network_requests` for `NativeBrowser` |
| `crates/wasm/src/glue/` | Add `networkEntries` accumulation in JS event handlers; add JS-side `network_requests` function; wire wasm-bindgen call |
| `crates/cli/src/main.rs` | Register `stapler_browser_network_requests` tool (or gate if tool-profile mechanism exists) |
| `crates/wasm/src/lib.rs` | Register `stapler_browser_network_requests` on wasm daemon |
