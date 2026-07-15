const { chromium } = require("playwright-core");

// One shared browser-process launch for the daemon's whole lifetime, lazily
// started on the first call — mirrors the native adapter's single
// `chromiumoxide::Browser` allocator. Uses the system-installed Chrome
// (`channel: "chrome"`) so nothing needs downloading.
let browserPromise = null;
function getBrowser() {
    if (!browserPromise) {
        browserPromise = chromium.launch({ headless: true, channel: "chrome" });
    }
    return browserPromise;
}

// Called once at daemon shutdown — without this the Chrome subprocess (and
// Node's handle to its CDP connection) keeps the event loop alive forever,
// even after the socket listener itself has been closed.
module.exports.jsCloseBrowser = async function () {
    if (browserPromise) {
        const browser = await browserPromise;
        await browser.close();
        browserPromise = null;
    }
};

module.exports.jsNavigateAndExtract = async function (url, timeoutMs) {
    const browser = await getBrowser();
    const page = await browser.newPage();
    try {
        await page.goto(url, { timeout: timeoutMs, waitUntil: "load" });
        const title = await page.title();
        const html = await page.content();
        const text = await page.evaluate(() => (document.body ? document.body.innerText : ""));
        const finalUrl = page.url();
        return { title, html, text, finalUrl };
    } finally {
        await page.close();
    }
};
