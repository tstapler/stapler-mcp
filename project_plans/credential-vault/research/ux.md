# UX Research: credential-vault

Agent 5 (UX Research), SDD Phase 2. Scope per `requirements.md`: a
`type_secret` MCP tool built on `BrowserDriver::type_secret`, a
`CredentialRef{domain, field}` port abstraction, and structural snapshot
redaction across all existing snapshot-producing tools. "UX" here is tool-call
ergonomics for the calling LLM, not a human GUI — see requirements' framing
and issue [tstapler/stapler-mcp#26](https://github.com/tstapler/stapler-mcp/issues/26).

## 0. Codebase conventions this design must match

Read `crates/cli/src/thin_client.rs` (the `#[tool(name, description)]` macro
call sites — the actual MCP-exposed docstrings) and `crates/core/src/schema.rs`
(the `*Input`/`*Output` structs, whose `///` doc comments become per-field
JSON Schema `description`s via `schemars`).

Observed conventions, confirmed by reading, not inferred:

- **Tool descriptions are one dense paragraph**, not multi-sentence prose:
  what it does, how targets are identified (`` a `ref` from a previous
  snapshot ``), and what it returns, in that order. Example —
  `stapler_browser_type`: *"Type text into an element in an existing browser
  session, identified by a `ref` from a previous snapshot, and return the
  accessibility-tree snapshot of the page after typing."*
  ([crates/cli/src/thin_client.rs:175-177](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/cli/src/thin_client.rs#L175-L177))
- **Field docs cross-reference the producing type**, e.g. `BrowserClickInput.ref_id`:
  `` /// A `ref` from a previous `AxSnapshotOutput`. `` — the LLM is expected
  to have called a snapshot-returning tool first; no field tries to be
  self-sufficient without that context.
  ([crates/core/src/schema.rs:369-371](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/core/src/schema.rs#L369-L371))
- **Error strings are terse, lowercase, `context: detail` shaped**, built with
  `format!()`, e.g. `"navigate blocked: {reason}"`, `"snapshot {session_id}:
  {other}"`, `"field '{ref_id}': {e}"`.
  ([crates/core/src/tools/browser.rs:107](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/core/src/tools/browser.rs#L107),
  [browser.rs:215](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/core/src/tools/browser.rs#L215),
  [browser.rs:663](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/core/src/tools/browser.rs#L663))
- **`PortError` variants carry a doc comment stating the caller's fix**, not
  just a description of the failure — e.g. `NotFound`: *"the caller's fix is
  always 'start a new session'"*; `SessionCrashed`: *"the caller's fix is
  not... silently reusing the same crashed session id would just crash
  again"* ([crates/core/src/ports.rs:20-31](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/core/src/ports.rs#L20-L31)). This is the single most important
  convention for `type_secret`'s error design (§4): every vault-related
  error variant should say, in its doc comment and ideally in its rendered
  string, what the LLM should do next.
- Every existing browser action tool returns `BrowserActionOutput { snapshot,
  note }` — `note` is reserved for informational-but-not-error messages
  (e.g. "click navigated to {url}; previous refs are now invalid"). This is
  the natural home for a redaction confirmation note (§2).

## 1. Comparable UX patterns

**Playwright MCP (`microsoft/playwright-mcp`) — closest direct precedent.**
It ships a `--secrets <file>` flag / `PLAYWRIGHT_MCP_SECRETS_FILE` env var
pointing at a dotenv-format file. The LLM never sees a value; it references a
key as a `{{PLACEHOLDER}}` token inside the *ordinary* `browser_type` /
`browser_fill_form` `text` parameter, and the server substitutes the real
value server-side before it reaches the page, then redacts any matching
plaintext out of tool responses (snapshots, form-fill confirmations, and, as
of a later release, `browser_console_messages`) before they reach the model.
Two things worth carrying over:
  - The maintainers are explicit that this is *"a convenience, not a
    guaranteed security boundary"* — text-matching redaction misses secrets
    that get base64-encoded, concatenated, or split across log lines, and it
    does not protect traces/HAR captures, which record full request/response
    bodies including auth headers.
  - There's an open design proposal
    ([microsoft/playwright-mcp#922](https://github.com/microsoft/playwright-mcp/issues/922))
    for a dedicated placeholder-resolution *tool* rather than overloading
    `browser_type` — closer to this project's `type_secret` shape — where the
    tool returns only `{ok, redirectUrl}`-style metadata, never a value or a
    field echo.
  - **Implication for credential-vault**: requirements already chose the
    dedicated-tool shape over Playwright's overload-the-type-tool shape. That
    is the right call for an LLM-ergonomics reason Playwright's own issue
    tracker surfaces: a tool named `type_secret` that structurally cannot
    accept a plaintext `text` field is far harder to misuse than a shared
    `browser_type` tool where the *safe* path (a `{{PLACEHOLDER}}` token) and
    the *unsafe* path (a literal string) are both syntactically valid input to
    the same parameter. Requirements' choice to make `type_secret` "never
    accept... plaintext" at the schema level (no free-text value parameter at
    all) is stronger than Playwright's convention-based placeholder syntax,
    which an LLM can still bypass by typing a real value into the same field.

**GitHub Actions log masking** — `::add-mask::` registers a literal string for
search-and-replace-to-`***` in log output. Two properties worth noting: (1)
GitHub silently declines to mask secrets under ~3-4 characters, because
masking short/common substrings would shred normal log output — a precedent
against ever encoding *length* in a redacted display value, since short
secrets are exactly the case where length would be most identifying; (2) it's
pure string substitution, not semantic/structural, so it has the same
"any transformation defeats it" weakness Playwright's redaction has. Our
redaction is structural (AX-tree node, not string search — see §4/open
question), which avoids this weakness entirely and is a genuine advantage
worth stating explicitly in the tool description so the calling LLM
understands *why* this is safer than typing through `browser_type`.

**AWS Secrets Manager** — `GetSecretValue`'s `SecretString`/`SecretBinary`
fields are excluded from CloudTrail by design; the audit trail records *that*
a principal called `GetSecretValue` on a named secret, with caller identity,
timestamp, and source IP, but never the value. This is the precedent for
Observability Requirements' audit log: log the `CredentialRef{domain, field}`
and outcome, never the resolved `SecretValue`, mirroring AWS's separation of
"proof of access" from "the accessed material."

**1Password CLI (`op`) / 1Password MCP** — the `op://vault/item/field`
reference syntax is the direct model for `CredentialRef{domain, field}`: a
reference is safe to write down (commit, log, echo) precisely because it
carries no secret material, only a locator. 1Password's own MCP tooling
(`op_run`-equivalent) documents the same contract requirements.md specifies:
"plaintext redacted from output and never logged back to the model." This
validates `domain`/`field` (not e.g. an item UUID or vault path) as the
locator shape — domain-scoping is also the exact axis requirements' rejection
policy needs (§4).

**Skyvern / Anchor Browser** (cited directly in issue #26) — both keep the
vault query inside the automation engine, never passing secrets through the
LLM's context, and both ship first-class MFA. Confirms this feature request
is solving a problem the browser-automation-agent space has already converged
on solving the same way (vault-side resolution + a purpose-built type-then-
never-echo action) rather than inventing a novel shape.

## 2. User mental models

The calling LLM is a **cold-start reader of the tool's JSON Schema at
decision time** — per this codebase's own convention (§0), it is *expected*
to have called a snapshot tool first, because every existing locator field
(`ref_id`) already assumes that. `type_secret` should follow the same
convention rather than trying to be usable with zero prior context, for
consistency and because a `CredentialRef` genuinely can't be chosen
correctly without having seen the field: the LLM must look at an actual
`AxSnapshotOutput` node (its role, name/label, and surrounding form) to infer
which `domain`/`field` pair applies. There is no way to guess `field: "totp"`
vs `field: "password"` from the DOM alone without a redaction-tagging signal
(§4) confirming "this node is a secret-shaped field."

What the LLM needs to already believe, to call this correctly on the first
try:

1. **This tool exists and is the answer to "how do I log in."** Discoverability
   risk: an LLM reasoning about a login form has strong prior weight toward
   `stapler_browser_type` (it's the tool it already used for every other
   field on the form). The tool description must open with the trigger
   condition, not the mechanism — lead with *"Use this instead of
   `stapler_browser_type` whenever a field is a password, TOTP/2FA code, or
   other secret you have a stored credential for"* rather than leading with
   the `CredentialRef` type shape. This mirrors how `stapler_browser_fill_form`'s
   description ([thin_client.rs:357](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/cli/src/thin_client.rs#L357)) opens with *when* to
   reach for it ("instead of one `stapler_browser_type`/... call per field")
   before describing its parameters.
2. **`domain` means the site being logged into, not a vault/item identifier.**
   The field doc should say explicitly it's matched against the current
   page's origin (or the navigated-to domain), because an LLM with no other
   signal might otherwise pass a vault name, an item title, or a guessed
   slug. State the matching semantics in the field doc, not just the type
   name — `CredentialRef.domain` doc comment should read something like:
   `` /// The site's domain (e.g. "github.com") — matched against the
   current page's origin. Wrong or ambiguous matches are rejected, never
   guessed. ``
3. **`field` is a small closed-ish vocabulary, not free text.** At minimum
   `"username"` / `"password"` / `"totp"` should be enumerated in the schema
   (an enum, or a doc comment listing them) — open-ended string `field` invites
   the LLM to invent plausible-looking values (`"login"`, `"email"`,
   `"otp"`) that silently miss the vault's actual field name. If the vault
   backend (1Password) uses its own field-name vocabulary, the tool
   description should say so plainly ("field names must match 1Password's
   field labels for the item") rather than leaving the LLM to infer it.
4. **A rejection is not a retry-with-different-args signal.** Because domain
   mismatch/ambiguous-match are policy rejections, not transient failures,
   the LLM must be able to tell from the error text alone that retrying the
   same call (or a superficially different one, like guessing at a `field`
   value) won't help — see §4, and the "caller's fix" convention in §0.
5. **The returned snapshot's acted-on node shows a placeholder, not real
   text, and that is success, not a bug.** Every other `type`-family tool in
   this codebase returns the field's real post-type value inside the
   snapshot (that's the point of returning a snapshot — confirm the value
   landed). `type_secret` breaking that pattern by design is exactly the
   kind of thing that reads as a malfunction to a model pattern-matching off
   every other tool it has used this session, unless the tool description
   and/or the `note` field say so outright. Recommend: always set
   `BrowserActionOutput.note` to something like `"value redacted; typed
   successfully"` on success, so the LLM doesn't need to infer intent from a
   masked string alone.

## 3. Accessibility

Not applicable in the WCAG/human-GUI sense — stated explicitly per the task,
not silently skipped: this tool has no rendered interface for a human to
perceive, operate, or need alternative-modality access to.

The adjacent, real concern is **human auditability of what an autonomous
agent touched with real credentials**, which requirements' Observability
Requirements section (not reproduced above, but referenced by this project's
scope) should cover functionally. From a UX-of-the-audit-log angle
specifically (i.e., is the log usable by the *human*, not just complete):

- Log lines should be readable without joining against a code path — the
  AWS CloudTrail precedent (§1) of recording principal + timestamp + secret
  *identifier* (never material) is the right shape, but the human-usability
  addition is: log the *domain* and *field* in plain text right in the line
  (not just an opaque ref id needing a lookup), so `grep`-ing a daemon log
  for "what did the agent type into my banking site" works with a plain
  `grep <domain>` rather than requiring a second system.
- Every rejection (domain mismatch, ambiguous match) is exactly as
  audit-worthy as a success — arguably more so, since it's the signal that
  something (a misconfigured vault, a spoofed page, a confused agent) needs
  human attention. The audit log should not distinguish "we typed it" from
  "we refused to type it" as log-worthy vs. not; both are state changes a
  human reviewing later needs visibility into.
- Given this is a solo-operator daemon (no multi-tenant human audience), the
  practical bar is "can Tyler `journalctl`/log-tail and answer 'did the agent
  ever touch my prod DB credential today'" — favor one grep-able structured
  log line per `type_secret` call (attempted, rejected, or succeeded) over a
  separate audit subsystem, unless Observability Requirements already
  specifies structured logging infra this should reuse.

## 4. Error states — example strings

Styled to match this codebase's existing `format!("{context}: {detail}")`
convention (§0) and the `PortError` pattern of a doc comment naming the
caller's fix. Each includes what the calling LLM should infer from it.

**1. Domain-mismatch rejection**

```
credential rejected: no vault entry for domain "example.com" (current page is
example.com, but the stored credential is scoped to accounts.example.org) —
not typed
```

LLM should infer: *stop, don't retry this call unmodified.* Either the
`domain` argument was wrong (fixable — re-check the current page's actual
origin from the last snapshot) or no credential exists for this site at all
(not fixable by retrying — surface to the human). Naming *both* domains (the
one requested and the one on the current page, if they differ) prevents a
misdiagnosis loop where the LLM assumes it's a typo and keeps guessing domain
spellings.

**2. Ambiguous multi-item vault match**

```
credential rejected: 3 vault items match domain "example.com" — ambiguous,
not typed. Ask the user which item to use, or scope the request further.
```

LLM should infer: *this is not fixable by retrying with the same
`CredentialRef` shape* — `domain`/`field` alone can't disambiguate further
(requirements' `CredentialRef` has no item-id field, by design, to avoid
leaking vault structure to the LLM). The explicit "ask the user" instruction
in the error text itself matters here more than in most error strings,
because there's no in-band way for the LLM to self-resolve ambiguity — this
is one of the few states in this tool where escalation to the human is the
*only* correct next action, and the message should say that outright rather
than making the LLM infer it from a generic "ambiguous" adjective.

**3. Vault unauthenticated**

Verified against the real `op` CLI locally (`op vault list` with no active
session, `op` 2.34.1): `op` itself returns *"You are not currently signed
in. Please run `op signin --help` for instructions."* — this is a strong
precedent for what "actionable" looks like: it names the exact next command.
`stapler-mcp`'s wrapped version should preserve that actionability rather
than flattening it to a generic port error:

```
vault unauthenticated: 1Password CLI reports not signed in — not typed. This
requires human action (run `op signin` or unlock the desktop app); the agent
cannot resolve this itself.
```

LLM should infer: *do not retry in a loop* — this is an environment
precondition, not a bad argument, and no amount of query reformulation fixes
it. The "requires human action" framing directly forecloses the failure mode
where an agent burns several turns retrying the same call.

**4. TOTP-window expired mid-flow**

```
credential rejected: TOTP code for example.com expired before it could be
typed (generated {t}, window closed {t+30s}) — not typed. Retry the same
call; a fresh code will be generated.
```

This is the one error in this set that *is* safely retryable, and the
message should say so explicitly — otherwise it looks identical in shape to
domain-mismatch/ambiguous-match (both "credential rejected... not typed") and
an LLM applying the same "don't retry" heuristic from cases 1-2 would
incorrectly give up on a transient timing issue. Recommend a distinct
message shape/prefix (`credential rejected:` for policy rejections vs. a
different lead-in, e.g. `credential expired:`, for this one) precisely so
the LLM can pattern-match "retryable" vs. "not retryable" from the error
family/prefix without parsing full sentences — mirrors `PortError` splitting
`NotFound` from `SessionCrashed` for the same reason (§0).

**5. Locator doesn't resolve to a password-type field**

```
type_secret refused: ref "e14" resolves to a plain text field (role=textbox,
no protected/password state), not a password or TOTP input — use
stapler_browser_type for non-secret fields, or re-snapshot if this field
should be a password field.
```

This is the tool actively refusing to be misused as a general-purpose typer
— arguably the most important error for tool-safety, since accepting *any*
locator would make `type_secret` a plaintext-typing tool with extra steps.
The message should redirect to the *right* tool (`stapler_browser_type`)
rather than just saying "invalid," since the most likely cause is the LLM
picked the wrong `ref` off a snapshot for an adjacent field.

## Open questions — recommendations

**Should the redacted placeholder be a fixed string, or encode length/shape?**

**Recommendation: fixed string, no length/shape encoding.** Evidence across
every comparable system converges on this:

- GitHub Actions masking declines to mask very short secrets specifically
  *because* short length is itself identifying against normal log content —
  the inverse argument (deliberately revealing length) makes a redacted value
  easier to narrow down, not harder, for anything short (4-digit PIN,
  6-digit TOTP code — this project's own explicit `field: "totp"` case is a
  6-character secret where length-preserving redaction (`••••••`) reveals
  the *entire* character count of a 6-digit code, materially narrowing a
  brute-force space where fixed-length masking normally protects it).
- AWS Secrets Manager excludes the value from CloudTrail entirely rather
  than including a shaped stand-in.
- Playwright MCP's redaction replaces matched plaintext outright, not with a
  length-preserving mask.
- This codebase's own `BrowserActionOutput.note` convention (§0) already has
  a place to put *debugging* affordance ("typed successfully") without
  touching the value's shape at all — so the debuggability argument for
  length/shape ("did it actually type something, and roughly how much") is
  already served by `note` plus a boolean-shaped success signal, with no
  need to leak shape through the placeholder itself.

Use one fixed placeholder string, identical regardless of the real value's
length or content (e.g. `"••••••••"` or a literal `"[REDACTED]"` — either is
fine; prefer whichever renders unambiguously as non-literal-content in a
terminal/log viewer with no Unicode-rendering risk, which slightly favors
`"[REDACTED]"` over bullet characters for a CLI-first tool ecosystem). The
one piece of debuggability worth keeping is a boolean/note-level "typed
successfully" signal — never anything derived from the value itself (not
length, not first/last character, not character-class shape).

## Sources

- [thin_client.rs](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/cli/src/thin_client.rs) — this repo, `#[tool]` descriptions (read directly, lines 94-457)
- [schema.rs](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/core/src/schema.rs) — this repo, `*Input`/`*Output` structs (read directly, lines 322-439)
- [ports.rs](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/core/src/ports.rs) — this repo, `PortError` (read directly, lines 16-51)
- [browser.rs](https://github.com/tstapler/stapler-mcp/blob/10faf5eb0328b66e8cf2df73d00c7e7e398e0076/crates/core/src/tools/browser.rs) — this repo, error-string call sites (read directly)
- [tstapler/stapler-mcp#26](https://github.com/tstapler/stapler-mcp/issues/26) — originating issue, cites Skyvern/Anchor Browser precedent
- [microsoft/playwright-mcp#922](https://github.com/microsoft/playwright-mcp/issues/922) — placeholder-resolution design proposal
- [Playwright MCP and CLI redact secrets from console logs (bug0.com)](https://bug0.com/blog/playwright-mcp-cli-secret-redaction-2026) — `--secrets` mechanism, text-matching redaction caveats
- [GitHub Actions secret masking (SSW.Rules)](https://www.ssw.com.au/rules/mask-secrets-in-github-actions) — `::add-mask::`, minimum-length masking exemption
- [AWS Secrets Manager CloudTrail log entries](https://docs.aws.amazon.com/secretsmanager/latest/userguide/cloudtrail_log_entries.html) — `SecretString`/`SecretBinary` excluded from audit trail
- [1Password: Securing MCP servers with 1Password](https://1password.com/blog/securing-mcp-servers-with-1password-stop-credential-exposure-in-your-agent) — `op://` reference pattern, plaintext-never-logged-to-model contract
- `op vault list` run locally against 1Password CLI 2.34.1 with no active session (VERIFIED, command run directly) — exact unauthenticated error text: *"You are not currently signed in. Please run `op signin --help` for instructions"*
- [MDN: ARIA textbox role](https://developer.mozilla.org/en-US/docs/Web/Accessibility/ARIA/Reference/Roles/textbox_role) / [W3C HTML-AAM](https://w3c.github.io/html-aam/) — `input type="password"` maps to role `textbox` + state `protected`, not a distinct role; structural redaction must key off the `protected`/password DOM attribute, not role name or field label text
