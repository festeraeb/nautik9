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

// Themes: each card applies, draws its artwork, and is saved on the server.
await tab('settings');
const art = () => page.evaluate(() => ({ theme: document.documentElement.dataset.theme, hero: document.getElementById('n9-hero').innerHTML.length, scene: document.getElementById('n9-scene').innerHTML.length }));
for (const t of ['lakes', 'dead', 'dark']) {
    await press(`[data-theme-card="${t}"]`);
    await page.waitForTimeout(500);
    const a = await art();
    const saved = await page.evaluate(async () => (await (await fetch('/api/ui')).json()).theme);
    check(`theme ${t} applies, has ${t === 'dark' ? 'no' : 'its'} artwork, saved on the server`, a.theme === t && saved === t && (t === 'dark' ? a.hero === 0 : a.hero > 2000 && a.scene > 2000), JSON.stringify({ ...a, saved }));
}
await press('#themeQuick');
await page.waitForTimeout(300);
check('quick toggle cycles the theme', (await art()).theme === 'lakes', (await art()).theme);
await press('[data-theme-card="dark"]');

// Apps: Wayfinder loads inside the app and says what does not work here.
await tab('apps');
await page.waitForTimeout(800);
await press('#n9BuiltinList button', 'Open');
await page.waitForTimeout(4000);
const wf = page.frames().find(f => f.url().includes('/static/apps/wayfinder/'));
check('Wayfinder frame loads', !!wf, wf ? wf.url() : 'no frame');
if (wf) {
    const body = await wf.evaluate(() => document.body.innerText);
    check('Wayfinder renders its UI and the host banner', /Wayfinder/.test(body) && /Running inside NautiK9/.test(body) && /Not available here/.test(body), body.slice(0, 160).replace(/\n/g, ' '));
}
const inv = await page.evaluate(async () => { const r = await fetch('/api/apps/wayfinder/invoke', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ cmd: 'scan_directory', args: {} }) }); return { status: r.status, body: (await r.text()).slice(0, 120) }; });
check('Wayfinder file commands are refused plainly', inv.status === 501, inv.body);

// Knowledge graph API round trip through the page
const kg = await page.evaluate(async () => {
    const post = b => fetch('/api/agent/knowledge', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(b) }).then(r => r.json());
    const v = await post({ action: 'verify', claim: 'e2e_probe(nothing, nothing)' });
    return v.verdict;
});
check('knowledge: an unknown claim is unknown', kg === 'unknown', kg);

// Buzz over Tailscale (read-only here: no test posts under Thom's name) and the Board's gate.
await page.evaluate(() => readNet());
await page.waitForTimeout(600);
const net = await page.evaluate(async () => (await (await fetch('/api/net')).json()));
const bz = await page.evaluate(async () => {
    const g = async u => { const r = await fetch(u); return { status: r.status, body: await r.json().catch(() => ({})) }; };
    return { settings: await g('/api/buzz/settings'), rooms: await g('/api/buzz/rooms'), msgs: await g('/api/buzz/messages?channel=ops&limit=5'), q: await g('/api/buzz/questions?room=human-questions'), tasks: await g('/api/buzz/tasks'), fleet: (await fetch('/api/fleet/status')).status, board: await g('/api/board/status') };
});
if (net.tailscale) {
    check('buzz rooms answer over Tailscale', bz.rooms.status === 200 && bz.rooms.body.rooms.length > 3, `${bz.rooms.status} ${bz.rooms.body.rooms ? bz.rooms.body.rooms.length + ' rooms' : bz.rooms.body.error}`);
    check('buzz messages arrive oldest to newest', bz.msgs.status === 200 && bz.msgs.body.messages.length > 0 && bz.msgs.body.messages.every((m, i, a) => !i || Number(a[i - 1].created_at) <= Number(m.created_at)), String(bz.msgs.body.messages && bz.msgs.body.messages.length));
    check('buzz open questions and tasks answer', bz.q.status === 200 && bz.tasks.status === 200 && Array.isArray(bz.tasks.body.tasks), `${bz.q.status}/${bz.tasks.status}`);
    check('fleet answers over Tailscale', bz.fleet === 200, String(bz.fleet));
} else {
    check('Tailscale off: buzz and fleet say so plainly, at once', bz.rooms.status === 503 && bz.rooms.body.kind === 'no_tailscale' && bz.fleet === 503, `${bz.rooms.body.error} / fleet ${bz.fleet}`);
}
check('buzz address is set or derived, key state is only a boolean', typeof bz.settings.body.has_board_key === 'boolean' && !JSON.stringify(bz.settings.body).match(/key["']?\s*:\s*"/), JSON.stringify(Object.keys(bz.settings.body)));
check('board without a key says to enter it', bz.board.body.kind === 'no_key' || bz.board.status === 200, `${bz.board.status} ${bz.board.body.kind || ''}`);
await tab('buzz');
await page.waitForTimeout(3000);
const bzUi = await page.evaluate(() => ({ rooms: document.querySelectorAll('#bzRooms button').length, msgs: document.querySelectorAll('#bzStream .msg-bubble').length, status: document.getElementById('bzStatus').innerText }));
check('Buzz tab shows rooms and messages (or the plain reason)', (bzUi.rooms > 0 && bzUi.msgs > 0) || /Tailscale|offline/i.test(bzUi.status), JSON.stringify(bzUi));

// God's Eye: read-only checks (no fix is sent, no tracking is switched on).
const ge = await page.evaluate(async () => { const r = await fetch('/api/godseye/status'); return { status: r.status, body: await r.json().catch(() => ({})) }; });
if (net.tailscale) check('God\'s Eye track and stream doors answer over Tailscale', ge.status === 200 && ge.body.track.ok && ge.body.stream.ok, JSON.stringify({ track: ge.body.track && ge.body.track.kind || 'ok', stream: ge.body.stream && ge.body.stream.kind || 'ok' }));
else check('God\'s Eye says Tailscale is off plainly', ge.body.track && ge.body.track.kind === 'no_tailscale', JSON.stringify(ge.body.track));
await tab('godseye');
await page.waitForTimeout(6000);
const geUi = await page.evaluate(() => ({ decision: document.getElementById('geDecision').innerText, track: document.getElementById('geTrack').checked }));
check('God\'s Eye tab shows a decision and tracking is off', /Chosen|Mode set/.test(geUi.decision) && geUi.track === false, JSON.stringify(geUi));
await tab('helm');

check('no page errors', errors.length === 0, errors.join(' | '));
for (const r of results) console.log(`${r.ok ? 'PASS' : 'FAIL'}  ${r.name}${r.detail ? '  — ' + r.detail : ''}`);
await browser.close();
process.exit(results.every(r => r.ok) ? 0 : 1);
