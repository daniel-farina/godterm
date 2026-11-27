export const PALETTE = ['sage', 'sand', 'slate', 'clay', 'mauve', 'stone', 'moss', 'teal', 'rose', 'dusk'];
export const GRID = 6;

function seedFn(str) {
  let h = 1779033703 ^ str.length;
  for (let i = 0; i < str.length; i++) {
    h = Math.imul(h ^ str.charCodeAt(i), 3432918353);
    h = (h << 13) | (h >>> 19);
  }
  return () => {
    h = Math.imul(h ^ (h >>> 16), 2246822507);
    h = Math.imul(h ^ (h >>> 13), 3266489909);
    return (h ^= h >>> 16) >>> 0;
  };
}

function rngFn(str) {
  let a = seedFn(String(str))();
  return () => {
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

export function pixels(seed) {
  const r = rngFn(seed);
  const i1 = Math.floor(r() * PALETTE.length);
  let i2 = Math.floor(r() * (PALETTE.length - 1));
  if (i2 >= i1) i2++;
  const half = GRID / 2;
  const rows = [];
  let lit = 0;
  for (let y = 0; y < GRID; y++) {
    const left = [];
    for (let x = 0; x < half; x++) {
      const v = r() < 0.52 ? (r() < 0.28 ? 2 : 1) : 0;
      if (v) lit++;
      left.push(v);
    }
    rows.push(left);
  }
  for (let y = 0; lit < 10 && y < GRID; y++) {
    if (!rows[y][half - 1]) { rows[y][half - 1] = 1; lit++; }
  }
  const cells = rows.map((left) => [...left, ...[...left].reverse()]);
  return { fg: PALETTE[i1], fg2: PALETTE[i2], cells };
}

export function genSvg(seed) {
  const { fg, fg2, cells } = pixels(seed);
  const m = 1, size = GRID + 2 * m;
  const rect = (v) => cells.flatMap((row, y) => row.map((c, x) => (c === v ? `<rect x="${x + m}" y="${y + m}" width="1" height="1"/>` : ''))).join('');
  return `<svg viewBox="0 0 ${size} ${size}" shape-rendering="crispEdges" aria-hidden="true" focusable="false">` +
    `<rect width="${size}" height="${size}" style="fill:var(--sw-${fg});opacity:.2"/>` +
    `<g style="fill:var(--sw-${fg})">${rect(1)}</g><g style="fill:var(--sw-${fg2})">${rect(2)}</g></svg>`;
}
