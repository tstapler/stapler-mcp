# Research: Persistent profile features, prior art, and edge cases

item_id: 10f6836c-71fc-4db2-8bc8-117d65da6a2d

## Codebase grounding (re-verified 2026-09-23)

- `NativeBrowser::launch()` (`crates/native/src/browser.rs:337-377`) is the only
  call site that constructs `BrowserConfig` / calls `.user_data_dir(...)`. It
  runs once per daemon process. No `clap`/`Args` struct exists anywhere in
  `crates/cli/src/main.rs` — arg parsing there is ad hoc `std::env::args()`
  scanning (only `--daemon` is checked). There is currently **no** CLI-flag or
  config-file plumbing at all for anything browser-related, so a persistent
  profile opt-in would be new surface, not an existing pattern to extend.
- The existing comment at `crates/native/src/browser.rs:339-346` already
  identifies the exact failure mode a shared/named profile must solve:
  chromiumoxide's default (unset `user_data_dir`) is one fixed shared path,
  and a second Chrome process pointed at an already-open profile "finds it
  locked (Chrome's own `SingletonLock`) and fails to launch." This is the
  same lock file mechanism industry tools hit (below) — the codebase has
  already independently rediscovered it for the *ephemeral* per-pid case;
  the same mechanism applies to a *named* profile shared across daemon
  restarts or concurrent daemons.
- No `BrowserContext`/incognito isolation exists; all sessions in a process
  share the one profile already (per requirements.md). Nothing currently
  removes `$TMPDIR/stapler-mcp-chromium-<pid>-<now_millis>` directories on
  exit (confirmed: no `remove_dir_all` or `Drop` impl touching
  `user_data_dir` in `browser.rs`) — that's the disk-leak gap requirements.md
  flags as separate/out of scope.

## Prior art

### Playwright MCP (`microsoft/playwright-mcp`) — closest direct comparable

This is the most relevant precedent: another MCP browser-automation server
built on the same CDP-driven single-profile model.

- **Persistent by default.** Unlike stapler-mcp's current always-ephemeral
  behavior, Playwright MCP defaults to a *persistent* profile stored per
  workspace at a fixed cache path (`~/.cache/ms-playwright/mcp-{channel}-profile`
  on Linux, keyed by a hash so different projects/workspaces get separate
  profiles automatically) — cookies and login state survive across server
  restarts with zero configuration.
- **`--isolated` flag** is the *opt-out*: forces a fresh, throwaway profile
  per session, discarded on close — the inverse of what requirements.md
  proposes (there, ephemeral is default and persistence is opt-in).
- **`--user-data-dir <path>`** lets a user point at an explicit custom
  profile directory, overriding the default cache-path scheme.
- **Documented concurrency failure mode, verbatim relevant to this repo**:
  "A persistent profile can only be used by one browser instance at a time,
  so concurrent MCP clients sharing the same workspace will conflict. To run
  several clients in parallel, start each additional client with `--isolated`
  or point it at a distinct `--user-data-dir`." This is the `SingletonLock`
  contention scenario, confirmed as a real operational issue for this exact
  architecture, not a theoretical one.

### Playwright (library) `launchPersistentContext`

- Chromium's `SingletonLock`/`SingletonSocket`/`SingletonCookie` files are
  written at launch and removed on clean exit. If the Chrome process is
  killed abnormally (crash, `kill -9`, OOM), the lock files are orphaned even
  though no process is running — a later launch against the same
  `userDataDir` fails with `SingletonLock: File exists (17)` even though
  nothing is actually holding the profile. Real-world implementations work
  around this with retry-with-stale-lock-removal logic (check the PID
  embedded in the lock; if dead, delete the lock and retry) — noted as
  unreliable on Linux specifically, since PID reuse can make a dead lock look
  live.
- Open Playwright issue (#35466) reports persistent-context cookie reads
  failing and profile corruption under some conditions — i.e., corruption is
  an observed-in-the-wild failure mode for this exact pattern, not
  hypothetical.

### Puppeteer `userDataDir`

- Same underlying mechanism (Chrome launch flag, one dir per launched
  browser process); Puppeteer's docs are the origin of the "profile dir is
  launch-time only, can't be swapped mid-session" constraint that
  requirements.md already independently confirms for chromiumoxide.

### Cloud/managed browser-automation services (Anchor Browser, Skyvern, AWS
Bedrock AgentCore, browser-use)

These operate at a different trust/deployment tier (multi-tenant cloud, not
single-user local) but converge on the same three design choices, useful as
signal for what a "done" version of this feature looks like:

- **Explicit `persist: true` / named profile as opt-in, decoupled from a
  particular session.** Anchor Browser: "the profile will be automatically
  saved when you set `persist: true` when creating the session... profiles
  are explicitly separate from the session they were captured in, and can be
  reused in completely new sessions later, with that session starting
  already authenticated." This matches requirements.md's framing (named,
  durable, opt-in) better than Playwright MCP's default-on scheme does —
  supports the recommendation to default to ephemeral and require an
  explicit name/flag.
- **Narrow persistence scope.** Anchor Browser scopes what's persisted to
  "cookies, local storage, and cache — the things that let a browser skip a
  login flow instead of repeating it," explicitly not the full profile
  (extensions, browsing history, etc.). Worth deciding explicitly whether
  stapler-mcp persists the whole Chrome user-data-dir (simplest — it's what
  the launch flag already does) or something narrower; whole-dir is the
  pragmatic MVP choice but carries more disk/secret surface.
  (INFERRED — Anchor's finer scoping is a cloud-service design constraint
  driven by multi-tenant storage/billing concerns, not necessarily applicable
  to a local single-user tool at the same weight; flagging as a considered
  alternative, not a mandate.)
- **Fingerprint/session consistency layer** (Anchor): UA, UA-client-hints,
  timezone, locale, viewport kept mutually coherent across a persisted
  session. Not a fit for this item's scope (single-user local tool, not
  anti-bot-detection product) — noting only because it's the kind of
  "beyond don't re-login" need a cloud tool solved that a personal tool
  likely doesn't need.
- **BrowserState** (`browserstate-org/browserstate`) and AWS Bedrock
  AgentCore both frame profile persistence as an explicit save/restore
  artifact (snapshot semantics) rather than "the directory just happens to
  still be there" — a more deliberate model than "leave a directory around
  and hope," worth considering if profile corruption recovery matters (see
  below), though heavier than this item's scope likely warrants.

## Edge cases and failure modes to design against

1. **Concurrent daemons pointed at the same named profile (lock
   contention).** Directly demonstrated by both the existing code comment
   (`browser.rs:339-346`, re: chromiumoxide's *unnamed* default path) and
   Playwright MCP's documented behavior for its *named* persistent profile.
   Chrome's `SingletonLock` will cause the second launch to fail outright,
   not degrade gracefully. Design must either: (a) fail fast with a clear
   error naming the conflict (simplest, matches Playwright MCP's approach —
   push the user toward `--isolated`/a distinct dir), or (b) detect a stale
   lock (dead PID) and reclaim it. Given this is explicitly a "single-user
   local tool" (per the research question framing) rather than a multi-client
   server, one daemon process at a time is likely the right assumption to
   design for — but two daemon instances (e.g., a stray previous process
   that didn't shut down cleanly) is a realistic accident, not just a
   multi-client scenario.
2. **Stale lock after unclean shutdown.** If the daemon is killed (not
   graceful `close()`), `SingletonLock` et al. can be left behind, blocking
   the *next* legitimate launch even with no real conflict — this is a
   likely-common case for a local dev tool (killed terminal, OOM, crash) and
   should be handled proactively (e.g., check/clear known-stale lock files
   before launch) rather than surfacing Chrome's raw launch failure to the
   MCP caller.
3. **Profile corruption / Chrome version skew across upgrades.** A profile
   directory written by one Chrome version can refuse to load ("profile
   cannot be used... from a newer version") after a Chrome upgrade/downgrade
   (e.g., Homebrew bumping the Chrome-for-Testing or system Chrome binary
   between daemon runs) — confirmed as a real Chrome behavior. A durable
   named profile used across "sessions spanning daemon restarts" (the
   item's own framing) will eventually cross a Chrome upgrade boundary
   locally. Options: detect the launch failure and offer/auto-fallback to a
   fresh profile with a clear message (don't silently swallow into a
   confusing generic launch error), vs. doing nothing and letting it surface
   as an opaque `PortError`.
4. **Multiple named profiles vs. a single default persistent one.** Playwright
   MCP defaults to *implicit* persistence (on by default, one profile per
   workspace hash) with `--isolated` as opt-out. Anchor Browser and this
   item's own requirements.md prefer *explicit* opt-in with a name. Given
   this item explicitly wants to preserve default (ephemeral) behavior, a
   single named flag (e.g., a daemon-startup `--profile-dir <path>` or
   `STAPLER_MCP_PROFILE_DIR` env var) covers the stated need without
   building a multi-profile registry; supporting *multiple* named profiles
   (for switching identities) is a natural next ask but is more surface than
   "smallest change" calls for.
5. **Secrets/cookies unencrypted on disk.** Chrome's own `Login Data` /
   cookie store in a profile directory is encrypted at rest using an
   OS-keychain-backed key on macOS/Windows, but on Linux the encryption key
   itself typically lands in a `libsecret`/plaintext-fallback keyring
   (behavior varies by desktop-keyring availability) — meaning a durable
   profile directory is realistically a plaintext-equivalent bundle of
   session cookies for whatever sites were visited, sitting at a
   predictable, non-`$TMPDIR` path for the lifetime of the machine. Given
   this is a local single-user tool the bar is lower than a shared/cloud
   system, but the design should still: default to *off* (already required
   by requirements.md), and probably create the directory with restrictive
   permissions (`0700`) analogous to how SSH/GPG directories are handled
   elsewhere in this dotfiles ecosystem — worth checking whether
   `std::fs::create_dir_all` in `browser.rs:352` sets any mode today (it
   doesn't; relies on umask).
6. **Stale login sessions expiring silently.** Out of this tool's control
   (site-side session TTLs), but worth a one-line note in whatever docs ship
   with this: a persistent profile reduces *daemon-restart* re-logins, not
   *all* re-logins — a caller should still expect to handle a login wall
   appearing mid-task and not treat profile persistence as a guarantee.
7. **Directory-leak-on-crash for the persistent path specifically.** The
   existing ephemeral leak (unremoved `$TMPDIR/...` dirs, called out as
   out-of-scope in requirements.md) is arguably *worse* once a persistent
   option exists side-by-side, because a user could plausibly confuse "my
   opt-in persistent dir" with "an abandoned ephemeral dir" when cleaning up
   manually — an argument for the persistent path using a clearly distinct,
   documented location/name (not sharing the `stapler-mcp-chromium-*` naming
   scheme used for ephemeral dirs).

## Unstated needs beyond "don't re-login"

- **An explicit override to force ephemeral even when a persistent profile
  is configured** — the mirror image of Playwright MCP's `--isolated` flag.
  A user who's set up a durable profile for normal use will still
  occasionally want a clean-slate/incognito-equivalent run (e.g., testing
  what a logged-out visitor sees, or avoiding polluting the persistent
  profile with a one-off site). Since the architecture is one profile per
  daemon *process* (not per session), this override would have to be a
  daemon-launch-time choice too, not a per-`navigate()` argument — same
  constraint requirements.md already identified for the positive case.
- **Switching between multiple logged-in identities** (e.g., personal vs.
  work Google account) — explicitly named in the research question, and a
  real personal-tool need, but requirements.md's own scope already excludes
  "per-session/per-tab profile isolation (multiple simultaneous identities)."
  Note this only as a likely *next* ask once single-profile persistence
  ships, not something to build now — the smallest-change principle in
  requirements.md argues for one named durable dir first.
- **Knowing which profile is active.** If a persistent-profile flag/env var
  exists, whatever tool surfaces daemon status (none currently found wired
  through MCP tools in this pass — worth a follow-up check) should probably
  report whether the current session is running against the ephemeral or
  the named persistent profile, so a caller isn't guessing why a site is
  (or isn't) already logged in.
- **A way to reset/clear the persistent profile without deleting it by
  hand** — once cookies/login state can silently go stale or a profile
  corrupts across a Chrome upgrade (edge case 3), a user will want a clean
  way to blow it away and start fresh rather than hunting for the directory
  path themselves.

## Sources

- [Playwright MCP — Profile & State](https://playwright.dev/mcp/configuration/user-profile)
- [Playwright MCP — User Profiles guide](https://www.mintlify.com/microsoft/playwright-mcp/guides/user-profiles)
- [Support isolated Playwright MCP browser instances (separate user-data-dir) · Issue #1294](https://github.com/microsoft/playwright-mcp/issues/1294)
- [\[Bug\]: Persistent Context (userDataDir) Fails Cookie Read & Corrupts Profile · Issue #35466](https://github.com/microsoft/playwright/issues/35466)
- [Playwright MCP Persistent, Isolated, and Browser Extension Profiles — QASkills.sh](https://qaskills.sh/blog/playwright-mcp-profile-modes-guide-2026)
- [Anchor Browser — Browser Profiles (Authenticated sessions)](https://docs.anchorbrowser.io/essentials/authentication-and-identity)
- [Anchor Browser — Understanding Browser Sessions and State Management](https://anchorbrowser.io/blog/understanding-browser-sessions-and-state-management)
- [Skyvern — Browser Profiles](https://www.skyvern.com/docs/developers/optimization/browser-profiles)
- [AWS Bedrock AgentCore — Using browser profiles](https://docs.aws.amazon.com/zh_tw/bedrock-agentcore/latest/devguide/browser-profiles.html)
- [BrowserState (browserstate-org/browserstate)](https://github.com/browserstate-org/browserstate)
- [Chromium — Supported Directory Variables (profile compat notes)](https://www.chromium.org/administrators/policy-list-3/user-data-directory-variables/)
- [Fix Chrome "profile from a newer version" error](https://www.groovypost.com/howto/fix-chrome-error-message-profile-newer-version/)
- Codebase: [`crates/native/src/browser.rs`](https://github.com/tstapler/stapler-mcp/blob/51def0e9b3e34908241ed37adf27d044e2b56d9c/crates/native/src/browser.rs) (line ranges cited inline above refer to this SHA — last commit touching the file)
