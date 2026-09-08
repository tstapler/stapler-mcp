# Build vs. Buy: credential-vault

Research for [requirements.md](../requirements.md) (GitHub issue
[#26](https://github.com/tstapler/stapler-mcp/issues/26)). Question: build
`CredentialStore` + `type_secret` + AX-snapshot redaction ourselves, or source
some/all of it externally?

## Bottom line

**Build**, largely as requirements.md already scopes it. Nothing found gives
away the browser-automation-specific half of this problem (secret injection
that never crosses the accessibility-snapshot leak vector) as a reusable
library, and the one SaaS product that does this well (1Password + Browserbase)
is tied to a cloud browser this project doesn't use and would violate the
project's own data-residency constraint. Two narrow "buy" decisions are
already made for us: `zeroize` is already a transitive dependency, and
`publicsuffix`/`psl` should replace any hand-rolled domain-match string
comparison. TOTP generation should defer to `op`/the 1Password SDK, never
touch `totp-rs`, per requirements.md's constraint and confirmed by the
`op item get --otp` behavior this pass already requires.

---

## 1. OSS library/framework for browser-automation-specific credential injection

**Verdict: Not recommended (nothing off-the-shelf covers the actual leak vector) — build.**

The requirements.md-flagged leak vector — a secret typed into a DOM field
becomes visible again the moment the automation layer reads back an
accessibility snapshot or DOM state — is a live, acknowledged, *unfixed* gap
in the ecosystem, not a solved problem with a library to reach for:

- **Playwright core**: masking exists only for **visual regression
  screenshots** (`toHaveScreenshot({ mask: [...] })` — overlays a pink box on
  the *pixel* output) and, as of Playwright MCP 0.0.77 (June 2026), for
  **console log messages and network payloads** via a `--secrets` dotenv file
  ([bug0.com writeup](https://bug0.com/blog/playwright-mcp-cli-secret-redaction-2026)).
  Neither covers the accessibility tree. A direct feature request —
  [`fillSensitive()` / `fill(..., { mask: true })`, issue #38673](https://github.com/microsoft/playwright/issues/38673)
  — was **closed as "not planned"** (P3-collecting-feedback). Trace files and
  HAR captures still record full request/response bodies including auth
  headers; storage-state exports still contain session cookies in plaintext.
- **Playwright MCP specifically**: [issue #1566](https://github.com/microsoft/playwright-mcp/issues/1566),
  opened April 2026, reports exactly this project's threat model — "a test
  password I filled out manually was serialized as part of the accessibility
  tree" and sent to the LLM. The issue is closed but the fetched page gave no
  resolution detail; treat "fixed upstream" as unverified until re-checked
  against a current Playwright MCP release, not assumed.
- **Puppeteer / `chromiumoxide`**: no evidence of any secret-redaction or
  credential-injection feature in either. Puppeteer's `Accessibility` class
  and `chromiumoxide`'s CDP wrapper are raw AX-tree/CDP passthroughs with no
  masking layer — confirming this really is adapter-layer work this project
  has to do itself for both the native (`chromiumoxide`) and wasm
  (`playwright-core`) adapters, as requirements.md already assumes.
- **One reusable pattern, not a library**: [browser-use](https://github.com/browser-use/browser-use)
  (Python, MIT) ships a `sensitive_data` parameter that (a) scopes secrets to
  a domain via regex pattern, and (b) substitutes placeholders so "the LLM
  only sees placeholders for sensitive data rather than the actual values"
  ([browser-use docs](https://docs.browser-use.com/open-source/examples/templates/sensitive-data)).
  This is the closest prior art to `type_secret` + domain-scoped
  disambiguation in requirements.md's scope — worth reading as a design
  reference during `sdd:3-plan` — but it's Python, MIT-licensed (compatible
  license, not that it matters since nothing is copied verbatim), and solves
  it by never handing the raw value to the *LLM*, not by redacting the
  *accessibility snapshot* itself (browser-use doesn't expose one). It
  doesn't transfer as code, only as a design pattern.

## 2. SaaS/managed credential-injection-as-a-service

**Verdict: Not recommended given this project's constraints — the strongest option (1Password + Browserbase) fails the data-residency and cloud-browser constraints outright.**

- **1Password "Secure Agentic Autofill"** ([1Password blog](https://1password.com/blog/closing-the-credential-risk-gap-for-browser-use-ai-agents),
  [Browserbase blog](https://www.browserbase.com/blog/1password-agentic-autofill),
  announced Oct 8, 2025, early access): closest thing to a managed version of
  this exact feature. Architecture: agent requests a credential → 1Password
  requests human approval on the user's device → credential is injected
  **directly into the browser extension context** over a Noise-protocol
  encrypted channel, bypassing the LLM/agent entirely. This is a stronger
  isolation guarantee than what `type_secret` alone can offer (this project's
  `type_secret` still keeps the secret in daemon-process memory briefly; 1P's
  design never lets it leave the browser-extension/1Password boundary).
  **But**: it is currently launched *only* for **Browserbase**, a hosted
  cloud-browser platform — this project drives a locally-controlled
  `chromiumoxide`/local-Chrome or wasm/`playwright-core` session, not
  Browserbase. No standalone API was found (WebFetch of the announcement
  explicitly could not confirm one exists). Adopting it would mean either (a)
  routing browser sessions through Browserbase's cloud infra — directly
  contradicts requirements.md's data-residency preference ("no data leaves
  the machine except to 1Password's own service") since page content and
  session control would now also transit Browserbase — or (b) waiting on an
  unannounced general-availability API with unknown pricing/terms. Revisit
  when 1Password documents a non-Browserbase integration path.
- **Anchor Browser + 1Password**: [docs.anchorbrowser.io/integrations/1password](https://docs.anchorbrowser.io/integrations/1password)
  describes the same shape (service-account token resolves credentials
  server-side, "never exposed to the AI agent, logs, or API responses") but
  Anchor Browser is itself a hosted managed-browser product, same
  data-residency objection as Browserbase, and its own OSS/licensing status
  couldn't be confirmed from the docs page fetched (needs a follow-up check
  of its main repo if this option is reconsidered).
- **Doppler / Infisical**: both are generic secrets managers (env-var
  injection, K8s operators, CLI wrapping a process's env). Neither has, or
  appears to be building, a browser-automation-specific product — confirmed
  by web search turning up only generic secrets-manager comparison content,
  no browser/AX-tree integration. Out of scope as a "buy" option; requirements.md's
  choice to standardize on `op` (1Password) for this pass is unaffected.

## 3. LLM-generated implementation vs. battle-tested library, per security-critical piece

| Piece | Verdict | Rationale |
|---|---|---|
| Secret memory zeroing | **Buy — already decided.** `zeroize` (Apache-2.0 OR MIT, RustCrypto-adjacent, 672M+ downloads on crates.io) is **already present in this repo's `Cargo.lock`** (`zeroize 1.9.0`, confirmed via `grep` — currently a transitive dependency, not yet a direct one for `SecretValue`). Use it directly for `SecretValue`'s `Drop` impl rather than hand-rolling zero-on-drop; it exists precisely because compiler-optimization-safe zeroing is easy to get subtly wrong by hand. |
| TOTP generation | **Do not build/use `totp-rs`.** [`totp-rs`](https://crates.io/crates/totp-rs) is real (MIT, actively maintained, v6.0.0 released ~9 days before this research, 408K downloads/month) and would be the right crate *if* this project ever generated TOTP codes itself. But requirements.md's own scope line is explicit: "TOTP/2FA via 1Password server-side generation" — i.e. `op item get --otp` / the SDK returns the *code*, this project never touches the TOTP seed/secret. Keep it that way: touching the raw TOTP secret at all (even to feed `totp-rs`) reintroduces the exact plaintext-secret-handling risk this whole project exists to eliminate. `totp-rs` is noted here only as the fallback-of-last-resort if a future site requires local TOTP generation 1Password can't do server-side — not for this pass. |
| Domain-matching / disambiguation | **Buy.** Use [`publicsuffix`](https://crates.io/crates/publicsuffix) (MIT/Apache-2.0, v2.3.0) or [`psl`](https://crates.io/crates/psl) (MIT/Apache-2.0, v2.1.231) for eTLD+1 comparison rather than hand-rolled string/suffix matching. `publicsuffix` parses Mozilla's PSL and can refresh it at runtime (slower, always current); `psl` embeds a static list at compile time (faster, needs a crate bump to pick up PSL changes). Given this project's threat model — "a buggy domain-match could type a credential into a phishing page" — prefer `publicsuffix` or `psl` over anything hand-rolled specifically because PSL-unaware string matching (`ends_with(".com")`-style logic) is the exact class of bug that lets `evil-github.com.attacker.net` or a multi-label public suffix (`co.uk`, `github.io`) slip past a naive check. Recommend `psl` unless the project wants runtime PSL updates without a crate release, in which case `publicsuffix`. |

## 4. Fork or adapt an existing OSS agent-browser project

**Verdict: Not recommended as a code fork; useful only as design reference.**

| Project | Credential-vault integration | License | Forkable? |
|---|---|---|---|
| [Skyvern](https://github.com/Skyvern-AI/skyvern) | Real and shipped: built-in credential providers for Bitwarden, 1Password, and Azure Key Vault, configured via a Credential Parameter (Vault ID + Item ID) in the workflow editor ([Skyvern docs](https://www.skyvern.com/docs/credentials/custom-credential-service)). Whether it actually avoids the AX-tree/DOM-leak vector this project cares about **could not be confirmed** — the credentials doc covers sourcing/config only, not LLM-context isolation mechanics. | **AGPL-3.0** ("all core logic... with the exception of anti-bot measures available in our managed cloud offering") | **No.** stapler-mcp is MIT-licensed (confirmed: repo `LICENSE` file). Copying AGPL-3.0 code into an MIT codebase is a license violation — AGPL's copyleft would force either relicensing the borrowed module (poisoning anything that links against it) or dropping it. At most: read Skyvern's credential-parameter design for inspiration, write no code from it. |
| [vercel-labs/agent-browser](https://github.com/vercel-labs/agent-browser) | **Not implemented.** [Issue #711](https://github.com/vercel-labs/agent-browser/issues/711) requests exactly this (pull from a 1Password vault via `op` CLI, especially for 2FA) — open, unlabeled, no maintainer response, no linked PR. The "vault plugin with `credential.read` capability" referenced in secondary sources is aspirational/plugin-architecture scaffolding, not a working 1Password integration. | Vercel-copyrighted (specific license text not confirmed from the page fetched — check before any reuse) | No — nothing to fork; the feature doesn't exist yet. |
| [browser-use](https://github.com/browser-use/browser-use) | Real, shipped, MIT — see §1. Domain-scoped `sensitive_data` + LLM-side placeholder substitution. | MIT | Not literally forkable (Python vs. this project's Rust, and a different mechanism — LLM-context substitution, not AX-tree redaction) but the **design pattern is directly reusable**: domain-scoped credential dict + placeholder-before-LLM is a reasonable model for this project's own domain-scoped lookup/disambiguation policy (requirements.md scope item). |
| Anchor Browser | Real, per §2, but hosted/managed — no OSS repo confirmed from the docs fetched. | Unconfirmed | Not assessed further — same data-residency objection as §2 makes it moot even if OSS. |

Net: Skyvern is the only project with a *shipped* 1Password integration doing
roughly what this project wants, but its AGPL-3.0 license rules out forking
into this MIT repo. browser-use offers a legitimately reusable *pattern*
(not code) for domain-scoped secret handling. Nothing changes requirements.md's
scope — this is still bespoke work for the `CredentialStore` port,
`type_secret`, and AX-snapshot redaction, informed by (not copied from)
Skyvern's config model and browser-use's disambiguation pattern.
