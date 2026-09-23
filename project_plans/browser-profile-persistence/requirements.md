# Requirements: Persistent browser profile / user-data-dir reuse across sessions

item_id: 10f6836c-71fc-4db2-8bc8-117d65da6a2d

## Problem statement

The backlog item asks whether `stapler_browser_navigate` sessions reuse a persistent
Chrome profile or always start ephemeral, and proposes an opt-in `user_data_dir`
parameter on `stapler_browser_navigate` so multi-turn browser tasks spanning daemon
restarts don't force re-authentication on every login-gated site.

## Ground truth (verified against `crates/native/src/browser.rs`, 2026-09-23)

- `NativeBrowser::launch()` (`crates/native/src/browser.rs:337-377`) creates **one**
  Chrome process and **one** `user_data_dir` per daemon process, at
  `$TMPDIR/stapler-mcp-chromium-<pid>-<now_millis>` (`browser.rs:347-352`), and passes
  it to `BrowserConfig::builder().user_data_dir(...)` once at launch.
- The comment at `browser.rs:339-346` explains this is deliberate: chromiumoxide's own
  default (unset `user_data_dir`) is a single fixed shared path
  (`$TMPDIR/chromiumoxide-runner`), so two daemons on one machine would collide on
  Chrome's `SingletonLock`. The pid+timestamp scoping exists to avoid that collision,
  not to force per-session isolation.
- A "session" (`SessionId`) is a `Page`/tab inside that one shared browser
  (`new_page(...)` at `browser.rs:1034`, `:1131`, `:1571`) — there is no
  `BrowserContext`/incognito-context creation anywhere in the file. All sessions in one
  daemon process **already share one Chrome profile**, including cookies and login
  state.
- `close_session` (`browser.rs:1469-1485`) and the idle-session reaper only remove the
  `Page`/session entry and close the tab (`session.close()`); neither touches
  `user_data_dir`. So closing a session, or a session expiring from idle timeout,
  does **not** lose login state — the profile directory and Chrome process live on
  until the daemon exits.
- **Conclusion: the premise "sessions ... always start from a fresh ephemeral
  context" is false for the multi-turn/idle-timeout case the item's Evidence section
  worries about.** The only real gap is that `user_data_dir` is a fresh temp directory
  **per daemon process**, so a daemon restart (upgrade, crash, machine reboot) loses
  all cookies/login state. Nothing in the code removes the temp directory on shutdown
  either — it also leaks on disk across restarts (a separate, smaller finding; see
  Suggestions in the final report).

## Revised problem framing

Not "add persistence" in the abstract — narrow it to: **let a daemon restart reuse
the same Chrome profile directory it used last time, opt-in, without changing the
per-session sharing model that already works.**

## Constraints from current architecture

- `user_data_dir` is a Chrome **launch-time** flag (`BrowserConfig::builder()`,
  `browser.rs:354-357`), consumed once in `NativeBrowser::launch()`. It cannot be set
  or changed per `navigate()` call without relaunching the whole Chrome process — so
  the item's proposed shape ("opt-in parameter on `stapler_browser_navigate`") does
  not fit the current one-browser-process-per-daemon architecture without either (a)
  moving the parameter to daemon startup instead of per-call, or (b) running multiple
  Chrome processes (one per named profile) inside one daemon, a materially bigger
  change.
- `stapler-mcp` is a local single-user tool (per `.claude/CLAUDE.md`'s
  `code-new-project` skill categorization of this kind of project) — there is no
  multi-tenant isolation requirement motivating per-session profile isolation today.

## In scope

- Determine the smallest change that lets a user opt in to a durable, named
  `user_data_dir` that survives daemon restarts.
- Decide where the "opt-in" surface belongs given the launch-time constraint above
  (daemon CLI flag / env var vs. a session-scoped MCP parameter).
- Identify what happens to in-flight ephemeral behavior (default) — must remain
  unchanged when the user does not opt in.

## Out of scope

- Per-session/per-tab profile isolation (multiple simultaneous logged-in identities
  in one daemon run) — no evidence of demand, no existing session/profile mapping to
  build on; flagged as a suggestion only.
- Cloud sync of profile state (the `browser-use` "profile-use" and Anchor Browser
  fingerprint-management features cited as evidence) — this item's proposed work
  section only asks about local named-profile reuse.
- Cleanup of leaked ephemeral temp dirs from prior runs — related but separate
  finding, not blocking this item.

## Acceptance criteria (draft, refined further in plan.md)

1. It's documented (code comment + this research) whether sessions currently share a
   profile — done above; verified false premise in the original item.
2. A daemon operator can opt in to a persistent, named `user_data_dir` that survives
   daemon restarts, without changing default (ephemeral, opt-out) behavior.
3. The opt-in surface is placed where it's technically enforceable (launch-time),
   not on a per-call MCP parameter that can't actually change a running browser's
   profile.
4. Existing ephemeral multi-daemon collision avoidance (pid+timestamp scoping) keeps
   working when persistence is not opted into.

## Priority

`later` (as labeled in the source item).
