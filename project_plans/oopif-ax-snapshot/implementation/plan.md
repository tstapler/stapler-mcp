# Implementation Plan: oopif-ax-snapshot

## Summary

Extend `crates/native/src/ax.rs`'s `fetch_frame_tree` to attach a child CDP session to cross-origin (OOPIF) iframe targets, capture their AX trees, and merge the results into the parent tree — mirroring the Playwright/Stagehand pattern. First pass is snapshot-only; click/type routing through child sessions is deferred.

## Prerequisites (must verify before any code)

**Task 0.1 — Probe chromiumoxide 0.9 child-session API**
Run a one-off Rust experiment (in a `tests/` file, not shipped code) to determine whether `chromiumoxide 0.9`'s `Browser` type exposes any of:
- `Browser::get_or_create_page(target_id: TargetId)` 
- A way to create a `Page` wrapping an existing CDP target rather than launching a new tab
- `Page::execute_on_session(session_id, params)` or raw CDP dispatch on a named session

If none exist, determine whether chromiumoxide's `Handler` exposes raw session multiplexing, or whether a crate version bump (to a post-0.9 release) adds this.

**Acceptance**: documented finding in `research/stack.md §Feasibility` specifying which path (A/B/C from the stack research) is used and citing the specific chromiumoxide API or its absence.

## Epic 1: OOPIF Snapshot Capture (Snapshot-only)

### Story 1.1 — OOPIF attach/capture in `fetch_frame_tree`

**Context**: `fetch_frame_tree` in `ax.rs` already has an `Err(_) => continue` fallback when `getFullAXTree{frameId: child_frame_id}` fails. OOPIF frames are exactly the case that triggers this fallback today.

**Task 1.1.1 — OOPIF detection and attach**
In the loop over `iframe_nodes` (after `DescribeNodeParams` returns a `child_frame_id`), when the recursive `fetch_frame_tree` call fails, attempt `Target.attachToTarget{targetId: <target for child_frame_id>}` to obtain a child CDP session. This requires:
a. Finding the `TargetId` for the child frame — either via `Target.getTargets` filtered by `frameId`, or from the CDP `Page.frameAttached` event (if available at capture time).
b. Calling `Target.attachToTarget` on that target — returns a `SessionId`.
c. Creating a temporary Page-like handle over that SessionId (per Task 0.1's finding on chromiumoxide API).
d. Calling `GetFullAxTreeParams::default()` (no frameId needed — the child session is already scoped to that renderer) on the child session.

**AC**: When a cross-origin iframe's `DescribeNodeParams` → `frameId` → `getFullAXTree` fails, a fallback `Target.attachToTarget` attempt is made; if it succeeds, the child tree's nodes are merged (with child TargetId prefix) and the iframe node gains children.

**Task 1.1.2 — TargetId prefix for OOPIF nodes**
Replace `frame_key = frame_id.as_ref().map(|f| f.inner().as_str()).unwrap_or("")` with a two-level prefix: `{target_id}:{frame_id}:{ax_node_id}` for OOPIF nodes. Ensures no collision between same-process `AXNodeId`s and OOPIF `AXNodeId`s even if both are `0` (a valid root ID in both namespaces).

**AC**: A parent-process node `"mainframe:123"` and an OOPIF node `"{target_id}::456"` never hash to the same key in `by_parent`. 

**Task 1.1.3 — RAII detach for child sessions**
Wrap each `Target.attachToTarget` result in an RAII guard that calls `Target.detachFromTarget` on `Drop`. Ensures leaked child sessions are not accumulated even on `?` early-returns from `fetch_frame_tree`.

**AC**: No child sessions remain attached after `capture_snapshot` returns (success or error).

**Task 1.1.4 — Graceful degradation on OOPIF attach failure**
If `Target.attachToTarget` itself fails (target gone, insufficient permissions), the fallback is identical to the current `Err(_) => continue` — the `Iframe` node remains a childless leaf and the overall snapshot succeeds.

**AC**: A snapshot of a page where one OOPIF attach fails still returns the rest of the page's AX tree.

### Story 1.2 — ResolvedRef tagging for OOPIF nodes

**Context**: `BackendNodeId` values from an OOPIF child process are not resolvable via the parent session's `Page`. For snapshot-only first pass, we only need to know this at redaction time (§7 in pitfalls.md) and click/type time (not yet in scope).

**Task 1.2.1 — Add `is_oopif: bool` to `ResolvedRef`**
Add a boolean field to `ax::ResolvedRef` marking nodes that came from a child CDP session. Set `is_oopif = true` for all nodes merged from an OOPIF child capture.

**AC**: `ResolvedRef { backend_node_id, role, is_oopif }` distinguishes same-process and OOPIF nodes.

**Task 1.2.2 — Fail-safe redaction for OOPIF form controls**
In `redact_form_control_values`, if the node's `ResolvedRef` has `is_oopif = true`, skip the CDP probe and unconditionally apply `REDACTED_PLACEHOLDER` for form-control roles (same as the existing `Err(())` fail-safe path, but without attempting a doomed CDP call).

**AC**: A password field inside an OOPIF always gets `REDACTED_PLACEHOLDER` in the snapshot, never its raw value.

## Epic 2: Integration Test

### Story 2.1 — Cross-origin iframe integration test

**Task 2.1.1 — Test harness for two-origin scenario**
Create a `#[tokio::test] #[ignore = "requires a real Chrome..."]` test in `ax.rs`'s `#[cfg(test)]` block (following `snapshot_should_redact_value_when_field_was_autofilled_out_of_band`'s pattern):
- Launch a Chrome page with an `<iframe src="http://127.0.0.1:<port>/inner">` where the inner origin is served on a different IP/port combination Chrome treats as cross-origin.
- Call `capture_snapshot` on the parent page.
- Assert the snapshot contains at least one node from the inner iframe's document (e.g., a button or textbox visible inside the iframe).

**AC**: Test name `snapshot_should_include_nodes_from_cross_origin_iframe`; marked `#[ignore]`; asserts non-empty children on the `Iframe`-role node in the snapshot.

## Epic 3: Documentation and Limitations

### Story 3.1 — Document snapshot-only OOPIF limitation

**Task 3.1.1 — Code comment in `ax.rs`**
Add a module-level comment paragraph noting that OOPIF nodes in the snapshot carry `is_oopif = true` in their `ResolvedRef` and that `click`/`type` into OOPIF nodes is not yet supported (routes through parent session, which cannot resolve cross-process `BackendNodeId`s) — targeted at future implementers of the click/type routing follow-on.

## Task Estimate Summary

| Task | Estimate | Category |
|------|----------|----------|
| 0.1 — Probe chromiumoxide child-session API | 4h | backend |
| 1.1.1 — OOPIF detect + attach in fetch_frame_tree | 8h | backend |
| 1.1.2 — TargetId prefix for OOPIF nodes | 2h | backend |
| 1.1.3 — RAII detach guard | 2h | backend |
| 1.1.4 — Graceful degradation on attach failure | 1h | backend |
| 1.2.1 — is_oopif field on ResolvedRef | 1h | backend |
| 1.2.2 — Fail-safe redaction for OOPIF | 2h | backend |
| 2.1.1 — Integration test | 4h | test |
| 3.1.1 — Code comment | 1h | docs |

**Total**: ~25h / ~3 engineering days (not counting Task 0.1 if the chromiumoxide probe reveals a major blocker requiring a version bump or raw CDP approach).

## Go/No-Go Gate

Task 0.1 is a hard gate. If chromiumoxide 0.9 does not expose a usable child-session API and neither a patch bump nor raw-CDP workaround is feasible within appetite, this item should remain "later" and the requirements.md open question should be documented with the empirical findings for the next engineer who revisits it.
