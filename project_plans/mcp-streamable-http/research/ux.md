# Research: Developer/Operator UX — mcp-streamable-http

Scope: this item has no end-user GUI. "UX" here means the experience of Tyler
configuring `mcp-servers.json` and operating the daemon's lifecycle, plus the
experience of every Claude Code session/subagent that depends on the daemon
being reachable. Findings below are organized by the five questions in the
task brief.

## 1. Comparable UX patterns — what's the "`docker ps`" for this setup?

**Today, this repo has no such command.** `crates/cli/src/main.rs` parses no
subcommands beyond a bare `--daemon` flag check (`main.rs:26`); there is no
`stapler-mcp status`/`--ping` today. The only "is it up" signal is indirect:
a thin client's `ensure_daemon` ping-or-spawn dance (`crates/core/src/client.rs:63-106`).
This is a real gap the plan phase should close regardless of the HTTP work,
but it becomes load-bearing once the daemon is a persistent service instead
of a lazily-spawned one.

Comparable tools split into two families:

- **Docker Desktop's MCP Toolkit** — status is surfaced two ways: the CLI
  (`docker mcp` / `claude mcp list` shows each server as `Connected` /
  not) and the GUI (Settings → Developer → MCP Servers shows a running
  badge). The underlying gateway process's own health isn't something the
  user inspects directly — Docker's own container lifecycle (`docker ps`)
  is the true source of truth, and the MCP-level status is a thin layer on
  top of it. The take-away for `stapler-mcp`: pair a **client-facing**
  status view (does the MCP handshake succeed) with a **process-facing**
  one (is the daemon process alive) — they answer different questions and
  a user needs both when triaging.
- **LSP daemons (rust-analyzer, gopls)** — both lazily spawn per-workspace
  and both have well-known "silently stuck" failure modes: rust-analyzer's
  first-index delay is frequently mistaken for a hang, and a crashed
  server goes into a "broken" set that isn't retried until an explicit
  `lsp restart`. The pattern worth borrowing is *the editor tells the user
  the server state explicitly* (a status-bar item — "indexing", "server
  crashed, click to restart") rather than leaving the symptom (no
  diagnostics appear) to imply the cause. `stapler-mcp` has no
  equivalent UI surface; the nearest analog is Claude Code's own `/mcp`
  command and its connection-status line, which the daemon can't control
  directly but whose accuracy depends on the daemon returning clear
  transport-level errors (see §4).

**Recommendation carried into scope**: add a lightweight `stapler-mcp
--status` (or `--ping`) subcommand that (a) checks the lockfile/socket the
way `ensure_daemon` already does, and (b) once HTTP exists, hits the
daemon's HTTP health endpoint too. This is the `docker ps` equivalent this
project currently lacks, and the natural place to also print "not running;
start it with `systemctl --user start stapler-mcp`" guidance (see §2).

## 2. Mental model shift: from in-band stdio error to "is the background service running"

**Today's diagnostic path is synchronous and in-band by construction.**
Every tool call goes through `call_daemon` (`crates/cli/src/thin_client.rs:35-63`),
which calls `ensure_daemon` first. If the daemon is unreachable,
`ensure_daemon` spawns it and polls with backoff (`client.rs:82-105`); if
that fails, the thin client returns `Err("ensure daemon: {e}")` as the tool
call's error string, and Claude Code surfaces that directly as the failed
tool call's error. There's no separate step for the user — the fix (spawn a
daemon) is automatic, and the diagnostic (if it still fails) attaches to the
exact tool call that triggered it.

**Once a persistent HTTP daemon replaces per-session spawning, this
guarantee disappears.** Nothing auto-spawns the daemon anymore; if it's not
running, the *first* symptom a user sees is whatever Claude Code's MCP HTTP
client does on connection failure — and per real-world reports this is
inconsistent even for mature clients. Anthropic's own Claude Code has open
issues where an HTTP MCP server's dropped connection produces confusing
downstream errors: `#39790` (stateless server, 405 on GET, "Failed to
connect"), `#11633` ("Failed to connect" despite the server working when
tested manually), and `#21721` (HTTP transport silently stops working
after ~89 minutes idle, with logs showing "HTTP connection dropped" /
"No transport found for sessionId" but no user-visible reconnect attempt).
These are not hypothetical — they're the exact failure class this item is
about to introduce for `stapler-mcp`, at a point where today's design has
zero occurrences of it (there is no "dropped connection" state in a
per-call stdio+socket round trip).

**Best-practice framing**: make "is the background service running" a
**first-checked, well-documented, single command** rather than something
inferred from a cryptic transport error — because the alternative (users
independently discovering `systemctl --user status stapler-mcp` /
`launchctl print gui/$(id -u)/com.tstapler.stapler-mcp` only after
Claude Code's own error is unhelpful) is exactly the diagnosis path LSP
users complain about with stuck servers. Concretely:

- Document the systemd/launchd status command as **step 1** of any
  connection-troubleshooting section in the README, right next to the
  new `--status` subcommand from §1 — give both, since `systemctl status`
  confirms the *process* and `--status`/an HTTP health check confirms the
  *protocol*.
- Prefer **connection-refused over silent hang** as the failure signature
  when the daemon isn't running: binding `127.0.0.1:<port>` means an
  absent daemon produces an immediate ECONNREFUSED at the TCP layer (fast,
  legible), which is strictly better than a slow timeout. This falls out
  of the transport choice already made in the requirements (`Bind to
  127.0.0.1:<port>`) — no extra design needed, just don't accidentally
  make Claude Code retry-with-backoff long enough that it *feels* like a
  hang (see `#21721`'s "silently stops reconnecting" as the anti-pattern:
  worse than a fast failure is a slow one that never resolves).
- Auto-restart (`Restart=on-failure` / `KeepAlive` — already Scope per
  requirements.md) matters more here than it would for a stateless tool,
  specifically *because* the diagnosis path got harder: if the service
  usually self-heals, users rarely need to learn the new mental model at
  all. Treat the launcher's auto-restart as a UX mitigation, not just an
  ops nicety.

## 3. Setup/config UX: `command`/`args` → `{"type":"http", "url":..., "headers":...}`

**Comparable pattern — Jupyter's token-on-first-run.** JupyterLab generates
a cryptographic token at first startup, prints it to stdout, and folds it
into the URL it tells you to open (`http://localhost:8888/lab?token=...`);
after that first paste, it's invisible — the browser remembers the URL.
There's no separate "go get your token" step; the token is handed to you
at the moment you need it. This is the model worth emulating over a design
where the user has to manually locate a token file and hand-copy it into
`mcp-servers.json`.

**Applied to `stapler-mcp`**, the equivalent first-run flow is:

1. First `stapler-mcp --daemon` start (whether from a launcher unit or
   manually) generates a bearer token if none exists yet, writes it to a
   file under `~/.stapler-mcp/` with `0600` permissions (the natural home
   given `daemon.lock`/`daemon.sock` already live there per
   `crates/native/src/lock.rs`/`paths.rs`), and prints the exact
   `mcp-servers.json` HTTP block (URL + header) to stdout/log on first
   generation only.
2. A companion `stapler-mcp --print-config` (or folded into the `--status`
   subcommand from §1) re-prints that block idempotently, so a user
   setting up a second machine or re-reading the README doesn't need to
   `cat` the token file and hand-assemble JSON — copy-paste the whole
   block.
3. This repo has **no existing secret-manager integration** to build on —
   grep across the tree found no 1Password/keychain usage; that tooling
   (`secrets` Ansible role, `op` CLI) lives in the sibling `dotfiles` repo
   and is a machine-bootstrap concern, not something `stapler-mcp` itself
   currently touches. A file-with-restrictive-permissions default (like
   Jupyter's own approach, and like SSH host keys) is the pragmatic
   plan-phase answer to requirements.md's open question on token storage
   — 1Password custody is a legitimate *future* enhancement (the user
   already has the tooling), but wiring it in is new integration scope
   this item's Appetite doesn't obviously budget for.

**Manual copy-paste pain, scoped**: the token only needs to move twice —
daemon → token file (automatic) and token file → `mcp-servers.json`
(one manual paste per consumer machine, same as any bearer-token API key
today). That's materially less painful than, say, OAuth device-code flows,
and is proportionate to a single-user local tool. The main risk is *silent
staleness*: if the daemon ever regenerates the token (e.g., a "rotate"
feature) without invalidating cleanly, whatever's in `mcp-servers.json`
goes stale with a confusing 401 (see §4b) — plan phase should treat token
rotation as out of scope unless explicitly designed, per requirements.md's
own framing of this as a deferred decision.

## 4. Error states, from the calling Claude Code session's perspective

**(a) Daemon not running, no launcher installed.** Today: invisible,
because `ensure_daemon` auto-spawns. Tomorrow: Claude Code's HTTP client
gets ECONNREFUSED on `127.0.0.1:<port>` at MCP-server-startup time (when
Claude Code itself tries to connect), which Claude Code already renders as
a connection failure for that server (confirmed pattern in `#11633`/`#39790`
research above — client-side "Failed to connect" is the existing shape of
this error, not something `stapler-mcp` needs to invent). What
`stapler-mcp` *can* control is making the failure legible past that point:
the README's troubleshooting section (Scope item: rewrite the architecture
doc) should say, in order, "1) is the daemon process running
(`systemctl --user status stapler-mcp` / `launchctl print ...` /
`stapler-mcp --status`), 2) if not, `systemctl --user start stapler-mcp`
(or run `stapler-mcp --daemon` directly if no launcher is installed)."
This is the single most important documentation deliverable from a UX
standpoint, because it's the one failure mode that has literally no
existing analog in the current stdio design for users to have already
learned.

**(b) Bearer token wrong/missing.** Should be a distinct, unambiguous
**401**, not folded into a generic transport error — this is exactly the
distinction requirements.md's own Observability Requirements section
already calls for ("Log rejected \[...\] requests distinctly from tool-call
errors"). On the Claude Code side, an HTTP 401 from an MCP server is a
well-trodden path (OAuth-gated servers hit this constantly), so the client
UX is already reasonably good here — the daemon's job is just to return a
clean 401 with a body that says *why* (missing header vs. token mismatch)
rather than a bare connection close, so the *daemon's own log* (not just
Claude Code's UI) gives Tyler something to grep for when a token was
rotated/misconfigured.

**(c) Daemon restarts mid-session (dropped SSE connection).** This is the
sharpest gap found in research: Claude Code's own issue tracker documents
this exact scenario going wrong in the wild — `#21721` (HTTP MCP
connection silently stops reconnecting after an idle drop, no user-visible
signal, requires a full Claude Code restart) and a related class of bugs
(`#11868`, others) where a client's SSE-404-after-restart handling doesn't
correctly trigger the MCP-spec-mandated "start a new session" path (a
`404` on a request carrying `Mcp-Session-Id` MUST cause the client to
re-`initialize`). Two implications for this item:

- The daemon-side implementation should follow the MCP spec precisely on
  session-id handling (return a clean `404` for an unknown/expired
  session, not a hang or a malformed response) — that's necessary but,
  per the evidence above, **not sufficient**, since Claude Code's client
  has had bugs on the *receiving* end of exactly this signal.
  Plan/verify phases should include a manual test: kill `-9` the daemon
  mid-session, restart it, and confirm Claude Code actually recovers
  (auto-reconnects) rather than requiring the user to restart Claude Code
  — this is a real, not hypothetical, risk given the linked issues, and
  worth flagging as a residual risk in the plan even if the daemon side
  is spec-correct.
  - **Open question this research could not resolve**: whether restarting
    the daemon under a systemd/launchd `Restart=on-failure` policy (as
    opposed to a clean shutdown) surfaces to Claude Code any differently
    than the idle-drop scenario in `#21721` — likely the same failure
    shape (dropped SSE, no session), but this should be verified rather
    than assumed once the launcher unit exists.
- Because of that residual client-side risk, keeping the stdio thin-client
  path available as a fallback (already Out of Scope to remove, per
  requirements.md) is doing real UX work, not just architectural
  hedging: a user who hits a stuck HTTP session has a working escape
  hatch (point `mcp-servers.json` back at `command`/`args`) instead of
  restarting Claude Code and hoping.

## 5. Job-to-be-done and the trade-off it introduces

**Functional job**: fewer OS processes and lower aggregate RSS when many
Claude Code sessions/subagents run concurrently — the daemon already holds
the heavyweight state (browser pool, embedder, caches); this item removes
the *N* thin-client processes (~4MB RSS each per requirements.md's Baseline)
that today exist purely to proxy stdio↔socket.

**Emotional/social job**: per the item's own origin (issue #37, cited in
requirements.md's Problem Statement), this user has flagged process-count
bloat as something that visibly bothered him — the job is closer to "stop
seeing a pile of `stapler-mcp` processes in `ps`/Activity Monitor and
feeling like something is wasteful/leaking" than a measured performance
complaint. Confirming that framing matters for scoping the UX bar: the
target user (Tyler, or a Claude Code session/subagent acting for him)
values *not noticing the infrastructure at all* — the RSS problem is
currently silent-but-visible-if-you-look; the fix should not trade it for
a problem that's loud by default.

**The trade-off, stated plainly**: today's failure mode (process count) is
low-severity and passive — nothing breaks, it just looks wasteful. The
HTTP+persistent-daemon design trades that for a failure mode that is
higher-severity and active: if the daemon isn't running, *every* tool call
across *every* session fails, immediately, until someone notices and
restarts it. That's a strictly worse failure mode in isolation — the
question is whether the mitigations already in scope are sufficient to
keep its *likelihood* low enough that the trade is worth it:

- Auto-restart via systemd/launchd (`Restart=on-failure`/`KeepAlive`,
  already in Scope) directly targets likelihood — most crashes self-heal
  before a human notices.
- A documented, single-command diagnostic (§1/§2) targets
  time-to-recovery when auto-restart doesn't cover the case (e.g., no
  launcher installed at all, or the machine hasn't been bootstrapped with
  the unit yet).
- Keeping the stdio path alive as a fallback (§4c) is the actual backstop
  — it converts "daemon down, no HTTP reachable" from a hard blocker back
  into "works, just spawns a process again," which is the *original*
  low-severity failure mode. In other words, the fallback isn't only an
  architecture-migration safety net; it's the mechanism that keeps this
  redesign from being a strict UX regression for a user who hasn't yet
  installed the launcher unit on a given machine.

**Bottom line**: the trade is justified only if (1) the launcher/auto-restart
piece ships as more than a "documented example" (requirements.md currently
leaves "whether this repo installs it automatically" as a plan-phase call —
this research recommends resolving that toward "installed by default via
the sibling dotfiles repo's bootstrap," not left as opt-in copy-paste, given
how much of the UX case above depends on auto-restart actually being
present) and (2) the stdio fallback genuinely stays first-class (not just
technically present but undocumented/bit-rotted) for the migration window.
Neither is guaranteed by the requirements doc as currently scoped — both
are UX-driven asks for the plan phase, not just engineering nice-to-haves.

## Sources

- Docker MCP Toolkit: <https://docs.docker.com/ai/mcp-catalog-and-toolkit/get-started.md>, <https://www.docker.com/blog/mcp-toolkit-mcp-servers-that-just-work/>
- MCP transports (stdio vs Streamable HTTP), auth model: <https://dev.to/zoricic/understanding-mcp-server-transports-stdio-sse-and-http-streamable-5b1p>, <https://archestra.ai/blog/stdio-vs-streamable-http-mcp>
- MCP Streamable HTTP session/reconnect spec behavior: <https://modelcontextprotocol.io/specification/2025-11-25/basic/transports>
- Claude Code HTTP MCP transport bugs (connection-refused/dropped-session UX): <https://github.com/anthropics/claude-code/issues/39790>, <https://github.com/anthropics/claude-code/issues/11633>, <https://github.com/anthropics/claude-code/issues/21721>
- Related dropped-session/reconnect bug in another MCP client: <https://github.com/danny-avila/LibreChat/issues/11868>
- Jupyter Notebook/Lab token-on-first-run security model: <https://jupyter-notebook.readthedocs.io/en/6.5.3/security.html>
- systemd credentials pattern (context for launcher-unit token handling): <https://systemd.io/CREDENTIALS/>
- In-repo: `crates/cli/src/main.rs:25-26`, `crates/cli/src/thin_client.rs:35-63`, `crates/core/src/client.rs:63-106`, `README.md:19-58` (architecture section), `project_plans/mcp-streamable-http/requirements.md`
