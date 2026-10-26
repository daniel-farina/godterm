// Terminal panes: one xterm.js per pane, fed by snapshot + live output.
// By default a pane keeps the tab's own cols x rows at this device's font
// size (A- and A+ change it), scaled down only when the tab would not fit,
// at the pane's top left; "take size" instead resizes the tab's PTY to
// this pane.
import { $, h, on, emit, store, toast, uidOf, isDesk, isTouch, DESK, paintIcons, tcPaint } from './util.js';
import { send, sendInput, KIND, conn, tool } from './conn.js';
import { openSheet, closeSheet } from './sheet.js';
import { S, tabById } from './state.js';
import { dlog } from './debuglog.js';
import { ownTextarea } from './textinput.js';

const THEME = {
  background: '#1b1d21', foreground: '#d9d6cf', cursor: '#c9a46a', cursorAccent: '#1b1d21',
  selectionBackground: '#4a4f57', black: '#2a2d33', red: '#c07a6e', green: '#8fae7e', yellow: '#c9a46a',
  blue: '#7f99ad', magenta: '#a68ba8', cyan: '#7fa8a0', white: '#d9d6cf', brightBlack: '#62666e',
  brightRed: '#d08f83', brightGreen: '#a4c193', brightYellow: '#d9b884', brightBlue: '#97afc1',
  brightMagenta: '#bba1bd', brightCyan: '#96bdb4', brightWhite: '#eeebe4',
};
const FONT = 'ui-monospace, "SF Mono", Menlo, "Cascadia Mono", "DejaVu Sans Mono", monospace';
const MINF = 5, MAXF = 30;
let wheelSaid = false;
let M = null, hinted = false, queuedSaid = false, cleared = 0, clearSaid = 0;
on('conn', (s) => { if (s === 'live') queuedSaid = false; });
on('input-lost', () => toast('Not connected for a while: some keys were not sent', 'warn'));
// "Fit text to this screen" (Settings > This browser; on by default on a
// phone): this phone sizes the tab it shows to its screen at its own font
// size, so zooming rewraps the text instead of making it overflow. Off,
// the tab keeps its size and the font is scaled down to fit it.
export const fitOn = () => !isDesk() && store.get('fitscreen', isTouch());
// A desktop sizes the tab it shows to its pane while it is in use (on by
// default): no Fit click, and the Mac's own pane gets it back when typed in.
export const deskFit = () => isDesk() && store.get('deskfit', true);
const autoOn = () => fitOn() || deskFit();
// The font size this device prefers (A- / A+; never scaled above it). A
// phone that fits the tab starts at 14 px: about 50 columns on a 430 px
// wide iPhone.
export const prefFont = () => store.get('fontpref', null) || (isDesk() ? 14 : fitOn() ? 14 : 13);
// A short name for this device, for the Mac's pane header ("sized by iPhone").
const DEVICE = /iPhone/.test(navigator.userAgent) ? 'iPhone' : /iPad|Macintosh.*Mobile/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1) ? 'iPad' : /Android/.test(navigator.userAgent) ? 'Android phone' : 'a browser';
// This phone is the tab's viewer while it is visible and was touched or
// typed on in the last 5 minutes; after that the tab's size goes back to
// the Mac until the next touch.
const IDLE_MS = 5 * 60000;
let activeAt = Date.now(), idleT = 0, hiddenT = 0, myClient = null;
const activeNow = () => !document.hidden && Date.now() - activeAt < IDLE_MS;
function markActive() {
  const was = activeNow();
  activeAt = Date.now();
  clearTimeout(idleT);
  idleT = setTimeout(() => emit('layout'), IDLE_MS + 500);
  if (!was) emit('layout');
}
addEventListener('touchstart', markActive, { capture: true, passive: true });
addEventListener('keydown', markActive, { capture: true, passive: true });
// Hidden for a minute: the sizes go back (iOS usually drops the socket
// sooner, which gives them back too).
document.addEventListener('visibilitychange', () => {
  clearTimeout(hiddenT);
  if (document.hidden) hiddenT = setTimeout(() => panes.list.forEach((p) => p.giveBack()), 60000);
  else markActive();
});
// When the on-screen keyboard last came or went (main.js toggles body.kb).
// The pane's height lags behind it for a few hundred ms (iOS drops the
// focus, and the class, before the visual viewport grows back), so sizes
// measured meanwhile are not the screen's: a closing keyboard once sent
// the tab 17 rows instead of 34, and Claude Code redrew at the bottom.
const KB_SETTLE = 900;
let kbAt = -1e9, kbWas = false;
new MutationObserver(() => {
  const k = document.body.classList.contains('kb');
  if (k !== kbWas) { kbWas = k; kbAt = performance.now(); }
}).observe(document.body, { attributes: true, attributeFilter: ['class'] });
// A zoom step (A- / A+, a pinch) resizes the tab only once the steps stop.
const ZOOM_SETTLE = 600;
// Names for the debug log.
const ENC = { DEFAULT: 'x10', SGR: 'sgr', SGR_PIXELS: 'sgr pixels' };

// The font's own advance and line height, per px of font size, measured
// at 1000 px. WebKit rounds a font's ascent and descent to whole pixels at
// the size asked for, so measured at 13 px SF Mono reports 17 px of line
// (1.31 em) instead of its real 1.19 em; xterm measures at the font size,
// which on iOS spread rows about 10% apart. At 1000 px the rounding is
// gone (Chrome and Firefox agree with it at any size).
function metrics() {
  if (M) return M;
  let w = 0, hh = 0;
  try {
    const c = document.createElement('canvas').getContext('2d');
    c.font = `1000px ${FONT}`;
    const m = c.measureText('W');
    w = m.width / 1000;
    hh = ((m.fontBoundingBoxAscent || 0) + (m.fontBoundingBoxDescent || 0)) / 1000;
  } catch {}
  if (!w || !hh) {
    const sp = document.createElement('span');
    sp.style.cssText = `position:absolute;visibility:hidden;white-space:pre;font-family:${FONT};font-size:100px;line-height:normal`;
    sp.textContent = 'W'.repeat(40);
    document.body.append(sp);
    const r = sp.getBoundingClientRect();
    sp.remove();
    w = w || r.width / 40 / 100;
    hh = hh || r.height / 100;
  }
  return (M = { w, h: hh });
}
// xterm's cell height comes from the same unrounded line height (its own
// measure keeps the width, which it does not round).
function trueLineHeight(t) {
  const cs = t._core?._charSizeService, st = cs?._measureStrategy;
  if (!st?.measure || st.gtPatched) return;
  const orig = st.measure.bind(st);
  st.measure = () => {
    const r = orig();
    const hh = metrics().h * t.options.fontSize;
    if (r && hh > 0) r.height = hh;
    return r;
  };
  st.gtPatched = true;
  cs.measure();
}
// Symbols iOS only has as color emoji (no text font there carries them),
// so Claude Code's "⏺" bullets came out as blue buttons: drawn as the
// plain shapes the Mac shows. Same UTF-8 length, so bytes swap in place.
const SYM = { 0xba: [0x97, 0x8f], 0xb8: [0x80, 0x96], 0xb9: [0x96, 0xa0] }; // U+23FA ⏺ -> ●, U+23F8 ⏸ -> ‖, U+23F9 ⏹ -> ■
const SYM_RE = /[\u23fa\u23f8\u23f9]/g, SYM_STR = { '\u23fa': '\u25cf', '\u23f8': '\u2016', '\u23f9': '\u25a0' };
const IOS = /iP(hone|ad|od)/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
export function plainSymbols(d) {
  if (!IOS || d == null) return d;
  if (typeof d === 'string') return d.replace(SYM_RE, (c) => SYM_STR[c]);
  let out = d;
  for (let i = d.indexOf(0xe2); i >= 0 && i + 2 < d.length; i = d.indexOf(0xe2, i + 1)) {
    const r = d[i + 1] === 0x8f && SYM[d[i + 2]];
    if (!r) continue;
    if (out === d) out = Uint8Array.from(d);
    out[i + 1] = r[0]; out[i + 2] = r[1];
  }
  return out;
}
// Copy text: the async clipboard where allowed (https, localhost), else
// the old execCommand path (works on http too, inside a tap).
export async function copyText(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch {}
  const ta = h('textarea', { style: 'position:fixed;top:0;left:0;opacity:0;font-size:16px', readonly: true });
  ta.value = text;
  document.body.append(ta);
  ta.select();
  let ok = false;
  try { ok = document.execCommand('copy'); } catch {}
  ta.remove();
  return ok;
}
// The phone keyboard's Return (Settings > This browser > Return key): a new
// line in the prompt by default on phones, as Shift+Enter is on the Mac,
// with Send (the key bar's right end) to submit. Each agent's own new
// line: ESC CR for Claude Code (what the terminal UI sends for
// Shift+Enter), Ctrl+J for Codex and OpenCode, else ESC CR as the Mac.
export const returnNewline = () => !isDesk() && isTouch() && store.get('retkey', 'newline') === 'newline';
const NEWLINE = { codex: '\n', opencode: '\n' };
export function newlineFor(id) {
  const t = tabById(id), a = t && S.accounts.find((x) => x.account === t.account);
  return NEWLINE[t?.harness || a?.harness] || '\x1b\r';
}
let inputHook = (d) => d;
export const setInputHook = (fn) => (inputHook = fn);

class Pane {
  constructor(i) {
    this.i = i;
    this.id = '';      // tab id "t123"
    this.cols = 0;
    this.rows = 0;
    this.ready = false;
    this.el = h('div.pane', { 'data-i': i });
    this.name = h('span.pn', { draggable: 'true' }, 'Empty pane');
    this.dot = h('span.dot');
    this.sz = h('span.sz');
    this.takeBtn = h('button.ib', { title: 'Take size: resize the tab to this pane', 'data-i': 'max', 'aria-pressed': 'false', onclick: () => this.toggleTake() });
    this.head = h('div.pane-head', {}, this.dot, this.name, this.sz,
      h('button.ib', { title: 'Smaller font', onclick: () => this.zoom(-1) }, 'A-'),
      h('button.ib', { title: 'Larger font', onclick: () => this.zoom(1) }, 'A+'),
      h('button.ib', { title: 'Back to this device\'s font size (smaller when the tab does not fit)', onclick: () => this.zoom(0) }, 'Fit'),
      this.takeBtn,
      h('button.ib', { title: 'Tab options', 'data-i': 'dots', onclick: (e) => { const r = e.currentTarget.getBoundingClientRect(); if (this.id) emit('tabmenu', this.id, { x: r.right - 320, y: r.bottom + 4 }); } }));
    this.host = h('div.xt');
    // Recent tabs' terminals, kept (hidden) on a phone so going back to
    // one shows it at once and asks GodTerm only for what it missed.
    this.stash = new Map();
    this.msg = h('div.pane-msg', {}, 'Pick a tab to watch it');
    // A tab much smaller than this pane: one click lets it fill it.
    this.hint = h('button.pane-hint', { hidden: true, onclick: (e) => { e.stopPropagation(); this.toggleTake(); } });
    // Touch: a small zoom control floating at the pane's top right
    // (fades after a few seconds, back on any touch).
    this.fzSize = h('span.fz-n', { 'aria-live': 'polite' });
    this.fz = h('div.fzoom', { role: 'toolbar', 'aria-label': 'Text size' },
      h('button', { type: 'button', 'aria-label': 'Smaller text', onclick: (e) => { e.stopPropagation(); this.step(-1); } }, 'A-'),
      this.fzSize,
      h('button', { type: 'button', 'aria-label': 'Larger text', onclick: (e) => { e.stopPropagation(); this.step(1); } }, 'A+'),
      this.fzFit = h('button.fz-fit', { type: 'button', 'aria-label': 'Fit the text to the screen', onclick: (e) => { e.stopPropagation(); this.step(0); } }, 'Fit'));
    this.body = h('div.pane-body', {}, this.host, this.msg, this.hint, this.fz);
    // GodTerm Sync: owned by another machine (read only), or moving there.
    this.lease = h('div.lease-bar', { hidden: true });
    this.el.append(this.head, this.lease, this.body);
    paintIcons(this.head);
    this.el.addEventListener('pointerdown', () => panes.focus(this.i), true);
    // A tap on the faded control only brings it back (no zoom, no
    // keyboard).
    this.body.addEventListener('touchstart', () => { if (this.yielded) { this.yielded = false; requestAnimationFrame(() => this.layout()); } }, { capture: true, passive: true });
    this.body.addEventListener('mousedown', () => { if (this.yielded && deskFit()) { this.yielded = false; requestAnimationFrame(() => this.layout()); } }, true);
    this.body.addEventListener('touchstart', (e) => { this.fzEat = this.fz.classList.contains('faded') && this.fz.contains(e.target); this.wake(); }, { capture: true, passive: true });
    this.fz.addEventListener('click', (e) => { if (this.fzEat) { this.fzEat = false; e.stopPropagation(); e.preventDefault(); } }, true);
    on('pane-font', (p) => p === this && this.paintZoom());
    this.wake();
    this.makeTerm();
    this.touch();
    this.dnd();
    // Not while the side list's edge is dragged: one refit when it ends.
    new ResizeObserver(() => { if (!document.documentElement.classList.contains('side-drag')) this.layout(); }).observe(this.body);
  }

  makeTerm() {
    if (!this.host.isConnected) this.body.prepend(this.host);
    const t = new Terminal({
      fontFamily: FONT, fontSize: 13, lineHeight: 1.0, scrollback: store.get('scrollback', 5000) || 5000, theme: THEME,
      allowProposedApi: true, macOptionIsMeta: true, cursorBlink: false, drawBoldTextInBrightColors: false,
      minimumContrastRatio: 1, rescaleOverlappingGlyphs: true, customGlyphs: true, cols: 80, rows: 24,
    });
    this.term = t;
    t.open(this.host);
    trueLineHeight(t);
    try { t.loadAddon(new Unicode11Addon.Unicode11Addon()); t.unicode.activeVersion = '11'; } catch {}
    try { t.loadAddon(new WebLinksAddon.WebLinksAddon((e, uri) => window.open(uri, '_blank', 'noopener'))); } catch {}
    this.fit = new FitAddon.FitAddon();
    t.loadAddon(this.fit);
    this.gpu();
    t.onData((d) => this.input(inputHook(d === '\r' && returnNewline() ? newlineFor(this.id) : d, this)));
    // Shift+Enter: a new line in the prompt, as in the terminal UI (xterm
    // alone sends CR, which submitted it).
    t.attachCustomKeyEventHandler((e) => {
      if (e.key !== 'Enter' || !e.shiftKey || e.ctrlKey || e.altKey || e.metaKey || e.isComposing) return true;
      if (e.type === 'keydown') { e.preventDefault(); this.input(inputHook(newlineFor(this.id), this)); }
      return false;
    });
    // Touch: the phone keyboard's text (typing, dictation, autocorrect,
    // predictive text, an IME) goes as what changed in the textarea, each
    // character once (textinput.js).
    if (isTouch()) t.gtMirror = ownTextarea(t, this.host, (d, how) => this.typed(d, how));
    t.onBinary((d) => this.input(Uint8Array.from(d, (c) => c.charCodeAt(0) & 255)));
    // An image pasted into the terminal (the phone's Paste, a Mac's
    // Cmd-V of a screenshot) goes to the tab as an image.
    this.host.addEventListener('paste', (e) => {
      const files = [...(e.clipboardData?.files || [])].filter((f) => f.type.startsWith('image/'));
      if (!files.length || !this.id) return;
      e.preventDefault(); e.stopPropagation();
      import('./images.js').then((m) => m.sendImages(files, this));
    }, true);
    // "Erase saved lines" (CSI 3 J) is ignored, as GodTerm's own screen
    // (vt100) ignores it: Claude Code sends it with its redraws, and xterm
    // honoured it, so the history the snapshot brought (and all output
    // since) was wiped and a phone could not scroll back at all, while the
    // Mac's terminal UI kept it. The screen itself is still cleared by 2 J.
    try {
      t.parser.registerCsiHandler({ final: 'J' }, (params) => {
        if (params[0] !== 3) return false;
        cleared++;
        if (!clearSaid || performance.now() - clearSaid > 60000) { clearSaid = performance.now(); dlog('term', `kept the history through ${cleared} "erase saved lines" so far`); }
        return true;
      });
    } catch {}
    t.onScroll(() => { this.paintJump(); if (this.held && !this.scrolledBack()) this.unhold(); });
    t.onWriteParsed(() => { this.paintJump(); this.showCursor(); });
    t.buffer.onBufferChange(() => this.paintJump());
  }

  // The WebGL renderer for the shown terminal (one GL context per pane:
  // a kept terminal gives its own back).
  gpu() {
    this.gl = null;
    if (store.get('webgl', true) && window.WebglAddon) {
      try {
        const g = new WebglAddon.WebglAddon();
        g.onContextLoss(() => { g.dispose(); if (this.gl === g) { this.gl = null; this.renderer = 'dom'; } });
        this.term.loadAddon(g);
        this.gl = g;
        this.renderer = 'webgl';
      } catch { this.renderer = 'dom'; }
    } else this.renderer = 'dom';
  }

  // Keep this tab's terminal aside (a phone, a tab whose stream is known)
  // and bring back `id`'s if it was kept. True when it was.
  swap(id) {
    const keep = !isDesk() && this.id && this.ready && this.sid && this.term;
    if (keep) {
      try { this.gl?.dispose(); } catch {}
      this.gl = null;
      this.host.hidden = true;
      this.stash.set(this.id, { term: this.term, host: this.host, fit: this.fit, sid: this.sid, pos: this.pos, cols: this.cols, rows: this.rows });
    }
    const c = id && this.stash.get(id);
    if (c) this.stash.delete(id);
    // At most three kept: the oldest goes.
    while (this.stash.size > 3) {
      const [k, o] = this.stash.entries().next().value;
      this.stash.delete(k);
      o.term.dispose(); o.host.remove();
    }
    if (c) {
      Object.assign(this, { term: c.term, host: c.host, fit: c.fit, cols: c.cols, rows: c.rows });
      this.host.hidden = false;
      this.term.options.theme = { ...THEME };
      this.gpu();
      return c;
    }
    if (keep) {
      this.host = h('div.xt');
      this.makeTerm();
    }
    return null;
  }

  // Whether this pane sets the tab's size now: "take size" by hand, or
  // fitting to this screen while this phone is the one in use.
  get sizing() { return this.take || (autoOn() && !!this.id && !this.yielded && activeNow()); }

  // The size this pane took goes back to the tab's own pane on the Mac.
  giveBack() {
    clearTimeout(this.rsT);
    if (!this.sent || !this.id) { this.sent = false; return; }
    send({ t: 'resize', tab: uidOf(this.id), release: true });
    this.sent = false;
  }

  // Text from the phone keyboard (textinput.js): `how` is text, del,
  // paste, enter or key.
  typed(d, how) {
    if (how === 'paste') { this.paste(d); return; }
    if (how === 'enter') { this.input(inputHook(returnNewline() ? newlineFor(this.id) : '\r', this)); return; }
    if (how === 'del') { this.input(d, true); return; }
    // A new line dictated or typed into the text: this tab's new line.
    const nl = how === 'text' && /\r?\n/.test(d);
    const txt = nl ? d.replace(/\r?\n/g, newlineFor(this.id)) : d;
    const out = inputHook(txt, this);
    this.input(out, true);
    // A latched Ctrl or Alt made it a control key: the textarea starts
    // again (the tab did not get that text).
    if (out !== txt) this.term.gtMirror?.reset();
    if (this.term.buffer.active.viewportY < this.term.buffer.active.baseY) this.term.scrollToBottom();
  }

  // `mirrored`: text from the textarea (textinput.js). Anything else (a
  // key, the key bar, Send, a paste) first sends what a composition holds
  // and starts the textarea again.
  input(d, mirrored = false) {
    if (!mirrored) this.term.gtMirror?.commit();
    if (!this.id || d == null || d === '') return;
    if (this.readOnly()) return;
    if (this.yielded) { this.yielded = false; this.layout(); }
    // Page keys page an app that scrolls itself (counted as a screen of
    // notches: "jump to the end" may send more than it needs, never
    // fewer); any other key brings it back to its end (Claude Code does).
    const pg = d === '\x1b[5~' ? 1 : d === '\x1b[6~' ? -1 : 0;
    if (pg && this.scrollPath() === 'app') {
      this.appBack = Math.max(0, (this.appBack || 0) + pg * Math.max(1, this.term.rows));
      dlog('term', `page ${pg > 0 ? 'up' : 'down'} to the app in ${this.id}, ${this.appBack} up now`);
      this.paintJump();
      if (!this.appBack) this.unhold();
    } else if (this.appBack) { this.appBack = 0; this.paintJump(); this.unhold(); }
    const r = sendInput(uidOf(this.id), d);
    if (r === 'dropped') toast('Not connected: input not sent', 'warn');
    else if (r === 'queued' && !queuedSaid) { queuedSaid = true; toast('Reconnecting: your keys go out once it is back', 'info'); }
  }

  // Paste text into the tab (bracketed when the app asked for it).
  paste(text) { if (this.id) this.term.paste(text); }

  // Paste, then Enter as its own write once the tab shows the text (or
  // after 1.5 s): an agent CLI that reads the paste and the CR together
  // takes the CR as a new line in the paste (Claude Code), or the whole
  // as a paste (Codex), and the prompt is not sent.
  async pasteThenEnter(text) {
    const id = this.id;
    if (!id) return;
    this.paste(text);
    const tail = String(text).replace(/\s+/g, ' ').trim().slice(-20);
    const t0 = performance.now();
    while (performance.now() - t0 < 1500) {
      await new Promise((r) => setTimeout(r, 60));
      if (this.id !== id) return;
      if (tail && this.screenText().replace(/\s+/g, ' ').includes(tail)) break;
    }
    await new Promise((r) => setTimeout(r, 60));
    if (this.id === id) this.input('\r');
  }

  // The text on the screen now (the rows shown, not the history).
  screenText() {
    const b = this.term.buffer.active, out = [];
    for (let i = b.baseY; i < b.baseY + this.term.rows; i++) out.push(b.getLine(i)?.translateToString(true) || '');
    return out.join('\n');
  }

  setTab(id) {
    if (this.id === id) return;
    const typing = this.host.contains(document.activeElement);
    this.giveBack();
    this.yielded = false;
    this.downRows = 0;
    this.appBack = 0;
    this.held = false;
    const kept = this.swap(id || '');
    this.id = id || '';
    this.ready = false;
    this.sid = null;
    this.from = kept ? { sid: kept.sid, pos: kept.pos } : null;
    if (!kept) this.term.reset();
    this.body.scrollLeft = this.body.scrollTop = 0;
    this.take = !!store.get('take.' + this.id, false);
    this.takeBtn.setAttribute('aria-pressed', this.take);
    this.msg.textContent = this.id ? 'Loading...' : 'Pick a tab to watch it';
    this.msg.hidden = !!kept;
    this.paintHead();
    if (kept) this.layout();
    // The keyboard stays up across a switch.
    if (typing && !this.host.contains(document.activeElement)) this.term.focus();
  }

  paintHead() {
    const t = tabById(this.id);
    this.name.textContent = t ? t.name : this.id ? this.id : 'Empty pane';
    this.dot.className = 'dot ' + (t ? t.state : '');
    tcPaint(this.el, t);
    this.sz.textContent = this.cols ? `${this.cols}x${this.rows}` : '';
    if (this.id && !t && S.tabs.length) { this.msg.textContent = 'This tab is closed'; this.msg.hidden = false; }
    // A tab that is not running (not started: no login yet; ended) has
    // nothing on screen: say so instead of a blank terminal.
    else if (t && this.ready && t.running === false && !this.screenText().trim()) {
      this.msg.textContent = `Not running (${t.state || 'stopped'}): its options can reopen it`;
      this.msg.hidden = false;
      this.idleMsg = true;
    } else if (this.idleMsg && this.ready) { this.idleMsg = false; this.msg.hidden = true; }
    this.paintLease(t);
  }

  // The lease banner: who owns this conversation and the ways to get it.
  paintLease(t) {
    const l = t?.lease;
    const key = l ? JSON.stringify(l) : '';
    if (key === this.leaseKey) return;
    this.leaseKey = key;
    if (!l) { this.lease.hidden = true; this.lease.replaceChildren(); return; }
    const uid = uidOf(this.id);
    const act = async (op, done) => {
      const r = await tool('sync_lease', { tab: uid, op });
      if (r.ok === false || r.error) toast(r.error || r.result?.error || 'It did not work', 'error');
      else toast(r.result?.say || done, 'ok');
    };
    const btn = (label, fn) => h('button.btn.sm', { type: 'button', onclick: (e) => { e.stopPropagation(); fn(); } }, label);
    const takeOver = () => {
      const yes = () => { closeSheet(); act('take_over', 'Taking it over'); };
      openSheet({ kind: 'confirm' }, h('div.sh-title', {}, h('span', {}, 'Take over')),
        h('div.appr-q', {}, `Take over this conversation from ${l.owner || 'the other machine'} now? It stops writing it there; anything it never pushed becomes a fork.`),
        h('div.appr-btns', {}, h('button.btn.pri', { onclick: yes }, 'Yes'), h('button.btn', { onclick: closeSheet }, 'No')));
    };
    const kids = [];
    if (l.state === 'waiting') kids.push(h('div.lb-t', {}, 'Checking who owns this conversation...'));
    else if (l.state === 'read_only') {
      const head = l.resumable && l.note ? '' : l.owner ? `Owned by ${l.owner}${l.active && l.active !== 'no heartbeat seen' ? ', ' + l.active : ''}` : '';
      if (head) kids.push(h('div.lb-t', {}, head));
      if (l.note) kids.push(h('div.lb-n', {}, l.note));
      if (l.offline) kids.push(h('div.lb-n.warn', {}, 'Offline: as last seen'));
      if (l.busy) kids.push(h('div.lb-n', {}, l.busy));
      const btns = [];
      if (l.resumable) btns.push(btn('Resume here', () => act('resume', 'Resuming here')));
      else {
        if (l.requested) btns.push(h('span.lb-n', {}, `Asked ${l.owner || 'the owner'} for it`));
        else btns.push(btn('Request take over', () => act('request', 'Asked for it')));
        btns.push(btn('Take over now', takeOver));
        if (l.offline) btns.push(btn('Work offline anyway', () => act('offline', 'Working offline')));
      }
      if (l.fork) btns.push(btn('Open fork ' + String(l.fork).slice(0, 8), () => act('open_fork', 'Opened the fork')));
      kids.push(h('div.lb-b', {}, btns));
    } else kids.push(h('div.lb-t', {}, String(l.banner || '').trim() || 'This conversation is moving to another machine'));
    this.lease.className = 'lease-bar ' + l.state;
    this.lease.replaceChildren(...kids);
    this.lease.hidden = false;
  }

  // Typing goes nowhere while another machine has the conversation.
  readOnly() {
    const l = tabById(this.id)?.lease;
    if (!l || l.state === 'offline') return false;
    const now = Date.now();
    if (now - (this.roSaid || 0) > 3000) {
      this.roSaid = now;
      toast(l.state === 'read_only' ? `Read only: ${l.owner || 'another machine'} has this conversation` : 'Input is blocked while this conversation moves', 'warn');
    }
    return true;
  }

  snapshot(m) {
    // Where the output after it starts (for a resume after a lost
    // connection).
    this.sid = m.sid || null;
    this.pos = m.pos || 0;
    this.from = null;
    this.cols = m.cols || this.cols || 80;
    this.rows = m.rows || this.rows || 24;
    this.term.reset();
    this.term.resize(this.cols, this.rows);
    this.ready = true;
    this.appBack = 0;
    // The overlay stays until xterm has drawn it all (a long history is
    // parsed over several frames).
    const gen = (this.gen = (this.gen || 0) + 1);
    this.term.write(plainSymbols(m.data || ''), () => {
      // A fresh copy starts at its end (a resize while it was drawn could
      // leave it a line up, which held this phone's resizes).
      if (this.gen === gen) this.term.scrollToBottom();
      if (this.gen === gen && this.ready) { this.msg.hidden = true; this.wake(); }
      const b = this.term.buffer.active;
      dlog('term', `snapshot ${this.id} ${this.cols}x${this.rows}: ${b.type} buffer, ${b.baseY} history lines`);
    });
    this.paintHead();
    this.layout();
  }

  output(bytes) {
    if (!this.ready) return;
    this.pos += bytes.length;
    this.term.write(plainSymbols(bytes));
  }

  // Back after a lost connection with the terminal as it was: GodTerm
  // sends only what this pane missed (no reset, no flash). False when
  // this pane's copy does not end where the resume starts.
  resume(m) {
    const f = this.from;
    this.from = null;
    if (!f || f.sid !== m.sid || f.pos !== m.pos || (m.cols && m.cols !== this.cols) || (m.rows && m.rows !== this.rows)) return false;
    this.sid = m.sid;
    this.pos = m.pos;
    this.ready = true;
    this.msg.hidden = true;
    return true;
  }

  // The tab was resized (by this phone, the Mac or another browser):
  // the terminal takes the size and output goes on, no new snapshot.
  resized(m) {
    if (!this.ready) return;
    if (m.sid) { this.sid = m.sid; this.pos = m.pos || 0; }
    if (m.cols !== this.term.cols || m.rows !== this.term.rows) this.term.resize(m.cols, m.rows);
    this.cols = m.cols; this.rows = m.rows;
    this.paintHead();
    this.layout();
  }

  // How a one finger drag scrolls this tab: "local" (this terminal's own
  // history) or "app" (wheel notches to the app, which scrolls itself),
  // as the terminal UI's wheel decides: an alternate screen app, or one
  // that reports the mouse and has no history here (Claude Code), gets
  // the wheel; a screen with history scrolls it.
  scrollPath() {
    const buf = this.term.buffer.active, mouse = this.term.modes.mouseTrackingMode !== 'none';
    return buf.type === 'alternate' || (mouse && buf.baseY === 0) ? 'app' : 'local';
  }

  // `lines` wheel notches (positive: up, towards older text) for the app
  // at viewport point (x, y): added up and sent once a frame.
  wheel(lines, x, y) {
    if (!this.id || !lines) return;
    this.wq = (this.wq || 0) + lines;
    if (x != null) this.wxy = [x, y];
    if (this.wraf) return;
    this.wraf = requestAnimationFrame(() => {
      this.wraf = 0;
      const q = this.wq;
      this.wq = 0;
      if (!q || !this.id) return;
      const [x0, y0] = this.wxy || [0, 0];
      const c = this.cellAt(x0, y0), up = q > 0;
      const row = c.row - this.term.buffer.active.viewportY;
      for (let left = Math.abs(q); left > 0; left -= 64) send({ t: 'wheel', tab: uidOf(this.id), up, n: Math.min(64, left), col: c.col, row });
      this.ws = this.ws || { up: 0, down: 0, msgs: 0 };
      this.ws[up ? 'up' : 'down'] += Math.abs(q);
      this.ws.msgs++;
      // How far up the app is (for "jump to the end" and holding resizes).
      this.appBack = Math.max(0, (this.appBack || 0) + q);
      this.paintJump();
      if (!this.appBack) this.unhold();
    });
  }

  // While scrolled back (this terminal's history, or the app's own),
  // nothing resizes the tab: a resize makes the app redraw at the bottom.
  scrolledBack() {
    const buf = this.term.buffer.active;
    return (buf.type === 'normal' && buf.viewportY < buf.baseY) || (this.appBack > 0 && this.scrollPath() === 'app');
  }

  // Back at the end: a resize held meanwhile goes out now.
  unhold() {
    if (!this.held) return;
    this.held = false;
    dlog('term', `back at the end of ${this.id}: the held resize goes out`);
    this.layout();
  }

  // The debug log: how the last drags scrolled (at most every 2 s).
  saidScroll(force) {
    const now = performance.now();
    if (!force && now - (this.scrollSaid || 0) < 2000) return;
    this.scrollSaid = now;
    const bf = this.term.buffer.active, m = this.term.modes.mouseTrackingMode;
    let enc = '';
    try { enc = ENC[this.term._core.coreMouseService.activeEncoding] || this.term._core.coreMouseService.activeEncoding; } catch {}
    const w = this.ws;
    this.ws = null;
    if (w) dlog('term', `scrolled ${this.id}: wheel to the app (${bf.type} buffer, ${bf.baseY} history lines, mouse ${m}${enc ? ' ' + enc : ''}): ${w.up} up, ${w.down} down in ${w.msgs} sends, ${this.appBack || 0} up now`);
    else dlog('term', `scrolled ${this.id}: local history (${bf.type} buffer), at ${bf.viewportY} of ${bf.baseY} history lines, mouse ${m}`);
  }

  cell() {
    const d = this.term._core?._renderService?.dimensions?.css?.cell;
    const f = this.term.options.fontSize;
    return d && d.width ? { w: d.width, h: d.height } : { w: f * 0.6, h: f * 1.2 };
  }

  setFont(f) {
    if (this.term.options.fontSize === f) return;
    // At the end before: at the end after. xterm keeps the viewport's
    // pixel offset, so taller rows left a tab with history a few lines
    // "scrolled back", and the resize the zoom asks for was held.
    // Put right once xterm has redrawn at the new size (scrolling in
    // the middle of the change drew rows the buffer did not have).
    const b = this.term.buffer.active, atEnd = this.ready && b.baseY > 0 && b.viewportY >= b.baseY;
    this.term.options.fontSize = f;
    if (atEnd) {
      requestAnimationFrame(() => requestAnimationFrame(() => {
        const c = this.term.buffer.active;
        if (this.ready && c.viewportY < c.baseY) this.term.scrollToBottom();
      }));
    }
    cancelAnimationFrame(this.anc);
    this.anc = requestAnimationFrame(() => this.anchor());
  }

  // The largest font (quarter px steps) up to `max` whose predicted cells
  // fit W x H. xterm floors the cell width and ceils its height in device
  // pixels.
  fitFont(W, H, max = MAXF) {
    const cols = this.cols || 80, rows = this.rows || 24, m = metrics(), dpr = devicePixelRatio || 1;
    const fits = (f) => cols * (Math.floor(m.w * f * dpr) / dpr) <= W && rows * (Math.ceil(m.h * f * dpr) / dpr) <= H;
    let f = Math.min(MAXF, max);
    while (f > MINF && !fits(f)) f -= 0.25;
    this.setFont(f);
    // Check against xterm's real cell once it has measured.
    cancelAnimationFrame(this.chk);
    this.chk = requestAnimationFrame(() => {
      for (let n = 0; n < 8 && !this.take; n++) {
        const c = this.cell();
        if ((cols * c.w <= W + 0.5 && rows * c.h <= H + 0.5) || this.term.options.fontSize <= MINF) break;
        if (store.get('font.' + this.id, null)) break;
        this.setFont((this.fitted = this.term.options.fontSize - 0.25));
      }
      this.anchor();
    });
    return f;
  }

  // The terminal's own padding (an even inner margin, wider on touch).
  pad() {
    if (this.pads) return this.pads;
    const cs = this.term.element && getComputedStyle(this.term.element);
    const x = cs ? parseFloat(cs.paddingLeft) + parseFloat(cs.paddingRight) : 4, y = cs ? parseFloat(cs.paddingTop) + parseFloat(cs.paddingBottom) : 4;
    return (this.pads = { x: x || 0, y: y || 0 });
  }

  layout() {
    const P = this.pad();
    const W = this.body.clientWidth - P.x;
    let H = this.body.clientHeight - P.y;
    if (W < 20 || H < 20) return;
    // With the keyboard up the text keeps its size: the pane pans to the
    // cursor instead of shrinking the font.
    const kb = document.body.classList.contains('kb');
    if (!kb) { this.fullH = H; this.fullBH = this.body.clientHeight; } else if (!this.take && this.fullH > H) H = this.fullH;
    // Fitting to this screen, the font is this device's own (A- / A+, a
    // pinch): never scaled down; the tab is sized to it instead.
    const user = this.id && !fitOn() ? store.get('font.' + this.id, null) : null;
    if (this.sizing) {
      this.setFont(user || prefFont());
      // The fit addon measures the host, so the host spans the pane here.
      this.host.style.cssText = 'right:0;bottom:0';
      this.body.classList.remove('roomy');
      const d = this.fit.proposeDimensions();
      // With the keyboard up, and for a moment after it came or went, the
      // tab keeps the rows it had without it (the pane pans to the
      // cursor): no resize each time it comes and goes.
      const since = performance.now() - kbAt, settling = since < KB_SETTLE;
      if (d && (kb || settling)) {
        if (this.downRows) d.rows = this.downRows;
        else if (kb) {
          // Shown with the keyboard already up: the rows of the screen
          // without it (iOS shrinks only the visual viewport).
          const vv = window.visualViewport, gap = vv ? Math.max(0, innerHeight - vv.height) : 0;
          d.rows = Math.max(d.rows, Math.floor((H + gap) / Math.max(1, this.cell().h)));
        }
        if (settling && !kb) { clearTimeout(this.kbT); this.kbT = setTimeout(() => this.layout(), KB_SETTLE - since + 50); }
      } else if (d) this.downRows = d.rows;
      if (d && d.cols > 1 && d.rows > 1 && this.ready && (d.cols !== this.cols || d.rows !== this.rows || !this.sent)) {
        // Settled first (a pinch, a rotation, the keyboard sliding); zoom
        // steps resize once, after the last one.
        const zoomed = performance.now() - (this.zoomAt || -1e9) < ZOOM_SETTLE;
        clearTimeout(this.rsT);
        this.rsT = setTimeout(() => {
          if (!this.sizing || !this.ready) return;
          if (performance.now() - (this.zoomAt || -1e9) < ZOOM_SETTLE - 20) { this.layout(); return; }
          // Not while scrolled back: the app would redraw at its end.
          if ((d.cols !== this.cols || d.rows !== this.rows) && this.scrolledBack()) {
            if (!this.held) dlog('term', `resize of ${this.id} to ${d.cols}x${d.rows} held: scrolled back`);
            this.held = true;
            return;
          }
          this.held = false;
          if (d.cols !== this.cols || d.rows !== this.rows) { this.cols = d.cols; this.rows = d.rows; this.term.resize(d.cols, d.rows); }
          send({ t: 'resize', tab: uidOf(this.id), cols: d.cols, rows: d.rows, by: DEVICE });
          this.sent = true;
          this.sentAt = Date.now();
          this.paintHead();
        }, zoomed ? ZOOM_SETTLE : 250);
      }
      this.fitted = this.term.options.fontSize;
    } else if (fitOn()) {
      // Fitting, but not the tab's viewer now (idle, or the Mac took it
      // back): the font stays as it is and the pane pans.
      this.giveBack();
      this.setFont(prefFont());
      this.fitted = prefFont();
      this.anchor();
    } else {
      this.fitted = this.fitFont(W, H, prefFont());
      if (user && user !== this.fitted) this.setFont(user);
      this.anchor();
    }
    this.paintHint(W, H);
    this.term.refresh(0, this.term.rows - 1);
    if (this.ready && !this.take && !user && this.fitted < 8 && !isDesk() && !hinted) {
      hinted = true;
      toast('Small text: pinch or A+ to zoom, or Size to fit the tab to this screen', 'info', { label: 'Size', fn: () => this.toggleTake() });
    }
    // Zoomed past the fit: the pane pans (touch drag) instead of shrinking.
    this.clampPan();
    this.showCursor();
    emit('pane-font', this);
  }

  // On a phone a tab with fewer rows than the screen holds sits at the
  // bottom, against the key bar, like a chat: its prompt stays where the
  // thumb is, and the empty part is above the oldest line, not between the
  // prompt and the keys. Desktop panes keep the top left.
  anchor() {
    if (this.sizing) return;
    const th = this.term.element?.offsetHeight || 0;
    const H = document.body.classList.contains('kb') ? this.fullBH || this.body.clientHeight : this.body.clientHeight;
    const top = !isDesk() && th ? Math.max(0, Math.floor(H - th)) : 0;
    this.host.style.cssText = top ? `top:${top}px` : '';
    // The size hint goes in that empty top part, and only when there is one.
    this.body.classList.toggle('roomy', top >= 60);
  }

  // Keep the cursor row on screen while the keyboard is up.
  showCursor() {
    if (!document.body.classList.contains('kb') || !this.ready) return;
    const b = this.body, c = this.cell(), buf = this.term.buffer.active;
    if (buf.viewportY !== buf.baseY) return;
    const scr = this.term.element?.querySelector('.xterm-screen');
    const y0 = scr ? scr.getBoundingClientRect().top - b.getBoundingClientRect().top + b.scrollTop : 0;
    const bottom = y0 + (buf.cursorY + 1) * c.h + 6, top = y0 + buf.cursorY * c.h;
    if (bottom > b.scrollTop + b.clientHeight) b.scrollTop = bottom - b.clientHeight;
    else if (top < b.scrollTop) b.scrollTop = Math.max(0, top - 4);
  }

  // "Use this window's size" while the tab, at its natural size, leaves
  // much of the pane empty; "Give the size back" while this pane sets it.
  paintHint(W, H) {
    let txt = '';
    if (this.take) txt = this.sent ? 'Give the size back' : '';
    else if (this.ready && this.id && (!autoOn() || this.yielded)) {
      const f = this.term.options.fontSize, m = metrics(), dpr = devicePixelRatio || 1;
      const w = this.cols * (Math.floor(m.w * f * dpr) / dpr), hh = this.rows * (Math.ceil(m.h * f * dpr) / dpr);
      if (f >= prefFont() - 0.01 && w * hh < 0.6 * W * H && (W - w > 160 || H - hh > 120)) txt = 'Use this window\'s size';
    }
    this.hint.textContent = txt;
    this.hint.title = this.take ? 'The tab goes back to the terminal UI\'s size' : 'Resize the tab to fill this pane (the terminal UI gets it back when you release it)';
    this.hint.hidden = !txt;
    this.hint.classList.toggle('on', this.take);
  }

  clampPan() {
    const b = this.body;
    b.scrollLeft = Math.min(b.scrollLeft, Math.max(0, b.scrollWidth - b.clientWidth));
    b.scrollTop = Math.min(b.scrollTop, Math.max(0, b.scrollHeight - b.clientHeight));
  }

  // The floating control: shown, then faded after a few quiet seconds.
  wake() {
    this.fz.classList.remove('faded');
    clearTimeout(this.fzT);
    this.fzT = setTimeout(() => this.fz.classList.add('faded'), 4000);
  }

  paintZoom() {
    const f = this.term.options.fontSize, user = fitOn() ? store.get('fontpref', null) : this.id && store.get('font.' + this.id, null);
    this.fzSize.textContent = String(Math.round(f * 4) / 4);
    this.fz.classList.toggle('user', !!user);
    // Fitting, the last button puts the default size back; else it fits
    // the tab's text to the screen.
    this.fzFit.textContent = fitOn() ? 'Reset' : 'Fit';
    this.fzFit.setAttribute('aria-label', fitOn() ? 'Back to the default text size' : 'Fit the text to the screen');
  }

  // Touch zoom (the floating A- / A+, the key bar's, a pinch): this tab's
  // text one px smaller or larger than shown now, kept on this device;
  // past the fit the pane pans. 0 goes back to the fit.
  step(dir) {
    if (!this.id) return;
    this.wake();
    this.zoomAt = performance.now();
    // Fitting to this screen: the size is this device's, for every tab,
    // and the tab rewraps to it. 0 goes back to the default size.
    if (fitOn()) {
      const cur = this.term.options.fontSize;
      const f = dir ? Math.max(8, Math.min(MAXF, dir > 0 ? Math.floor(cur) + 1 : Math.ceil(cur) - 1)) : null;
      store.set('fontpref', f);
      this.body.scrollLeft = this.body.scrollTop = 0;
      emit('layout');
      this.layout();
      return;
    }
    if (!dir) { store.set('font.' + this.id, null); this.body.scrollLeft = this.body.scrollTop = 0; this.layout(); return; }
    const cur = this.term.options.fontSize;
    const f = Math.max(MINF, Math.min(MAXF, dir > 0 ? Math.floor(cur) + 1 : Math.ceil(cur) - 1));
    store.set('font.' + this.id, f);
    this.layout();
  }

  // A- / A+: this device's font size, for every pane (remembered); Fit
  // drops this tab's own pinch zoom. `abs` sets this tab's own size.
  zoom(dir, abs) {
    if (!this.id) return;
    this.zoomAt = performance.now();
    if (abs != null) { store.set('font.' + this.id, abs); this.layout(); return; }
    store.set('font.' + this.id, null);
    if (dir) {
      const f = Math.max(8, Math.min(MAXF, Math.round((prefFont() + dir) * 4) / 4));
      store.set('fontpref', f);
      toast('Font ' + f + ' px on this device', 'info');
    }
    emit('layout');
    this.layout();
  }

  toggleTake() {
    if (!this.id) return;
    this.take = !this.take;
    store.set('take.' + this.id, this.take);
    // Give the size back: the tab returns to the terminal UI's own size.
    if (!this.take) { send({ t: 'resize', tab: uidOf(this.id), release: true }); this.sent = false; }
    this.takeBtn.setAttribute('aria-pressed', this.take);
    emit('take', this);
    toast(this.take ? 'This pane now sets the tab size' : 'Back to the tab\'s own size (fit by font)', 'info');
    this.layout();
  }

  // The buffer cell under a viewport point: {col, row} (row in the buffer).
  cellAt(x, y) {
    const r = this.term.element.querySelector('.xterm-screen').getBoundingClientRect();
    const c = this.cell();
    const col = Math.max(0, Math.min(this.term.cols - 1, Math.floor((x - r.left) / c.w)));
    const row = Math.max(0, Math.min(this.term.rows - 1, Math.floor((y - r.top) / c.h)));
    return { col, row: row + this.term.buffer.active.viewportY };
  }

  // The link (http, https) under a viewport point, if any.
  linkAt(x, y) {
    const { col, row } = this.cellAt(x, y);
    const line = this.term.buffer.active.getLine(row)?.translateToString(true) || '';
    for (const m of line.matchAll(/https?:\/\/[^\s"'<>`]+[^\s"'<>`.,;:!?)\]]/g)) {
      if (col >= m.index && col < m.index + m[0].length) return m[0];
    }
    return '';
  }

  // Touch: one finger drags history (or pans when zoomed; an alternate
  // screen app gets it as wheel input: mouse reports or arrow keys, as the
  // terminal UI does), a tap focuses the terminal (the keyboard) or opens
  // a link, a long press selects (drag to extend, then Copy), two fingers
  // pinch the font size. Handled before xterm sees it, so nothing scrolls
  // twice and the page never moves.
  touch() {
    const b = this.body;
    let sx, sy, lx, ly, t0, moved, acc = 0, pinch = null, vel = 0, lt = 0, raf = 0, lpT = 0, sel = null;
    const dist = (e) => Math.hypot(e.touches[0].clientX - e.touches[1].clientX, e.touches[0].clientY - e.touches[1].clientY);
    const scrollPx = (dy) => {
      // Pan a zoomed view first, then scroll history.
      const before = b.scrollTop;
      b.scrollTop = before - dy;
      dy -= before - b.scrollTop;
      if (Math.abs(dy) < 0.01) return b.scrollTop !== before;
      const ch = this.cell().h;
      acc += dy;
      const lines = Math.trunc(acc / ch);
      if (!lines) return true;
      acc -= lines * ch;
      // The app scrolls itself (Claude Code, less, vim): one wheel notch
      // per row of drag, at the cell under the finger, as GodTerm's
      // terminal UI sends a wheel. Else this terminal's own history.
      if (this.scrollPath() === 'app') {
        this.wheel(lines, lx, ly);
        return true;
      }
      const buf = this.term.buffer.active, y0 = buf.viewportY;
      this.term.scrollLines(-lines);
      return buf.viewportY !== y0;
    };
    const selectTo = (x, y) => {
      const c = this.cellAt(x, y), cols = this.term.cols;
      let a = sel.a, z = c;
      if (z.row < a.row || (z.row === a.row && z.col < a.col)) [a, z] = [z, a];
      this.term.select(a.col, a.row, (z.row - a.row) * cols + (z.col - a.col) + 1);
    };
    const opts = { passive: false, capture: true };
    b.addEventListener('touchstart', (e) => {
      cancelAnimationFrame(raf);
      clearTimeout(lpT);
      e.stopPropagation();
      if (e.touches.length === 2) {
        pinch = { d: dist(e), f: this.term.options.fontSize };
        sel = null;
        e.preventDefault();
        return;
      }
      if (e.touches.length !== 1) return;
      sx = lx = e.touches[0].clientX; sy = ly = e.touches[0].clientY;
      t0 = lt = performance.now(); moved = false; acc = 0; vel = 0;
      if (e.target.closest('button')) return;
      // A long press without moving: select from here.
      lpT = setTimeout(() => {
        if (moved || pinch || !this.id) return;
        const c = this.cellAt(sx, sy);
        const line = this.term.buffer.active.getLine(c.row)?.translateToString(false) || '';
        let s0 = c.col, s1 = c.col;
        while (s0 > 0 && /\S/.test(line[s0 - 1] || '')) s0--;
        while (s1 < line.length - 1 && /\S/.test(line[s1 + 1] || '')) s1++;
        sel = { a: { col: s0, row: c.row } };
        this.term.select(s0, c.row, Math.max(1, s1 - s0 + 1));
        navigator.vibrate?.(15);
      }, 450);
    }, opts);
    b.addEventListener('touchmove', (e) => {
      e.stopPropagation();
      if (e.target.closest('button') && !moved) return;
      e.preventDefault();
      if (pinch && e.touches.length === 2) {
        const f = Math.max(MINF, Math.min(MAXF, Math.round(pinch.f * (dist(e) / pinch.d) * 4) / 4));
        if (f !== this.term.options.fontSize) { this.setFont(f); store.set(fitOn() ? 'fontpref' : 'font.' + this.id, f); this.clampPan(); this.paintZoom(); }
        return;
      }
      if (e.touches.length !== 1) return;
      const x = e.touches[0].clientX, y = e.touches[0].clientY, now = performance.now();
      if (Math.abs(x - sx) + Math.abs(y - sy) > 8) { moved = true; clearTimeout(lpT); }
      if (sel) { selectTo(x, y); lx = x; ly = y; return; }
      if (!moved) return;
      b.scrollLeft -= x - lx;
      scrollPx(y - ly);
      // Velocity, smoothed over the last moves (px per ms).
      vel = 0.6 * ((y - ly) / Math.max(1, now - lt)) + 0.4 * vel;
      lx = x; ly = y; lt = now;
    }, opts);
    const end = (e) => {
      e.stopPropagation();
      clearTimeout(lpT);
      if (pinch) { if (e.touches.length < 2) { pinch = null; this.zoomAt = performance.now(); this.layout(); } return; }
      if (sel) {
        sel = null;
        if (this.term.hasSelection()) this.copyHint();
        e.preventDefault();
        return;
      }
      if (e.target.closest('button')) return;
      if (!moved && performance.now() - t0 < 450) {
        e.preventDefault();
        if (this.term.hasSelection()) { this.term.clearSelection(); this.copyHint(false); return; }
        const url = this.linkAt(sx, sy);
        if (url) { window.open(url, '_blank', 'noopener'); return; }
        this.term.focus();
        return;
      }
      // Momentum after a flick, unless the finger rested before lifting;
      // the debug log says how it scrolled once it stops.
      const said = () => moved && this.saidScroll();
      if (performance.now() - lt > 80) { said(); return; }
      let v = vel * 16;
      const step = () => { if (Math.abs(v) < 0.5 || !scrollPx(v)) { said(); return; } v *= 0.95; raf = requestAnimationFrame(step); };
      if (Math.abs(v) > 2) raf = requestAnimationFrame(step);
      else said();
    };
    b.addEventListener('touchend', end, opts);
    b.addEventListener('touchcancel', (e) => { clearTimeout(lpT); pinch = null; sel = null; e.stopPropagation(); }, opts);
  }

  // A Copy button while text is selected (a long press on a phone).
  copyHint(show = true) {
    if (!this.copyBtn) {
      this.copyBtn = h('button.pane-hint.copy', { type: 'button', onclick: async (e) => {
        e.stopPropagation();
        const text = this.term.getSelection();
        this.term.clearSelection();
        this.copyBtn.hidden = true;
        if (await copyText(text)) toast('Copied', 'ok');
        else toast('Copy is not allowed here: select it in the box', 'warn');
      } }, 'Copy');
      this.body.append(this.copyBtn);
    }
    this.copyBtn.hidden = !show;
  }

  // "Jump to the end" while scrolled back in history.
  // In an app that scrolls itself, it takes the notches back down.
  paintJump() {
    const buf = this.term.buffer.active;
    const up = this.ready && ((buf.type === 'normal' && buf.viewportY < buf.baseY) || (this.appBack > 0 && this.scrollPath() === 'app'));
    if (!this.jump) {
      this.jump = h('button.jump', { type: 'button', 'aria-label': 'Jump to the end', hidden: true, onclick: (e) => {
        e.stopPropagation();
        if (this.appBack > 0 && this.scrollPath() === 'app') { this.wheel(-this.appBack); this.saidScroll(true); } else this.term.scrollToBottom();
      } });
      this.jump.innerHTML = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14M6 13l6 6 6-6"/></svg>';
      this.body.append(this.jump);
    }
    if (this.jump.hidden === up) this.jump.hidden = !up;
  }

  dnd() {
    this.name.addEventListener('dragstart', (e) => e.dataTransfer.setData('text/gt-pane', String(this.i)));
    this.el.addEventListener('dragover', (e) => {
      const ty = e.dataTransfer.types;
      if (ty.includes('text/gt-tab') || ty.includes('text/gt-pane')) { e.preventDefault(); this.el.classList.add('drop'); }
    });
    this.el.addEventListener('dragleave', () => this.el.classList.remove('drop'));
    this.el.addEventListener('drop', (e) => {
      this.el.classList.remove('drop');
      const tab = e.dataTransfer.getData('text/gt-tab');
      const from = e.dataTransfer.getData('text/gt-pane');
      e.preventDefault();
      if (tab) panes.show(tab, this.i);
      else if (from !== '') panes.swap(+from, this.i);
    });
  }
}

export const panes = {
  list: [],
  n: 1,
  cur: 0,
  subs: new Set(),
  waiting: new Set(),
  init() {
    this.box = $('#panes');
    const ids = store.get('panes', []);
    this.n = isDesk() ? store.get('npanes', 1) : 1;
    for (let i = 0; i < 4; i++) {
      const p = new Pane(i);
      this.list.push(p);
      this.box.append(p.el);
      if (ids[i]) p.setTab(ids[i]);
    }
    this.cur = Math.min(store.get('pfocus', 0), this.n - 1);
    this.apply();
    matchMedia(DESK).addEventListener('change', () => { this.n = isDesk() ? store.get('npanes', 1) : 1; this.apply(); });
    on('msg:resume', (m) => {
      this.waiting.delete(uidOf(m.tab));
      const id = 't' + uidOf(m.tab);
      const ps = this.visible().filter((p) => p.id === id);
      // Every pane on that tab must take it; else a fresh snapshot.
      if (!ps.map((p) => p.resume(m)).every(Boolean)) { ps.forEach((p) => { p.ready = false; p.from = null; }); this.again(uidOf(m.tab)); }
      else dlog('conn', `resumed ${id}`);
    });
    on('msg:snapshot', (m) => { this.waiting.delete(uidOf(m.tab)); const id = 't' + uidOf(m.tab); for (const p of this.visible()) if (p.id === id) p.snapshot(m); });
    on('msg:wheel', (m) => { if (!m.taken) { const id = 't' + uidOf(m.tab); for (const p of this.visible()) if (p.id === id && p.appBack) { p.appBack = 0; p.paintJump(); } if (!wheelSaid) { wheelSaid = true; dlog('term', `t${uidOf(m.tab)}: the app took no wheel (main screen, no mouse reports)`); } } });
    on('msg:resized', (m) => { const id = 't' + uidOf(m.tab); for (const p of this.visible()) if (p.id === id) p.resized(m); });
    on('bin:' + KIND.OUTPUT, (uid, b) => { const id = 't' + uid; for (const p of this.visible()) if (p.id === id) p.output(b); });
    on('state', () => {
      // Nothing picked yet: start on the terminal UI's focused tab.
      if (!this.list.some((p) => p.id) && S.focused != null && !this.picked) { this.picked = true; this.show('t' + S.focused, 0); }
      for (const p of this.list) p.paintHead();
      // A size this phone took that the state no longer gives it: the Mac
      // was typed in (or another browser took it). Not sized again until
      // this pane is used.
      for (const p of this.visible()) {
        const t = p.sent && !p.take && tabById(p.id);
        if (t && Date.now() - (p.sentAt || 0) > 2000 && t.size_taken_by !== myClient) { p.sent = false; p.yielded = true; p.layout(); }
      }
      this.sync();
    });
    on('msg:error', (m) => {
      if (m.tab == null) return;
      const u = uidOf(m.tab);
      this.waiting.delete(u);
      for (const p of this.visible()) if (uidOf(p.id) === u) { p.msg.textContent = m.error === 'no such tab' ? 'This tab is closed' : m.error; p.msg.hidden = false; }
    });
    // GodTerm's own palette for the terminal background and text.
    on('hello', (m) => {
      myClient = m.client;
      const th = m.theme || {};
      if (!th.bg) return;
      Object.assign(THEME, { background: th.bg, foreground: th.fg || THEME.foreground, cursorAccent: th.bg, selectionBackground: th.selection || THEME.selectionBackground });
      document.documentElement.style.setProperty('--term', th.bg);
      for (const p of this.list) p.term.options.theme = { ...THEME };
    });
    // Fonts that load late change the cell: measure again.
    document.fonts?.ready?.then(() => { M = null; for (const p of this.list) { p.term._core?._charSizeService?.measure(); p.layout(); } });
    on('conn', (s) => {
      // A new connection: each pane asks for only what it missed.
      if (s === 'live') {
        this.subs.clear(); this.waiting.clear();
        // GodTerm gave the sizes back when the old connection went.
        for (const p of this.list) { p.from = p.ready && p.sid ? { sid: p.sid, pos: p.pos } : null; p.ready = false; p.sent = false; }
        this.sync();
      }
    });
  },
  visible() { return this.list.slice(0, this.n); },
  focused() { return this.list[this.cur]; },
  apply() {
    this.box.dataset.n = this.n;
    this.list.forEach((p, i) => { p.el.hidden = i >= this.n; p.el.classList.toggle('focus', i === this.cur && this.n > 1); });
    this.sync();
    emit('focus', this.focused());
  },
  setN(n) { this.n = n; store.set('npanes', n); if (this.cur >= n) this.cur = 0; this.apply(); },
  focus(i) {
    if (i === this.cur) return;
    this.cur = i;
    store.set('pfocus', i);
    this.list.forEach((p, j) => p.el.classList.toggle('focus', j === i && this.n > 1));
    emit('focus', this.focused());
  },
  show(id, i = this.cur) {
    const already = this.visible().findIndex((p) => p.id === id);
    if (already >= 0 && i === this.cur) { this.focus(already); return; }
    this.list[i].setTab(id);
    this.focus(i);
    this.save();
    this.sync();
    emit('focus', this.focused());
  },
  swap(a, b) {
    const x = this.list[a].id, y = this.list[b].id;
    this.list[a].setTab(y); this.list[b].setTab(x);
    this.save(); this.sync();
  },
  save() { store.set('panes', this.list.map((p) => p.id)); },
  // Subscribe to exactly the tabs that are on screen; a pane that has no
  // screen yet for an already subscribed tab gets a fresh snapshot.
  sync() {
    for (const p of this.list.slice(this.n)) p.ready = false;
    if (!conn.open) return;
    const vis = this.visible().filter((p) => p.id);
    const want = new Set(vis.map((p) => uidOf(p.id)));
    const need = new Set(vis.filter((p) => !p.ready).map((p) => uidOf(p.id)).filter((u) => !this.waiting.has(u)));
    const del = [...this.subs].filter((u) => !want.has(u));
    const again = [...need].filter((u) => this.subs.has(u));
    if (del.length || again.length) send({ t: 'unsubscribe', tabs: [...del, ...again] });
    const add = [...want].filter((u) => !this.subs.has(u) || need.has(u));
    if (add.length) {
      const resume = {};
      for (const u of add) {
        const p = vis.find((q) => uidOf(q.id) === u && q.from);
        if (p) resume[u] = p.from;
      }
      send(Object.keys(resume).length ? { t: 'subscribe', tabs: add, resume } : { t: 'subscribe', tabs: add });
      add.forEach((u) => this.waiting.add(u));
    }
    this.subs = want;
  },
  // A fresh snapshot for one tab (a resume that did not fit).
  again(u) {
    send({ t: 'unsubscribe', tabs: [u] });
    send({ t: 'subscribe', tabs: [u] });
    this.waiting.add(u);
  },
};
