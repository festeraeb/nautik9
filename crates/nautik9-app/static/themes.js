/* NautiK9 themes: dark (the original cockpit), lakes (Great Lakes sailing + wreck diving),
   dead (Deadhead tie-dye). A theme is a CSS custom-property set (themes.css) plus an
   optional SVG illustration layer built here. All artwork is drawn for NautiK9 (CSS + SVG,
   no images, no network). Mack is the owner's dog, a rust-coloured Vizsla. */
(function () {
  'use strict';
  var IDS = ['dark', 'lakes', 'dead'];
  var NAMES = { dark: 'Cockpit dark', lakes: 'Great Lakes', dead: 'Deadhead' };
  var LS = 'nautik9_theme', LSP = 'nautik9_plain', LSM = 'nautik9_motion';
  function norm(t) { if (t === 'sail') return 'lakes'; return IDS.indexOf(t) >= 0 ? t : 'dark'; }
  function lsGet(k) { try { return localStorage.getItem(k); } catch (e) { return null; } }
  function lsSet(k, v) { try { localStorage.setItem(k, v); } catch (e) {} }

  /* ---------- Mack, drawn as a cartoon Vizsla. Origin = his paws, facing the viewer. ---------- */
  function mack(o) {
    o = o || {};
    var helmet = o.helmet, pack = o.pack, flower = o.flower, shirt = o.shirt, arms = o.arms;
    var s = '<g class="n9-mack">';
    if (pack) {
      s += '<g transform="translate(-17,-24)"><rect x="-4" y="0" width="8" height="15" rx="4" fill="#5fd4e8" stroke="#0b4a60" stroke-width="1"/>' +
        '<rect x="-4" y="9" width="8" height="2.4" fill="#ff7a1a"/>' +
        '<path class="n9-flame" d="M-3 15 L0 25 L3 15 Z" fill="#ffb02e"/><path class="n9-flame n9-flame2" d="M-1.6 15 L0 21 L1.6 15 Z" fill="#fff2c2"/></g>';
    }
    s += '<path class="n9-wag" d="M9 -9 Q 20 -14 19 -25" stroke="#b8683a" stroke-width="4.4" fill="none" stroke-linecap="round"/>';
    s += '<ellipse cx="0" cy="-10" rx="10" ry="11" fill="#b8683a"/>';
    s += '<ellipse cx="0" cy="-8" rx="6" ry="7.4" fill="#c98351"/>';
    if (shirt) {
      s += '<path d="M-10 -14 Q0 -20 10 -14 L11 -3 Q0 1 -11 -3 Z" fill="url(#n9dyeShirt)" stroke="#2a0a3a" stroke-width=".8"/>';
    }
    if (arms) {
      s += '<path class="n9-armL" d="M-9 -12 Q-17 -19 -15 -29" stroke="#b8683a" stroke-width="4" fill="none" stroke-linecap="round"/>' +
           '<path class="n9-armR" d="M9 -12 Q17 -19 15 -29" stroke="#b8683a" stroke-width="4" fill="none" stroke-linecap="round"/>';
    } else {
      s += '<rect x="-7.5" y="-8" width="4.6" height="9" rx="2.3" fill="#b8683a"/><rect x="2.9" y="-8" width="4.6" height="9" rx="2.3" fill="#b8683a"/>' +
           '<ellipse cx="-5.2" cy="0.4" rx="3.6" ry="1.9" fill="#c98351"/><ellipse cx="5.2" cy="0.4" rx="3.6" ry="1.9" fill="#c98351"/>';
    }
    /* collar, orange like the photo */
    s += '<path d="M-8 -19 Q0 -15.5 8 -19 L8 -16.4 Q0 -13 -8 -16.4 Z" fill="#ff7a1a"/><circle cx="0" cy="-13.6" r="1.8" fill="#ffd04a" stroke="#8a5a00" stroke-width=".4"/>';
    /* head */
    s += '<g class="n9-head"><ellipse cx="0" cy="-29" rx="11.5" ry="11" fill="#b8683a"/>' +
      '<path d="M-11 -34 Q-21 -31 -18.6 -14 Q-13.4 -19 -9.6 -28 Z" fill="#8a4a2b"/>' +
      '<path d="M11 -34 Q21 -31 18.6 -14 Q13.4 -19 9.6 -28 Z" fill="#8a4a2b"/>' +
      '<ellipse cx="0" cy="-24.4" rx="6.6" ry="5.4" fill="#c98351"/>' +
      '<path d="M-3 -35 Q0 -31 3 -35" stroke="#9a5630" stroke-width="1.2" fill="none"/>' +
      '<ellipse cx="0" cy="-26.6" rx="2.9" ry="2.1" fill="#6b4752"/>' +
      '<ellipse cx="-0.8" cy="-27.2" rx="1" ry=".6" fill="#a98592"/>' +
      '<g class="n9-blink"><circle cx="-4.6" cy="-30.4" r="2.5" fill="#f8efe0"/><circle cx="4.6" cy="-30.4" r="2.5" fill="#f8efe0"/>' +
      '<circle cx="-4.4" cy="-30.2" r="1.7" fill="#d9922e"/><circle cx="4.8" cy="-30.2" r="1.7" fill="#d9922e"/>' +
      '<circle cx="-4.3" cy="-30.2" r=".85" fill="#1a0f0a"/><circle cx="4.9" cy="-30.2" r=".85" fill="#1a0f0a"/>' +
      '<circle cx="-4.9" cy="-30.9" r=".5" fill="#fff"/><circle cx="4.3" cy="-30.9" r=".5" fill="#fff"/></g>' +
      '<path d="M-4.4 -21.4 Q0 -18.6 4.4 -21.4" stroke="#4a2a1c" stroke-width=".9" fill="none"/>' +
      '<path d="M-1.6 -20.4 Q0 -17 1.8 -20.4 Z" fill="#ff6b8a"/>';
    if (flower) s += '<g transform="translate(-9,-37)"><circle r="3.4" fill="#ff4fa3"/><circle r="1.4" fill="#ffe14a"/><circle cx="-3" cy="-1" r="2" fill="#ff7bbf"/><circle cx="3" cy="-1" r="2" fill="#ff7bbf"/><circle cy="3" r="2" fill="#ff7bbf"/></g>';
    s += '</g>';
    if (helmet) {
      s += '<circle cx="0" cy="-29" r="16.6" fill="rgba(190,240,255,.16)" stroke="#bfefff" stroke-width="1.3"/>' +
           '<path d="M-10 -39 Q-4 -45 4 -43" stroke="#fff" stroke-width="1.6" fill="none" stroke-linecap="round" opacity=".8"/>' +
           '<path d="M9 -40 Q14 -36 15 -30" stroke="#fff" stroke-width="1.1" fill="none" stroke-linecap="round" opacity=".5"/>' +
           '<path d="M-14 -24 Q-15 -33 -9 -39 L-6 -35 Q-10 -30 -10 -24 Z" fill="#fff" opacity=".18"/>' +
           '<path d="M-12 -21 Q0 -15 12 -21" stroke="#ffd18a" stroke-width="1" fill="none" opacity=".55"/>' +
           '<circle cx="6" cy="-42" r=".9" fill="#fff4c9"/>' +
           '<ellipse cx="0" cy="-17" rx="12.6" ry="3.2" fill="#9fb4bd" stroke="#e6f3f7" stroke-width=".8"/>' +
           '<circle cx="-8" cy="-17" r=".9" fill="#ff7a1a"/><circle cx="8" cy="-17" r=".9" fill="#ff7a1a"/>';
    }
    return s + '</g>';
  }

  function spiral(cx, cy, turns, rmax, cls) {
    var pts = [], n = turns * 40;
    for (var i = 0; i <= n; i++) {
      var a = i / 40 * 2 * Math.PI, r = rmax * i / n;
      pts.push((cx + r * Math.cos(a)).toFixed(1) + ' ' + (cy + r * Math.sin(a)).toFixed(1));
    }
    return '<polyline class="' + (cls || '') + '" points="' + pts.join(' ') + '" fill="none"/>';
  }

  function shade(hex, f) { /* f<0 darker, f>0 lighter */
    var n = parseInt(hex.slice(1), 16), r = n >> 16, g = (n >> 8) & 255, b = n & 255;
    function m(c) { return Math.max(0, Math.min(255, Math.round(f < 0 ? c * (1 + f) : c + (255 - c) * f))); }
    return '#' + ((1 << 24) + (m(r) << 16) + (m(g) << 8) + m(b)).toString(16).slice(1);
  }
  function fur(cx, cy, rx, ry, seed, n, amp) { /* a tufted outline: tips and valleys, deterministic */
    var d = '';
    for (var i = 0; i < n * 2; i++) {
      var a = i / (n * 2) * 2 * Math.PI, tip = i % 2 === 0;
      var j = Math.sin(seed * 12.9 + i * 7.3) * .5 + .5;
      var k = tip ? 1 + amp * (.6 + .6 * j) : 1 - amp * .35;
      var x = cx + rx * k * Math.cos(a), y = cy + ry * k * Math.sin(a);
      d += (i ? 'L' : 'M') + x.toFixed(1) + ' ' + y.toFixed(1);
    }
    return d + 'Z';
  }
  function furStrokes(cx, cy, rx, ry, seed, count, col) {
    var o = '';
    for (var i = 0; i < count; i++) {
      var a = (i / count) * 2 * Math.PI + seed, rr = .45 + .4 * (Math.sin(seed * 5 + i * 3.1) * .5 + .5);
      var x = cx + rx * rr * Math.cos(a), y = cy + ry * rr * Math.sin(a), l = 2.2;
      o += 'M' + x.toFixed(1) + ' ' + y.toFixed(1) + ' q' + (Math.cos(a) * l).toFixed(1) + ' ' + (Math.sin(a) * l + 1).toFixed(1) + ' ' + (Math.cos(a) * l * 1.3).toFixed(1) + ' ' + (Math.sin(a) * l * 1.6 + 1.5).toFixed(1) + ' ';
    }
    return '<path d="' + o + '" stroke="' + col + '" stroke-width=".7" fill="none" stroke-linecap="round" opacity=".55"/>';
  }

  function rose(x, y, sc, cls) {
    /* layered petals around a spiral heart, two leaves with veins, a thorned stem */
    var petals = '', cols = ['#a80f35', '#c4153f', '#d81b4a', '#ec3a63'], i;
    for (i = 0; i < 7; i++) {
      petals += '<path transform="rotate(' + (i * 51.4) + ')" d="M0 0 C-9 -3 -11 -12 -3 -14 C3 -15 8 -11 6 -4 Z" fill="' + cols[i % 2] + '" stroke="#6e0a22" stroke-width=".5"/>';
    }
    for (i = 0; i < 5; i++) {
      petals += '<path transform="rotate(' + (i * 72 + 20) + ')" d="M0 0 C-6 -2 -7 -8 -2 -9 C2 -10 5 -7 3.4 -3 Z" fill="' + cols[2 + (i % 2)] + '" stroke="#8a0f2c" stroke-width=".4"/>';
    }
    return '<g transform="translate(' + x + ',' + y + ') scale(' + sc + ')"><g class="' + (cls || '') + '">' +
      '<path d="M0 8 Q-5 20 -2 34" stroke="#17783f" stroke-width="2.4" fill="none" stroke-linecap="round"/>' +
      '<path d="M-3 17 l-3 -1.4 M-1.4 26 l3 -1.6" stroke="#0e4f29" stroke-width="1.3" stroke-linecap="round"/>' +
      '<path d="M-3 22 Q-15 13 -17 22 Q-9 28 -3 22 Z" fill="#23b45f" stroke="#0e4f29" stroke-width=".6"/><path d="M-3 22 Q-10 19 -15 21" stroke="#0e4f29" stroke-width=".5" fill="none"/>' +
      '<path d="M-2 28 Q9 20 13 26 Q7 32 -2 28 Z" fill="#2fcf70" stroke="#0e4f29" stroke-width=".6"/><path d="M-2 28 Q5 24 11 26" stroke="#0e4f29" stroke-width=".5" fill="none"/>' +
      '<circle r="11.5" fill="#6e0a22" opacity=".35" cy="1.2"/>' + petals +
      '<path d="M0 0 m-1 0 a1 1 0 1 1 2 0 a2.4 2.4 0 1 1 -4.4 0 a3.8 3.8 0 1 1 7 0 a5.2 5.2 0 1 1 -9.6 0" stroke="#ffb3c6" stroke-width=".9" fill="none" opacity=".85"/>' +
      '<path d="M-6 -9 Q-1 -13 4 -10" stroke="#ff9fb6" stroke-width="1" fill="none" opacity=".7" stroke-linecap="round"/>' +
      '</g></g>';
  }

  function bolt(x, y, sc, cls) {
    return '<g transform="translate(' + x + ',' + y + ') scale(' + sc + ')"><g class="' + (cls || '') + '">' +
      '<polygon points="0,0 14,0 6,16 18,16 -4,46 2,24 -8,24" fill="#ffe14a" stroke="#ff4fa3" stroke-width="2.4" stroke-linejoin="round"/></g></g>';
  }

  function skull(x, y, sc, cls) {
    /* an original round, grinning skull with depth (shaded bone, glowing sockets, a rose crown); not a licensed mark */
    var teeth = '', i;
    for (i = 0; i < 6; i++) teeth += '<rect x="' + (-12 + i * 4.2) + '" y="19" width="3.6" height="8" rx="1.2" fill="url(#skTooth)" stroke="#3a1a40" stroke-width=".8"/>';
    return '<g transform="translate(' + x + ',' + y + ') scale(' + sc + ')"><g class="' + (cls || '') + '">' +
      '<defs><radialGradient id="skBone" cx=".38" cy=".3" r=".85"><stop offset="0" stop-color="#fffaf0"/><stop offset=".55" stop-color="#f0dcc4"/><stop offset="1" stop-color="#b79a86"/></radialGradient>' +
      '<radialGradient id="skEye" cx=".5" cy=".55" r=".6"><stop offset="0" stop-color="#7a1a8c"/><stop offset=".6" stop-color="#2a0a3a"/><stop offset="1" stop-color="#12031c"/></radialGradient>' +
      '<linearGradient id="skTooth" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff6ea"/><stop offset="1" stop-color="#cdb59f"/></linearGradient></defs>' +
      '<ellipse cx="2" cy="32" rx="22" ry="4" fill="#12031c" opacity=".45"/>' +
      '<path d="M-24 4 Q-26 -26 0 -28 Q26 -26 24 4 Q24 14 14 18 L12 29 H-12 L-14 18 Q-24 14 -24 4 Z" fill="url(#skBone)" stroke="#2a0a3a" stroke-width="2"/>' +
      '<path d="M-23 6 Q-14 16 -14 18 L-12 29" fill="none" stroke="#9a7c68" stroke-width="2" opacity=".5"/>' +
      '<path d="M16 -20 Q24 -14 23 2" stroke="#fff" stroke-width="1.6" fill="none" opacity=".55" stroke-linecap="round"/>' +
      '<ellipse cx="-10" cy="3" rx="7.6" ry="8.2" fill="url(#skEye)" stroke="#2a0a3a" stroke-width="1.2"/><ellipse cx="10" cy="3" rx="7.6" ry="8.2" fill="url(#skEye)" stroke="#2a0a3a" stroke-width="1.2"/>' +
      '<circle cx="-12" cy="0.4" r="1.5" fill="#ff7bbf" opacity=".9"/><circle cx="8" cy="0.4" r="1.5" fill="#ff7bbf" opacity=".9"/>' +
      '<path d="M0 10 C-4 12 -4.4 18 -2.6 18 L2.6 18 C4.4 18 4 12 0 10 Z" fill="#2a0a3a"/>' +
      '<path d="M-20 12 Q-16 15 -13 14 M20 12 Q16 15 13 14" stroke="#9a7c68" stroke-width="1.2" fill="none" opacity=".6"/>' +
      '<path d="M-13 19 H13" stroke="#2a0a3a" stroke-width="1.4"/>' + teeth +
      '<path d="M4 -24 L-2 -14 L3 -14 L-4 -3" stroke="#ff4fa3" stroke-width="2" fill="none" stroke-linejoin="round" opacity=".9"/>' +
      '<path d="M-6 -20 q-2 6 1 9" stroke="#8a6a58" stroke-width=".8" fill="none" opacity=".6"/>' +
      '<g transform="translate(-23,-21) scale(.62)"><g class="n9-sway">' + roseHead() + '</g></g>' +
      '<g transform="translate(0,-31) scale(.8)">' + roseHead() + '</g>' +
      '<g transform="translate(23,-21) scale(.62)"><g class="n9-sway n9-sway2">' + roseHead() + '</g></g>' +
      '<path d="M-31 -16 q-8 4 -3 11 M31 -16 q8 4 3 11 M-14 -29 q4 -2 8 -1 M14 -29 q-4 -2 -8 -1" stroke="#23b45f" stroke-width="3" fill="none" stroke-linecap="round"/>' +
      '</g></g>';
  }
  function roseHead() {
    var p = '', i;
    for (i = 0; i < 6; i++) p += '<path transform="rotate(' + (i * 60) + ')" d="M0 0 C-8 -2 -10 -11 -3 -13 C3 -14 7 -10 5 -3 Z" fill="' + (i % 2 ? '#c4153f' : '#e02a55') + '" stroke="#6e0a22" stroke-width=".6"/>';
    return p + '<path d="M0 0 a2 2 0 1 1 3 2 a4 4 0 1 1 -7 -1" stroke="#ffb3c6" stroke-width="1" fill="none"/><circle r="1.6" fill="#ffe14a"/>';
  }

  function terrapin(x, y, sc, cls) {
    return '<g transform="translate(' + x + ',' + y + ') scale(' + sc + ')"><g class="' + (cls || '') + '">' +
      '<ellipse cx="-17" cy="-9" rx="5" ry="3" fill="#3dd68c"/><ellipse cx="17" cy="-9" rx="5" ry="3" fill="#3dd68c"/>' +
      '<ellipse cx="-16" cy="9" rx="5" ry="3" fill="#3dd68c"/><ellipse cx="16" cy="9" rx="5" ry="3" fill="#3dd68c"/>' +
      '<circle cx="0" cy="-19" r="6" fill="#3dd68c"/><circle cx="-2.2" cy="-20" r=".9" fill="#1a0614"/><circle cx="2.2" cy="-20" r=".9" fill="#1a0614"/>' +
      '<path d="M3 11 Q10 16 6 20" stroke="#3dd68c" stroke-width="3" fill="none" stroke-linecap="round"/>' +
      '<ellipse cx="0" cy="0" rx="18" ry="15" fill="url(#n9dyeShell)" stroke="#2a0a3a" stroke-width="1.6"/>' +
      '<path d="M-6 -8 H6 L10 0 L6 8 H-6 L-10 0 Z M-18 0 H-10 M18 0 H10 M-6 -8 L-12 -12 M6 -8 L12 -12 M-6 8 L-12 12 M6 8 L12 12" stroke="#2a0a3a" stroke-width="1.2" fill="none"/></g></g>';
  }

  var POSES = [
    { aL: 150, aR: -150, lL: 8, lR: -8, tilt: 0, rot: 0 },
    { aL: 160, aR: -75, lL: 16, lR: -6, tilt: 7, rot: 4 },
    { aL: 105, aR: -135, lL: 34, lR: 0, tilt: -6, rot: 10 },
    { aL: 92, aR: -92, lL: 6, lR: -6, tilt: 0, rot: -10 },
    { aL: 28, aR: -28, lL: 6, lR: -6, tilt: -14, rot: 0 },
    { aL: 172, aR: -48, lL: 12, lR: -12, tilt: 5, rot: -4 }
  ];
  function limb(sx, sy, ang, len, w, col, edge) {
    return '<g transform="rotate(' + ang + ' ' + sx + ' ' + sy + ')"><path d="M' + sx + ' ' + sy + ' l0 ' + len + '" stroke="' + edge + '" stroke-width="' + (w + 1.4) + '" stroke-linecap="round"/><path d="M' + sx + ' ' + sy + ' l0 ' + len + '" stroke="' + col + '" stroke-width="' + w + '" stroke-linecap="round"/>' +
      '<circle cx="' + sx + '" cy="' + (sy + len) + '" r="' + (w * .62) + '" fill="' + shade(col, -.18) + '"/></g>';
  }
  function bear(x, y, sc, fill, cls, dly, pose) {
    /* a furry, round dancing bear drawn for NautiK9: tufted outline, fur strokes, a bandana,
       six postures. Not the classic outlined bears. */
    var P = POSES[(pose || 0) % POSES.length], dk = shade(fill, -.38), lt = shade(fill, .38), seed = (x * .13 + y * .07 + (pose || 0));
    var band = BEAR_COLORS[(BEAR_COLORS.indexOf(fill) + 2 + 6) % 6];
    return '<g class="n9-bear ' + (cls || '') + '" style="animation-delay:' + (dly || 0) + 's" transform="translate(' + x + ',' + y + ') scale(' + sc + ') rotate(' + P.rot + ')">' +
      '<g class="n9-bear-in">' +
      '<ellipse cx="0" cy="25" rx="12" ry="2.6" fill="#12031c" opacity=".4"/>' +
      limb(-5, 11, P.lL, 14, 6, fill, dk) + limb(5, 11, P.lR, 14, 6, fill, dk) +
      limb(-9, -5, P.aL, 14, 5.4, fill, dk) + limb(9, -5, P.aR, 14, 5.4, fill, dk) +
      '<path d="' + fur(0, 2, 11.5, 13.5, seed, 16, .1) + '" fill="' + fill + '" stroke="' + dk + '" stroke-width="1"/>' +
      '<ellipse cx="0" cy="4" rx="6.6" ry="8" fill="' + lt + '" opacity=".6"/>' + furStrokes(0, 2, 11, 13, seed, 14, dk) +
      '<g transform="rotate(' + P.tilt + ' 0 -9)">' +
      '<circle cx="-7.6" cy="-24.4" r="3.8" fill="' + dk + '"/><circle cx="7.6" cy="-24.4" r="3.8" fill="' + dk + '"/><circle cx="-7.6" cy="-24.4" r="2" fill="' + lt + '" opacity=".7"/><circle cx="7.6" cy="-24.4" r="2" fill="' + lt + '" opacity=".7"/>' +
      '<path d="' + fur(0, -17, 9.4, 8.8, seed + 2, 14, .12) + '" fill="' + fill + '" stroke="' + dk + '" stroke-width="1"/>' + furStrokes(0, -17, 9, 8.4, seed + 1, 9, dk) +
      '<ellipse cx="0" cy="-14.4" rx="4.6" ry="3.6" fill="' + lt + '" stroke="' + dk + '" stroke-width=".6"/>' +
      '<ellipse cx="0" cy="-15.8" rx="1.7" ry="1.2" fill="#2a0a3a"/><path d="M0 -14.6 v1.4 M0 -13.2 q-2 1.8 -3.4 .4 M0 -13.2 q2 1.8 3.4 .4" stroke="#2a0a3a" stroke-width=".7" fill="none" stroke-linecap="round"/>' +
      '<circle cx="-3.4" cy="-18.6" r="1.7" fill="#fff"/><circle cx="3.4" cy="-18.6" r="1.7" fill="#fff"/><circle cx="-3.2" cy="-18.4" r="1" fill="#2a0a3a"/><circle cx="3.6" cy="-18.4" r="1" fill="#2a0a3a"/><circle cx="-3.5" cy="-18.9" r=".4" fill="#fff"/><circle cx="3.3" cy="-18.9" r=".4" fill="#fff"/>' +
      '<path d="M-5 -21.4 q2 -1.4 4 -.2 M5 -21.4 q-2 -1.4 -4 -.2" stroke="' + dk + '" stroke-width=".8" fill="none" stroke-linecap="round"/>' +
      '</g>' +
      '<path d="M-6 -9 L6 -9 L0 -2 Z" fill="' + band + '" stroke="' + shade(band, -.4) + '" stroke-width=".6"/><circle cx="0" cy="-7" r=".8" fill="#fff" opacity=".8"/>' +
      '</g></g>';
  }

  /* tie-dye cloth: folded-and-twisted look = radial pleats + rings, warped by turbulence, with fabric grain */
  function dyeCloth(cx, cy, R, id, hues) {
    var rings = '', wedges = '', i;
    for (i = hues.length * 3; i >= 0; i--) {
      rings += '<circle cx="' + cx + '" cy="' + cy + '" r="' + (R * (i + 1) / (hues.length * 3 + 1)).toFixed(1) + '" fill="' + hues[i % hues.length] + '"/>';
    }
    for (i = 0; i < 12; i++) {
      var a = i * 30 * Math.PI / 180, a2 = (i * 30 + 15) * Math.PI / 180;
      wedges += '<path d="M' + cx + ' ' + cy + ' L' + (cx + R * 1.3 * Math.cos(a)).toFixed(1) + ' ' + (cy + R * 1.3 * Math.sin(a)).toFixed(1) + ' L' + (cx + R * 1.3 * Math.cos(a2)).toFixed(1) + ' ' + (cy + R * 1.3 * Math.sin(a2)).toFixed(1) + ' Z" fill="#12031c" opacity=".16"/>';
    }
    return '<g filter="url(#' + id + 'W)">' + rings + wedges + '</g>';
  }
  function dyeFilters(id, scale, freq) {
    return '<filter id="' + id + 'W" x="-20%" y="-20%" width="140%" height="140%"><feTurbulence type="fractalNoise" baseFrequency="' + freq + '" numOctaves="3" seed="7" result="n"/><feDisplacementMap in="SourceGraphic" in2="n" scale="' + scale + '" xChannelSelector="R" yChannelSelector="G"/></filter>' +
      '<filter id="' + id + 'G" x="0" y="0" width="100%" height="100%"><feTurbulence type="fractalNoise" baseFrequency=".9" numOctaves="2" seed="3" result="g"/><feColorMatrix in="g" type="matrix" values="0 0 0 0 .1  0 0 0 0 .02  0 0 0 0 .15  0 0 0 .55 -.18"/></filter>';
  }
  function wordmark(x, y, sc) {
    /* hand-lettered NautiK9: stacked offsets, a wobble filter, rainbow fill */
    var t = function (dx, dy, fill, extra) { return '<text x="' + (x + dx) + '" y="' + (y + dy) + '" text-anchor="middle" font-size="' + (30 * sc) + '" font-weight="900" ' + extra + ' fill="' + fill + '">NautiK9</text>'; };
    return '<g class="n9-word" font-family="\'Chalkboard SE\',\'Marker Felt\',\'Comic Neue\',\'Comic Sans MS\',\'Segoe Print\',casual,cursive" filter="url(#dhWobble)" aria-hidden="true">' +
      t(2.2, 2.6, '#12031c', 'stroke="#12031c" stroke-width="5" stroke-linejoin="round"') +
      t(0, 0, 'url(#dhLetters)', 'stroke="#fff7ec" stroke-width="1.6" stroke-linejoin="round" paint-order="stroke"') + '</g>';
  }

  /* ---------------- GREAT LAKES ---------------- */
  function heroLakes() {
    var rays = '';
    for (var i = 0; i < 12; i++) {
      var a = i * 30 * Math.PI / 180;
      rays += '<polygon points="' + (290 + 13 * Math.cos(a - .09)).toFixed(1) + ',' + (40 + 13 * Math.sin(a - .09)).toFixed(1) + ' ' +
        (290 + (i % 2 ? 26 : 38) * Math.cos(a)).toFixed(1) + ',' + (40 + (i % 2 ? 26 : 38) * Math.sin(a)).toFixed(1) + ' ' +
        (290 + 13 * Math.cos(a + .09)).toFixed(1) + ',' + (40 + 13 * Math.sin(a + .09)).toFixed(1) + '" fill="#ffe9a8"/>';
    }
    return '<svg class="n9-hero-svg" viewBox="0 0 360 200" preserveAspectRatio="xMidYMid slice" role="img" aria-label="Mighty Mack, in a space-age bubble helmet, on the deck of a sailboat over a wreck that a sonar beam is finding">' +
      '<defs>' +
      '<linearGradient id="lkSky" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#1b5b7a"/><stop offset=".6" stop-color="#4cb1bd"/><stop offset="1" stop-color="#ffd18a"/></linearGradient>' +
      '<linearGradient id="lkWater" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#2398b0"/><stop offset=".35" stop-color="#0b4d68"/><stop offset=".75" stop-color="#05283b"/><stop offset="1" stop-color="#020f18"/></linearGradient>' +
      '<linearGradient id="lkShaft" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#e9fbff" stop-opacity=".5"/><stop offset="1" stop-color="#e9fbff" stop-opacity="0"/></linearGradient>' +
      '<filter id="lkCaus" x="0" y="0" width="100%" height="100%"><feTurbulence type="fractalNoise" baseFrequency=".035 .09" numOctaves="2" seed="4"/><feColorMatrix type="matrix" values="0 0 0 0 .85  0 0 0 0 1  0 0 0 0 1  0 0 0 9 -4.7"/></filter>' +
      '<linearGradient id="lkCausFade" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#fff" stop-opacity="0"/></linearGradient><mask id="lkCausMask"><rect y="86" width="360" height="80" fill="url(#lkCausFade)"/></mask>' +
      '<radialGradient id="lkScope" cx=".5" cy=".5" r=".5"><stop offset="0" stop-color="#0b3b2b"/><stop offset="1" stop-color="#03150f"/></radialGradient>' +
      '<linearGradient id="lkSweep" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#5dffb0" stop-opacity="0"/><stop offset="1" stop-color="#5dffb0" stop-opacity=".85"/></linearGradient>' +
      '</defs>' +
      '<rect width="360" height="86" fill="url(#lkSky)"/>' +
      '<g class="n9-sun">' + rays + '<circle cx="290" cy="40" r="11" fill="#fff4c9"/></g>' +
      /* lighthouse and a laker on the horizon */
      '<g transform="translate(330,52)"><polygon points="-5,34 5,34 3,6 -3,6" fill="#f4efe0"/><polygon points="-4.4,26 4.4,26 4,20 -4,20" fill="#e5483a"/><polygon points="-3.6,14 3.6,14 3.2,9 -3.2,9" fill="#e5483a"/><rect x="-4" y="2" width="8" height="5" fill="#ffd04a"/><polygon points="-5,2 5,2 0,-4" fill="#e5483a"/></g>' +
      '<g class="n9-drift"><path d="M170 80 H232 L228 85 H176 Z" fill="#17384a"/><rect x="214" y="72" width="12" height="8" fill="#f4efe0"/><rect x="216" y="68" width="5" height="4" fill="#e5483a"/></g>' +
      /* a space-age sky pod, in the Jetsons spirit */
      '<g class="n9-pod"><ellipse cx="52" cy="26" rx="15" ry="4.6" fill="#e9fbff" stroke="#0b4a60" stroke-width="1"/><path d="M41 25 Q52 9 63 25 Z" fill="rgba(190,240,255,.55)" stroke="#0b4a60" stroke-width="1"/><circle cx="46" cy="27" r="1.2" fill="#ff7a1a"/><circle cx="52" cy="28" r="1.2" fill="#ff7a1a"/><circle cx="58" cy="27" r="1.2" fill="#ff7a1a"/><path d="M36 28 H24 M34 25 H26" stroke="#fff4c9" stroke-width="1.2" stroke-linecap="round" opacity=".7"/></g>' +
      /* water */
      '<rect y="86" width="360" height="114" fill="url(#lkWater)"/>' +
      '<g mask="url(#lkCausMask)"><rect class="n9-caustic" x="-40" y="86" width="440" height="80" filter="url(#lkCaus)" opacity=".34"/></g>' +
      '<g class="n9-shafts"><polygon points="40,86 70,86 130,200 70,200" fill="url(#lkShaft)" opacity=".5"/><polygon points="130,86 150,86 230,200 190,200" fill="url(#lkShaft)" opacity=".4"/><polygon points="250,86 280,86 330,200 270,200" fill="url(#lkShaft)" opacity=".45"/></g>' +
      '<path class="n9-wave" d="M-30 86 Q-10 82 10 86 T50 86 T90 86 T130 86 T170 86 T210 86 T250 86 T290 86 T330 86 T370 86 V92 H-30 Z" fill="#37b6cb" opacity=".7"/>' +
      '<g stroke="#e9fbff" stroke-opacity=".28" stroke-width=".8" fill="none"><path d="M0 112 Q90 106 180 112 T360 112"/><path d="M0 142 Q90 136 180 142 T360 142"/><path d="M0 168 Q90 162 180 168 T360 168"/></g>' +
      '<g font-family="monospace" font-size="6" fill="#bfefff" fill-opacity=".6"><text x="344" y="114" text-anchor="end">10 m</text><text x="344" y="144" text-anchor="end">25 m</text><text x="344" y="170" text-anchor="end">40 m</text></g>' +
      /* sonar beam from the hull and its pings */
      '<g class="n9-beam"><polygon points="131,92 119,186 159,186" fill="#5dffb0" opacity=".16"/>' +
      '<path class="n9-ping" d="M113 112 Q139 124 165 112" stroke="#5dffb0" stroke-width="1.4" fill="none"/>' +
      '<path class="n9-ping n9-ping2" d="M105 112 Q139 130 173 112" stroke="#5dffb0" stroke-width="1.2" fill="none"/>' +
      '<path class="n9-ping n9-ping3" d="M97 112 Q139 136 181 112" stroke="#5dffb0" stroke-width="1" fill="none"/></g>' +
      /* lake bed and the wreck */
      '<path d="M0 186 Q60 180 120 186 T250 184 T360 188 V200 H0 Z" fill="#0a1a22"/>' +
      '<g class="n9-wreck" transform="translate(139,184) rotate(-7)"><path d="M-30 0 L-24 -9 H22 L34 -14 L30 0 Z" fill="#12313e" stroke="#5dffb0" stroke-width="1" stroke-opacity=".8"/>' +
      '<path d="M-20 -9 V0 M-12 -9 V0 M-4 -9 V0 M4 -9 V0 M12 -9 V0 M20 -9 V-1" stroke="#5dffb0" stroke-opacity=".55" stroke-width=".8"/>' +
      '<path d="M-6 -9 L-12 -34 M-12 -34 L-4 -30 M10 -9 L18 -26" stroke="#1c4b5d" stroke-width="2" fill="none"/></g>' +
      '<g class="n9-fish"><path d="M222 140 q8 -5 14 0 q-6 5 -14 0 Z M236 140 l5 -4 v8 z" fill="#e9fbff" opacity=".7"/></g>' +
      '<g class="n9-fish n9-fish2"><path d="M262 160 q7 -4 12 0 q-5 4 -12 0 Z M274 160 l4 -3 v6 z" fill="#ff9a4a" opacity=".8"/></g>' +
      /* sonar scope */
      '<g transform="translate(36,158)"><circle r="23" fill="url(#lkScope)" stroke="#e2b15a" stroke-width="2.2"/>' +
      '<circle r="15" fill="none" stroke="#5dffb0" stroke-opacity=".35"/><circle r="8" fill="none" stroke="#5dffb0" stroke-opacity=".35"/><path d="M-23 0 H23 M0 -23 V23" stroke="#5dffb0" stroke-opacity=".25"/>' +
      '<g class="n9-sweep"><path d="M0 0 L22 0 A22 22 0 0 0 15.6 -15.6 Z" fill="url(#lkSweep)"/></g><circle class="n9-blip" cx="8" cy="-9" r="2" fill="#bfffe0"/></g>' +
      /* sailboat, mid-century modern */
      '<g class="n9-boat"><path d="M68 70 H198 Q216 66 222 60 Q214 87 190 89 H92 Q74 87 68 70 Z" fill="#ff7a2f"/>' +
      '<path d="M72 77 H214" stroke="#fff4c9" stroke-width="2.2"/>' +
      '<polygon points="68,70 54,52 76,66" fill="#e5483a"/>' +
      '<circle cx="96" cy="81" r="3" fill="#ffe9a8" stroke="#0b4a60" stroke-width="1"/><circle cx="110" cy="82" r="3" fill="#ffe9a8" stroke="#0b4a60" stroke-width="1"/><circle cx="124" cy="82.4" r="3" fill="#ffe9a8" stroke="#0b4a60" stroke-width="1"/>' +
      '<line x1="152" y1="70" x2="152" y2="12" stroke="#f4efe0" stroke-width="2.6"/><circle cx="152" cy="10" r="3.2" fill="#ffd04a"/><path d="M146 10 a6 6 0 0 1 12 0 M143 10 a9 9 0 0 1 18 0" stroke="#ffd04a" stroke-width=".8" fill="none" opacity=".7"/>' +
      '<polygon class="n9-sail" points="149,16 149,67 92,67" fill="#fff4c9"/><polygon points="149,40 149,52 112,67 100,67" fill="#ff7a2f" opacity=".85"/>' +
      '<polygon class="n9-sail" points="155,20 155,67 196,67" fill="#5fd4e8"/><path d="M155 34 L186 67" stroke="#fff4c9" stroke-width="1.6"/>' +
      '<path d="M120 67 H200" stroke="#7b5a2a" stroke-width="1.6"/>' +
      '<g transform="translate(184,72) scale(1.4)">' + mack({ helmet: true, pack: true }) + '</g>' +
      '</g>' +
      '</svg>';
  }

  function sceneLakes() {
    var bub = '';
    var xs = [30, 70, 120, 160, 210, 250, 300, 350, 90, 330];
    for (var i = 0; i < xs.length; i++) bub += '<circle class="n9-bubble b' + (i % 5) + '" cx="' + xs[i] + '" cy="780" r="' + (2 + (i % 3)) + '" fill="none" stroke="#bfefff" stroke-opacity=".5"/>';
    return '<svg viewBox="0 0 400 800" preserveAspectRatio="xMidYMax slice" aria-hidden="true">' +
      '<defs><linearGradient id="lsBg" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#0d4a63"/><stop offset=".25" stop-color="#07354a"/><stop offset=".6" stop-color="#041f2e"/><stop offset="1" stop-color="#010a10"/></linearGradient>' +
      '<linearGradient id="lsShaft" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#e9fbff" stop-opacity=".18"/><stop offset="1" stop-color="#e9fbff" stop-opacity="0"/></linearGradient></defs>' +
      '<rect width="400" height="800" fill="url(#lsBg)"/>' +
      '<g class="n9-shafts"><polygon points="20,0 70,0 190,800 60,800" fill="url(#lsShaft)"/><polygon points="170,0 210,0 380,800 280,800" fill="url(#lsShaft)"/><polygon points="300,0 340,0 420,700 330,700" fill="url(#lsShaft)"/></g>' +
      '<g stroke="#bfefff" stroke-opacity=".12" fill="none"><path d="M0 200 Q100 190 200 200 T400 200"/><path d="M0 380 Q100 370 200 380 T400 380"/><path d="M0 560 Q100 550 200 560 T400 560"/></g>' +
      bub +
      '<path d="M0 770 Q80 758 170 770 T330 766 T400 772 V800 H0 Z" fill="#050f15"/>' +
      '<g transform="translate(250,770) rotate(-6)" opacity=".85"><path d="M-70 0 L-56 -20 H50 L78 -30 L70 0 Z" fill="#0b2531" stroke="#5dffb0" stroke-opacity=".45"/><path d="M-30 -20 L-44 -64 M-44 -64 L-26 -56 M20 -20 L38 -52" stroke="#12384a" stroke-width="3" fill="none"/></g>' +
      '</svg>';
  }

  /* ---------------- DEADHEAD ---------------- */
  var BEAR_COLORS = ['#ff4fa3', '#ffe14a', '#3dff9a', '#8a5cff', '#ff8a3d', '#4fd8ff'];


  function dyeDefs() {
    return '<linearGradient id="n9dyeShirt" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#ff4fa3"/><stop offset=".35" stop-color="#ffe14a"/><stop offset=".7" stop-color="#3dff9a"/><stop offset="1" stop-color="#8a5cff"/></linearGradient>' +
      '<radialGradient id="n9dyeShell" cx=".4" cy=".4" r=".8"><stop offset="0" stop-color="#ffe14a"/><stop offset=".45" stop-color="#ff4fa3"/><stop offset="1" stop-color="#6a2cff"/></radialGradient>' +
      '<linearGradient id="dhLetters" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="#ffe14a"/><stop offset=".3" stop-color="#ff7bbf"/><stop offset=".6" stop-color="#3dff9a"/><stop offset="1" stop-color="#4fd8ff"/></linearGradient>' +
      '<filter id="dhWobble" x="-5%" y="-20%" width="110%" height="140%"><feTurbulence type="fractalNoise" baseFrequency=".05" numOctaves="2" seed="11" result="w"/><feDisplacementMap in="SourceGraphic" in2="w" scale="2.4" xChannelSelector="R" yChannelSelector="G"/></filter>' +
      '<radialGradient id="dhVig" cx=".5" cy=".5" r=".75"><stop offset=".55" stop-color="#12031c" stop-opacity="0"/><stop offset="1" stop-color="#12031c" stop-opacity=".75"/></radialGradient>' +
      dyeFilters('dh', 26, '.014') + dyeFilters('ds', 60, '.006');
  }

  function heroDead() {
    var hues = ['#ff4fa3', '#ffe14a', '#3dff9a', '#4fd8ff', '#8a5cff', '#ff8a3d'];
    var bears = '';
    for (var i = 0; i < 6; i++) bears += bear(28 + i * 61, 166, .66, BEAR_COLORS[i], i % 2 ? 'n9-hop2' : '', i * 0.13, i);
    return '<svg class="n9-hero-svg" viewBox="0 0 360 200" preserveAspectRatio="xMidYMid slice" role="img" aria-label="NautiK9 in hand lettering over layered tie-dye, with a rose-crowned skull, lightning bolts, roses, a terrapin and six furry dancing bears with Mack in a tie-dye shirt">' +
      '<defs>' + dyeDefs() + '</defs>' +
      '<rect width="360" height="200" fill="#12031c"/>' +
      '<g class="n9-spin n9-dye-slow" style="transform-origin:290px 50px">' + dyeCloth(290, 50, 120, 'dh', hues) + '</g>' +
      '<g class="n9-spin n9-spin-rev n9-dye-slow" style="transform-origin:50px 130px" opacity=".92">' + dyeCloth(50, 130, 95, 'dh', hues.slice().reverse()) + '</g>' +
      '<g class="n9-dye-slow" opacity=".75">' + dyeCloth(190, 215, 110, 'dh', hues.slice(2).concat(hues.slice(0, 2))) + '</g>' +
      '<g class="n9-spin" style="transform-origin:290px 50px" stroke="url(#dhLetters)" stroke-width="2.4" stroke-linecap="round" opacity=".8">' + spiral(290, 50, 3.5, 40) + '</g>' +
      '<rect width="360" height="200" filter="url(#dhG)" opacity=".5" style="mix-blend-mode:multiply"/>' +
      '<rect width="360" height="200" fill="url(#dhVig)"/>' +
      bolt(238, 70, 1.1, 'n9-flash') + bolt(96, 62, .7, 'n9-flash n9-flash2') +
      rose(112, 100, 1.05, 'n9-sway') + rose(246, 112, .9, 'n9-sway n9-sway2') + rose(338, 118, .8, 'n9-sway') +
      skull(180, 96, 1.0, 'n9-skull') +
      terrapin(292, 142, .8, 'n9-crawl') +
      wordmark(180, 34, 1) +
      bears +
      '<g transform="translate(180,184) scale(.8)">' + mack({ shirt: true, arms: true, flower: true }) + '</g>' +
      '</svg>';
  }

  function sceneDead() {
    var hues = ['#ff4fa3', '#ffe14a', '#3dff9a', '#4fd8ff', '#8a5cff', '#ff8a3d'];
    var bears = '';
    for (var i = 0; i < 8; i++) bears += bear(25 + i * 50, 772, .95, BEAR_COLORS[i % 6], i % 2 ? 'n9-hop2' : '', i * 0.11, i);
    return '<svg viewBox="0 0 400 800" preserveAspectRatio="xMidYMid slice" aria-hidden="true"><defs>' + dyeDefs() + '</defs>' +
      '<rect width="400" height="800" fill="#14031d"/>' +
      '<g class="n9-spin n9-dye-slow" style="transform-origin:330px 130px" opacity=".6">' + dyeCloth(330, 130, 300, 'ds', hues) + '</g>' +
      '<g class="n9-spin n9-spin-rev n9-dye-slow" style="transform-origin:40px 560px" opacity=".55">' + dyeCloth(40, 560, 320, 'ds', hues.slice().reverse()) + '</g>' +
      '<rect width="400" height="800" fill="#14031d" opacity=".45"/>' +
      '<rect width="400" height="800" filter="url(#dsG)" opacity=".35" style="mix-blend-mode:multiply"/>' +
      rose(36, 260, 1.5, 'n9-sway') + rose(366, 420, 1.4, 'n9-sway n9-sway2') + rose(30, 640, 1.3, 'n9-sway') +
      bolt(340, 300, 1.2, 'n9-flash') + bolt(18, 120, .9, 'n9-flash n9-flash2') +
      terrapin(360, 700, 1.2, 'n9-crawl') + skull(34, 380, .8, 'n9-skull') +
      bears + '</svg>';
  }

  /* ---------------- switcher ---------------- */
  var state = { theme: 'dark', plain: false };
  var built = {};

  function paintScene(t) {
    var sc = document.getElementById('n9-scene'), hero = document.getElementById('n9-hero');
    if (sc) sc.innerHTML = t === 'lakes' ? sceneLakes() : t === 'dead' ? sceneDead() : '';
    if (hero) {
      hero.innerHTML = t === 'lakes' ? heroLakes() : t === 'dead' ? heroDead() : '';
      hero.hidden = t === 'dark';
    }
  }

  function paintControls() {
    var t = state.theme;
    var cards = document.querySelectorAll('[data-theme-card]');
    for (var i = 0; i < cards.length; i++) {
      var on = cards[i].getAttribute('data-theme-card') === t;
      cards[i].classList.toggle('on', on);
      cards[i].setAttribute('aria-pressed', on ? 'true' : 'false');
    }
    var q = document.getElementById('themeQuick');
    if (q) {
      var nxt = IDS[(IDS.indexOf(t) + 1) % IDS.length];
      q.setAttribute('aria-label', 'Theme: ' + NAMES[t] + '. Tap for ' + NAMES[nxt]);
      q.title = 'Theme: ' + NAMES[t] + ' (tap for ' + NAMES[nxt] + ')';
      q.querySelector('span').textContent = t === 'lakes' ? '⛵' : t === 'dead' ? '🌹' : '🌑';
    }
    var p = document.getElementById('plainTalk');
    if (p) {
      p.classList.toggle('on', state.plain);
      p.setAttribute('aria-pressed', state.plain ? 'true' : 'false');
      var l = p.querySelector('span:last-child');
      if (l) l.textContent = state.plain ? 'Plain talk on' : 'Plain talk';
    }
    var vs = document.getElementById('voiceState');
    if (vs) vs.textContent = state.plain ? 'Plain talk is ON' : 'Plain talk is off';
    var m = document.querySelector('meta[name="theme-color"]');
    if (m) m.setAttribute('content', t === 'lakes' ? '#041c2b' : t === 'dead' ? '#1a0426' : '#030712');
    var input = document.getElementById('chatInput');
    if (input && document.activeElement !== input) {
      input.placeholder = state.plain ? 'Business note...' : (t === 'dead' ? 'Ask the rail...' : t === 'lakes' ? 'Ask Mack on the helm...' : 'Ask Mack or query survey...');
    }
  }

  function apply(t, plain) {
    state.theme = norm(t);
    if (typeof plain === 'boolean') state.plain = plain;
    var h = document.documentElement;
    h.dataset.theme = state.theme;
    h.dataset.voice = state.plain ? 'plain' : 'character';
    paintScene(state.theme);
    paintControls();
  }

  function pushServer() {
    try {
      fetch('/api/ui', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ theme: state.theme, plain: state.plain }) }).catch(function () {});
    } catch (e) {}
  }

  var N9T = {
    ids: IDS, names: NAMES,
    current: function () { return state.theme; },
    plain: function () { return state.plain; },
    set: function (t) { lsSet(LS, norm(t)); apply(t); pushServer(); },
    cycle: function () { N9T.set(IDS[(IDS.indexOf(state.theme) + 1) % IDS.length]); },
    setPlain: function (on) { lsSet(LSP, on ? '1' : '0'); apply(state.theme, !!on); pushServer(); },
    togglePlain: function () { N9T.setPlain(!state.plain); },
    setMotion: function (m) { lsSet(LSM, m); document.documentElement.dataset.motion = m; var s = document.getElementById('motionSel'); if (s) s.value = m; },
    /* the server copy decides only when this browser has never chosen */
    sync: function () {
      try {
        fetch('/api/ui').then(function (r) { return r.ok ? r.json() : null; }).then(function (j) {
          if (!j) return;
          if (lsGet(LS) === null && j.theme) { apply(j.theme, typeof j.plain === 'boolean' ? j.plain : state.plain); lsSet(LS, state.theme); }
          if (lsGet(LSP) === null && typeof j.plain === 'boolean') apply(state.theme, j.plain);
          if (lsGet(LS) !== null && (j.theme !== state.theme || j.plain !== state.plain)) pushServer();
        }).catch(function () {});
      } catch (e) {}
    },
    mack: mack
  };
  window.N9T = N9T;

  var saved = lsGet(LS);
  apply(saved === null ? 'dark' : saved, lsGet(LSP) === '1');
  N9T.setMotion(lsGet(LSM) || 'auto');
  /* stop the loops while the page is hidden: saves the phone's battery */
  document.addEventListener('visibilitychange', function () { document.documentElement.classList.toggle('n9-paused', document.hidden); });
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', function () { paintControls(); }); else paintControls();
})();
