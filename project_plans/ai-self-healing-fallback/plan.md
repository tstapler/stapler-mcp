# Plan: ai-self-healing-fallback

**Date**: 2026-10-29
**Phase**: sdd:3-plan

## Decision

**Do NOT add LLM calls to the daemon.** The daemon's "no AI dependency" design is not accidental — it is a core property enabling predictable, low-cost local operation. Adding an LLM call in the failure path of `stapler_browser_click`/`stapler_browser_type` violates this property for marginal gain over what the calling agent can already do.

**Self-healing belongs at the agent layer, not the daemon layer.** The calling LLM agent is already in the best position to handle stale-ref failures: it has context about what it was trying to accomplish, can decide whether re-snapshot+retry makes sense or whether a different strategy is needed, and can absorb the extra round-trip without surprising the user.

## Recommendation: Three-part improvement, no LLM dependency

### Part 1 — Improve error messages to make agent-side retry obvious (P1 of this item)

The existing `PortError::NotFound` error for stale refs should include a structured recovery hint. Current message (approximate): something identifying the session and failing ref. Improved message:

> `element ref '{ref_id}' is no longer in the current accessibility tree (the page may have re-rendered); call stapler_browser_snapshot to get fresh refs and retry`

This zero-cost change makes the agent-side retry idiom explicit. An LLM agent that reads this message knows exactly what to do. Implementation: modify `not_found_message` in `crates/native/src/browser.rs` (and the wasm equivalent) to include a structured hint when the failure is a locator resolution error vs. a session-not-found error.

### Part 2 — Add recovery hint to tool schema descriptions (P2 of this item)

The `stapler_browser_click` and `stapler_browser_type` tool schema descriptions (doc comments in `crates/core/src/schema.rs`) should note:

> "If `refId` is stale (the page re-rendered since the snapshot), this call returns an error; call `stapler_browser_snapshot` to get a fresh accessibility tree and retry with the new ref."

This surfaces the retry pattern to the LLM before it even makes the first call — not just when it fails. Zero implementation cost.

### Part 3 — Evaluate daemon-side last-snapshot re-resolve (P3 of this item, lower priority)

A zero-LLM-dependency middle ground: cache the last `AxSnapshot` per session (already returned after every action) and on a `NotFound` failure:
1. Re-take a fresh snapshot (one CDP round-trip).
2. Scan for a node with the same role+name as the cached ref.
3. If unambiguous match found, retry automatically.
4. Return the result + a `note` field indicating self-heal fired (consistent with the existing `note` field pattern in `BrowserActionOutput`).

This handles the most common stale-ref case (SPA re-render without content change) with no LLM dependency and ~50-200ms added latency only on failure.

**Risk**: role+name matching is ambiguous when duplicate elements exist (e.g., two "Submit" buttons). Mitigation: only fire the re-resolve if the match is unambiguous (exactly one candidate with the same role+name); propagate the original error otherwise.

**Scope**: small. Requires:
- Adding a `last_snapshot: Option<AxSnapshot>` field to the per-session state in `NativeBrowser` (and wasm equivalent).
- A simple scan function over the AX tree by role+name.
- Logic in `click`/`type_text` driver implementations to trigger it on `NotFound`.
- A `note` field in the success response indicating self-heal fired.

This is a "nice-to-have" for the deferred backlog. The error-message improvement (Part 1) is the highest-value, lowest-cost change.

## Decision answers (from requirements.md evaluation questions)

**Q1: Is daemon-side self-healing worth doing at all?**
No, for LLM-based self-healing. Yes, for the zero-LLM last-snapshot re-resolve (Part 3, deferred). The agent-side retry pattern (already possible) plus improved error messages (Part 1) covers 80% of the value.

**Q2: Which approach fits better — Stagehand or Skyvern?**
Neither, for the daemon. Stagehand's re-snapshot+LLM-relocate requires an LLM dependency that violates the daemon's design. Skyvern's vision-hybrid is a different product architecture entirely and doesn't map to incremental improvement. The zero-LLM analog of Stagehand (Part 3) is the closest viable option.

**Q3: Does this belong in the daemon or the calling agent?**
The calling agent, for LLM-based recovery. The daemon, for the zero-LLM re-resolve (Part 3) if that scope is approved.

**Q4: Cost/latency/API-key implications of daemon calling an LLM directly?**
New API key class in the daemon environment, ~$0.001/call (low but non-zero), +200-1000ms latency on every triggered failure, new failure modes (rate limit, auth, network). Not justified given that agent-side retry achieves the same result.

## Implementation roadmap (if approved)

### Epic 1 — Error message improvements (small, do first)

**Story 1.1** — Distinguish "ref stale" from "session not found" in `PortError::NotFound`  
Currently, both session-not-found and element-not-found map to `PortError::NotFound`. Adding a sub-variant or a structured message for the element-not-found case enables Part 1's improvement.
- `crates/native/src/browser.rs` — `not_found_message` function: add explicit recovery hint for locator resolution failure.
- `crates/wasm/src/browser.rs` — equivalent JS glue change.
- Estimate: 2h.

**Story 1.2** — Update tool schema doc comments  
`crates/core/src/schema.rs` — `BrowserClickInput.ref_id` and `BrowserTypeInput.ref_id` doc comment additions.
- Estimate: 30m.

### Epic 2 — Zero-LLM daemon re-resolve (medium, defer)

**Story 2.1** — Cache last AxSnapshot per session in `NativeBrowser`  
Add `last_snapshot: Option<AxSnapshot>` to the session struct; update on every successful snapshot return.
- `crates/native/src/browser.rs` — session struct + update sites.
- Estimate: 1h.

**Story 2.2** — Re-resolve by role+name on locator NotFound  
On `PortError::NotFound` in `click`/`type_text`, check last snapshot for an unambiguous role+name match, re-try CDP dispatch with new ref.
- `crates/native/src/browser.rs` — click/type_text implementations.
- Estimate: 3h.

**Story 2.3** — Surface `note` field on self-heal  
Add "self-heal fired: re-resolved '{role}/{name}' to new ref" to the `BrowserActionOutput.note` field.
- `crates/core/src/tools/browser.rs` — `browser_click`/`browser_type` note construction.
- Estimate: 1h.

**Story 2.4** — Test coverage for re-resolve  
Integration test: navigate, snapshot, force AX tree rebuild (navigate same URL), click with stale ref — expect success + note. Unit test: role+name scan on a fake AX tree with ambiguous + unambiguous cases.
- `crates/cli/tests/browser_session.rs` — new `#[ignore]` real-browser test.
- Estimate: 2h.

**Story 2.5** — Wasm adapter equivalent  
`crates/wasm/src/browser.rs` — same last-snapshot cache + re-resolve logic in JS glue.
- Estimate: 2h.

## Locator format note

Refs are opaque CDP AX backend node IDs, consistent with `playwright-mcp`'s model and unchanged by this work. Part 3's re-resolve uses role+name as a secondary resolution key — a fallback only, not a replacement for the ref contract.

## What is NOT planned

- LLM calls in the daemon (any path).
- Vision-hybrid targeting (Skyvern-style).
- Integration with any external LLM API (Anthropic or otherwise) from the daemon layer.
- Any change to the snapshot+ref caller contract — this is purely additive.

## Deferral condition

Parts 1 and 2 (error message + schema doc comment) should ship as a fast-follow to the browser automation work already done. Part 3 (daemon re-resolve) should be deferred until there is evidence (from real agent usage) that stale-ref errors are frequent enough to justify the implementation cost. The key signal: how often do callers see `PortError::NotFound` on `click`/`type` after a successful navigate/snapshot in the same session?
