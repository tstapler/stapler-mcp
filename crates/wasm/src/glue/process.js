const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

// `exeHint` here means "the CLI entry script to hand to `node`", not a
// standalone binary — there's no separate executable in the Node/WASM world,
// so re-exec always goes through `process.execPath` (the `node` binary)
// plus a script path, mirroring the native adapter's re-exec-self design one
// layer up.
module.exports.jsSpawnDaemon = function (exeHint, logPath) {
    const scriptPath = exeHint && exeHint.length > 0 ? exeHint : require.main.filename;
    fs.mkdirSync(path.dirname(logPath), { recursive: true });
    const logFd = fs.openSync(logPath, "a");
    try {
        const child = spawn(process.execPath, [scriptPath, "--daemon"], {
            detached: true,
            stdio: ["ignore", logFd, logFd],
            env: process.env,
        });
        child.unref();
    } finally {
        fs.closeSync(logFd);
    }
};
