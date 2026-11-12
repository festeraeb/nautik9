/* Nexus panel: join, status and leave for a Lemonade Nexus network.
 * One file for NautiK9 and Jade. The app passes a transport with five calls that return the
 * nexus-join JSON (status, offer, discard, join, leave); the panel only draws what they return.
 * Plain DOM, no framework. Labels are short on purpose.
 *
 *   NexusPanel.mount(rootElement, transport, {deviceName?, pollMs?})
 *   transport = { status(), offer(text), discard(), join(deviceName), leave(force) }  -> Promise<object>
 */
(function (global) {
  'use strict';

  function el(tag, attrs, kids) {
    const e = document.createElement(tag);
    if (attrs) for (const k in attrs) {
      if (k === 'text') e.textContent = attrs[k];
      else if (k === 'class') e.className = attrs[k];
      else if (k.startsWith('on')) e.addEventListener(k.slice(2), attrs[k]);
      else e.setAttribute(k, attrs[k]);
    }
    (kids || []).forEach(c => c && e.appendChild(typeof c === 'string' ? document.createTextNode(c) : c));
    return e;
  }

  function ago(t, now) {
    if (!t) return 'never';
    const s = Math.max(0, (now || Date.now() / 1000) - t);
    if (s < 60) return Math.round(s) + ' s ago';
    if (s < 3600) return Math.round(s / 60) + ' min ago';
    if (s < 86400) return Math.round(s / 3600) + ' h ago';
    return new Date(t * 1000).toLocaleString();
  }

  function bytes(n) {
    if (n == null) return '-';
    if (n < 1024) return n + ' B';
    if (n < 1048576) return (n / 1024).toFixed(1) + ' KB';
    return (n / 1048576).toFixed(1) + ' MB';
  }

  function pathLabel(p, peer) {
    if (!p) return 'no path';
    if (p === 'relay' && peer && peer.relay_endpoint) return 'relay';
    return p; // lan | direct | relay | wss | none
  }

  const CSS = `
.nx { font-size: 0.9rem; }
.nx-row { display: flex; gap: 8px; flex-wrap: wrap; align-items: center; margin: 4px 0; }
.nx-k { color: var(--text-sub, #888); min-width: 7.5em; }
.nx-badge { display: inline-block; padding: 1px 8px; border-radius: 10px; font-size: 0.8rem; border: 1px solid currentColor; }
.nx-ok { color: #2e9d5b; } .nx-warn { color: #c98a00; } .nx-bad { color: #c0392b; } .nx-dim { color: var(--text-sub, #888); }
.nx textarea { width: 100%; min-height: 4.5em; box-sizing: border-box; font-family: monospace; font-size: 0.8rem; }
.nx input[type=text] { min-width: 12em; }
.nx table { width: 100%; border-collapse: collapse; font-size: 0.8rem; margin-top: 6px; }
.nx th, .nx td { text-align: left; padding: 3px 4px; border-bottom: 1px solid rgba(128,128,128,0.25); }
.nx-msg { min-height: 1.2em; font-size: 0.8rem; }
`;

  function mount(root, t, opts) {
    opts = opts || {};
    if (!document.getElementById('nx-css')) document.head.appendChild(el('style', { id: 'nx-css', text: CSS }));
    root.innerHTML = '';
    const box = el('div', { class: 'nx' });
    root.appendChild(box);
    let last = null, busy = false, leaveFailed = false, timer = null, msg = '';

    async function call(fn) {
      if (busy) return;
      busy = true; msg = ''; draw();
      try {
        const r = await fn();
        if (r && r.ok === false) msg = r.error || 'Failed';
      } catch (e) { msg = String(e && e.message || e); }
      busy = false;
      await refresh();
    }

    async function refresh() {
      try { last = await t.status(); } catch (e) { last = last || null; msg = msg || ('No answer: ' + (e && e.message || e)); }
      draw();
    }

    function row(k, v, cls) {
      return el('div', { class: 'nx-row' }, [el('span', { class: 'nx-k', text: k }), el('span', { class: cls || '', text: v == null || v === '' ? '-' : String(v) })]);
    }

    function draw() {
      box.innerHTML = '';
      const s = last;
      if (!s) { box.appendChild(el('div', { class: 'nx-dim', text: 'Loading...' })); return; }
      const now = s.now || Date.now() / 1000;
      if (!s.sdk || !s.sdk.loaded) {
        box.appendChild(el('div', { class: 'nx-row' }, [el('span', { class: 'nx-badge nx-bad', text: 'SDK missing' })]));
        box.appendChild(el('div', { class: 'nx-dim', text: (s.sdk && s.sdk.error) || '' }));
      }
      let badge;
      if (s.joined) badge = s.stale ? ['Joined, stale', 'nx-warn'] : ['Joined', 'nx-ok'];
      else if (s.state === 'joining') badge = ['Joining', 'nx-warn'];
      else if (s.offer) badge = ['Invite ready', 'nx-warn'];
      else badge = ['Not joined', 'nx-dim'];
      box.appendChild(el('div', { class: 'nx-row' }, [el('span', { class: 'nx-badge ' + badge[1], text: badge[0] }), busy ? el('span', { class: 'nx-dim', text: 'Working...' }) : null]));

      if (s.joined) {
        box.appendChild(row('Device', s.device_name || '(server name)'));
        box.appendChild(row('Network', s.network));
        box.appendChild(row('Server', s.server));
        box.appendChild(row('Mesh IP', s.tunnel_ip));
        box.appendChild(row('To server', pathLabel(s.server_path), s.tunnel_up ? 'nx-ok' : 'nx-bad'));
        box.appendChild(row('Updated', ago(s.read_at, now) + (s.stale ? ' (stale: ' + (s.stale_reason || 'no answer') + ')' : ''), s.stale ? 'nx-warn' : ''));
        box.appendChild(row('Traffic', 'in ' + bytes(s.rx_bytes) + ', out ' + bytes(s.tx_bytes)));
        if (s.needs_rejoin) box.appendChild(row('Server', 'asks for a new join', 'nx-bad'));
        const peers = s.peers || [];
        if (!peers.length) box.appendChild(el('div', { class: 'nx-dim', text: s.read_at ? 'No peers.' : 'No peer list yet.' }));
        else {
          const tb = el('tbody');
          peers.forEach(p => tb.appendChild(el('tr', { class: s.stale ? 'nx-dim' : '' }, [
            el('td', { text: p.name || p.node_id || '?' }),
            el('td', { text: p.tunnel_ip || '' }),
            el('td', { class: p.online ? 'nx-ok' : 'nx-dim', text: pathLabel(p.path, p) }),
            el('td', { text: ago(p.last_handshake, now) }),
            el('td', { text: bytes(p.rx_bytes) + ' / ' + bytes(p.tx_bytes) }),
          ])));
          box.appendChild(el('table', null, [el('thead', null, [el('tr', null, ['Peer', 'Mesh IP', 'Path', 'Handshake', 'In / out'].map(h => el('th', { text: h })))]), tb]));
          if (s.stale) box.appendChild(el('div', { class: 'nx-dim', text: 'Last known list, read ' + ago(s.read_at, now) + '.' }));
        }
        if (s.last_network_change) box.appendChild(row('Network change', ago(s.last_network_change.at, now) + ' (' + s.last_network_change.what + ')'));
        const btns = el('div', { class: 'nx-row' }, [
          el('button', { class: 'btn-mini', id: 'nxLeave', text: 'Leave', onclick: () => { if (confirm('Leave ' + (s.network || 'the network') + '?')) call(async () => { const r = await t.leave(false); leaveFailed = r && r.ok === false; return r; }); } }),
          leaveFailed ? el('button', { class: 'btn-mini', id: 'nxForget', text: 'Forget here', onclick: () => { if (confirm('Forget on this device only? The server keeps an offline entry.')) call(async () => { leaveFailed = false; return t.leave(true); }); } }) : null,
        ]);
        box.appendChild(btns);
      } else if (s.offer) {
        const o = s.offer;
        box.appendChild(row('Network', o.network));
        box.appendChild(row('Server', o.server));
        box.appendChild(row('Expires', o.expires_at ? (o.expired ? 'expired' : new Date(o.expires_at * 1000).toLocaleString()) : 'unknown', o.expired ? 'nx-bad' : ''));
        if (o.permissions && o.permissions.length) box.appendChild(row('Grants', o.permissions.join(', ')));
        if (o.address_map && o.address_map.length) box.appendChild(row('Via', 'address map (' + o.address_map.length + ')'));
        const name = el('input', { type: 'text', id: 'nxName', value: opts.deviceName || s.device_default || '', placeholder: 'Device name' });
        box.appendChild(el('div', { class: 'nx-row' }, [el('span', { class: 'nx-k', text: 'Device name' }), name]));
        box.appendChild(el('div', { class: 'nx-row' }, [
          el('button', { class: 'btn-primary', id: 'nxJoin', text: 'Join', onclick: () => call(() => t.join(name.value.trim())) }),
          el('button', { class: 'btn-mini', id: 'nxDiscard', text: 'Discard', onclick: () => call(() => t.discard()) }),
        ]));
      } else {
        const ta = el('textarea', { id: 'nxInvite', placeholder: 'Invite (file text or 8-digit code)', autocomplete: 'off', spellcheck: 'false' });
        const file = el('input', { type: 'file', id: 'nxFile', accept: '.json,.txt,application/json,text/plain', style: 'display:none' });
        file.addEventListener('change', async () => { const f = file.files && file.files[0]; if (f) { const text = await f.text(); call(() => t.offer(text)); } });
        box.appendChild(ta);
        box.appendChild(el('div', { class: 'nx-row' }, [
          el('button', { class: 'btn-primary', id: 'nxCheck', text: 'Check invite', onclick: () => { const v = ta.value; ta.value = ''; call(() => t.offer(v)); } }),
          el('button', { class: 'btn-mini', id: 'nxLoad', text: 'Load file', onclick: () => file.click() }),
          file,
        ]));
      }
      if (s.last_error) box.appendChild(el('div', { class: 'nx-msg nx-bad', text: s.last_error }));
      box.appendChild(el('div', { class: 'nx-msg nx-bad', id: 'nxMsg', text: msg }));
    }

    refresh();
    timer = setInterval(() => { if (!busy && root.offsetParent !== null) refresh(); }, opts.pollMs || 5000);
    return { refresh, stop: () => clearInterval(timer) };
  }

  global.NexusPanel = { mount };
})(typeof window !== 'undefined' ? window : this);
