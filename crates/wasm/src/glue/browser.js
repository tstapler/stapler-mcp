const { chromium } = require("playwright-core");

// One shared browser-process launch for the daemon's whole lifetime, lazily
// started on the first call — mirrors the native adapter's single
// `chromiumoxide::Browser` allocator. Uses the system-installed Chrome
// (`channel: "chrome"`) so nothing needs downloading.
let browserPromise = null;
function getBrowser() {
    if (!browserPromise) {
        browserPromise = chromium.launch({ headless: true, channel: "chrome" });
    }
    return browserPromise;
}

// Called once at daemon shutdown — without this the Chrome subprocess (and
// Node's handle to its CDP connection) keeps the event loop alive forever,
// even after the socket listener itself has been closed.
module.exports.jsCloseBrowser = async function () {
    // The reaper's `setInterval` otherwise keeps firing (and the `sessions`
    // map keeps referencing already-torn-down pages) after the browser
    // itself has been closed — nothing else ever clears either.
    if (reaperTimer) {
        clearInterval(reaperTimer);
        reaperTimer = null;
    }
    sessions.clear();
    if (browserPromise) {
        const browser = await browserPromise;
        await browser.close();
        browserPromise = null;
    }
};

module.exports.jsNavigateAndExtract = async function (url, timeoutMs) {
    const browser = await getBrowser();
    const page = await browser.newPage();
    try {
        await page.goto(url, { timeout: timeoutMs, waitUntil: "load" });
        const title = await page.title();
        const html = await page.content();
        const text = await page.evaluate(() => (document.body ? document.body.innerText : ""));
        const finalUrl = page.url();
        return { title, html, text, finalUrl };
    } finally {
        await page.close();
    }
};

// ---------------------------------------------------------------------------
// Persistent browser sessions (Epic 4): a session Map + idle reaper + a
// FrameNavigatedGuard, the wasm-side counterpart of the native adapter's
// `SessionRegistry`/`SessionIdleReaper`/blocked-host tracking. Exported (not
// just used internally) so a Node test harness can drive them directly with
// mock `page`/`browser` objects, without a real Chromium instance.

const sessions = new Map(); // sessionId -> { page, lastUsed, blocked }
module.exports.sessions = sessions;

let sessionCounter = 0;
function newSessionId() {
    return `sess-${Date.now().toString(16)}-${sessionCounter++}`;
}
module.exports.newSessionId = newSessionId;

const SESSION_IDLE_TIMEOUT_MS = 300_000;
module.exports.SESSION_IDLE_TIMEOUT_MS = SESSION_IDLE_TIMEOUT_MS;

// The reaper scan body itself, exported separately from the `setInterval`
// wiring so a Node test harness can invoke it directly rather than waiting on
// the real 30s timer.
function reapIdleSessions() {
    for (const [id, s] of sessions) {
        // A session with an in-flight (or queued) call must not be closed
        // out from under it — mirrors the native adapter's per-session-lock
        // fix for the same TOCTOU. `runSerialized` (below) increments
        // `s.busy` for the duration of every call against this session, so
        // skipping here just defers the reap until the next 30s scan, by
        // which point `lastUsed` will have been bumped past the idle window
        // anyway if the session is still being used.
        if (s.busy) {
            continue;
        }
        if (Date.now() - s.lastUsed > SESSION_IDLE_TIMEOUT_MS) {
            // Best-effort close — a rejected close (e.g. the tab already
            // crashed/closed itself) must not become an unhandled rejection.
            s.page.close().catch(() => {});
            sessions.delete(id);
        }
    }
}
module.exports.reapIdleSessions = reapIdleSessions;

// Hard cap on concurrently open sessions/tabs, mirroring the native
// adapter's equivalent limit — without one, a caller can spawn unbounded
// pages/tabs before the 300s idle reaper ever fires (MAJOR/DoS finding).
const MAX_SESSIONS = 50;
module.exports.MAX_SESSIONS = MAX_SESSIONS;

// Per-session in-flight-call serialization (MAJOR/concurrency finding):
// without this, `reapIdleSessions` can `page.close()` a session whose own
// call is still running, and two concurrent calls against the same
// `sessionId` can interleave unsynchronized awaits against the same
// Playwright `Page`. `session.queue` is a promise chain — each call appends
// itself and only starts once every previously-queued call on this session
// has settled; `session.busy` is a simple in-flight counter the reaper
// checks above.
function runSerialized(session, fn) {
    session.busy = (session.busy || 0) + 1;
    const prior = session.queue || Promise.resolve();
    const result = prior.then(fn, fn);
    // Swallow here so a failed call doesn't poison the chain for the next
    // queued caller — the real rejection still propagates via `result`.
    session.queue = result.then(
        () => {},
        () => {},
    );
    return result.finally(() => {
        session.busy -= 1;
    });
}

function sleep(ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
}

// SSRF grace-period poll (BLOCKER/CRITICAL findings): `session.blocked` is
// set asynchronously by the `framenavigated` listener wired in
// `wireFrameNavigatedGuard`, so checking it immediately after an awaited
// navigate/click/fill can race the listener and miss a same-call
// navigation to a blocked host (e.g. the metadata service). Mirrors the
// native adapter's `dispatch_action` poll added in commit de6ce2e: up to
// 200ms total, in 20ms steps, returning as soon as `blocked` is observed.
async function waitForBlockedGracePeriod(session) {
    let waited = 0;
    const step = 20;
    while (!session.blocked && waited < 200) {
        await sleep(step);
        waited += step;
    }
}
module.exports.waitForBlockedGracePeriod = waitForBlockedGracePeriod;

// Guarded module-level singleton, matching the existing lazy `browserPromise`
// pattern — the interval is only ever created once per process.
let reaperTimer = null;
function ensureReaper() {
    if (reaperTimer) {
        return;
    }
    reaperTimer = setInterval(reapIdleSessions, 30_000);
    // Don't let the reaper's timer keep the Node process alive on its own.
    if (typeof reaperTimer.unref === "function") {
        reaperTimer.unref();
    }
}

// Mirrors `crates/core/src/tools/webcrawl.rs`'s `blocked_host_reason`
// private/loopback/link-local logic — JS glue can't call that Rust `pub fn`
// directly, so the check is duplicated here. Best-effort literal check only
// (no DNS resolution), same limitation as the native version.
function isBlockedIpv4(a, b) {
    if (a === 127) return true; // loopback
    if (a === 10) return true; // private
    if (a === 172 && b >= 16 && b <= 31) return true; // private
    if (a === 192 && b === 168) return true; // private
    if (a === 169 && b === 254) return true; // link-local
    if (a === 0) return true; // unspecified / "this network"
    return false;
}

function isBlockedHost(hostname) {
    if (process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS === "1") {
        return false;
    }
    // Strip brackets/zone id so "[::1]" and "fe80::1%eth0" match the same
    // way as their unbracketed forms below.
    const host = hostname.toLowerCase().replace(/^\[|\]$/g, "").split("%")[0];
    if (host === "localhost" || host.endsWith(".localhost")) {
        return true;
    }
    const v4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
    if (v4) {
        return isBlockedIpv4(Number(v4[1]), Number(v4[2]));
    }
    if (host === "::1" || host === "::") {
        return true; // loopback / unspecified
    }
    if (host.startsWith("fe80:")) {
        return true; // link-local
    }
    if (host.startsWith("fc") || host.startsWith("fd")) {
        return true; // unique-local, fc00::/7
    }
    // IPv4-mapped IPv6 — mirrors native's `Ipv6Addr::to_ipv4_mapped()`
    // unwrapping in `blocked_host_reason`. Node's URL parser always
    // normalizes these to compressed hex-hextet form (e.g. "::ffff:a9fe:a9fe"
    // for 169.254.169.254), never dotted-quad, so match that shape.
    const v4MappedHex = host.match(/^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/);
    if (v4MappedHex) {
        const hi = Number.parseInt(v4MappedHex[1], 16);
        return isBlockedIpv4((hi >> 8) & 0xff, hi & 0xff);
    }
    // Dotted-quad form (::ffff:a.b.c.d or ::a.b.c.d) for callers that
    // construct the hostname string directly rather than via `URL`.
    const v4MappedDotted = host.match(/^::(?:ffff:)?(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
    if (v4MappedDotted) {
        return isBlockedIpv4(Number(v4MappedDotted[1]), Number(v4MappedDotted[2]));
    }
    return false;
}
module.exports.isBlockedHost = isBlockedHost;

// The exact Error-5 "recoverable block" wording a caller sees regardless of
// which daemon (native or wasm) it's talking to — see `design/ux.md` §Error 5
// and native's Task 3.4.2.
function blockedHostMessage(sessionId, host) {
    return `session '${sessionId}' navigated to a blocked host '${host}' during the last action; call stapler_browser_navigate with this sessionId and a safe URL to recover it, or start a fresh session`;
}
module.exports.blockedHostMessage = blockedHostMessage;

// Wires `page.on('framenavigated', ...)` for a session's page, the wasm-side
// `FrameNavigatedGuard` counterpart to native's Task 3.4.2: any subsequent
// call against a blocked session fails until the caller re-navigates it to a
// safe URL (see `jsBrowserNavigate`'s session-reuse path below, which clears
// `blocked` before issuing `page.goto`).
function wireFrameNavigatedGuard(sessionId, session) {
    session.page.on("framenavigated", (frame) => {
        // Only top-level (main-frame) navigations are in scope for the SSRF
        // guard — an iframe embedding a private-looking URL isn't the
        // session itself navigating.
        if (typeof frame.parentFrame === "function" && frame.parentFrame()) {
            return;
        }
        let host;
        try {
            host = new URL(frame.url()).hostname;
        } catch {
            return;
        }
        if (isBlockedHost(host)) {
            session.blocked = blockedHostMessage(sessionId, host);
        }
    });
}
module.exports.wireFrameNavigatedGuard = wireFrameNavigatedGuard;

// Mirrors native's `crashed_message` (see `crates/native/src/browser.rs`):
// same wording regardless of which adapter is behind the call, and the
// substring `"crashed"` is load-bearing — `WasmBrowser::map_js_error` greps
// for it to produce `PortError::SessionCrashed` rather than a generic error.
function crashedMessage(sessionId) {
    return `browser session "${sessionId}" crashed — call stapler_browser_navigate (without sessionId) to start a fresh session`;
}
module.exports.crashedMessage = crashedMessage;

// Wires `page.on('crash', ...)` for a session's page — the wasm-side
// counterpart of native's `Target.targetCrashed` listener
// (`spawn_session_listeners` in `crates/native/src/browser.rs`). Playwright
// fires its own `'crash'` event on the page when the underlying renderer
// crashes; once that happens the page is unusable, so every subsequent call
// against this session must fail instead of hanging or throwing an opaque
// Playwright error.
function wireCrashListener(sessionId, session) {
    session.page.on("crash", () => {
        session.crashed = crashedMessage(sessionId);
    });
}
module.exports.wireCrashListener = wireCrashListener;

function requireSession(sessionId) {
    const session = sessions.get(sessionId);
    if (!session) {
        throw new Error(`no session '${sessionId}' found; call stapler_browser_navigate to start one`);
    }
    return session;
}

function checkBlocked(session) {
    if (session.blocked) {
        throw new Error(session.blocked);
    }
}

// Shared crash-check-then-evict step, mirroring native's `touch_or_evict`
// (`crates/native/src/browser.rs`): on crash, the session is deleted from
// `sessions` immediately rather than waiting for the idle reaper to notice
// it. Deliberately does *not* touch `blocked` or `lastUsed` — callers differ
// on both (see `requireLiveSession` vs. `jsBrowserNavigate`'s session-reuse
// branch below), so those stay the caller's responsibility.
function evictIfCrashed(session, sessionId) {
    if (session.crashed) {
        sessions.delete(sessionId);
        throw new Error(session.crashed);
    }
}

// Shared entry-check for `jsBrowserClick`/`jsBrowserType`/`jsBrowserSnapshot`:
// crashed/blocked must be checked *before* `lastUsed` is bumped — bumping
// first would keep a crashed session's idle clock refreshed on every failed
// call, so it would never age past `SESSION_IDLE_TIMEOUT_MS` and never get
// reaped, defeating the crash-detection fix's purpose.
function requireLiveSession(sessionId) {
    const session = requireSession(sessionId);
    evictIfCrashed(session, sessionId);
    checkBlocked(session);
    session.lastUsed = Date.now();
    return session;
}

// Parses Playwright's own ref-annotated `page.ariaSnapshot()` output (e.g.
// `- button "Submit" [ref=e1]`, nested children indented by 2 spaces) into
// the plain-object node shape `AxSnapshot.root`/`AxNode` (`{ role, name, ref,
// children, value? }`) that Task 4.3.1's Rust side deserializes — the
// adapter-agnostic shape both native and wasm produce.
function parseAriaSnapshot(text) {
    const lines = text.split("\n").filter((line) => line.trim().length > 0);
    const lineRe = /^(\s*)- ([A-Za-z][A-Za-z0-9_-]*)(?:\s+"((?:[^"\\]|\\.)*)")?(?:\s*\[ref=([^\]]+)\])?/;

    const syntheticRoot = { role: "generic", name: "", ref: "root", children: [] };
    const stack = [{ indent: -1, node: syntheticRoot }];

    for (const line of lines) {
        const m = line.match(lineRe);
        if (!m) {
            continue;
        }
        const indent = m[1].length;
        const node = {
            role: m[2],
            name: m[3] ?? "",
            ref: m[4] ?? "",
            children: [],
        };
        while (stack.length > 1 && stack[stack.length - 1].indent >= indent) {
            stack.pop();
        }
        stack[stack.length - 1].node.children.push(node);
        stack.push({ indent, node });
    }

    // A single top-level node becomes the snapshot root directly rather than
    // being wrapped in a synthetic one; multiple top-level nodes are wrapped.
    if (syntheticRoot.children.length === 1) {
        return syntheticRoot.children[0];
    }
    return syntheticRoot;
}
module.exports.parseAriaSnapshot = parseAriaSnapshot;

// Mirrors native's `MAX_SNAPSHOT_NODES` (`crates/native/src/ax.rs`) —
// without a cap, a caller can trigger an unbounded-size snapshot walk
// (MAJOR/DoS finding) and there was never a way for a wasm-adapter caller
// to detect truncation (`truncated` was hardcoded `false`).
const MAX_SNAPSHOT_NODES = 500;
module.exports.MAX_SNAPSHOT_NODES = MAX_SNAPSHOT_NODES;

// Walks the tree `parseAriaSnapshot` already built and prunes it down to at
// most `maxNodes` total nodes (root inclusive), in the same pre-order the
// tree was constructed in. Mutates `children` arrays in place. Returns
// whether any node was dropped, so callers can set `truncated` accurately
// instead of hardcoding it.
function capSnapshotNodes(root, maxNodes) {
    let count = 1; // the root itself counts against the cap
    let truncated = false;
    function visit(node) {
        const kept = [];
        for (const child of node.children) {
            if (count >= maxNodes) {
                truncated = true;
                break;
            }
            count += 1;
            kept.push(child);
            visit(child);
        }
        node.children = kept;
    }
    visit(root);
    return truncated;
}
module.exports.capSnapshotNodes = capSnapshotNodes;

async function captureSnapshot(page) {
    const text = await page.ariaSnapshot();
    const root = parseAriaSnapshot(text);
    const truncated = capSnapshotNodes(root, MAX_SNAPSHOT_NODES);
    return { root, url: page.url(), truncated };
}

// Distinguishes a Playwright error that actually indicates the resolved
// `ref` is missing/detached/never-became-actionable from any other error
// (e.g. "execution context was destroyed" from a same-click navigation, or
// an unrelated dispatch failure) — MAJOR/correctness finding. Only the
// former should be rewritten into the "ref not found" wording; rewriting
// unconditionally causes `WasmBrowser::map_js_error`
// (`crates/wasm/src/browser.rs`) to misclassify a real
// navigation-in-progress as `PortError::NotFound` and send callers into an
// incorrect retry loop.
const MISSING_REF_ERROR_RE =
    /not attached to the dom|element is not attached|failed to find element|no node found for selector|waiting for (?:locator|selector)|resolved to hidden|strict mode violation|timeout \d+ms exceeded/i;

function isMissingRefError(message) {
    return MISSING_REF_ERROR_RE.test(message);
}
module.exports.isMissingRefError = isMissingRefError;

// Shared catch-block behavior for `jsBrowserClick`/`jsBrowserType`: rewrite
// only genuine "ref missing" errors into the actionable wording; let
// anything else (e.g. a same-action navigation tearing down the execution
// context) pass through unchanged so it isn't misclassified downstream.
function describeActionError(refId, e) {
    if (isMissingRefError(e.message)) {
        return new Error(`ref '${refId}' not found or no longer attached: ${e.message}`);
    }
    return e;
}
module.exports.describeActionError = describeActionError;

module.exports.jsBrowserNavigate = async function (url, sessionId, timeoutMs) {
    let id = sessionId;
    let session;
    if (id) {
        session = requireSession(id);
        // Unlike a blocked session, a crashed one is not recoverable via
        // re-navigating the same session id — the underlying renderer is
        // gone, so the caller must start a fresh session instead. Evict it
        // immediately (mirroring native's `touch_or_evict`) rather than
        // leaving it in `sessions` for up to `SESSION_IDLE_TIMEOUT_MS` for
        // the idle reaper to eventually notice.
        evictIfCrashed(session, id);
        // A session that once bounced through a blocked host is recoverable
        // via a later legitimate re-navigate rather than permanently stuck
        // (Task 4.2.2's clear-on-re-navigate fix).
        session.blocked = undefined;
    } else {
        if (sessions.size >= MAX_SESSIONS) {
            throw new Error(
                `too many open browser sessions (limit ${MAX_SESSIONS}); close an existing session or wait for the idle reaper`,
            );
        }
        const browser = await getBrowser();
        const page = await browser.newPage();
        id = newSessionId();
        session = { page, lastUsed: Date.now(), blocked: undefined, crashed: undefined };
        sessions.set(id, session);
        wireFrameNavigatedGuard(id, session);
        wireCrashListener(id, session);
        ensureReaper();
    }
    session.lastUsed = Date.now();
    return runSerialized(session, async () => {
        await session.page.goto(url, { timeout: timeoutMs, waitUntil: "load" });
        // BLOCKER fix: navigate() itself must not skip the same SSRF check
        // click/type/snapshot already perform — a redirect (server- or
        // in-page) during this same `goto` to a blocked host (e.g. the
        // cloud metadata service) would otherwise leak a full snapshot
        // before the block is ever surfaced. `session.blocked` is set
        // asynchronously by the `framenavigated` listener, hence the grace
        // poll before checking it.
        await waitForBlockedGracePeriod(session);
        checkBlocked(session);
        const snapshot = await captureSnapshot(session.page);
        return { sessionId: id, finalUrl: session.page.url(), snapshot };
    });
};

module.exports.jsBrowserClick = async function (sessionId, refId, timeoutMs) {
    const session = requireLiveSession(sessionId);
    return runSerialized(session, async () => {
        const urlBefore = session.page.url();
        const locator = session.page.locator(`aria-ref=${refId}`);
        try {
            await locator.click({ timeout: timeoutMs });
        } catch (e) {
            throw describeActionError(refId, e);
        }
        // CRITICAL/SSRF-race fix: give the async `framenavigated` listener a
        // grace period to observe a click-triggered navigation to a blocked
        // host before trusting `session.blocked`.
        await waitForBlockedGracePeriod(session);
        checkBlocked(session);
        const snapshot = await captureSnapshot(session.page);
        const urlAfter = session.page.url();
        if (urlAfter !== urlBefore) {
            snapshot.navigatedFrom = urlBefore;
        }
        return snapshot;
    });
};

module.exports.jsBrowserType = async function (sessionId, refId, text, timeoutMs) {
    const session = requireLiveSession(sessionId);
    return runSerialized(session, async () => {
        const urlBefore = session.page.url();
        const locator = session.page.locator(`aria-ref=${refId}`);
        try {
            await locator.fill(text, { timeout: timeoutMs });
        } catch (e) {
            throw describeActionError(refId, e);
        }
        // Same SSRF-race fix as `jsBrowserClick` above.
        await waitForBlockedGracePeriod(session);
        checkBlocked(session);
        const snapshot = await captureSnapshot(session.page);
        const urlAfter = session.page.url();
        if (urlAfter !== urlBefore) {
            snapshot.navigatedFrom = urlBefore;
        }
        return snapshot;
    });
};

module.exports.jsBrowserSnapshot = async function (sessionId, timeoutMs) {
    const session = requireLiveSession(sessionId);
    void timeoutMs; // snapshot itself has nothing to time out on; kept for a uniform signature
    return runSerialized(session, () => captureSnapshot(session.page));
};
