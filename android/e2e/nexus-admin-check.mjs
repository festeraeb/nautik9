// Presses the Nexus tab's Admission buttons for real (desktop build; the device needs the private admin file
// <config dir>/nexus/nexus-admin.json, which is never in the repo).
//   node nexus-admin-check.mjs --url 'http://127.0.0.1:8356/?token=...' [--accept NAME] [--revoke NAME] [--unban NAME] [--shots DIR]
//   --accept NAME  press Accept on the waiting request of that name
//   --revoke NAME  press "Revoke and k-line" on that device, wait for the panel to say it is k-lined
//   --unban NAME   press Un-ban on that k-lined device
// Prints each step with its time. Exit code 1 if a check fails.
import { createRequire } from 'module';
import fs from 'fs';
const require = createRequire(process.env.PLAYWRIGHT_FROM || import.meta.url);
const { chromium } = require('playwright');
const arg = (k) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : undefined; };
const url = arg('--url'), accept = arg('--accept'), revoke = arg('--revoke'), unban = arg('--unban'), shots = arg('--shots');
if (!url) { console.error('--url required'); process.exit(2); }
const browser = await chromium.launch({ executablePath: process.env.CHROME || '/usr/bin/google-chrome', args: ['--no-sandbox'] });
const page = await (await browser.newContext({ viewport: { width: 412, height: 915 } })).newPage();
page.on('dialog', d => d.accept());
const errors = []; page.on('pageerror', e => errors.push(String(e)));
let fails = 0;
const check = (name, ok, detail) => { if (!ok) fails++; console.log((ok ? 'PASS ' : 'FAIL ') + name + (detail ? ' :: ' + detail : '')); };
const text = () => page.evaluate(() => (document.getElementById('nexusPanel') || {}).innerText || '');
const shot = async n => { if (shots) { fs.mkdirSync(shots, { recursive: true }); await page.screenshot({ path: `${shots}/${n}.png`, fullPage: true }); } };
const waitText = async (re, ms) => { const t0 = Date.now(); while (Date.now() - t0 < ms) { if (re.test(await text())) return Date.now() - t0; await page.waitForTimeout(250); } return -1; };
const press = (label, near) => page.evaluate(([label, near]) => {
    const root = document.getElementById('nexusPanel');
    const btns = [...root.querySelectorAll('.nx-admin button')].filter(b => b.textContent.trim() === label);
    const b = btns.find(b => { let n = b; for (let i = 0; i < 4 && n; i++, n = n.parentElement) { if (n.innerText && n.innerText.includes(near)) return true; } return false; });
    if (!b) return false; b.click(); return true;
}, [label, near]);

await page.goto(url);
await page.evaluate(() => switchTab('nexus', document.getElementById('tabNexus')));
let ms = await waitText(/Admission[\s\S]*(Nothing waiting|Waiting for a decision)/, 15000);
check('Admission section draws', ms >= 0, ms + ' ms');
await shot('admin-1-open');
if (accept) {
    ms = await waitText(new RegExp(accept), 8000);
    check('waiting request is listed: ' + accept, ms >= 0);
    const t0 = Date.now(); check('Accept pressed', await press('Accept', accept));
    const t1 = Date.now();
    while (Date.now() - t1 < 10000 && new RegExp('(Waiting for a decision)\\n' + accept).test(await text())) await page.waitForTimeout(250);
    check('request left the waiting list', !new RegExp('(Waiting for a decision)\\n' + accept).test(await text()), (Date.now() - t0) + ' ms after the press');
    console.log('accept round trip ' + (Date.now() - t0) + ' ms');
    await shot('admin-2-accepted');
}
if (revoke) {
    ms = await waitText(new RegExp(revoke + '\\s+(online|offline)'), 10000);
    check('device is listed: ' + revoke, ms >= 0);
    const t0 = Date.now(); check('Revoke and k-line pressed (confirm accepted)', await press('Revoke and k-line', revoke));
    ms = await waitText(new RegExp('k-lined: ' + revoke), 15000);
    check('panel shows it k-lined', ms >= 0, ms >= 0 ? (Date.now() - t0) + ' ms after the press' : 'never');
    await shot('admin-3-klined');
}
if (unban) {
    const t0 = Date.now(); check('Un-ban pressed', await press('Un-ban', unban));
    ms = await waitText(new RegExp('^(?![\\s\\S]*k-lined: ' + unban + ')[\\s\\S]*$'), 10000);
    check('k-line lifted in the panel', ms >= 0, (Date.now() - t0) + ' ms after the press');
    await shot('admin-4-unbanned');
}
check('no page errors', errors.length === 0, errors.join(' | '));
await browser.close();
process.exit(fails ? 1 : 0);
