# Adversarial Review: browser-profile-persistence

**Date**: 2026-09-23
**Verdict**: CONCERNS
**Note**: Blocker re-verified after plan.md repair pass (see git history / prior review below for original finding).

## Blockers

None — see Resolved Blockers below.

## Resolved Blockers

- [x] **The signature change breaks an existing test that no task updates.** —
  verified fixed: `plan.md`'s Story 1.2.1 (line 327) now lists
  `crates/cli/tests/browser_session.rs` in its Files line, and the new
  Task 1.2.1b ("Fix the now-broken `browser_session.rs` call site (BLOCKER fix)",
  plan.md:349-364) changes `NativeBrowser::launch()` at `browser_session.rs:1003`
  to `NativeBrowser::launch(None)`, runs `grep -rn "NativeBrowser::launch(" crates/`
  as a preflight, and instructs `cargo build -p stapler-mcp --tests` to confirm the
  workspace compiles. Independently re-ran that grep against the real (still
  pre-implementation) source tree: it returns exactly three hits —
  `crates/cli/src/main.rs:88` (a real call site, updated by Task 1.2.2a),
  `crates/cli/tests/browser_session.rs:1003` (a real call site, updated by the new
  Task 1.2.1b — line number matches the plan's citation exactly), and
  `crates/native/src/browser.rs:2486` (a code comment referencing `launch()`, not
  a call site). So the plan's "exactly two call sites" claim is correct and the
  fix targets the right file and line. Task ordering: 1.2.1a (signature change)
  and 1.2.1b (call-site fix) are adjacent tasks inside the same Story/Epic, both
  handled by a single epic-level worker per the `sdd:5-implement` execution model
  (one worker implements every task in an epic before committing), so the
  momentarily-non-compiling state between the two tasks is never a commit
  boundary. Task 1.2.1a's own text also explicitly flags "must land in the same
  change" as a guardrail. No other task in Epic 1.2/1.3 touches
  `browser_session.rs` or reintroduces a zero-arg call.

## Concerns

*Note: kept as historical record from the original review pass. Task numbers*
*below (e.g. "Task 1.2.1d", "Task 1.2.1a/b" as recommendations) predate the*
*repair pass's renumbering to 1.2.1a-h and no longer point at the tasks they*
*meant at the time — e.g. the SingletonLock test recommended below now exists*
*as Task 1.3.2a, and the git/sync-folder warning recommended below now exists*
*as Task 1.2.1c. Not rewritten in place; see plan.md directly for current state.*

- [ ] **Story 1.2.1's own acceptance criteria have no corresponding test task.**
  Every other story in this plan pairs its Given/When/Then ACs with an explicit
  test-writing task (Story 1.1.1 → Task 1.1.1b; Story 1.2.2 → covered by Task
  1.2.2a/b's own wiring plus 1.3.1's daemon-level check; Story 1.3.1 → Task
  1.3.1a). Story 1.2.1 defines three ACs — `launch(None)` byte-identical to
  today, `launch(Some(dir))` creates the dir and chmods it `0700`, and a
  `SingletonLock` collision produces the wrapped error message — but no task
  under Epic 1.2 writes a test for any of them. These are also the two most
  heavily-researched behaviors in this plan (permission hardening from
  `pitfalls.md` §3, SingletonLock UX from `ux.md` §3), so leaving them
  unverified by anything beyond manual review is a real gap, not a minor one.
  — **Recommendation**: add a Task 1.2.1d (real-Chrome, `#[ignore]`d, same
  pattern as 1.3.1a) that drives two concurrent `NativeBrowser::launch(Some(dir))`
  calls against the same dir and asserts the second returns the
  "appears to be in use" message; the `launch(None)`-unchanged and
  `0700`-permission ACs can be covered by cheaper non-Chrome-dependent checks
  (e.g. a unit test around the branch logic extracted to a pure function, or
  an `#[ignore]`d test that just inspects the created dir's mode bits).

- [ ] **The one integration test covers the less risky of the two restart triggers.**
  `requirements.md`'s own problem framing names the motivating scenario as
  "a daemon restart (upgrade, crash, machine reboot)," but Story 1.3.1's smoke
  test only exercises the graceful `"shutdown"` RPC path before restarting.
  `research/pitfalls.md` §1 asserts crash recovery "should" work based on
  general Chromium `ProcessSingleton` documentation, but flags a real edge
  case (hostname mismatch after a crash, common in containers) and this repo
  has no test proving chromiumoxide's specific launch path recovers cleanly
  from an unclean daemon death. Plan's "Unresolved Questions: None" therefore
  overstates confidence — the flagged upstream risk (issue #252, cookie
  persistence) is de-risked, but the crash-path risk pitfalls.md itself raised
  is not. — **Recommendation**: either extend Task 1.3.1a to `kill -9` the
  daemon subprocess instead of (or in addition to) calling `"shutdown"`, or
  explicitly narrow the plan's claim to "graceful-restart persistence
  verified; crash-path relies on documented Chromium behavior, not tested
  here" so the gap is visible rather than implied-covered.

- [ ] **ADR-0001 declines pitfalls.md's own recommendation #2 and offers only a doc warning in return.**
  `research/pitfalls.md` §4 recommends the profile location be "default(ed)
  and probably force(d)" away from git repos and sync-folder roots, or at
  minimum rejected/warned on at runtime, because — per the same research
  file's §3 — this directory is a "near-plaintext-at-rest credential store"
  reachable by prompt injection through any generic file-read tool the daemon
  exposes. ADR-0001 resolves the tension by requiring a literal, operator-typed
  path (reasonable) but backs the git-repo/sync-folder hazard with a README
  paragraph only (Task 1.4.1a) — nothing in the code stops an operator (or an
  LLM agent suggesting the value) from setting
  `STAPLER_MCP_BROWSER_PROFILE_DIR=$HOME/dotfiles/browser-profile` or a Dropbox
  path. — **Recommendation**: add a cheap runtime check in Task 1.2.1a/b (e.g.
  refuse or loudly warn if the resolved path's ancestry contains a `.git` dir
  or a name matching `Dropbox|iCloud Drive|Syncthing`) — the research already
  did the work of identifying the exact hazard; the plan doesn't need to
  invent new logic, just act on recommendation #2 instead of only #1.

## Minors

- Task 1.2.2a's own rationale ("clone needed because the log line also reads
  it after the move-by-value call") is inconsistent with Task 1.2.2b's
  snippet, which reads `&persistent_profile_dir` *before* the `launch(...)`
  move — i.e., in the order the plan itself describes, the `.clone()` isn't
  needed. Purely stylistic (possible `clippy::redundant_clone`), not a
  functional bug; worth tidying when the two tasks are implemented together.
- Task 1.2.1b's `chmod 0700` applies only to the top-level `user_data_dir`,
  not recursively. If an operator points the env var at a pre-existing
  directory populated under a looser umask by some other process, files
  already inside keep their old permissions. Low-likelihood for a
  freshly-designated profile dir, but worth a one-line README caveat
  alongside the corruption-recovery note Task 1.4.1a already adds.
