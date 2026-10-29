/* NautiK9 notes workspace: search, split editor, two notes side by side, graph view,
 * Markdown preview with KaTeX math and ![[image]] / ![[note]] embeds.
 * Plain JS, no build step, no CDN. Mounts into <div id="notesPlus"> on the Vault tab.
 * The page's own fetch wrapper adds the Authorization header to same-origin requests.
 * renderMarkdown() is pure (no DOM) so it can be tested with node. */
(function (root) {
  'use strict';

  // ---------------------------------------------------------------- Markdown
  var MEDIA = {
    png: 'img', jpg: 'img', jpeg: 'img', gif: 'img', webp: 'img', bmp: 'img', avif: 'img',
    mp3: 'audio', wav: 'audio', ogg: 'audio', m4a: 'audio', mp4: 'video', webm: 'video', pdf: 'pdf', txt: 'pdf'
  };

  function esc(v) {
    return String(v == null ? '' : v).replace(/[&<>"']/g, function (c) {
      return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c];
    });
  }

  function mediaKind(name) {
    var m = /\.([A-Za-z0-9]+)$/.exec(name);
    return m ? MEDIA[m[1].toLowerCase()] || null : null;
  }

  /* opts.findNote(name) -> {id,title,content} | null ; opts.depth / opts.seen guard note embeds. */
  function renderMarkdown(src, opts) {
    opts = opts || {};
    var depth = opts.depth || 0;
    var seen = opts.seen || [];
    var stash = [];
    function keep(html) { stash.push(html); return '\u0000' + (stash.length - 1) + '\u0000'; }

    var text = String(src == null ? '' : src).replace(/\u0000/g, '').replace(/\r\n?/g, '\n');

    // 1. code (so nothing inside it is touched)
    text = text.replace(/^(```|~~~)[^\n]*\n([\s\S]*?)(?:\n\1[ \t]*$|$(?![\s\S]))/gm, function (m, f, code) {
      return '\n' + keep('<pre class="np-code"><code>' + esc(code) + '</code></pre>') + '\n';
    });
    text = text.replace(/`([^`\n]+)`/g, function (m, code) { return keep('<code>' + esc(code) + '</code>'); });

    // 2. math: $$ ... $$ (display) and $ ... $ (inline). Typeset later by typeset().
    text = text.replace(/\$\$([\s\S]+?)\$\$/g, function (m, tex) {
      return keep('<div class="np-math" data-display="1" data-tex="' + esc(tex.trim()) + '">' + esc(tex.trim()) + '</div>');
    });
    text = text.replace(/(^|[^\\$\w])\$(?!\s)([^$\n]*?[^\s\\$])\$(?![\d$])/g, function (m, pre, tex) {
      return pre + keep('<span class="np-math" data-tex="' + esc(tex) + '">' + esc(tex) + '</span>');
    });

    // 3. embeds: ![[photo.png|300]] and ![[Another note]]
    text = text.replace(/!\[\[([^\]\n]+)\]\]/g, function (m, inner) {
      var parts = inner.split('|');
      var name = parts[0].split('#')[0].trim();
      var size = parts[1] && /^\d{1,4}$/.test(parts[1].trim()) ? parseInt(parts[1], 10) : 0;
      var kind = mediaKind(name);
      if (kind === 'img') {
        return keep('<img class="np-att" data-att="' + esc(name) + '" alt="' + esc(name) + '"' + (size ? ' style="width:' + size + 'px"' : '') + '>');
      }
      if (kind === 'audio' || kind === 'video') {
        return keep('<' + kind + ' class="np-att" controls data-att="' + esc(name) + '"></' + kind + '>');
      }
      if (kind === 'pdf') {
        return keep('<a class="np-attlink" href="#" data-att-open="' + esc(name) + '">📎 ' + esc(name) + '</a>');
      }
      var note = opts.findNote ? opts.findNote(name) : null;
      if (!note) return keep('<span class="np-missing">![[' + esc(name) + ']] (no such note)</span>');
      if (depth >= 2 || seen.indexOf(note.id) >= 0) {
        return keep('<span class="np-missing">![[' + esc(name) + ']] (embed stops here: ' + (depth >= 2 ? 'too deep' : 'it embeds itself') + ')</span>');
      }
      var inside = renderMarkdown(note.content, { findNote: opts.findNote, depth: depth + 1, seen: seen.concat([note.id]) });
      return keep('<div class="np-embed"><div class="np-embed-title">' + esc(note.title) + '</div>' + inside + '</div>');
    });

    // 4. escape everything else, then block + inline rules
    function inline(s) {
      s = esc(s);
      s = s.replace(/\[\[([^\]\n]+)\]\]/g, function (m, t) {
        var target = t.split('|')[0].split('#')[0].trim();
        var label = t.indexOf('|') >= 0 ? t.slice(t.indexOf('|') + 1) : t.split('#')[0];
        return '<a class="np-link" href="#" data-note="' + target + '">' + label + '</a>';
      });
      s = s.replace(/\[([^\]\n]+)\]\((https?:\/\/[^\s)]+|mailto:[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>');
      s = s.replace(/\*\*([^*\n]+)\*\*/g, '<strong>$1</strong>');
      s = s.replace(/(^|[^*\w])\*([^*\s][^*\n]*?)\*(?!\w)/g, '$1<em>$2</em>');
      s = s.replace(/~~([^~\n]+)~~/g, '<del>$1</del>');
      s = s.replace(/(^|[\s(])#([A-Za-z][\w\/-]*)/g, '$1<span class="tag-pill np-tag" data-tag="$2">#$2</span>');
      return s;
    }

    var lines = text.split('\n');
    var out = [];
    var i = 0;
    while (i < lines.length) {
      var line = lines[i];
      var m;
      if (/^\s*$/.test(line)) { i++; continue; }
      if ((m = /^(#{1,6})\s+(.*)$/.exec(line))) {
        out.push('<h' + m[1].length + '>' + inline(m[2]) + '</h' + m[1].length + '>'); i++; continue;
      }
      if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(line)) { out.push('<hr>'); i++; continue; }
      if (/^\s*>/.test(line)) {
        var q = [];
        while (i < lines.length && /^\s*>/.test(lines[i])) { q.push(lines[i].replace(/^\s*>\s?/, '')); i++; }
        out.push('<blockquote>' + inline(q.join('\n')).replace(/\n/g, '<br>') + '</blockquote>'); continue;
      }
      if (/^\s*([-*+]|\d+[.)])\s+/.test(line)) {
        var ordered = /^\s*\d/.test(line);
        var items = [];
        while (i < lines.length && /^\s*([-*+]|\d+[.)])\s+/.test(lines[i]) && /^\s*\d/.test(lines[i]) === ordered) {
          var task = /^\s*[-*+]\s+\[([ xX])\]\s+(.*)$/.exec(lines[i]);
          if (task) items.push('<li>' + (task[1] === ' ' ? '☐ ' : '☑ ') + inline(task[2]) + '</li>');
          else items.push('<li>' + inline(lines[i].replace(/^\s*([-*+]|\d+[.)])\s+/, '')) + '</li>');
          i++;
        }
        out.push((ordered ? '<ol>' : '<ul>') + items.join('') + (ordered ? '</ol>' : '</ul>')); continue;
      }
      var para = [];
      while (i < lines.length && !/^\s*$/.test(lines[i]) && !/^(#{1,6})\s+/.test(lines[i]) && !/^\s*>/.test(lines[i]) && !/^\s*([-*+]|\d+[.)])\s+/.test(lines[i])) {
        para.push(lines[i]); i++;
      }
      if (!para.length) { para.push(lines[i]); i++; }
      out.push('<p>' + inline(para.join('\n')).replace(/\n/g, '<br>') + '</p>');
    }
    return out.join('\n').replace(/<p>\u0000(\d+)\u0000<\/p>/g, function (m, n) { return /^<(pre|div)/.test(stash[+n]) ? '\u0000' + n + '\u0000' : m; }).replace(/\u0000(\d+)\u0000/g, function (m, n) { return stash[+n]; });
  }

  // ------------------------------------------------------------ graph layout
  /* Pure: positions nodes in place (x,y). nodes: [{id}], edges: [{source,target}]. */
  function layoutGraph(nodes, edges, w, h, iterations) {
    var idx = {};
    nodes.forEach(function (n, i) {
      idx[n.id] = i;
      if (typeof n.x !== 'number') {
        var a = (i / Math.max(1, nodes.length)) * Math.PI * 2;
        n.x = w / 2 + Math.cos(a) * w * 0.3 + (Math.random() - 0.5) * 10;
        n.y = h / 2 + Math.sin(a) * h * 0.3 + (Math.random() - 0.5) * 10;
      }
      n.vx = 0; n.vy = 0;
    });
    var es = edges.map(function (e) { return [idx[e.source], idx[e.target]]; }).filter(function (e) { return e[0] != null && e[1] != null; });
    var k = idealLen(nodes.length, w, h);
    for (var it = 0; it < iterations; it++) { stepLayout(nodes, es, w, h, k, 1 - it / iterations); }
    return es;
  }

  function idealLen(n, w, h) { return Math.min(110, Math.sqrt((w * h) / Math.max(1, n)) * 0.8); }

  function stepLayout(nodes, es, w, h, k, temp) {
    var n = nodes.length, i, j;
    for (i = 0; i < n; i++) {
      for (j = i + 1; j < n; j++) {
        var dx = nodes[i].x - nodes[j].x, dy = nodes[i].y - nodes[j].y;
        var d2 = dx * dx + dy * dy;
        if (d2 < 0.01) { dx = Math.random() - 0.5; dy = Math.random() - 0.5; d2 = 0.5; }
        var d = Math.sqrt(d2), f = (k * k) / d;
        nodes[i].vx += (dx / d) * f; nodes[i].vy += (dy / d) * f;
        nodes[j].vx -= (dx / d) * f; nodes[j].vy -= (dy / d) * f;
      }
    }
    es.forEach(function (e) {
      var a = nodes[e[0]], b = nodes[e[1]];
      var dx = b.x - a.x, dy = b.y - a.y, d = Math.sqrt(dx * dx + dy * dy) || 0.1;
      var f = (d * d) / k * 0.08;
      a.vx += (dx / d) * f; a.vy += (dy / d) * f;
      b.vx -= (dx / d) * f; b.vy -= (dy / d) * f;
    });
    var maxStep = 30 * Math.max(0.05, temp);
    nodes.forEach(function (p) {
      if (p.fixed) { p.vx = p.vy = 0; return; }
      p.vx += (w / 2 - p.x) * 0.02; p.vy += (h / 2 - p.y) * 0.02;
      var s = Math.sqrt(p.vx * p.vx + p.vy * p.vy) || 1;
      var m = Math.min(s, maxStep);
      p.x += (p.vx / s) * m; p.y += (p.vy / s) * m;
      p.x = Math.max(12, Math.min(w - 12, p.x)); p.y = Math.max(12, Math.min(h - 12, p.y));
      p.vx = 0; p.vy = 0;
    });
  }

  var api = { renderMarkdown: renderMarkdown, layoutGraph: layoutGraph, esc: esc, mediaKind: mediaKind };
  if (typeof module !== 'undefined' && module.exports) { module.exports = api; }
  if (typeof document === 'undefined') { return; }
  root.NotesPlus = api;

  // ------------------------------------------------------------------- UI
  var notes = [];            // last list from /api/notes
  var panes = [];            // editor panes
  var mode = 'split';        // edit | split | two | graph
  var attCache = {};         // attachment name -> blob URL
  var katexPromise = null;

  function $(id) { return document.getElementById(id); }
  function el(tag, cls, html) { var e = document.createElement(tag); if (cls) e.className = cls; if (html != null) e.innerHTML = html; return e; }
  function jget(url) { return fetch(url).then(function (r) { return r.json(); }); }

  function findNote(name) {
    var l = String(name).toLowerCase();
    for (var i = 0; i < notes.length; i++) { if (notes[i].id === name) return notes[i]; }
    for (var j = 0; j < notes.length; j++) { if (String(notes[j].title).toLowerCase() === l) return notes[j]; }
    return null;
  }

  function ensureKatex() {
    if (root.katex) return Promise.resolve(root.katex);
    if (katexPromise) return katexPromise;
    katexPromise = new Promise(function (resolve, reject) {
      var css = document.createElement('link');
      css.rel = 'stylesheet'; css.href = '/static/katex/katex.min.css';
      document.head.appendChild(css);
      var s = document.createElement('script');
      s.src = '/static/katex/katex.min.js';
      s.onload = function () { resolve(root.katex); };
      s.onerror = function () { katexPromise = null; reject(new Error('KaTeX did not load')); };
      document.head.appendChild(s);
    });
    return katexPromise;
  }

  /* Fill math spans, load attachment blobs, wire note links and tags inside `box`. */
  function hydrate(box) {
    var math = box.querySelectorAll('.np-math[data-tex]');
    if (math.length) {
      ensureKatex().then(function (k) {
        math.forEach(function (m) {
          try {
            k.render(m.getAttribute('data-tex'), m, { displayMode: m.getAttribute('data-display') === '1', throwOnError: false, trust: false, output: 'htmlAndMathml' });
          } catch (e) { m.classList.add('np-math-bad'); }
        });
      }).catch(function () { math.forEach(function (m) { m.title = 'Math could not be drawn (KaTeX missing); showing the source'; }); });
    }
    box.querySelectorAll('[data-att]').forEach(function (m) {
      attachmentUrl(m.getAttribute('data-att')).then(function (u) { m.src = u; }).catch(function () {
        var s = el('span', 'np-missing', esc('missing attachment: ' + m.getAttribute('data-att')));
        if (m.parentNode) m.parentNode.replaceChild(s, m);
      });
    });
    box.querySelectorAll('[data-att-open]').forEach(function (a) {
      a.onclick = function (ev) {
        ev.preventDefault();
        attachmentUrl(a.getAttribute('data-att-open')).then(function (u) { window.open(u, '_blank', 'noopener'); }).catch(function () { a.textContent = 'missing attachment'; });
      };
    });
    box.querySelectorAll('a.np-link').forEach(function (a) {
      a.onclick = function (ev) {
        ev.preventDefault();
        var n = findNote(a.getAttribute('data-note'));
        if (n) openNote(n.id, ev.shiftKey ? 1 : 0); else a.classList.add('np-missing');
      };
    });
    box.querySelectorAll('.np-tag').forEach(function (t) {
      t.style.cursor = 'pointer';
      t.onclick = function () { $('npQ').value = '#' + t.getAttribute('data-tag'); search(); };
    });
  }

  function attachmentUrl(name) {
    if (attCache[name]) return Promise.resolve(attCache[name]);
    return fetch('/api/attachments/' + encodeURIComponent(name)).then(function (r) {
      if (!r.ok) throw new Error('HTTP ' + r.status);
      return r.blob();
    }).then(function (b) { attCache[name] = URL.createObjectURL(b); return attCache[name]; });
  }

  // ----- panes
  function makePane(slot) {
    var p = { slot: slot, id: null };
    p.root = el('div', 'np-pane');
    p.pick = el('select', 'np-pick');
    p.title = el('input'); p.title.type = 'text'; p.title.placeholder = 'Note title';
    p.text = el('textarea', 'np-text'); p.text.placeholder = 'Markdown. [[link]], #tag, $math$, ![[image.png]], ![[Other note]]';
    p.view = el('div', 'np-view');
    p.status = el('span', 'np-status');
    var bar = el('div', 'np-bar');
    var save = el('button', 'btn-mini', 'Save'); save.onclick = function () { savePane(p); };
    var fresh = el('button', 'btn-mini', 'New'); fresh.onclick = function () { loadInto(p, null); };
    var attach = el('button', 'btn-mini', 'Attach'); var file = el('input'); file.type = 'file'; file.style.display = 'none';
    attach.onclick = function () { file.click(); };
    file.onchange = function () { if (file.files[0]) uploadInto(p, file.files[0]); file.value = ''; };
    bar.append(p.pick, save, fresh, attach, file, p.status);
    var ed = el('div', 'np-ed'); ed.append(p.title, p.text);
    p.ed = ed;
    p.root.append(bar, ed, p.view);
    p.pick.onchange = function () { loadInto(p, p.pick.value || null); };
    var t = null;
    p.text.oninput = function () { clearTimeout(t); t = setTimeout(function () { preview(p); }, 120); };
    return p;
  }

  function fillPick(p) {
    p.pick.innerHTML = '<option value="">(new note)</option>' + notes.map(function (n) {
      return '<option value="' + esc(n.id) + '">' + esc(n.title) + '</option>';
    }).join('');
    p.pick.value = p.id || '';
  }

  function preview(p) {
    p.view.innerHTML = renderMarkdown(p.text.value, { findNote: findNote });
    hydrate(p.view);
  }

  function loadInto(p, id) {
    var n = id ? findNote(id) : null;
    p.id = n ? n.id : null;
    p.title.value = n ? n.title : '';
    p.text.value = n ? n.content : '';
    p.status.textContent = '';
    fillPick(p);
    preview(p);
  }

  function savePane(p) {
    var body = { title: p.title.value.trim() || 'Untitled note', content: p.text.value };
    if (p.id) body.id = p.id;
    p.status.textContent = 'saving...';
    fetch('/api/notes', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) })
      .then(function (r) { return r.json(); })
      .then(function (d) {
        if (!d.success) { p.status.textContent = 'save FAILED'; return; }
        p.id = d.id; p.status.textContent = 'saved';
        return refreshNotes().then(function () { panes.forEach(fillPick); if (typeof root.loadNotes === 'function') root.loadNotes(); });
      }).catch(function (e) { p.status.textContent = 'save FAILED: ' + e; });
  }

  function uploadInto(p, f) {
    var name = f.name.replace(/[^A-Za-z0-9._ ()-]/g, '_');
    p.status.textContent = 'uploading...';
    fetch('/api/attachments?name=' + encodeURIComponent(name), { method: 'POST', body: f })
      .then(function (r) { return r.json(); })
      .then(function (d) {
        if (!d.ok) { p.status.textContent = 'upload refused: ' + d.error; return; }
        var at = p.text.selectionStart == null ? p.text.value.length : p.text.selectionStart;
        p.text.value = p.text.value.slice(0, at) + d.embed + p.text.value.slice(at);
        p.status.textContent = 'attached ' + d.name;
        preview(p);
      }).catch(function (e) { p.status.textContent = 'upload failed: ' + e; });
  }

  function openNote(id, slot) {
    if (mode === 'graph') setMode('split');
    if (slot === 1 && mode !== 'two') setMode('two');
    loadInto(panes[slot || 0], id);
    var w = $('npWork'); if (w && w.scrollIntoView) w.scrollIntoView({ block: 'nearest' });
  }

  function refreshNotes() {
    return jget('/api/notes').then(function (d) { notes = d.notes || []; });
  }

  // ----- search
  function search() {
    var q = $('npQ').value.trim();
    var box = $('npResults'), badge = $('npMode');
    if (!q) { box.innerHTML = ''; badge.textContent = ''; return; }
    jget('/api/notes?q=' + encodeURIComponent(q)).then(function (d) {
      badge.textContent = d.semantic ? 'hybrid: keywords + meaning' : 'keyword only (no embedding endpoint answered)';
      badge.className = 'np-badge ' + (d.semantic ? 'on' : 'off');
      badge.title = d.note || '';
      if (!d.notes.length) { box.innerHTML = '<div class="np-empty">No notes match.</div>'; return; }
      box.innerHTML = '';
      d.notes.forEach(function (n) {
        var r = el('div', 'np-hit');
        var how = (n.keyword_rank ? 'kw#' + n.keyword_rank : '') + (n.keyword_rank && n.semantic_rank ? ' ' : '') + (n.semantic_rank ? 'sem#' + n.semantic_rank : '');
        r.innerHTML = '<b>' + esc(n.title) + '</b> <span class="np-how">' + esc(how) + '</span><div class="np-snip">' + esc(String(n.content).slice(0, 140)) + '</div>';
        r.onclick = function () { openNote(n.id, 0); };
        box.appendChild(r);
      });
    });
  }

  // ----- graph
  var G = null;
  function drawGraph() {
    var cv = $('npCanvas'); if (!cv) return;
    var dpr = window.devicePixelRatio || 1;
    var w = cv.clientWidth, h = cv.clientHeight;
    if (cv.width !== Math.round(w * dpr)) { cv.width = Math.round(w * dpr); cv.height = Math.round(h * dpr); }
    var c = cv.getContext('2d');
    c.setTransform(dpr, 0, 0, dpr, 0, 0);
    c.clearRect(0, 0, w, h);
    if (!G) return;
    var css = getComputedStyle(document.body);
    var col = { note: (css.getPropertyValue('--accent') || '#38bdf8').trim(), tag: (css.getPropertyValue('--brass') || '#d97706').trim(), miss: (css.getPropertyValue('--rose') || '#f43f5e').trim(), text: (css.getPropertyValue('--text-sub') || '#94a3b8').trim() };
    G.es.forEach(function (e) {
      var a = G.nodes[e[0]], b = G.nodes[e[1]], tagEdge = e[2] === 'tag';
      c.beginPath(); c.moveTo(a.x, a.y); c.lineTo(b.x, b.y);
      c.strokeStyle = tagEdge ? 'rgba(217,119,6,0.35)' : 'rgba(148,163,184,0.55)';
      c.setLineDash(tagEdge ? [3, 4] : []); c.lineWidth = 1; c.stroke();
    });
    c.setLineDash([]);
    c.font = '11px sans-serif'; c.textBaseline = 'middle';
    G.nodes.forEach(function (n) {
      var r = 4 + Math.sqrt(n.degree || 0) * 1.6;
      n.r = r;
      c.beginPath();
      if (n.kind === 'tag') { c.rect(n.x - r, n.y - r, r * 2, r * 2); c.fillStyle = col.tag; c.fill(); }
      else if (n.kind === 'missing') { c.arc(n.x, n.y, r, 0, 7); c.strokeStyle = col.miss; c.lineWidth = 1.5; c.stroke(); }
      else { c.arc(n.x, n.y, r, 0, 7); c.fillStyle = col.note; c.fill(); }
      if (n === G.hover || (n.degree || 0) >= 2 || G.nodes.length <= 40) {
        c.fillStyle = n === G.hover ? '#fff' : col.text;
        c.fillText(n.title.length > 28 ? n.title.slice(0, 27) + '…' : n.title, n.x + r + 3, n.y);
      }
    });
  }

  function nodeAt(x, y) {
    if (!G) return null;
    var best = null, bd = 1e9;
    G.nodes.forEach(function (n) {
      var d = Math.hypot(n.x - x, n.y - y);
      if (d <= (n.r || 6) + 5 && d < bd) { best = n; bd = d; }
    });
    return best;
  }

  function loadGraph() {
    var cv = $('npCanvas'), info = $('npGraphInfo');
    jget('/api/notes/graph').then(function (g) {
      var nodes = g.nodes.slice();
      var note = '';
      if (nodes.length > 500) {
        nodes = nodes.filter(function (n) { return n.degree > 0; }).sort(function (a, b) { return b.degree - a.degree; }).slice(0, 500);
        note = ' (showing the 500 best-connected)';
      }
      var ids = {}; nodes.forEach(function (n, i) { ids[n.id] = i; });
      var w = cv.clientWidth || 600, h = cv.clientHeight || 420;
      layoutGraph(nodes, [], w, h, 0);
      var es = g.edges.filter(function (e) { return ids[e.source] != null && ids[e.target] != null; }).map(function (e) { return [ids[e.source], ids[e.target], e.kind]; });
      G = { nodes: nodes, es: es, hover: null };
      info.textContent = g.notes + ' notes, ' + g.tags + ' tags, ' + g.missing + ' missing links, ' + g.edges.length + ' connections' + note + '. Click a note to open it (Shift: second pane). Drag to move.';
      var k = idealLen(nodes.length, w, h), frame = 0, running = false;
      function run() {
        if (running) return;
        running = true;
        (function tick() {
          if (mode !== 'graph' || G === null || G.nodes !== nodes) { running = false; return; }
          for (var s = 0; s < 3; s++) stepLayout(nodes, es, w, h, k, 1 - Math.min(1, frame / 240));
          frame += 3; drawGraph();
          if (frame < 300 || G.drag) requestAnimationFrame(tick); else running = false;
        })();
      }
      G.restart = function () { frame = Math.min(frame, 200); run(); };
      run();
    }).catch(function (e) { info.textContent = 'Graph failed to load: ' + e; });
  }

  function wireGraph() {
    var cv = $('npCanvas');
    function pos(ev) { var r = cv.getBoundingClientRect(); return [ev.clientX - r.left, ev.clientY - r.top]; }
    var down = null;
    cv.addEventListener('pointerdown', function (ev) {
      var p = pos(ev), n = nodeAt(p[0], p[1]);
      down = { x: p[0], y: p[1], n: n, moved: false };
      if (n) { n.fixed = true; G.drag = n; cv.setPointerCapture(ev.pointerId); G.restart(); }
    });
    cv.addEventListener('pointermove', function (ev) {
      var p = pos(ev);
      if (down && down.n) {
        if (Math.hypot(p[0] - down.x, p[1] - down.y) > 4) down.moved = true;
        down.n.x = p[0]; down.n.y = p[1];
      } else if (G) {
        var h = nodeAt(p[0], p[1]);
        if (h !== G.hover) { G.hover = h; cv.style.cursor = h ? 'pointer' : 'default'; drawGraph(); }
      }
    });
    function up(ev) {
      if (!down) return;
      var d = down; down = null;
      if (d.n) { d.n.fixed = false; G.drag = null; }
      if (d.n && !d.moved) {
        if (d.n.kind === 'note') openNote(d.n.id, ev.shiftKey ? 1 : 0);
        else if (d.n.kind === 'tag') { $('npQ').value = d.n.title; setMode('split'); search(); }
      }
    }
    cv.addEventListener('pointerup', up);
    cv.addEventListener('pointercancel', function () { if (down && down.n) { down.n.fixed = false; G.drag = null; } down = null; });
  }

  // ----- layout modes
  function setMode(m) {
    mode = m;
    var w = $('npWork'), gr = $('npGraphBox');
    w.className = 'np-work np-mode-' + (m === 'graph' ? 'split' : m);
    gr.style.display = m === 'graph' ? 'block' : 'none';
    w.style.display = m === 'graph' ? 'none' : 'grid';
    panes[1].root.style.display = m === 'two' ? '' : 'none';
    document.querySelectorAll('#notesPlus .np-modebtn').forEach(function (b) { b.classList.toggle('active', b.getAttribute('data-mode') === m); });
    if (m === 'graph') { loadGraph(); }
    else { panes.forEach(preview); }
  }

  function mount() {
    var host = $('notesPlus');
    if (!host) return;
    host.innerHTML =
      '<div class="card np-card">' +
      '<div class="card-header"><span class="card-title">🗂️ Notes workspace</span><span id="npMode" class="np-badge"></span></div>' +
      '<div class="np-row"><input type="text" id="npQ" placeholder="Search notes (keywords; meaning too when an embedding endpoint is set)"><button class="btn-mini" id="npGo">Search</button></div>' +
      '<div id="npResults" class="np-results"></div>' +
      '<div class="np-modes">' +
      '<button class="btn-mini np-modebtn" data-mode="edit">Edit</button>' +
      '<button class="btn-mini np-modebtn" data-mode="split">Edit + preview</button>' +
      '<button class="btn-mini np-modebtn" data-mode="two">Two notes</button>' +
      '<button class="btn-mini np-modebtn" data-mode="graph">Graph</button></div>' +
      '<div id="npWork" class="np-work"></div>' +
      '<div id="npGraphBox" style="display:none"><div id="npGraphInfo" class="np-empty"></div><canvas id="npCanvas" class="np-canvas"></canvas></div>' +
      '</div>';
    panes = [makePane(0), makePane(1)];
    panes.forEach(function (p) { $('npWork').appendChild(p.root); });
    $('npGo').onclick = search;
    $('npQ').onkeydown = function (e) { if (e.key === 'Enter') search(); };
    host.querySelectorAll('.np-modebtn').forEach(function (b) { b.onclick = function () { setMode(b.getAttribute('data-mode')); }; });
    wireGraph();
    refreshNotes().then(function () { panes.forEach(function (p) { loadInto(p, null); }); setMode('split'); });
  }

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', mount); else mount();
})(typeof window !== 'undefined' ? window : globalThis);
