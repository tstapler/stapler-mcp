const fs = require("fs");
const path = require("path");

function isProcessAlive(pid) {
    try {
        process.kill(pid, 0);
        return true;
    } catch (e) {
        // EPERM means it exists but is owned by another user — still alive.
        return e.code === "EPERM";
    }
}

// ponytail: a directory-create is atomic (mkdir(2)) and needs no extra
// dependency — this is Node's own equivalent of a non-blocking exclusive
// flock. Unlike a real flock, a crashed owner leaves the directory behind, so
// staleness is checked via `process.kill(pid, 0)` (liveness, not a timeout)
// before reclaiming.
module.exports.jsAcquireLock = function (lockPath) {
    for (let attempt = 0; attempt < 2; attempt++) {
        try {
            fs.mkdirSync(lockPath, { recursive: false });
            fs.writeFileSync(path.join(lockPath, "pid"), String(process.pid));
            return true;
        } catch (e) {
            if (e.code !== "EEXIST") {
                throw e;
            }
            let ownerPid = null;
            try {
                ownerPid = parseInt(fs.readFileSync(path.join(lockPath, "pid"), "utf8"), 10);
            } catch (_) {
                // pid file missing/unreadable — treat as stale below.
            }
            if (ownerPid && isProcessAlive(ownerPid)) {
                return false;
            }
            try {
                fs.rmSync(lockPath, { recursive: true, force: true });
            } catch (_) {
                // Lost a race with another reclaimer; the retry will see it exists again.
            }
        }
    }
    return false;
};

module.exports.jsReleaseLock = function (lockPath) {
    try {
        fs.rmSync(lockPath, { recursive: true, force: true });
    } catch (_) {
        // Best-effort — matches the native adapter's PID-write, which also ignores errors.
    }
};
