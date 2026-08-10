# Adversarial Review: browser-automation (Round 2)
**Date**: 2026-08-06
**Verdict**: CONCERNS

## Blocker Re-check

1. Tool naming — FIXED — Task 5.2.1 (plan.md:709-724) now registers `"stapler_browser_navigate"`/`"stapler_browser_click"`/`"stapler_browser_type"`/`"stapler_browser_snapshot"` on the daemon, explicitly citing the `stapler_index_docs` precedent; Task 5.3.1/5.3.2 (plan.md:727-740) mirror the same wire-level names for wasm and assert `list_tools_json()` contains exactly those 4 names. Matches requirements.md line 17 verbatim.

2. Tab-crash detection — FIXED — New Story 2.5 (plan.md:413-458): `Target.targetCrashed`/`TargetDestroyed` listener sets a `crashed: Cell<bool>` flag (Task 2.5.1), every `BrowserDriver` method checks it and returns the new dedicated `PortError::SessionCrashed` variant, evicting the entry (Task 2.5.2), and the wasm side wires `page.on('crash', ...)` with a matching marker-string convention (Task 2.5.3). `PortError::SessionCrashed(String)` is added in Task 1.1.3 (plan.md:282-294) as a distinct variant from `NotFound`, addressing the "confusing raw CDP error" failure mode the original blocker named.

3. Lazy AX-tree — FIXED — Task 3.2.1 (plan.md:497-526) now explicitly waits for the page's load event after `goto` before calling `ax::capture_snapshot`, and if the returned root has zero children, issues one bounded priming retry after a 100ms backoff before returning. A new "real page, not synthetic" AC is added that exercises this against Story 6.2's real mock test page and explicitly states it's "addressing the gap adversarial-review.md flagged in Success Metric #4's coverage" (plan.md:521-526). Task 3.1.1 also gained a doc-comment requirement stating the lazy-tree caveat for callers (plan.md:471-478).

4. Reaper TOCTOU ordering — FIXED — Task 2.2.1 (plan.md:351-369) is rewritten to remove-then-close: the expired entry is removed from the map in a synchronous, `.await`-free critical section *before* `page.close()` is awaited on the now-exclusively-owned entry, with a new AC that explicitly asserts the map is empty "before `page.close()`'s `.await` resolves." Story 2.3 (Tasks 2.3.1/2.3.2, plan.md:381-398) is consistent with this ordering (last_used bump synchronous at call entry; lookup and removal share the same no-`.await` critical-section discipline). The Risk Control table row (plan.md:178) now describes the same remove-before-close ordering instead of the previously contradictory claim. No remaining contradiction between the two stories.

5. SSRF guard scope honesty — FIXED — The Risk Control table row (plan.md:180) now states plainly that `FrameNavigatedGuard` "does **not** prevent the initial request from going out ... and does **not** cover subresource requests ... an accepted, explicitly out-of-scope gap for this pass," pointing to a new Unresolved Question 4 (plan.md:201-215) that spells out the `169.254.169.254`/hidden-`<img>`/`fetch()` scenario in full and names `Fetch`-domain request interception as the fast-follow. This replaces the prior one-line "closes the gap" overclaim with an explicit, honest limitation statement — matching the `a15ed28` documented-limitation precedent the original blocker asked for.

## Remaining Blockers (if any)

None of the original 5 blockers survive. No new blocker-level issues found in the diffed areas.

## Remaining Concerns

- [ ] **wasm-side test-harness gap still unresolved.** Epic 6 (plan.md:742-798) still only covers `crates/core`/`crates/native` unit tests (Story 6.1) and one native-only `#[ignore]`d integration test (Story 6.2); no story stands up a JS/Node test harness, yet Tasks 4.1.1, 4.1.2, 4.2.1, 4.2.2, and now 2.5.3 all write ACs phrased as "in a Node test harness" (e.g. plan.md:616, 630, 644, 455) as if one exists. This is the same gap the previous review flagged, now slightly larger in surface area since Task 2.5.3 was added with the same unresolved assumption. — Add a concrete Epic 6 story creating a minimal JS/Node test runner before these ACs are actually executable.
- [ ] **Success Metric #3 (idle reaping) still only verified at the reaper's internal scan level, not via the public session lifecycle.** Task 6.1.3 (plan.md:766-772) still asserts only that "the session is removed" from the map; no task calls a subsequent `browser_navigate`/`click`/`snapshot` against the reaped `session_id` and asserts it surfaces `PortError::NotFound` end-to-end, which is how Success Metric #3 itself is phrased ("confirms the session is gone"). Task 2.5.2's crash-eviction AC does exercise this end-to-end pattern (plan.md:443-446) for the crash path — the same treatment was not extended to the reaper path.
- [ ] **No explicit final-gate task for Success Metric #5** (`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings`). Confirmed via search: neither string appears anywhere in plan.md. Every other Success Metric traces to a task; this one still has no corresponding Phase 6 checklist item.
- [ ] **Click/type-triggered navigation still has no designed handling beyond the SSRF guard.** Story 3.3 (Tasks 3.3.1/3.3.2, plan.md:530-546) is materially unchanged from the prior round — no policy for "execution context destroyed" when a click/type itself triggers navigation; the integration test (6.2.2) still only exercises the same-page, no-navigation case.
- [ ] **Mid-lifetime reaper panic still has no detection**, only the shutdown-time `JoinHandle` check (Story 2.4, unchanged; Observability Plan line 169-171 still only checks the handle "at shutdown"). Not recorded as an accepted risk in Unresolved Questions either — still just absent.

## Minors

- Task 3.4.1 (plan.md:555-559) still only changes visibility to `pub`; no doc comment carries `blocked_host_reason`'s documented limitation ("pre-fetch literal check only... doesn't catch DNS rebinding or a redirect followed internally") to the new native-adapter callers.
- `AxNode`/`AxSnapshot` truncation is still only a boolean `truncated` flag with no named max-depth/max-node cap constant (unlike `SESSION_IDLE_TIMEOUT`'s concrete `Duration::from_secs(300)`) — Unresolved Question 3 (plan.md:196-198) still just says "depth/node-capped" without a number.
- Task 3.3.1's click-dispatch mechanism (plan.md:530-534) is still left as an either/or ("`page.find_element`/`Input.dispatchMouseEvent` or chromiumoxide's element-click helper") — unresolved, unlike every other genuinely open design question in Step 3.

## Note: incidental resolution of prior concerns

Of the 5 previously-listed concerns and 3 minors, only two saw any incidental movement:
- The "reaped session end-to-end lookup" concern got a partial precedent set by Task 2.5.2's crash-path AC (which does verify `PortError::NotFound` on a subsequent call after eviction) — but this pattern wasn't carried over to the reaper's own test (Task 6.1.3), so the concern remains open for the reaper path specifically.
- No other concern or minor from the previous round was touched by this edit pass; all are restated above in their original form.
