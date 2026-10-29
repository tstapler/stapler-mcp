# Adversarial Review: oopif-ax-snapshot

## Reviewer perspective

Independent review of the plan.md for failure modes, hidden assumptions, and scope risks.

---

## Finding 1: Task 0.1 is under-resourced and gates everything

**Claim in plan.md**: Task 0.1 "probe chromiumoxide child-session API" is estimated at 4h.

**Challenge**: chromiumoxide's internal CDP handling is complex. The `Browser` type in chromiumoxide 0.9 is built around a `Handler` (chromiumoxide's own CDP protocol driver, not the standard Chrome DevTools Protocol client). Wrapping an existing target's `SessionId` as a `Page` requires either (a) private access to chromiumoxide internals or (b) submitting a PR upstream. This is not a 4-hour probe — if the API doesn't exist, the decision is to fork chromiumoxide, vendor a patch, or switch to raw CDP. Each of those paths is a week+ of work.

**Risk**: If Task 0.1 reveals no viable chromiumoxide API, the entire epic collapses. The plan should explicitly bound the 0.1 work: "if no API found within 8h of investigation, document findings and punt this item back to backlog."

**Mitigation added**: Plan already has a Go/No-Go gate. Strengthen it: add a 8h time-box to Task 0.1 explicitly.

---

## Finding 2: TargetId discovery is not specified

**Claim in plan.md**: "Finding the `TargetId` for the child frame — either via `Target.getTargets` filtered by `frameId`, or from the CDP `Page.frameAttached` event."

**Challenge**: Neither of these is reliable:
- `Target.getTargets` returns all open targets but does not filter by `frameId` in the CDP response (it returns `TargetInfo` structs that contain a `url` and `type` but not `frameId` in all Chrome versions).
- `Page.frameAttached` fires when the frame is created but provides a `FrameId` → parent `FrameId` relationship, not a `TargetId`.

The actual reliable way to correlate a `FrameId` to a `TargetId` is via `Target.setAutoAttach{autoAttach: true, waitForDebuggerOnStart: false, flatten: true}` which causes CDP to emit `Target.attachedToTarget` events for every new rendere process. This is how Playwright and Puppeteer do it. It requires setting up the auto-attach infrastructure at session creation time (in `spawn_session_listeners` in `browser.rs`), not at snapshot time.

**Risk**: The plan as written may be unable to find the `TargetId` for a given `FrameId` at snapshot time, making Epic 1 Story 1.1 Task 1.1.1a unachievable with the stated approach.

**Mitigation**: Add a new task before 1.1.1 to investigate TargetId discovery: (a) verify `Target.getTargets` response includes `frameId` for OOPIFs in Chrome 130+, or (b) add `Target.setAutoAttach` registration to `spawn_session_listeners` and build a `FrameId → TargetId` map there. This is additional scope (~4h) but is a prerequisite.

---

## Finding 3: Snapshot-only first pass leaves a user-hostile UX gap

**Claim in plan.md**: "scope to snapshot-only — surface OOPIF content in the snapshot... defer the click/type routing to a follow-on."

**Challenge**: For the primary use cases (Stripe payment forms, OAuth widgets), the ability to *see* the form is useless if you can't type into it. An LLM agent that gets a snapshot showing a payment form's fields but gets an error when trying to type a credit card number is worse than the current behavior (where the OOPIF is invisible and the agent knows to try a different approach). A partial implementation may create more confusion than no implementation.

**Mitigation**: This is a real concern but the backlog item explicitly rates this as "later — evaluate demand." The snapshot-only first pass is appropriate as a discovery step: if OOPIF content appears in snapshots and callers try to click/type into it, that generates concrete demand signal for the follow-on. Document the limitation clearly in the snapshot output format (e.g., OOPIF ref strings could be prefixed `iframe:` to signal they're snapshot-only).

---

## Finding 4: Redaction fail-safe for OOPIF is weaker than claimed

**Claim in plan.md**: "Skip the CDP probe and unconditionally apply REDACTED_PLACEHOLDER for form-control roles."

**Challenge**: `is_form_control_role` in `ax.rs` only covers `textbox | searchbox | combobox`. An OOPIF Stripe payment form might render a `<input type="password">` that Chrome's AX layer gives role `textbox` — which is covered. But what about a `<input type="text" autocomplete="cc-number">` (credit card number)? That also gets role `textbox`, so it IS covered. But an `<input type="tel">` for a phone number gets role `textbox` too — and that would be unconditionally redacted by the fail-safe even though it's not sensitive. This is too aggressive for the OOPIF case.

**Challenge 2**: The fail-safe (unconditional redaction for all OOPIF form controls) will redact non-sensitive search boxes inside iframes. This is a known over-redaction trade-off, justified only because we can't probe the DOM without a working child session.

**Verdict**: The redaction behavior is still correct (fail-safe → over-redact rather than leak), but the plan should document this trade-off explicitly. When click/type-into-OOPIF ships (the follow-on), the redaction can be tightened to use the child session for probing.

---

## Finding 5: Integration test portability is genuinely hard

**Claim in pitfalls.md §6**: "Use `127.0.0.1` vs. `::1` since Chrome treats those as separate security origins."

**Challenge**: Chrome's site-isolation policy is determined by the "site" (scheme + registerable domain), not the IP address. `http://127.0.0.1:8080` and `http://[::1]:8080` have different hosts but both resolve to localhost; Chrome's `kSandboxedRendererProcessLimit` logic and OOPIF triggering is based on the registered domain (both are `localhost` effectively). In practice, Chrome often does NOT create a separate renderer process for `127.0.0.1` vs `[::1]` iframes on localhost.

**Better approach**: Use Chrome's `--site-per-process` flag (already the default for non-localhost origins) combined with two distinct local servers bound to different hostnames via `/etc/hosts` entries (`inner.localhost` vs `outer.localhost`) — both resolve to 127.0.0.1 but Chrome treats them as different sites for OOPIF purposes.

**Mitigation**: Update the test to use `inner.localhost` and `outer.localhost` with `/etc/hosts` entries, or use the `--disable-web-security` Chrome flag in test-only mode to force a controlled test environment. Document the exact Chrome launch flags needed in the test's `#[ignore]` comment.

---

## Overall verdict

The plan is structurally sound but has two critical uncertainties that should be resolved before committing implementation work:

1. **Task 0.1 blocker**: chromiumoxide's child-session API (or lack thereof) is the single highest-risk unknown. Time-box to 8h with an explicit punt-back decision.
2. **TargetId discovery**: the plan's proposed mechanism may not work; `Target.setAutoAttach` is likely required, adding scope to `browser.rs`'s session setup code.

The snapshot-only first pass is the right scope for an initial implementation; the redaction trade-off is acceptable and well-reasoned.
