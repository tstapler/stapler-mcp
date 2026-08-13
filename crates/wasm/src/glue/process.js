const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

const NATIVE_BINARY_NAMES = process.platform === "win32"
    ? ["stapler-mcp.exe", "stapler-mcp.cmd"]
    : ["stapler-mcp"];

// Both adapters speak the identical wire protocol and are interoperable on
// the same machine (see README's "Two distributions, one core"), so if the
// user has also `cargo install`ed the native binary, prefer spawning that as
// the daemon over `node`+wasm — full native performance (real reqwest,
// chromiumoxide) for the same shared daemon every thin client talks to.
module.exports.findNativeBinary = function () {
    const pathEnv = process.env.PATH || "";
    for (const dir of pathEnv.split(path.delimiter)) {
        if (!dir) continue;
        for (const name of NATIVE_BINARY_NAMES) {
            const candidate = path.join(dir, name);
            try {
                fs.accessSync(candidate, fs.constants.X_OK);
                return candidate;
            } catch {
                // not here, keep looking
            }
        }
    }
    return null;
};

// `exeHint` here means "the CLI entry script to hand to `node`", not a
// standalone binary — there's no separate executable in the Node/WASM world
// unless a native binary is found on PATH (see `findNativeBinary` above), so
// the fallback re-exec goes through `process.execPath` (the `node` binary)
// plus a script path, mirroring the native adapter's re-exec-self design one
// layer up.
module.exports.jsSpawnDaemon = function (exeHint, logPath) {
    fs.mkdirSync(path.dirname(logPath), { recursive: true });
    const logFd = fs.openSync(logPath, "a");
    try {
        const nativeBinary = module.exports.findNativeBinary();
        const scriptPath = exeHint && exeHint.length > 0 ? exeHint : require.main.filename;
        const [command, args] = nativeBinary
            ? [nativeBinary, ["--daemon"]]
            : [process.execPath, [scriptPath, "--daemon"]];
        const child = spawn(command, args, {
            detached: true,
            stdio: ["ignore", logFd, logFd],
            env: process.env,
        });
        child.unref();
    } finally {
        fs.closeSync(logFd);
    }
};
