module.exports.jsHttpGet = async function (url, headersJson) {
    const headers = JSON.parse(headersJson);
    const resp = await fetch(url, { method: "GET", headers });
    const body = new Uint8Array(await resp.arrayBuffer());
    return { status: resp.status, body };
};
