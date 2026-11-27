import { h, toast } from './util.js';
import { popMenu, closeMenu, chromeInfo, openIn } from './ebv_open.js';

const MAX_TABS = 6;
let seq = 0;
let pasteOk = null;

const isWeb = (u) => /^https?:\/\//i.test(u || '');
const blank = (u) => !u || u === 'about:blank';

function probePaste() {
  if (pasteOk !== null) return;
  pasteOk = !!(window.isSecureContext && navigator.clipboard && typeof navigator.clipboard.readText === 'function');
  try {
    navigator.permissions?.query({ name: 'clipboard-read' }).then((r) => {
      if (r.state === 'denied') pasteOk = false;
      r.onchange = () => { if (r.state === 'denied') pasteOk = false; };
    }, () => {});
  } catch {}
}

function legacyCopy(text) {
  let ok = false;
  const t = h('textarea', { readOnly: true, 'aria-hidden': 'true', tabindex: '-1', style: 'position:fixed;left:-9999px;top:0;opacity:0' });
  t.value = text;
  try {
    document.body.append(t);
    t.select();
    ok = document.execCommand('copy');
  } catch { ok = false; }
  t.remove();
  return ok;
}

function copyBox(view, text) {
  const box = h('textarea.eo-copybox', { readOnly: true, rows: 4, 'aria-label': 'Text to copy', spellcheck: false });
  box.value = text;
  box.addEventListener('focus', () => box.select());
  const node = h('div.eo-copy', {}, box,
    h('div.eo-sub', {}, 'Copying was not allowed here. Select the text and copy it.'),
    h('button.btn', { type: 'button', onclick: () => closeMenu(true) }, 'Close'));
  const r = view.screen.getBoundingClientRect();
  popMenu({ title: 'Copy', node, cls: 'ctx', x: r.left + 8, y: r.top + 8, bounds: r, restore: () => view.screen.focus({ preventScroll: true }) });
  box.select();
}

export function copyText(view, text, done) {
  const finish = (ok) => { if (ok) toast(done, 'ok'); else copyBox(view, text); };
  if (navigator.clipboard && typeof navigator.clipboard.writeText === 'function') {
    let p;
    try { p = navigator.clipboard.writeText(text); } catch { return finish(legacyCopy(text)); }
    p.then(() => finish(true), () => finish(legacyCopy(text)));
    return;
  }
  finish(legacyCopy(text));
}

function short(s, n) {
  const t = String(s || '').replace(/\s+/g, ' ').trim();
  return t.length > n ? t.slice(0, n - 1).trimEnd() + '…' : t;
}

function hostOf(u) {
  try { return new URL(u).host || u; } catch { return u || ''; }
}

export function searchUrl(text) {
  const t = String(text || '').replace(/\s+/g, ' ').trim().slice(0, 300);
  return t ? 'https://duckduckgo.com/?q=' + encodeURIComponent(t).replace(/%20/g, '+') : '';
}

function external(what, url) {
  const info = chromeInfo();
  if (!info || !isWeb(url)) return [];
  const out = [];
  if (info.chrome) {
    const profiles = info.profiles || [];
    if (profiles.length > 1) {
      out.push({ name: 'Open ' + what + ' in Chrome', title: 'Open ' + what + ' in Chrome', items: profiles.map((p) => ({ name: p.name, sub: p.account, run: () => openIn('chrome', p.dir, url) })) });
    } else out.push({ name: 'Open ' + what + ' in Chrome', run: () => openIn('chrome', profiles[0]?.dir, url) });
  }
  if (info.safari) out.push({ name: 'Open ' + what + ' in Safari', run: () => openIn('safari', undefined, url) });
  return out;
}

function build(view, hit, pt) {
  const groups = [];
  const full = view.tabCount >= MAX_TABS;
  const tabItem = (name, url) => ({ name, run: () => view.openInTab(url), disabled: full, sub: full ? 'This browser has ' + MAX_TABS + ' tabs' : undefined });
  const link = hit?.link;
  if (link && link.href) {
    const g = [];
    if (link.open) {
      g.push({ name: 'Open link', run: () => view.go(link.href) });
      g.push(tabItem('Open link in new tab', link.href));
    } else g.push({ name: 'This link cannot be opened here', sub: link.why, disabled: true });
    g.push({ name: 'Copy link address', run: () => copyText(view, link.href, 'Link address copied') });
    if (link.open) g.push(...external('link', link.href));
    groups.push(g);
  } else if (link && !link.open && link.why) {
    groups.push([{ name: 'This link cannot be opened here', sub: link.why, disabled: true }]);
  }
  const img = hit?.image;
  if (img && img.src) {
    const g = [];
    if (img.open) g.push(tabItem('Open image in new tab', img.src));
    g.push({ name: 'Copy image address', run: () => copyText(view, img.src, 'Image address copied') });
    groups.push(g);
  }
  const sel = hit?.selection || '';
  const edit = (what, extra = {}) => view.ask({ op: 'edit', what, fx: pt.fx, fy: pt.fy, ...extra }, 8000).then((m) => { if (m.ok === false) toast(m.error || 'That did not work', 'error'); });
  if (hit?.editable) {
    const g = [];
    if (sel && !hit.readonly && !hit.password) g.push({ name: 'Cut', run: () => { copyText(view, sel, 'Cut'); edit('delete'); } });
    if (sel) g.push({ name: 'Copy', run: () => copyText(view, sel, 'Copied') });
    if (!hit.readonly) {
      probePaste();
      if (pasteOk) {
        g.push({ name: 'Paste', run: () => {
          let p;
          try { p = navigator.clipboard.readText(); } catch { p = Promise.reject(new Error('refused')); }
          p.then((t) => { if (t) return edit('paste', { text: t.slice(0, 20000) }); }, () => { pasteOk = false; toast('The browser did not allow reading the clipboard. Paste with the keyboard instead.', 'error'); });
        } });
      }
    }
    g.push({ name: 'Select all', run: () => edit('selectall') });
    groups.push(g);
  } else if (sel) {
    groups.push([{ name: 'Copy', run: () => copyText(view, sel, 'Copied') }]);
  }
  if (sel && !hit.password && searchUrl(sel)) groups.push([tabItem('Search the web for "' + short(sel, 28) + '"', searchUrl(sel))]);
  const m = view.meta;
  groups.push([
    { name: 'Back', run: () => view.op('back'), disabled: !m.back },
    { name: 'Forward', run: () => view.op('forward'), disabled: !m.fwd },
    { name: 'Reload', run: () => view.op('reload') },
  ]);
  if (!blank(m.url)) {
    groups.push([{ name: 'Copy page address', run: () => copyText(view, m.url, 'Page address copied') }, ...external('page', m.url), { name: 'View page source', run: () => view.viewSource() }]);
  }
  const items = [];
  for (const g of groups) {
    if (items.length) items.push('sep');
    items.push(...g);
  }
  return items;
}

export function showContextMenu(view, x, y) {
  if (!view.attached) return;
  const my = ++seq;
  const r = view.screen.getBoundingClientRect();
  const pt = {
    fx: Math.min(1, Math.max(0, (x - r.left) / Math.max(1, r.width))),
    fy: Math.min(1, Math.max(0, (y - r.top) / Math.max(1, r.height))),
  };
  const title = hostOf(view.meta.url) || 'Page';
  let handle = null;
  let answered = false;
  const show = (hit) => popMenu({ title, items: build(view, hit, pt), x, y, bounds: r, cls: 'ctx', restore: () => view.screen.focus({ preventScroll: true }) });
  const late = setTimeout(() => { if (!answered && my === seq) handle = show(null); }, 150);
  view.ask({ op: 'hit', ...pt }, 8000).then((m) => {
    answered = true;
    clearTimeout(late);
    if (my !== seq) return;
    const hit = m && m.ok !== false ? m : null;
    if (handle) handle.update(build(view, hit, pt));
    else handle = show(hit);
  });
}
