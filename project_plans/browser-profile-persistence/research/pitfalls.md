# Pitfalls: persistent Chrome `user_data_dir` reuse via chromiumoxide/CDP

item_id: 10f6836c-71fc-4db2-8bc8-117d65da6a2d

Repo uses `chromiumoxide = "0.9"` (`crates/native/Cargo.toml:12`). Current launch code
(`crates/native/src/browser.rs:337-364`) already generates a unique
`$TMPDIR/stapler-mcp-chromium-<pid>-<now_millis>` dir per daemon process specifically to
avoid the shared-default-dir collision described below — see the comment at
`crates/native/src/browser.rs:341-347`.

## 1. `SingletonLock` and crash recovery

Chrome's `ProcessSingleton` writes a `SingletonLock` file (on Linux, a symlink of the
form `<hostname>-<pid>`) into the profile dir at startup and checks it on every launch.

- **Normal crash recovery works.** On POSIX, `ProcessSingleton::Create()`
  (chromium `chrome/browser/process_singleton_posix.cc`) resolves the symlink, extracts
  the embedded hostname + pid, and checks whether that pid is still alive. If the
  previous process is dead, Chrome deletes the stale lock and proceeds — so a daemon
  that crashes and is later relaunched against the same `user_data_dir` should recover
  automatically in the common case (this is standard, long-shipped Chromium behavior,
  not something chromiumoxide adds or removes).
- **Where it does NOT recover cleanly:** the hostname embedded in the lock symlink must
  match the current hostname for the pid-liveness check to even run. In containers
  (Docker/CI runners) where the hostname changes between the crash and the restart —
  or where the profile dir is bind-mounted into a container with a different
  hostname/pid namespace than where it was created — Chrome treats the lock as held by
  a *different host* and refuses to start, surfacing as
  `Failed to create <dir>/SingletonLock: File exists (17)`. Recovery in that case is
  manual: delete `SingletonLock` (and generally also `lockfile`, another guard file in
  newer Chrome versions) before relaunch. This exact failure signature shows up
  repeatedly in the wild for tools that launch Chrome with a fixed/reused
  `user_data_dir` in CI or containers.
- **Concurrent launches against the same dir are not crash-related but hit the same
  lock:** two live Chrome processes against one `user_data_dir` — e.g. a stray daemon
  process from a previous run that never exited, or an integration test that launches a
  second `NativeBrowser` before the first is dropped — race `SingletonLock` and one
  launch fails outright. This is exactly the failure the existing code comment at
  `crates/native/src/browser.rs:341-347` calls out for chromiumoxide's own default
  shared path (`$TMPDIR/chromiumoxide-runner`, used when `user_data_dir` is left unset).
  **Implication for the opt-in named-profile feature:** if a user runs two daemon
  instances (e.g. two terminal sessions) pointed at the same named profile, the second
  `launch()` will fail with this same error — the design needs either an explicit "in
  use" error surfaced clearly, or a documented single-instance-per-named-profile
  constraint.

## 2. Chrome version/schema migrations corrupting a long-lived profile

- Chrome's profile format (prefs schema, `Local State`, extension/history DB versions)
  is versioned and **migrations are one-directional**: a profile touched by a newer
  Chrome binary is not guaranteed to open cleanly with an older binary. Since this
  daemon uses whatever Chrome/Chromium binary chromiumoxide finds on the host (or
  auto-downloads), and that binary can change out from under a long-lived named profile
  via routine OS package upgrades (`pacman -Syu`, `apt upgrade`, Homebrew auto-update),
  a profile created under Chrome N can end up opened by Chrome N+2 weeks later — normally
  fine (forward migration), but a *downgrade* (e.g. pinning an older chromiumoxide-bundled
  Chrome after having used system Chrome) is the scenario that risks corruption/refusal
  to open.
- **Concrete evidence this bites chromiumoxide specifically:** closed upstream issue
  [mattsse/chromiumoxide#243](https://github.com/mattsse/chromiumoxide/issues/243)
  ("Chromiumoxide stopped working with the latest version of Chrome") documents a real
  case where a Chrome auto-update (to 129.0.6668.90) broke the CDP websocket wire
  contract chromiumoxide 0.7 depended on (`Failed to deserialize WS response data did
  not match any variant of untagged enum Message`), reproduced independently by the
  reporter and a colleague after their Chrome updated. This isn't profile-schema
  corruption, but it's the same underlying hazard: a long-lived daemon's assumptions
  about "the Chrome version" silently drift, and the failure surfaces only after a host
  Chrome update, disconnected from any chromiumoxide/daemon code change. A named,
  long-lived profile makes this more likely to matter in practice (a short-lived
  ephemeral profile is torn down before host Chrome has a chance to update underneath
  it; a persistent one lives across arbitrarily many host upgrades).
- Recommend documenting that a corrupted/unopenable named profile's recovery path is
  "delete the profile directory and let it be recreated" (same recovery as an ephemeral
  one) rather than attempting any repair.

## 3. Security: session cookies / saved passwords at rest

- `user_data_dir/Default/Cookies` and `user_data_dir/Default/Login Data` are SQLite
  databases holding session cookies and any saved passwords respectively. Values are
  encrypted, but **on Linux, without a running OS keyring (GNOME Keyring / KWallet)
  backing `libsecret`, Chrome silently falls back to "Basic" encryption — AES-128-CBC
  with a hardcoded, publicly-known key (historically the literal string `"peanuts"`)**.
  This is long-documented Chromium behavior (the fallback exists so Chrome still works
  headless/on minimal Linux setups) and is exactly why cookie/password extraction tools
  (`browser_cookie3`, various "chrome password decryptor" utilities) work out of the box
  against headless/server Chrome profiles with no keyring — no OS-level secret needed,
  just the SQLite file and the DB's per-value nonce/IV.
- This MCP daemon runs headless on a workstation/server with no guarantee a keyring
  daemon is present (very likely absent on a minimal server, likely present on a desktop
  session), so a persistent profile's cookie store should be treated as **near-plaintext
  at rest** unless a keyring is explicitly verified as active — not just "encrypted, so
  it's fine."
- Because this daemon is driven by an LLM agent via MCP tools, and the whole point of
  the feature is to keep authenticated sessions alive, the attack surface is: any prompt
  that gets the agent to read/exfiltrate `<profile_dir>/Default/Cookies` (or the whole
  profile dir) via a generic file-read tool, or to `stapler_browser_evaluate` JS that
  reads `document.cookie` on an already-authenticated page and echoes it back, yields a
  live session token without needing to crack the DB encryption at all. The persistent
  profile raises the stakes of that class of prompt injection specifically because the
  cookies now outlive a single conversation/daemon restart — a leaked session token from
  an ephemeral profile is only ever as valuable as one login; from a persistent one it's
  a standing credential.
- Mitigation directions worth flagging for the plan (not decided here): keep the
  profile dir out of reach of generic filesystem tools, restrict it to `0700` on Linux
  (verify chromiumoxide/Chrome don't rely on wider perms), and treat the named profile's
  existence itself as sensitive configuration state deserving a warning in the tool's
  description so the agent (and a human skimming its actions) understands it's handling
  long-lived credentials, not a throwaway sandbox.

## 4. Secrets-in-git / secrets-in-backup hazard from profile location

- The requirements doc's "smallest change" framing (an opt-in named `user_data_dir`) is
  exactly the kind of feature likely to get a default location chosen for developer
  convenience — e.g. `./chrome-profile` inside the repo, or `~/Dropbox/...` /
  `~/Library/CloudStorage/...` for "it survives reinstalls." Either default is a
  concrete hazard:
  - **Inside the repo:** `Cookies`/`Login Data` SQLite files would be one
    accidental-`git add -A`/missing-`.gitignore`-entry away from landing in a commit —
    and unlike a `.env` file, `Cookies` doesn't look like a secret at a glance, so it's
    far less likely to get caught by the usual "does this look like a secret" review
    pass or standard secret-scanners (which mostly regex for token *shapes*, not binary
    SQLite blobs).
  - **Inside a synced folder (Dropbox/iCloud/Syncthing):** the whole point of those
    tools is continuous background sync of anything written under them — a Chrome
    profile actively being written to (cookies update on every authenticated request)
    would sync session tokens to every device and to the cloud provider's storage,
    with no user-facing indication distinct from any other synced file.
- The fix (proper XDG state dir — `$XDG_STATE_HOME` / `~/.local/state/stapler-mcp/` on
  Linux, `~/Library/Application Support/` on macOS) is cheap and should be the *only*
  default; if a user-specified path is allowed at all, the tool should probably reject
  or at least loudly warn on paths that resolve inside a detected git repo or common
  sync-folder roots (`Dropbox`, `iCloud Drive`, `Syncthing` are the common ones to
  pattern-match).

## 5. Known chromiumoxide issues relevant to persistent profiles

Checked via `gh api search/issues?q=repo:mattsse/chromiumoxide+...` against the real
upstream repo (not just web-search summaries, which surfaced several oddly-specific
citations from small unrelated third-party repos that could not be independently
verified and are not included here):

- [#243](https://github.com/mattsse/chromiumoxide/issues/243) (closed) — CDP wire
  protocol broke against a newer Chrome release; see point 2 above.
- [#252](https://github.com/mattsse/chromiumoxide/issues/252) ("Cookies Issue", closed)
  — reporter says cookies aren't persisting into a supplied `user_data_dir`. **Closed
  without a confirmed root cause or fix** — the maintainer asked for a minimal repro and
  none was provided before close. Treat as an open question, not a resolved bug: worth
  a smoke test (launch with a named dir, log in somewhere, restart, confirm the session
  survives) before shipping this feature rather than trusting `user_data_dir` persistence
  blindly.
- [#293](https://github.com/mattsse/chromiumoxide/issues/293) (open) —
  `browser.clear_cookies()` / `browser.set_cookies(vec![])` hang indefinitely when
  `user_data_dir` is set (reporter's repro includes `.user_data_dir("browser_data")`
  explicitly). Relevant if any profile-management UX (e.g. a "clear this named profile's
  cookies" tool/flag) is considered — that code path may not be safe to build on
  chromiumoxide 0.9 today.
- No open or closed upstream issue specifically mentions `SingletonLock` by name — the
  lock-collision failure mode in section 1 is well-documented general Chromium behavior
  (and this repo already designed around it), not a chromiumoxide-specific defect.

## Summary of concrete recommendations for the plan phase

1. Document (and ideally detect) the "profile already in use" `SingletonLock` failure
   with a clear error message rather than letting chromiumoxide's raw CDP launch error
   surface — the current `PortError::Other(e.to_string())` wrapping will produce Chrome's
   raw `Failed to create .../SingletonLock` text, unhelpful to an agent/user.
2. Default (and probably force) the named-profile location under an XDG-style state
   dir; disallow or warn on paths under the repo or common sync-folder roots.
3. Smoke-test actual cross-restart cookie persistence before relying on it — upstream
   #252 suggests it isn't unconditionally reliable, though it lacks a confirmed repro.
4. Call out the plaintext-ish-at-rest cookie/password store explicitly in whatever
   surfaces this feature to a user (tool description / docs), since the daemon is
   LLM-agent-driven and the profile is now a standing credential rather than a
   throwaway one.
5. If any "clear cookies for this profile" management is ever added, check upstream
   #293 isn't still open/hanging on the chromiumoxide version in use at that time.
