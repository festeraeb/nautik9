// Presses the Nexus tab's buttons for real: paste an invite, check it, join, wait for the tunnel and
// the peer list, screenshot, then (with --leave) leave and check the panel says so.
//   Phone:   adb forward tcp:9229 localabstract:webview_devtools_remote_<pid>
//            PLAYWRIGHT_FROM=/path/to/package.json node android/e2e/nexus-check.mjs --invite FILE [--leave] [--shots DIR]
//   Desktop: ... nexus-check.mjs --url 'http://127.0.0.1:8340/?token=...' --invite FILE [--leave] [--shots DIR]
// The invite file is read here and typed into the page; it is never printed. Exit code 1 if a check fails.
import { createRequire } from 'module';
import fs from 'fs';
const require = createRequire(process.env.PLAYWRIGHT_FROM || import.meta.url);
const { chromium } = require('playwright');

const arg = (k, d) => { const i = process.argv.indexOf(k); return i > 0 ? process.argv[i + 1] : d; };
const url = arg('--url'), inviteFile = arg('--invite'), shots = arg('--shots'), leave = process.argv.includes('--leave');
const joinOnly = process.argv.includes('--join-only'), leaveOnly = process.argv.includes('--leave-only');
let browser, page;
if (url) {
    browser = await chromium.launch({ executablePath: process.env.CHROME || '/usr/bin/google-chrome', args: ['--no-sandbox'] });
    page = await (await browser.newContext({ viewport: { width: 412, height: 915 } })).newPage();
    await page.goto(url);
} else {
    browser = await chromium.connectOverCDP('http://127.0.0.1:9229');
    page = browser.contexts()[0].pages().find(p => p.url().startsWith('http://127.0.0.1:8340'));
    if (!page) throw new Error('NautiK9 page not found in the WebView');
}
page.on('dialog', d => d.accept());
const errors = [];
page.on('pageerror', e => errors.push(String(e)));
const results = [];
const check = (name, ok, detail) => { results.push({ name, ok: !!ok, detail }); console.log((ok ? 'PASS ' : 'FAIL ') + name + (detail ? ' :: ' + detail : '')); };
const panelText = () => page.evaluate(() => (document.getElementById('nexusPanel') || {}).innerText || '');
const status = () => page.evaluate(async () => (await fetch('/api/nexus/status')).json());
const shot = async n => { if (shots) { fs.mkdirSync(shots, { recursive: true }); await page.screenshot({ path: `${shots}/${n}.png`, fullPage: true }); } };
const press = id => page.evaluate(i => { const e = document.getElementById(i); if (!e) throw new Error('no #' + i); e.click(); }, id);
const waitFor = async (fn, ms) => { const t0 = Date.now(); while (Date.now() - t0 < ms) { const s = await status(); if (fn(s)) return s; await page.waitForTimeout(2000); } return status(); };

await page.evaluate(() => switchTab('nexus', document.getElementById('tabNexus')));
await page.waitForTimeout(1500);
let s = await status();
check('Nexus tab draws the panel', (await panelText()).length > 0, (await panelText()).split('\n')[0]);
check('SDK loaded', s.sdk && s.sdk.loaded, s.sdk && (s.sdk.path || s.sdk.error));
if (!s.joined) check('not joined: no peers shown', (s.peers || []).length === 0 && !/Peer\s+Mesh IP/.test(await panelText()), '');
await shot('nexus-0-start');

if (!leaveOnly && !s.joined && inviteFile) {
    const text = fs.readFileSync(inviteFile, 'utf8');
    await page.evaluate(t => { document.getElementById('nxInvite').value = t; }, text);
    await press('nxCheck');
    await page.waitForTimeout(1500);
    s = await status();
    const pt = await panelText();
    check('invite shows network and server before joining', s.offer && pt.includes(s.offer.network) && pt.includes(s.offer.server), s.offer ? `${s.offer.network} @ ${s.offer.server}` : (s.last_error || 'no offer'));
    check('invite secret is not on the page', !/lnk_[0-9a-f]{20}/.test(await page.content()), '');
    await shot('nexus-1-offer');
    await press('nxJoin');
    s = await waitFor(x => x.joined && x.tunnel_up && (x.peers || []).length > 0, 150000);
    check('join: joined with a tunnel to the server', s.joined && s.tunnel_up, `state ${s.state}, path ${s.server_path}, ip ${s.tunnel_ip}, error ${s.last_error}`);
    check('peers listed from the SDK', (s.peers || []).length > 0, (s.peers || []).map(p => `${p.name}:${p.path || 'no path'}`).join(', '));
    await page.waitForTimeout(2500);
    await shot('nexus-2-joined');
}

if (!joinOnly && (leave || leaveOnly)) {
    s = await status();
    if (s.joined) {
        await press('nxLeave');
        s = await waitFor(x => !x.joined || x.leave_unconfirmed, 120000);
        if (s.joined && s.leave_unconfirmed) {
            // The server dropped the tunnel before its answer came back: the panel says so and offers Forget.
            await page.waitForTimeout(6000);
            await shot('nexus-3a-leave-no-answer');
            check('leave without an answer: panel says so and offers Forget', /no answer/.test(await panelText()) && await page.evaluate(() => !!document.getElementById('nxForget')), s.last_error || '');
            await press('nxForget');
            s = await waitFor(x => !x.joined, 30000);
        }
        check('leave: panel back to not joined', !s.joined && /Not joined/.test(await panelText()), s.last_error || '');
        await shot('nexus-3-left');
    }
}
check('no page errors', errors.length === 0, errors.join(' | '));
await browser.close();
process.exit(results.every(r => r.ok) ? 0 : 1);
