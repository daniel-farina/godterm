// The keyboard on a desktop: F6 (Shift+F6 back) and Ctrl+` move the
// focus between the areas (the tab list, the pane shown, the status bar,
// the tab panel, the assistant, the toolbar), Shift+Escape leaves the
// terminal for its pane header, and "?" lists every shortcut. Without
// these the Tab key entered xterm's hidden textarea and never left it
// (xterm takes Tab for the tab). body.kbd says the keyboard moved the
// focus last: the focus rings (and the terminal's) show only then.
import { $, $$, h, icon, isDesk } from './util.js';
import { openSheet, closeSheet, sheetKind } from './sheet.js';

const FOCUSABLE = 'button:not([disabled]),[href],input:not([disabled]):not([type=hidden]),select:not([disabled]),textarea:not([disabled]),[tabindex]:not([tabindex="-1"])';
const visible = (el) => !!(el.offsetWidth || el.offsetHeight || el.getClientRects().length) && !el.closest('[hidden],[aria-hidden=true]');
export const focusables = (root) => $$(FOCUSABLE, root).filter(visible);

const typing = (el) => !!el && (el.tagName === 'TEXTAREA' || (el.tagName === 'INPUT' && !/^(checkbox|radio|button|file|range)$/.test(el.type)) || el.isContentEditable || !!el.closest?.('.xterm'));
const inTerm = (el) => !!el?.closest?.('.xterm');

// The areas in order: each names the element to land on, or null while
// it is not on screen.
const AREAS = [
  ['tabs', () => {
    if (isDesk() ? $('#app').classList.contains('noside') : !$('#side').classList.contains('open')) return null;
    return $('#list .row[aria-current=true]') || $('#list .row') || $('#q');
  }],
  ['pane', () => $('#panes .pane.focus .xterm-helper-textarea') || $('#panes .xterm-helper-textarea') || focusables($('#panes'))[0] || null],
  ['status', () => $('#sbar .sb-i')],
  ['panel', () => { const p = $('#tabpanel'); return p.hidden || !visible(p) ? null : focusables(p)[0] || null; }],
  ['assistant', () => ($('#app').classList.contains('chat-open') ? $('#say') : null)],
  ['toolbar', () => focusables($('#top .tr2'))[0] || $('#b-menu')],
];
const HOSTS = ['#side', '#panes', '#sbar', '#tabpanel', '#chat', '#top'];
const areaOf = (el) => (!el || el === document.body ? -1 : HOSTS.findIndex((s) => $(s)?.contains(el)));

export function land(el) {
  if (!el) return false;
  document.body.classList.add('kbd');
  el.focus();
  if (el.classList?.contains('row')) el.scrollIntoView({ block: 'nearest' });
  return document.activeElement === el;
}

// The next (dir 1) or previous (dir -1) area that is on screen.
export function cycle(dir) {
  const n = AREAS.length, cur = areaOf(document.activeElement);
  const from = cur < 0 ? (dir > 0 ? -1 : 0) : cur;
  for (let k = 1; k <= n; k++) {
    const i = (from + dir * k + 2 * n) % n;
    if (land(AREAS[i][1]())) return AREAS[i][0];
  }
  return '';
}

// Out of the terminal: to its pane header (a desktop), else the Tab button.
export function leaveTerm() {
  const pane = document.activeElement?.closest('.pane');
  const to = (pane && focusables(pane.querySelector('.pane-head') || pane)[0]) || $('#b-tabmenu') || $('#title');
  land(to);
}

const K = (...keys) => h('span.kh-k', {}, keys.map((k) => h('kbd', {}, k)));
const R = (keys, what) => h('div.kh-r', {}, K(...[].concat(keys)), h('span', {}, what));
const G = (label, rows) => [h('div.gl', {}, label), h('div.list.kh', {}, rows)];
const mac = /Mac|iPhone|iPad/.test(navigator.platform || '');

// The shortcuts, as a sheet ("?" or Settings > This browser > Keyboard).
export function helpSheet() {
  const desk = isDesk();
  openSheet({ kind: 'keys', tall: true },
    h('div.sh-title', {}, h('span', {}, 'Keyboard shortcuts'), h('button.ib', { 'aria-label': 'Close', title: 'Close', onclick: closeSheet, html: icon('x') })),
    h('div.mnote', {}, 'The terminal takes every key while it has the focus, Tab included. These three get you out of it.'),
    ...G('Moving around', [
      R(['F6'], 'Next area: tab list, terminal, status bar, tab panel, assistant, toolbar'),
      R(['Shift', 'F6'], 'Previous area'),
      R(['Ctrl', '`'], 'Next area (the same as F6)'),
      R(['Shift', 'Esc'], 'Leave the terminal for its pane header'),
      R(['Tab'], 'Next control; Shift+Tab the one before'),
      R(['Esc'], 'Close the sheet or menu that is open'),
      R(['?'], 'This list (not while typing)'),
    ]),
    ...G('The tab list', [
      R(['↑', '↓'], 'Previous or next tab row; Home and End the first or last'),
      R(['Enter'], 'Show the tab; Space the same'),
      R(['Shift', 'F10'], 'The row\'s options (a right click)'),
      desk ? R(['←', '→'], 'On the list\'s edge handle: narrower or wider (double click resets)') : null,
    ]),
    ...G('The terminal', [
      R(['Shift', 'Enter'], 'A new line in the prompt (Enter alone submits)'),
      R([mac ? 'Cmd' : 'Ctrl', 'V'], 'Paste; a copied screenshot goes to the tab as an image'),
      desk ? R(['Wheel'], 'Scroll back; an app that scrolls itself gets the notches') : null,
    ]),
    ...G('The assistant', [
      R(['Enter'], 'Send; Shift+Enter a new line'),
      R(['M'], 'Mute or unmute the mic while voice is on (outside a text field)'),
    ]),
    h('div.mnote', {}, 'Settings > This browser > Keyboard shortcuts opens this too.'));
}

export function initKeys() {
  addEventListener('keydown', (e) => {
    const tick = e.code === 'Backquote' && e.ctrlKey && !e.altKey && !e.metaKey;
    if (e.key === 'F6' || tick) { e.preventDefault(); e.stopPropagation(); cycle(e.shiftKey ? -1 : 1); return; }
    if (e.key === 'Escape' && e.shiftKey && inTerm(e.target)) { e.preventDefault(); e.stopPropagation(); leaveTerm(); return; }
    if (e.key === 'Tab' || e.key.startsWith('Arrow') || e.key === 'Home' || e.key === 'End') document.body.classList.add('kbd');
    if (e.key === '?' && !e.ctrlKey && !e.metaKey && !e.altKey && !typing(e.target)) {
      e.preventDefault();
      if (sheetKind === 'keys') closeSheet(); else helpSheet();
    }
  }, true);
  addEventListener('pointerdown', () => document.body.classList.remove('kbd'), { capture: true, passive: true });
}
