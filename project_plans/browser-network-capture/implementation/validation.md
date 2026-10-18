# Validation: browser-network-capture

**Date**: 2026-10-10
**Source**: `requirements.md`, `implementation/plan.md`, `implementation/adversarial-review.md`

## Validation against Success Metrics

| Metric | Plan Coverage | Status |
|---|---|---|
| `stapler_browser_network_requests` tool exists, registered on both daemons | Epic 3 Story 3.3 covers both CLI and wasm registration | COVERED |
| Returns ordered list with URL, method, status, content-type, body | Epic 1 Story 1.1 defines `NetworkEntry`; Epic 2 Story 2.3 covers accumulation | COVERED |
| `filter_url` parameter narrows results | Epic 2 Story 2.5 AC 2.5.3, Epic 3 Story 3.2 AC 3.2.2 | COVERED |
| `save_har` writes a HAR 1.2 document | Epic 3 Story 3.2 AC 3.2.3 | COVERED |
| Request list cleared on each `navigate` | Epic 2 Story 2.4 | COVERED |
| Native integration test verifying XHR capture | Epic 5 Story 5.1 | COVERED |
| `cargo test` and `cargo clippy` pass | Unit tests in Epic 2/3 + CI convention | COVERED |

## Validation against Constraints

| Constraint | Plan Treatment |
|---|---|
| Existing `BrowserDriver` methods unchanged | Plan is purely additive; no existing method signatures are modified |
| Tool-profile gating | Registered unconditionally with a documented note for future gating |
| Both adapters implement new method | Epic 2 (native) + Epic 4 (wasm) both required |
| Response body capture is best-effort | `body: None` for binary types and unavailable responses (pitfalls.md P2/P3 mitigations) |

## Adversarial review resolutions

All 5 challenges from `adversarial-review.md` are addressed:

1. **CDP event correlation**: `partial_entries` map added to `BrowserSession` (Epic 2
   Story 2.3) — this is the key design gap the adversarial review surfaced.
2. **HAR compliance**: Use `-1` for unknown `headersSize`/`bodySize` fields; documented.
3. **Tool-profile gating**: Registered unconditionally with explicit "candidate for
   network profile" note.
4. **Memory budget**: 320 MB worst-case is accepted; 16 KB default body cap documented
   in tool schema.
5. **`clear` semantics**: Copy-then-clear in single `borrow_mut` scope — atomic.

## Risk Register

| Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|
| CDP response body evicted before retrieval | Medium | Medium | Eager retrieval in `EventLoadingFinished` handler (pitfalls P1) |
| wasm `playwright-core` body retrieval mismatch | Medium | Medium | JS glue converts Buffer → UTF-8 string; try/catch for unavailable bodies (pitfalls P3) |
| Memory pressure from body accumulation | Low (with caps) | High without caps | 16 KB per-body cap + 1000 entry cap (pitfalls P4) |
| CDP Network domain subscription timing race | Low | Medium | Subscribe before first `goto` in session creation (pitfalls P2) |
| SSRF data leakage via network entries | Low | High | Return `NotFound` for blocked sessions (pitfalls P6) |

## Open Questions (pre-implementation decisions)

1. **Single combined event task vs. three separate tasks**: verify whether `chromiumoxide`
   supports a combined event stream that can be polled for all three `Network.*` event
   types, or whether three separate `spawn_local` tasks are required. The latter increases
   complexity of the `partial_entries` map access pattern.

2. **`max_body_bytes` hard ceiling**: should the tool enforce a server-side hard ceiling
   (e.g. 64 KB) even if the caller requests more? Recommended: yes — document that
   `max_body_bytes` is capped at 65 536 to prevent a caller from accidentally requesting
   gigabyte-sized body storage.

3. **wasm body retrieval method**: confirm `response.body()` vs `response.text()` vs
   `response.json()` for playwright-core's wasm-glue-accessible API surface. `text()` may
   be safer (always returns UTF-8 string) and avoid the Buffer→bytes conversion.

## Go/No-Go Assessment

**Go**. The plan is:
- Architecturally consistent with the existing codebase (same `Rc<RefCell<...>>` pattern,
  same port-extension model, same tool-layer conventions)
- Feasibly scoped (1–2 weeks: port types 0.5 day, native adapter 3–4 days, tool layer 1 day,
  wasm adapter 1–2 days, integration test 1 day)
- All adversarial challenges are either resolved or explicitly documented as acceptable
- No new external dependencies required (CDP Network domain already available in
  `chromiumoxide`; `playwright-core`'s event API is already used)

The one implementation-time decision that needs verification before writing code:
**how `chromiumoxide` exposes simultaneous subscription to multiple CDP event types
from a single async task** (Challenge 1 from adversarial review). Verify against
`chromiumoxide`'s actual API before committing to the three-task vs. single-task
event architecture.
