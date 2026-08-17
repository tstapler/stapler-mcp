# ADR-0002: Native accessibility-tree assembly and cross-adapter element-locator design

**Status**: Accepted
**Date**: 2026-07-17
**Deciders**: Tyler Stapler (solo project)
**Related**: `research/stack.md` §1, `research/build-vs-buy.md` (re-scoped AX-tree finding),
`research/architecture.md` §4.2, ADR-0001, `plan.md` Phase 2

## Context

`browser_snapshot`/`browser_find`/every element-targeting tool (`click`, `type`, `hover`,
`select_option`, `drag`, `drop`, `file_upload`) needs a way to (1) show the caller a
role/name-based view of the page, and (2) let the caller point back at a specific element in a
later call. `requirements.md` originally flagged this as the single largest open risk, on the
assumption `chromiumoxide` would need a from-scratch AccName-style computation to match
`playwright-core`'s `ariaSnapshot`. `build-vs-buy.md`'s research re-scoped this during Phase 2:
CDP's `Accessibility` domain already returns Chromium's own *computed* `role`/`name` per node
(`AXNode.role`, `AXNode.name`) — the real remaining work is tree assembly (flat `Vec<AxNode>` →
real tree via `childIds`), `ignored`-node filtering, output formatting, and a role+name(+index) →
live-element resolution scheme with disambiguation. Separately, `playwright-core`'s own internal
`aria-ref` selector engine (used by upstream `playwright-mcp` to resolve `[ref=eN]` back to a live
element) is explicitly flagged by research as undocumented and unsafe to depend on — the public,
documented equivalent is `page.getByRole(role, {name})`.

Three sub-decisions had to be made together: (1) what does `browser_snapshot`'s output actually
look like, on the wire, for both adapters; (2) how does a caller's later `target` reference
resolve back to a live element, on both adapters, without depending on either side's private
internals; (3) where does the ref-to-element mapping live — adapter-owned or core-owned.

## Decision

1. **Shared textual snapshot format (`AxSnapshotText`)**: both adapters produce (and
   `browser_snapshot`/`browser_find` return) the same indented text-tree shape:
   `- role "name" [ref=eN]` per line, nested by indentation, one line per accessible node. Native
   builds this by walking `Accessibility.getFullAXTree()`'s flat node list into a real tree via
   `childIds`, filtering `ignored: true` nodes, and formatting. Wasm gets this almost verbatim from
   `page.ariaSnapshot({mode: "ai"})`, which already emits this exact `[ref=eN]`-annotated shape —
   only light reformatting (if any) is needed to match native's exact output grammar.
2. **`ElementLocator { role, name, nth }` is the only thing that crosses the `BrowserDriver` port
   boundary** for element targeting — not an opaque ref string. `ElementLocator` lives in
   `crates/core/src/ports.rs` (plain data, like `PageExtract`/`HttpResponse`) because it appears in
   `BrowserDriver` trait method signatures (`session_click(&self, session: &SessionId, loc:
   &ElementLocator, ...)`, etc.). Native resolves an `ElementLocator` to a live element via CDP
   `Accessibility.queryAXTree(role, name)` (a server-side, protocol-native role/name filter) indexed
   by `nth`; wasm resolves it via `page.getByRole(role, {name}).nth(nth)`. Neither adapter ever
   depends on the other's internal ref/selector engine.
3. **Ref allocation and resolution (`ElementRef` ↔ `ElementLocator`) is core-owned, not
   adapter-owned.** A new `crates/core/src/tools/browser.rs` type, `SnapshotRegistry` (`Rc`-shared,
   `RefCell<HashMap<SessionId, RefTable>>`, wired once in `main.rs`/`lib.rs` exactly like
   `docs.rs`'s `SourceLocks`), owns a `RefTable` (`HashMap<ElementRef, ElementLocator>`) per
   session. `browser_snapshot`/`browser_find` parse the adapter's returned `AxSnapshotText` (via a
   single shared `parse_snapshot_text` function in `browser.rs`) into a fresh `RefTable`, assigning
   `nth` as "the Nth time this exact (role, name) pair has been seen so far, in document order,
   within this parse." Every subsequent `target: ElementRef` argument on `click`/`type`/etc. is
   resolved against the session's current `RefTable` *in core, before the adapter is ever called* —
   a miss produces a distinguishable "stale reference" error with zero adapter involvement.

## Rationale

- **Zero duplicated ref-resolution logic across adapters.** If each adapter owned its own ref
  table (native mapping `ref=eN` → `backendDomNodeId`, wasm relying on Playwright's internal
  `aria-ref` engine), the two adapters' notion of "what does `target: "e12"` mean" could silently
  diverge — exactly the two-adapter-drift failure mode `pitfalls.md` flags as this feature's
  standard failure mode. With ref allocation and resolution done once, in `core`, from one shared
  text format, there is structurally only one place this logic can drift from itself.
- **`queryAXTree`/`getByRole` are both public, documented, protocol/library-native primitives** —
  neither depends on an undocumented internal selector engine (research's explicit warning about
  `aria-ref`). `ElementLocator{role,name,nth}` is the smallest data shape that both primitives can
  act on directly.
- **Testability.** `browser.rs`'s snapshot-parsing, ref-allocation, and ref-resolution logic is
  pure data transformation over strings/hashmaps — unit-testable with hand-written
  `AxSnapshotText` fixtures and zero real browser, matching every other `tools/*.rs` module's
  `FakePort`-based test style (see ADR-0002 from `docs-index` for the precedent this follows).
- **Stale-reference handling falls out for free.** Because `RefTable` is rebuilt fresh on every
  `browser_snapshot`/extended by every `browser_find`, an `ElementRef` from a now-superseded
  snapshot (e.g. after a navigation) is simply absent from the current `RefTable` — the "stale
  reference" error `pitfalls.md`/`ux.md` both call for is a natural consequence of the data
  structure, not a special case that has to be separately implemented and kept in sync on both
  adapters.
- **This re-scopes, but does not eliminate, the native AX-tree risk.** `chromiumoxide` still
  requires new, bounded tree-assembly/formatting code (`getFullAXTree` → tree → `AxSnapshotText`)
  that has no precedent in this codebase — that work is real and is Phase 2's dedicated spike
  (`plan.md` Epic 2.1), just smaller in kind than requirements.md originally assumed.

## Consequences

- **Positive**: `BrowserDriver`'s element-targeting methods (`session_click`, `session_type`,
  `session_hover`, `session_select_option`, `session_drag`, `session_drop`, `session_file_upload`,
  `session_evaluate` with a target) all take the exact same `&ElementLocator` shape — one pattern,
  not 8 bespoke ones.
- **Positive**: `crates/core/src/tools/browser.rs`'s snapshot/ref logic is unit-testable
  independent of both real adapters, with fixtures as simple as a hand-written `AxSnapshotText`
  string.
- **Negative**: `nth`-based disambiguation is positional, not identity-based — if a page's DOM
  order for a repeated role/name pair changes between a `browser_snapshot` call and a later
  `browser_click` (without an intervening re-snapshot), `nth` can silently resolve to the wrong
  element instead of erroring. Accepted for v1, matching `pitfalls.md`'s general stance on stale
  references: bounded-retry re-resolution by role/name is the mitigation for *detached* elements;
  a same-role/name-pair *reorder* without detachment is a narrower, lower-probability edge case not
  separately solved here. Flagged in `plan.md`'s Unresolved Questions.
- **Negative**: `queryAXTree`'s and `getByRole`'s matching semantics (exact vs. substring name
  match, case sensitivity, whether `name` is the accessible name or a locator-style pattern) must
  be verified against the real installed crate/library during Phase 2 implementation, not assumed
  identical — both research summaries flagged their respective API surfaces as assessed from
  documentation, not yet verified against installed-version rustdoc/`types.d.ts` behaviorally.

## Alternatives Considered

| Alternative | Rejected because |
|---|---|
| Adapter-owned ref tables (native: `ref → backendDomNodeId`; wasm: Playwright's internal `aria-ref` engine) | Two independently-maintained resolution schemes for the same concept — the exact two-adapter-drift risk this feature is most exposed to; also depends on an explicitly-flagged-unsafe undocumented Playwright internal |
| Opaque `ElementRef` string crossing the `BrowserDriver` port boundary directly (adapter resolves the ref itself) | Forces each adapter to independently reimplement ref-table bookkeeping instead of sharing one `core`-side implementation; no testability win over the chosen design |
| Deeply-nested JSON `SnapshotNode` schema (role/name/children tree) instead of a shared text format | `ariaSnapshot({mode:"ai"})` already emits playwright-mcp's own text shape natively on wasm — inventing a second structured schema means transforming wasm's native output into it for no benefit, while native would have to produce it from scratch either way |
| Native AccName-from-scratch computation (porting Playwright's `roleUtils.ts`/`ariaSnapshot.ts`) | `build-vs-buy.md`'s re-scoped finding: CDP's `Accessibility` domain already returns Chromium's own computed role/name; Playwright only reimplements this because it is cross-browser (Firefox/WebKit have no CDP `Accessibility` domain) — this Chrome-only adapter doesn't share that constraint |
