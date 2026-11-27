import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const src = process.argv[2];
const out = process.argv[3] || 'web/js/icons.js';
if (!src) {
  console.error('usage: node scripts/build-icons.mjs <lucide-static>/icons [web/js/icons.js]');
  process.exit(2);
}

const MAP = {
  menu: 'menu',
  gear: 'settings',
  tune: 'sliders-horizontal',
  history: 'history',
  manage: 'layout-dashboard',
  usage: 'gauge',
  chat: 'sparkles',
  check: 'shield-check',
  more: 'ellipsis',
  dots: 'ellipsis-vertical',
  mic: 'mic',
  micoff: 'mic-off',
  audio: 'audio-lines',
  ear: 'ear',
  send: 'send-horizontal',
  plus: 'plus',
  x: 'x',
  split: 'columns-2',
  search: 'search',
  image: 'image',
  max: 'maximize',
  edit: 'pencil',
  pin: 'pin',
  color: 'palette',
  group: 'group',
  move: 'arrow-right-left',
  dup: 'copy-plus',
  copyto: 'clipboard-copy',
  copy: 'copy',
  desktop: 'monitor-up',
  model: 'cpu',
  clock: 'clock',
  stop: 'square',
  reopen: 'rotate-ccw',
  folder: 'folder',
  folderopen: 'folder-open',
  hash: 'hash',
  ext: 'external-link',
  globe: 'globe',
  archive: 'archive',
  chev: 'chevron-right',
  chevd: 'chevron-down',
  back: 'chevron-left',
  filter: 'list-filter',
  kbdown: 'keyboard',
  loops: 'repeat',
  sched: 'calendar-clock',
  branch: 'g' + 'it-branch',
  users: 'users',
  newchat: 'message-square-plus',
  memory: 'memory-stick',
  privacy: 'eye-off',
  leaf: 'leaf',
  volx: 'volume-x',
  vol1: 'volume-1',
  vol2: 'volume-2',
  down: 'arrow-down',
  arrowr: 'arrow-right',
  bell: 'bell-ring',
  tick: 'check',
  shieldalert: 'shield-alert',
};

const rows = [];
for (const [name, file] of Object.entries(MAP)) {
  const p = join(src, file + '.svg');
  if (!existsSync(p)) {
    console.error('missing icon', file);
    process.exit(1);
  }
  const svg = readFileSync(p, 'utf8');
  const m = svg.match(/<svg[^>]*>([\s\S]*?)<\/svg>/);
  const inner = m[1]
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean)
    .join('')
    .replace(/ \/>/g, '/>')
    .replace(/\s{2,}/g, ' ');
  rows.push(`  ${name}: '${inner.replace(/'/g, "\\'")}',`);
}

writeFileSync(out, `export const I = {\n${rows.join('\n')}\n};\n`);
console.log(`${rows.length} icons -> ${out}`);
