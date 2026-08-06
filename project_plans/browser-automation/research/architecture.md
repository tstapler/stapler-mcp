# Architecture Research: browser-automation (session-scoped browser ops)

## 1. Current architecture (as read)

### Port trait idiom — `crates/core/src/ports.rs`
- Every port is a plain trait using **native `async fn` in traits**, deliberately not `#[async_trait]` / `Box<dyn>` (see file header comment, lines 1-9). Every caller is generic over the concrete port type: `fn foo<D: BrowserDriver>(...)`.
- `BrowserDriver` today (lines 118-128) has exactly one method:
  ```rust
  async fn navigate_and_extract(&self, url: &str, timeout: Duration) -> Result<PageExtract, PortError>;
  ```
  `&self`, not `&mut self` — the trait was designed for a stateless, one-shot call.
- `PortError` is a flat 3-variant enum (`Io`, `Timeout`, `Other`) — no session-not-found / session-expired variant exists yet.
- Precedent for a stateful port living behind a simple `RefCell`-guarded struct: `docs.rs`'s `SourceLocks` (`crates/core/src/tools/docs.rs:303`) — `Rc`-shared, `RefCell<HashSet<SourceId>>` internally, RAII guard (`SourceLockGuard`) releasing on every exit path via `Drop`. This is the closest existing analog to "keyed session state shared across concurrent tool calls" and should be the template, not `tokio::sync::Mutex`.

### Native adapter — `crates/native/src/browser.rs` (94 lines, read in full)
- `NativeBrowser` holds one `chromiumoxide::Browser` for the daemon's entire lifetime, built via `Browser::launch`, plus a detached `tokio::spawn` task draining the CDP event stream forever (line 27).
- **No mutex anywhere.** The doc comment (lines 8-11) explains why: `Page` methods take `&self`, and today's design creates a **fresh `Page` per call** (`browser.new_page(url)` inside `navigate_and_extract`), so concurrent calls just get independent tabs — no shared mutable state to protect.
- `close()` (lines 33-41) has an explicit, unusual contract: "must be called once, explicitly, at daemon shutdown, after every `Rc<NativeBrowser>` clone held by registered tool handlers has been dropped" — enforced in `crates/cli/src/main.rs:245-253` via `Rc::get_mut` after `drop(daemon)`.
- This whole design assumes **zero persistent per-call state**. Introducing session-scoped `Page`s breaks the "no mutex needed" invariant directly.

### Wasm adapter — `crates/wasm/src/browser.rs` (52 lines, read in full)
- `WasmBrowser` is a **zero-field unit struct**. It calls into JS glue (`/src/glue/browser.js`) via `wasm_bindgen` extern fns `jsNavigateAndExtract` / `jsCloseBrowser`, awaited through `JsFuture`. All actual browser/session state (if any) lives in the JS host, not in Rust.
- For session support, this adapter has **no place to hold a `session_id -> Page` map on the Rust side at all** — session state must live in the JS glue module (presumably backed by playwright-core's own `Browser`/`Page`/`BrowserContext` objects), and the Rust struct only ever proxies `session_id` strings across the wasm boundary. This is a meaningfully different shape than the native adapter.

### `fetch_page` tool — `crates/core/src/tools/fetch.rs` (48 lines, full file)
- `pub async fn fetch_page<B: BrowserDriver, F: FileStore>(browser: &B, fs: &F, input: FetchPageInput) -> Result<FetchPageOutput, String>` — the generic-dispatch idiom in full: a free function generic over port traits, taking `&B` (shared reference to the port, not `&mut`), doing the `Duration`/timeout logic itself and calling exactly one port method.
- Wired up identically on both backends:
  - Native: `crates/cli/src/main.rs:81-108` — `let mut browser = Rc::new(NativeBrowser::launch().await?)`; each `daemon.register("fetch_page", json_handler(...))` closure clones the `Rc` and calls `fetch::fetch_page(&*browser, &*fs, input)`.
  - Wasm: `crates/wasm/src/lib.rs:45-59` — `let browser = Rc::new(browser::WasmBrowser)`; same clone-into-closure pattern, same call to `fetch::fetch_page`.
- Both bootstraps are **single global `Rc<Browser...>` shared across every registered handler closure** — this is the seam any session map must attach to (constructed once at daemon start, cloned into every closure that needs it, including a new reaper task).

### Daemon runtime and dispatch — `crates/cli/src/main.rs:20-38`, `crates/core/src/daemon.rs`
- **This is the single most important fact for the concurrency design.** The daemon runs on a `tokio::runtime::Builder::new_current_thread()` + `tokio::task::LocalSet` (`crates/cli/src/main.rs:23-31`), explicitly chosen so "no `Send` bounds anywhere" — which is *also* why `Rc` (not `Arc`) is used throughout the whole codebase for shared port instances, and why `SourceLocks` uses `RefCell` (not `tokio::sync::Mutex` or `std::sync::Mutex`).
- `Daemon::run` (`crates/core/src/daemon.rs:80-98`) is a **strictly sequential accept loop**: `accept().await` → read one frame → `handle_request_bytes().await` → write one frame → loop. There is **no `spawn`/`spawn_local` per connection**. This means tool calls are **never concurrent with each other** from the daemon's own request-handling perspective — only one client tool call is in flight at any instant today.
- The only pre-existing concurrent-with-the-main-loop task is the browser's own CDP-event-drain task (`tokio::spawn` in `browser.rs:27`) — a fire-and-forget background task with no shared mutable state, plus (once introduced) any reaper task. `tokio::spawn` (not `spawn_local`) is used there today only because that particular future happens to be `Send`-compatible incidentally; a `Rc`-holding reaper task will need `tokio::task::spawn_local` instead, which requires running inside the `LocalSet` (already the case, since `run_daemon` is `local.block_on(&rt, run_daemon())`).

### No other existing background-task / reaper / session-map pattern
- Grepped the whole `crates/` tree: **zero** uses of `Mutex`/`RwLock` in production code (`crates/cli/examples/relevance_spot_check.rs`'s hit is a search-query string, not real usage). **Zero** existing timeout/reaper background tasks. The only `tokio::spawn` call in production code is the CDP-drain loop in `browser.rs:27`.
- So there is no precedent to reuse for "background task auto-expiring keyed state" other than `SourceLocks`'s `RefCell`-guarded-map idiom, which is synchronous-guard-per-key, not time-based expiry — the reaper is new ground for this codebase.

## 2. Integration design

### Session map: `Rc<RefCell<HashMap<SessionId, SessionEntry>>>`, not `Arc<Mutex<...>>`
Given the current-thread + `Rc`/`RefCell` idiom used everywhere else (`SourceLocks`, `NativeBrowser` itself held as `Rc<NativeBrowser>`), the session map should follow the same shape:

```rust
// crates/native/src/browser.rs
pub struct NativeBrowser {
    browser: Browser,
    sessions: Rc<RefCell<HashMap<String, SessionEntry>>>,
}
struct SessionEntry {
    page: chromiumoxide::Page,
    last_used: Instant, // or a ClockPort-driven millis value, to keep this OS-detached like the rest of the port design
}
```
- `Rc<RefCell<...>>` is safe here specifically **because the daemon's request-handling loop is sequential** (finding above) — no two tool-call handlers ever run truly concurrently, only cooperatively interleaved with the reaper task at `.await` points. The one discipline this requires (as `SourceLocks` also had to observe): **never hold a `RefCell` borrow across an `.await`** — look up/insert/remove the entry, clone out the `chromiumoxide::Page` handle (itself cheaply `Clone`, being a CDP session handle) or take ownership before awaiting any CDP round trip, then re-borrow only for short synchronous map mutations (insert/touch-timestamp/remove).
- Do **not** introduce `tokio::sync::Mutex`/`std::sync::Mutex` — that would be the first `Send`-oriented primitive in a codebase whose entire raison d'être for `current_thread`+`Rc` (per the `main.rs` comment) is to let the *same* core also satisfy a `!Send` wasm-bindgen adapter. Introducing `Arc<Mutex<_>>` in the native adapter only would be a real idiom break flagged in review.
- `SessionEntry` needs a `Clock`-driven timestamp for idle-timeout math; the codebase already threads a `ClockPort`/`NativeClock` for exactly this "no raw `Instant`/`SystemTime` in `crates/core`" reason (see `ports.rs:91-93`, `ClockPort`). The reaper and `navigate`/`click`/`type`/`snapshot` handlers should touch `last_used` via the same injected `ClockPort` used elsewhere (`main.rs:91`, `Rc::new(NativeClock)`), not `Instant::now()` directly, to keep `crates/core` free of direct OS-time calls per the port-boundary rule stated at the top of `ports.rs`.

### Where the reaper lives
- Spawn it once in `run_daemon()` (`crates/cli/src/main.rs`), right after `NativeBrowser::launch()`, as `tokio::task::spawn_local(reaper_loop(sessions.clone(), clock.clone()))` — it must be `spawn_local` (not `tokio::spawn`) because it holds an `Rc`, and `spawn_local` is only legal inside the already-established `LocalSet` (`main.rs:31`, `local.block_on(&rt, run_daemon())`), so no runtime change is needed.
- It should get a **clone of the exact same `Rc<RefCell<HashMap<...>>>`** that `NativeBrowser` holds — cleanest is to construct the session map once in `main.rs` (or inside `NativeBrowser::launch`, exposing a `sessions()` accessor) and hand a clone both into `NativeBrowser` (for the port methods) and to the spawned reaper closure, mirroring exactly how `browser`, `fs`, `embedder`, `source_locks` are each constructed once and `.clone()`-captured into every `daemon.register(...)` closure today (`main.rs:81-240`).
- Shutdown: the reaper's `JoinHandle` should be held (not detached like the CDP-drain task) so it can be aborted (`handle.abort()`) right where `main.rs:250` already does `drop(daemon)` before `NativeBrowser::close()` — otherwise an idle-timeout tick firing after `browser.close()` would try to close pages on an already-closed `Browser`, a new failure mode this feature introduces that the existing "must be called once, explicitly, at daemon shutdown" contract doesn't yet have to guard against.
- On the **wasm side**, there is no Rust-side session map to reap — `WasmBrowser` is a zero-field struct proxying to JS. Either (a) implement the reaper in the JS glue module (`/src/glue/browser.js`) using `setInterval`/playwright-core's own context lifecycle, or (b) skip idle-timeout on wasm for v1 and document the asymmetry (there is precedent for a deliberate native/wasm asymmetry already — see `Embedder`'s doc comment in `ports.rs:139-148` for how this codebase documents such gaps rather than silently omitting them).

### Extending `BrowserDriver` without breaking the generic-dispatch idiom
The existing `fn fetch_page<B: BrowserDriver, F: FileStore>(browser: &B, ...)` pattern (free function, `&B`, generic over the port trait) extends directly — new tool functions follow the identical shape:

```rust
pub trait BrowserDriver {
    async fn navigate_and_extract(&self, url: &str, timeout: Duration) -> Result<PageExtract, PortError>; // existing, unchanged
    async fn navigate(&self, url: &str, timeout: Duration) -> Result<NavigateResult, PortError>; // returns session_id + initial snapshot/extract
    async fn click(&self, session_id: &str, locator: &Locator, timeout: Duration) -> Result<(), PortError>;
    async fn type_text(&self, session_id: &str, locator: &Locator, text: &str, timeout: Duration) -> Result<(), PortError>;
    async fn snapshot(&self, session_id: &str, timeout: Duration) -> Result<AxSnapshot, PortError>;
}
```
- `&self` (not `&mut self`) is preserved because the mutability lives inside the `RefCell`, not in the trait signature — this matches how `SourceLocks::try_acquire` also takes `&self` despite mutating a `RefCell` internally. No generic-dispatch code needs to change from `&B` to `&mut B`.
- Each new tool (`browser_navigate`, `browser_click`, `browser_type`, `browser_snapshot`) gets its own file/function in `crates/core/src/tools/` (new `browser.rs`, sibling to `fetch.rs`), each `pub async fn foo<B: BrowserDriver>(browser: &B, input: FooInput) -> Result<FooOutput, String>`, each registered in both `crates/cli/src/main.rs` and `crates/wasm/src/lib.rs` with the identical `Rc::clone` + `daemon.register(name, json_handler(...))` boilerplate already used for `fetch_page`.
- `PortError` likely needs a new variant (`SessionNotFound` or reuse `Other(String)` for v1) for "click/type/snapshot called with an unknown or reaped session_id" — a real new error path this feature introduces that `fetch_page`'s error surface never had to handle.

### Session ID generation and threading
- `session_id` is a plain `String` (uuid or similar) generated by the adapter inside `navigate` (not by `crates/core`, to keep ID-generation an OS/adapter concern — consistent with `crates/core` never touching randomness/OS directly per the `ports.rs` header comment). It flows: adapter generates → returned in `NavigateResult` → surfaces in `schema.rs`'s `BrowserNavigateOutput.session_id` → client passes it back verbatim in `BrowserClickInput.session_id` / `BrowserTypeInput.session_id` / `BrowserSnapshotInput.session_id` → tool function passes `&input.session_id` straight through to the `BrowserDriver` port method → native adapter looks it up in the `RefCell<HashMap<...>>`; wasm adapter passes the string across the `wasm_bindgen` boundary for the JS glue to look up in its own map.

## 3. Data flow trace: `navigate` → `click` → `snapshot`

1. **Client → daemon socket**: MCP tool call `browser_navigate {url}` arrives over the Unix socket, one connection/one frame per the sequential `Daemon::run` accept loop (`daemon.rs:80-98`).
2. **`handle_request_bytes`** (`daemon.rs:42`) deserializes into the registered handler for `"browser_navigate"`, itself a `json_handler(...)`-wrapped closure created in `main.rs` capturing `Rc::clone(&browser)`.
3. Closure calls `tools::browser::navigate(&*browser, input)` (new function, `fetch_page`'s shape) → calls `browser.navigate(&input.url, timeout)` on the `BrowserDriver` trait object (statically dispatched, generic `B`).
4. **Native adapter**: `NativeBrowser::navigate` calls `self.browser.new_page(url)` (as `navigate_and_extract` does today), generates a new `session_id`, inserts `{page, last_used: now}` into `self.sessions.borrow_mut()`, drops the borrow, returns `NavigateResult{session_id, extract}` — no borrow held across the CDP `.await` calls for title/html/text extraction.
5. Response serialized, `session_id` flows back to the client as part of `BrowserNavigateOutput`.
6. **Client → daemon**, second connection: `browser_click {session_id, role, name}`. Same dispatch path to `tools::browser::click(&*browser, input)` → `browser.click(&input.session_id, &locator, timeout)`.
7. **Native adapter**: `self.sessions.borrow()` looks up `session_id`; if missing (reaped or bogus), returns `PortError::Other("session not found")` / a new `SessionNotFound` variant immediately — no CDP call attempted. If found, clone the `chromiumoxide::Page` handle out of the borrow, drop the borrow, then run the CDP `Accessibility.getFullAXTree` (or targeted query) to resolve role+name to a backend node id, issue `Input.dispatchMouseEvent` (or chromiumoxide's higher-level click helper) against that node, `borrow_mut()` again only to bump `last_used`.
8. **Client → daemon**, third connection: `browser_snapshot {session_id}` → same lookup path → walk the CDP Accessibility domain tree into `AxSnapshot`, return it, bump `last_used`.
9. **Concurrently, out-of-band**: the `spawn_local` reaper wakes on a timer (e.g. every 30s), `self.sessions.borrow_mut()`, iterates entries, for any `now - last_used > idle_timeout` removes the entry and (after dropping the borrow) calls `page.close().await` on the removed `Page`. Because it only ever removes-then-closes (never operates on an entry `click`/`snapshot` currently holds a cloned handle to), a session actively mid-use when the reaper ticks is not the *same* `Page` value being closed out from under an in-flight call — but if `click`'s CDP round-trip is slow enough that the reaper's idle-timeout should have already fired at the time `click` started, the design must make idle-timeout comparisons happen only when no handler currently holds that session's lookup builder mid-call; the safest fix is bumping `last_used` **before** issuing the CDP call in step 7/8 (optimistically extending the deadline for the duration of the call) rather than after, since "after" leaves a window where a slow call's session looks idle to the reaper for its whole duration.

## 4. EventStorming / Event-Command-Policy table
Not applicable — per the requirements this is a stateful technical integration (session map + reaper), not a multi-actor business-rules domain. Skipped as instructed.

## Key open risk carried into planning
- Native AX-tree role/name resolution has no existing precedent in this codebase (chromiumoxide gives raw CDP `Accessibility.getFullAXTree`/`getPartialAXTree` nodes, no role/name-to-locator matching helper) — this is real net-new logic, not a wiring exercise, and should get its own design-phase attention rather than being treated as "just another BrowserDriver method."
