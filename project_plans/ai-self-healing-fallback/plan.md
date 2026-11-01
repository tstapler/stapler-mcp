# Plan: ai-self-healing-fallback

**Date**: 2026-10-29
**Phase**: sdd:3-plan

## Decision

**Do NOT add LLM calls to the daemon.** The daemon's "no AI dependency" design is not accidental — it is a core property enabling predictable, low-cost local operation. Adding an LLM call in the failure path of `stapler_browser_click`/`stapler_browser_type` violates this property for marginal gain over what the calling agent can already do.

**Self-healing belongs at the agent layer, not the daemon layer.** The calling LLM agent is already in the best position to handle stale-ref failures: it has context about what it was trying to accomplish, can decide whether re-snapshot+retry makes sense or whether a different strategy is needed, and can absorb the extra round-trip without surprising the user.

## Recommendation: Two doc-only changes now, one deferred option, no LLM dependency

### Part 1 — Stale-ref error messages (AC2): already satisfied; only the `NotFound` doc comment changes

Click/type resolve refs via `resolve_locator_impl` (`crates/native/src/browser.rs`) and `verify_node_live` (same file). Both return `PortError::NotFound` — the same variant as session-not-found (`not_found_message`, same file). The stale-ref messages already say "call stapler_browser_snapshot for current refs" and are distinct from the session message ("start a new session"); existing tests assert this (`resolve_locator_should_return_stale_ref_message_when_ref_issued_before_navigation`, `ux_ac3_locator_not_found_error_should_name_ref_and_page_url_and_corrective_call`). `map_error` (`crates/core/src/tools/browser.rs`) passes the message through unchanged. The one fix: the `PortError::NotFound` doc comment in `crates/core/src/ports.rs` said "always start a new session", which is wrong for stale refs; it now covers both cases (doc-only).

### Part 2 — Recovery hint on `refId` schema docs (AC3), done in `crates/core/src/schema.rs`

The `stapler_browser_click` and `stapler_browser_type` tool schema descriptions (doc comments in `crates/core/src/schema.rs`) should note:

> "If `refId` is stale (the page re-rendered since the snapshot), this call returns an error; call `stapler_browser_snapshot` to get a fresh accessibility tree and retry with the new ref."

This surfaces the retry pattern to the LLM before it even makes the first call — not just when it fails. Zero implementation cost.

### Part 3 — Daemon-side last-snapshot re-resolve (NOT approved; deferred; AC4 applies only if approved)

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

This is a "nice-to-have" for the deferred backlog. Role+name matching has limited coverage on pages with repeated element names (several "Submit" buttons, repeated "Learn more" links), which are the pages where stale refs are most likely.

## Decision answers (from requirements.md evaluation questions)

**Q1: Is daemon-side self-healing worth doing at all?**
No, for LLM-based self-healing. Not now, for the zero-LLM last-snapshot re-resolve (Part 3): revisit once real usage shows how often callers hit stale refs. Until then, the agent-side retry (already possible, since the stale-ref error names `stapler_browser_snapshot`) plus the Part 2 schema docs is the recovery path.

**Q2: Which approach fits better — Stagehand or Skyvern?**
Neither, for the daemon. Stagehand's re-snapshot+LLM-relocate requires an LLM dependency that violates the daemon's design. Skyvern's vision-hybrid is a different product architecture entirely and doesn't map to incremental improvement. The zero-LLM analog of Stagehand (Part 3) is the closest viable option.

**Q3: Does this belong in the daemon or the calling agent?**
The calling agent, for LLM-based recovery. The daemon, for the zero-LLM re-resolve (Part 3) if that scope is approved.

**Q4: Cost/latency/API-key implications of daemon calling an LLM directly?**
New API key class in the daemon environment, ~$0.001/call (low but non-zero), +200-1000ms latency on every triggered failure, new failure modes (rate limit, auth, network). Not justified given that agent-side retry achieves the same result.

## Implementation roadmap (if approved)

### Epic 1 — Doc-only changes (done)

**Story 1.1** — `PortError::NotFound` doc comment covers stale refs (`crates/core/src/ports.rs`). Cost: XS.

**Story 1.2** — `BrowserClickInput.ref_id` and `BrowserTypeInput.ref_id` docs describe the stale-ref failure and the re-snapshot+retry pattern (`crates/core/src/schema.rs`). Cost: XS. Other ref-taking inputs (`BrowserTypeSecretInput`, `BrowserHoverInput`, `BrowserSelectOptionInput`, `BrowserFormField`, `BrowserSetCheckedInput`) keep the bare wording; extending them is a follow-up outside this item's ACs.

Known gap: the wasm adapter's stale-ref error (`crates/wasm/src/glue/browser.js`, `describeActionError`) does not name `stapler_browser_snapshot`, so the schema docs say "not-found error" rather than promising the tool name. Adding the hint there is a follow-up.

### Epic 2 — Zero-LLM daemon re-resolve (medium, defer)

**Story 2.1** — Cache last AxSnapshot per session in `NativeBrowser`  
Add `last_snapshot: Option<AxSnapshot>` to the session struct; update on every successful snapshot return.
- `crates/native/src/browser.rs` — session struct + update sites.
- Cost: S.

**Story 2.2** — Re-resolve by role+name on locator NotFound  
On `PortError::NotFound` in `click`/`type_text`, check last snapshot for an unambiguous role+name match, re-try CDP dispatch with new ref.
- `crates/native/src/browser.rs` — click/type_text implementations.
- Cost: M.

**Story 2.3** — Surface `note` field on self-heal  
Add "self-heal fired: re-resolved '{role}/{name}' to new ref" to the `BrowserActionOutput.note` field.
- `crates/core/src/tools/browser.rs` — `browser_click`/`browser_type` note construction.
- Cost: S.

**Story 2.4** — Test coverage for re-resolve  
Integration test: navigate, snapshot, force AX tree rebuild (navigate same URL), click with stale ref — expect success + note. Unit test: role+name scan on a fake AX tree with ambiguous + unambiguous cases.
- `crates/cli/tests/browser_session.rs` — new `#[ignore]` real-browser test.
- Cost: S.

**Story 2.5** — Wasm adapter equivalent  
`crates/wasm/src/browser.rs` — same last-snapshot cache + re-resolve logic in JS glue.
- Cost: L; wasm parity has been the costly part of earlier browser work, so size it separately.

## Locator format note

Refs are opaque CDP AX backend node IDs, consistent with `playwright-mcp`'s model and unchanged by this work. Part 3's re-resolve uses role+name as a secondary resolution key — a fallback only, not a replacement for the ref contract.

## What is NOT planned

- LLM calls in the daemon (any path).
- Vision-hybrid targeting (Skyvern-style).
- Integration with any external LLM API (Anthropic or otherwise) from the daemon layer.
- Any change to the snapshot+ref caller contract — this is purely additive.

## Deferral condition

Parts 1 and 2 (the `NotFound` doc comment and the `refId` schema docs) are doc-only and ship now. Part 3 (daemon re-resolve) should be deferred until there is evidence (from real agent usage) that stale-ref errors are frequent enough to justify the implementation cost. The key signal: how often do callers see `PortError::NotFound` on `click`/`type` after a successful navigate/snapshot in the same session?
