const os = require("os");

module.exports.jsGetEnv = function (key) {
    const v = process.env[key];
    return v === undefined ? undefined : v;
};

module.exports.jsHomeDir = function () {
    return os.homedir();
};
