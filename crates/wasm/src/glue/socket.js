const net = require("net");
const fs = require("fs");

let nextId = 1;
const listeners = new Map(); // id -> { server, queue: number[], waiters: fn[] }
const conns = new Map(); // id -> { socket, buf: string, queue: string[], waiters: fn[], ended: bool }

function registerConn(socket) {
    const id = nextId++;
    const state = { socket, buf: "", queue: [], waiters: [], ended: false };
    conns.set(id, state);

    socket.on("data", (chunk) => {
        state.buf += chunk.toString("utf8");
        let idx;
        while ((idx = state.buf.indexOf("\n")) !== -1) {
            const line = state.buf.slice(0, idx);
            state.buf = state.buf.slice(idx + 1);
            if (state.waiters.length > 0) {
                state.waiters.shift()(line);
            } else {
                state.queue.push(line);
            }
        }
    });
    const onEof = () => {
        state.ended = true;
        while (state.waiters.length > 0) {
            state.waiters.shift()(null);
        }
    };
    socket.on("end", onEof);
    socket.on("close", onEof);
    socket.on("error", onEof);

    return id;
}

// Resolves like the wrapped promise, but rejects with a distinct Error after
// `ms` if it hasn't settled yet. The original promise's eventual resolution
// after a timeout is simply discarded (harmless: connections in this
// protocol are one-shot, so at most one stale waiter can ever leak).
function withTimeout(promise, ms) {
    if (!ms) {
        return promise;
    }
    return new Promise((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error("timeout")), ms);
        promise.then(
            (v) => {
                clearTimeout(timer);
                resolve(v);
            },
            (e) => {
                clearTimeout(timer);
                reject(e);
            },
        );
    });
}

module.exports.jsRemoveStale = function (path) {
    try {
        fs.unlinkSync(path);
    } catch (_) {
        // Fine if it never existed.
    }
};

module.exports.jsBind = function (path) {
    return new Promise((resolve, reject) => {
        const listenerId = nextId++;
        const state = { server: null, queue: [], waiters: [] };
        listeners.set(listenerId, state);

        const server = net.createServer((socket) => {
            const connId = registerConn(socket);
            if (state.waiters.length > 0) {
                state.waiters.shift()(connId);
            } else {
                state.queue.push(connId);
            }
        });
        state.server = server;
        server.on("error", (e) => {
            listeners.delete(listenerId);
            reject(e);
        });
        server.listen(path, () => resolve(listenerId));
    });
};

module.exports.jsCloseListener = function (listenerId) {
    const state = listeners.get(listenerId);
    if (state) {
        state.server.close();
        listeners.delete(listenerId);
    }
};

module.exports.jsAccept = function (listenerId) {
    const state = listeners.get(listenerId);
    if (!state) {
        return Promise.reject(new Error("unknown listener"));
    }
    return new Promise((resolve) => {
        if (state.queue.length > 0) {
            resolve(state.queue.shift());
        } else {
            state.waiters.push(resolve);
        }
    });
};

module.exports.jsConnect = function (path, timeoutMs) {
    return new Promise((resolve, reject) => {
        const socket = new net.Socket();
        const timer = setTimeout(() => {
            socket.destroy();
            reject(new Error("connect timeout"));
        }, timeoutMs);
        socket.once("connect", () => {
            clearTimeout(timer);
            resolve(registerConn(socket));
        });
        socket.once("error", (e) => {
            clearTimeout(timer);
            reject(e);
        });
        socket.connect(path);
    });
};

module.exports.jsReadLine = function (connId, timeoutMs) {
    const state = conns.get(connId);
    if (!state) {
        return Promise.reject(new Error("unknown connection"));
    }
    const p = new Promise((resolve) => {
        if (state.queue.length > 0) {
            resolve(state.queue.shift());
            return;
        }
        if (state.ended) {
            resolve(null);
            return;
        }
        state.waiters.push(resolve);
    });
    return withTimeout(p, timeoutMs);
};

module.exports.jsWriteLine = function (connId, line) {
    const state = conns.get(connId);
    if (!state) {
        return Promise.reject(new Error("unknown connection"));
    }
    return new Promise((resolve, reject) => {
        state.socket.write(line + "\n", (err) => {
            if (err) {
                reject(err);
            } else {
                resolve();
            }
        });
    });
};

module.exports.jsCloseConn = function (connId) {
    const state = conns.get(connId);
    if (state) {
        state.socket.destroy();
        conns.delete(connId);
    }
};
