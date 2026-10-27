import { $, h, on, emit, toast, paintIcons, isTouch, store } from './util.js';
import { panes, setInputHook } from './term.js';
import { openSheet, closeSheet } from './sheet.js';

const sendImages = (files, p) => import('./images.js').then((m) => m.sendImages(files, p));

function pickImages(pane, then) {
  const inp = h('input', { type: 'file', accept: 'image/*', multiple: true, style: 'position:fixed;left:-100px;top:0;width:1px;height:1px;opacity:0' });
  inp.addEventListener('change', () => { const f = [...inp.files]; if (then) { if (f.length) then(f); } else sendImages(f, pane); inp.remove(); });
  document.body.append(inp);
  inp.click();
  setTimeout(() => inp.isConnected && !inp.files?.length && inp.remove(), 120000);
}

const mods = { ctrl: 0, alt: 0 };
const btns = {};

function cur() { return panes.focused(); }
function app() { return cur().term.modes.applicationCursorKeysMode; }
function arrow(c) { return (app() ? '\x1bO' : '\x1b[') + c; }

const page = (dir) => () => {
  const p = cur();
  if (p.scrollPath() === 'app') return dir < 0 ? '\x1b[5~' : '\x1b[6~';
  p.term.scrollPages(dir);
  return '';
};

const KEYS = [
  [null, 'kbhide', 'Hide the keyboard', 'kbdown'], ['Esc', () => '\x1b'], ['Ctrl', 'ctrl'], ['Tab', () => '\t'],
  ['↑', () => arrow('A'), 'Up', null, 'arr'], ['↓', () => arrow('B'), 'Down', null, 'arr'], ['←', () => arrow('D'), 'Left', null, 'arr'], ['→', () => arrow('C'), 'Right', null, 'arr'],
  ['Enter', () => '\r'], ['^C', () => '\x03', 'Control C'], '|',
  ['Paste', 'paste'], [null, 'image', 'Attach an image to this tab', 'image'], '|',
  ['PgUp', page(-1), 'Page up'], ['PgDn', page(1), 'Page down'], ['Home', () => '\x1b[H'], ['End', () => '\x1b[F'], ['Alt', 'alt'], ['⇧Tab', () => '\x1b[Z', 'Shift Tab'], '|',
  ['Prompt', 'prompt', 'Write a prompt for this tab'], '|', ['A-', 'fminus', 'Smaller text'], ['A+', 'fplus', 'Larger text'], ['Fit', 'fit', 'Fit the text to the screen'], ['Size', 'take', "Use this screen's size"],
];

function ctrlOf(d) {
  if (d.length !== 1) return d;
  const c = d.toUpperCase().charCodeAt(0);
  if (c >= 64 && c <= 95) return String.fromCharCode(c - 64);
  if (d === ' ' || d === '2') return '\x00';
  if (d === '?' || d === '8') return '\x7f';
  if (d >= '3' && d <= '7') return String.fromCharCode(d.charCodeAt(0) - 24); 
  return d;
}

function withMods(d) {
  if (!mods.ctrl && !mods.alt) return d;

  if (mods.ctrl && !mods.alt && (d === 'v' || d === 'V') && isTouch()) {
    if (mods.ctrl === 1) setMod('ctrl', 0);
    paste(true);
    return '';
  }
  let out = d;
  if (mods.ctrl) out = ctrlOf(out);
  if (mods.alt) out = '\x1b' + out;
  if (mods.ctrl === 1) setMod('ctrl', 0);
  if (mods.alt === 1) setMod('alt', 0);
  return out;
}

function setMod(k, v) {
  mods[k] = v;
  const b = btns[k];
  if (!b) return;
  b.classList.toggle('on', v > 0);
  b.classList.toggle('lock', v === 2);
  b.setAttribute('aria-pressed', String(v > 0));
}

const lastTap = { ctrl: 0, alt: 0 };
function tapMod(k) {
  const now = performance.now();
  if (mods[k] === 1 && now - lastTap[k] < 350) { setMod(k, 2); navigator.vibrate?.(12); }
  else setMod(k, mods[k] ? 0 : 1);
  lastTap[k] = now;
}

export async function paste(ctrlV = false) {
  const p = cur();
  if (!p.id) return toast('Pick a tab first', 'warn');
  let empty = false, asked = false;
  if (navigator.clipboard?.read) {
    asked = true;
    try {
      const items = await navigator.clipboard.read();
      for (const it of items) {
        const type = it.types.find((t) => t.startsWith('image/'));
        if (type) return sendImages([new File([await it.getType(type)], 'pasted', { type })], p);
      }
      for (const it of items) {
        if (!it.types.includes('text/plain')) continue;
        const t = await (await it.getType('text/plain')).text();
        if (t) return p.paste(t);
      }
      empty = true;
    } catch {}
  }
  if (!asked && navigator.clipboard?.readText) {
    try {
      const t = await navigator.clipboard.readText();
      if (t) return p.paste(t);
      empty = true;
    } catch {}
  }
  if (empty) return ctrlV ? p.input('\x16') : toast('Nothing on this phone\'s clipboard', 'info');
  promptSheet('paste', [], ctrlV);
}

function pickImage() {
  const p = cur();
  if (!p.id) return toast('Pick a tab first', 'warn');
  pickImages(p, (files) => promptSheet('image', files));
}

export function promptSheet(mode = 'prompt', files = [], ctrlV = false) {
  if (mode === true) mode = 'paste';
  if (mode === false) mode = 'prompt';
  const isPaste = mode === 'paste';
  const ta = h('textarea.f', { rows: mode === 'image' ? 2 : 5, enterkeyhint: 'enter',
    placeholder: isPaste ? 'Tap and hold here, then Paste' : mode === 'image' ? 'Text to go with it (optional)' : 'Type a prompt for this tab' });
  const enter = h('input', { type: 'checkbox', checked: mode === 'prompt' });
  const imgs = [...files];
  const thumbs = h('div.att-in');
  const paint = () => thumbs.replaceChildren(...imgs.map((f, i) => h('span.thumb', {}, h('img', { src: URL.createObjectURL(f), alt: 'image ' + (i + 1) }),
    h('button', { type: 'button', 'aria-label': 'Remove image', title: 'Remove image', onclick: () => { imgs.splice(i, 1); paint(); } }, '×'))));
  paint();

  const tabId = cur()?.id, dkey = 'draft.p.' + tabId;
  if (mode === 'prompt' && tabId) {
    ta.value = store.get(dkey, '') || '';
    ta.addEventListener('input', () => store.set(dkey, ta.value || null));
  }
  ta.addEventListener('paste', (e) => {
    const got = [...(e.clipboardData?.files || [])].filter((f) => f.type.startsWith('image/'));
    if (!got.length) return;
    e.preventDefault();
    imgs.push(...got);
    paint();
  });
  const go = async () => {
    const p = cur();
    if (!p.id) return toast('Pick a tab first', 'warn');
    closeSheet();
    if (mode === 'prompt') store.set('draft.p.' + p.id, null);
    if (imgs.length) await sendImages(imgs, p);

    if (ta.value && enter.checked) await p.pasteThenEnter(ta.value);
    else if (ta.value) p.paste(ta.value);
    else if (enter.checked) {

      if (imgs.length) await new Promise((r) => setTimeout(r, 600));
      p.input('\r');
    }
  };
  const choose = h('button.btn', { type: 'button', 'data-i': 'image', onclick: () => pickImages(cur(), (f) => { imgs.push(...f); paint(); }) }, imgs.length ? 'Add photo' : 'Choose photo');
  const name = cur()?.name?.textContent;
  const title = isPaste ? 'Paste into the tab' : mode === 'image' ? 'Image for the tab' : 'Prompt this tab';
  openSheet(h('div.sh-title', {}, h('span', {}, title), name ? h('span.dim', { style: 'font-weight:400;font-size:14px' }, name) : null),
    isPaste ? h('div.mnote', {}, "This phone's clipboard was not shared. Tap and hold in the box, then Paste, or choose a photo.") : null,
    ta, thumbs,
    h('div.acts', {}, choose,
      isPaste && ctrlV ? h('button.btn', { type: 'button', title: 'Send Ctrl-V to the tab instead (its own paste, on the Mac)', onclick: () => { closeSheet(); cur().input('\x16'); } }, 'Send ^V instead') : null),
    h('label.bar', {}, enter, ' Press Enter after it'),
    h('div.acts', {}, h('button.btn', { type: 'button', onclick: closeSheet }, 'Cancel'), h('button.btn.pri', { type: 'button', onclick: go }, 'Send')));
  paintIcons($('#sheet-body'));
  if (mode !== 'image') setTimeout(() => ta.focus(), 50);
}

function press(act, b) {
  const p = cur();

  if (act === 'kbhide') return document.activeElement?.blur();
  if (act === 'ctrl' || act === 'alt') return tapMod(act);
  if (act === 'paste') return paste();
  if (act === 'image') return pickImage();
  if (act === 'prompt') return promptSheet('prompt');
  if (act === 'fminus') return p.step(-1);
  if (act === 'fplus') return p.step(1);
  if (act === 'fit') return p.step(0);
  if (act === 'take') { p.toggleTake(); b.classList.toggle('on', p.take); return; }
  if (!p.id) return toast('Pick a tab first', 'warn');
  let d = act();
  if (!d) return;
  if (mods.alt) { d = '\x1b' + d; if (mods.alt === 1) setMod('alt', 0); }
  if (mods.ctrl === 1) setMod('ctrl', 0);
  p.input(d);
}

function paintEdges(bar) {
  const max = bar.scrollWidth - bar.clientWidth;
  bar.classList.toggle('more-l', bar.scrollLeft > 2);
  bar.classList.toggle('more-r', bar.scrollLeft < max - 2);
}

let hw = false;
export const keybarPref = () => store.get('keybar', 'auto');
export function applyKeybar() {
  const pref = keybarPref();
  const off = pref === 'never' || (pref === 'auto' && hw && !document.body.classList.contains('kb'));
  if (off === document.body.classList.contains('nokeys')) return;
  document.body.classList.toggle('nokeys', off);
  emit('layout');
}
function watchHardwareKeys() {
  addEventListener('keydown', (e) => {
    if (hw || !isTouch()) return;
    const k = e.key || '';
    if (!(k === 'Escape' || k === 'Tab' || k.startsWith('Arrow') || ((e.ctrlKey || e.metaKey) && k.length === 1))) return;
    hw = true;
    applyKeybar();
    if (keybarPref() === 'auto' && !document.body.classList.contains('kb')) toast('Hardware keyboard: the key bar is hidden', 'info', { label: 'Keep it', fn: () => { store.set('keybar', 'always'); applyKeybar(); } });
  }, true);
  on('layout', () => { if (hw && document.body.classList.contains('kb')) hw = false; applyKeybar(); });
  applyKeybar();
}

export function initKeybar() {
  watchHardwareKeys();
  const bar = $('#keybar');
  const map = new Map();
  for (const k of KEYS) {
    if (k === '|') { bar.append(h('span.sep', { 'aria-hidden': 'true' })); continue; }
    const [label, act, aria, ic, cls] = k;
    const b = h('button', { type: 'button', tabindex: -1, 'aria-label': aria || null, 'data-i': ic || null, class: [ic && 'kic', cls].filter(Boolean).join(' ') || null }, label || '');
    if (typeof act === 'string') btns[act] = b;
    map.set(b, act);
    bar.append(b);
  }
  paintIcons(bar);
  let down = null;
  bar.addEventListener('pointerdown', (e) => {
    const b = e.target.closest('button');
    down = b ? { b, id: e.pointerId, x: e.clientX, y: e.clientY, sl: bar.scrollLeft, t: performance.now() } : null;
    b?.classList.add('down');
  });
  const lift = () => { down?.b.classList.remove('down'); down = null; };
  bar.addEventListener('pointermove', (e) => {
    if (down && e.pointerId === down.id && Math.abs(e.clientX - down.x) + Math.abs(e.clientY - down.y) > 10) lift();
  });
  bar.addEventListener('pointercancel', lift);
  bar.addEventListener('pointerleave', (e) => { if (e.pointerType === 'mouse') lift(); });
  bar.addEventListener('pointerup', (e) => {
    const d = down;
    lift();
    if (!d || e.pointerId !== d.id || Math.abs(bar.scrollLeft - d.sl) > 2) return;
    if (e.target.closest('button') !== d.b) return;
    press(map.get(d.b), d.b);
  });

  bar.addEventListener('touchend', (e) => { if (e.cancelable) e.preventDefault(); }, { passive: false });
  bar.addEventListener('mousedown', (e) => e.preventDefault());
  bar.addEventListener('click', (e) => e.preventDefault());
  bar.addEventListener('scroll', () => paintEdges(bar), { passive: true });
  new ResizeObserver(() => paintEdges(bar)).observe(bar);
  setInputHook(withMods);

  const sendB = $('#b-ksend');
  const sendNow = (e) => { e.preventDefault(); const p = cur(); if (!p.id) return toast('Pick a tab first', 'warn'); p.input('\r'); };
  sendB.addEventListener('mousedown', (e) => e.preventDefault());
  sendB.addEventListener('touchend', (e) => { if (e.cancelable) sendNow(e); }, { passive: false });
  sendB.addEventListener('click', sendNow);
  for (const ev of ['focus', 'take']) on(ev, (p) => btns.take?.classList.toggle('on', !!p?.take));
}
