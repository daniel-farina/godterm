// One bottom sheet (a centered panel on desktop) for every dialog, and a
// small popover menu anchored where it was asked for (desktop menus).
import { $, h, emit } from './util.js';

let onClose = null;
export let sheetKind = '';

export function openSheet(...kids) {
  let opts = {};
  if (kids.length && kids[0] && !kids[0].nodeType && typeof kids[0] === 'object' && !Array.isArray(kids[0])) opts = kids.shift();
  if (pop && !pop.hidden) { pop.hidden = true; pop.replaceChildren(); }
  const body = $('#sheet-body');
  body.replaceChildren(...kids.flat().filter(Boolean));
  body.scrollTop = 0;
  if (onClose) { const f = onClose; onClose = null; f(); }
  onClose = opts.onClose || null;
  sheetKind = opts.kind || '';
  $('#sheet').classList.toggle('tall', !!opts.tall);
  $('#sheet').classList.toggle('wide', !!opts.wide);
  $('#sheet').classList.toggle('big', !!opts.big);
  $('#sheet').classList.add('on');
  $('#scrim').classList.add('on');
  emit('sheet', sheetKind);
}

let pop = null;
// A popover at `at` ({x, y}, viewport px), kept inside the window. Called
// again while open it replaces its content in place (a submenu).
export function openMenu(at, ...kids) {
  if (!pop) {
    pop = h('div', { id: 'pop', role: 'menu', hidden: true });
    document.body.append(pop);
    addEventListener('pointerdown', (e) => { if (!pop.hidden && !pop.contains(e.target)) closeSheet(); }, true);
    addEventListener('resize', () => !pop.hidden && closeSheet());
  }
  if ($('#sheet').classList.contains('on')) { $('#sheet').classList.remove('on'); $('#scrim').classList.remove('on'); }
  const wasOpen = !pop.hidden;
  pop.replaceChildren(...kids.flat().filter(Boolean));
  pop.hidden = false;
  sheetKind = 'menu';
  if (!wasOpen) pop.at = at;
  const a = pop.at || { x: 8, y: 8 };
  const w = pop.offsetWidth, hh = pop.offsetHeight;
  pop.style.left = Math.max(8, Math.min(a.x, innerWidth - w - 8)) + 'px';
  // `up`: it ends at a.y (a status bar item opens it above itself).
  pop.style.top = Math.max(8, Math.min(a.up ? a.y - hh : a.y, innerHeight - hh - 8)) + 'px';
  emit('sheet', sheetKind);
}
export const menuOpen = () => !!pop && !pop.hidden;

export function closeSheet() {
  if (pop && !pop.hidden) { pop.hidden = true; pop.replaceChildren(); }
  $('#sheet').classList.remove('on');
  // The drawer may still be open under the sheet: its scrim stays.
  $('#scrim').classList.toggle('on', $('#side').classList.contains('open'));
  const f = onClose; onClose = null; sheetKind = '';
  if (f) f();
  emit('sheet', '');
}

export const sheetOpen = () => $('#sheet').classList.contains('on') || menuOpen();
