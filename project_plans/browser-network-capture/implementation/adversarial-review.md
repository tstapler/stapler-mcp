# Adversarial Review: browser-network-capture

**Date**: 2026-10-10
**Reviewer**: pre-implementation triage (adversarial pass)

## Challenge 1: CDP event subscription model — will this actually work with `chromiumoxide`'s event architecture?

**Challenge**: The plan calls for subscribing to `EventRequestWillBeSent`,
`EventResponseReceived`, and `EventLoadingFinished` on session creation, correlating
events by `request_id`. But `chromiumoxide`'s event model fires events on a per-page
event listener, and the existing `browser.rs` uses these via `page.event_listener::<Event>()`.
Can three *separate* event listeners (one per event type) share mutable access to
`BrowserSession`'s `network_entries: Vec<NetworkEntry>` without a `Mutex`?

**Assessment**: The existing code handles `EventFrameNavigated` and `EventTargetCrashed`
as separate `spawn_local` tasks that mutate `BrowserSession` via the `SessionRegistry`'s
`Rc<RefCell<...>>`. The same pattern works here — each event handler task holds an
`Rc::clone(&session_registry)` and calls `borrow_mut()` inside the event loop body,
never holding the borrow across an `.await`. This is exactly the "never hold a borrow
across `.await`" discipline already documented for `SourceLocks`.

**However**, there is a genuine correlation problem: three separate event streams
(`RequestWillBeSent`, `ResponseReceived`, `LoadingFinished`) must be correlated by
`request_id`. Doing this across three separate `spawn_local` tasks with a shared
`HashMap<String, PartialEntry>` in `BrowserSession` adds meaningful complexity. An
alternative is a single `spawn_local` task subscribing to all three event types via
`chromiumoxide`'s combined event enum — check whether `chromiumoxide` supports that.

**Risk level**: MEDIUM. The correlation logic is not trivial; the plan should explicitly
call out that a `HashMap<request_id, PartialNetworkEntry>` needs to live in `BrowserSession`
alongside `network_entries`, and that the three-event handlers must be a single task (or
at minimum, the partial-entry map must use the same `Rc<RefCell<...>>` borrow discipline).

**Verdict**: The architecture is feasible but the plan underspecifies the event correlation
design. Update Epic 2 Story 2.3 to specify the partial-entry map explicitly.

---

## Challenge 2: Memory budget arithmetic is wrong

**Challenge**: The plan says "default `max_body_bytes = 16 KB`; worst-case per-session
memory 16 MB (1000 × 16 KB); total across 20 sessions 320 MB." This arithmetic treats
every entry as having a full 16 KB body, which is pessimistic but mathematically correct.
However, the plan also says binary content types skip body storage. On a typical page load,
~50–70% of requests are for images, fonts, CSS, and scripts. In practice, JSON API
responses that an agent actually cares about are a small fraction. The concern is about
whether 320 MB is acceptable for a local single-user daemon.

**Assessment**: 320 MB worst case is within the reasonable range for a desktop-class machine
(typical modern development machine has 16+ GB RAM). More importantly, in practice the
daemon serves one user's interactive session at a time; all 20 sessions being simultaneously
active with all 1000 entries at full body cap is an unrealistic worst case.

**Verdict**: No change required. The plan's arithmetic and mitigation are correct. Add a
note that the 16 KB default should be documented in the tool's schema description so
callers aren't surprised.

---

## Challenge 3: HAR 1.2 compliance — the plan omits required HAR fields

**Challenge**: HAR 1.2 requires `entry.request` to include `httpVersion`, `cookies`,
`headers`, `queryString`, `bodySize`, and `headersSize`. The plan's schema captures
none of these from CDP events. An HAR file missing required fields is technically
non-compliant and may fail to load in devtools or analysis tools.

**Assessment**: The `request.headers` and `response.headers` are available from
`EventRequestWillBeSent` and `EventResponseReceived` respectively. Not capturing them
means the HAR is degraded but still loadable in most tools (they treat missing/empty
arrays as no headers, not an error). Capturing full headers would expand the port types
significantly and increase memory usage.

**Recommended resolution**: Set `headersSize: -1` and `bodySize: -1` (HAR spec's "unknown"
values) for compliance without requiring full header capture. Document in the tool schema
description that the exported HAR has limited header data. This is the approach used by
many real-world HAR exporters that don't have access to raw HTTP bytes.

**Verdict**: No architectural change needed; add explicit notes in plan about using -1
for unknown HAR fields.

---

## Challenge 4: Tool-profile gating — the plan defers this but the item explicitly asks for it

**Challenge**: The item description says the tool should be "gated behind the same
tool-profile mechanism proposed for selectable MCP tool profiles so it doesn't bloat
the default tool schema." The plan acknowledges this as out-of-scope. But if no gating
mechanism exists, the tool lands in the default schema unconditionally, which the item
explicitly wants to avoid.

**Assessment**: The browser-automation feature shipped without a tool-profile gating
mechanism (all tools are registered unconditionally). There is no existing mechanism
to gate against. The item acknowledges this is future work. The plan's decision to
register unconditionally with a note is pragmatically correct given the current
codebase state.

**Verdict**: Acceptable. The plan should explicitly document: "register unconditionally
for now; when tool-profile gating lands, this tool is a candidate for the 'network'
profile (not the default profile)."

---

## Challenge 5: `clear: true` semantics race — clear after copy or atomic?

**Challenge**: The plan says "`clear: true` clears the list *and* returns the entries
that were in it." In a single-threaded `current_thread` runtime with `Rc<RefCell<...>>`,
there is no actual race (the borrow_mut is synchronous, no `.await` between copy and
clear). But the spec needs to be explicit: does `clear` return the entries *before* the
clear, or is it "query and forget"?

**Assessment**: "Return entries then clear" is the more useful semantic (the caller gets
to see what was there). Since the runtime is single-threaded and `borrow_mut` is held
for the duration of the copy-and-clear operation (no `.await` inside), this is atomic
from the caller's perspective.

**Verdict**: Plan is correct; add an explicit note in AC 2.5.5 confirming "copy entries
to return value, then clear the list — both within the same `borrow_mut` scope."

---

## Summary of Required Plan Updates

1. Epic 2 Story 2.3: Specify `partial_entries: HashMap<String, PartialNetworkEntry>` in
   `BrowserSession` for correlating the three CDP event streams by `request_id`. This
   map is separate from `network_entries` (completed entries) and `network_truncated`.
2. Epic 3 Story 3.1/3.2: Add explicit documentation note about `-1` values for unknown
   HAR fields.
3. Epic 3 Story 3.3: Add explicit comment in plan: "register unconditionally for now;
   candidate for 'network' tool profile when that mechanism exists."
4. Epic 2 Story 2.5 AC 2.5.5: Confirm copy-then-clear is atomic (same `borrow_mut` scope).
