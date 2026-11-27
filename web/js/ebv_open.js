import { h, on, toast } from './util.js';
import { send } from './conn.js';

const CHROME_SVG = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 12L2.474 6.5A11 11 0 0 1 21.526 6.5Z" fill="#b5655c"/><path d="M12 12L21.526 6.5A11 11 0 0 1 12 23Z" fill="#cdb162"/><path d="M12 12L12 23A11 11 0 0 1 2.474 6.5Z" fill="#6f9a74"/><circle cx="12" cy="12" r="5.4" fill="#e8e6e1"/><circle cx="12" cy="12" r="3.9" fill="#6f8fb0"/></svg>';
const SAFARI_SVG = '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="10.2" fill="none" stroke="#6f8fb0" stroke-width="1.6"/><path d="M17.2 6.8L13.3 13.3 10.7 10.7Z" fill="#b5655c"/><path d="M6.8 17.2L10.7 10.7 13.3 13.3Z" fill="#a9a6a0"/></svg>';

const views = new Set();
const waits = new Map();
let info = null;
let seq = 0;
let menu = null;

on('msg:oext', (m) => {
  const w = m.id != null && waits.get(m.id);
  if (w) { waits.delete(m.id); w(m); }
});

function ask(frame) {
  return new Promise((res) => {
    const id = 'o' + (++seq) + '-' + Date.now();
    waits.set(id, res);
    if (!send({ t: 'oext', id, ...frame })) { waits.delete(id); return res({ ok: false, error: 'not connected to GodTerm' }); }
    setTimeout(() => { if (waits.has(id)) { waits.delete(id); res({ ok: false, error: 'GodTerm did not answer in time' }); } }, 15000);
  });
}

async function refresh() {
  const m = await ask({ op: 'list' });
  info = m.ok ? m : null;
  for (const v of views) v.paint();
}

on('conn', (s) => { if (s === 'live') refresh(); });
on('state', () => { if (!info) refresh(); else for (const v of views) v.paint(); });

export function closeMenu(back) {
  if (!menu) return;
  const { el, anchor, off, restore } = menu;
  menu = null;
  off();
  el.remove();
  anchor?.setAttribute('aria-expanded', 'false');
  if (back) (anchor || { focus: restore }).focus?.();
  else restore?.();
}

export const menuOpen = () => !!menu;

function place(el, o) {
  const w = el.offsetWidth, hh = el.offsetHeight;
  const vw = innerWidth, vh = innerHeight;
  if (o.anchor) {
    const r = o.anchor.getBoundingClientRect();
    el.style.left = Math.max(8, Math.min(r.right - w, vw - w - 8)) + 'px';
    el.style.top = Math.min(r.bottom + 4, Math.max(8, vh - hh - 8)) + 'px';
    return;
  }
  const fit = (pos, size, lo, hi, vmax) => {
    const area = hi - lo;
    if (size + 8 <= area) return Math.max(lo + 4, Math.min(pos, hi - size - 4));
    return Math.max(8, Math.min(pos, vmax - size - 8));
  };
  const b = o.bounds || { left: 0, top: 0, right: vw, bottom: vh };
  el.style.left = fit(o.x, w, b.left, b.right, vw) + 'px';
  el.style.top = fit(o.y, hh, b.top, b.bottom, vh) + 'px';
}

export function popMenu(o) {
  closeMenu();
  const el = h('div.eo-menu' + (o.cls ? '.' + o.cls : ''), { role: 'menu', 'aria-label': o.title });
  document.body.append(el);
  const state = { rows: [], stack: [] };
  const render = (title, items, back) => {
    const rows = [];
    const kids = [h('div.eo-title', {}, title)];
    if (back) {
      const r = h('button.eo-item.eo-back', { type: 'button', role: 'menuitem', onclick: back }, h('span.eo-name', {}, 'Back'));
      rows.push(r);
      kids.push(r);
    }
    let sep = false;
    for (const it of items) {
      if (it === 'sep') { sep = rows.length > 0 || kids.length > 1; continue; }
      if (sep) { kids.push(h('div.eo-sep', { role: 'separator' })); sep = false; }
      const sub = it.items ? () => {
        state.stack.push([title, items, back]);
        render(it.title || it.name, it.items, () => {
          const [t, i, b] = state.stack.pop();
          render(t, i, b);
          place(el, o);
          state.rows[0]?.focus();
        });
        place(el, o);
        state.rows[0]?.focus();
      } : null;
      const row = h('button.eo-item' + (it.items ? '.eo-more' : ''), {
        type: 'button', role: 'menuitem', disabled: !!it.disabled, 'aria-haspopup': it.items ? 'menu' : null,
        onclick: () => { if (sub) return sub(); closeMenu(); it.run(); },
      }, h('span.eo-name', {}, it.name), it.sub ? h('span.eo-sub', {}, it.sub) : null);
      kids.push(row);
      if (!it.disabled) rows.push(row);
    }
    if (o.foot) kids.push(h('div.eo-foot', {}, o.foot));
    el.replaceChildren(...kids);
    state.rows = rows;
  };
  if (o.node) el.replaceChildren(h('div.eo-title', {}, o.title), o.node);
  else render(o.title, o.items);
  place(el, o);
  o.anchor?.setAttribute('aria-expanded', 'true');
  const down = (e) => { if (!el.contains(e.target) && !(o.anchor && o.anchor.contains(e.target))) closeMenu(); };
  const key = (e) => {
    if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); closeMenu(true); return; }
    if (o.node) return;
    if (e.key === 'Tab') { closeMenu(); return; }
    if (e.key === 'ArrowLeft' && state.stack.length) { e.preventDefault(); state.rows[0]?.click(); return; }
    if (e.key === 'ArrowRight' && document.activeElement?.classList.contains('eo-more')) { e.preventDefault(); document.activeElement.click(); return; }
    if (e.key !== 'ArrowDown' && e.key !== 'ArrowUp' && e.key !== 'Home' && e.key !== 'End') return;
    e.preventDefault();
    const rows = state.rows;
    if (!rows.length) return;
    const i = rows.indexOf(document.activeElement);
    const n = rows.length;
    const j = e.key === 'Home' ? 0 : e.key === 'End' ? n - 1 : e.key === 'ArrowDown' ? (i + 1) % n : (i - 1 + n) % n;
    rows[j].focus();
  };
  const gone = () => closeMenu();
  document.addEventListener('pointerdown', down, true);
  document.addEventListener('keydown', key, true);
  addEventListener('resize', gone);
  addEventListener('blur', gone);
  menu = { el, anchor: o.anchor, restore: o.restore, off: () => {
    document.removeEventListener('pointerdown', down, true);
    document.removeEventListener('keydown', key, true);
    removeEventListener('resize', gone);
    removeEventListener('blur', gone);
  } };
  const handle = {
    el,
    live: () => menu && menu.el === el,
    update(items) {
      if (!handle.live()) return;
      const was = document.activeElement;
      const had = el.contains(was);
      state.stack = [];
      render(o.title, items);
      place(el, o);
      if (had) state.rows[0]?.focus();
    },
  };
  (o.node ? o.node.querySelector('textarea,button') : state.rows[0])?.focus();
  return handle;
}

function openMenu(anchor, title, items) {
  popMenu({ anchor, title, items, foot: 'Opens on the computer running GodTerm' });
}

export const chromeInfo = () => info;

export async function openIn(browser, profile, url) {
  const m = await ask({ op: 'open', browser, profile, url });
  if (m.ok) toast(m.message || 'Opened', 'ok');
  else toast(m.error || 'Could not open the page', 'error');
}

export function openExternal(getUrl) {
  const mk = (label, svg) => h('button.ib.eo-btn', { type: 'button', hidden: true, 'aria-label': label, title: label, 'aria-haspopup': 'menu', 'aria-expanded': 'false', html: svg });
  const chrome = mk('Open in Chrome', CHROME_SVG);
  const safari = mk('Open in Safari', SAFARI_SVG);
  const view = {
    paint() {
      const profiles = info?.profiles || [];
      chrome.hidden = !info?.chrome;
      safari.hidden = !info?.safari;
      chrome.title = chrome.ariaLabel = profiles.length > 1 ? 'Open in Chrome: choose a profile' : 'Open in Chrome (opens on the computer running GodTerm)';
      safari.title = safari.ariaLabel = 'Open in Safari (opens on the computer running GodTerm)';
      const bar = chrome.parentElement;
      if (bar) bar.classList.toggle('has-open', !chrome.hidden || !safari.hidden);
    },
  };
  const pick = (btn, browser, title, entries) => () => {
    const url = getUrl();
    if (!url || url === 'about:blank') return toast('There is no page to open yet', 'error');
    if (menu && menu.anchor === btn) return closeMenu(true);
    if (entries.length <= 1) return openIn(browser, entries[0]?.dir, url);
    openMenu(btn, title, entries.map((e) => ({ name: e.name, sub: e.account, run: () => openIn(browser, e.dir, url) })));
  };
  chrome.addEventListener('click', (e) => pick(chrome, 'chrome', 'Open in Chrome', info?.profiles || [])(e));
  safari.addEventListener('click', (e) => pick(safari, 'safari', 'Open in Safari', [{ name: 'Safari' }])(e));
  views.add(view);
  view.paint();
  if (!info) refresh();
  const frag = document.createDocumentFragment();
  frag.append(chrome, safari);
  return frag;
}
