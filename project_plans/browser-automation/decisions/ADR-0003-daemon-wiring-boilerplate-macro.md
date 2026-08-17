# ADR-0003: Bundled `BrowserToolDeps` + `register_browser_tool!` macro for daemon-wiring boilerplate

**Status**: Accepted
**Date**: 2026-07-17
**Deciders**: Tyler Stapler (solo project)
**Related**: `research/features.md` §"registration boilerplate", `research/architecture.md` §"Integration points",
ADR-0001, `plan.md` Phase 8

## Context

`crates/cli/src/main.rs::run_daemon` and `crates/wasm/src/lib.rs::run_daemon` each already spend
~130 lines wiring 5 tools: every `daemon.register("tool_name", json_handler({ ... }))` call clones
every `Rc<T>` dependency the tool's core function needs once for the outer closure and once again
for the inner `move |input| { ... }` closure, then calls the matching `crate::tools::*` function.
This feature adds ~23 new `BrowserDriver`-backed tools (ADR-0001) to both entry points (native
`main.rs`, wasm `lib.rs`) plus 23 new `#[tool]` methods to `crates/cli/src/thin_client.rs`. Naively
repeating the existing per-tool pattern would take `main.rs`/`lib.rs` from ~130 lines to well over
600 lines each of near-identical double-clone-into-closure boilerplate, which `research/features.md`
flagged explicitly as needing a mitigation before this plan's Phase 8 (daemon wiring) is written.

Every browser tool's core function needs the same fixed set of dependencies — the `BrowserDriver`
adapter (`Rc<NativeBrowser>` / `Rc<WasmBrowser>`), the new core-owned `SessionRegistry`, the new
core-owned `SnapshotRegistry` (ADR-0002), and a `ClockPort` for session-id/idle-timestamp
generation — unlike `docs.rs`'s tools, which each need a different subset of `http`/`fs`/`embedder`/
`clock`/`source_locks`. This uniformity is the leverage point: browser tools don't need per-tool
dependency selection, only per-tool input type and core-function dispatch.

## Decision

1. **Bundle the four browser-tool dependencies into one struct**, wrapped in a single `Rc`:
   ```rust
   // crates/core/src/tools/browser.rs
   pub struct BrowserToolDeps<B: BrowserDriver, C: ClockPort> {
       pub browser: B,
       pub clock: C,
       pub sessions: SessionRegistry,
       pub snapshots: SnapshotRegistry,
   }
   ```
   `main.rs`/`lib.rs` construct exactly one `Rc<BrowserToolDeps<NativeBrowser, NativeClock>>` /
   `Rc<BrowserToolDeps<WasmBrowser, WasmClock>>` each, in place of four separate `Rc`s.
2. **A `macro_rules! register_browser_tool` in each entry point** expands the repeated
   `daemon.register(name, json_handler({ let deps = deps.clone(); move |input: In| { let deps =
   deps.clone(); async move { stapler_mcp_core::tools::browser::<fn>(&deps, input).await } } }))`
   shape to a single line per tool: `register_browser_tool!(daemon, deps, "stapler_browser_click",
   ClickInput, browser::click);`. The macro is duplicated (not shared) between `main.rs` and
   `lib.rs` because each closure captures a differently-typed `Rc<BrowserToolDeps<...>>` — this
   mirrors how `json_handler` itself is already called independently at each entry point today, not
   factored into a shared helper crate.

## Rationale

- **Cuts every browser-tool registration from ~12 lines of double-clone boilerplate to 1 line**,
  without introducing type erasure, `Box<dyn ...>`, or a runtime dispatch table anywhere in the
  daemon's hot path — every tool call still resolves to a statically-typed, monomorphized function
  call through the macro expansion, identical in shape (and cost) to today's hand-written closures.
- **One `Rc::clone` per browser-tool call instead of four.** Beyond readability, this reduces the
  actual work: a tool needing all four deps went from 4 atomic-free `Rc` clones (already cheap,
  single-threaded `Rc` not `Arc`) to 1 — a minor win, but a genuine one, not just cosmetic.
- **Matches this codebase's established taste for `macro_rules!` over proc-macros or dynamic
  dispatch**: `rmcp`'s own `#[tool_router]`/`#[tool]` attribute macros are the only proc-macro
  usage in this codebase, and both are third-party, not authored here. Introducing a first
  first-party proc-macro or build-script code-gen step for one repetitive-but-structurally-simple
  call shape would be disproportionate infrastructure for what a declarative macro already solves.
- **Bundling deps (not just macro-izing the registration call) is what makes the macro tractable.**
  Without `BrowserToolDeps`, the macro would need a variadic capture-list parameter per tool (since
  today's docs-index tools each need a different dependency subset) — a meaningfully more complex
  macro. Because every browser tool needs the exact same four dependencies, the macro's only
  per-tool variables are the tool name string, the `Input` type, and the core function path.

## Consequences

- **Positive**: `main.rs`/`lib.rs` stay close to their current line count despite 23 new tools —
  Phase 8's actual diff is dominated by the four dependency constructions plus 23 one-line macro
  invocations per entry point, not 23 repeated 12-line closures.
- **Positive**: adding tool #25 in the future (if `stapler-mcp` ever grows a 25th browser tool) is
  a 1-line addition at each entry point, not a copy-paste-and-edit of a 12-line block — lower
  chance of a copy-paste bug (e.g. forgetting to update the cloned tool name inside the closure
  body, a real risk with the current hand-written pattern).
- **Negative**: `BrowserToolDeps` intentionally hands every browser tool's core function access to
  all four dependencies, even tools that only need one or two (e.g. `resize` only needs `browser`
  and `sessions`, not `snapshots`) — a minor loosening of the "function signature documents exactly
  what it touches" property `docs.rs`'s per-tool-tailored parameter lists have today. Accepted:
  the alternative (keeping per-tool-tailored parameter lists) is exactly the boilerplate this ADR
  exists to eliminate, and `BrowserToolDeps` fields are all cheap `Rc`-shared handles, not owned
  state that could be accidentally mutated out of turn.
- **Negative**: `thin_client.rs`'s 23 new `#[tool]` methods are **not** covered by this macro —
  each remains a small, distinct `rmcp`-macro-generated method calling `call_daemon(...)`, since
  that boilerplate is already minimal (3 lines per tool, `rmcp`'s own macro already does the heavy
  lifting) and not the boilerplate `features.md` flagged. No change needed there beyond what Phase
  8 adds mechanically, one method per tool.

## Alternatives Considered

| Alternative | Rejected because |
|---|---|
| Runtime dispatch table: `static TOOLS: &[(&str, fn(...) -> ...)]` looped over at startup | Every tool's `Input` type differs, so a uniform function-pointer signature would need `Box<dyn Fn(Value) -> BoxFuture<Value>>` type erasure — the daemon's first use of dynamic dispatch/heap-boxed futures at this boundary, for no readability win over a macro at this codebase's scale (24 tools, not hundreds) |
| Proc-macro / build-script code generation (e.g. generate registration calls from a `tools.toml` manifest) | Heavyweight new build-time infrastructure (first first-party proc-macro or codegen step in this codebase) to solve a problem a `macro_rules!` already solves in ~15 lines |
| Do nothing — hand-write all 23 registrations at both entry points | `features.md`'s own estimate (~130 lines for 5 tools) extrapolates to 600+ near-duplicate lines per entry point; directly contradicts this codebase's existing low-boilerplate style and materially increases copy-paste-bug risk |
