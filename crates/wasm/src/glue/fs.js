const fs = require("fs");
const path = require("path");

module.exports.jsEnsureDir = function (dirPath) {
    fs.mkdirSync(dirPath, { recursive: true });
};

module.exports.jsReadFile = function (filePath) {
    return new Promise((resolve, reject) => {
        fs.readFile(filePath, (err, data) => {
            if (err) {
                if (err.code === "ENOENT") {
                    resolve(null);
                } else {
                    reject(err);
                }
                return;
            }
            resolve(new Uint8Array(data));
        });
    });
};

module.exports.jsWriteFile = function (filePath, bytes) {
    return new Promise((resolve, reject) => {
        fs.mkdir(path.dirname(filePath), { recursive: true }, (err) => {
            if (err) {
                reject(err);
                return;
            }
            fs.writeFile(filePath, Buffer.from(bytes), (err2) => {
                if (err2) {
                    reject(err2);
                } else {
                    resolve();
                }
            });
        });
    });
};
