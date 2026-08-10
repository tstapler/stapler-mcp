# Requirements: browser-automation

**Date**: 2026-08-06
**Type**: feature addition
**Complexity**: 4 — high-stakes / cross-cutting (new port surface across two adapters, session state, background reaper)

## Problem Statement
`stapler-mcp`'s stated direction is to replace always-running third-party Node MCP servers with a native, shared-daemon Rust equivalent. `fetch_page` covers one-shot page rendering, but there is no interactive browser automation — navigate, click, type, and accessibility-tree snapshot against a *persistent* session — the `playwright-mcp` equivalent. Without it, any task needing multi-step browser interaction (log in, fill a form, click through a flow, inspect the resulting DOM) still requires the separate `playwright-mcp` Node server, defeating the single-daemon goal.

## Baseline
Today, an LLM caller that needs multi-step browser interaction must use the external `playwright-mcp` Node MCP server (a second always-running process, outside this project's daemon). `stapler-mcp`'s own `BrowserDriver` port and `NativeBrowser`/wasm adapters only support `navigate_and_extract` — a coarse, one-shot "load URL, return title/html/text" call with no session state (`crates/core/src/ports.rs:118-128`, `crates/native/src/browser.rs`). There is no way to click an element, type into a field, or take an accessibility snapshot, let alone do so across a sequence of calls against the same page.

## Users / Consumers
LLM agents (via MCP tool calls, same as every other tool in this project) that need to drive a real browser through a multi-step interaction — e.g. filling and submitting a form, clicking through a UI flow, or inspecting page structure via an accessibility-tree snapshot — where a single one-shot `fetch_page` call is insufficient.

## Success Metrics
- New tools `stapler_browser_navigate`, `stapler_browser_click`, `stapler_browser_type`, `stapler_browser_snapshot` exist, are registered on both the native and wasm daemons, and are reachable through the real Unix-socket protocol (mirroring `docs_index_round_trip`'s and `webcrawl.rs`'s real-daemon integration test pattern).
- A session created by `navigate` can be reused by subsequent `click`/`type`/`snapshot` calls via a returned `session_id`, proving state persists across calls (unlike `fetch_page`'s fresh-tab-per-call model) — verified by a **native-adapter** integration test that navigates, clicks, types, and snapshots in sequence against a single session. The equivalent full round-trip test is explicitly *not* required for the wasm adapter in this pass (wasm gets a single-call smoke test instead) — per the wasm/native behavioral-parity-testing rabbit hole below, standing up a Node/`playwright-core` test harness capable of running an equivalent multi-step flow is materially more infrastructure than this pass's appetite covers; scoping the full round-trip requirement to native only is the honest statement of what's actually verified, rather than an untasked aspiration.
- An idle session is reaped (its underlying page/tab closed and resources freed) after a bounded idle timeout without requiring an explicit close call — verified by a test that waits past the timeout and confirms the session is gone.
- Native adapter's snapshot returns a role/name-labeled accessibility tree (not just raw CDP AX node dumps) for a real test page with distinguishable interactive elements (a button, a text input) sufficient to build a click/type locator from it.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` (or equivalent) both pass with no new warnings.

## Appetite
Large (3–6 weeks) — matches the issue's own Shape Up sizing ("a large bet... weeks, not a small batch"). User has explicitly selected full implementation as the scope for this pass (not a small first slice, not plan-only).

## Constraints
- Solo project — no team-size constraint, but no dedicated QA pass beyond the automated test suite and manual spot checks.
- No hard external deadline.
- Must preserve the existing `fetch_page` tool and its one-shot semantics unchanged — this is additive, not a replacement.
- Native and wasm adapters must both implement the extended `BrowserDriver` port surface (existing project convention: every port has at least a partial wasm adapter, per `ports.rs:139-145`'s note on `Embedder` being the sole deliberate exception).

## Non-functional Requirements
- **Performance SLO**: not specified — browser automation is inherently interactive/human-latency-bound (page loads, DOM settling); no p99 target, but individual calls (click/type/snapshot against an already-navigated page) should not add unnecessary latency beyond the underlying CDP/playwright-core round trip.
- **Scalability**: expected to be a handful of concurrent sessions per daemon (single local user, interactive use) — not a multi-tenant or high-concurrency design target.
- **Security classification**: internal/local-only, same trust model as `fetch_page` (the daemon already fetches arbitrary URLs on behalf of the local user; the existing SSRF guard precedent from `a15ed28` should be considered for navigate's URL, consistent with `fetch_page`'s and the crawler's guard).
- **Data residency**: not applicable (local process, no persisted user data beyond transient in-memory session state).

## Scope
### In Scope
- Extend the `BrowserDriver` port with session-scoped operations: `navigate` (returns a `session_id` plus the initial extract/snapshot), `click` (by locator — resolved during planning to an opaque `ref` id scoped to the most recent `AxSnapshot`, matching `playwright-mcp`'s model, **not** an exposed role+name pair; see `implementation/plan.md`'s "Locator format" decision in Step 3 for the full rationale), `type` (into a focused/located element), `snapshot` (accessibility tree for the current page state in the session).
- Native adapter (`chromiumoxide`): persistent `Page` per session (not fresh-tab-per-call like `navigate_and_extract`), keyed by `session_id`; hand-rolled AX-tree walking over CDP's `Accessibility` domain to produce role/name-labeled nodes usable as click/type locators.
- Wasm adapter: wire the same four operations to `playwright-core`'s existing session/page and accessibility-snapshot APIs (already available in the JS glue per the issue).
- An idle-timeout reaper: sessions unused for longer than a fixed timeout are closed and their resources freed automatically, without requiring an explicit "close session" call from the client.
- New MCP tool schemas (input/output structs, `schemars`/serde camelCase, following `FetchPageInput`/`FetchPageOutput`'s existing pattern) and daemon registration on both native and wasm.
- Fast, offline unit tests (fakes/mocks, following `InMemoryFileStore`/`FakeHttpClient` conventions) plus one `#[ignore]`d real-daemon integration test mirroring `docs_index_round_trip`.

### Out of Scope
- Explicit session-close/teardown tool (relying on the idle-timeout reaper for v1; an explicit `close_session` tool is a plausible fast-follow, not required now).
- Multi-tab / multi-page-per-session support (one page per session, matching the issue's stated scope).
- File upload, drag-and-drop, dialog handling, or other advanced Playwright interaction primitives beyond navigate/click/type/snapshot.
- Download handling or network interception.
- Any UI/visual screenshot capability (accessibility-tree snapshot only, not pixel screenshots).
- Decommissioning or replacing the external `playwright-mcp` Node server (that's a separate, manual post-implementation step, same precedent as the `docs-mcp-server` coexistence note in the docs-index pre-mortem).

## Rabbit Holes
- **Native AX-tree role/name fidelity**: `chromiumoxide` only exposes the raw CDP `Accessibility` domain; there's no guarantee the hand-rolled walk produces locators as reliable as `playwright-core`'s battle-tested resolution. Budget explicit time for this; don't assume parity between native and wasm adapters (per the issue's own warning).
- **Session lifecycle correctness under concurrency**: the existing `NativeBrowser` shares one `Browser` across concurrent calls with no mutex (relying on independent fresh pages). Persistent per-session pages plus a background reaper introduce genuine concurrent-access and time-of-check-to-time-of-use hazards (e.g. reaper closing a page mid-use) that need a real design pass, not an afterthought. During planning this rabbit hole surfaced a further, necessary consequence not anticipated here: a persistent `Page`'s underlying renderer can crash independently of the driving process (`Target.targetCrashed`), so tab/renderer-crash detection (`PortError::SessionCrashed`, `implementation/plan.md` Epic 2 Story 2.5) was added to the plan as a required part of session lifecycle correctness, not as silent scope drift.
- **Locator matching semantics**: deciding exactly how a `click`/`type` locator (role+name string? CSS selector? both?) maps onto the accessibility tree returned by `snapshot` is a design decision with real depth — get this wrong and the tools are unusable in practice even though they "work."
- **wasm/native behavioral parity testing**: because the wasm adapter's tests likely can't run in the same CI/dev environment as native (Node + playwright-core availability), verifying both adapters actually implement the same contract may take more infrastructure than expected.

## Alternatives Considered
- **Keep using the external `playwright-mcp` Node server** (status quo) — rejected: defeats the project's core goal of a single native daemon with no separate Node process.
- **Small first slice** (e.g. navigate + snapshot only, no click/type, no reaper) — considered via `AskUserQuestion`; user explicitly chose full implementation instead.
- **Design/plan only, defer implementation** — considered via `AskUserQuestion`; user explicitly chose full implementation instead.

## Feasibility Risks
- `chromiumoxide`'s CDP `Accessibility` domain API surface may be less complete or more awkward than expected — could force a fallback (e.g. supplementing with raw DOM queries) not currently anticipated.
- `playwright-core`'s wasm-bindgen JS glue may not currently expose persistent-session APIs in the shape assumed by the issue ("already a listed capability") — needs verification against the actual `crates/wasm/src/browser.rs` glue before committing to the design.
- Idle-timeout reaper needs a background task in the daemon's async runtime; must not leak or block daemon shutdown (existing `NativeBrowser::close`'s "must be called once, explicitly, at daemon shutdown" contract needs to extend cleanly to N session-scoped pages).

## Observability Requirements
- Log session creation (`session_id`, target URL) and session reaping (`session_id`, idle duration) at a level consistent with existing daemon logging (check `log_path` usage elsewhere in the codebase for precedent).
- No new metrics/alerting infrastructure exists in this project (single local daemon, no oncall) — standard log lines are sufficient; no new alert condition needed.

## Risk Control
- No feature-flag system exists in this codebase; new tools are additive (existing `fetch_page` and other tools are unaffected). Rollback procedure: revert the shipping commit(s) — since this is a fully new tool surface, reverting removes the tools cleanly with no migration/data concerns (no persisted state involved, only in-memory sessions).
- No staged rollout needed (single local daemon, no external users to stage against).

## Open Questions
- Exact idle-timeout duration (e.g. 5 minutes?) — to be decided in planning/research, not user-blocking; pick a sensible default and make it easy to find/change.
- ~~Exact locator format for `click`/`type` (role+accessible-name pair vs. a snapshot-returned opaque node ref)~~ — **Resolved during planning**: opaque, snapshot-scoped `ref` id (playwright-mcp's model), not a role+name pair. See `implementation/plan.md` Step 3, "Locator format — resolved to ref-based (not role+name)".
- Whether `navigate` reuses an existing session_id (re-navigate within a session) or always creates a new one — needs a research/plan-phase decision.
