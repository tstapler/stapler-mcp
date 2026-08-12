// Node-harness unit test for `crates/wasm/src/glue/http.js` (issue #9):
// `jsHttpGet` must surface the real post-redirect URL via `resp.url` (Node's
// `fetch` follows redirects itself and exposes the final URL there), mirroring
// native's `should_populate_final_url_from_response_url_when_native_http_get_follows_redirect`
// in `crates/native/src/http.rs`.

const test = require("node:test");
const assert = require("node:assert");
const path = require("node:path");
const http = require("node:http");

const httpGlue = require(path.join(__dirname, "..", "..", "crates", "wasm", "src", "glue", "http.js"));

function startRedirectingMockServer() {
    return new Promise((resolve) => {
        const server = http.createServer((req, res) => {
            if (req.url === "/old-page") {
                res.writeHead(301, { Location: "/new-page" });
                res.end();
                return;
            }
            if (req.url === "/new-page") {
                res.writeHead(200, { "Content-Type": "text/plain" });
                res.end("new page body");
                return;
            }
            res.writeHead(404);
            res.end();
        });
        server.listen(0, "127.0.0.1", () => {
            const { port } = server.address();
            resolve({ server, baseUrl: `http://127.0.0.1:${port}` });
        });
    });
}

test("jsHttpGet_should_return_post_redirect_url_when_server_redirects", async () => {
    const { server, baseUrl } = await startRedirectingMockServer();
    try {
        const result = await httpGlue.jsHttpGet(`${baseUrl}/old-page`, "{}");

        assert.strictEqual(result.status, 200);
        assert.strictEqual(result.url, `${baseUrl}/new-page`);
        assert.strictEqual(Buffer.from(result.body).toString("utf8"), "new page body");
    } finally {
        server.close();
    }
});
