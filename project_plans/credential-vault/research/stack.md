# Stack Research: credential-vault

Date: 2026-09-08. Scope: resolve concrete libraries/versions/patterns for the
`CredentialStore` port + `type_secret` + snapshot-redaction work described in
`project_plans/credential-vault/requirements.md`.

## 1. `op` CLI service-account workflow

**Env var**: `OP_SERVICE_ACCOUNT_TOKEN`. Set it in the process environment
before invoking `op`; no `op signin` call is needed or possible with a
service-account token — it bypasses interactive auth entirely,
non-interactive by construction. ([Use service accounts with 1Password
CLI](https://developer.1password.com/docs/service-accounts/use-with-1password-cli/))

**Supported commands for this feature**: `op read` (secret-reference syntax,
`op://vault/item/field`) and `op item get --vault <id> <item> --format json`
are both explicitly supported under a service account. `op item get` and
`op read` each cost **3 requests** against the account's rate limit unless
you pass vault *and* item **IDs** (not names) — then it drops to 1. Given
`requirements.md`'s domain-scoped lookup policy needs an item lookup by
domain (name-based) before it can resolve an ID, expect the first resolve
against a new domain to cost 3 requests and design the daemon to cache the
resolved vault/item ID pair in memory for the process lifetime to keep
repeat resolves (e.g. password then `totp` field on the same login) at 1
request each.

**Version**: current release is **2.39.0** (per
[releases.1password.com/developers/cli](https://releases.1password.com/developers/cli/),
confirmed via WebSearch — "faster secret reads, improved debug output"). No
`op` CLI is currently pinned anywhere in this repo (`stapler-mcp` has no
Ansible role of its own — `bootstrap/roles/secrets/` lives in the separate
`dotfiles` repo and wasn't readable from here); pin to `>= 2.30` in whatever
setup docs/checks this project adds, since service-account behavior has been
stable across that range and 2.39.0 is confirmed current as of this
research.

**Invocation pattern for the native adapter**: shell out via the existing
`ProcessSpawner`-style pattern (`crates/core/src/ports.rs:98`) — i.e. a new
narrow port method (or a dedicated `CredentialStore` adapter using the same
underlying process-spawn primitive) that runs `op read
"op://<vault>/<item>/<field>"` with `OP_SERVICE_ACCOUNT_TOKEN` set from
`EnvPort::var` (`crates/core/src/ports.rs:105-108`) at daemon startup, and
captures stdout only — never argv, since a subprocess's argv is visible to
other processes on the same host via `/proc` or `ps`. Prefer `op read` over
`op item get` where a single field value is all that's needed (1Password's
own guidance: `op read` with a fully-qualified secret reference is the
cheaper, more secret-shaped operation of the two).

## 2. `@1password/sdk` (1Password JavaScript SDK)

**Current version**: **0.5.0** (confirmed via `npm view`/registry:
`registry.npmjs.org/@1password/sdk/latest` → `sdk-0.5.0.tgz`, published
~1 month before this research). Still pre-1.0 — expect breaking changes
across minor versions; pin exactly rather than with `^`.

**Service-account auth — supported, not just Connect-server auth**:
confirmed directly in the npm README. Two `createClient` auth modes exist:

```js
const sdk = require("@1password/sdk");

const client = await sdk.createClient({
  auth: process.env.OP_SERVICE_ACCOUNT_TOKEN, // service-account token, a plain string
  integrationName: "stapler-mcp",
  integrationVersion: "0.1.0",
});

const secret = await client.secrets.resolve("op://vault/item/field");
```

The other mode (`auth: new sdk.DesktopAuth("account-name")`) is
human-in-the-loop desktop-app auth — not applicable here (single-user
headless daemon, no interactive prompt to answer). Service-account auth is
explicitly the SDK's own recommendation "for automated access and limiting
your integration to least privilege access."
([npmjs.com/package/@1password/sdk](https://www.npmjs.com/package/@1password/sdk))

**TOTP-generation API**: the SDK's item model has a `Totp` field type (see
[Manage items — TOTP
appendix](https://developer.1password.com/docs/sdks/manage-items/#totp)).
Reading it is documented as "Get a one-time password": fetch the item via
`client.items.get(vaultId, itemId)`, then read the `Totp`-typed field's
`value` — the SDK computes the live 6-digit code from the field's stored
`otpauth://` URL or raw seed at read time, mirroring `op item get`'s `otp`
output. There is no separate "generate TOTP" call; it's just a field read
like any other, which fits this project's uniform
`CredentialStore::resolve(&CredentialRef{ field: "totp" })` shape from
`requirements.md` — `field: "totp"` maps to reading the item's `Totp` field
value rather than a special-cased method.

**Wasm-bindgen glue compatibility**: yes, drops into the existing pattern in
`crates/wasm/src/glue/browser.js`. That file's structure — a lazily
initialized module-level singleton (`browserPromise`, guarded by
`getBrowser()`) built once and reused, plus a set of `module.exports.jsFoo =
async function(...)` entry points called from the Rust side — is exactly the
shape a new `crates/wasm/src/glue/credentials.js` should follow: a lazy
`clientPromise`/`getClient()` singleton wrapping `sdk.createClient(...)`
(created once per daemon lifetime, same as the shared `Browser` process),
plus `module.exports.jsResolveCredential = async function(domain, field) {
... }` that calls `client.secrets.resolve(...)` or `client.items.get(...)` +
field lookup and returns a plain value for the wasm boundary. No async-init
surprises beyond what `getBrowser()` already handles — `createClient` is
itself async and awaited exactly once behind the same kind of guard.

## 3. `keyring` crate (Rust) — optional caching layer

Not currently a dependency anywhere in the workspace (`Cargo.toml` at repo
root only declares the `[workspace]` table; `crates/native/Cargo.toml` has
no `keyring` entry — checked directly).

**Current version**: **4.2.0** (`raw.githubusercontent.com/hwchen/keyring-rs/master/Cargo.toml`).
Two feature modes: `v1` (default) — a simple, all-in-one password/secret
manager, API-compatible with the older keyring 1.x/2.x/3.x usage most
examples online show — and `cli`, which links every backend for building
CLI tools. `v1` is what a caching layer would want.

**Platform backend coverage for this user's machines** — `v1`'s default
feature set is exactly the two platforms in scope:
- **Linux (Manjaro, primary)**: `zbus-secret-service-keyring-store` — a
  pure-Rust (via `zbus`) D-Bus Secret Service client, talking to whatever
  the desktop's Secret Service provider is (GNOME Keyring's `gnome-keyring-daemon`
  or KWallet's Secret Service shim on KDE — Manjaro ships either depending
  on desktop environment). No native `libdbus` linkage needed (the `zbus`
  backend is pure Rust); a `dbus-secret-service-keyring-store` variant also
  exists (native libdbus-based) but isn't in the default `v1` feature set.
- **macOS (work)**: `apple-native-keyring-store` with the `keychain`
  sub-feature — wraps the system Keychain via Apple's Security framework.

**Recommendation for this project**: keyring-as-cache is a reasonable
optional layer *in front of* `op`, but note the requirements' own risk
framing — "Actionable vault-error surfacing, no silent plaintext fallback"
and the observability requirement that every resolve attempt be logged
(resolved/rejected/failed) — a keyring cache changes what "resolved" means
(cache hit vs. live `op` call) and should log which path served the
resolve. Given the stated non-functional target ("no worse than an
interactive `op read` invocation... not a hot path"), a caching layer is not
load-bearing for this project's Appetite/timeline and could reasonably be
deferred past the first pass — flagging it as an open scoping question for
`sdd:3-plan` rather than a settled yes/no here.

## 4. CDP `DOM.describeNode` via chromiumoxide (native adapter)

**Pinned version**: `crates/native/Cargo.toml:11` pins `chromiumoxide =
"0.9"`, which resolves to the current latest, **0.9.1** (confirmed via
`crates.io/api/v1/crates/chromiumoxide` — `default_version`/`max_version`
both `0.9.1`).

**Confirmed support — already wired, not something to add**: chromiumoxide's
public `Element` type (`src/element.rs` in the `mattsse/chromiumoxide` repo,
read directly) has two directly relevant methods already:

- `Element::description(&self) -> Result<Node>` — calls `DOM.describeNode`
  (`DescribeNodeParams::builder().backend_node_id(self.backend_node_id).depth(100).build()`)
  and returns the raw CDP `Node`, whose `.attributes` field is the flat
  `[name1, value1, name2, value2, ...]` array CDP returns — this is the
  literal `DOM.describeNode` call the requirements' Rabbit Holes section
  calls out as needing "`DOM.describeNode` or eval-on-node."
- `Element::attribute(&self, attribute: impl AsRef<str>) -> Result<Option<Value>>`
  — a convenience wrapper that does the eval-on-node alternative instead
  (`this.getAttribute('type')` via `call_js_fn`), useful if the redaction
  check wants a single attribute rather than the whole flat array.
- `Element::attributes(&self) -> Result<Vec<String>>` — flat array from
  `description()`, ready to scan for `"type"`/`"password"` pairs without a
  second round trip.

Either `description()` (real `DOM.describeNode`, matches the requirement's
own wording) or `attribute("type")` (JS eval, one CDP round trip via
`Runtime.callFunctionOn` instead) works; `description()` is the more direct
match for "structural snapshot redaction... keyed off DOM `type=password`"
since it's a real DOM-domain call, not a JS eval that could itself be
affected by page script tampering with `Element.prototype.getAttribute`.
The redaction pass in `AxSnapshot`-building code should call
`Element::description()` (or `.attributes()`) per form-control node and
check for `("type", "password")` in the flat pairs before deciding whether
to redact `AxNode.value`.

## 5. Playwright equivalent (wasm adapter, Node side)

**Pinned version**: `npm/package.json:21` pins `"playwright-core":
"^1.61.1"`.

**API**: Playwright's `Locator` (the only handle type used anywhere in
`crates/wasm/src/glue/browser.js` — see `refLocator()` at line 383, used by
every ref-targeted action) has `Locator.getAttribute(name)`, which returns
the attribute's string value or `null`. For redaction purposes:

```js
const type = await refLocator(page, refId).getAttribute("type");
if (type === "password") { /* redact this node's value in the snapshot */ }
```

This is a direct, first-class Locator method — no `evaluate()` escape hatch
needed (Playwright's own docs note `ElementHandle` is discouraged in favor
of `Locator`, and this codebase already uses `Locator` exclusively via
`refLocator`). For the structural, snapshot-wide redaction pass (not just
the acted-on node from `type_secret`), the wasm adapter's `captureSnapshot`
(`crates/wasm/src/glue/browser.js:469-474`) parses `page.ariaSnapshot()`
text — that ARIA-snapshot text has no `type` attribute in it at all, so
redaction can't be a post-hoc string scan of the ARIA snapshot; it needs a
second pass that walks the *live* page's password-type inputs (e.g.
`page.locator('input[type="password"]')` via `page.$$eval` or an ARIA
`ref=`-to-DOM-node matching step) and redacts the matching node(s) in the
already-parsed tree before returning it. This mirrors the native side's
symmetric challenge: `AxSnapshot` construction there also needs a
`type=password` check merged into whatever CDP accessibility-tree walk
already builds `AxNode`s (`crates/core/src/ports.rs:150-162`), not a
separate call bolted on afterward.

## Summary of concrete additions

| Component | Crate/package | Version | Where |
|---|---|---|---|
| `op` CLI (native, spawned) | `op` binary (not a Cargo dep) | 2.39.0+ (any recent 2.3x) | invoked via `ProcessSpawner`-style port |
| 1Password JS SDK (wasm) | `@1password/sdk` | 0.5.0 (pin exact, pre-1.0) | new `npm/package.json` dependency |
| Optional cache (native) | `keyring` | 4.2.0, `default-features` (`v1`) | new `crates/native/Cargo.toml` dependency, deferred pending `sdd:3-plan` scoping |
| DOM type-attribute lookup (native) | `chromiumoxide` | already pinned `"0.9"` (0.9.1) | `Element::description()`/`.attribute("type")` — no new dependency |
| DOM type-attribute lookup (wasm) | `playwright-core` | already pinned `"^1.61.1"` | `Locator.getAttribute("type")` — no new dependency |
