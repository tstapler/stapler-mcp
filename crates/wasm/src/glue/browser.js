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
        if (Date.now() - s.lastUsed > SESSION_IDLE_TIMEOUT_MS) {
            // Best-effort close — a rejected close (e.g. the tab already
            // crashed/closed itself) must not become an unhandled rejection.
            s.page.close().catch(() => {});
            sessions.delete(id);
        }
    }
}
module.exports.reapIdleSessions = reapIdleSessions;

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
function isBlockedHost(hostname) {
    if (process.env.STAPLER_MCP_ALLOW_PRIVATE_NETWORKS === "1") {
        return false;
    }
    const host = hostname.toLowerCase();
    if (host === "localhost" || host.endsWith(".localhost")) {
        return true;
    }
    const v4 = host.match(/^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/);
    if (v4) {
        const a = Number(v4[1]);
        const b = Number(v4[2]);
        if (a === 127) return true; // loopback
        if (a === 10) return true; // private
        if (a === 172 && b >= 16 && b <= 31) return true; // private
        if (a === 192 && b === 168) return true; // private
        if (a === 169 && b === 254) return true; // link-local
        if (a === 0) return true; // unspecified
        return false;
    }
    if (host === "::1" || host === "[::1]" || host === "::" || host === "[::]") {
        return true; // loopback / unspecified
    }
    if (host.startsWith("fe80:") || host.startsWith("[fe80:")) {
        return true; // link-local
    }
    if (host.startsWith("fc") || host.startsWith("fd") || host.startsWith("[fc") || host.startsWith("[fd")) {
        return true; // unique-local
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

function checkCrashed(session) {
    if (session.crashed) {
        throw new Error(session.crashed);
    }
}

function checkBlocked(session) {
    if (session.blocked) {
        throw new Error(session.blocked);
    }
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

async function captureSnapshot(page) {
    const text = await page.ariaSnapshot();
    const root = parseAriaSnapshot(text);
    return { root, url: page.url(), truncated: false };
}

module.exports.jsBrowserNavigate = async function (url, sessionId, timeoutMs) {
    let id = sessionId;
    let session;
    if (id) {
        session = requireSession(id);
        // Unlike a blocked session, a crashed one is not recoverable via
        // re-navigating the same session id — the underlying renderer is
        // gone, so the caller must start a fresh session instead.
        checkCrashed(session);
        // A session that once bounced through a blocked host is recoverable
        // via a later legitimate re-navigate rather than permanently stuck
        // (Task 4.2.2's clear-on-re-navigate fix).
        session.blocked = undefined;
    } else {
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
    await session.page.goto(url, { timeout: timeoutMs, waitUntil: "load" });
    const snapshot = await captureSnapshot(session.page);
    return { sessionId: id, finalUrl: session.page.url(), snapshot };
};

module.exports.jsBrowserClick = async function (sessionId, refId, timeoutMs) {
    const session = requireSession(sessionId);
    session.lastUsed = Date.now();
    checkCrashed(session);
    checkBlocked(session);
    const urlBefore = session.page.url();
    const locator = session.page.locator(`aria-ref=${refId}`);
    try {
        await locator.click({ timeout: timeoutMs });
    } catch (e) {
        throw new Error(`ref '${refId}' not found or no longer attached: ${e.message}`);
    }
    checkBlocked(session);
    const snapshot = await captureSnapshot(session.page);
    const urlAfter = session.page.url();
    if (urlAfter !== urlBefore) {
        snapshot.navigatedFrom = urlBefore;
    }
    return snapshot;
};

module.exports.jsBrowserType = async function (sessionId, refId, text, timeoutMs) {
    const session = requireSession(sessionId);
    session.lastUsed = Date.now();
    checkCrashed(session);
    checkBlocked(session);
    const urlBefore = session.page.url();
    const locator = session.page.locator(`aria-ref=${refId}`);
    try {
        await locator.fill(text, { timeout: timeoutMs });
    } catch (e) {
        throw new Error(`ref '${refId}' not found or no longer attached: ${e.message}`);
    }
    checkBlocked(session);
    const snapshot = await captureSnapshot(session.page);
    const urlAfter = session.page.url();
    if (urlAfter !== urlBefore) {
        snapshot.navigatedFrom = urlBefore;
    }
    return snapshot;
};

module.exports.jsBrowserSnapshot = async function (sessionId, timeoutMs) {
    const session = requireSession(sessionId);
    session.lastUsed = Date.now();
    checkCrashed(session);
    checkBlocked(session);
    void timeoutMs; // snapshot itself has nothing to time out on; kept for a uniform signature
    return captureSnapshot(session.page);
};
