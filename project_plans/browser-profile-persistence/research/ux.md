# Research: UX (developer/operator ergonomics + LLM-agent tool-call ergonomics)

No end-user GUI exists (`stapler-mcp` is a headless daemon), so this covers
discoverability of the opt-in for the developer configuring the daemon, and
the failure-signaling contract exposed to an LLM agent calling
`stapler_browser_navigate`. WCAG/ARIA is out of scope per the requirements
doc.

## 1. Least-surprising opt-in surface: env var, not a CLI flag or config file

The repo has exactly one established precedent for daemon-level
configuration, and it's environment variables, not CLI flags:

- `crates/cli/src/main.rs:26` parses args with a single
  `std::env::args().any(|a| a == "--daemon")` — there is no `clap`/argument
  parser in the binary at all. Adding a `--user-data-dir` (or similar) flag
  would be the *first* CLI flag in the project and would require pulling in
  an argument-parsing dependency that doesn't exist today.
- `BRAVE_API_KEY` and `BRAVE_API_BASE_URL` are the one existing example of
  opt-in daemon configuration, read directly from `std::env::var` in
  `main.rs:122-123` and documented in the README's tool table: "Reads
  `BRAVE_API_KEY` from the **daemon's** environment... Base URL overridable
  via `BRAVE_API_BASE_URL`" (`README.md:92`), with a matching callout in the
  Usage section: "`BRAVE_API_KEY` must be set in the environment the
  **daemon** happens to run from... or export it before `--daemon` is
  auto-spawned" (`README.md:150-152`).
- All daemon-owned filesystem locations (`~/.stapler-mcp/daemon.sock`,
  `daemon.lock`, cache dir, docs-index dir, embedding-cache dir) are
  computed centrally in `crates/core/src/paths.rs:9-39`, each via a small
  `fn xxx_dir<E: EnvPort>(env: &E) -> String` that defaults under
  `~/.stapler-mcp/` and is exercised through the `EnvPort` trait (mockable
  in tests, see `paths.rs:48-95`). A persistent-profile directory fits this
  exact pattern: a `paths::browser_profile_dir(&env)` default (e.g.
  `~/.stapler-mcp/browser-profile`), with an env var like
  `STAPLER_MCP_BROWSER_PROFILE_DIR` following the same "opt out of default
  by setting an env var the daemon process sees" contract `BRAVE_API_KEY`
  already teaches developers.

**Why this is least surprising**: a developer who has already read the
README's `BRAVE_API_KEY` callout has the mental model "daemon config = env
var, and it has to be set in the daemon's actual environment, which may not
be your interactive shell's environment if the daemon was auto-spawned by a
thin client." Introducing a CLI flag alongside that would mean two
different mechanisms for daemon opt-in config with no explained reason for
the split. A config file would be a third, entirely new mechanism this repo
has never used — the requirements doc's "smallest change" framing argues
against introducing it just for this.

**Caveat**: env vars for the daemon have the same discoverability gotcha
`README.md:150-152` already documents for `BRAVE_API_KEY` — if the thin
client auto-spawns the daemon, the var must be set wherever *that* spawn
happens (e.g. the shell profile sourced by whatever launches Claude Code),
not just the interactive shell the developer is typing in when debugging.
Any new env var should get the identical explicit callout, or developers
will hit the same "I set it and nothing changed" confusion twice.

## 2. Risk of surfacing this as an MCP tool parameter instead

`user_data_dir` is a Chrome *launch-time* flag (`browser.rs:354-356`,
`BrowserConfig::builder().user_data_dir(...)`) consumed once inside
`NativeBrowser::launch()`, which runs exactly once per daemon process
(called once from `run_daemon()`, confirmed by the single call site and the
doc comment at `browser.rs:338-345` explaining *why* a fresh dir is chosen
per daemon instance — to avoid Chrome's `SingletonLock` collision between
concurrent daemons). By the time any `stapler_browser_navigate` call
reaches the daemon, Chrome has already launched with whatever profile dir
it launched with.

If a `persistent: bool` or `profile: string` parameter were added to
`BrowserNavigateInput` (`schema.rs:278-287`), it would silently no-op on
every call after the very first `navigate` of a given daemon's lifetime —
including the first call of every *subsequent* MCP session/subagent that
reuses the same long-lived daemon (per the README's whole architectural
premise: "Exactly one instance runs machine-wide," `README.md:33-34`). This
is the worst kind of silent failure for an LLM agent: it can't be detected
by reading a single tool response, only by comparing behavior across calls
or across daemon restarts it has no visibility into. A parameter that
"sometimes does something, sometimes doesn't, depending on invisible
process history" is more surprising than no parameter at all.

Concretely worse than a normal no-op: the parameter would appear to *work*
on the lucky first call of a fresh daemon (session survives a restart) and
then silently stop working for the rest of that developer's session,
producing a flaky-looking bug report ("it worked yesterday") that's
actually 100% deterministic given daemon lifecycle, but looks
non-deterministic from the agent/developer's vantage point because daemon
restarts are invisible background events.

**Recommended signaling if this parameter existed anyway** (e.g. as a
stopgap before a daemon-level env var lands): reject-with-error, not
warn-and-ignore. Given `PortError`'s existing taxonomy
(`ports.rs:16-35` — `NotFound`, `SessionCrashed`, `NotActionable`, each
picked because "the caller's fix is X, not Y"), the right fix-signal for a
late/mismatched `persistent`/`profile` param is "this call's fix is
`stapler_browser_close_session`+relaunch is not possible without a daemon
restart, which this caller cannot trigger" — i.e., a hard error naming the
mismatch, not a warning buried in a successful response the agent has no
strong incentive to read. An LLM agent, like a human skimming output, will
not reliably notice a soft warning attached to an otherwise-successful
navigate; an error forces the agent to surface (or handle) the mismatch to
whoever's driving it. This is also why the *daemon-startup* opt-in (§1) is
strictly better: it removes the entire class of "only honored on first
call" ambiguity, since the profile dir is fixed before any tool call is
even possible.

## 3. Error states for the persistent profile dir

Three failure modes to design for, using the existing `PortError` shape
(`ports.rs:16-38`) rather than inventing a new error channel:

- **Locked by another running daemon** (Chrome's own `SingletonLock`,
  already the documented reason per-daemon dirs are pid+timestamp-scoped
  today, `browser.rs:320-333`). Opting into a *shared* persistent dir
  reintroduces exactly the collision the current scheme was built to avoid,
  now deliberately. Chrome will fail the CDP launch with a lock-related
  error from `Browser::launch()`, currently mapped generically via
  `.map_err(|e| PortError::Other(e.to_string()))` at `browser.rs:357-359`.
  `PortError::Other` is too coarse here: the fix for "profile dir locked by
  another daemon" is "stop the other daemon or don't opt into a shared
  dir," which is a different remediation than a generic I/O failure. Worth
  a dedicated variant (or at minimum a message prefix distinguishing it)
  so the daemon's startup failure — and any log line a developer greps for
  — says "profile dir in use by another process" rather than an opaque
  chromiumoxide error string. This failure must surface at **daemon
  startup**, not at the first `navigate` call, since that's when
  `launch()` runs; a developer starting a second daemon against the same
  profile dir needs the failure in the daemon's own stderr/exit code, not
  as a mysterious tool-call failure reported later through an unrelated MCP
  session.
- **Missing/unwritable directory**: same code path,
  `std::fs::create_dir_all` at `browser.rs:352` already
  `map_err`s to `PortError::Other(e.to_string())` before Chrome is even
  invoked — this case is already handled structurally, just also folded
  into the generic `Other` variant. Given the daemon fails to launch at
  all in this case (this runs before `Browser::launch()`), the practical
  developer experience is "daemon won't start, check stderr" — acceptable
  as long as the error message actually names the path it tried to create
  (worth confirming `e.to_string()` on a permissions error includes the
  path; `std::io::Error` display does not always include it depending on
  the OS/errno).
- **Incompatible Chrome version** (profile dir written by a newer/older
  Chrome than the one currently installed): Chrome itself handles this by
  either upgrading the profile format silently (usually) or, for a
  downgrade, printing its own "profile version is newer" dialog/error and
  refusing to start in some configurations. This is the least controllable
  case — the daemon has no way to inspect a `user_data_dir`'s Chrome
  version out of band before calling `Browser::launch()`, so it inherits
  whatever failure mode chromiumoxide surfaces (again currently collapsed
  into `PortError::Other`). Not worth bespoke handling given how rarely
  local Chrome downgrades happen; a clear error message naming the profile
  dir path is the achievable bar, not a version check the daemon would need
  headless Chrome-version introspection to perform in the first place.

In all three cases, the important design choice is *when* the error
surfaces: the requirements doc's own ground truth (`requirements.md`:
"`user_data_dir` is a Chrome launch-time flag... it can't be changed per
`navigate()` call") means these are daemon-startup failures, not tool-call
failures. The MCP tool surface (`stapler_browser_navigate` et al.) should
never see them directly — a developer needs them in the daemon's own
startup logs/exit code, the same place they'd already look if the daemon
failed to bind its socket or acquire its process lock today.

## 4. Job-to-be-done, and whether it changes the opt-in shape

The requirements doc frames two candidate jobs: "uninterrupted multi-day
agent task" vs. "don't want to log into Gmail every test run." These point
to the same underlying need — *survive a daemon restart* — and the
existing single-profile-per-daemon-process design (`browser.rs:320-333`)
already fully satisfies the *within-a-daemon-lifetime* version of both (the
doc's own ground truth confirms sessions/tabs already share cookies within
one daemon process). The only gap either job needs closed is exactly what
§1 proposes: a **named, durable directory that survives a daemon restart**,
opted into at daemon-launch time.

Where the two jobs *would* diverge is a scenario neither is describing but
which the opt-in shape should not foreclose: a developer wanting **multiple
concurrent named profiles** (e.g. one persistent profile for a
logged-into-service test suite, a separate ephemeral one for scraping
untrusted sites in the same daemon lifetime). Today's architecture is one
Chrome process, hence one profile, per daemon — supporting multiple
concurrent profiles would mean multiple Chrome processes or multiple daemon
instances, a materially larger change explicitly out of scope per the
requirements doc's "smallest change" framing. The env-var-at-startup design
in §1 doesn't preclude this later (a developer could always run a second
daemon on a second socket path with a second profile-dir env var, mirroring
how the pid+timestamp scoping already prevents two daemons from colliding
today) — it just doesn't solve it now, which matches both stated jobs.

Neither job benefits from the MCP-tool-parameter shape in §2 — both are
about *daemon* longevity, not per-call or per-session profile switching
within one daemon's lifetime, reinforcing that the opt-in belongs at daemon
startup, not on `stapler_browser_navigate`.

## Summary

1. **Opt-in surface**: an env var read once in `NativeBrowser::launch()`
   (e.g. `STAPLER_MCP_BROWSER_PROFILE_DIR`), following the exact pattern
   `BRAVE_API_KEY`/`BRAVE_API_BASE_URL` already establish
   (`README.md:92,150-152`) and the `paths.rs` per-purpose-dir convention —
   not a CLI flag (no arg parser exists in the binary at all,
   `main.rs:26`) and not a new config file.
2. **Tool-parameter risk**: a `persistent`/`profile` param on
   `stapler_browser_navigate` would silently no-op on every call after the
   daemon's first `launch()` (`browser.rs:338`), including every call from
   later, unrelated MCP sessions sharing the same long-lived daemon — the
   fix is a hard error naming the mismatch (fitting `PortError`'s existing
   "the error tells you the correct next action" convention,
   `ports.rs:16-35`), not a silently-ignored warning, and the better fix is
   not exposing this on the tool surface at all.
3. **Error states**: all three (locked profile, missing/unwritable dir,
   incompatible Chrome version) are daemon-*startup* failures given
   `user_data_dir` is launch-time-only — they belong in the daemon's own
   stderr/exit code path (same place a socket-bind or lock-acquire failure
   already surfaces), not as an MCP tool-call error, since by the time any
   tool call runs, Chrome has already launched or the daemon already failed
   to start.
