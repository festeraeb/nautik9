// WCAG contrast check for every theme's text tokens against its surfaces.
// Usage: node tests/theme-contrast.mjs   (exit 1 if any body-text pair is under 4.5:1; we hold 7:1)
import fs from 'node:fs';
const css = fs.readFileSync(new URL('../static/themes.css', import.meta.url), 'utf8');
const html = fs.readFileSync(new URL('../src/index.html', import.meta.url), 'utf8');
function vars(block) { const o = {}; for (const m of block.matchAll(/--([a-z-]+):\s*(#[0-9a-fA-F]{6})/g)) o[m[1]] = m[2]; return o; }
const themes = {
  dark: vars(html.match(/:root\s*\{([^}]*)\}/)[1]),
  lakes: vars(css.match(/html\[data-theme="lakes"\]\s*\{([^}]*)\}/)[1]),
  dead: vars(css.match(/html\[data-theme="dead"\]\s*\{([^}]*)\}/)[1]),
};
const lin = c => { c /= 255; return c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4; };
const L = h => { const n = parseInt(h.slice(1), 16); return 0.2126 * lin(n >> 16) + 0.7152 * lin((n >> 8) & 255) + 0.0722 * lin(n & 255); };
const ratio = (a, b) => { const [x, y] = [L(a), L(b)].sort((p, q) => q - p); return (x + 0.05) / (y + 0.05); };
let bad = 0;
for (const [name, v] of Object.entries(themes)) {
  for (const fg of ['text-main', 'text-sub', 'text-muted', 'accent', 'brass']) {
    for (const bg of ['bg', 'surface', 'surface-card', 'surface-elevated']) {
      const r = ratio(v[fg], v[bg]);
      const need = (name === 'dark' && fg === 'text-sub') || fg === 'text-muted' || fg === 'accent' || fg === 'brass' ? 4.5 : 7;
      if (r < need) { bad++; console.log(`FAIL ${name}: ${fg} on ${bg} = ${r.toFixed(2)} (need ${need})`); }
      else if (fg === 'text-main' || fg === 'text-sub') console.log(`ok   ${name}: ${fg} on ${bg} = ${r.toFixed(2)}`);
    }
  }
}
// buttons: dark text on the lakes / dead gradient buttons
const btn = { lakes: ['#1b1000', ['#ff7a2f', '#f2b84b']], dead: ['#1a0524', ['#ff4fa3', '#ffe14a', '#3dff9a', '#4fd8ff']] };
for (const [n, [fg, bgs]] of Object.entries(btn)) for (const bg of bgs) { const r = ratio(fg, bg); console.log(`${r >= 4.5 ? 'ok  ' : 'FAIL'} ${n}: button text on ${bg} = ${r.toFixed(2)}`); if (r < 4.5) bad++; }
process.exit(bad ? 1 : 0);
