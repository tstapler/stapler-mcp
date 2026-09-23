# Build vs. Buy: persistent `user_data_dir` across daemon restarts

item_id: 10f6836c-71fc-4db2-8bc8-117d65da6a2d

## Ground truth recap

`crates/native/src/browser.rs:337-357` (`NativeBrowser::launch()`) already builds a
`chromiumoxide::BrowserConfig` via `.user_data_dir(&user_data_dir)` — the flag is
already wired end to end. The only thing hardcoded is the *path*: a fresh
`$TMPDIR/stapler-mcp-chromium-{pid}-{timestamp}` directory on every launch,
scoped by pid+timestamp specifically to avoid two daemons colliding on Chrome's
`SingletonLock`/`ProcessSingleton` (see the comment at browser.rs:339-346). The
change in scope is: make that path a stable, named, config-driven location for
users who opt in, instead of always deriving it from pid+timestamp.

`chromiumoxide = "0.9"` (`crates/native/Cargo.toml:12`). No `dirs`/`directories`/
`xdg` crate is currently in the dependency tree (confirmed via `Cargo.toml` grep
across all four crates and `Cargo.lock`).

## 1. Existing OSS library — chromiumoxide / chromiumoxide_cdp

**What's there:** `BrowserConfig` exposes `user_data_dir` as a plain
`Option<PathBuf>` builder field
([docs.rs](https://docs.rs/chromiumoxide/latest/chromiumoxide/browser/struct.BrowserConfig.html)).
That's it — no profile-directory resolution helper, no XDG-path helper, no
lock-detection/recovery logic. chromiumoxide's own out-of-the-box default (used
when `user_data_dir` is left unset) is a single fixed path,
`$TMPDIR/chromiumoxide-runner`, shared by every process that doesn't override
it — which is exactly the collision this codebase's comment already documents
and works around. chromiumoxide does not solve "multiple launches, one stable
profile" for you; it only gives you the flag to point at a path you manage
yourself.

**Is there a reason not to use the builder method directly?** No — it's the
only mechanism that exists, and the codebase already calls it. There's nothing
to swap in "instead of" `.user_data_dir()`.

**The adjacent question — an XDG-aware path crate (`dirs`/`directories`) for
resolving *where* the named profile lives:** neither crate is in the tree
today. `dirs` is the low-level, actively-maintained option (added
`XDG_STATE_HOME`/`state_dir()` support); `directories` is a slightly
higher-level sister crate with the same author. Either would replace a
hand-rolled `home dir + platform match` block with one function call
(`dirs::state_dir()` or similar), at the cost of one new dependency for a
single call site.

**Verdict: Recommended** — keep using `BrowserConfig::builder().user_data_dir()`
directly; nothing else in chromiumoxide is relevant. **Viable, not required**
for the `dirs` crate: reasonable either way given this is the first XDG-path
need in the codebase — a 5-10 line hand-rolled resolver (`$XDG_STATE_HOME` env
var with a `~/.local/state` fallback on Linux, `dirs`-equivalent literal on
macOS) avoids a new dependency for one call site; pulling in `dirs` avoids
hand-maintaining platform branches if more path resolution shows up later.
Either choice is defensible; default to hand-rolling per YAGNI unless a second
XDG path need appears.

## 2. SaaS/managed API (Anchor Browser, BrowserBase, Steel.dev, etc.)

**What they offer:** Anchor Browser's core pitch is exactly this problem —
named, reusable browser profiles where "every future session that requests
that profile starts already logged in," plus enterprise re-auth handling
([Anchor docs](https://docs.anchorbrowser.io/pricing),
[Anchor blog](https://anchorbrowser.io/blog/browser-agent-amnesia-skill-caching-anchor)).
Pricing is usage-metered: ~$0.05/browser-hour, a $0.01 per-session minimal
fee, and $0.20-$8/GB egress depending on proxy use.

**Why it doesn't fit here:**
- **Wrong problem shape.** stapler-mcp is explicitly a single-user local daemon
  driving CDP for one person's own browser sessions on their own machine. Anchor
  and similar vendors solve *fleet-of-headless-browsers-for-many-tenants*
  (scraping infra, agent swarms, anti-bot fingerprinting) — none of which this
  project has. The requirements doc already excludes fingerprinting/cloud-sync
  from scope for exactly this reason.
- **Latency/architecture mismatch.** Routing local CDP calls through a remote
  hosted browser adds a network hop (and a WebSocket round trip per CDP command)
  for a tool whose whole value proposition is a low-latency local automation
  daemon. It would also mean the "browser" the user sees is no longer their own
  desktop Chrome window — a materially different product.
- **Cost for no benefit.** Metered per-hour/per-session billing for a personal
  tool that just wants "my cookies survive a restart" is solving a problem this
  project doesn't have (multi-tenant isolation, fingerprint rotation, proxy
  egress) while adding cost, an external dependency, and a network failure mode
  where none existed.

**Verdict: Not recommended.** Hosted browser-session services target
multi-tenant/production scraping; this is a local single-user daemon with a
one-line config change available. Swapping backends would be a net regression
in latency, cost, and simplicity for zero gain on the actual requirement.

## 3. LLM-generated implementation vs. battle-tested library (algorithmic complexity check)

Is there a nontrivial algorithm here? Candidates considered:

- **Profile-lock detection/recovery** (stale `SingletonLock` after a crash):
  real prior art exists (Chromium issue tracker, Puppeteer #4860, the "profile
  appears to be in use" class of bugs found via search) — but the existing code
  already sidesteps this entirely by using a unique directory per daemon
  process. A *named, reused* profile reintroduces exactly one case this design
  avoided: if a previous daemon crashed without cleanup, Chrome itself detects
  and handles a stale `SingletonLock` on next launch (this is standard Chrome
  behavior, not something the wrapper needs to reimplement) — worst case the
  user sees Chrome's own error and can delete the lock file by hand. No
  daemon-side lock-recovery algorithm is required for an opt-in, single-user
  feature.
- **Atomic profile-dir creation:** `std::fs::create_dir_all` (already used at
  browser.rs:352) is sufficient — there's no concurrent-creator race to guard
  against in a single-user local daemon that only creates its own directory.

Nothing here rises above "resolve a path, pass it to a builder that already
exists." No hashing, no concurrency-sensitive state machine, no protocol
parsing.

**Verdict: Recommended to hand-roll.** This is a config-flag change of
roughly 15-30 lines (config option → path resolution → pass to the existing
`.user_data_dir()` call, replacing the pid+timestamp path when the option is
set). Sourcing this from a library would be over-engineering for a personal
tool.

## 4. Fork or adapt a reference implementation

Searched `rust-headless-chrome`/`fantoccini` and chromiumoxide's own examples
for a merged "persistent named profile" feature to crib from. Findings:

- **chromiumoxide** ships no example beyond the raw builder call itself; the
  crate's own default path (`$TMPDIR/chromiumoxide-runner`) is effectively a
  *degenerate* single-fixed-profile implementation — it's the thing this
  project's current code deliberately diverges from, not a pattern to copy.
- **rust-headless-chrome** (`headless_chrome::LaunchOptionsBuilder`) exposes
  the same shape of `user_data_dir: Option<PathBuf>` field with no additional
  resolution/lock logic layered on top — same story as chromiumoxide, no
  reference implementation to adapt.
- **fantoccini** is WebDriver-based (talks to `chromedriver`/`geckodriver`,
  not CDP directly), so profile persistence there is a webdriver-capabilities
  concern, not a comparable API shape — not a useful reference for a
  chromiumoxide-based wrapper.
- **Browserless.io** (hosted, not a Rust lib) documents user-data-directory
  handling for its own product, confirming the general pattern (named,
  persistent dir passed at launch) but again nothing Rust-specific to fork.

**Verdict: Not recommended / not applicable.** No comparable Rust CDP tool has
published a reference implementation worth adapting — every one of them stops
at exposing the same raw builder field this project already uses. There is
nothing to fork.

## Summary table

| Option | Verdict |
|---|---|
| Use `chromiumoxide`'s existing `user_data_dir` builder directly | Recommended |
| Add `dirs`/`directories` crate for XDG path resolution | Viable, optional (YAGNI-adjacent) |
| Switch to hosted browser-session SaaS (Anchor/BrowserBase/Steel.dev) | Not recommended |
| Hand-roll the ~20-30 line config/path-resolution change | Recommended |
| Fork/adapt from fantoccini / rust-headless-chrome / chromiumoxide examples | Not applicable — nothing to adapt |

## Final recommendation

Build, not buy: add an opt-in daemon config option that resolves a stable
named directory (hand-rolled path resolution, `dirs` crate optional) and pass
it to the `user_data_dir` builder call that already exists at
`crates/native/src/browser.rs:354-357`, replacing the pid+timestamp path only
when the option is set — no new dependency is required, and no hosted
browser service is warranted for a local single-user daemon.
