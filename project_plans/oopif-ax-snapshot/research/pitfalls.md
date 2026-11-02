# Pitfalls: oopif-ax-snapshot

## §1 — chromiumoxide has no first-class child-session API

**Problem**: `Target.attachToTarget` returns a `SessionId` (opaque string). chromiumoxide's `Page` type is constructed only through `Browser`'s internal connection management. There is no `Page::from_session_id(id)` or `Browser::adopt_target(target_id)` in chromiumoxide 0.9.x's public API. You cannot pass a raw `SessionId` to `GetFullAxTreeParams::execute()`.

**Consequence**: "attach a CDP session to that frame's target" is straightforward in TypeScript/Node (Playwright, Stagehand use it) but requires either (a) routing raw CDP JSON manually or (b) finding/adding a chromiumoxide API for this use case.

**Mitigation**: Empirically probe the chromiumoxide 0.9 API before committing to the implementation path. Check if `Browser::get_page(target_id)` or equivalent exists. If not, check whether a patch-version or minor-version bump to chromiumoxide provides it. Fallback: prototype raw CDP message routing against the underlying `Handler`.

## §2 — BackendNodeId namespace is per-renderer-process

**Problem**: `BackendNodeId` values are assigned by each renderer process independently. A `BackendNodeId` of `42` in an OOPIF child process is a different DOM node from `BackendNodeId` of `42` in the parent process. The current `refs` map in `TabState` stores `BackendNodeId → ResolvedRef` without tracking which CDP session the ID came from. `resolve_object_id`/`invoke_on_node`/`verify_node_live` all take `&Page` (the parent session's page) — they will misroute OOPIF refs.

**Consequence**: click/type targeting an element inside an OOPIF would silently target the wrong element (same integer BackendNodeId in the parent process), or fail with a "node not found" CDP error if no parent-process node has that ID.

**Mitigation**: For the first pass, scope to snapshot-only — surface OOPIF nodes in the AX tree with their correct ref strings, but do not add per-frame session handles to `TabState`. Add a `session_id: Option<ChildSessionId>` field to `ResolvedRef` and propagate it in a follow-on. Document this as a known limitation in the snapshot output (a special `ref` prefix like `oopif:...` can serve as a cue to callers that click/type is not yet supported for these refs).

## §3 — AXNodeId uniqueness across processes

**Problem**: The existing `frameId`-prefix scheme (`format!("{frame_key}:{id}")`) uses `FrameId` strings to namespace `AXNodeId`s when merging same-process iframes into a flat `Vec<RawAxNode>`. For OOPIF, the `FrameId` of the child frame is assigned in the parent process; the child process also uses `FrameId` in its own AX node IDs, but those are assigned by the child's own renderer. Using the same `FrameId` as prefix in both processes would create collisions if any `AXNodeId` string in the child happens to match one in the parent.

**Mitigation**: Use the child `TargetId` (not `FrameId`) as the prefix namespace for OOPIF child nodes. `TargetId` is guaranteed unique across all CDP targets in a session. The parent `Iframe` node's re-parenting link (child root's `parent_id` set to the parent `Iframe`'s node_id) must use the child-prefixed node_id.

## §4 — Child session lifecycle: detach on error

**Problem**: If `capture_snapshot` partially attaches to child targets and then fails mid-way (e.g., a depth-limit hit, a timeout, or the `BackendNodeId` prefix collision discovered during merge), the already-attached child CDP sessions are never detached. Leaked sessions consume resources in the Chrome process.

**Mitigation**: Use an RAII guard for each `Target.attachToTarget` call — detach on `Drop` regardless of success or error. Structure the OOPIF recursion so a single `collect` phase gathers all attach-handles, followed by all captures, followed by all detaches.

## §5 — MAX_FRAME_DEPTH bypass via OOPIF chains

**Problem**: The existing `MAX_FRAME_DEPTH = 5` guard is checked before the same-process recursive call. If OOPIF targets chain (iframe A embeds cross-origin iframe B which embeds cross-origin iframe C), the depth counter must be threaded through the OOPIF path too, not just the same-process path.

**Mitigation**: Thread `depth` as-is into the OOPIF branch. The same `if depth >= MAX_FRAME_DEPTH { return Ok(raw); }` guard already in `fetch_frame_tree` will fire before attempting the OOPIF attach if the limit is reached.

## §6 — Integration test portability

**Problem**: A test that verifies OOPIF traversal needs two origins serving different content. In CI, there is no guaranteed way to have two HTTP servers running on different ports treated as "cross-origin" by Chrome's site-isolation — Chrome may not trigger OOPIF for `localhost:port1` vs `localhost:port2` (same host, different port = same origin in the site-isolation sense).

**Mitigation**: Use `--disable-site-isolation-trials` Chrome flag in test-only launch config, OR serve the inner iframe from a different IP (e.g., `127.0.0.1` vs. `::1`) since Chrome treats those as separate security origins for site-isolation. The `#[ignore]` pattern for integration tests is already established in `ax.rs` — this test will follow the same convention with explicit documentation of the launch flag needed.

## §7 — Redaction in OOPIF subtrees

**Problem**: `redact_form_control_values` (post-`build_tree` pass in `capture_snapshot`) iterates the AX tree nodes and calls `probe.probe(backend_node_id)` to check if each form-control field should be redacted. For OOPIF nodes, `probe_redaction` uses `DOM.resolveNode` on the parent session's `Page`. A `BackendNodeId` from an OOPIF child process cannot be resolved via the parent's `DOM.resolveNode` — the call will return an error (or worse, resolve a different node with the same integer ID in the parent process).

**Mitigation**: For the snapshot-only first pass, OOPIF nodes that are form controls should be redacted unconditionally (fail-safe behavior matching what `probe_redaction`'s `Err(())` path already does). Add a note to `ResolvedRef` or a separate OOPIF-node flag so `redact_form_control_values` knows to apply the fail-safe path without attempting a CDP probe for those nodes.
