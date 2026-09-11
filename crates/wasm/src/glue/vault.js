// Wasm-side `CredentialStore` glue (Epic 4.2): wraps `@1password/sdk`
// directly (not the `op` CLI native's `crates/native/src/vault.rs` shells
// out to) — mirrors that file's shape (domain-scoped Login-item lookup,
// in-flight dedup, ADR-003) one level up in the wasm/JS stack, and mirrors
// `browser.js`'s lazy-singleton `getBrowser()` pattern for the SDK client
// itself.
const {
    createClient,
    RateLimitExceededError,
    AuthExpiredError,
    DesktopSessionExpiredError,
} = require("@1password/sdk");

function defaultClientFactory() {
    return createClient({
        auth: process.env.OP_SERVICE_ACCOUNT_TOKEN,
        integrationName: "stapler-mcp",
        integrationVersion: "0.1.0",
    });
}

// One shared authenticated client for the daemon's whole lifetime, lazily
// constructed on first use — same reasoning as `browser.js`'s `browserPromise`
// (re-authenticating per call would be both slow and needlessly chatty
// against 1Password's API).
let clientFactory = defaultClientFactory;
let clientPromise = null;
function getVaultClient() {
    if (!clientPromise) {
        clientPromise = clientFactory();
    }
    return clientPromise;
}

// Test-only seam (mirrors `crates/native/src/vault.rs`'s `TEST_LOG_SINK`
// override pattern): lets a Node test substitute a fake client factory
// instead of the real `@1password/sdk`, so the singleton/dedup/error-mapping
// behavior below can be exercised without real credentials or network
// access. Resets `clientPromise` too so the very next `getVaultClient()`
// call picks up the injected factory rather than a client built before the
// test ran.
module.exports.__setClientFactoryForTesting = function (factory) {
    clientFactory = factory;
    clientPromise = null;
};
module.exports.__resetForTesting = function () {
    clientFactory = defaultClientFactory;
    clientPromise = null;
    pending.clear();
};

// In-flight dedup (ADR-003 item 3, Story 4.2.2): coalesces concurrent
// identical `jsResolveCredential` calls into one underlying SDK round trip.
// Keyed by `${domain}::${field}`, holding the in-progress `Promise` itself —
// unlike native's `Shared<LocalBoxFuture<...>>` plumbing, a plain JS
// `Promise` is already safely awaitable by multiple callers, so no extra
// cloning/waiter-list machinery is needed. `.finally()` below removes the
// entry the instant the real call settles (success or failure), keeping this
// dedup-only, never a value cache — the next `jsResolveCredential` for a
// previously-seen key always triggers a fresh SDK call.
const pending = new Map();

// Same bracket/zone-id-stripped, lowercased normalization discipline as
// `browser.js`'s `isBlockedHost` (not its private-IP logic — just "normalize
// before comparing") so a domain match isn't defeated by case or literal
// formatting differences between the requested domain and a stored item's
// website URL.
function normalizedHost(url) {
    return url.hostname.toLowerCase().replace(/^\[|\]$/g, "").split("%")[0];
}

// Mirrors `crates/core/src/tools/webcrawl.rs`'s `same_host` (used natively
// via `crates/native/src/vault.rs::lookup_domain`) — exact host-string
// equality, no suffix/subdomain matching. `domain` has no scheme (it's a
// bare host from `CredentialRef`), so it's parsed the same way native's
// `lookup_domain` does: `https://${domain}`.
function sameHost(domain, candidateUrlStr) {
    let candidate;
    try {
        candidate = new URL(candidateUrlStr);
    } catch {
        return false;
    }
    let target;
    try {
        target = new URL(`https://${domain}`);
    } catch {
        return false;
    }
    return normalizedHost(candidate) === normalizedHost(target);
}

// Mirrors native's `pick_unique_match`: resolves a domain's candidate list
// down to exactly one `{vaultId, itemId, title}`, or the corresponding
// `ux.md` §4 example-1/example-2 rejection — verbatim strings, so
// `WasmCredentialStore`'s error mapping (`crates/wasm/src/vault.rs`) can
// classify them the same way native's `PortError` construction does.
function pickUniqueMatch(domain, matches) {
    if (matches.length === 0) {
        throw new Error(`no vault entry for domain "${domain}" — not typed`);
    }
    if (matches.length > 1) {
        const titles = matches.map((m) => m.title).join(", ");
        throw new Error(
            `${matches.length} vault items match domain "${domain}" — ambiguous, not typed. Ask the user which item to use, or scope the request further. Candidates: ${titles}`,
        );
    }
    return matches[0];
}

// Mirrors native's `lookup_domain`/`parse_matching_items`/`item_matches_domain`,
// adapted to the SDK's vault-scoped `items.list(vaultId)` shape (unlike the
// `op` CLI's single account-wide `item list --categories Login`, the SDK has
// no cross-vault list call, so every accessible vault is enumerated in turn).
// `ItemOverview.websites` already carries each item's website URLs, so no
// per-item `items.get()` fetch is needed just to filter by domain — only the
// eventual username/password read (`resolveUncached` below) fetches the full
// item.
function itemMatchesDomain(item, domain) {
    if (item.category !== "Login") {
        return false;
    }
    const websites = item.websites || [];
    return websites.some((w) => sameHost(domain, w.url));
}

async function lookupDomain(client, domain) {
    const vaults = await client.vaults.list();
    const matches = [];
    for (const vault of vaults) {
        const items = await client.items.list(vault.id);
        const found = items
            .filter((item) => itemMatchesDomain(item, domain))
            .map((item) => ({ vaultId: item.vaultId, itemId: item.id, title: item.title }));
        matches.push(...found);
    }
    return pickUniqueMatch(domain, matches);
}

// Reads `field`'s value off an already-fetched `Item` — matched by field
// `id` first (1Password's standard Login item field ids, "username" and
// "password", which is also the `op://` reference segment native's
// `field_wire_segment` uses), falling back to a case-insensitive title match
// for a non-standard item layout.
function fieldValueFromItem(item, field, domain) {
    const match = item.fields.find(
        (f) => f.id === field || (typeof f.title === "string" && f.title.toLowerCase() === field),
    );
    if (!match) {
        throw new Error(`item for domain "${domain}" has no "${field}" field — not typed`);
    }
    return match.value;
}

// Every path through `resolveUncached`'s try block funnels here, including
// this module's *own* rejections (`pickUniqueMatch`'s domain-mismatch/
// ambiguous errors, `fieldValueFromItem`'s no-such-field error) alongside
// actual `@1password/sdk` failures — so this function must tell the two
// apart. Our own errors are already fixed, safe, marker-bearing strings (never
// raw/untrusted text) and are recognized by those same markers and passed
// through unchanged, so `outcomeForError`/`crates/wasm/src/vault.rs`'s
// `map_vault_js_error` keep classifying them correctly. Known-typed SDK
// errors (`@1password/sdk`'s `errors.js`) get rewritten into a fixed,
// descriptive message carrying a marker substring `map_vault_js_error`
// greps for — mirrors native's `build_op_error`'s fixed-message convention
// (never forwarding raw SDK/CLI error text — even as a "Detail: ..."
// suffix — wholesale into a `PortError`, since that's untrusted backend text
// reaching the MCP client). Anything else — a bare, unrecognized `Error`
// (e.g. `createClient`'s own token-shape validation failure, or any SDK
// error shape this function doesn't yet know about) maps to a fixed,
// generic message rather than being rethrown verbatim.
function normalizeVaultError(e) {
    const message = e && e.message ? e.message : String(e);
    if (
        message.includes("no vault entry for domain") ||
        message.includes("ambiguous, not typed") ||
        /has no ".+" field/.test(message)
    ) {
        return e instanceof Error ? e : new Error(message);
    }
    if (e instanceof RateLimitExceededError || /rate limit/i.test(message)) {
        return new Error(
            "1Password rate limit exceeded — not typed. Retry after an unspecified delay.",
        );
    }
    if (
        e instanceof AuthExpiredError ||
        e instanceof DesktopSessionExpiredError ||
        /invalid service account token|not signed in|not authenticated/i.test(message)
    ) {
        return new Error(
            "1Password SDK reports not authenticated — not typed. This requires human action (verify OP_SERVICE_ACCOUNT_TOKEN); the agent cannot resolve this itself.",
        );
    }
    return new Error("vault lookup failed");
}

// Mirrors native's `log_resolve_outcome` (`crates/native/src/vault.rs`) —
// same line shape and stderr channel (stdout carries the MCP JSON-RPC
// stream), one line per resolve attempt, success and every rejection reason
// alike (`ux.md` §3: a rejection is exactly as audit-worthy as a success).
// Never includes the resolved value — `outcome` is a fixed classification
// string, never the raw secret.
function logResolveOutcome(domain, field, outcome) {
    console.error(`stapler-mcp: credential resolve domain='${domain}' field=${field} outcome=${outcome}`);
}
// Exported so `crates/wasm/src/browser.rs`'s pre-resolve domain-mismatch gate
// (`type_secret`'s `check_credential_domain` call, Story 4.3.3) can log its
// own early rejection through this same chokepoint (A1 code review fix) —
// otherwise that rejection would be silently missing from the audit trail,
// since `jsResolveCredential` (and its own `logResolveOutcome` call) is never
// reached on this path.
module.exports.jsLogResolveOutcome = logResolveOutcome;

// Classifies a (already `normalizeVaultError`-passed) error into native's
// log-line outcome vocabulary (`resolved`/`rejected-domain-mismatch`/
// `rejected-ambiguous`/`rate-limited`/`totp-expired`/`vault-lookup-failed`).
// Mirrors `crates/wasm/src/vault.rs`'s `map_vault_js_error` substring
// dispatch — same marker substrings, same precedence order (checking
// "not authenticated"/"not signed in" before "expired" matters: an
// unauthenticated error's `Detail: ...` suffix can itself contain the word
// "expired", e.g. from a `DesktopSessionExpiredError`) — so the log line
// never disagrees with the `PortError` variant `map_vault_js_error` actually
// produces for the same message. Unauthenticated and any other unrecognized
// failure both mean the vault lookup itself couldn't complete, same as
// native's `Err(_) => "vault-lookup-failed"` catch-all.
function outcomeForError(e) {
    const message = e && e.message ? e.message : String(e);
    const lower = message.toLowerCase();
    if (lower.includes("ambiguous")) {
        return "rejected-ambiguous";
    }
    if (lower.includes("no vault entry for domain")) {
        return "rejected-domain-mismatch";
    }
    if (lower.includes("not authenticated") || lower.includes("not signed in")) {
        return "vault-lookup-failed";
    }
    if (lower.includes("rate limit")) {
        return "rate-limited";
    }
    if (lower.includes("expired")) {
        return "totp-expired";
    }
    return "vault-lookup-failed";
}

// The actual (uncached-at-the-`pending`-level) resolve: client singleton,
// domain lookup, the field-conditional branch (Task 4.2.1b), error
// normalization, and the audit-log line (Task 5.4.1b), in that order.
async function resolveUncached(domain, field) {
    try {
        const client = await getVaultClient();
        const { vaultId, itemId } = await lookupDomain(client, domain);

        let value;
        if (field === "totp") {
            // The Node SDK's `items.get()` doesn't return computed OTP
            // values (`architecture.md` §6) — `secrets.resolve()` with the
            // `?attribute=otp` query is required instead, unlike the
            // username/password path below.
            value = await client.secrets.resolve(`op://${vaultId}/${itemId}/${field}?attribute=otp`);
        } else {
            const item = await client.items.get(vaultId, itemId);
            value = fieldValueFromItem(item, field, domain);
        }

        logResolveOutcome(domain, field, "resolved");
        return value;
    } catch (e) {
        const normalized = normalizeVaultError(e);
        logResolveOutcome(domain, field, outcomeForError(normalized));
        throw normalized;
    }
}

// `jsResolveCredential(domain, field)` — the one exported entry point
// `crates/wasm/src/vault.rs`'s `WasmCredentialStore::resolve` binds to.
// Deliberately not declared `async`: the synchronous prefix (key
// computation, `pending` lookup/insert) must run to completion *before* the
// first `await` inside `resolveUncached`, so two calls issued back-to-back
// for the same key are guaranteed to share one `Promise` regardless of how
// far either has actually progressed — exactly the guarantee Story 4.2.2's
// ACs assert.
function jsResolveCredential(domain, field) {
    const key = `${domain}::${field}`;
    const existing = pending.get(key);
    if (existing) {
        return existing;
    }
    const promise = resolveUncached(domain, field).finally(() => pending.delete(key));
    pending.set(key, promise);
    return promise;
}
module.exports.jsResolveCredential = jsResolveCredential;
