# Feature Research: credential-vault

Agent 2 (Features) — SDD Phase 2 research. Complements
`research/architecture.md` (where resolution happens, port shapes) with:
what comparable tools actually do, edge cases requirements.md doesn't name,
and unstated needs.

## 1. How comparable tools keep secrets out of LLM context

Checked actual docs/source (not marketing copy) for five tools. All five
converge on the same core mechanism — **placeholder-in-prompt,
substitute-at-dispatch** — which is exactly what requirements.md's
`type_secret(session_id, locator, credential_ref, ...)` already proposes.
None of them solve the *response*-side leak (`AxSnapshot`/DOM read-back)
as rigorously as this project's scope demands; that's this project's
differentiator, not a solved problem to copy.

| Tool | Request-side mechanism | Response-side (redaction) | 2FA/TOTP | Domain scoping | Ambiguous-match handling |
|---|---|---|---|---|---|
| **Skyvern** | Vault credential (1Password/Bitwarden/Azure Key Vault) resolved server-side; LLM prompt/recording shows a placeholder token (`BW_PASSWORD`, `AZ_TOTP`); value "discarded as soon as the field is filled" | Not documented | Full server-side TOTP generation, incl. split-box (one-digit-per-input) layouts; email/SMS via polled/pushed endpoints; magic-link auto-follow | Implied per-credential, not detailed | **Not documented** — real gap they haven't published a rule for |
| **browser-use** | `sensitive_data` dict; LLM sees only placeholder keys (`x_user`, `x_pass`) substituted into typed text after the model emits its action, never before | Recommends `use_vision=False` to keep screenshots (which *would* show plaintext) out of the loop entirely — a blunt instrument, not structural redaction | None built-in | Regex/glob per-domain (`https://*.example-staging.com`) — different domains get different credential sets | Not documented |
| **Stagehand** | `%variableName%` template syntax in the natural-language instruction; real value passed in a separate `variables` object, substituted at execution, never sent to the model | Not documented (recommends `verbose: 0` to suppress secrets from logs — same blunt-instrument pattern as browser-use) | None built-in | None documented | N/A (caller names the exact variable) |
| **Anchor Browser** | `secretValues` key-value map; agent prompt only ever sees the *key name*; "type-time replacement" — substituted at the instant of keystroke dispatch | Not documented | Not documented | Wildcard domain patterns (`*.linkedin.com`); explicitly states "if the agent navigates to a different domain, those secrets won't be accessible" | **Not documented** |
| **vercel-labs/agent-browser** | Out-of-process **credential provider plugins** over a `stdio` JSON protocol (`agent-browser.plugin.v1`); explicit design note: *"Do not put vault tokens or passwords in plugin command args"* — secret never crosses into the CLI's own argv/config | Not documented | **Not automated** — 2FA is `--headed` + "wait for user to complete 2FA manually" | Explicit `--url`/`--username-selector`/`--password-selector` overrides, i.e. operator picks the exact field per invocation rather than the tool inferring it | Operator-specified selectors sidestep the ambiguity question entirely |

Takeaways that matter for this design:

- **Every competitor's mechanism is request-side only.** None of them
  publish a redaction story for what the model reads *back* (accessibility
  tree, DOM snapshot, screenshot). requirements.md's structural-redaction
  scope item is therefore not "catching up" to an existing pattern — it's
  filling a gap the whole category has left open. `browser-use`'s
  `use_vision=False` and Stagehand's `verbose:0` are the closest analogues,
  and both are *disable a whole capability* workarounds, not "detect and
  mask the specific field." This validates requirements.md's choice to key
  redaction off `type=password` structurally rather than trying to filter
  logs/screenshots after the fact.
- **`agent-browser`'s plugin-protocol pattern is worth adopting narrowly.**
  Its "don't put the token in argv/config, resolve it out-of-process" rule
  is exactly the shape `crates/native`'s `op read`/`op item get` via
  `ProcessSpawner` already has to satisfy — the service-account token lives
  in `EnvPort`/the OS keychain, never in a CLI arg that'd show up in `ps`.
  Worth an explicit test asserting the 1Password token never appears in a
  spawned process's argv.
- **Nobody has published a real ambiguous-match rule.** Skyvern, Anchor
  Browser, and browser-use all punt on "what if two vault items match this
  domain." requirements.md's own Open Questions section flags this
  correctly as unresolved industry-wide, not just internally — Phase 3
  planning should treat "reject with an actionable multi-item error,
  listing item names/uuids the caller can pass a disambiguating `field` or
  future `item_hint` for" as the safe default, since every competitor that
  *does* document behavior (agent-browser) resolves ambiguity by having the
  operator specify selectors up front rather than guessing.
- **`agent-browser`'s explicit-selector-override pattern suggests a fallback
  UX**, not just an error: when domain match is ambiguous, the tool could
  accept (in a later pass, not this one — Out of Scope already excludes
  self-healing) a `field`/`item` hint from the caller rather than only
  ever failing. For this pass, per Constraints, reject-never-guess is
  correct; just don't paint the error message into a corner that can't
  later carry a hint.
- Stagehand's `%var%`-in-instruction pattern is a UX idea worth borrowing
  even though `stapler-mcp`'s tool surface isn't natural-language: it's
  the same shape as `CredentialRef` — the *only* thing that crosses the
  wire is a name, resolved on the trusted side of the boundary.

Sources:
- https://www.skyvern.com/docs/cloud/managing-credentials/totp-setup
- https://skyvern.mintlify.app/developers/features/authentication-and-2fa
- https://docs.browser-use.com/open-source/examples/templates/sensitive-data
- https://docs.stagehand.dev/v3/basics/act
- https://docs.anchorbrowser.io/agentic-browser-control/secret-values
- https://github.com/vercel-labs/agent-browser/blob/main/skill-data/core/references/authentication.md
- https://1password.com/blog/closing-the-credential-risk-gap-for-browser-use-ai-agents

## 2. Edge cases requirements.md doesn't yet name

Grounded against the real types in `crates/core/src/ports.rs`, the real
tool handlers in `crates/core/src/tools/browser.rs`, and the real AX-tree
code in `crates/native/src/ax.rs`/`crates/native/src/browser.rs`.

### 2.1 "Show password" toggle flips `input type` dynamically

Chromium's `Accessibility.getFullAXTree` (what `crates/native/src/ax.rs`'s
`capture_snapshot` calls) does **not** report the DOM `type` attribute at
all — `AxNode` here only ever carries `role`/`name`/`value`
([ports.rs:151-162](/home/tstapler/Programming/stapler-mcp/crates/core/src/ports.rs#L151-L162)),
and the raw CDP `AXNode.role` for both `<input type=password>` and the same
node after a JS toggle to `type=text` computes to the **same** accessible
role (`"textbox"`) in Chromium — the AX role does not encode password-ness.
This is already flagged as a Rabbit Hole ("DOM `type`-attribute retrieval
for redaction on both adapters") — confirming it structurally: redaction
*cannot* be keyed off anything already in `AxNode`/`AxSnapshot`; it needs an
independent DOM query per node (`DOM.describeNode`'s `attributes` list, or
a JS `evaluate` pass) cross-referenced against the AX tree by
`BackendNodeId`, done at snapshot-build time, not deferred to render time.

The toggle itself is the sharper edge case: if redaction snapshots the DOM
`type` attribute **once**, before the toggle, and a user/script flips it to
`type=text` a moment later, a subsequent `browser_snapshot` call would
re-query the DOM fresh each time (per `ax.rs`'s module doc, every capture
is a full re-walk) — so the redaction check must run **per snapshot call**,
not cached from an earlier one. Getting this right for free is good news:
since `capture_snapshot` already re-fetches the full tree on every call, a
DOM-type check added there naturally re-evaluates every time too. The trap
is only in an implementation that snapshots the `type` attribute once and
memoizes it against `BackendNodeId`.

### 2.2 Browser-extension autofill lands a secret before `type_secret` is called

Nothing in `stapler-mcp`'s architecture (native `chromiumoxide` drives real
Chrome; wasm drives Playwright-core) currently disables password-manager
extensions or browser-native autofill. If a user's real 1Password/Chrome
autofill extension fills the field on page load — before any tool call —
the value is now sitting in the DOM as `AXNode.value`, unredacted, and
`browser_snapshot` (no `type_secret` involved at all) would leak it under
today's code. This is exactly the "closes a pre-existing autofill leak
too" line in requirements.md's Scope — good, it's explicitly in scope —
but it means: **redaction has to run on every `AxSnapshot`-producing path,
not just `type_secret`'s own return value.** `browser.rs`'s
`AxCapture`/`build_tree` is the single choke point every `BrowserDriver`
method funnels through
([ax.rs:96-119](/home/tstapler/Programming/stapler-mcp/crates/native/src/ax.rs#L96-L119)),
so putting the DOM-type check there (rather than post-hoc in
`tools/browser.rs`'s per-tool output mapping) is the only placement that
actually closes this — matches `architecture.md`'s §3 recommendation
("structural, not call-scoped").

### 2.3 Password field re-renders as a new DOM node mid-flow

SPA frameworks routinely destroy and recreate the whole form (a fresh
`BackendNodeId`) on a validation error or step transition. The codebase
already has the exact defensive pattern needed for this in
`verify_node_live`
([browser.rs:510-551](/home/tstapler/Programming/stapler-mcp/crates/native/src/browser.rs#L510-L551)):
re-describe the node and re-check its expected role immediately before
dispatch, closing the TOCTOU gap between snapshot and action. `type_secret`
needs the same check, but it must additionally re-verify the DOM `type`
attribute (not just AX role) at that same pre-dispatch instant — a
re-rendered node could keep role `textbox` while silently losing
`type=password` (or vice versa: a node that *was* plain text at snapshot
time gets swapped for a password field, e.g. a site that shows a
plaintext-echo field until you tab out of it). Concretely: `type_secret`
should refuse to type into a `Locator` whose live-verified DOM type isn't
`password` at dispatch time (see 2.5), using the same "changed since
snapshot" `NotFound`/actionability error shape `verify_node_live` already
produces.

### 2.4 Multiple 1Password items match a domain, but the user has a stated preference

requirements.md's Rabbit Holes correctly flag "ambiguous/multi-item vault
match disambiguation rule" but frames it as pure reject-on-ambiguity. The
sharper case: `op item list --vault X` for `github.com` legitimately
returns a personal account and a work account, and the user *does* have a
deterministic preference (e.g. "always the item tagged `primary`", or "the
one whose vault matches the current 1Password profile"), but
`CredentialRef { domain, field }` as scoped in requirements.md carries no
slot for that preference. Two honest options for Phase 3: (a) keep
`CredentialRef` exactly as scoped and require the *caller* (the LLM) to
disambiguate by being told the item titles in the rejection error and
retrying — consistent with "no silent plaintext fallback" and "reject...
never typing"; or (b) widen scope to a `CredentialRef.item_hint: Option<String>`
later, which the vercel-labs `agent-browser` precedent (explicit selector
overrides) supports as a reasonable follow-up. Recommend (a) for this pass
per Constraints — flagging it here so Phase 3 doesn't quietly reinvent (b)
as a "trivial addition" without noting it moves `CredentialRef`'s shape.

### 2.5 `type_secret` called on a locator that isn't actually a password field

Two sub-cases, both real: caller error (LLM passes a `ref` for the email
field by mistake), and site deception (a field that's visually a password
box but is `type=text` with CSS masking, or the reverse — `type=password`
used for a non-credential field like a "confirmation code" the site
author didn't bother making `type=text`). Given 2.1's finding that AX role
alone can't distinguish these, `type_secret`'s own contract needs a
decision recorded in Phase 3: does it (a) refuse anything that isn't
literally `type=password` at dispatch time — safest, matches the "never
silent" principle in Constraints — or (b) permit typing into any locator
but *still* redact the resulting `AxSnapshot`'s `value` for that node
unconditionally, since the caller explicitly declared it a secret via
`type_secret` regardless of the DOM's own type attribute. (b) is more
useful for the "confirmation code" false-negative case above and doesn't
weaken security (redaction only ever adds masking, never removes it), so
it's the better default — but it must be stated explicitly rather than
left to fall out of whichever branch gets written first, since it changes
what "closes the pre-existing autofill leak" in Scope actually covers (an
autofilled field the user never called `type_secret` on still needs (a)'s
DOM-type-based structural check to catch it — the two mechanisms are
complementary, not either/or).

### 2.6 Per-tab/per-session scoping

Checked directly: `Locator`/`SessionId` as consumed by every existing
`BrowserDriver` method (including the ones `type_secret` sits alongside)
already get this right as of the just-shipped fix in commit `6b6b56a`
("fix: scope ref state per-tab and race SSRF redirect detection against
goto", #34/#35). `crates/native/src/browser.rs` moved `latest_refs`/
`known_refs`/`nav_generation`/`latest_url` off the session-wide
`BrowserSession` onto a per-tab `TabState`
([browser.rs:104-170](/home/tstapler/Programming/stapler-mcp/crates/native/src/browser.rs#L104-L170)),
specifically because a ref issued for tab A was being invalidated by
activity on tab B. Since `type_secret`'s proposed signature is
`(session_id, locator, credential_ref, timeout)` — the exact same
`SessionId`+`Locator` pair every other `BrowserDriver` method takes — it
inherits this per-tab correctness for free through
`resolve_locator_impl`'s existing active-tab lookup
([browser.rs:475-501](/home/tstapler/Programming/stapler-mcp/crates/native/src/browser.rs#L475-L501)),
**provided** the implementation is added as a new arm alongside
`click`/`type_text` in the same dispatch path rather than as a
parallel/bolted-on code path that re-derives its own tab lookup. No new
scoping discipline is needed *for the locator*; there's a second,
separate scoping question this doesn't answer: whether `CredentialStore`
resolution itself should be domain-scoped against the *active tab's
current URL* (not the session's, given multi-tab) at dispatch time — i.e.
the domain check in "reject on domain mismatch" (Scope) must read
`active_tab_state`'s URL, the same one `capture_snapshot` uses for
`AxSnapshot.url`, not a session-level URL that could be stale if the
active tab changed between snapshot and this call.

## 3. Unstated needs

### 3.1 Domain discovery ("what credentials exist for this site")

Every reviewed competitor's mechanism assumes the caller already knows the
placeholder/key name to use (`sensitive_data['x_pass']`, `%password%`,
`secretValues['LINKEDIN_PASSWORD']`) — none of them expose a "list what's
available for this domain" call, because in every case *the human operator*
wrote the credential mapping into the agent's config ahead of time. That
assumption breaks for `stapler-mcp`: the calling LLM is choosing which
domain to log into autonomously mid-session, with no config file to read.
Blind attempt-and-reject (call `type_secret`, get a `PortError` naming
"no 1Password item for domain X" or "N items match, ambiguous") is
*technically* sufficient per Constraints' "actionable vault-error
surfacing, no silent plaintext fallback," and avoids a second tool surface
this pass doesn't budget for (Appetite is already 3-6 weeks). But it costs
a wasted round-trip per attempt and — worse — a caller has no way to know
*whether* to attempt `type_secret` at all before trying, so it will
default to trying it on every login form it encounters, including ones
with no vault entry, generating vault-lookup traffic (and, if 1Password
rate-limits `op item get`, a real production annoyance) for domains that
were never going to resolve. Recommend Phase 3 explicitly decide
attempt-and-reject vs. a lightweight discovery call rather than let it
fall out by omission — the tradeoff is one extra `CredentialStore` method
(`list_domains() -> Vec<String>`, no field-level detail, so it can't itself
leak anything) against Appetite pressure.

### 3.2 Partial-fill / "form also wants an OTP" feedback

Grounded directly against `browser_fill_form`'s actual behavior
([tools/browser.rs:622-688](/home/tstapler/Programming/stapler-mcp/crates/core/src/tools/browser.rs#L622-L688)):
today's fill-form is fail-fast, not aggregate-report — "Fields are filled
in order; if one fails, earlier fields remain filled and the error names
which field failed" (the function's own doc comment). `type_secret` isn't
a batch call at all (one locator, one credential ref, matching
`type_text`'s shape), so there's no equivalent "did all N fields
succeed" question to answer structurally. But the OTP hand-off the
requirement calls out ("TOTP/2FA via 1Password server-side generation")
*does* need explicit handling: after `type_secret(..., field: "password")`
succeeds and the site reveals a second-factor field, the caller's only
signal that an OTP step exists is reading the returned `AxSnapshot` itself
(the way it already reads any post-action snapshot for `browser_click`
today) and recognizing a new textbox with an OTP-shaped
name/`autocomplete=one-time-code`. That's sufficient — it doesn't need a
new field on `AxSnapshot` — but it does mean `type_secret`'s docstring
should say so explicitly (return the fresh snapshot exactly like
`type_text`, so the caller's existing "inspect what changed" habit already
covers this), rather than a caller reasonably expecting some
`needs_second_factor: bool` flag that doesn't exist. Worth a note in
Phase 3's plan so the tool's MCP-facing description sets that expectation
rather than the LLM discovering it by trial and error.

### 3.3 Vault-error surfacing needs to distinguish "no item" from "op not authenticated"

Not explicitly named in requirements.md's Scope item ("actionable
vault-error surfacing"), but a real distinction `crates/native`'s `op
read`/`op item get` invocation (via `ProcessSpawner`) will hit in practice:
a service-account token that's missing/expired/revoked produces a
completely different failure than "no matching item," and the two need
different caller-facing guidance (the first is "fix your 1Password
service-account setup," an operator problem outside any single tool call;
the second is "this domain has no vault entry," an LLM-actionable "don't
retry" signal). `PortError`'s existing variants
([ports.rs:15-36](/home/tstapler/Programming/stapler-mcp/crates/core/src/ports.rs#L15-L36))
don't have an auth-vs-not-found split for this new port — `CredentialStore`
will likely want its own error enum (or a documented mapping onto
`PortError::Other` with an actionable prefix) rather than overloading
`PortError::NotFound`, which today's doc comment defines specifically as
"the caller's fix is always 'start a new session'" — not true for a vault
auth failure.
