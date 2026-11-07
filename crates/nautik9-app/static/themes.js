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
           '<path d="M-10 -39 Q-4 -45 4 -43" stroke="#fff" stroke-width="1.6" fill="none" stroke-linecap="round" opacity=".75"/>' +
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

  function rose(x, y, sc, cls) {
    return '<g transform="translate(' + x + ',' + y + ') scale(' + sc + ')"><g class="' + (cls || '') + '">' +
      '<path d="M0 6 Q-4 18 -2 30" stroke="#1f9d55" stroke-width="2" fill="none"/>' +
      '<path d="M-1 20 Q-12 14 -14 20 Q-8 24 -1 20 Z" fill="#23b45f"/><path d="M-1 25 Q9 18 12 23 Q6 28 -1 25 Z" fill="#23b45f"/>' +
      '<circle r="9" fill="#d81b4a"/><path d="M-6 -2 Q0 -9 6 -2 Q3 5 -3 3 Q-7 1 -4 -4" stroke="#ff7a9a" stroke-width="1.3" fill="none"/>' +
      '<path d="M-3 0 Q0 -4 3 -1 Q1 2 -1 1" stroke="#ffb3c6" stroke-width="1.1" fill="none"/></g></g>';
  }

  function bolt(x, y, sc, cls) {
    return '<g transform="translate(' + x + ',' + y + ') scale(' + sc + ')"><g class="' + (cls || '') + '">' +
      '<polygon points="0,0 14,0 6,16 18,16 -4,46 2,24 -8,24" fill="#ffe14a" stroke="#ff4fa3" stroke-width="2.4" stroke-linejoin="round"/></g></g>';
  }

  function skull(x, y, sc, cls) {
    /* an original round, grinning skull with a rose crown and a star-flecked brow, not a licensed mark */
    return '<g transform="translate(' + x + ',' + y + ') scale(' + sc + ')"><g class="' + (cls || '') + '">' +
      '<path d="M-24 4 Q-26 -26 0 -28 Q26 -26 24 4 Q24 14 14 18 L12 28 H-12 L-14 18 Q-24 14 -24 4 Z" fill="#fff6ea" stroke="#2a0a3a" stroke-width="2"/>' +
      '<circle cx="-10" cy="2" r="7.5" fill="#2a0a3a"/><circle cx="10" cy="2" r="7.5" fill="#2a0a3a"/>' +
      '<path d="M-10 -2 l1.8 3.6 l-3.6 0 z M10 -2 l1.8 3.6 l-3.6 0 z" fill="#ff4fa3"/>' +
      '<path d="M0 10 L-3.4 17 H3.4 Z" fill="#2a0a3a"/>' +
      '<path d="M-12 22 H12 M-8 19 V28 M-3 19 V28 M3 19 V28 M8 19 V28" stroke="#2a0a3a" stroke-width="1.6" fill="none"/>' +
      '<polygon points="-4,-24 3,-24 -1,-17 5,-17 -6,-6 -3,-15 -8,-15" fill="#ffe14a" stroke="#ff4fa3" stroke-width="1.2" stroke-linejoin="round" transform="translate(11,2) scale(.7)"/>' +
      '<g transform="translate(-22,-22)"><circle r="7" fill="#d81b4a"/><circle r="2.4" fill="#ff7a9a"/></g>' +
      '<g transform="translate(0,-30)"><circle r="8" fill="#ff4fa3"/><circle r="3" fill="#ffe14a"/></g>' +
      '<g transform="translate(22,-22)"><circle r="7" fill="#d81b4a"/><circle r="2.4" fill="#ff7a9a"/></g>' +
      '<path d="M-30 -22 q-6 4 -2 9 M30 -22 q6 4 2 9" stroke="#23b45f" stroke-width="3" fill="none"/></g></g>';
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

  function bear(x, y, sc, fill, cls, dly) {
    /* a round, blobby dancing bear drawn for NautiK9 (not the classic outlined bears) */
    return '<g class="n9-bear ' + (cls || '') + '" style="animation-delay:' + (dly || 0) + 's" transform="translate(' + x + ',' + y + ') scale(' + sc + ')">' +
      '<g class="n9-bear-in"><path d="M-9 -4 Q-17 -12 -13 -22 M9 -4 Q17 -12 13 -22" stroke="' + fill + '" stroke-width="5" fill="none" stroke-linecap="round"/>' +
      '<ellipse cx="0" cy="0" rx="11" ry="13" fill="' + fill + '"/><path d="M-5 12 L-7 22 M5 12 L7 22" stroke="' + fill + '" stroke-width="5.4" stroke-linecap="round"/>' +
      '<circle cx="0" cy="-17" r="8.5" fill="' + fill + '"/><circle cx="-7" cy="-24" r="3.2" fill="' + fill + '"/><circle cx="7" cy="-24" r="3.2" fill="' + fill + '"/>' +
      '<circle cx="-3" cy="-18" r="1.1" fill="#1a0614"/><circle cx="3" cy="-18" r="1.1" fill="#1a0614"/><path d="M-3 -13.6 Q0 -11 3 -13.6" stroke="#1a0614" stroke-width="1" fill="none"/>' +
      '<ellipse cx="0" cy="3" rx="6" ry="6.4" fill="#fff6ea" opacity=".5"/></g></g>';
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
      '<radialGradient id="n9dyeShell" cx=".4" cy=".4" r=".8"><stop offset="0" stop-color="#ffe14a"/><stop offset=".45" stop-color="#ff4fa3"/><stop offset="1" stop-color="#6a2cff"/></radialGradient>';
  }

  function heroDead() {
    var bears = '';
    for (var i = 0; i < 6; i++) bears += bear(30 + i * 60, 168, .62, BEAR_COLORS[i], i % 2 ? 'n9-hop2' : '', i * 0.13);
    return '<svg class="n9-hero-svg" viewBox="0 0 360 200" preserveAspectRatio="xMidYMid slice" role="img" aria-label="Tie-dye spirals, a rose-crowned skull, a lightning bolt, a terrapin and a line of dancing bears with Mack in a tie-dye shirt">' +
      '<defs>' + dyeDefs() +
      '<radialGradient id="dhBg" cx=".5" cy=".5" r=".75"><stop offset="0" stop-color="#ff4fa3"/><stop offset=".3" stop-color="#8a2cff"/><stop offset=".62" stop-color="#2a0a5a"/><stop offset="1" stop-color="#12031c"/></radialGradient>' +
      '<linearGradient id="dhSpiral" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#ffe14a"/><stop offset=".33" stop-color="#3dff9a"/><stop offset=".66" stop-color="#4fd8ff"/><stop offset="1" stop-color="#ff4fa3"/></linearGradient></defs>' +
      '<rect width="360" height="200" fill="url(#dhBg)"/>' +
      '<g class="n9-spin" style="transform-origin:290px 52px"><g stroke="url(#dhSpiral)" stroke-width="3.4" stroke-linecap="round">' + spiral(290, 52, 4, 46) + '</g></g>' +
      '<g class="n9-spin n9-spin-rev" style="transform-origin:62px 118px"><g stroke="url(#dhSpiral)" stroke-width="3" stroke-linecap="round" opacity=".9">' + spiral(62, 118, 3, 38) + '</g></g>' +
      '<g class="n9-spin" style="transform-origin:180px 8px" opacity=".55"><g stroke="#ffe14a" stroke-width="2" stroke-linecap="round">' + spiral(180, 8, 3, 40) + '</g></g>' +
      bolt(238, 70, 1.1, 'n9-flash') + bolt(96, 20, .7, 'n9-flash n9-flash2') +
      skull(176, 84, 1.05, 'n9-skull') +
      rose(124, 96, 1.1, 'n9-sway') + rose(230, 108, .9, 'n9-sway n9-sway2') + rose(334, 120, .8, 'n9-sway') +
      terrapin(318, 148, .9, 'n9-crawl') +
      bears +
      '<g transform="translate(180,184) scale(.8)">' + mack({ shirt: true, arms: true, flower: true }) + '</g>' +
      '</svg>';
  }

  function sceneDead() {
    var bears = '';
    for (var i = 0; i < 8; i++) bears += bear(25 + i * 50, 780, .9, BEAR_COLORS[i % 6], i % 2 ? 'n9-hop2' : '', i * 0.11);
    return '<svg viewBox="0 0 400 800" preserveAspectRatio="xMidYMid slice" aria-hidden="true"><defs>' + dyeDefs() +
      '<linearGradient id="dsSp" x1="0" y1="0" x2="1" y2="1"><stop offset="0" stop-color="#ffe14a"/><stop offset=".5" stop-color="#ff4fa3"/><stop offset="1" stop-color="#4fd8ff"/></linearGradient>' +
      '<radialGradient id="dsA" cx=".2" cy=".15" r=".6"><stop offset="0" stop-color="#ff4fa3" stop-opacity=".9"/><stop offset="1" stop-color="#ff4fa3" stop-opacity="0"/></radialGradient>' +
      '<radialGradient id="dsB" cx=".85" cy=".35" r=".6"><stop offset="0" stop-color="#8a5cff" stop-opacity=".95"/><stop offset="1" stop-color="#8a5cff" stop-opacity="0"/></radialGradient>' +
      '<radialGradient id="dsC" cx=".25" cy=".7" r=".55"><stop offset="0" stop-color="#3dff9a" stop-opacity=".6"/><stop offset="1" stop-color="#3dff9a" stop-opacity="0"/></radialGradient>' +
      '<radialGradient id="dsD" cx=".8" cy=".92" r=".5"><stop offset="0" stop-color="#ffe14a" stop-opacity=".6"/><stop offset="1" stop-color="#ffe14a" stop-opacity="0"/></radialGradient></defs>' +
      '<rect width="400" height="800" fill="#14031d"/><rect width="400" height="800" fill="url(#dsA)"/><rect width="400" height="800" fill="url(#dsB)"/><rect width="400" height="800" fill="url(#dsC)"/><rect width="400" height="800" fill="url(#dsD)"/>' +
      '<g class="n9-spin" style="transform-origin:330px 120px" opacity=".55"><g stroke="url(#dsSp)" stroke-width="5" stroke-linecap="round">' + spiral(330, 120, 5, 130) + '</g></g>' +
      '<g class="n9-spin n9-spin-rev" style="transform-origin:50px 520px" opacity=".5"><g stroke="url(#dsSp)" stroke-width="5" stroke-linecap="round">' + spiral(50, 520, 5, 150) + '</g></g>' +
      '<g class="n9-spin" style="transform-origin:300px 640px" opacity=".35"><g stroke="#ffe14a" stroke-width="3" stroke-linecap="round">' + spiral(300, 640, 4, 90) + '</g></g>' +
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
