# Requirements: oopif-ax-snapshot

**Date**: 2026-10-29
**Type**: feature
**Complexity**: 3 — moderate/non-trivial (CDP session management, recursive tree merge, session lifecycle)

## Problem Statement

Out-of-process iframes (cross-origin `<iframe>`s) are invisible to `stapler_browser_snapshot`. Chrome's site-isolation runs each cross-origin frame as a separate renderer process and CDP target. A single `Accessibility.getFullAXTree` call is scoped to one target, so it stops at a cross-origin frame boundary and returns a childless `Iframe` leaf node for that frame — even though the frame's document has a full accessibility tree of its own. Content inside cross-origin iframes (embedded payment forms, OAuth widgets, third-party embeds) is currently unreachable by `stapler_browser_snapshot`, `stapler_browser_click`, and `stapler_browser_type`.

The `fetch_frame_tree` function in `crates/native/src/ax.rs` already handles same-process iframes recursively (via `DescribeNodeParams` → `frameId` → recursive `getFullAXTree`). When it encounters a cross-origin frame, the call to retrieve the child frame's content fails silently and the iframe is left as a childless leaf — the code already has `Err(_) => continue` for this case (line ~203). This silently truncates the AX tree at every cross-origin boundary without the caller knowing.

## Baseline

The existing `fetch_frame_tree` implementation in `crates/native/src/ax.rs` (lines 144–215):
- Already descends into same-process child iframes recursively via `DescribeNodeParams` → `child_frame_id` → recursive `getFullAXTree`.
- Does **not** handle OOPIF: cross-origin frames run as separate CDP targets in a different renderer process; `GetFullAxTreeParams{frameId: child_frame_id}` against the parent session cannot reach them, so `execute(params)` fails and the code falls through to `continue`, leaving a childless `Iframe` leaf.
- No CDP `Target.attachToTarget` / `Target.attachedToTarget` machinery exists anywhere in the native adapter today — `browser.rs` and `ax.rs` both work exclusively within a single page's CDP session.

## Users / Consumers

LLM agents using `stapler_browser_snapshot` / `stapler_browser_click` / `stapler_browser_type` against pages that embed cross-origin iframes: payment flows (Stripe, Braintree), OAuth widgets (Google sign-in, GitHub OAuth), embedded third-party forms, or any site using cross-origin `<iframe>` for isolation.

## Success Metrics

- `stapler_browser_snapshot` returns AX tree nodes from inside a cross-origin `<iframe>` (verified by an `#[ignore]`d integration test that loads a page with a cross-origin iframe and asserts the iframe's inner interactive elements appear in the snapshot).
- `stapler_browser_click` and `stapler_browser_type` can target a `ref` resolved from inside a cross-origin iframe — the `BackendNodeId` returned by the child session's tree resolves correctly when dispatching clicks/type via the child session's CDP connection.
- A cross-origin iframe that cannot be attached to (no content document, or target no longer available) still degrades gracefully: it becomes a childless `Iframe` leaf, not an error that aborts the whole snapshot.
- The feature is **native-adapter only** — the wasm adapter (playwright-core) already handles OOPIF transparently via its own multi-session management and is explicitly out of scope.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` pass with no new warnings.
- The `MAX_FRAME_DEPTH` guard (currently `5`) continues to cap recursive descents including OOPIF chains.

## Appetite

Medium (1–2 weeks) — non-trivial CDP session-lifecycle work but scoped to a single file (`ax.rs`) plus one supporting change in `browser.rs` to thread the `Browser`/CDP target-attach surface down into `fetch_frame_tree`. No new dependencies required if `chromiumoxide`'s existing `Target.attachToTarget` CDP binding covers the need.

## Constraints

- Native adapter only — the wasm adapter (`playwright-core`) already handles OOPIF natively and is out of scope.
- Must not regress same-process iframe traversal already working in `fetch_frame_tree`.
- Must not change `capture_snapshot`'s public signature (`page: &Page, next_ref_id, previous_refs`) — the OOPIF session registry is an internal implementation detail.
- The fail-safe "leave as childless leaf" behavior for unattachable frames must be preserved — a failed OOPIF attach must not abort the whole snapshot.
- `MAX_FRAME_DEPTH` must apply to OOPIF recursion chains too.

## Non-functional Requirements

- **Session lifecycle**: attached child CDP sessions must be detached after use (not leaked). An OOPIF target that crashes between attach and AX capture must be caught and treated as an unattachable frame.
- **Security**: no change to the SSRF guard — the OOPIF content is already being loaded by the browser (the user navigated to the parent page); this feature only reads AX data from already-loaded frames, it does not initiate new navigations.
- **Ref namespacing**: `BackendNodeId` values are scoped per renderer process; a `BackendNodeId` from a child session can collide numerically with one from the parent. The existing `frameId`-prefix scheme in `fetch_frame_tree` (line ~159: `prefix = |id: &str| format!("{frame_key}:{id}")`) ensures `AXNodeId` uniqueness in the flat merged `Vec<RawAxNode>`. The same prefix must cover `BackendNodeId` resolution so `resolve_locator_impl`/`verify_node_live` route clicks/types through the correct CDP session.
- **Observability**: no new log lines required beyond the existing "continue on failure" behavior; however, a debug-level log when an OOPIF attach fails (with the frame's URL, if available) would be useful for diagnosing sites where no OOPIF content appears.

## Scope

### In Scope
- Modify `fetch_frame_tree` (or a successor) to attempt `Target.attachToTarget` when a child frame cannot be reached via `getFullAXTree{frameId}` due to cross-origin site-isolation.
- Build an OOPIF child session, capture its AX tree via the child session's own CDP connection, and splice its nodes into the parent tree (matching the Playwright `mergeIframeChildren` / Stagehand `attachToTarget` patterns described in the backlog item).
- Ensure `BackendNodeId` resolution for click/type (`resolve_locator_impl` → `verify_node_live` → `invoke_on_node`) works correctly for refs originating in child sessions — this may require threading a per-frame session handle alongside the `refs` map.
- Session cleanup: detach child CDP sessions after snapshot capture completes (or on error).
- An `#[ignore]`d integration test using two pages served from different origins (or a test page with a cross-domain iframe) to verify end-to-end OOPIF visibility.

### Out of Scope
- Wasm adapter changes (playwright-core already handles OOPIF).
- Nested OOPIF-within-OOPIF beyond what `MAX_FRAME_DEPTH` already limits.
- Shadow DOM traversal (already handled via a single `getFullAXTree` call per the existing code comments).
- Any new MCP tool parameters or output changes — OOPIF content appears seamlessly in existing `snapshot` output; no API surface changes.
- `click`/`type` routing through OOPIF child sessions if `BackendNodeId` resolution turns out to require per-frame session handles — scope that to a follow-on if the routing complexity is high.

## Rabbit Holes

- **Per-frame session handles for click/type**: `invoke_on_node`/`resolve_object_id` in `browser.rs` both take a `&Page` (the parent session's page). A `BackendNodeId` from an OOPIF child process can only be resolved via that child session's CDP connection, not the parent's. Threading a `HashMap<BackendNodeId, ChildSession>` alongside `refs` would allow correct routing — but this is non-trivial bookkeeping and may justify deferring click/type-into-OOPIF to a follow-on (snapshot-only first pass).
- **Target discovery**: `chromiumoxide` exposes `Target.attachToTarget` via `AttachToTargetParams` in its CDP bindings. Confirm the bindings include it and that the returned session handle is usable as a first-class `Page`-equivalent before assuming this works. Stagehand's comment that it "already attached a dedicated CDP session to that frame's target elsewhere" implies this is non-trivial to wire up in the right lifecycle order.
- **Flat `Vec<RawAxNode>` merge**: the current merge splices child nodes by re-parenting the child root to the parent `Iframe` node id. A child session's `AXNodeId` namespace is independent; the existing frameId-prefix scheme needs to extend to the OOPIF case (prefix with the child target id, not the frame id, since the frame id space may not be unique across processes).

## Alternatives Considered

- **Status quo — leave OOPIF as childless leaf**: already the current behavior; acceptable for same-origin pages but a hard limit for payment/OAuth flows, which are the primary motivation for MCP browser automation in the first place.
- **Snapshot-only, no click/type into OOPIF**: a viable first slice — surface OOPIF content in the snapshot so the caller can see it, defer the click/type routing to a follow-on once the per-frame session handle bookkeeping is figured out.
- **Use `Runtime.evaluate` to extract AX data via JS**: would require injecting JS into cross-origin frames (blocked by same-origin policy), so not viable for OOPIF.

## Open Questions

1. Does `chromiumoxide 0.9`'s CDP binding for `Target.attachToTarget` return a session handle usable as a `Page`? Or does it only return an opaque `SessionId` string, requiring manual CDP message routing? (Needs empirical verification before planning the implementation.)
2. Can `BackendNodeId` values from a child CDP session be resolved via the parent session's `DOM.resolveNode`? If not, click/type into OOPIF must route through the child session — which implies storing child session handles in `TabState.latest_refs` or a parallel map.
3. Is `Target.attachToTarget` the right CDP primitive, or does `Target.setAutoAttach` + `sessionCreated` event give better lifecycle integration for multi-frame scenarios?
4. What is the demand / frequency of OOPIF-heavy target sites among current users? The backlog item explicitly marks this as "later — evaluate demand/frequency before committing."
