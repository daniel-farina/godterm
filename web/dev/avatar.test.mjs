import assert from 'node:assert/strict';
import { pixels, genSvg, PALETTE, GRID } from '../js/avatar-gen.js';

assert.equal(genSvg('godterm'), genSvg('godterm'));
assert.deepEqual(pixels('api'), pixels('api'));

const seeds = Array.from({ length: 200 }, (_, i) => 'project-' + i);
assert.equal(new Set(seeds.map(genSvg)).size, seeds.length);
assert.notEqual(genSvg('docs'), genSvg('docs#2'));

for (const s of seeds) {
  const { fg, fg2, cells } = pixels(s);
  assert.ok(PALETTE.includes(fg) && PALETTE.includes(fg2) && fg !== fg2);
  assert.equal(cells.length, GRID);
  let lit = 0;
  for (const row of cells) {
    assert.equal(row.length, GRID);
    for (let x = 0; x < GRID; x++) {
      assert.equal(row[x], row[GRID - 1 - x]);
      if (row[x]) lit++;
    }
  }
  assert.ok(lit >= 10);
}

const svg = genSvg('x');
assert.ok(svg.startsWith('<svg') && !/<script|on\w+=/i.test(svg));
console.log('avatar generator ok');
