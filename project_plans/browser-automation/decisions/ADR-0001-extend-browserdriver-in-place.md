# ADR-0001: Extend `BrowserDriver` in place, not a new port trait

**Status**: Accepted
**Date**: 2026-07-17
**Deciders**: Tyler Stapler (solo project)
**Related**: `project_plans/browser-automation/research/architecture.md` §1, `research/features.md` §3, `plan.md` Step 0.5

## Context

`crates/core/src/ports.rs` currently defines `BrowserDriver` with exactly one method,
`navigate_and_extract`, backing `fetch_page`'s one-shot fresh-tab model. This feature adds
persistent-session semantics (~24 new tool-level operations: navigate-with-session, click, type,
snapshot, etc.) that all need the same underlying resource `fetch_page` already uses — one shared
`chromiumoxide::Browser` process (native) / one shared `playwright-core` `chromium.launch()`
(wasm). The question: does the new session surface get its own port trait (e.g.
`InteractiveBrowserDriver`), a fully separate tool-family with its own mini-architecture outside
`ports.rs` entirely, or does `BrowserDriver` grow in place to ~24+ methods?

Three shapes were considered (`plan.md`'s Step 0.5 creative pass):

- **A — Extend `BrowserDriver` in place.** One trait, ~24 new methods alongside the existing
  `navigate_and_extract`.
- **B — New parallel port trait** (`InteractiveBrowserDriver`), implemented by the same
  `NativeBrowser`/`WasmBrowser` structs, wired as a second `Rc<T>` into `main.rs`.
- **C — Fully separate tool-family with its own mini-architecture**, decoupled from `ports.rs`
  entirely (e.g. its own session-driver abstraction, not expressed as a `crate::ports` trait at
  all).

## Decision

**Approach A.** `BrowserDriver` grows from 1 method to ~24 (see `plan.md`'s Domain Glossary and
Phase 1–6 method list), still implemented by exactly one `NativeBrowser` (native) and one
`WasmBrowser` (wasm) struct each, still wired as the same single `Rc<NativeBrowser>` /
`WasmBrowser` value already threaded through `main.rs`/`lib.rs`.

## Rationale

- **No natural process boundary to split on.** Both B and C would still be backed by the exact
  same shared `Browser`/`chromium.launch()` resource `fetch_page` already uses (requirements.md's
  own Feasibility Risks section is explicit that a shared single `Browser` process means a crash
  is total-session-loss for every session at once — there is no per-tool-family isolation to
  preserve by splitting the trait). Splitting the *interface* without splitting the *resource*
  buys no isolation, only more wiring.
- **Wiring cost is real and avoidable.** `main.rs`/`lib.rs` already hold one `Rc<NativeBrowser>`
  clone per tool-handler closure (5 tools' worth of `.clone()` calls today, per `NOTES.md`'s
  Phase 1b/4 entries). A second trait implemented by the same struct would mean every browser tool
  handler closure clones *two* `Rc`s instead of one, for no behavioral benefit — pure boilerplate
  tax, and exactly the kind of "no infra it doesn't need" this codebase's existing precedent
  (`ports.rs`'s own header comment: "add a port only for genuinely OS/hardware-touching behavior")
  argues against multiplying without cause.
- **`fetch_page` and the new tools are the same *capability* at different call shapes, not
  different capabilities.** `navigate_and_extract` is, structurally, "open a page, do one thing,
  report the result" — the new `session_navigate` is the same operation with a returned handle for
  follow-up calls instead of an implicit close. One trait expressing "things you can do to/with a
  browser page" is the more honest model than pretending these are unrelated concerns.
- **Approach C's isolation is illusory and costly.** A fully separate mini-architecture (its own
  session-driver trait defined outside `ports.rs`, its own adapter wiring convention) would
  duplicate the ports-and-adapters pattern this whole codebase already has, for a feature that
  needs the exact same native/wasm adapter split as everything else. It also cuts against
  `requirements.md`'s explicit premise — "one daemon, one architecture" — by introducing a second
  architecture-within-the-architecture for browser tools specifically.
- **Precedent**: `architecture.md`'s own research explicitly recommends keeping `BrowserDriver` as
  one trait ("stays ONE trait — don't split, avoids wiring multiple `Rc<T>`s into `main.rs`"),
  and this planning pass found no counter-evidence during the Step 0.5 pass strong enough to
  override it.

## Consequences

- **Positive**: `main.rs`/`lib.rs` wiring stays exactly one `Rc<NativeBrowser>` / one
  `WasmBrowser` value for every browser-family tool, old and new alike — no new `Rc` juggling.
- **Positive**: `crates/cli/tests/` and any future `FakeBrowserDriver` test double only need to
  implement one trait to fully exercise every browser tool's `core` logic.
- **Negative**: `BrowserDriver` becomes this codebase's largest port trait by a wide margin (1 →
  ~24 methods, versus `FileStore`'s 3 or `HttpClient`'s 1) — a future reader skimming `ports.rs`
  will find one trait dominating the file. Mitigated by grouping the new methods under a clearly
  labeled `// --- Session-based browser automation (see docs/browser-automation) ---` comment
  block in `ports.rs`, keeping `navigate_and_extract` visually separate as the original one-shot
  method.
  ​
- **Negative**: every `NativeBrowser`/`WasmBrowser` change (even one unrelated to sessions) now
  touches a much larger `impl BrowserDriver for ...` block. Accepted — the alternative (splitting)
  was shown above to buy no real isolation.

## Alternatives Considered

| Alternative | Rejected because |
|---|---|
| B — parallel `InteractiveBrowserDriver` trait | Same underlying `Browser`/`chromium.launch()` resource as `BrowserDriver`; splits the interface without splitting the resource, doubling `Rc` wiring in every handler closure for no isolation benefit |
| C — fully separate tool-family / mini-architecture outside `ports.rs` | Duplicates this codebase's whole ports-and-adapters pattern for one feature; contradicts requirements.md's explicit "one daemon, one architecture" premise |
