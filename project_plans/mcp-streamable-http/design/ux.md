# UX Design: mcp-streamable-http

This feature has no GUI. Every surface below is a developer/operator
interacting via CLI output, a config file, a log line, or a systemd/launchd
status command — Tyler, running Claude Code sessions against this daemon on
his own machine(s). Per the condensed non-interactive treatment: each surface
gets one representative sample + acceptance criteria, not a wireframe.

**Accessibility**: N/A, explicitly scoped out — every surface here is
CLI/log/config-file text output for a single local operator (no GUI, no
rendered visual layout, nothing a screen reader or keyboard-nav concern would
apply to), so WCAG/screen-reader/keyboard-navigation guidance doesn't apply.
This is a deliberate scoping statement, not a silent omission.

Grounded in `implementation/plan.md`'s actual committed surfaces (not a
hypothetical richer one): `--status` (Story 7.1.1), `--print-config` (Story
7.1.2), the `.example` systemd/launchd templates (Story 5.2.1), the auth
middleware's 401 shape (Story 4.2.1), and the bind-failure/backpressure log
lines (Epic 3.1, Story 6.1.1). Where the plan leaves a message's exact
wording unspecified, the sample below proposes concrete text consistent
with the plan's cited log-line fragments — implementers should treat the
quoted strings as a starting draft, not a locked contract.

## Surface inventory

| # | Surface | Plan reference |
|---|---------|-----------------|
| 1 | `mcp-servers.json` migration (stdio → http) | Task 7.2.1d, `ux.md` (research) §3 |
| 2 | First-run token generation | Story 4.1.1, Task 3.2.1b |
| 3 | `stapler-mcp --print-config` | Story 7.1.2 |
| 4 | `stapler-mcp --status` | Story 7.1.1 |
| 5 | Error: daemon not running | Task 3.2.1a, `ux.md` (research) §4a |
| 6 | Error: bad/missing bearer token (401) | Story 4.2.1 |
| 7 | Error: dropped connection mid-session | `ux.md` (research) §4c, Story 9.1.1 |
| 8 | Persistent-service templates (systemd/launchd) | Story 5.2.1 |

---

## First-time HTTP setup (ordered walkthrough)

The 8 surfaces above are each documented individually; this section ties them
into the one ordered path an operator actually follows the first time they
enable HTTP on a machine. Each step references its surface by number above
rather than re-describing it.

1. Start the daemon with HTTP enabled: `STAPLER_MCP_HTTP_PORT=47439 stapler-mcp --daemon` (surface 2).
2. A bearer token is auto-generated on that first start and logged (path + permissions only, never the value) (surface 2).
3. Run `stapler-mcp --print-config` to get the exact `mcp-servers.json`-shaped block, with the real token and port filled in (surface 3).
4. Paste that block into `~/.claude.json`'s user-scoped `mcpServers` entry — never the git-tracked `mcp-servers.json` — after confirming `~/.claude.json` itself isn't tracked by any dotfiles repo on this machine (surface 1).
5. Optionally install the persistent systemd/launchd unit so the daemon survives reboots/logouts instead of needing a manual foreground start (surface 8).
6. Verify everything is wired up with `stapler-mcp --status`, which confirms both the Unix socket and the HTTP port are live (surface 4).

If anything fails along the way, surface 5 (daemon not running) and surface 6 (bad/missing token) cover the two most likely stumbling points, each with its own exit path.

---

## 1. `mcp-servers.json` migration (stdio → http)

Config-file surface. The tracked file stays on stdio by default (Out of
Scope: not removing the fallback); the HTTP block is assembled by the
operator from `--print-config` output (surface 3) and pasted into
`~/.claude.json`'s user-scoped `mcpServers` entry — Claude Code's real,
verified user-level MCP config file (`code.claude.com/docs/en/mcp.md`; not
a file inside the `~/.claude/` subdirectory, which this repo's own
`CLAUDE.md` documents as a dotfiles-managed symlink and therefore unsafe
for secrets) — never the git-tracked `mcp-servers.json`, because that file
is `llm-sync`-mirrored and, per `research/ux.md` §3, Claude Code's
`${ENV_VAR}` substitution inside `headers` was found unreliable at
research time, so there is no safe way to keep the tracked file generic and
env-var-driven instead. **Before first use, the operator must confirm
`~/.claude.json` itself is not tracked** by any dotfiles repo they run on
that machine, using this exact check (verified by execution against three
real cases — outside the repo, inside and tracked, inside and untracked —
during `pm:triad-review`, 2026-09-14; a naive `git -C <repo> ls-files
--error-unmatch ~/.claude.json` is wrong here: it raises `fatal: ...
is outside repository` instead of the intended "not tracked" message
whenever the target isn't inside the repo's own working-tree directory,
which is the common case for a dotfiles repo that lives at e.g. `~/dotfiles`
and symlinks individual files into `$HOME`):

```bash
target=$(readlink -f ~/.claude.json); repo=$(readlink -f ~/dotfiles)
case "$target" in
  "$repo"/*) git -C "$repo" ls-files --error-unmatch -- "${target#$repo/}" \
             && echo "TRACKED — unsafe, gitignore it or use headersHelper instead" \
             || echo "inside the repo dir but untracked — safe for now" ;;
  *) echo "outside the repo dir entirely — cannot be tracked, safe" ;;
esac
```

(substitute the operator's actual dotfiles repo path for `~/dotfiles`); if
it comes back tracked, add it to that repo's `.gitignore` first or use a
`headersHelper` script path outside any tracked directory instead (Claude
Code supports both a literal `headers` value and a `headersHelper` script —
see README's token-distribution section, Task 7.2.1d).

```jsonc
// mcp-servers.json (tracked, unchanged default — every machine keeps working
// with zero edits even if HTTP is never set up)
{
  "mcpServers": {
    "stapler-mcp": {
      "command": "stapler-mcp",
      "args": []
    }
  }
}

// ~/.claude.json (Claude Code's own user-level config file, NOT inside
// ~/.claude/ — verify untracked by any dotfiles repo before pasting a
// token here; created/edited by pasting `stapler-mcp --print-config`'s
// "mcpServers" block into this file's existing "mcpServers" key)
{
  "mcpServers": {
    "stapler-mcp": {
      "type": "http",
      "url": "http://127.0.0.1:47439/mcp",
      "headers": { "Authorization": "Bearer 7f3a9c1e...redacted...4b2d" }
    }
  }
}
```

**Acceptance criteria**
- The tracked `mcp-servers.json` never contains a literal token — README (Task 7.2.1d) states this explicitly as a warning, not just an implication.
- README explicitly names the `${ENV_VAR}`-in-`headers` substitution as unsafe to rely on, so a reader doesn't independently reach for it as a "cleaner" alternative and hit the same unresolved upstream bug.
- README names `~/.claude.json` as the concrete file (not a generic "local override file") and includes the one-line command to verify it isn't dotfiles-tracked before pasting a token into it — this is a new, blocker-level UX finding (triad review, 2026-09-14), not present in the original surface design.
- Switching a machine from stdio to HTTP requires editing exactly one block (the `stapler-mcp` entry) in `~/.claude.json` — no changes to the tracked `mcp-servers.json` are needed to opt in.
- Reverting to stdio (rollback path) is deleting/reverting that one block — the tracked file was never touched, so there's nothing to undo there.

---

## 2. First-run token generation

Log/stdout surface, daemon-side. Fires once, the first time `--daemon`
starts with `STAPLER_MCP_HTTP_PORT` set and no token file yet exists
(Story 4.1.1, Task 3.2.1b).

```
$ STAPLER_MCP_HTTP_PORT=47439 stapler-mcp --daemon
stapler-mcp: generated new HTTP bearer token at ~/.stapler-mcp/http-token (0600)
stapler-mcp: HTTP transport listening on 127.0.0.1:47439/mcp
stapler-mcp: run `stapler-mcp --print-config` to get the mcp-servers.json block
```

**Acceptance criteria**
- The token value itself never appears in this output or in `daemon.log` (Story 4.1.1's "never logged" acceptance criterion) — only the file path and the fact that generation happened.
- The message names the exact next command (`--print-config`) rather than telling the operator to go read the token file and assemble JSON by hand.
- A second daemon start (token file already present) prints no "generated new token" line — only the "listening on" line — so the one-time nature of generation is visible in the log history itself.
- File permissions (`0600`) are stated in the message, not just enforced silently, so an operator auditing the log can confirm the security property without a separate `ls -l`.

---

## 3. `stapler-mcp --print-config`

CLI output surface. Idempotent, read-only — reads the existing token file
and the daemon's persisted `~/.stapler-mcp/http-port` file (never
`STAPLER_MCP_HTTP_PORT` from its own invoking shell — that var is scoped to
whatever process started the daemon, e.g. the systemd/launchd unit in
surface 8, and is never exported here), never generates either (Story
7.1.2's explicit second acceptance criterion).

```
$ stapler-mcp --print-config
{
  "type": "http",
  "url": "http://127.0.0.1:47439/mcp",
  "headers": {
    "Authorization": "Bearer 7f3a9c1e8d2b4f6a1c9e3d7b5a8f2c4e6b1d9a3f7c5e8b2d4a6f1c9e3d7b5a8f"
  }
}
```

```
$ stapler-mcp --print-config
stapler-mcp: no HTTP token found at ~/.stapler-mcp/http-token
stapler-mcp: start the daemon with HTTP enabled first:
  STAPLER_MCP_HTTP_PORT=47439 stapler-mcp --daemon
```

**Acceptance criteria**
- Output with a token present is valid JSON on its own (pipeable to `jq`, or directly paste-able into the override file from surface 1) — no surrounding prose mixed into stdout.
- Output with no token present is *not* JSON — it's a plain-English explanation plus the exact command to run, so a script piping this into `jq` fails loudly (empty/invalid JSON) rather than writing a bogus config silently.
- The "no token" path never fabricates a placeholder token — Story 7.1.2's acceptance criterion is explicit that a read-only command must not have generation side effects.
- Running it twice in a row (token present both times) produces byte-identical output — this is what makes it safe to re-run when setting up a second machine (research `ux.md` §3's "re-prints that block idempotently").

---

## 4. `stapler-mcp --status`

CLI output surface — the `docker ps` equivalent this project currently
lacks (research `ux.md` §1). Checks the Unix socket unconditionally, plus
an HTTP TCP-connect probe against whatever port the daemon itself persisted
to `~/.stapler-mcp/http-port` on its last startup (Task 3.2.1c) — not
`STAPLER_MCP_HTTP_PORT` read from `--status`'s own invoking shell, which the
systemd/launchd-started case (surface 8) never has set (Story 7.1.1's
acceptance-criteria cases).

```
$ stapler-mcp --status
daemon: not running
  try: systemctl --user start stapler-mcp   (or: stapler-mcp --daemon)

$ stapler-mcp --status
daemon: running (pid 48213)
http: listening on 127.0.0.1:47439

$ stapler-mcp --status
daemon: running (pid 48213)
http: not configured (no ~/.stapler-mcp/http-port — last daemon start had STAPLER_MCP_HTTP_PORT unset)

$ stapler-mcp --status
daemon: running (pid 48213)
http: not listening on 127.0.0.1:47439 (configured but unreachable)
```

**Acceptance criteria**
- The "not running" case always includes a next-step command on the same screen — no dead end requiring the operator to already know the systemd unit name or the raw `--daemon` invocation.
- The HTTP-probe check is a raw TCP connect against the persisted `~/.stapler-mcp/http-port` value, not an authenticated request — confirms the *port*, not the *token* (Story 7.1.1's explicit design choice) — so `--status` works even before an operator has a token, and never needs to read the token file.
- Exit code is nonzero when the daemon is not running and zero when it is — so this is scriptable (`stapler-mcp --status || systemctl --user start stapler-mcp`), not just human-readable prose.
- Distinguishes "daemon up, HTTP never enabled" from "daemon up, HTTP enabled but port unreachable" — these are different failure classes (config choice vs. real fault) and collapsing them into one "http: down" line would send an operator down the wrong troubleshooting branch.

---

## 5. Error: daemon not running

Cross-boundary surface — the failure originates in Claude Code's HTTP MCP
client (`stapler-mcp` doesn't control this text), but the *recovery path*
is this project's to document (Task 3.2.1a, research `ux.md` §2/§4a). This
is the sharpest net-new failure mode versus today's stdio design (nothing
auto-spawns the daemon anymore), so it gets the most explicit treatment.

```
# What Claude Code shows (client-side, not controlled by stapler-mcp;
# confirmed pattern from anthropics/claude-code#11633, #39790):
Failed to connect to MCP server "stapler-mcp"

# What the README troubleshooting section (Task 7.2.1c) tells the operator to run, in order:
$ systemctl --user status stapler-mcp        # is the process up at all?
$ stapler-mcp --status                       # confirms socket + HTTP port together
$ systemctl --user start stapler-mcp         # (or: stapler-mcp --daemon)
```

**Acceptance criteria**
- README's troubleshooting section lists these three commands in this exact order (process check → protocol check → fix) — matches research `ux.md` §2's explicit recommendation and Task 7.2.1c.
- Binding `127.0.0.1:<port>` means this fails fast (ECONNREFUSED) rather than hanging — the daemon-down case must never look like a slow/stuck server, which research flagged as strictly worse (issue `#21721`'s "silently stops reconnecting" anti-pattern).
- The exit path always terminates in a command that starts the daemon — an operator following the three steps above never reaches a dead end asking "now what."
- The stdio fallback (pointing `mcp-servers.json` back at `command`/`args`, surface 1) is documented in the same troubleshooting section as the escape hatch when HTTP is stuck for a reason the three commands above don't resolve (research `ux.md` §4c's "backstop" framing).

---

## 6. Error: bad or missing bearer token (401)

HTTP response + log-line surface (Story 4.2.1). Must be a clean,
distinguishable 401 — not a generic connection failure — both to the
client and in `daemon.log`.

```
$ curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:47439/mcp
401

$ curl -s http://127.0.0.1:47439/mcp -H 'Authorization: Bearer wrong-token'
{"error": "invalid bearer token"}

# daemon.log:
2026-09-14T10:03:21Z rejected unauthenticated HTTP request from 127.0.0.1:53412: missing Authorization header
2026-09-14T10:03:44Z rejected unauthenticated HTTP request from 127.0.0.1:53414: invalid bearer token
```

**Acceptance criteria**
- The 401 body distinguishes "missing header" from "wrong token" (Story 4.2.1's two sub-cases) — an operator debugging a stale token in `mcp-servers.json` sees "invalid bearer token," not an ambiguous generic failure that could equally mean "not authenticated at all."
- The rejected-request log line is visually distinct from a tool-call error line (different message shape/prefix), satisfying requirements.md's Observability Requirement to `grep` these apart from ordinary tool failures.
- Neither the log line nor the HTTP response body ever echoes the presented (wrong) token value back — only the failure class, per Story 4.2.1's explicit "never the presented value" constraint.
- Exit path: the operator's fix is always "re-run `stapler-mcp --print-config` and re-paste" (surface 3) — the troubleshooting doc (Task 7.2.1d) states this as the standard recovery for a 401, not just "check your token" with no pointer to the command that regenerates the correct paste-able block.

---

## 7. Error: dropped connection mid-session (daemon restart)

The residual, only-partially-controllable risk research flagged as most
unresolved (`research/ux.md` §4c, Story 9.1.1). `stapler-mcp`'s side is
spec-correct session handling; Claude Code's reconnect behavior on the
receiving end is the part this project cannot fix and must instead document
as a known-risk with an escape hatch.

```
# Daemon-side: what stapler-mcp controls — a clean 404 on a request
# carrying an unknown/expired session id (MCP spec: this MUST cause the
# client to re-initialize a new session).
$ curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:47439/mcp \
    -H 'Mcp-Session-Id: stale-id-from-before-restart'
404

# Client-side (Claude Code): may or may not auto-reconnect — confirmed
# unresolved in the wild for this exact scenario (anthropics/claude-code#21721).
# If it doesn't: the operator's escape hatch is the stdio fallback (surface 1),
# not a Claude Code restart-and-hope.
```

**Acceptance criteria**
- Under this plan's chosen `stateful_mode: false` (Pattern Decisions), there is no persistent session to go stale in the first place for ordinary tool calls — this failure class is scoped down to whatever `rmcp`/axum-level behavior remains, not eliminated as a concept (documented, not assumed away).
- Daemon restarts (via `Restart=on-failure`) are graceful where possible — `SIGTERM` handling (Story 5.1.1) runs the same cleanup the existing `SHUTDOWN_TOOL` path does, rather than an unclean kill, minimizing how often this failure shape is even triggered.
- `NOTES.md` records the manual verification outcome (Task 9.1.1a) either way — "Claude Code recovered transparently" or "required a restart, treated as accepted residual risk" — so this isn't left as an assumed-fine unknown at ship time.
- Exit path when the client doesn't recover on its own: fall back to the stdio transport (surface 1) rather than requiring a full Claude Code restart — this is stated explicitly in README's troubleshooting section, not left implicit.

---

## 8. Persistent-service templates (systemd/launchd)

Setup-file surface — `.example` templates the operator copies once per
machine (Story 5.2.1), following the existing `docs-mcp-server.service.example`
convention already in `scripts/`.

```ini
# scripts/stapler-mcp.service.example → ~/.config/systemd/user/stapler-mcp.service
[Unit]
Description=stapler-mcp daemon (shared MCP backend, HTTP + Unix socket)
After=network.target

[Service]
Type=simple
Environment=STAPLER_MCP_HTTP_PORT=47439
ExecStart=%h/.cargo/bin/stapler-mcp --daemon
Restart=on-failure
RestartSec=5
StartLimitBurst=5
StartLimitIntervalSec=60

[Install]
WantedBy=default.target
```

```
$ systemctl --user daemon-reload
$ systemctl --user enable --now stapler-mcp
$ stapler-mcp --status
daemon: running (pid 48213)
http: listening on 127.0.0.1:47439
```

**Acceptance criteria**
- The template header comment states the exact copy/enable/verify sequence (mirroring the existing `docs-mcp-server.service.example` header convention) — an operator follows 3 commands and lands on `--status` output confirming success, not a silent "hope it worked."
- `Restart=on-failure` + `RestartSec`/`StartLimitBurst` are present by default in the shipped template, not left for the operator to add — research `ux.md`'s bottom line treats auto-restart as load-bearing for the whole UX trade-off, not an opt-in nicety.
- The macOS `launchd` plist template's `KeepAlive`/`ThrottleInterval` fields are the documented equivalent of the same restart policy — an operator on either OS gets an equally self-healing setup, not a weaker one on macOS.
- Both templates are cross-referenced from README's "Running as a persistent service" section (Task 5.2.1c) — an operator reading the README's troubleshooting or setup flow finds these files without already knowing `scripts/` exists.

---

## Cross-cutting: no dead ends

Every error state above resolves to a concrete next action, not a "contact
support"-shaped dead end:

| Error state | Exit path |
|---|---|
| Daemon not running (§5) | `systemctl --user status` → `--status` → `systemctl --user start` (or `--daemon` directly) |
| Bad/missing token (§6) | `stapler-mcp --print-config` → re-paste into the local override |
| Dropped session mid-restart (§7) | Wait for `Restart=on-failure` to recover; if the client doesn't reconnect, fall back to the stdio entry in `mcp-servers.json` (§1) |
| HTTP port already in use (bind failure, not a standalone surface above — Task 3.1.1d) | Daemon logs `"HTTP port {port} already in use — continuing with stdio/socket transport only"` and keeps running on stdio; operator either frees the port or accepts stdio-only for that machine |
| No token generated yet, `--print-config` run early (§3) | Message names the exact `STAPLER_MCP_HTTP_PORT=... stapler-mcp --daemon` command to run first |
