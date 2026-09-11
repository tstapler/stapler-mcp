# Requirements: credential-vault

**Date**: 2026-09-08
**Type**: feature addition
**Complexity**: 4 — high-stakes / cross-cutting

## Problem Statement
`stapler-mcp`'s browser-automation tools have no first-class way to log into a
site. Today the only path is `stapler_browser_type`, which means any
credential (password, TOTP code) flows through the MCP tool-call transcript —
and, per architecture research already done for this project
(`project_plans/credential-vault/research/architecture.md`), through the
tool's *response* too: `AxNode.value` on a password-type control is returned
unredacted by `type_text`, `browser_fill_form`, and any later
`browser_snapshot`, because Chromium's CDP `Accessibility.getFullAXTree`
reports the real value regardless of the field's rendered masking. There is
also no support for 2FA/TOTP flows. GitHub issue:
https://github.com/tstapler/stapler-mcp/issues/26.

## Baseline
The calling LLM/agent types credentials in plaintext via
`stapler_browser_type`/`stapler_browser_fill_form`. The plaintext secret
appears in the MCP JSON-RPC request, and — independently — in every
subsequent `AxSnapshot` response (from the type call itself or any later
`stapler_browser_snapshot`) for as long as the field exists in the DOM. Sites
requiring a second factor cannot be driven through `stapler-mcp` at all
today.

## Users / Consumers
Claude Code sessions (and other MCP clients) invoking `stapler-mcp`'s
browser-automation tools to drive an authenticated flow on behalf of this
user. Single-user, local-only daemon — same trust boundary as every other
`stapler-mcp` tool (confirmed in the existing architecture research: the
daemon is already the sole OS/network-touching, single-user process).

## Success Metrics
- A credential-typing tool call's request payload contains only an opaque
  `CredentialRef` (domain + field name) — never a secret value — verified by
  inspecting the JSON-RPC request the daemon receives.
- No `AxSnapshot` returned by any tool (`type_secret`, `browser_snapshot`,
  `browser_fill_form`, `click`, etc.) ever contains the literal plaintext
  value of a `password`-type control, including values that arrived via
  browser autofill rather than `type_secret` — verified by a test that
  autofills a password field out-of-band and then calls
  `stapler_browser_snapshot`.
- A site presenting a TOTP/OTP challenge after primary auth can be driven to
  completion using 1Password's TOTP generation, with the same
  never-in-request, never-in-response guarantee as the password case.
- Behavior is identical (same redaction guarantee, same wire shape) on both
  the native (`chromiumoxide`) and wasm (`playwright-core`) adapters — this
  repo's existing parity bar for every `BrowserDriver` port method.

## Appetite
Large (3–6 weeks).
*(Scope must fit the appetite. If it doesn't fit, cut scope — do not move the deadline.)*

## Constraints
- Vault backend is 1Password, consistent with this user's existing tooling
  (the `secrets` Ansible role, the `mcp__1password__*` MCP server, and the
  architecture research's own recommendation). No other vault backend is in
  scope for this pass.
- Daemon-side resolution only (per the research's wire-protocol analysis: the
  thin-client↔daemon Unix socket carries the identical bytes as the MCP
  JSON-RPC payload one hop later, so resolving in the thin client buys no
  privacy the daemon-side approach lacks, and the thin client has no
  standing OS/network access today).
- `crates/core` stays `#![no OS calls]` — the new `CredentialStore` trait
  lives there; OS/process/network work (`op` CLI, 1Password Node SDK) lives
  in the `crates/native`/`crates/wasm` adapters, matching the existing
  8-port pattern.

## Non-functional Requirements
- **Performance SLO**: not specified beyond "no worse than an interactive
  `op` CLI invocation" — this is a login-flow-frequency operation, not a
  hot path. A resolved-value cache (e.g. via `keyring`) is a valid future
  optimization, not required for this pass.
- **Scalability**: single-user, single-daemon, but concurrent multi-tab
  resolution of the *same* credential is a real scenario (e.g. two tabs
  both auto-triggering a login flow for the same domain) and must not
  produce duplicate/racing vault lookups that risk 1Password rate limits —
  in-flight requests for an identical `CredentialRef` are deduplicated so
  concurrent callers share one vault round trip.
- **Security classification**: confidential/regulated — this feature
  exists specifically to handle credentials and must not leak them.
- **Data residency**: no special requirements (local-only daemon, no data
  leaves the machine except to 1Password's own service, which the user
  already trusts for `op`).

## Scope
### In Scope
- New `CredentialStore` port (`crates/core/src/ports.rs`) with a
  `resolve(&self, credential_ref: &CredentialRef) -> Result<SecretValue, PortError>`
  method; `SecretValue` is a newtype with a hand-rolled, non-leaking `Debug`.
- `CredentialRef { domain, field }` — opaque, no value, used for both
  password and TOTP lookups (`field: "totp"` for the second-factor case).
- New `BrowserDriver::type_secret(session_id, locator, credential_ref, timeout) -> Result<AxSnapshot, PortError>`
  method — never accepts or logs plaintext; the adapter substitutes a fixed
  placeholder for the acted-on node's `value` before returning.
- Structural snapshot redaction: wherever any adapter builds an `AxSnapshot`
  (`crates/native/src/ax.rs`'s `build_tree`/`ax_value_to_string`, and the
  equivalent in `crates/wasm/src/glue/browser.js`), substitute a fixed
  placeholder for any node whose underlying DOM `type` attribute is
  `password`, **or** whose `autocomplete` attribute is one of
  `one-time-code`, `current-password`, `new-password` — regardless of which
  tool call triggered the snapshot. The autocomplete-based key is required,
  not optional: TOTP fields are conventionally `type="text"`, so a
  `type`-only redaction key would leave the TOTP success metric above
  unsatisfied. This necessarily also closes the pre-existing,
  credential-vault-unrelated leak where a browser-autofilled password
  field's value was exposed by `browser_snapshot` with zero vault
  involvement.
- Native adapter: `op read`/`op item get` via the existing `ProcessSpawner`
  port (`crates/native/src/spawn.rs`), using a 1Password **service account
  token** read once at daemon startup via `EnvPort` — not the user's
  interactive `op` session, not stored in `~/.stapler-mcp/` config.
- Wasm adapter: 1Password's Node SDK (`@1password/sdk`) via the same
  wasm-bindgen glue pattern as `crates/wasm/src/glue/browser.js` (lazy
  module-level singleton client for the daemon's lifetime).
- Domain-scoped lookup policy: reject (without ever typing anything) when
  the requested domain doesn't match an allow-listed/known vault entry, or
  when the lookup is ambiguous (multiple matching items) — surfaced as an
  error naming the domain, never a value.
- TOTP/2FA: `RequestSecondFactor(credential_ref, channel)` seam from the
  research's Event-Command-Policy table — 1Password generates the TOTP code
  server-side (daemon-side), `type_secret` types it into the OTP field with
  the same redaction guarantee.
- Actionable vault-error surfacing (`op` unauthenticated, service-account
  token expired/missing) — no silent fallback to plaintext typing.

### Out of Scope
- Non-1Password vault backends (Bitwarden, generic HTTP credential APIs).
- Persistent browser profile / user-data-dir reuse across sessions
  (tracked separately — issue #25).
- AI-assisted/self-healing element resolution for locating login fields
  (tracked separately — issue #22).
- Cross-origin iframe (OOPIF) credential fields — a login form embedded in
  a cross-origin `<iframe>` (e.g. some SSO widgets) is not reachable by
  `stapler_browser_snapshot` at all yet, independent of this project
  (tracked separately — issue #21).
- SMS/email-based 2FA channels — 1Password-generated TOTP only.

## Rabbit Holes
- **DOM `type`-attribute retrieval for redaction.** CDP's
  `Accessibility.getFullAXTree` doesn't hand over the DOM `type` attribute
  for free; the research names `DOM.describeNode` (native) or an
  eval-on-node fallback as the mechanism. Confirm this resolves correctly
  for every password-field variant (including ones with ARIA
  role-overrides) before assuming redaction is complete — the wasm side
  needs an equivalent (`elementHandle.evaluate(el => el.type)` or
  Playwright's own accessibility snapshot attributes) verified
  independently; a design that's safe on native but leaky on wasm fails the
  "identical wire protocol" requirement.
- **1Password service-account token provisioning.** Where the daemon reads
  it from (its own launch environment vs. OS keychain via a future
  `keyring` integration) has real operational consequences (token rotation,
  what happens on daemon restart) — resolve during planning, not
  implementation.
- **TOTP timing.** 1Password-generated TOTP codes are only valid for a
  ~30s window; the flow from "resolve" to "type into field" must not race
  that window, especially across the daemon→adapter→CDP/Playwright hops.
- **Ambiguous/multi-item vault matches.** Domain-scoped lookup needs a
  precise disambiguation rule (exact hostname match? subdomain-aware?) —
  get this wrong and either legitimate logins get rejected or the wrong
  credential gets typed.

## Alternatives Considered
- **Thin-client resolves the secret, not the daemon** — rejected. The
  thin-client↔daemon Unix socket carries the identical JSON-RPC bytes one
  hop later, so this doesn't reduce exposure, and it would require the
  thin client to gain OS/network/vault access it doesn't have today,
  cutting against the existing "daemon is the sole OS-touching process"
  design.
- **Boolean/enum flag on existing `type_text`** — rejected. The method
  signature already takes a plaintext `text: &str`, so a flag doesn't stop
  a caller from passing the secret through the wrong path, and it doesn't
  change the leaky return type, which is the actual problem.
- **`type_secret` returns no snapshot (`Result<(), PortError>`)** —
  considered as an alternative to returning a redacted `AxSnapshot`;
  rejected in favor of parity with `click`/`type_text`'s "get a fresh
  snapshot back" ergonomics, since redaction has to be structural (applied
  to every snapshot-producing call) regardless of which shape `type_secret`
  itself picks.

## Feasibility Risks
- CDP's `Accessibility.getFullAXTree` reporting the real value for
  `input[type=password]` is confirmed behavior (masking is a rendering
  concern, not an AX-tree concern) — the redaction fix is real work, not a
  hypothetical.
- 1Password Node SDK (`@1password/sdk`) integration into the wasm-bindgen
  glue pattern is unproven for this project (no existing glue file talks to
  an external SaaS API the way `browser.js` talks to Playwright) — first
  adapter of this kind in the codebase.
- Service-account token scoping/permissions in 1Password need to be
  narrow enough to satisfy least-privilege but broad enough to cover every
  domain this tool might be asked to log into.

## Observability Requirements
- Every `CredentialTypeRequested`/`ResolveCredential` attempt is logged
  with domain and field name (never the resolved value) and outcome
  (resolved / rejected-domain-mismatch / rejected-ambiguous /
  vault-lookup-failed) — this is the audit trail for what the tool did on
  the user's behalf.
- `SecretValue` must never appear in a `Debug`/`Display` impl or log
  statement — enforced by the type itself (hand-rolled redacting `Debug`),
  not by convention.
- Standard request logging (already in place for other tools) is
  sufficient beyond the above; no new metrics/alerting infrastructure
  needed for a single-user local daemon.

## Known Residual Risk
- **Redaction covers the MCP transport, not the live page DOM.** "Never
  appears in the MCP transport" (Success Metrics above) is not the same
  guarantee as "never leaves the machine." `type_secret` still writes the
  plaintext value into the page's live DOM (`this.value = text`) — any
  script already running on that exact-host-matched page (site analytics,
  an injected/XSS payload, a third-party embedded widget) has the same
  read access to it that every password manager's autofill grants once a
  value is typed. This is an accepted, named residual risk, not a gap this
  feature closes. The calling LLM should avoid typing into fields on
  pages/widgets it doesn't recognize as the login provider itself.

## Risk Control
- The feature is inherently opt-in at the infrastructure level: the
  `CredentialStore` port only initializes if a 1Password service-account
  token is present in the daemon's environment at startup; absent that,
  `type_secret`/vault-backed tools are simply unavailable (return a clear
  "vault not configured" error) rather than partially working.
- No schema migrations, no changes to existing tool behavior for callers
  that never invoke `type_secret` — blast radius is limited to the new
  tool surface plus the redaction change to `AxSnapshot` construction
  (which is a strict safety improvement, not a behavior change any caller
  should be relying on).
- Rollback: revert the port/adapter changes; no persistent state to
  unwind (the daemon holds no credentials at rest — everything is
  resolved fresh from 1Password per call).

## Open Questions
- Exact domain-matching rule for the allow-list/lookup policy (§Rabbit
  Holes) — resolve in Phase 3 planning.
- Whether the redacted placeholder value should be a fixed string (e.g.
  `"••••••••"`) or should encode length/shape of the real value — resolve
  in Phase 3 planning; research recommends a fixed placeholder for
  simplicity but doesn't rule out fidelity-preserving alternatives.
- Where exactly the 1Password service-account token should live
  operationally (daemon launch env vs. keychain) — flagged as a rabbit
  hole above, needs a decision before implementation.
