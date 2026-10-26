// Small helpers shared by every module.
export const $ = (s, r = document) => r.querySelector(s);
export const $$ = (s, r = document) => [...r.querySelectorAll(s)];

const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
export const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ESC[c]);

// Element builder: h('button.btn', {onclick}, 'text', child)
export function h(sel, props, ...kids) {
  const [tag, ...cls] = sel.split('.');
  const el = document.createElement(tag || 'div');
  if (cls.length) el.className = cls.join(' ');
  for (const [k, v] of Object.entries(props || {})) {
    if (v == null || v === false) continue;
    if (k.startsWith('on')) el.addEventListener(k.slice(2), v);
    else if (k === 'html') el.innerHTML = v;
    else if (k in el && k !== 'list' && typeof v !== 'string') el[k] = v;
    else el.setAttribute(k, v === true ? '' : v);
  }
  for (const k of kids.flat()) if (k != null && k !== false) el.append(k.nodeType ? k : String(k));
  return el;
}

const P = {
  menu: '<path d="M4 7h16M4 12h16M4 17h16"/>',
  gear: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/>',
  history: '<path d="M4 12a8 8 0 1 0 2.4-5.7M4 4v4h4"/><path d="M12 8v4l3 2"/>',
  usage: '<path d="M5 20V11M12 20V5M19 20v-7"/>',
  chat: '<path d="M5 5h14v10H9l-4 4z"/>',
  check: '<path d="M12 3l7 3v5c0 5-3 8-7 10-4-2-7-5-7-10V6z"/><path d="M9 12l2 2 4-4"/>',
  more: '<circle cx="5" cy="12" r="1.2"/><circle cx="12" cy="12" r="1.2"/><circle cx="19" cy="12" r="1.2"/>',
  mic: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>',
  send: '<path d="M4 12l16-8-6 16-2-6z"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  x: '<path d="M6 6l12 12M18 6L6 18"/>',
  split: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M12 4v16"/>',
  search: '<circle cx="11" cy="11" r="6"/><path d="M20 20l-4.5-4.5"/>',
  dots: '<circle cx="12" cy="5" r="1.2"/><circle cx="12" cy="12" r="1.2"/><circle cx="12" cy="19" r="1.2"/>',
  image: '<rect x="4" y="5" width="16" height="14" rx="2"/><circle cx="9.5" cy="10" r="1.5"/><path d="M20 16l-5-5-8 8"/>',
  max: '<path d="M4 9V4h5M20 9V4h-5M4 15v5h5M20 15v5h-5"/>',
  edit: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="M13 7l4 4"/>',
  pin: '<path d="M9 4h6l-1 6 4 3H6l4-3z"/><path d="M12 13v7"/>',
  color: '<circle cx="12" cy="12" r="8"/><path d="M12 4a8 8 0 0 0 0 16z" fill="currentColor" stroke="none"/>',
  group: '<rect x="4" y="4" width="7" height="7" rx="1.5"/><rect x="13" y="4" width="7" height="7" rx="1.5"/><rect x="4" y="13" width="7" height="7" rx="1.5"/><rect x="13" y="13" width="7" height="7" rx="1.5"/>',
  move: '<path d="M5 12h13M13 6l6 6-6 6"/>',
  dup: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5H4v11h4"/>',
  copyto: '<path d="M12 4v11M7 10l5 5 5-5"/><path d="M5 20h14"/>',
  copy: '<rect x="9" y="9" width="11" height="11" rx="2"/><path d="M5 15V6a2 2 0 0 1 2-2h9"/>',
  desktop: '<rect x="3" y="4" width="18" height="12" rx="2"/><path d="M8 20h8M12 16v4"/>',
  model: '<rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 3v3M15 3v3M9 18v3M15 18v3M3 9h3M3 15h3M18 9h3M18 15h3"/>',
  clock: '<circle cx="12" cy="12" r="8"/><path d="M12 8v4l3 2"/>',
  stop: '<rect x="7" y="7" width="10" height="10" rx="1.5"/>',
  reopen: '<path d="M20 12a8 8 0 1 1-2.3-5.7"/><path d="M20 4v4h-4"/>',
  folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
  hash: '<path d="M5 9h14M5 15h14M10 4L8 20M16 4l-2 16"/>',
  ext: '<path d="M14 4h6v6M20 4l-9 9"/><path d="M18 14v6H4V6h6"/>',
  globe: '<circle cx="12" cy="12" r="8"/><path d="M4 12h16M12 4c2.7 2.7 2.7 13.3 0 16M12 4c-2.7 2.7-2.7 13.3 0 16"/>',
  archive: '<rect x="3" y="4" width="18" height="5" rx="1"/><path d="M5 9v11h14V9M10 13h4"/>',
  chev: '<path d="M9 6l6 6-6 6"/>',
  back: '<path d="M15 6l-6 6 6 6"/>',
  filter: '<path d="M4 6h16M7 12h10M10 18h4"/>',
  kbdown: '<rect x="3" y="3" width="18" height="11" rx="2"/><path d="M7 7h.01M11 7h.01M15 7h.01M7 10.5h10"/><path d="M9 18l3 3 3-3"/>',
};
export const icon = (n) => `<svg viewBox="0 0 24 24" aria-hidden="true">${P[n] || ''}</svg>`;
export function paintIcons(root = document) {
  for (const el of $$('[data-i]', root)) {
    if (el.dataset.ip) continue;
    el.dataset.ip = 1;
    el.insertAdjacentHTML('afterbegin', icon(el.dataset.i));
  }
}

// A tiny event bus.
const subs = {};
export const on = (ev, fn) => ((subs[ev] ||= []).push(fn), () => (subs[ev] = subs[ev].filter((f) => f !== fn)));
export const emit = (ev, ...a) => (subs[ev] || []).forEach((f) => { try { f(...a); } catch (e) { console.error(e); } });

// localStorage that never throws.
export const store = {
  get(k, d) { try { const v = localStorage.getItem('gt.' + k); return v == null ? d : JSON.parse(v); } catch { return d; } },
  set(k, v) { try { if (v == null) localStorage.removeItem('gt.' + k); else localStorage.setItem('gt.' + k, JSON.stringify(v)); } catch {} },
};

// A toast: tap or swipe it sideways to dismiss. On a desktop it sits in
// the layout (see #toasts); on a phone it floats above the key bar.
// `action`: {label, fn} or a list of them (buttons in a row). `ms`: how
// long it stays (0: until tapped, swiped or removed).
export function toast(text, level = 'info', action, ms) {
  const box = $('#toasts');
  // The same message again (copied twice): the shown one stays, no stack.
  // (An action toast keeps its own life: asked again it does not stay
  // longer, else a tip asked on every paint never goes.)
  for (const o of box.children) if (o.dataset.t === level + '|' + text && !o.classList.contains('gone')) { if (!action) { clearTimeout(o._life); o._life = setTimeout(() => o.remove(), ms ?? 4000); } return o; }
  const el = h('div.toast.' + level, { role: 'status', 'data-t': level + '|' + text }, h('span', {}, text));
  const gone = (dx = 0) => {
    if (!el.isConnected || el.classList.contains('gone')) return;
    el.classList.add('gone');
    el.style.transform = dx ? `translateX(${dx > 0 ? 110 : -110}%)` : '';
    setTimeout(() => el.remove(), 160);
  };
  for (const a of [action].flat().filter(Boolean)) el.append(h('button' + (a.cls ? '.' + a.cls : ''), { onclick: (e) => { e.stopPropagation(); a.fn(); gone(); } }, a.label));
  // With an action there is a plain x too (a tap elsewhere on it dismisses as well).
  if (action) el.append(h('button.tx', { type: 'button', 'aria-label': 'Dismiss', onclick: (e) => { e.stopPropagation(); gone(); }, html: icon('x') }));
  let x0 = null, dx = 0;
  el.addEventListener('pointerdown', (e) => { if (e.target.tagName !== 'BUTTON') { x0 = e.clientX; dx = 0; } });
  el.addEventListener('pointermove', (e) => {
    if (x0 == null) return;
    dx = e.clientX - x0;
    if (Math.abs(dx) > 6) { el.style.transform = `translateX(${dx}px)`; el.style.opacity = String(Math.max(0.2, 1 - Math.abs(dx) / 240)); }
  });
  const up = () => {
    if (x0 == null) return;
    x0 = null;
    if (Math.abs(dx) > 70) gone(dx);
    else if (Math.abs(dx) <= 6) gone();
    else { el.style.transform = ''; el.style.opacity = ''; }
  };
  el.addEventListener('pointerup', up);
  el.addEventListener('pointercancel', () => { x0 = null; el.style.transform = ''; el.style.opacity = ''; });
  box.append(el);
  while (box.children.length > 3) box.firstChild.remove();
  const life = ms ?? (action ? 8000 : 4000);
  if (life) el._life = setTimeout(() => gone(), life);
  return el;
}

export const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };

// "t3763" -> 3763 and back.
export const uidOf = (id) => (typeof id === 'number' ? id : parseInt(String(id).replace(/^t/, ''), 10) || 0);
export const idOf = (uid) => (typeof uid === 'string' && uid.startsWith('t') ? uid : 't' + uid);

// A strip that scrolls sideways (the tab list's chips): edge fades say
// there is more. A finger scrolls it natively; on a desktop a mouse drag
// or the wheel scrolls it too, and a drag is never taken as a click.
export function hscroll(el) {
  const paint = () => {
    const max = el.scrollWidth - el.clientWidth;
    el.classList.toggle('more-l', el.scrollLeft > 2);
    el.classList.toggle('more-r', el.scrollLeft < max - 2);
  };
  el.addEventListener('scroll', paint, { passive: true });
  new ResizeObserver(paint).observe(el);
  el.addEventListener('wheel', (e) => {
    if (el.scrollWidth <= el.clientWidth || Math.abs(e.deltaX) >= Math.abs(e.deltaY)) return;
    e.preventDefault();
    el.scrollLeft += e.deltaY * (e.deltaMode === 1 ? 16 : 1);
  }, { passive: false });
  let d = null, moved = false;
  el.addEventListener('pointerdown', (e) => {
    if (e.pointerType !== 'mouse' || e.button !== 0) return;
    d = { x: e.clientX, sl: el.scrollLeft, id: e.pointerId };
    moved = false;
  });
  el.addEventListener('pointermove', (e) => {
    if (!d || e.pointerId !== d.id) return;
    const dx = e.clientX - d.x;
    if (!moved && Math.abs(dx) < 5) return;
    if (!moved) { moved = true; el.setPointerCapture(e.pointerId); el.classList.add('dragging'); }
    el.scrollLeft = d.sl - dx;
  });
  const up = () => { d = null; el.classList.remove('dragging'); setTimeout(() => { moved = false; }, 0); };
  el.addEventListener('pointerup', up);
  el.addEventListener('pointercancel', up);
  el.addEventListener('click', (e) => { if (moved) { e.stopPropagation(); e.preventDefault(); } }, true);
  paint();
  return paint;
}

export const isTouch = () => matchMedia('(pointer:coarse)').matches;
// Wide and tall enough for the side list and split panes; a phone on its
// side (932 x 430) keeps the phone layout, the terminal using the width.
export const DESK = '(min-width:760px) and (min-height:560px)';
export const isDesk = () => matchMedia(DESK).matches;

// A tab's accent color (its own, else its group's: `color` in the tab's
// state), one of the muted swatches the terminal UI uses. tcStyle gives
// the inline `--tc` for an element; the CSS draws the stripe or the dot.
export const SWATCHES = ['sage', 'sand', 'slate', 'clay', 'mauve', 'stone', 'moss', 'teal', 'rose', 'dusk'];
export const tcOf = (t) => { const c = String(t?.color || '').toLowerCase(); return SWATCHES.includes(c) ? c : ''; };
export const tcStyle = (t) => tcOf(t) ? `--tc:var(--sw-${tcOf(t)})` : '';
export function tcPaint(el, t) {
  const c = tcOf(t);
  el.classList.toggle('tc', !!c);
  if (c) el.style.setProperty('--tc', `var(--sw-${c})`); else el.style.removeProperty('--tc');
}

export function pctClass(p) { return p == null ? '' : p < 5 ? 'out' : p < 25 ? 'lo' : ''; }

// Raw values in words, the same everywhere: a state key
// ("not_logged_in" -> "not logged in"), a tab's state as what the agent
// is doing ("approval" -> "waiting for an approval"), a cron line
// ("0 8 * * 1-5" -> "weekdays at 08:00"; '' when it is not one, so the
// caller falls back to the raw text).
export const words = (s) => String(s ?? '').replace(/[_-]+/g, ' ').replace(/\s+/g, ' ').trim();
const DOING = { approval: 'waiting for an approval', waiting: 'waiting', working: 'working', busy: 'busy', background: 'working in the background', starting: 'starting', ready: 'ready', ended: 'ended', error: 'stopped on an error', limit: 'out of usage' };
export const stateWords = (s) => DOING[String(s || '').split(' ')[0]] || words(s) || 'busy';
const DAYS = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'];
const MONTHS = ['', 'January', 'February', 'March', 'April', 'May', 'June', 'July', 'August', 'September', 'October', 'November', 'December'];
const DAYN = { sun: 0, mon: 1, tue: 2, wed: 3, thu: 4, fri: 5, sat: 6 };
export function cronWords(c) {
  const f = String(c || '').trim().split(/\s+/);
  if (f.length !== 5) return '';
  const [mi, hr, dom, mon, dow] = f;
  const num = (x) => /^\d+$/.test(x);
  const two = (n) => String(n).padStart(2, '0');
  const dayOf = (x) => (num(x) ? +x % 7 : DAYN[x.toLowerCase().slice(0, 3)]);
  const nth = (n) => n + (n % 10 === 1 && n !== 11 ? 'st' : n % 10 === 2 && n !== 12 ? 'nd' : n % 10 === 3 && n !== 13 ? 'rd' : 'th');
  let when = '';
  if (num(mi) && num(hr)) when = 'at ' + two(hr) + ':' + two(mi);
  else if (num(mi) && /^\d+(,\d+)+$/.test(hr)) when = 'at ' + hr.split(',').map((x) => two(x) + ':' + two(mi)).join(' and ');
  else if (/^\*\/\d+$/.test(mi) && hr === '*') when = 'every ' + mi.slice(2) + ' minutes';
  else if (num(mi) && /^\*\/\d+$/.test(hr)) when = 'every ' + hr.slice(2) + ' hours' + (+mi ? ' at :' + two(mi) : '');
  else if (num(mi) && hr === '*') when = 'every hour' + (+mi ? ' at :' + two(mi) : '');
  else if (mi === '*' && hr === '*') when = 'every minute';
  else return '';
  let day = '';
  if (dow !== '*') {
    const parts = dow.split(',').map((p) => {
      const r = p.split('-');
      if (r.length === 2) { const a = dayOf(r[0]), b = dayOf(r[1]); if (a == null || b == null) return null; if (a === 1 && b === 5) return 'weekdays'; const out = []; for (let d = a; ; d = (d + 1) % 7) { out.push(DAYS[d]); if (d === b) break; } return out.join(', '); }
      const d = dayOf(p); return d == null ? null : DAYS[d] + 's';
    });
    if (parts.some((x) => x == null)) return '';
    day = parts.join(' and ').replace(/^Saturdays and Sundays$|^Sundays and Saturdays$/, 'weekends');
  }
  if (dom !== '*') { if (!num(dom)) return ''; day = (day ? day + ', ' : '') + 'on the ' + nth(+dom); }
  if (mon !== '*') { if (!num(mon) || !MONTHS[+mon]) return ''; day = (day ? day + ' ' : '') + 'in ' + MONTHS[+mon]; }
  return (day ? day + ' ' : 'every day ' ) + when;
}

// A list that is loading: grey bars in the rows' shape (no words). `n`
// rows; '.sm' for the compact rows of the tab panel and popovers.
export function skeleton(n = 3, cls = '') {
  return h('div.skel' + cls, { 'aria-busy': 'true', 'aria-label': 'Loading' },
    Array.from({ length: n }, (_, i) => h('div.sk-r', {}, h('span.sk-b', { style: `width:${[58, 44, 66, 50][i % 4]}%` }), h('span.sk-b.sk-s', { style: `width:${[30, 38, 24, 34][i % 4]}%` }))));
}
// A request that failed: why, and Retry. A GodTerm older than the page
// ("unknown frame type", "unknown tool") says so instead of the raw error.
export function failRow(error, retry, what = 'This') {
  const e = String(error || 'It did not work');
  const old = /unknown (frame type|tool|git op)/i.test(e);
  return h('div.fail', { role: 'alert' },
    h('span.ft', {}, old ? `${what} needs a newer GodTerm: restart it to update.` : e),
    retry && !old ? h('button.btn.sm', { type: 'button', onclick: retry }, 'Retry') : null);
}
// An empty list: one line, a hint, and one thing to do about it.
export function emptyState(text, hint, action) {
  return h('div.empty', {}, h('div', {}, text), hint ? h('div.eh', {}, hint) : null,
    action ? h('button.btn.sm', { type: 'button', onclick: action.fn }, action.label) : null);
}

// Menus and sheets share one look: grouped lists with hairline rows
// (icon, label, a trailing value), small section labels.
export function li(ic, label, o = {}) {
  return h('button.li' + (o.bad ? '.bad' : '') + (o.on ? '.on' : ''), { type: 'button', role: 'menuitem', disabled: !!o.off, title: o.off || o.tip || '', onclick: o.fn },
    h('span', { html: icon(ic) }), h('span.ll', {}, label), o.hint ? h('span.lh', {}, o.hint) : null, o.chev ? h('span.lc', { html: icon('chev') }) : null);
}
export const group = (label, rows) => {
  rows = rows.flat().filter(Boolean);
  return rows.length ? [label ? h('div.gl', {}, label) : null, h('div.list', {}, rows)] : [];
};
