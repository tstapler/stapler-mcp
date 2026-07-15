module.exports.jsNowMillis = function () {
    return Date.now();
};

module.exports.jsSleep = function (ms) {
    return new Promise((resolve) => setTimeout(resolve, ms));
};
