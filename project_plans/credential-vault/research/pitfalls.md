# Pitfalls Research: credential-vault

Agent 4 (Pitfalls). Complements `research/architecture.md` (Agent 3) — that
doc already establishes the crux (AX-tree redaction must be structural) and
the port shapes; this doc goes one layer deeper into what breaks in
practice, cites external sources where the codebase itself doesn't answer
the question, and flags what's VERIFIED against this working tree vs.
INFERRED from docs/general knowledge and worth confirming during
implementation.

## 1. Secret-leakage side channels beyond payload/AxSnapshot

### 1a. Process argv visibility — VERIFIED safe by design, if implemented as documented

**Confirmed** (1Password CLI docs, `op read` reference —
https://developer.1password.com/docs/cli/reference/commands/read/): `op
read <reference>` and `op item get <reference>` take a **secret reference**
(`op://vault/item/field`) as their argument, never the resolved value. The
value only ever appears on **stdout**. So as long as the native adapter's
`vault.rs` invokes `op` with the reference as the CLI arg and captures
stdout directly (never round-tripping through a shell string, a temp file,
or a second `op` invocation), the secret itself never touches `argv` and is
therefore never visible via `ps`/`/proc/<pid>/cmdline` on Linux. This is a
constraint to enforce in code review, not a design gap — **but it depends
on `ProcessSpawner` growing a new method**, since the existing trait
(`crates/core/src/ports.rs:98-103`, impl'd in `crates/native/src/spawn.rs`)
only has `spawn_daemon`, which is fire-and-forget (stdout/stderr redirected
to a log file, never captured) and exists for a completely different
purpose (launching the daemon itself). `CredentialStore`'s native adapter
needs a **new** port method shaped like `run_capture(cmd, args, stdin:
Option<&[u8]>) -> Result<Output, PortError>` (or similar) — call this out
explicitly in planning so it isn't assumed to already exist.

**Pitfall to design against**: a naive implementation that builds a shell
command string (`format!("op read {ref} | ...")`) instead of `Command::new("op").arg("read").arg(reference)` would still keep the secret out of argv (the reference isn't the secret), but shelling through `sh -c` at all is an unnecessary injection surface if `credential_ref.field`/`domain` ever contain shell metacharacters — use `Command::arg()` (no shell), never `Command::new("sh").arg("-c")`.

### 1b. Error messages interpolating raw vault output — MEDIUM severity, needs an explicit rule

CWE-535 ("Information Exposure Through Shell Error Message") is the
general pattern here, and it has bitten real 1Password-adjacent tooling
before: at least one Python package (`ops`) was documented as leaking
secrets because a `subprocess.CalledProcessError`'s message included the
full argv of a failing `secret-*` command
(https://advisories.gitlab.com/pkg/pypi/ops). That specific failure mode
doesn't apply here (per 1a, the secret is never in argv), but the
**analogous risk does**: if `op`'s stdout is partially written before a
non-zero exit (e.g., a TOTP field concatenated with a warning on the same
stream, or `op` echoing a truncated/garbled value while erroring on a
downstream step), and the adapter's error path does something like
`PortError::Other(format!("op read failed: stdout={}, stderr={}",
stdout, stderr))`, that interpolation becomes the leak. **Design rule to
enforce**: on non-zero exit, the native adapter must construct the
`PortError` from **stderr only**, never stdout — and even stderr should be
treated as untrusted (1Password's CLI is well-behaved today, but a future
`op` version or a misconfigured vault item name could echo user-controlled
data). This mirrors the discipline already in
`crates/core/src/tools/browser.rs:78-90`'s `map_error`, which is careful
about *what* gets embedded in an error string and why — the same
"construct, don't blindly interpolate" discipline needs to extend to
`vault.rs`'s error path, and specifically: never format a raw stdout
buffer into an error, only ever a fixed enum'd reason (`NotFound`,
`AmbiguousMatch`, `RateLimited`, `NotAuthenticated`, ...).

### 1c. Environment inheritance — the token itself, not a resolved secret — HIGH severity, confirmed risk

**Verified in code**: `crates/native/src/spawn.rs:30-31`'s own comment says
plainly — *"`Command` inherits the parent's environment by default — this
is how `STAPLER_MCP_HOME` propagates to the spawned daemon."* `EnvPort`
(`crates/core/src/ports.rs:105-108`) is a thin read-only wrapper over
`std::env::var`, and `NativeEnv` (`crates/native/src/env.rs`) confirms
there's no scoping — reading `OP_SERVICE_ACCOUNT_TOKEN` via `EnvPort` at
daemon startup means it lives in the **daemon process's own environment
block** for the daemon's whole lifetime. Rust's `std::process::Command`
inherits the full parent environment unless `.env_clear()` or `.env_remove()`
is called explicitly. Today the only other subprocess the daemon spawns is
itself (`spawn_daemon`, for `--daemon` re-exec) — that's harmless since the
child *should* have the token too. **But this is a standing landmine for
future code**: any *future* `ProcessSpawner`-based tool (browser binary
launch already happens via `chromiumoxide`'s own process management, not
this port, but nothing stops a future feature from shelling out to some
other CLI) would silently inherit `OP_SERVICE_ACCOUNT_TOKEN` unless the
spawn path explicitly scrubs it. **Design recommendation**: the
`CredentialStore` native adapter's own `op` invocation should use the
inherited environment (that's *how* `op` authenticates — via
`OP_SERVICE_ACCOUNT_TOKEN` in its own env, this is intended), but any
*other* current or future `ProcessSpawner` call site (e.g., a hypothetical
future "run arbitrary shell tool" feature) should default to
`.env_clear()` plus an explicit allow-list, not implicit full inheritance.
Flag this as a follow-up hardening item even though today's `spawn_daemon`
is not itself at risk (its only child is a re-exec of itself).

### 1d. Core dumps / memory zeroing — MEDIUM severity, `zeroize` is the right tool but has real caveats

Rust gives no guarantee that memory holding a `String`'s bytes is
overwritten on drop — the compiler is free to elide "dead" writes
(confirmed via `zeroize` crate docs, https://docs.rs/zeroize/latest/zeroize/:
*"Rust does not guarantee that memory is left untouched by things like
temporary values, which can be left on the stack, or moved/copied data
which is not accounted for by the Drop impl"*). Two things to actually
implement, not just wave at:

- Wrap `SecretValue`'s inner storage in `zeroize::Zeroizing<String>` (or
  hand-roll `Drop` calling `.zeroize()`), not just a non-leaking `Debug`
  impl as `architecture.md` §4 already specifies — a redacting `Debug` stops
  *logging* leaks but does nothing for a core dump or a `/proc/<pid>/mem`
  read of a crashed daemon.
- **Caveat, confirmed from the same docs**: `zeroize`'s `String`/`Vec`
  impls zero the buffer's *current allocated capacity*, but **cannot
  guarantee earlier reallocations didn't leave copies elsewhere in the
  heap** — e.g., if `SecretValue` is built via `String::from(stdout_str)`
  after several intermediate `String`/`Vec<u8>` allocations in the
  `op`-output-parsing path (trimming, UTF-8 validation, JSON deserialize),
  each intermediate buffer is a copy `zeroize` never touches. **Design
  rule**: minimize the number of owned buffers the secret passes through
  between "read from `op`'s stdout pipe" and "wrapped in
  `Zeroizing<String>`" — ideally one `Vec<u8>` read directly from the
  child's stdout, zeroized itself after conversion, converted once into the
  final `Zeroizing<String>`. Also register a Rust-level TODO to prefer
  `Command::stdin`/`stdout` piped `Vec<u8>` reads over anything that
  round-trips through `serde_json::Value` (which would leave the secret
  sitting in an untyped `serde_json::Value::String` heap allocation that
  `zeroize` has no impl for).
- Register-level exposure ("the value sat in a CPU register or was copied
  by the optimizer") is explicitly **not** fully solvable by `zeroize`
  alone per its own docs (*"clearing registers is a difficult problem...
  requires either inline ASM or rustc support"*) — worth naming as a
  known, accepted residual risk rather than something the design claims to
  fully close.
- This is `crates/native`-only scope — `crates/core` stays `#![no OS
  calls]`, and `zeroize` doesn't touch the OS, so it's fine to depend on
  from either `core` or `native`, but `SecretValue`'s *type* likely lives
  in `core` (it's part of the `CredentialStore` port signature) — decide in
  planning whether `core` takes a `zeroize` dependency or `SecretValue`
  wraps a native-only zeroizing type behind the port boundary.

## 2. 1Password service-account token pitfalls

### 2a. Rate limits are aggressive and account-wide — HIGH severity, directly threatens a crash-loop

**Confirmed** (1Password Developer docs,
https://www.1password.dev/service-accounts/rate-limits, corroborated by
community reports at
https://www.1password.community/developers-69/service-account-rate-limits-15-minutes-block-no-backoff-duration-shown-23967
and a real crash-loop incident at
https://github.com/openclaw/openclaw/issues/56217):

- Rate limits apply **per-1Password-account**, not per-service-account —
  exhausting the limit blocks *every* service account under that account,
  for up to 24 hours in the daily-limit case.
- The limiting is aggressive: roughly **15 requests over 10 minutes** can
  trigger a 15+ minute block; error messages ("try again in seconds") have
  been reported as misleading about actual reset time (one report: stated
  duration was blank, actual reset was 6 hours later).
- **Direct relevance to this project**: a plausible failure mode is a busy
  Claude Code session that calls `type_secret` repeatedly (e.g., retrying a
  failed login form across several attempts, or a multi-tab automation
  flow each independently resolving the same credential) — each call is a
  fresh `op read` subprocess invocation with no caching. That's exactly the
  "crash-loop exhausts rate limit" shape documented in the issue above.
  **Design implication for Phase 3**: cache a resolved `SecretValue` for a
  short TTL (seconds, not the "resolved fresh every call" stance
  `requirements.md`'s Risk Control section currently states for
  rollback-simplicity reasons) or at minimum de-duplicate concurrent
  in-flight resolves for the same `CredentialRef` within one session — the
  requirements' "no persistent state" rollback story is about *durable*
  state, not necessarily about forbidding a short in-memory cache, so this
  is worth revisiting in Phase 3 rather than treated as settled.
- `op service-account ratelimit` exists as a diagnostic command
  (surfaced in the rate-limits doc) — worth wiring into the "vault lookup
  failed" error path so a rate-limited failure surfaces as
  `"vault rate-limited, retry after Ns"` rather than a generic I/O error,
  per the Observability/actionable-error requirement.

### 2b. Token expiry is only detectable *by failing* — no advance warning

No evidence found (searched 1Password CLI/service-account docs
specifically for this) of a way to introspect a service-account token's
remaining validity before using it — `op whoami`/`op user get --me`
confirms *which* account is authenticated but nothing in the public docs
describes a pre-flight "is this token about to expire" check. **Practical
consequence, confirmed via 1Password's own token-rotation guidance**
(https://developer.1password.com/docs/service-accounts/manage-service-accounts/,
which documents rotating a token *with an overlap expiration window*
specifically so callers have time to update *before* the old one dies):
the intended failure mode is that the *old* token keeps working until its
set expiration, then the *next* `op read` call fails outright with an
authentication error — there is no soft-fail/warning period from the
client's perspective. **Design implication**: `CredentialLookupFailed`
(already in `architecture.md`'s Event-Command-Policy table) needs a
distinguishable "authentication failed / token expired or revoked" variant
(not lumped into a generic vault-unreachable case) so the actionable error
message can say "re-provision `OP_SERVICE_ACCOUNT_TOKEN` and restart the
daemon" rather than something that reads like a transient network blip —
and this failure is only observable *reactively*, at the next resolve
call, matching the requirements doc's own framing but confirmed here
rather than assumed.

### 2c. Audit-log visibility — good but not sufficient on its own for this project's Observability Requirements

**Confirmed** (1Password Events API docs,
https://developer.1password.com/docs/events-api/audit-events/, and
https://support.1password.com/events-reporting/): every `op read`/`op item
get` via a service account does generate an audit event on 1Password's
side, with `actor_details` distinguishing "Service Account" from "User" —
so there *is* a 1Password-side record of every resolve. **But it's not a
substitute for this project's own logging**, for two concrete reasons:
(1) 1Password's audit log requires a **Business** plan and pulling it
requires the separate Events API / a bearer token of its own — it's not
something the daemon can cheaply query inline to correlate "which MCP tool
call triggered this vault read," and (2) 1Password's audit event records
*that a read happened*, not the domain/field-scoped semantic context this
project's Observability Requirements ask for ("domain/field and outcome").
**Confirms the requirements doc's own stance is correct**: duplicate,
local logging (domain + field + outcome, never the value) is necessary
regardless of 1Password's own audit trail — the two are complementary, not
either/or, and 1Password's log should not be treated as "we already log
this elsewhere" grounds for skipping the local requirement.

## 3. CDP/Chromium-specific pitfalls for AX-tree redaction

### 3a. Redaction needs a *second* CDP call per password-type node — architecture.md already flags the mechanism; here's the timing risk it doesn't cover

`architecture.md` §3 correctly identifies that `Accessibility.getFullAXTree`
doesn't hand over the DOM `type` attribute, and that `DOM.describeNode`
(already imported at `crates/native/src/ax.rs:20`, currently used only for
iframe traversal — see `fetch_frame_tree`,
`crates/native/src/ax.rs:137-208`) or an eval-on-node fallback is the
mechanism to get it. What that doc doesn't dig into: **this makes
redaction a two-request read** (`getFullAXTree` for the value, then a
per-candidate-node `describeNode`/eval for the type), and CDP gives no
atomicity guarantee across two separate protocol calls against a live
page. Concretely:

- A password-manager extension, an app's own JS (e.g., a "show password"
  toggle that swaps `type="password"` → `type="text"` on focus), or the
  page's own async rendering could mutate the `type` attribute *between*
  the `getFullAXTree` call and the follow-up `describeNode` call, meaning
  the redaction check reads a `type` value from a slightly different
  moment than the `value` it's deciding whether to mask. In practice this
  is far more likely to fail *safe* than *unsafe* (a field that was
  `type=password` at AX-tree time but flips to `type=text` microseconds
  later before the `describeNode` call would exit `getFullAXTree`'s value
  capture with `value` already correct, since Chromium's AX pipeline and
  the JS mutation both race the same JS-thread event loop) — but the
  reverse direction (a field created as `type=text` and switched to
  `type=password` by page JS, e.g., a login form that starts unmasked and
  masks itself after the first character) is the dangerous case: if the
  `getFullAXTree` call lands *before* that switch, `value` is captured as
  plaintext under a control this project's own redaction check would
  correctly classify as non-password (because at `describeNode` time it
  might *also* already be `text` again, or the reverse). **Concrete
  mitigation to specify in the plan**: fetch `type` (or the fuller node
  description) via a **single combined query per node** where possible —
  a `Runtime.callFunctionOn`/eval that reads both `el.type` and the value
  it needs to make a redaction decision on **in one round-trip**, mirroring
  the existing `invoke_on_node`/eval pattern already used for
  `dispatch_type`/`dispatch_click` (`crates/native/src/browser.rs:792-808`)
  rather than two independent CDP calls that can straddle a DOM mutation.
  This is a correctness note for Phase 3, not just a performance one.

### 3b. Headless vs. headed AX-tree differences for masked fields — UNVERIFIED, treat as an explicit test gap, not an assumption

Searched specifically for documented behavioral differences between
headless and headed Chrome in what `Accessibility.getFullAXTree` reports
for `type=password` fields; **found no evidence of a documented
difference** (general accessibility-tree docs describe the masking as a
*rendering* concern determined by the OS/browser UI layer, which is
consistent with `architecture.md`'s own conclusion that the AX tree
reports the real value regardless). Given the project's likely default of
running Chromium headless for the daemon, and given that this specific
question has no authoritative public answer, **this needs an empirical
check as a test-plan item, not an assumption carried into design**: before
shipping, run the redaction path against a real password field in both
headless and headed launch configurations and diff the raw
`getFullAXTree` output. Absence of evidence that they differ is not
evidence they don't.

### 3c. Shadow DOM — `pierce: true` is required and easy to omit silently

**Confirmed** (CDP docs via search,
https://chromedevtools.github.io/devtools-protocol/tot/DOM/, and
corroborated at https://yotam.net/posts/piercing-the-shadow-root-using-cdp/):
`DOM.describeNode`'s `pierce` parameter **defaults to `false`**, and
without it the call does not traverse shadow roots — meaning a password
field inside a shadow root (increasingly common with web-component-based
design systems, e.g. many bank/SaaS login forms built with Lit/Stencil)
would come back from `describeNode` **without its `type` attribute
reachable at all**, unless the redaction code explicitly passes `pierce:
true`. Given `architecture.md`'s own note that
`get_full_ax_tree` already includes shadow DOM content in a single call
(the accessibility tree "pierces" shadow boundaries by default, confirmed
empirically per `crates/native/src/ax.rs:126-127`'s comment) — there's a
real risk of an **asymmetry bug**: the *value* is captured (via
`getFullAXTree`, which already pierces shadow roots) but the *type
attribute check* (via `describeNode`, which does NOT pierce by default)
silently fails to find the node inside its shadow root, and the redaction
logic — depending on how it's written — could either (a) correctly
fail-safe by treating "type attribute unresolvable" as "redact anyway," or
(b) incorrectly treat "couldn't confirm type=password" as "not a password
field, don't redact," which would **leak the exact class of value this
whole feature exists to protect**, specifically for shadow-DOM-based login
forms. **This is the single highest-severity CDP pitfall found**: whichever
mechanism ends up fetching the `type` attribute (`DOM.describeNode` with
explicit `pierce: true`, or an eval-based fallback which naturally pierces
shadow roots since `this.type` on an element handle works regardless of
shadow placement) must be verified to default to fail-safe (redact when
uncertain), and a test case with a password field inside a shadow root
(closed and open modes both, since `pierce` affects visibility into closed
shadow roots differently than open ones per general Shadow DOM semantics)
should be a required part of the redaction test plan, not an
edge case.

## 4. wasm/Playwright-side pitfalls: `@1password/sdk`

### 4a. The SDK is Node.js-targeted, and this project's glue pattern is not plain Node

**Confirmed** (npm package page, https://www.npmjs.com/package/@1password/sdk):
the 1Password JS SDK "currently supports Node.JS" and its package
description states it's "built with a Rust core compiled to WASM via
`wasm_bindgen`." That's a second, independent `wasm_bindgen`-compiled
component living *inside* a wasm-bindgen-compiled adapter — i.e., this
project's wasm crate (`crates/wasm`) would, if it imports `@1password/sdk`
through its JS glue layer, be **wasm calling into JS glue that itself
loads another wasm module**. Two concrete risks, both currently
unverified against this project's actual wasm target/bundler and worth a
narrow spike before committing to the architecture:

- **Nested-wasm-instantiation compatibility**: `@1password/sdk`'s own wasm
  core needs to instantiate correctly inside whatever JS host is running
  this project's `crates/wasm` output (per the repo map, this project uses
  a wasm-bindgen adapter delegating to a Node.js host per
  `research/architecture.md`'s references to `crates/wasm/src/glue/*.js`).
  If that Node host is a restricted/sandboxed one, or if module resolution
  for `@1password/sdk`'s own `.wasm` binary asset doesn't resolve the same
  way plain npm-installed Node code would, instantiation could fail in a
  way that's specific to this project's glue pattern and invisible in
  `@1password/sdk`'s own (presumably plain-Node) test suite. **No specific
  GitHub issue found describing this exact double-wasm nesting failure for
  `@1password/sdk`** — this is an architectural risk inferred from the
  package's own stated build (Rust→wasm core), not a confirmed bug, and
  should be spiked early rather than discovered mid-implementation.
- **Worker-thread assumptions**: general search on wasm/worker-thread
  compatibility (no `@1password/sdk`-specific reports found) confirms that
  worker semantics differ meaningfully between plain Node.js (no
  `WorkerGlobalScope`) and browser/other embedded JS hosts, and that wasm
  modules using threads add real nested-worker-hierarchy complexity. If
  `@1password/sdk`'s Rust core uses any threading internally (common for
  crypto-heavy Rust code, though not confirmed for this specific SDK), and
  this project's JS host isn't a stock Node.js process (e.g., if it's
  embedded differently for the wasm-bindgen glue), that's a second
  independent point of failure. **Action item, not a finding**: before
  Phase 3 commits to this SDK for the wasm adapter, do a 30-minute spike —
  `npm install @1password/sdk` inside this project's actual `crates/wasm`
  Node host and confirm client construction + one `resolve()` call
  succeeds, rather than assuming npm-package-description compatibility
  claims transfer unmodified to this project's specific embedding.

### 4b. Native crypto bindings — no evidence found either way, flag as unverified

Searched for whether `@1password/sdk` depends on Node-native (N-API/native
addon) crypto bindings that a wasm-bindgen-hosted environment might lack;
found nothing definitive. Given the SDK's own description centers on a
wasm-compiled Rust core (which would do crypto *inside* wasm, not via
Node-native bindings), this specific risk looks **low probability** — but
name it as an unverified assumption rather than a closed question, and let
the 4a spike settle it empirically (a failed `resolve()` call with a
native-module-not-found error would be the tell).

### 4c. Comparison point: native adapter's `op` shell-out has none of these risks

Worth stating explicitly for planning trade-off purposes: the native
adapter's "shell out to `op`" approach (architecture.md §4) sidesteps
*all* of §4a/4b's nested-wasm/worker-thread risk entirely, since `op` is a
real native binary invoked as a subprocess, not a second wasm module
nested inside this project's own wasm build. This is a real asymmetry in
implementation risk between the two adapters that the plan should account
for — e.g., by sequencing the native adapter first and treating the wasm
adapter's `@1password/sdk` integration as the higher-uncertainty stretch
goal, consistent with `research/architecture.md`'s own framing that this
is "the first adapter of this kind in the codebase" (`requirements.md`
Feasibility Risks).

## Summary table

| # | Pitfall | Severity | Status |
|---|---|---|---|
| 1a | `op` CLI never takes secret as argv (only the reference) | — | VERIFIED safe by design; `ProcessSpawner` needs a new capturing method |
| 1b | Error-message interpolation of raw `op` stdout | Medium | Design rule needed: stderr-only, enum'd reasons |
| 1c | `OP_SERVICE_ACCOUNT_TOKEN` inherited by any future subprocess spawn | High | Verified inheritance mechanism; needs `.env_clear()` policy for non-`op` spawns |
| 1d | No memory zeroing without `zeroize`; `zeroize` has real coverage gaps | Medium | Use `Zeroizing<String>` + minimize intermediate buffers; register-level exposure is an accepted residual risk |
| 2a | Service-account rate limits are aggressive and account-wide | High | Verified; needs caching/de-dup to avoid crash-loop |
| 2b | Token expiry only detectable by failing the next call | Medium | Verified via rotation-docs framing; needs a distinct error variant |
| 2c | 1Password audit log doesn't substitute for local domain/field logging | Low | Verified; confirms requirements doc's existing stance |
| 3a | Two-request (value + type) redaction check isn't atomic | Medium | Prefer single combined eval over `getFullAXTree` + separate `describeNode` |
| 3b | Headless vs. headed AX masking differences | Unverified | No evidence found; add as an explicit empirical test-plan item |
| 3c | Shadow DOM: `DOM.describeNode` doesn't pierce by default | **Highest** | Verified; must fail-safe when type-check can't resolve, or shadow-DOM login forms leak exactly the value this feature protects |
| 4a | `@1password/sdk`'s nested wasm-in-wasm-glue compatibility | Unverified | Architectural risk inferred from package's stated build; spike before committing |
| 4b | Native crypto binding dependency in `@1password/sdk` | Unverified | No evidence found; low probability, confirm via 4a spike |

## Sources

- [op read — 1Password Developer](https://developer.1password.com/docs/cli/reference/commands/read/)
- [Use secret references with 1Password CLI](https://developer.1password.com/docs/cli/secret-references/)
- [Service account rate limits — 1Password Developer](https://www.1password.dev/service-accounts/rate-limits)
- [Service Account Rate Limits: 15+ Minutes Block, No Backoff Duration Shown — 1Password Community](https://www.1password.community/developers-69/service-account-rate-limits-15-minutes-block-no-backoff-duration-shown-23967)
- [Secret provider crash-loop exhausts 1Password service account rate limits — openclaw#56217](https://github.com/openclaw/openclaw/issues/56217)
- [Manage service accounts — 1Password Developer](https://developer.1password.com/docs/service-accounts/manage-service-accounts/)
- [Audit events — 1Password Developer](https://developer.1password.com/docs/events-api/audit-events/)
- [Get started with 1Password Events Reporting](https://support.1password.com/events-reporting/)
- [Advisories for Pypi/ops package — GitLab Advisory DB](https://advisories.gitlab.com/pkg/pypi/ops)
- [zeroize — docs.rs](https://docs.rs/zeroize/latest/zeroize/)
- [DOM domain — Chrome DevTools Protocol](https://chromedevtools.github.io/devtools-protocol/tot/DOM/)
- [Piercing the Shadow Root Using CDP — Yotam's blog](https://yotam.net/posts/piercing-the-shadow-root-using-cdp/)
- [@1password/sdk — npm](https://www.npmjs.com/package/@1password/sdk)
- Codebase (this repo, working tree at time of research): `crates/core/src/ports.rs`, `crates/native/src/spawn.rs`, `crates/native/src/env.rs`, `crates/native/src/ax.rs`, `crates/native/src/browser.rs:792-808`, `crates/core/src/tools/browser.rs:78-90`, `project_plans/credential-vault/research/architecture.md`, `project_plans/credential-vault/requirements.md`
