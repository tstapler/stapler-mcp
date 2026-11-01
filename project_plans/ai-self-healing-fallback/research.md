# Research: ai-self-healing-fallback

**Date**: 2026-10-29
**Phase**: sdd:2-research

## What is the actual failure mode?

### Current element resolution path (verified in codebase)

`stapler_browser_click` and `stapler_browser_type` both:
1. Accept a `ref_id` (opaque string — a CDP AX backend node ID, wrapped in `Locator`) from a prior `stapler_browser_snapshot`.
2. Pass it to `BrowserDriver::click` / `BrowserDriver::type_text` in `crates/core/src/tools/browser.rs:125-187`.
3. If the driver returns `PortError::NotFound`, the error message is passed through verbatim (see `map_error` at `crates/core/src/tools/browser.rs:87-92`).

The `not_found_message` authored in `crates/native/src/browser.rs` is described in the tool-layer docstring as "already actionable." The error path is clean but terminal — no retry, no fallback.

### When does a ref go stale?

A ref is an opaque AX backend node ID scoped to the CDP session. It goes stale when:
- The page re-renders (SPA update, React re-mount, async content injection) and the AX tree is rebuilt.
- The user (or JS) navigates the tab between snapshot and click.
- An animation or lazy-loader mutates the DOM after the snapshot.

Critically, **stale ref** and **element gone** are different:
- Stale ref: the element still exists visually/semantically, but its AX backend node ID changed because the tree was rebuilt. Re-snapshot and re-resolve would succeed.
- Element gone: the element was removed entirely (navigation, SPA unmount, conditional rendering). No re-snapshot can recover this.

Only the first case is amenable to self-healing. The second is a genuine workflow failure that the agent must handle via different logic (e.g., re-navigate, change strategy).

## Competing tool analysis

### Stagehand — `selfHealAction()` (`actService.ts:371-431`)

**Mechanism** (confirmed via code-archaeology described in the item):
- On any action failure, re-captures a fresh snapshot.
- Sends the snapshot + original action description to an LLM.
- LLM returns a new selector/reference for the target element.
- Retries the action once with the new reference.
- Gated by `context.selfHeal === true` in `takeDeterministicAction()` (opt-in, not default).

**Architectural implication for stapler-mcp**:
- Requires an LLM API call from within the daemon.
- Requires a model (API key, model name) to be configured in the daemon's environment.
- The daemon currently has **zero** LLM calls and **zero** AI dependency. `BRAVE_API_KEY` is the only external API credential it manages.
- Stagehand runs inside a browser extension in a session controlled by an LLM; it doesn't have the daemon's "no AI dependency" design constraint.
- The daemon does not know what the original "action description" was (it only receives `ref_id`, not a natural-language description of the target). Implementing Stagehand's approach would require adding a natural-language description field to `BrowserClickInput`/`BrowserTypeInput`, plus making the daemon call an LLM.

**Cost model**:
- One additional LLM call per stale-ref failure. If using Claude Haiku, ~$0.001/call — low per-call, but adds per-call latency (~200-500ms at p50 for a small prompt).
- Requires API key provisioning in the daemon environment (`ANTHROPIC_API_KEY` or similar).
- Adds a new failure mode: LLM call fails (rate limit, network, auth) → self-heal itself fails → harder debugging.

**Verdict**: Not a good fit for stapler-mcp's current architecture. The daemon's "no LLM dependency" is not an accidental gap; it reflects the design goal of a lean, predictable, locally-operated daemon. Adding an LLM call in the hot path of every click/type failure violates that constraint.

### Skyvern — vision+DOM hybrid initial targeting

**Mechanism** (from README/source-level, not full code-archaeology):
- Rather than resolving element refs post-hoc, Skyvern combines visual page analysis (screenshot + bounding-box extraction) with DOM/AX tree data to identify elements before the action.
- Framed as inherently more resilient to DOM churn because element identification is multi-modal (not solely dependent on a fragile node ID).

**Architectural implication for stapler-mcp**:
- This is a different approach from self-healing (which fixes failures after they happen). Skyvern's model would require replacing the entire snapshot+ref-click loop with a new targeting model.
- It requires a vision-capable LLM model for screenshot analysis — even heavier than Stagehand's text-only approach.
- The stapler-mcp AX-tree snapshot model is already reasonably resilient: `stapler_browser_find` can locate elements by text/role without a snapshot ref. The fundamental problem is that refs obtained from snapshots do go stale.
- This approach would be a ground-up redesign of the browser automation tool surface, not an incremental improvement.

**Verdict**: Not the right comparison for this item. Skyvern's vision-hybrid is a different product architecture entirely — a cloud-based orchestrator, not an MCP tool wrapper. It doesn't map cleanly onto "add a fallback to click/type."

### browser-use — agent fallback loop

**Mechanism** (README-level):
- On any action failure, hands off to a full autonomous agent loop.
- The agent re-analyzes the page, decides on a new strategy, and retries.

**Architectural implication for stapler-mcp**:
- This is essentially what a calling LLM agent can already do: catch the error, re-snapshot, re-locate the element, retry. The difference is that browser-use builds this loop into the tool layer.
- For stapler-mcp, this pattern is already accessible to callers — it's not something that needs daemon support.

**Verdict**: This is the "agent-side retry" baseline. It already works today without any daemon change.

## Agent-side retry: the baseline approach

The simplest self-healing path requires zero daemon changes:

```
1. Agent calls stapler_browser_click(session, ref_A)
2. Error: "element not found" (ref_A stale)
3. Agent calls stapler_browser_snapshot(session)       ← fresh AX tree
4. Agent scans new snapshot for the target element by role/name/text
5. Agent calls stapler_browser_click(session, ref_B)   ← new ref
```

This pattern:
- Works today with existing tools.
- Is zero-cost to the daemon (no new code, no new dependencies).
- Adds one round-trip (snapshot + re-click) — ~100-500ms latency depending on page complexity.
- Requires the agent to handle the error, which LLM agents are capable of when the error message is clear.

The quality of the error message is the key lever here. If the `PortError::NotFound` message clearly says "element ref stale — call `stapler_browser_snapshot` to get fresh refs and retry," a capable LLM agent will do exactly that.

## Daemon-side improvement option: last-snapshot-cached re-resolve

A zero-LLM-dependency option not covered by the item's framing:

The daemon already returns an `AxSnapshot` after every click/type/navigate (the tool returns a fresh snapshot). It could additionally cache the last returned snapshot per session (already in-memory state). On a `NotFound` error in click/type:

1. Re-take a fresh snapshot automatically (one CDP round-trip).
2. Scan for a node matching the original ref's role+name (from cached prior snapshot).
3. If found, retry with the new ref.
4. If not found, return the original error + the new snapshot so the caller can re-plan.

**Cost**: one extra CDP round-trip on failure (no LLM call). Latency: ~50-200ms. No new dependencies.
**Risk**: role+name matching is imprecise — two elements with the same role and name will be ambiguous. This is not a full solution but handles the most common case (page re-render without content change).
**Implementation scope**: small — requires caching the last snapshot per session (already in-memory) and a simple AX tree scan on failure.

This option is not in the item's framing but is worth highlighting as a zero-dependency middle ground.

## Architecture assessment

The daemon's core design principle is captured in `README.md:164-175`: it is a "thin daemon" with the only AI-adjacent external dependencies being `BRAVE_API_KEY` (for web search) and `chromiumoxide`/`playwright-core` for browser automation. The daemon's "port" trait design (`crates/core/src/ports.rs`) models the external surface of the system; adding an `LlmClient` port would be a meaningful architectural expansion.

Practical implications of adding LLM calls to the daemon:
- **New credential class**: `ANTHROPIC_API_KEY` (or equivalent) must be present in the daemon's environment. The daemon currently manages one API key; adding another complicates the operational model for a local tool.
- **New failure modes**: LLM rate limits, network timeouts, and auth failures now surface as click/type errors. These are unpredictable and hard to diagnose.
- **Latency**: self-heal adds 200-1000ms per triggered call. For a local daemon, this is noticeable.
- **Cost**: low per-call but non-zero. For a local single-user tool, this is not a blocker, but it's a new concern.
- **Dual-adapter complexity**: both the native (Rust/reqwest) and wasm (Node/fetch) adapters would need an LLM client implementation — same two-sided build cost as any other port.

## Summary of findings

| Approach | LLM dep | Daemon change | Cost | Latency | Stale-ref | Element-gone |
|---|---|---|---|---|---|---|
| Agent-side re-snapshot+retry | No | None | Free | +1 round-trip | ✓ | ✗ |
| Daemon: last-snapshot re-resolve | No | Small | Free | +1 CDP call on fail | ✓ (common case) | ✗ |
| Stagehand-style LLM relocate | Yes | Large | ~$0.001/call | +200-1000ms on fail | ✓ | Partial |
| Skyvern-style vision-hybrid | Yes (vision) | Ground-up | High | Always higher | ✓ | Partial |

The item's explicit design constraint ("the daemon has no LLM dependency at all") maps directly to the first two rows. The last two rows both cross that constraint.

## Key sources

- `crates/core/src/tools/browser.rs:87-187` — current click/type implementation and error mapping.
- `crates/core/src/ports.rs` — port trait definitions; no LLM port exists.
- `README.md:164-175` — two-adapter architecture and external dependency inventory.
- Stagehand `actService.ts:371-431` — confirmed via code-archaeology cited in the item; not re-cloned in this pass.
- Skyvern, browser-use — README/source-level from the item's prior research pass; not cloned.
