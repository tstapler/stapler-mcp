# Requirements: ai-self-healing-fallback

**Date**: 2026-10-29
**Type**: evaluation / architecture decision
**Complexity**: 2 — bounded research question with a clear decision gate; no implementation unless evaluation resolves favorably

## Problem Statement

`stapler_browser_click` and `stapler_browser_type` fail hard when a `ref` from a prior `stapler_browser_snapshot` is stale — either because the page changed after the snapshot or the element disappeared. The current behavior is a `PortError::NotFound` with an actionable message, but with no automatic recovery. The caller (typically an LLM agent) must re-snapshot and retry manually. Several competing browser-automation tools (Stagehand, Skyvern, browser-use) bake a "self-healing" fallback directly into the action: on failure, automatically re-capture state and attempt to re-locate the element via an LLM call or vision-hybrid approach before propagating the error.

## Baseline

Today the ref lifecycle is: `stapler_browser_snapshot` → returns a tree of nodes with opaque `ref` ids → caller passes a `ref` to `stapler_browser_click` or `stapler_browser_type` → if the ref is no longer valid in the browser's AX tree, `PortError::NotFound` is returned with a message identifying the session. There is no automatic retry, no re-snapshot, and no LLM involvement. The daemon has **zero LLM dependency** and makes **no outbound API calls** beyond its tool-layer operations (Brave search, page fetch).

Stagehand's `selfHealAction()` (confirmed at `actService.ts:371-431`, gated by `context.selfHeal === true` in `takeDeterministicAction()`) re-captures a fresh snapshot and calls an LLM to re-locate the target element. Skyvern uses a vision+DOM hybrid to select elements, which is more resilient to DOM churn from the start. browser-use falls back to a full autonomous agent loop on failure.

## Evaluation Questions (this item's deliverable)

This item is an **evaluation**, not an implementation. The output is a decision document (or this plan.md) that answers:

1. **Is daemon-side self-healing worth doing at all**, given the "thin daemon, no AI dependency" design principle?
2. **Which approach fits better** — Stagehand-style re-snapshot+LLM-relocate on failure, or Skyvern-style vision-hybrid initial targeting?
3. **Does self-healing belong in the daemon**, or should it remain the calling agent's responsibility (re-snapshot + retry the tool call itself, with no daemon change)?
4. **Cost/latency/API-key implications** of the daemon calling an LLM directly (new dependency class: credentials, network, rate limits, per-call cost).

## Users / Consumers

LLM agents using `stapler_browser_click`/`stapler_browser_type` that encounter stale-ref errors because the page mutated between snapshot and action. The concrete failure mode: an agent snapshots, receives refs, calls click on a ref, but the page rerendered (SPA update, animation, async content load) and the AX tree node with that ref no longer exists.

## Success Metrics

This is an evaluation item; success is a documented, defensible decision, not a feature shipped:

- A decision document (plan.md) that answers all four evaluation questions above with supporting rationale.
- Evidence cited for architectural fit (or misfit) with the daemon's existing design.
- A recommendation with a concrete "if yes" path (approach + rough scope) and "if no" path (what to improve instead — error message fidelity, documented retry pattern, etc.).
- The recommendation is grounded in observed failure frequency or a principled proxy thereof (e.g., how often do LLM agents re-snapshot after a click error in practice?).

## Appetite

Small (evaluation only) — a few hours of research, architecture analysis, and documentation. The item is explicitly marked "later — future-looking; no implementation until evaluation resolves." This document and the resulting plan.md are the deliverable.

## Constraints

- Must not modify source code.
- Evaluation must respect the existing design principle: the daemon makes no LLM calls and has no AI dependency.
- Any "if yes" path must account for both adapters (native Rust + wasm/Node).
- API key management in the daemon is a non-trivial operational cost (it currently reads only `BRAVE_API_KEY` and `STAPLER_MCP_BROWSER_PROFILE_DIR`).

## Non-functional Requirements

- **No new latency** in the common (non-failing) path. Any self-healing must not add overhead to successful click/type calls.
- **No new external dependencies** unless the evaluation positively recommends daemon-side LLM integration.
- **Observability**: if a self-heal fires, it should be logged (session_id, original ref, action taken).

## Scope

### In Scope (evaluation)
- Analysis of Stagehand's `selfHealAction` mechanism (confirmed code-archaeology: `actService.ts:371-431`).
- Analysis of Skyvern's vision+DOM hybrid approach.
- Analysis of browser-use's agent-fallback pattern.
- Analysis of the agent-side re-snapshot+retry pattern (baseline: zero daemon change, caller responsibility).
- Architecture fit assessment against the daemon's "no LLM dependency" design.
- Cost/latency modeling for the daemon-side LLM path.
- Recommendation + "if yes" scoping + "if no" improvement list.

### Out of Scope (this item)
- Any implementation of the self-healing mechanism.
- Changes to existing tool behavior.
- Stagehand/Skyvern/browser-use code-archaeology beyond what is needed for the architectural comparison.

## Rabbit Holes

- **Vision-based targeting (Skyvern-style)** requires screenshots, bounding-box extraction, and a vision-capable LLM model — substantially more complexity than a text-only AX-tree relocator. This is a different problem from stale-ref recovery and may not be the right comparison.
- **Stale ref vs. element gone**: these are meaningfully different failure modes. Stale ref (element still exists, node ID changed) may be self-healable via re-snapshot. Element gone (user navigated, SPA unmounted the component) is not — no amount of LLM re-location will find something that isn't there. Self-healing is only useful for the first case.
- **Frequency unknown**: without telemetry data, it's unclear how often stale-ref errors actually occur in practice. If rarely, the complexity of daemon-side self-healing is not justified.

## Alternatives Considered

- **Do nothing** — callers already can re-snapshot and retry; the tools don't prevent this. The question is whether doing it in the daemon is worth the complexity.
- **Improve error messages** — make the `PortError::NotFound` message even more explicit about re-snapshot+retry as the recovery path. Zero complexity, immediate improvement.
- **Document the retry pattern** — add a note to the tool description (schema doc comment) explaining the re-snapshot+retry idiom. Zero complexity.
- **Daemon-side self-healing with LLM** — the Stagehand approach. Introduces new dependency class, operational complexity, and latency.

## Open Questions

- How often do stale-ref errors actually occur in real agent usage sessions? Without this data, sizing the problem is speculative.
- Is there a way to detect "stale ref" (AX node ID recycled or removed) vs. "element gone" (no equivalent element exists) in the CDP AX tree response, and surface that distinction in the error message?
- Could the daemon cache the last snapshot's AX tree per session and attempt a simple re-resolve (by role+name, not LLM) before failing? This would be a zero-LLM-dependency, daemon-internal optimization that avoids the AI dependency entirely.

## Acceptance Criteria (from backlog item 3dfa3377)

1. Decision document answers the four evaluation questions (this directory's `plan.md`, "Decision answers").
2. Stale-ref `PortError::NotFound` from click/type names `stapler_browser_snapshot` as the recovery action, distinct from session-not-found.
3. `refId` doc comments in `BrowserClickInput`/`BrowserTypeInput` describe the stale-ref failure and the re-snapshot+retry recovery.
4. **Conditional on Part 3 approval**: on `NotFound`, re-snapshot and retry if exactly one node has the same role+name; set `BrowserActionOutput.note`. If Part 3 is not approved, this criterion is not applicable (`/backlog/fail-3` with that reason, not silently passed).
5. No LLM calls and no new API keys in the daemon on any path.
6. `cargo test --workspace` and `cargo clippy --workspace --all-targets -- -D warnings` pass.

Note: "Must not modify source code" under Constraints applies to the evaluation phases (1-4); criteria 2-3 and 6 are the implementation phase.
