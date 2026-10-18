# Implementation Plan: browser-network-capture

**Source**: `project_plans/browser-network-capture/requirements.md`,
`research/architecture.md`, `research/pitfalls.md`

## Step 1 — System Type

**Stateful port extension inside the existing hexagonal-architecture daemon** — same pattern
as the browser-automation feature. Adds:
- One new method on `BrowserDriver` trait + two new port types
- Per-session accumulation storage in `NativeBrowser`'s `BrowserSession` struct
- One new tool function in `crates/core/src/tools/browser.rs`
- CDP `Network` domain subscription in native adapter (existing `chromiumoxide` pattern)
- JS glue changes in wasm adapter
- Tool registration in both `crates/cli/src/main.rs` and `crates/wasm/src/lib.rs`

## Step 2 — Domain Glossary

| Term | Definition |
|---|---|
| `NetworkEntry` | One captured network transaction: URL, method, status, content-type, body (optionally truncated), timing. |
| `NetworkRequestsResult` | Return type of `BrowserDriver::network_requests`: `entries: Vec<NetworkEntry>`, `truncated: bool`. |
| `network_entries` | Per-session `Vec<NetworkEntry>` in `BrowserSession`, bounded by `MAX_NETWORK_ENTRIES`. Cleared on each `navigate` call. |
| `MAX_NETWORK_ENTRIES` | Hard cap per session: 1000 entries. |
| `MAX_BODY_BYTES` | Default per-body cap: 16 384 bytes. Callers can lower via `max_body_bytes` input param; they cannot exceed the hard cap. |
| `filter_url` | Optional substring filter applied to `entry.url` before returning — reduces response size. |
| `save_har` | Optional file path: if set, the tool writes a HAR 1.2 JSON document to that path via `FileStore`. |
| `clear` | Optional bool on input: if true, clears the session's entry list and returns empty (or the entries just before clearing, TBD). |
| HAR 1.2 | HTTP Archive format — JSON document with `log.entries` array, used by browser devtools and analysis tools. |

## Step 3 — Pattern Decisions

### Body capture: eager vs. lazy

**Chosen: eager** — bodies are retrieved in the `EventLoadingFinished` handler (CDP) /
`response.on('finished')` (playwright) at the moment they are available, not on-demand
when the caller calls `network_requests`. Rationale: Chrome evicts response bodies from
its in-memory cache independently of the CDP session; a deferred call may find the body
gone (see `research/pitfalls.md` P1). Eager capture with a per-body cap avoids this.

### Binary response bodies

Skip body capture entirely for non-text content types (`image/*`, `font/*`, `audio/*`,
`video/*`, `application/octet-stream`). Store `body: None` and `body_truncated: false`.
Rationale: these bodies are useless to an LLM agent and would consume most of the memory
budget.

### `filter_url` scope

Filter is applied server-side (in the tool layer) after the port call returns the full
list, not at accumulation time. Rationale: keeping accumulation unconditional means the
same entries are available regardless of which filter the caller uses; filtering at query
time is O(n) over a bounded list and has negligible cost.

### `clear` semantics

`clear: true` clears the list *and* returns the entries that were in it (so a caller can
retrieve and clear in one call). `clear: false` (default) returns without modifying the list.

### HAR timestamp

`NetworkEntry` stores a `started_at_ms: u64` (ms since Unix epoch) alongside `timing_ms`.
The HAR exporter uses `started_at_ms` to populate `entry.startedDateTime` in ISO 8601 format.

## Epic 1 — Port layer

### Story 1.1: Add `NetworkEntry` and `NetworkRequestsResult` types to `ports.rs`

**AC 1.1.1**: `NetworkEntry` has fields `url: String`, `method: String`,
`status: Option<u16>`, `content_type: Option<String>`, `body: Option<String>`,
`body_truncated: bool`, `timing_ms: Option<u64>`, `started_at_ms: u64`.

**AC 1.1.2**: `NetworkRequestsResult` has fields `entries: Vec<NetworkEntry>`,
`truncated: bool`.

**AC 1.1.3**: Both types derive `Debug`, `Clone`, `PartialEq`.

### Story 1.2: Add `network_requests` to `BrowserDriver`

**AC 1.2.1**: Signature:
```rust
async fn network_requests(
    &self,
    session_id: &SessionId,
    filter_url: Option<&str>,
    max_body_bytes: Option<usize>,
    clear: bool,
) -> Result<NetworkRequestsResult, PortError>;
```
(No `timeout` parameter — this is an in-memory read, no network I/O.)

**AC 1.2.2**: `StubBrowser` in `ports.rs` tests compiles with a `todo!()` body.

## Epic 2 — Native adapter

### Story 2.1: Enable CDP Network domain on session creation

**AC 2.1.1**: `NativeBrowser::open_session` calls `page.enable_network()` before the
first `goto`.

**AC 2.1.2**: If `enable_network()` fails, the session creation fails with
`PortError::Other` (not silently swallowed).

### Story 2.2: Add `network_entries` to `BrowserSession`

**AC 2.2.1**: `BrowserSession` has a `network_entries: Vec<NetworkEntry>` field
(empty on creation).

**AC 2.2.2**: `BrowserSession` has a `network_truncated: bool` field (false on creation).

### Story 2.3: Subscribe to CDP Network events

**AC 2.3.1**: On session creation, subscribe to `EventRequestWillBeSent` to record
`url`, `method`, `started_at_ms`.

**AC 2.3.2**: Subscribe to `EventResponseReceived` to record `status`, `content_type`
for the matching request (matched by `request_id`).

**AC 2.3.3**: Subscribe to `EventLoadingFinished`. If the accumulated entry count is
below `MAX_NETWORK_ENTRIES`, call `Network.getResponseBody` for text/JSON content types;
store result as `body` (truncated at `MAX_BODY_BYTES`). For binary content types, store
`body: None`. Increment entry count.

**AC 2.3.4**: When `network_entries.len() >= MAX_NETWORK_ENTRIES`, set
`network_truncated = true` and skip further body retrieval (but still record the entry
with `body: None`).

### Story 2.4: Clear `network_entries` on `navigate`

**AC 2.4.1**: `NativeBrowser::navigate` clears `network_entries` and resets
`network_truncated = false` for the session before issuing `page.goto`.

### Story 2.5: Implement `BrowserDriver::network_requests` for `NativeBrowser`

**AC 2.5.1**: Returns `PortError::NotFound` if `session_id` has no live entry.

**AC 2.5.2**: Returns `PortError::NotFound` if the session's `blocked = true` (see
`research/pitfalls.md` P6).

**AC 2.5.3**: Applies `filter_url` substring filter to the entry list if given.

**AC 2.5.4**: Truncates each entry's `body` to `max_body_bytes` if given (must be ≤
`MAX_BODY_BYTES`).

**AC 2.5.5**: If `clear = true`, clears `network_entries` and resets `network_truncated`
after copying the list to return.

**AC 2.5.6**: Returns entries with `truncated` reflecting the session-level truncation
flag (not the per-body flag).

## Epic 3 — Tool layer

### Story 3.1: Add schema types

**AC 3.1.1**: `BrowserNetworkEntry` schema struct with camelCase serde fields:
`url`, `method`, `status` (Option), `contentType` (Option), `body` (Option),
`bodyTruncated`, `timingMs` (Option).

**AC 3.1.2**: `BrowserNetworkRequestsInput`: `sessionId: String`,
`filterUrl: Option<String>`, `maxBodyBytes: Option<u32>`, `clear: Option<bool>`,
`savePath: Option<String>`.

**AC 3.1.3**: `BrowserNetworkRequestsOutput`: `entries: Vec<BrowserNetworkEntry>`,
`truncated: bool`, `savedTo: Option<String>`.

### Story 3.2: Implement `browser_network_requests` tool function

**AC 3.2.1**: Validates `session_id` non-empty.

**AC 3.2.2**: Calls `browser.network_requests(...)`, maps `PortError::NotFound` via the
`not-found` passthrough convention (same as `browser_snapshot`).

**AC 3.2.3**: If `save_path` given, assembles HAR 1.2 JSON from entries, writes via `fs`,
sets `saved_to`.

**AC 3.2.4**: Unit tests:
- empty session_id returns error
- filter_url narrows returned entries
- clear: true returns entries then removes them (tested via fake with pre-seeded entries)
- save_path triggers HAR write to fake FileStore

### Story 3.3: Register the tool in both daemons

**AC 3.3.1**: `stapler_browser_network_requests` registered in `crates/cli/src/main.rs`.

**AC 3.3.2**: `stapler_browser_network_requests` registered in `crates/wasm/src/lib.rs`.

## Epic 4 — wasm adapter

### Story 4.1: JS glue network event accumulation

**AC 4.1.1**: JS glue adds a `networkEntries` array per session.

**AC 4.1.2**: Subscribes to `page.on('request')` and `page.on('response')` to accumulate
entries; response handler calls `await response.body()` in a try/catch, converts to UTF-8
string; skips body for binary content types.

**AC 4.1.3**: On `page.goto()`, clears the session's `networkEntries`.

**AC 4.1.4**: Implements `network_requests(sessionId, filterUrl, maxBodyBytes, clear)` JS
function called from Rust wasm glue.

## Epic 5 — Integration test

### Story 5.1: Native integration test

**AC 5.1.1**: `#[ignore]`d test `browser_network_requests_should_capture_xhr_when_session_makes_xhr_call`:
- Launches real daemon
- Navigates to a local test HTML page that makes an XHR call to a local server returning
  JSON (same pattern as existing `docs_index_round_trip` integration test)
- Calls `stapler_browser_network_requests` with `filter_url = "/api/data"`
- Asserts: entry with status 200 and JSON body containing expected content is present

## As built (deltas from the plan above)

- Native state lives in `crates/native/src/network.rs` (`NetworkLog`): in-flight requests are
  correlated by `request_id` in a private `partial` map, separate from completed `entries`;
  `BrowserSession.network` is an `Rc<RefCell<NetworkLog>>` shared with four listener tasks
  (`requestWillBeSent`, `responseReceived`, `loadingFinished`, `loadingFailed`), each borrowing
  only synchronously. Resolves adversarial challenge 1.
- `clear: true` is copy-then-clear inside one `NetworkLog::query` call (no `.await`). Challenge 5.
- HAR export uses `-1` for `headersSize`/`bodySize`; headers/cookies arrays are empty. Challenge 3.
- Registered unconditionally in both daemons; candidate for a future `network` tool profile.
  Challenge 4.
- Redirect hops (repeated `request_id`) become separate entries; `max_body_bytes` is clamped to
  the 16 384-byte capture cap.
- Open question 1 (single vs. multiple event tasks): `page.event_listener` returns one stream
  per event type, so four tasks are used.

- `filter_url`, the body cap and the clear run inside each adapter (not the tool layer), so
  `clear` stays atomic with the copy. At the 1000-entry cap further entries are dropped (matches
  requirement 8), not recorded body-less as AC 2.3.4 said.
- Only the session's first tab is observed (native and wasm).
- With `savePath` + `clear`, the tool reads without clearing, writes the HAR, then clears, so a
  failed write loses nothing.
- A body is fetched only when the content type is known and non-binary (same rule in both
  adapters); native bodies are fetched per `loadingFinished` event with a 3 s timeout, wasm with
  a 3 s race.
- `loadingFinished` yields (bounded) until its `responseReceived` is recorded, because the
  `Network.*` streams are drained by separate tasks.

## Ordering / Dependencies

```
Epic 1 (port types) → Epic 2 (native adapter) → Epic 5 (integration test)
Epic 1 (port types) → Epic 3 (tool layer) [can start once port types defined]
Epic 1 (port types) → Epic 4 (wasm adapter) [can start once port types defined]
Epic 3 and Epic 4 can be parallelized after Epic 1.
```
