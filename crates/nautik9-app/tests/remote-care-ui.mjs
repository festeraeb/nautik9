// Playwright check of the Settings > Remote care card against a running nautik9-app on the host.
//   nautik9-app --config-dir CFG serve --bind 127.0.0.1 --port 18343 --sms-bridge app --allow-from loopback,tailnet
//   PLAYWRIGHT_FROM=/path/to/package.json node crates/nautik9-app/tests/remote-care-ui.mjs CFG SHOTS_DIR
// Expects a Tailscale address on the host (TAILNET_ADDR, default t440's 100.72.182.77). The token is never printed.
import { createRequire } from 'module';
import fs from 'fs';
const require = createRequire(process.env.PLAYWRIGHT_FROM || import.meta.url);
const { chromium } = require('playwright');
const cfg = process.argv[2]; const TS = process.env.TAILNET_ADDR || '100.72.182.77'; const URL = 'http://127.0.0.1:18343';
const tok = fs.readFileSync(cfg + '/token', 'utf8').trim();
const b = await chromium.launch({ executablePath: '/usr/bin/google-chrome', headless: true, args: ['--no-sandbox'] });
const ctx = await b.newContext({ viewport: { width: 412, height: 915 } });
await ctx.grantPermissions(['clipboard-read', 'clipboard-write'], { origin: URL });
const pg = await ctx.newPage(); const errs = []; pg.on('pageerror', e => errs.push(String(e)));
await pg.goto(URL + '/?token=' + tok); await pg.waitForTimeout(800);
await pg.evaluate(() => switchTab('settings', document.querySelector(`[onclick*="'settings'"]`))); await pg.waitForTimeout(800);
const t = id => pg.evaluate(i => document.getElementById(i).innerText, id);
const res = [];
const check = (n, ok, d) => { res.push(ok); console.log((ok ? 'PASS ' : 'FAIL ') + n + (d ? ' - ' + d : '')); };
check('card visible', await pg.isVisible('#rcCard'));
check('switch starts off', !(await pg.isChecked('#rcOn')));
const addr = await t('rcAddr'); check('shows the tailnet address and port', addr.includes(TS) && addr.includes('port 18343'), addr);
const ver = await t('rcVer'); check('shows the running build', /Running: server 0\.1\.0, build [0-9a-f]+/.test(ver), ver);
check('note text', (await pg.innerText('#rcCard')).includes('On: NautiK9 answers its home fleet over Tailscale, token needed, nothing from Wi-Fi. Off: this phone only.'));
await pg.click('#rcOn'); await pg.waitForTimeout(500);
const st = JSON.parse(fs.readFileSync(cfg + '/remote_care.json', 'utf8'));
check('switch on is saved', st.enabled === true && (await t('rcMsg')) === 'On.');
await pg.reload(); await pg.waitForTimeout(800);
await pg.evaluate(() => switchTab('settings', document.querySelector(`[onclick*="'settings'"]`))); await pg.waitForTimeout(800);
check('still on after reload', await pg.isChecked('#rcOn'));
await pg.click('#rcCopy'); await pg.waitForTimeout(300);
const clip = await pg.evaluate(() => navigator.clipboard.readText());
check('pairing line copied with this token', clip === 'http://' + TS + ':18343/?token=' + tok, clip.replace(tok, '<token>'));
check('token not on the page', !(await pg.content()).includes(tok));
await pg.click('#rcOn'); await pg.waitForTimeout(500);
check('switch off is saved', JSON.parse(fs.readFileSync(cfg + '/remote_care.json', 'utf8')).enabled === false);
for (const theme of ['dark', 'lakes', 'dead']) { await pg.evaluate(th => N9T.set(th), theme); await pg.waitForTimeout(300); await pg.locator('#rcCard').screenshot({ path: process.argv[3] + '/rc-' + theme + '.png' }); }
check('no page errors', errs.length === 0, errs.join(' | '));
await b.close();
process.exit(res.every(Boolean) ? 0 : 1);
