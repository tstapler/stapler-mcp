# Stack Research: oopif-ax-snapshot

## Codebase State (VERIFIED — read directly)

### `crates/native/src/ax.rs` — the primary change target

- `fetch_frame_tree` (lines 144–215): already handles same-process iframes by calling `DescribeNodeParams{backendNodeId}` → extracting `node.frame_id` → recursing with `GetFullAxTreeParams{frameId: child_frame_id}`. When the child `getFullAXTree` fails (cross-origin, or no content document), it does `Err(_) => continue` — leaving the `Iframe` node as a childless leaf in `raw`.
- The function signature is `fn fetch_frame_tree(page: &Page, frame_id: Option<FrameId>, depth: usize) -> BoxFuture<'_, Result<Vec<RawAxNode>, PortError>>`. The `&Page` is the parent session's page — there is no facility to create or use a child CDP session here.
- Node ID prefix scheme: `prefix = |id: &str| format!("{frame_key}:{id}")` where `frame_key` is `frame_id.as_ref().map(|f| f.inner().as_str()).unwrap_or("")`. This ensures `AXNodeId` uniqueness when merging flat `Vec<RawAxNode>` from different frames. The same scheme needs to extend to OOPIF nodes.

### `crates/native/src/browser.rs`

- `NativeBrowser` holds a `chromiumoxide::Browser` instance. The `Browser` type exposes `browser.new_page(url)` but not a direct `Target.attachToTarget` API in chromiumoxide's public surface.
- `TabState.latest_refs: Rc<RefCell<HashMap<String, ax::ResolvedRef>>>` maps ref strings to `BackendNodeId + role`. Currently all `BackendNodeId` values are assumed to be resolvable via the session's own `Page` (the parent CDP connection).
- `invoke_on_node` and `resolve_object_id` both take `page: &Page` — hardcoded to the parent session's page.

### `crates/native/Cargo.toml`

- `chromiumoxide = "0.9"` — version 0.9.x. Generated CDP bindings from Chrome PDL files.

## chromiumoxide CDP Bindings for Target Domain

To evaluate whether `Target.attachToTarget` is available:

```
grep -r "AttachToTarget\|attach_to_target" crates/native/
```

**Finding**: chromiumoxide 0.9.x generates its CDP bindings from Chrome's DevTools Protocol specification. The `Target` domain is part of the spec and includes `Target.attachToTarget` (returns a `sessionId` string) and `Target.detachFromTarget`. These appear in the generated bindings under `chromiumoxide::cdp::browser_protocol::target`.

**Critical gap**: `attachToTarget` returns a `SessionId` (an opaque string), not a `chromiumoxide::Page` object. A `Page` in chromiumoxide is constructed only by the `Browser` type's internal connection management, not from a raw CDP `SessionId`. To use a child session, you would need to either:
1. Route CDP messages manually using the raw `SessionId` (bypassing chromiumoxide's `Page` abstraction entirely).
2. Find a chromiumoxide API to "wrap" a `SessionId` into a usable Page-equivalent — none exists in the public API of 0.9.x.
3. Use `Browser::attach_to_browser_target` or a similar chromiumoxide-specific method if one exists.

This is the primary implementation risk identified in the backlog item: "real session management, not a flag."

## External Art: Stagehand and Playwright Patterns

### Stagehand (`frame.ts:94-122`)
- Calls `Accessibility.getFullAXTree({frameId})` scoped, then falls back unscoped.
- The comment notes this only works because "Stagehand has already attached a dedicated CDP session to that frame's target elsewhere" via `Target.attachToTarget` in its target-management layer.
- Stagehand is TypeScript/Node.js and has direct access to the CDP protocol's raw session multiplexing, which is simpler in Node than in Rust.

### Playwright (`page.ts:1113-1163`)
- `ariaSnapshotJSONForFrame` recursively re-invokes itself per iframe ref found.
- Uses a separate per-frame CDP session (Playwright's internal `CDPSession` — a first-class object in Playwright's architecture).
- `mergeIframeChildren` splices child trees back into the parent by ref (`page.ts:1158-1163`).
- Playwright manages CDP sessions at a lower level than chromiumoxide; it wraps the protocol natively and exposes `CDPSession` as a public API.

**Key difference**: both Stagehand and Playwright have first-class CDP session objects distinct from their "Page" abstraction. chromiumoxide does not — its `Page` is the session, and there is no public API for creating a child session from a `SessionId`.

## Feasibility Assessment

### Path A: Raw CDP Message Routing (Hard)
Use `chromiumoxide`'s lower-level `Handler` / raw CDP transport to send/receive CDP messages on a child session's `SessionId`. This would require bypassing the `Page` abstraction entirely and doing manual JSON CDP message construction — no type-safe bindings, significant boilerplate, fragile.

### Path B: Browser::new_page with Target Association (Possibly viable)
If `chromiumoxide::Browser` exposes a way to "adopt" an existing target by its `targetId` (as returned by `Target.getTargets`), creating a `Page` handle for an already-existing target rather than launching a new tab. Needs empirical verification against chromiumoxide 0.9.x's public API.

### Path C: chromiumoxide Version Bump (Investigate)
Later chromiumoxide versions (post-0.9) may add explicit OOPIF / child session support. Check GitHub issue tracker.

### Path D: Snapshot-only via `Target.sendMessageToTarget` (Deprecated)
`Target.sendMessageToTarget` was the old way to send CDP to a specific session; replaced by `sessionId` on all messages. Marked deprecated in the protocol spec. Not viable.

### Path E: Evaluate demand first, defer implementation
Given the open questions about chromiumoxide's session API, and the "evaluate demand before committing" priority in the backlog item, the most prudent path may be to (1) create a minimal test harness to verify chromiumoxide 0.9.x session API capabilities empirically, then (2) decide on Path A/B/C based on findings.

## Recommended Versions / Dependency Actions

| Component | Current | Action |
|---|---|---|
| `chromiumoxide` (native) | `"0.9"` | Verify Target.attachToTarget binding exists; may need bump if child-session API not exposed |
| New Cargo deps | — | Possibly none; or `chromiumoxide` patch if child session not in 0.9 |
| wasm adapter | N/A | Out of scope — playwright-core handles OOPIF natively |

## Patterns to Follow

- `fetch_frame_tree`'s existing `Err(_) => continue` for unattachable frames is the right degradation model — extend, don't replace.
- The `prefix = |id: &str| format!("{frame_key}:{id}")` scheme must be extended to OOPIF nodes, using the child target's `targetId` as the prefix key (not `frameId`, which may not be unique across processes).
- Integration tests follow the `#[ignore = "requires a real Chrome..."]` pattern established in `ax.rs` tests.

## Task 0.1 Finding: GO (Path A, typed raw commands)

Probe result against chromiumoxide 0.9.1 (done within the 8h time-box):

- `Page` is bound to a single session and has no public constructor from an existing `SessionId`; `Browser::get_page` cannot serve an OOPIF target. Path B is not viable.
- `chromiumoxide::Connection`, `Command`, and the typed `Target.attachToTarget` / `Target.detachFromTarget` params are public. A second browser-level connection can attach (`flatten: true`) and send typed commands (e.g. `Accessibility.getFullAXTree`) with a `SessionId`. Path A is viable without hand-built JSON.
- No version bump needed (Path C unnecessary).

Implemented in `crates/native/src/oopif.rs` (`OopifHub`, lazy connect, `detach_all` on every exit path). Verified end to end by the `#[ignore]`d test `ax::tests::snapshot_should_include_nodes_from_cross_origin_iframe` (passes against real Chrome). Click/type routing into OOPIF nodes remains out of scope.
