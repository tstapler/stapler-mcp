# Validation: ai-self-healing-fallback

**Date**: 2026-10-29
**Phase**: sdd:4-validate (adversarial review)

## Review Summary

The plan recommends against LLM-based daemon-side self-healing and instead proposes: (1) improved error messages, (2) schema doc comment improvements, and (3) a deferred zero-LLM re-resolve using cached last-snapshot. This adversarial pass challenges the plan's assumptions and stress-tests its conclusions.

---

## Challenge 1: "Agent-side retry already works" — is this actually true?

**Claim**: an LLM agent can already re-snapshot and retry without any daemon change.

**Stress test**: Does the current error message actually tell the agent what to do?

The plan acknowledges the error message needs improvement (Part 1). If the current message is not actionable, then the claim "it already works" is overstated — it works in principle but fails in practice when the agent doesn't recognize the recovery path. The plan's own Part 1 is an implicit admission that the baseline is insufficient.

**Verdict**: The claim holds in principle but the plan correctly identifies improving error messages as the highest-priority action. The risk is underestimating how much the quality of the error message matters in practice. Mitigation: Part 1 should be framed as a bug fix (poor error message) rather than a nice-to-have.

---

## Challenge 2: The zero-LLM re-resolve (Part 3) is optimistic about role+name ambiguity

**Claim**: the daemon can re-resolve a stale ref by finding an unambiguous role+name match in a fresh snapshot.

**Stress test**: Many interactive pages have multiple elements with the same role and name.
- Multiple "Submit" buttons (primary, secondary, form-level, modal-level).
- Multiple checkboxes labeled "Agree" or "Enable".
- Multiple links with the same anchor text ("Learn more", "View details").

If the re-resolve fires and picks the wrong "Submit" button, it silently submits the wrong form — a worse outcome than an error. The plan's "only fire if unambiguous" mitigation is correct but means the re-resolve will fail to fire in many real-world cases, reducing its practical value.

**Verdict**: The ambiguity concern is real and the plan's mitigation is sound but narrows the feature's applicability significantly. The plan should note that role+name re-resolve has low coverage on pages with repeated element names — which are exactly the pages (complex SPAs, multi-form flows) where stale-ref errors are most likely to occur. Consider whether Part 3 is worth the implementation cost given this narrowed applicability.

---

## Challenge 3: The plan doesn't quantify the frequency of stale-ref failures

**Claim**: the recommendation is appropriate given the failure frequency is unknown.

**Stress test**: If stale-ref failures are common (e.g., every SPA-driven click is potentially stale), then the plan's "agent-side retry" recommendation adds one full round-trip to every common workflow — a material latency cost. The plan frames this as acceptable without evidence.

**Verdict**: Valid gap. The plan's deferral condition ("defer Part 3 until evidence of frequency") is the right call, but the plan should also note: if frequency turns out to be high, daemon-side re-resolve becomes more attractive, and the LLM-based approach becomes worth reconsidering even at the cost of adding an AI dependency. The current plan is correct for the current (unknown) frequency, but should not be treated as permanent.

---

## Challenge 4: The Stagehand comparison omits the "description" dimension

**Claim**: Stagehand's LLM-based self-heal requires a natural-language description field that doesn't exist in `BrowserClickInput`.

**Stress test**: Is this actually a blocker, or could the daemon use the existing AX tree context (the last snapshot) as the description?

The last snapshot already provides rich context: role, name, value, children. An LLM prompted with "find the element that best matches: role=button, name=Submit, previously at AX ref ABC, in this current accessibility tree" doesn't need a human-authored natural-language description — it can use the structural AX context. The plan's framing that a description field is required is potentially overconstrained.

**Verdict**: The plan overstates the implementation gap for LLM-based re-resolve. However, the conclusion (don't add LLM dependency to the daemon) remains correct for the reasons stated elsewhere (operational complexity, credential management, new failure modes). This challenge weakens one of the argument's supporting claims without changing the recommendation.

---

## Challenge 5: Part 1 (error message) and Part 2 (schema doc) may not reach the LLM

**Claim**: improving error messages and tool schema descriptions will help LLM agents recover.

**Stress test**: The `ref_id` field doc comment is in `crates/core/src/schema.rs` and served as part of `tools/list` JSON schema. Whether LLM agents read the full schema description for each parameter before each call is model-dependent. Similarly, error messages are returned in the tool result, which the agent should read — but a poorly-prompted agent might not surface this to the model's context.

**Verdict**: This concern is real but outside the daemon's control. Better error messages and schema descriptions are still the right improvement — they are necessary, even if not sufficient. The plan is correct.

---

## Challenge 6: Two-adapter cost is understated for Part 3

**Claim**: wasm adapter equivalent of last-snapshot re-resolve is "2h."

**Stress test**: The wasm adapter's browser integration (`crates/wasm/src/browser.rs`) is JS glue on top of `playwright-core`. The re-resolve logic requires caching a Rust `AxSnapshot` struct across calls in a WASM context. Rust structs with `Option<>` fields need either JS serialization/deserialization or careful lifetime management via `wasm_bindgen`. This is non-trivial: it's the same category of wasm/native behavioral parity complexity flagged as a rabbit hole in the `browser-automation` requirements.

**Verdict**: The 2h estimate for Story 2.5 is likely underestimated. The precedent from the browser-automation phase (the wasm full-round-trip test was deliberately descoped as "materially more infrastructure") suggests the wasm equivalent of daemon re-resolve could take 4-8h, not 2h. Risk: flag this as a sizing risk before committing.

---

## Overall Verdict

The plan's core recommendation is sound: do not add LLM calls to the daemon, improve error messages, defer the zero-LLM re-resolve until frequency data is available.

**Confirmed strengths**:
- Correctly identifies the agent-side retry as the right first-line response.
- Correctly identifies the "no LLM dependency" constraint as non-negotiable for this item.
- Part 1 + Part 2 are zero-risk, zero-dependency improvements with clear value.
- The deferral condition for Part 3 is correct.

**Required plan adjustments before implementation**:
1. Frame Part 1 (error message) as a bug fix, not a nice-to-have — the current message does not meet the "actionable" standard the codebase claims for it.
2. Add a note in Part 3 that role+name re-resolve has limited coverage on pages with repeated element names — the most common real-world failure sites.
3. Revise Story 2.5's estimate to 4-8h and add it to the "wasm parity rabbit hole" risk list.
4. Add a plan revision condition: "If stale-ref failure rate proves high in practice, revisit the LLM-based approach with explicit appetite sizing."

**No blocking issues.** Parts 1 and 2 can proceed immediately without further validation. Part 3 remains correctly deferred.

## Addendum (code-path verification)

Challenge 1 is resolved by reading the code: the stale-ref message is already actionable (see plan.md Part 1). Required adjustment 1 ("treat Part 1 as a bug fix") is withdrawn. AC4 is conditional on Part 3 approval; the plan defers Part 3, so AC4 is reported as not applicable, with the reason. Gate: PASS for Parts 1-2 (doc-only changes) and the decision document.
