// Node-harness unit test for `crates/wasm/src/glue/process.js` (issue #8's
// "opportunistic native-binary fast path"): `findNativeBinary` should locate
// a `cargo install`ed native `stapler-mcp` binary on PATH, if present, so
// `jsSpawnDaemon` can prefer it over spawning the Node/wasm daemon — both
// adapters are interoperable on the same machine (see README's "Two
// distributions, one core").

const test = require("node:test");
const assert = require("node:assert");
const path = require("node:path");
const fs = require("node:fs");
const os = require("node:os");

const processGlue = require(path.join(__dirname, "..", "..", "crates", "wasm", "src", "glue", "process.js"));

function withTempPathDir(setup, fn) {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), "stapler-mcp-native-bin-"));
    const originalPath = process.env.PATH;
    try {
        setup(dir);
        process.env.PATH = [dir, originalPath].join(path.delimiter);
        return fn(dir);
    } finally {
        process.env.PATH = originalPath;
        fs.rmSync(dir, { recursive: true, force: true });
    }
}

test("findNativeBinary_should_return_null_when_no_native_binary_on_path", () => {
    withTempPathDir(
        () => {},
        () => {
            assert.strictEqual(processGlue.findNativeBinary(), null);
        },
    );
});

test("findNativeBinary_should_return_path_when_executable_stapler_mcp_on_path", () => {
    withTempPathDir(
        (dir) => {
            const binPath = path.join(dir, "stapler-mcp");
            fs.writeFileSync(binPath, "#!/bin/sh\nexit 0\n", { mode: 0o755 });
        },
        (dir) => {
            assert.strictEqual(processGlue.findNativeBinary(), path.join(dir, "stapler-mcp"));
        },
    );
});

test("findNativeBinary_should_return_null_when_stapler_mcp_file_on_path_but_not_executable", () => {
    withTempPathDir(
        (dir) => {
            fs.writeFileSync(path.join(dir, "stapler-mcp"), "not a real binary", { mode: 0o644 });
        },
        () => {
            assert.strictEqual(processGlue.findNativeBinary(), null);
        },
    );
});
