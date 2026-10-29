// Drives the NautiK9 app's WebView on the phone over adb (debuggable build).
//   adb forward tcp:9229 localabstract:webview_devtools_remote_<pid>
//   node android/e2e/app-check.mjs [--download]
// Checks: the page has the app bridge, the inbox is pushed with no adb in the server,
// and (with --download) starts the on-device model download.
import { createRequire } from 'module';
const require = createRequire(process.env.PLAYWRIGHT_FROM || import.meta.url);
const { chromium } = require('playwright');

const browser = await chromium.connectOverCDP('http://127.0.0.1:9229');
const page = browser.contexts()[0].pages().find(p => p.url().startsWith('http://127.0.0.1:8340'));
if (!page) throw new Error('NautiK9 page not found in the WebView');
const errors = [];
page.on('pageerror', e => errors.push(String(e)));

const bridge = await page.evaluate(() => !!window.NautiK9Android);
const inbox = await page.evaluate(async () => {
    await pushAppInbox();
    const r = await fetch('/api/sms/inbox');
    const d = await r.json();
    return { status: r.status, count: d.messages.length, statuses: d.messages.map(m => m.status) };
});
const model = await page.evaluate(() => JSON.parse(window.NautiK9Android.offlineModelStatus()));
let after = null;
if (process.argv.includes('--download') && model.state === 'none') {
    await page.evaluate(() => window.NautiK9Android.downloadOfflineModel());
    await page.waitForTimeout(5000);
    after = await page.evaluate(() => JSON.parse(window.NautiK9Android.offlineModelStatus()));
}
console.log(JSON.stringify({ bridge, inbox, model, after, errors }, null, 1));
await browser.close();
