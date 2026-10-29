// Presses the app's buttons in the phone's WebView and reports what each one did.
//   adb forward tcp:9229 localabstract:webview_devtools_remote_<pid>
//   PLAYWRIGHT_FROM=/path/to/package.json node android/e2e/buttons-check.mjs [--slow]
// --slow also runs the model-backed checks (tree of thought, chat with weather and web search).
// Exit code 1 if any check fails. The torch is switched on for one second and then off.
import { createRequire } from 'module';
const require = createRequire(process.env.PLAYWRIGHT_FROM || import.meta.url);
const { chromium } = require('playwright');

const slow = process.argv.includes('--slow');
const browser = await chromium.connectOverCDP('http://127.0.0.1:9229');
const page = browser.contexts()[0].pages().find(p => p.url().startsWith('http://127.0.0.1:8340'));
if (!page) throw new Error('NautiK9 page not found in the WebView');
const errors = [];
page.on('pageerror', e => errors.push(String(e)));
const results = [];
const check = (name, ok, detail) => results.push({ name, ok: !!ok, detail });
const text = id => page.evaluate(i => (document.getElementById(i) || {}).innerText || '', id);
// DOM clicks: the same onclick handlers, and they work while the phone screen is off (Playwright's
// own click waits for frames that a dozing screen never draws).
const press = (sel, label) => page.evaluate(([s, l]) => {
    const el = l ? [...document.querySelectorAll(s)].find(e => e.innerText.includes(l)) : document.querySelector(s);
    if (!el) throw new Error('no element ' + s + ' ' + (l || ''));
    el.click();
}, [sel, label || '']);
const tab = async id => { await page.evaluate(t => switchTab(t, document.querySelector(`[onclick*="'${t}'"]`)), id); await page.waitForTimeout(800); };

const b = await page.evaluate(() => ['vibrate', 'torch', 'torchState', 'battery', 'requestLocation'].filter(m => window.NautiK9Android && typeof window.NautiK9Android[m] === 'function'));
check('bridge has vibrate/torch/battery/location', b.length === 5, b.join(','));
check('host badge says phone app', (await text('hostBadge')) === 'PHONE APP', await text('hostBadge'));

await tab('senses');
await press('button', '📳 Haptic Pulse');
await page.waitForTimeout(300);
check('haptic pulse vibrates', /Vibrated \(phone app\)/.test(await text('controlStatus')), await text('controlStatus'));

await press('#torchBtn');
await page.waitForTimeout(1000);
const on = await text('controlStatus');
await press('#torchBtn');
await page.waitForTimeout(500);
const off = await text('controlStatus');
check('torch turns on then off', on === 'Torch on.' && off === 'Torch off.', `${on} / ${off}`);

const batt = await text('battReadout');
check('battery shows a real level', /^\d+%$/.test(batt), `${batt} ${await text('chargeReadout')}`);

await press('button', 'Acquire Live GPS');
await page.waitForFunction(() => !/Acquiring/.test(document.getElementById('gpsBadge').innerText), null, { timeout: 45000 }).catch(() => {});
const gps = `${await text('gpsBadge')} | ${await text('gpsReadout')} | ${await text('gpsDetail')}`;
check('gps gives a live fix or a stated reason (never a made-up position)', /Live fix|Old fix/.test(gps) || /No fix: .+/.test(gps), gps);
check('no fixed Thunder Bay position anywhere', !(await page.content()).includes('45.031200'), '');

const before = await text('avatarStateText');
await page.evaluate(() => triggerAvatarAction('navigate'));
await page.waitForTimeout(800);
check('avatar action changes Mack state', (await text('avatarStateText')) !== before, await text('avatarStateText'));

await tab('helm');
await page.evaluate(() => { document.getElementById('quickSearch').value = 'mack'; });
await page.evaluate(() => quickSniff());
await page.waitForTimeout(1500);
check('quick search answers', !/Searching/.test(await text('quickSearchResults')), (await text('quickSearchResults')).slice(0, 120));

await tab('vault');
await page.waitForTimeout(1500);
const notes = await page.evaluate(async () => { const r = await fetch('/api/notes'); return { status: r.status, body: (await r.text()).slice(0, 200) }; });
check('vault notes load', notes.status === 200, notes.body);

await tab('fleet');
await page.waitForTimeout(2500);
const fleet = await page.evaluate(async () => (await fetch('/api/fleet/status')).status);
check('fleet tab answers', fleet === 200, String(fleet));

await tab('tot');
check('ToT tab does not say planned', !/Planned, not built|Not implemented/.test(await page.evaluate(() => document.getElementById('pane-tot').innerText)), '');

const providers = await page.evaluate(async () => (await (await fetch('/api/providers')).json()).providers.map(p => `${p.name} ${p.base_url}`));
check('provider chain (info)', true, providers.join(' ; '));

if (slow) {
    await page.evaluate(() => { document.getElementById('totProblem').value = 'Pick a safe anchoring spot for a small boat in a 15 knot west wind near a north-south shoreline'; });
    await page.evaluate(() => runToT());
    await page.waitForFunction(() => !/Asking the server/.test(document.getElementById('totTreeContainer').innerText), null, { timeout: 420000 }).catch(() => {});
    const tree = await text('totTreeContainer'), plan = await text('totWinningPlanContainer');
    check('tree of thought builds a tree and answers', tree.length > 40 && plan.length > 40 && !/not implemented/i.test(tree), (plan || tree).slice(0, 300));

    for (const [name, q, tool] of [['chat answers weather with the weather tool', 'What is the weather in Detroit today?', 'weather'],
                                    ['chat searches the web', 'Search the web: when did the SS Cedarville sink?', 'web_search']]) {
        const r = await page.evaluate(async m => {
            const res = await fetch('/api/agent/chat', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ message: m }) });
            return { status: res.status, body: await res.json() };
        }, q);
        const out = r.body.result || {};
        const used = (out.tool_calls || []).map(t => `${t.name}:${t.ok}`);
        check(name, r.status === 200 && used.includes(`${tool}:true`), `${used.join(',')} | ${out.model || r.body.error || ''} | ${(out.reply || JSON.stringify(r.body.attempts || '')).slice(0, 240)}`);
    }
}

check('no page errors', errors.length === 0, errors.join(' | '));
for (const r of results) console.log(`${r.ok ? 'PASS' : 'FAIL'}  ${r.name}${r.detail ? '  — ' + r.detail : ''}`);
await browser.close();
process.exit(results.every(r => r.ok) ? 0 : 1);
