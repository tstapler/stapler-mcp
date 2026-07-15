// The one real check for the wasm/Node daemon architecture (mirrors the Rust
// integration test): drives the actual wasm-pack-built package end-to-end
// over a real Unix socket in a fully isolated state dir, including the two
// real tools. A single test function, deliberately — env vars here
// (STAPLER_MCP_HOME, BRAVE_API_KEY, BRAVE_API_BASE_URL) are process-global,
// and node:test can run files in parallel across separate processes but
// tests *within* a file share this process, so splitting this into multiple
// env-mutating tests would race.

const test = require("node:test");
const assert = require("node:assert");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const http = require("node:http");

const wasm = require("../pkg/stapler_mcp_wasm.js");

// The real CLI entry point — same one a real npx install would run — used
// both as the thing under test and as the `--daemon` re-exec target,
// mirroring how the Rust test spawns the actual compiled binary rather than
// a test-only stub.
const ENTRY = path.join(__dirname, "..", "bin", "stapler-mcp.js");

function startMockBraveServer(body) {
    return new Promise((resolve) => {
        const server = http.createServer((_req, res) => {
            res.writeHead(200, { "Content-Type": "application/json" });
            res.end(body);
        });
        server.listen(0, "127.0.0.1", () => {
            const { port } = server.address();
            resolve({ server, url: `http://127.0.0.1:${port}/res/v1/web/search` });
        });
    });
}

const FILLER =
    "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor " +
    "incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation " +
    "ullamco laboris nisi ut aliquip ex ea commodo consequat. Duis aute irure dolor in reprehenderit in " +
    "voluptate velit esse cillum dolore eu fugiat nulla pariatur.";

// Small synthetic multi-page site + robots.txt, mirroring the Rust
// `webcrawl.rs` integration test — same routes, same assertions, proving the
// wasm/Node adapter's crawl/robots/cache behavior matches the native one.
function startMockSite() {
    const routes = {
        "/robots.txt": "User-agent: *\nDisallow: /private\n",
        "/": `<html><head><title>Index</title></head><body><p>${FILLER}</p><a href="/page2">Page 2</a> <a href="/private">Private</a></body></html>`,
        "/page2": `<html><head><title>Page Two</title></head><body><p>${FILLER}</p><a href="/page3">Page 3</a></body></html>`,
        "/page3": `<html><head><title>Page Three</title></head><body><p>${FILLER}</p></body></html>`,
        "/private": `<html><head><title>Private</title></head><body><p>${FILLER}</p></body></html>`,
    };
    return new Promise((resolve) => {
        const server = http.createServer((req, res) => {
            const body = routes[req.url];
            if (body === undefined) {
                res.writeHead(404);
                res.end();
                return;
            }
            res.writeHead(200, { "Content-Type": "text/html" });
            res.end(body);
        });
        server.listen(0, "127.0.0.1", () => {
            const { port } = server.address();
            resolve({ server, url: `http://127.0.0.1:${port}` });
        });
    });
}

test("daemon architecture and real tools round trip", async () => {
    const home = fs.mkdtempSync(path.join(os.tmpdir(), "smcp-e2e-"));
    process.env.STAPLER_MCP_HOME = home;
    process.env.BRAVE_API_KEY = "test-key";

    const mock = await startMockBraveServer(
        JSON.stringify({
            web: {
                results: [
                    {
                        title: "Rust Programming Language",
                        url: "https://www.rust-lang.org/",
                        description: "A language empowering everyone.",
                    },
                ],
            },
        }),
    );
    process.env.BRAVE_API_BASE_URL = mock.url;

    try {
        // 1. ensure_daemon auto-spawns and a real `ping` round-trips.
        const pingResult = JSON.parse(await wasm.ensure_daemon_and_call("ping", "", ENTRY));
        assert.strictEqual(pingResult.pong, true);

        // 2. A second call against the same STAPLER_MCP_HOME reuses the
        //    already-running daemon — checked via the PID file staying put.
        const lockPidPath = path.join(home, "daemon.lock", "pid");
        const pidBefore = fs.readFileSync(lockPidPath, "utf8");
        await wasm.ensure_daemon_and_call("ping", "", ENTRY);
        const pidAfter = fs.readFileSync(lockPidPath, "utf8");
        assert.strictEqual(pidBefore, pidAfter, "no second daemon should have been spawned");

        // 3. `brave_web_search` against the mock server.
        const searchResult = JSON.parse(
            await wasm.ensure_daemon_and_call(
                "brave_web_search",
                JSON.stringify({ query: "rust programming language" }),
                ENTRY,
            ),
        );
        assert.strictEqual(searchResult.results[0].title, "Rust Programming Language");

        // 4. `fetch_page` against a real URL, via playwright-core + system Chrome.
        const fetchResult = JSON.parse(
            await wasm.ensure_daemon_and_call(
                "fetch_page",
                JSON.stringify({ url: "https://example.com" }),
                ENTRY,
            ),
        );
        assert.strictEqual(fetchResult.title, "Example Domain");
        assert.strictEqual(fetchResult.finalUrl, "https://example.com/");

        // 5. `read_website` — BFS depth-2 crawl discovers index -> page2 ->
        //    page3, never /private (robots.txt-disallowed); then a second
        //    call after the site is shut down still succeeds from cache,
        //    returning only the seed page (cache hit skips link expansion).
        const site = await startMockSite();
        try {
            const crawlResult = JSON.parse(
                await wasm.ensure_daemon_and_call(
                    "read_website",
                    JSON.stringify({ url: site.url, maxDepth: 2, maxPages: 10 }),
                    ENTRY,
                ),
            );
            const titles = crawlResult.pages.map((p) => p.title);
            assert.strictEqual(crawlResult.pages.length, 3, `expected 3 pages, got: ${JSON.stringify(titles)}`);
            assert.ok(titles.includes("Index"));
            assert.ok(titles.includes("Page Two"));
            assert.ok(titles.includes("Page Three"));
            assert.ok(!titles.includes("Private"), "robots.txt should have disallowed /private");

            // 6. `download_website` — same crawl, raw HTML to disk.
            const saveDir = fs.mkdtempSync(path.join(os.tmpdir(), "smcp-e2e-download-"));
            const downloadResult = JSON.parse(
                await wasm.ensure_daemon_and_call(
                    "download_website",
                    JSON.stringify({ url: site.url, saveDir, maxDepth: 2, maxPages: 10 }),
                    ENTRY,
                ),
            );
            assert.strictEqual(downloadResult.pages.length, 3);
            for (const page of downloadResult.pages) {
                const contents = fs.readFileSync(page.path, "utf8");
                assert.ok(contents.includes("<html>"), `saved file should contain raw HTML: ${page.path}`);
            }

            site.server.close();
            await new Promise((resolve) => setTimeout(resolve, 100));

            const cachedResult = JSON.parse(
                await wasm.ensure_daemon_and_call(
                    "read_website",
                    JSON.stringify({ url: site.url, maxDepth: 2, maxPages: 10 }),
                    ENTRY,
                ),
            );
            assert.strictEqual(
                cachedResult.pages.length,
                1,
                `cache hit should skip link expansion, got: ${JSON.stringify(cachedResult.pages)}`,
            );
            assert.strictEqual(cachedResult.pages[0].title, "Index");
        } finally {
            site.server.close();
        }

        // 7. `shutdown` cleanly stops the daemon (and its browser — see
        //    `jsCloseBrowser` — otherwise the process never exits). Checked
        //    below via the socket/lock files, not another `ensure_daemon_and_call`
        //    — that would just respawn a fresh daemon, which is the whole
        //    point of `ensure_daemon` and would make the check meaningless.
        await wasm.ensure_daemon_and_call("shutdown", "", ENTRY);
        await new Promise((resolve) => setTimeout(resolve, 300));
    } finally {
        mock.server.close();
    }

    assert.ok(!fs.existsSync(path.join(home, "daemon.sock")), "socket should be removed after shutdown");
    assert.ok(!fs.existsSync(path.join(home, "daemon.lock")), "lock dir should be removed after shutdown");
});
