# Validation: oopif-ax-snapshot

## Acceptance Criteria Checklist

### AC 1 — OOPIF nodes appear in snapshot
- **Test**: `snapshot_should_include_nodes_from_cross_origin_iframe` (integration, `#[ignore]`).
- **Pass condition**: `capture_snapshot` on a page with a cross-origin iframe returns an `AxSnapshot` where the `Iframe`-role node has at least one child node, and at least one of those descendants has a non-empty `name` matching content from the inner frame.
- **Status**: pending (requires Task 0.1 + 1.1.1 complete).

### AC 2 — Childless leaf on failed OOPIF attach
- **Test**: unit test with a fake that always returns `Err` for `Target.attachToTarget`.
- **Pass condition**: `capture_snapshot` returns `Ok(snapshot)`, the `Iframe` node is present as a childless leaf, and no `PortError` is propagated.
- **Status**: pending.

### AC 3 — OOPIF form control values are redacted
- **Test**: unit test using `FakeProbe` that is never called for `is_oopif = true` nodes; asserts `REDACTED_PLACEHOLDER` on a textbox-role OOPIF node.
- **Pass condition**: `redact_form_control_values` applies `REDACTED_PLACEHOLDER` to OOPIF form-control nodes without calling `probe.probe()`.
- **Status**: pending.

### AC 4 — MAX_FRAME_DEPTH applies to OOPIF chains
- **Test**: unit/integration test with synthetic OOPIF chain of depth 6; assert that depth-5 OOPIF is left as childless leaf (depth guard fires before attach attempt).
- **Pass condition**: No OOPIF attach is attempted at depth >= MAX_FRAME_DEPTH.
- **Status**: pending.

### AC 5 — Child sessions are detached after snapshot
- **Test**: integration test; instrument Chrome's DevTools to check active sessions count before and after `capture_snapshot`.
- **Pass condition**: active session count is the same after capture as before (no leaked child sessions).
- **Status**: pending.

### AC 6 — Existing same-process iframe traversal unaffected
- **Test**: Existing integration tests for same-origin iframes (if any); also verify `fetch_frame_tree`'s existing unit tests still pass.
- **Pass condition**: `cargo test --workspace` passes with no regressions on existing iframe-traversal paths.
- **Status**: passes today (baseline); must continue to pass after changes.

### AC 7 — AXNodeId uniqueness across OOPIF boundary
- **Test**: Unit test `build_tree_should_not_collide_oopif_and_parent_nodeids` — synthesize `RawAxNode` list where an OOPIF node and a parent node share the same underlying integer id but different frame-prefix strings; assert both appear as distinct nodes in the output.
- **Pass condition**: both nodes present, separate refs, no map collision.
- **Status**: pending.

### AC 8 — No regressions: clippy and tests
- `cargo test --workspace` green.
- `cargo clippy --workspace --all-targets -- -D warnings` clean.
- **Status**: pending.

## Go/No-Go Criteria

Task 0.1 (chromiumoxide API probe) must return one of:
- **Go**: chromiumoxide 0.9 exposes a viable child-session API → proceed with Epic 1.
- **Conditional go**: A minor chromiumoxide version bump (still in `0.9.x` or `0.10.x`) adds the API with no other breaking changes → bump and proceed.
- **No-go**: No viable path found within 8h time-box → document findings, close this item as "blocked on chromiumoxide upstream," and add an open question to requirements.md for the next evaluator.
