import { $, h, emit, isDesk } from './util.js';

let onClose = null;
export let sheetKind = '';

let opener = null;
const FOCUSABLE = 'button:not([disabled]),[href],input:not([disabled]):not([type=hidden]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';
const focusables = (root) => [...root.querySelectorAll(FOCUSABLE)].filter((el) => (el.offsetWidth || el.offsetHeight || el.getClientRects().length) && !el.closest('[hidden]'));
function takeOpener() {
  const a = document.activeElement;
  if (sheetOpen() || !a || a === document.body) return;
  if ($('#sheet').contains(a) || (pop && pop.contains(a))) return;
  opener = a;
}
function giveBack() {
  const o = opener, a = document.activeElement;
  opener = null;
  const inside = !a || a === document.body || $('#sheet').contains(a) || (pop && pop.contains(a));
  if (o && o.isConnected && inside) { try { o.focus({ preventScroll: true }); } catch {} }
}

function trap(box) {
  box.addEventListener('keydown', (e) => {
    if (e.key === 'Tab') {
      const f = focusables(box);
      if (!f.length) { e.preventDefault(); return; }
      const i = f.indexOf(document.activeElement);
      if (e.shiftKey && (i <= 0)) { e.preventDefault(); f[f.length - 1].focus(); }
      else if (!e.shiftKey && (i < 0 || i === f.length - 1)) { e.preventDefault(); f[0].focus(); }
      return;
    }
    if ((e.key === 'ArrowDown' || e.key === 'ArrowUp') && box.id === 'pop') {
      const f = focusables(box);
      if (!f.length) return;
      e.preventDefault();
      const i = f.indexOf(document.activeElement);
      f[(i + (e.key === 'ArrowDown' ? 1 : -1) + f.length) % f.length].focus();
    }
  });
}
trap($('#sheet'));

export function openSheet(...kids) {
  let opts = {};
  if (kids.length && kids[0] && !kids[0].nodeType && typeof kids[0] === 'object' && !Array.isArray(kids[0])) opts = kids.shift();
  if (pop && !pop.hidden) { pop.hidden = true; pop.replaceChildren(); }
  const body = $('#sheet-body');
  takeOpener();
  body.replaceChildren(...kids.flat().filter(Boolean));
  body.scrollTop = 0;
  if (onClose) { const f = onClose; onClose = null; f(); }
  onClose = opts.onClose || null;
  sheetKind = opts.kind || '';
  $('#sheet').classList.toggle('tall', !!opts.tall);
  $('#sheet').classList.toggle('wide', !!opts.wide);
  $('#sheet').classList.toggle('big', !!opts.big);

  $('#sheet').dataset.kind = sheetKind;
  $('#sheet').classList.add('on');
  $('#scrim').classList.add('on');

  if (!$('#sheet').contains(document.activeElement)) $('#sheet').focus({ preventScroll: true });
  emit('sheet', sheetKind);
}

let pop = null;

export function openMenu(at, ...kids) {
  if (!pop) {
    pop = h('div', { id: 'pop', role: 'menu', hidden: true, tabindex: -1 });
    document.body.append(pop);
    trap(pop);
    addEventListener('pointerdown', (e) => { if (!pop.hidden && !pop.contains(e.target)) closeSheet(); }, true);
    addEventListener('resize', () => !pop.hidden && closeSheet());
  }
  if ($('#sheet').classList.contains('on')) { $('#sheet').classList.remove('on'); $('#scrim').classList.remove('on'); }
  const wasOpen = !pop.hidden;
  takeOpener();
  pop.replaceChildren(...kids.flat().filter(Boolean));
  pop.hidden = false;
  sheetKind = 'menu';
  if (!wasOpen) pop.at = at;
  const a = pop.at || { x: 8, y: 8 };
  const w = pop.offsetWidth, hh = pop.offsetHeight;

  const cs = getComputedStyle(document.documentElement);
  const sab = parseFloat(cs.getPropertyValue('--sab')) || 0, sal = parseFloat(cs.getPropertyValue('--sal')) || 0, sar = parseFloat(cs.getPropertyValue('--sar')) || 0;
  pop.style.left = Math.max(8 + sal, Math.min(a.x, innerWidth - w - 8 - sar)) + 'px';

  pop.style.top = Math.max(8, Math.min(a.up ? a.y - hh : a.y, innerHeight - hh - 8 - sab)) + 'px';

  if (!pop.contains(document.activeElement)) (document.body.classList.contains('kbd') ? focusables(pop)[0] || pop : pop).focus({ preventScroll: true });
  emit('sheet', sheetKind);
}
export const menuOpen = () => !!pop && !pop.hidden;

export function closeSheet() {
  if (pop && !pop.hidden) { pop.hidden = true; pop.replaceChildren(); }
  $('#sheet').classList.remove('on');

  $('#scrim').classList.toggle('on', $('#side').classList.contains('open'));
  const f = onClose; onClose = null; sheetKind = '';
  giveBack();
  if (f) f();
  emit('sheet', '');
}

export const sheetOpen = () => $('#sheet').classList.contains('on') || menuOpen();

{
  const sheet = $('#sheet');
  let g = null;
  const scrollerOf = (el) => {
    for (; el && el !== sheet; el = el.parentElement) {
      const cs = getComputedStyle(el);
      if (/(auto|scroll)/.test(cs.overflowY) && el.scrollHeight > el.clientHeight + 1) return el;
    }
    return null;
  };
  sheet.addEventListener('touchstart', (e) => {
    g = null;

    if (e.touches.length !== 1 || !sheet.classList.contains('on') || isDesk()) return;
    if (e.target.closest('input[type=range], textarea, select, [data-nodrag]')) return;
    const t = e.touches[0];
    g = { x: t.clientX, y: t.clientY, ly: t.clientY, lt: performance.now(), v: 0, on: false, sc: scrollerOf(e.target) };
  }, { passive: true });
  sheet.addEventListener('touchmove', (e) => {
    if (!g) return;
    const t = e.touches[0], dx = t.clientX - g.x, dy = t.clientY - g.y, now = performance.now();
    if (!g.on) {
      if (Math.abs(dx) > 12 && Math.abs(dx) > Math.abs(dy)) { g = null; return; }
      if (dy <= 0) { if (dy < -8) g = null; return; }
      if (g.sc && g.sc.scrollTop > 0) { g = null; return; }

      if (e.cancelable) e.preventDefault();
      if (dy < 8) return;
      g.on = true;
      sheet.classList.add('drag');
    }
    if (e.cancelable) e.preventDefault();
    const off = Math.max(0, dy);
    sheet.style.setProperty('--sdy', off + 'px');
    g.v = 0.6 * ((t.clientY - g.ly) / Math.max(1, now - g.lt)) + 0.4 * g.v;
    g.ly = t.clientY; g.lt = now; g.dy = off;
  }, { passive: false });
  const end = () => {
    if (!g) return;
    const was = g;
    g = null;
    if (!was.on) return;
    sheet.classList.remove('drag');
    sheet.style.removeProperty('--sdy');
    if (was.dy > Math.min(sheet.offsetHeight / 3, 180) || (was.v > 0.3 && was.dy > 40)) closeSheet();
  };
  sheet.addEventListener('touchend', end);
  sheet.addEventListener('touchcancel', end);
}
