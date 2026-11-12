/* NautiK9 host for Wayfinder's web build. Wayfinder's UI calls Tauri commands through
   window.__TAURI_INTERNALS__.invoke. Here they go to NautiK9's own server
   (POST /api/apps/wayfinder/invoke). Only the commands NautiK9 really implements answer;
   every other command fails with a plain message, and a banner at the top says which ones. */
(function () {
  'use strict';
  function tok() { try { return localStorage.getItem('nautik9_token') || ''; } catch (e) { return ''; } }
  // Model-backed commands go through the NautiK9 page's modelFetch (same origin, the parent of this
  // frame): it wakes the on-device model when it is asleep and retries once. Anything else, or a frame
  // with no such parent, uses the frame's own fetch.
  function send(cmd, url, init) {
    if (cmd === 'chat_llama') {
      try {
        if (window.parent !== window && typeof window.parent.modelFetch === 'function') return window.parent.modelFetch(url, init, null);
      } catch (e) { /* cross-origin parent: fall through */ }
    }
    return fetch(url, init);
  }
  async function call(cmd, args) {
    var r = await send(cmd, '/api/apps/wayfinder/invoke', {
      method: 'POST', headers: { 'Content-Type': 'application/json', 'Authorization': 'Bearer ' + tok() },
      body: JSON.stringify({ cmd: cmd, args: args || {} })
    });
    var j = null; try { j = await r.json(); } catch (e) {}
    if (!r.ok) throw (j && j.error) || ('NautiK9 host: HTTP ' + r.status);
    return j.result;
  }
  window.__TAURI_INTERNALS__ = {
    invoke: function (cmd, args) { return call(cmd, args); },
    transformCallback: function () { return 0; },
    convertFileSrc: function (p) { return p; },
    metadata: { currentWindow: { label: 'nautik9' }, currentWebview: { label: 'nautik9' } }
  };
  window.__N9_HOST__ = 'nautik9';
  document.addEventListener('DOMContentLoaded', function () {
    call('__capabilities', {}).then(function (c) {
      var bar = document.createElement('div');
      bar.setAttribute('role', 'note');
      bar.style.cssText = 'position:sticky;top:0;z-index:99999;background:#3b2a00;color:#fff3c9;border-bottom:2px solid #f2b84b;padding:8px 12px;font:14px/1.35 system-ui,sans-serif';
      bar.innerHTML = '<b>Running inside NautiK9 (web build).</b> Working here: <b>' + c.supported.join(', ') + '</b> (chat and the Lemonade check use NautiK9\'s provider chain). ' +
        'Not available here: ' + c.unsupported_summary + ' Use the Wayfinder desktop app for those. ' +
        '<button style="margin-left:8px;border:1px solid #f2b84b;background:none;color:inherit;border-radius:6px;padding:2px 8px">Hide</button>';
      bar.querySelector('button').onclick = function () { bar.remove(); };
      document.body.insertBefore(bar, document.body.firstChild);
    }).catch(function () {});
  });
})();
