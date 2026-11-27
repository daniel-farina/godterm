import { h, on, emit, esc, store, toast } from './util.js';
import { S } from './state.js';
import { tool } from './conn.js';
import { genSvg } from './avatar-gen.js';

export const HARNESS = { claude: 'Claude', grok: 'Grok', codex: 'Codex', cursor: 'Cursor', antigravity: 'Antigravity', opencode: 'OpenCode', gemini: 'Gemini' };
export const HGLYPH = {
  claude: '<path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6L5.6 18.4"/>',
  grok: '<circle cx="12" cy="12" r="8"/><path d="M8 16l8-8"/>',
  codex: '<path d="M6 8l5 4-5 4M13 17h5"/>',
  cursor: '<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9z"/>',
  antigravity: '<path d="M12 4l8 15H4z"/>',
  opencode: '<path d="M9 5C7 5 7 8 7 10s-1 2-2 2c1 0 2 0 2 2s0 5 2 5M15 5c2 0 2 3 2 5s1 2 2 2c-1 0-2 0-2 2s0 5-2 5"/>',
};

export const harnessOf = (t) => t.harness || S.accounts.find((a) => a.account === t.account)?.harness || 'claude';

export function hbadge(k) {
  const g = HGLYPH[k];
  if (!g) return '';
  return `<span class="hb hb-${esc(k)}" title="${esc(HARNESS[k] || k)}"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round">${g}</svg></span>`;
}

export const avatarsOn = () => S.tab_avatars !== false && store.get('avatars', true) !== false;

export function setAvatarsOn(on) {
  store.set('avatars', on ? null : false);
  sync();
  emit('state', S);
}

const GEN = /^gen:(\d{1,9})$/;
const FILE = /^[0-9a-f]{16}\.(png|jpg|gif|webp|svg)$/;
export const isImage = (t) => FILE.test(t?.avatar || '');

export function seedOf(t) {
  const folder = String(t.folder || '').replace(/[\\/]+$/, '').split(/[\\/]/).pop();
  const base = folder && folder !== '~' ? folder : t.id || t.name || 'tab';
  const v = GEN.exec(t.avatar || '');
  return v ? `${base}#${v[1]}` : base;
}

function inner(t) {
  if (isImage(t)) return `<img src="/avatar/${t.avatar}" alt="" draggable="false" decoding="async">`;
  return genSvg(seedOf(t));
}

const BUSY = new Set(['working', 'background', 'busy']);

export function avatarHtml(t, size = '') {
  const st = esc(t.state || '');
  return `<span class="av${size ? ' ' + size : ''}${BUSY.has(t.state) || t.busy ? ' busy' : ''}" data-tab="${esc(t.id)}"><span class="avi">${inner(t)}</span><span class="dot ${st}"></span>${hbadge(harnessOf(t))}</span>`;
}

export function paintAvatar(el, t, size = '') {
  if (!el) return;
  const key = t ? [t.avatar, seedOf(t), harnessOf(t), t.state, t.busy, size].join('|') : '';
  if (el._k === key) return;
  el._k = key;
  if (!t) { el.replaceChildren(); el.hidden = true; return; }
  el.hidden = false;
  el.innerHTML = avatarHtml(t, size);
}

function sync() {
  document.documentElement.classList.toggle('avon', avatarsOn());
}
on('state', sync);
sync();

function load(file) {
  return new Promise((res, rej) => {
    const url = URL.createObjectURL(file);
    const im = new Image();
    im.onload = () => { res({ im, url }); };
    im.onerror = () => { URL.revokeObjectURL(url); rej(new Error('that file is not an image the browser can read')); };
    im.src = url;
  });
}

export async function squarePng(file, px = 128) {
  const { im, url } = await load(file);
  try {
    const w = im.naturalWidth || 256, hh = im.naturalHeight || 256;
    const s = Math.min(w, hh);
    const c = document.createElement('canvas');
    c.width = c.height = px;
    const g = c.getContext('2d');
    g.imageSmoothingQuality = 'high';
    g.drawImage(im, (w - s) / 2, (hh - s) / 2, s, s, 0, 0, px, px);
    return c.toDataURL('image/png');
  } finally { URL.revokeObjectURL(url); }
}

export async function setFromFile(tabId, file) {
  if (!file) return false;
  if (!/^image\//.test(file.type) && !/\.(png|jpe?g|gif|webp|svg)$/i.test(file.name || '')) { toast('Pick an image file', 'warn'); return false; }
  if (file.size > 20 << 20) { toast('That image is over 20 MB', 'warn'); return false; }
  try {
    const data = await squarePng(file);
    const r = await tool('set_tab_avatar', { tab: tabId, data });
    if (r.ok === false || r.error || r.result?.error) { toast(r.error || r.result?.error || 'It did not work', 'error'); return false; }
    toast('Avatar set', 'ok');
    return true;
  } catch (e) { toast(e.message || 'It did not work', 'error'); return false; }
}

export function pickImage(tabId) {
  const inp = h('input', { type: 'file', accept: 'image/*', hidden: true });
  inp.addEventListener('change', () => { const f = inp.files?.[0]; inp.remove(); if (f) setFromFile(tabId, f); });
  inp.addEventListener('cancel', () => inp.remove());
  document.body.append(inp);
  inp.click();
}

const filesIn = (e) => !!e.dataTransfer?.types?.includes('Files');
let over = null;
function clearOver() { over?.classList.remove('drop'); over = null; }
addEventListener('dragover', (e) => {
  const a = filesIn(e) && e.target.closest?.('.av[data-tab]');
  if (!a) { clearOver(); return; }
  e.preventDefault();
  e.stopPropagation();
  e.dataTransfer.dropEffect = 'copy';
  if (over !== a) { clearOver(); over = a; a.classList.add('drop'); }
}, true);
addEventListener('dragleave', (e) => { if (over && !e.relatedTarget) clearOver(); }, true);
addEventListener('drop', (e) => {
  const a = filesIn(e) && e.target.closest?.('.av[data-tab]');
  clearOver();
  if (!a) return;
  e.preventDefault();
  e.stopPropagation();
  setFromFile(a.dataset.tab, e.dataTransfer.files?.[0]);
}, true);
