import { $, h, on, emit, store, toast, uidOf, isDesk, isTouch, DESK, paintIcons, tcPaint, icon } from './util.js';
import { send, sendInput, KIND, conn, tool, gzOk } from './conn.js';
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

const minF = () => (isDesk() ? 9 : MINF);
let wheelSaid = false;
let M = null, hinted = false, queuedSaid = false, cleared = 0, clearSaid = 0;
on('conn', (s) => { if (s === 'live') queuedSaid = false; });
on('input-lost', () => toast('Not connected for a while: some keys were not sent', 'warn'));

export const fitOn = () => !isDesk() && store.get('fitscreen', isTouch());

export const deskFit = () => isDesk() && store.get('deskfit', true);
const autoOn = () => fitOn() || deskFit();

export const prefFont = () => store.get('fontpref', null) || (isDesk() ? 14 : fitOn() ? 14 : 13);

const DEVICE = /iPhone/.test(navigator.userAgent) ? 'iPhone' : /iPad|Macintosh.*Mobile/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1) ? 'iPad' : /Android/.test(navigator.userAgent) ? 'Android phone' : 'a browser';

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

document.addEventListener('visibilitychange', () => {
  clearTimeout(hiddenT);
  if (document.hidden) hiddenT = setTimeout(() => panes.list.forEach((p) => p.giveBack()), 60000);
  else markActive();
});

const KB_SETTLE = 900;
let kbAt = -1e9, kbWas = false;
new MutationObserver(() => {
  const k = document.body.classList.contains('kb');
  if (k !== kbWas) { kbWas = k; kbAt = performance.now(); }
}).observe(document.body, { attributes: true, attributeFilter: ['class'] });

const ZOOM_SETTLE = 600;

const ENC = { DEFAULT: 'x10', SGR: 'sgr', SGR_PIXELS: 'sgr pixels' };

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

const SYM = { 0xba: [0x97, 0x8f], 0xb8: [0x80, 0x96], 0xb9: [0x96, 0xa0] }; 
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
    this.id = '';      
    this.cols = 0;
    this.rows = 0;
    this.ready = false;
    this.el = h('div.pane', { 'data-i': i });
    this.name = h('span.pn', { draggable: 'true' }, 'Empty pane');
    this.dot = h('span.dot');
    this.sz = h('span.sz');

    this.meta = h('span.pmeta', { onclick: (e) => {
      const f = e.target.closest('.pm-dir')?.dataset.full;
      if (f) { navigator.clipboard?.writeText(f).then(() => toast('Copied ' + f, 'ok'), () => {}); e.stopPropagation(); }
    } });
    this.takeBtn = h('button.ib', { title: 'Take size: resize the tab to this pane', 'data-i': 'max', 'aria-pressed': 'false', onclick: () => this.toggleTake() });
    this.head = h('div.pane-head', {}, this.dot, this.name, this.meta, this.sz,
      h('button.ib.zb', { title: 'Smaller font', onclick: () => this.zoom(-1) }, 'A-'),
      h('button.ib.zb', { title: 'Larger font', onclick: () => this.zoom(1) }, 'A+'),
      h('button.ib.zb', { title: 'Back to this device\'s font size (smaller when the tab does not fit)', onclick: () => this.zoom(0) }, 'Fit'),
      this.takeBtn,
      h('button.ib', { title: 'Tab options', 'data-i': 'dots', onclick: (e) => { const r = e.currentTarget.getBoundingClientRect(); if (this.id) emit('tabmenu', this.id, { x: r.right - 320, y: r.bottom + 4 }); } }));
    this.host = h('div.xt');

    this.stash = new Map();
    this.msg = h('div.pane-msg', {}, 'Pick a tab to watch it');

    this.hint = h('button.pane-hint', { hidden: true, onclick: (e) => {
      e.stopPropagation();

      if (this.yielded && autoOn() && !this.take) { this.yielded = false; this.layout(); return; }
      this.toggleTake();
    } });

    this.fzSize = h('span.fz-n', { 'aria-live': 'polite' });
    this.fz = h('div.fzoom', { role: 'toolbar', 'aria-label': 'Text size' },
      h('button', { type: 'button', 'aria-label': 'Smaller text', onclick: (e) => { e.stopPropagation(); this.step(-1); } }, 'A-'),
      this.fzSize,
      h('button', { type: 'button', 'aria-label': 'Larger text', onclick: (e) => { e.stopPropagation(); this.step(1); } }, 'A+'),
      this.fzFit = h('button.fz-fit', { type: 'button', 'aria-label': 'Fit the text to the screen', onclick: (e) => { e.stopPropagation(); this.step(0); } }, 'Fit'));
    this.body = h('div.pane-body', {}, this.host, this.msg, this.hint, this.fz);

    this.lease = h('div.lease-bar', { hidden: true });
    this.el.append(this.head, this.lease, this.body);
    paintIcons(this.head);
    this.el.addEventListener('pointerdown', () => panes.focus(this.i), true);

    on('pane-font', (p) => p === this && this.paintZoom());
    this.wake();

    this.term = null;
    this.touch();
    this.dnd();

    new ResizeObserver(() => { if (!document.documentElement.classList.contains('side-drag')) this.layout(); }).observe(this.body);
  }

  ensureTerm() { if (!this.term) this.makeTerm(); }

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

    try {
      const core = t._core, sl = core.scrollLines;
      if (typeof sl === 'function') core.scrollLines = function (n, ...a) { return Number.isFinite(n) ? sl.call(this, n, ...a) : undefined; };
    } catch {}

    const unstick = () => { try { const v = t._core.viewport; if (v?._ignoreNextScrollEvent) v._ignoreNextScrollEvent = false; } catch {} };
    this.host.addEventListener('wheel', unstick, { capture: true, passive: true });
    this.host.addEventListener('pointerdown', unstick, { capture: true, passive: true });
    this.gpu();
    t.onData((d) => this.input(inputHook(d === '\r' && returnNewline() ? newlineFor(this.id) : d, this)));

    t.attachCustomKeyEventHandler((e) => {
      if (e.key !== 'Enter' || !e.shiftKey || e.ctrlKey || e.altKey || e.metaKey || e.isComposing) return true;
      if (e.type === 'keydown') { e.preventDefault(); this.input(inputHook(newlineFor(this.id), this)); }
      return false;
    });

    if (isTouch()) t.gtMirror = ownTextarea(t, this.host, (d, how) => this.typed(d, how));
    t.onBinary((d) => this.input(Uint8Array.from(d, (c) => c.charCodeAt(0) & 255)));

    this.host.addEventListener('paste', (e) => {
      const files = [...(e.clipboardData?.files || [])].filter((f) => f.type.startsWith('image/'));
      if (!files.length || !this.id) return;
      e.preventDefault(); e.stopPropagation();
      import('./images.js').then((m) => m.sendImages(files, this));
    }, true);

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

  gpu() {
    this.gl = null;
    if (store.get('webgl', true) && window.WebglAddon) {
      try {
        const g = new WebglAddon.WebglAddon();

        g.onContextLoss(() => { g.dispose(); if (this.gl === g) { this.gl = null; this.renderer = 'dom'; this.glRetry = (this.glRetry || 0) + 1; if (this.glRetry <= 2) setTimeout(() => { if (!this.gl && this.term) { this.gpu(); this.term.refresh(0, this.term.rows - 1); } }, 1500); } });
        this.term.loadAddon(g);
        this.gl = g;
        this.renderer = 'webgl';
      } catch { this.renderer = 'dom'; }
    } else this.renderer = 'dom';
  }

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

  get sizing() { return this.take || (autoOn() && !!this.id && !this.yielded && activeNow()); }

  giveBack() {
    clearTimeout(this.rsT);
    if (!this.sent || !this.id) { this.sent = false; return; }
    send({ t: 'resize', tab: uidOf(this.id), release: true });
    this.sent = false;
  }

  typed(d, how) {
    if (how === 'paste') { this.paste(d); return; }
    if (how === 'enter') { this.input(inputHook(returnNewline() ? newlineFor(this.id) : '\r', this)); return; }
    if (how === 'del') { this.input(d, true); return; }

    const nl = how === 'text' && /\r?\n/.test(d);
    const txt = nl ? d.replace(/\r?\n/g, newlineFor(this.id)) : d;
    const out = inputHook(txt, this);
    this.input(out, true);

    if (out !== txt) this.term.gtMirror?.reset();
    if (this.term.buffer.active.viewportY < this.term.buffer.active.baseY) this.term.scrollToBottom();
  }

  input(d, mirrored = false) {
    if (!mirrored) this.term?.gtMirror?.commit();
    if (!this.id || d == null || d === '' || !this.term) return;
    if (this.readOnly()) return;
    if (this.yielded) { this.yielded = false; this.layout(); }

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

  paste(text) { if (this.id) this.term.paste(text); }

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
    if (!kept && this.term) this.clear();
    this.body.scrollLeft = this.body.scrollTop = 0;
    this.take = !!store.get('take.' + this.id, false);
    this.takeBtn.setAttribute('aria-pressed', this.take);
    this.msg.textContent = this.id ? 'Loading...' : 'Pick a tab to watch it';
    this.msg.hidden = !!kept;
    this.paintHead();
    if (kept) this.layout();

    if (typing && !this.host.contains(document.activeElement)) this.term.focus();
  }

  paintHead() {
    const t = tabById(this.id);
    this.name.textContent = t ? t.name : this.id ? this.id : 'Empty pane';
    this.dot.className = 'dot ' + (t ? t.state : '');

    this.el.classList.toggle('empty', !this.id);
    tcPaint(this.el, t);
    this.sz.textContent = this.cols ? `${this.cols}x${this.rows}` : '';

    for (const b of this.head.querySelectorAll('button.ib')) {
      if (b.dataset.t0 == null) b.dataset.t0 = b.title;
      b.disabled = !t;
      b.title = t ? b.dataset.t0 : 'Pick a tab for this pane first';
    }
    this.paintMeta(t);
    if (this.id && !t && S.tabs.length) { this.msg.textContent = 'This tab is closed'; this.msg.hidden = false; }

    else if (t && this.ready && t.running === false && !this.screenText().trim()) {
      this.msg.textContent = `Not running (${t.state || 'stopped'}): its options can reopen it`;
      this.msg.hidden = false;
      this.idleMsg = true;
    } else if (this.idleMsg && this.ready) { this.idleMsg = false; this.msg.hidden = true; }
    this.paintLease(t);
  }

  paintMeta(t) {
    const g = t?.git_facts;
    const git = g?.branch ? [g.branch, g.dirty ? g.dirty + ' changed' : '', g.unpushed ? '↑' + g.unpushed : '', g.behind ? '↓' + g.behind : ''].filter(Boolean).join(' ') : '';

    const email = t && S.accounts.find((x) => x.account === t.account)?.email;
    const hide = store.get('hideEmail', false);
    const bits = t ? [
      t.folder && ['pm-dir', t.folder, t.folder_full || t.folder],
      t.account_label && ['pm-acct', t.account_label],
      email && ['pm-email', hide ? '••••••' : email],
      t.model && ['pm-model', t.model + (t.effort ? ' · ' + t.effort : '')],
      git && ['pm-git', git],
      t.state && ['pm-state', t.state],
    ].filter(Boolean) : [];
    const key = JSON.stringify(bits);
    if (key === this.metaKey) return;
    this.metaKey = key;
    this.metaEls = bits.map(([c, v, full]) => c === 'pm-email' ? h('button.pm-email', { type: 'button', title: hide ? 'Show the account email' : 'Hide the account email', 'aria-label': hide ? 'Show the account email' : 'Hide the account email', onclick: () => { store.set('hideEmail', !hide); for (const q of panes.list) { q.metaKey = ''; q.paintMeta(tabById(q.id)); } } }, v) : full ? h('span.' + c, { 'data-full': full, title: 'Copy ' + full }, h('span', {}, v), h('span', { html: icon('copy') })) : h('span.' + c, { title: v }, v));
    this.meta.replaceChildren(...this.metaEls);
    this.fitHead();
  }

  fitHead() {
    const head = this.head, W = head.clientWidth;
    if (!W || !this.metaEls) return;
    this.meta.replaceChildren(...this.metaEls);
    this.el.classList.remove('tight');
    head.classList.add('measuring');
    const fits = () => head.scrollWidth <= head.clientWidth + 1;
    for (const c of ['pm-email', 'pm-acct', 'pm-model', 'pm-git', 'pm-dir']) {
      if (fits()) break;
      this.meta.querySelector('.' + c)?.remove();
    }
    if (!fits()) this.el.classList.add('tight');
    head.classList.remove('measuring');
  }

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

  clear(fn) {
    if (this.clearing) { if (fn) this.clearing.push(fn); return; }
    const el = this.term?.element?.querySelector('.xterm-viewport');
    if (!el || !el.scrollTop) { this.term?.reset(); fn?.(); return; }
    el.scrollTop = 0;
    this.clearing = fn ? [fn] : [];
    requestAnimationFrame(() => {
      const q = this.clearing;
      this.clearing = null;
      this.term?.reset();
      for (const f of q) f();
    });
  }

  snapshot(m) {
    this.ensureTerm();
    this.clear(() => {

      this.sid = m.sid || null;
      this.pos = m.pos || 0;
      this.from = null;
      this.cols = m.cols || this.cols || 80;
      this.rows = m.rows || this.rows || 24;
      this.term.resize(this.cols, this.rows);
      this.ready = true;
      this.appBack = 0;

      const gen = (this.gen = (this.gen || 0) + 1);
      this.term.write(plainSymbols(m.data || ''), () => {

        if (this.gen === gen) this.term.scrollToBottom();
        if (this.gen === gen && this.ready) { this.msg.hidden = true; this.wake(); }
        const b = this.term.buffer.active;
        dlog('term', `snapshot ${this.id} ${this.cols}x${this.rows}: ${b.type} buffer, ${b.baseY} history lines`);
      });
      this.paintHead();
      this.layout();
    });
  }

  output(bytes) {

    if (this.clearing) { this.clearing.push(() => this.output(bytes)); return; }
    if (!this.ready) return;
    this.pos += bytes.length;
    this.term.write(plainSymbols(bytes));
  }

  resume(m) {
    const f = this.from;
    this.from = null;
    if (!f || f.sid !== m.sid || f.pos !== m.pos || (m.cols && m.cols !== this.cols) || (m.rows && m.rows !== this.rows)) return false;
    this.sid = m.sid;
    this.pos = m.pos;
    this.ready = true;
    this.msg.hidden = true;

    this.layout();
    return true;
  }

  resized(m) {
    if (!this.ready) return;
    if (m.sid) { this.sid = m.sid; this.pos = m.pos || 0; }
    if (m.cols !== this.term.cols || m.rows !== this.term.rows) this.term.resize(m.cols, m.rows);
    this.cols = m.cols; this.rows = m.rows;
    this.paintHead();
    this.layout();
  }

  scrollPath() {
    const buf = this.term.buffer.active, mouse = this.term.modes.mouseTrackingMode !== 'none';
    return buf.type === 'alternate' || (mouse && buf.baseY === 0) ? 'app' : 'local';
  }

  wheel(lines, x, y) {
    if (!this.id || !lines || !this.term) return;
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

      this.appBack = Math.max(0, (this.appBack || 0) + q);
      this.paintJump();
      if (!this.appBack) this.unhold();
    });
  }

  scrolledBack() {
    const buf = this.term.buffer.active;
    return (buf.type === 'normal' && buf.viewportY < buf.baseY) || (this.appBack > 0 && this.scrollPath() === 'app');
  }

  unhold() {
    if (!this.held) return;
    this.held = false;
    dlog('term', `back at the end of ${this.id}: the held resize goes out`);
    this.layout();
  }

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

  fitFont(W, H, max = MAXF) {
    const cols = this.cols || 80, rows = this.rows || 24, m = metrics(), dpr = devicePixelRatio || 1;
    const fits = (f) => cols * (Math.floor(m.w * f * dpr) / dpr) <= W && rows * (Math.ceil(m.h * f * dpr) / dpr) <= H;
    let f = Math.min(MAXF, max);
    while (f > minF() && !fits(f)) f -= 0.25;
    this.setFont(f);

    cancelAnimationFrame(this.chk);
    this.chk = requestAnimationFrame(() => {
      for (let n = 0; n < 8 && !this.take; n++) {
        const c = this.cell();
        if ((cols * c.w <= W + 0.5 && rows * c.h <= H + 0.5) || this.term.options.fontSize <= minF()) break;
        if (store.get('font.' + this.id, null)) break;
        this.setFont((this.fitted = this.term.options.fontSize - 0.25));
      }

      const c = this.cell();
      this.body.classList.toggle('pan', isDesk() && (cols * c.w > W + 0.5 || rows * c.h > H + 0.5));
      this.anchor();
    });
    return f;
  }

  pad() {
    if (this.pads) return this.pads;
    const cs = this.term.element && getComputedStyle(this.term.element);
    const x = cs ? parseFloat(cs.paddingLeft) + parseFloat(cs.paddingRight) : 4, y = cs ? parseFloat(cs.paddingTop) + parseFloat(cs.paddingBottom) : 4;
    return (this.pads = { x: x || 0, y: y || 0 });
  }

  layout() {
    if (!this.term) return;
    const P = this.pad();
    const W = this.body.clientWidth - P.x;
    let H = this.body.clientHeight - P.y;
    if (W < 20 || H < 20) return;

    const kb = document.body.classList.contains('kb');

    if (!kb) { this.fullH = H; this.fullBH = this.body.clientHeight; this.fullW = W; }
    else if (this.fullW !== W) { this.fullH = this.fullBH = 0; }
    else if (!this.take && this.fullH > H) H = this.fullH;

    if (kb !== this.kbLast) { this.kbLast = kb; requestAnimationFrame(() => { if (!this.term) return; this.term.refresh(0, this.term.rows - 1); this.clampPan(); this.showCursor(); }); }

    const user = this.id && !fitOn() ? store.get('font.' + this.id, null) : null;
    if (this.sizing) {
      this.setFont(user || prefFont());

      this.host.style.cssText = 'right:0;bottom:0';
      this.body.classList.remove('roomy', 'pan');
      const d = this.fit.proposeDimensions();

      const since = performance.now() - kbAt, settling = since < KB_SETTLE;
      if (d && (kb || settling)) {
        if (this.downRows) d.rows = this.downRows;
        else if (kb) {

          const vv = window.visualViewport, gap = vv ? Math.max(0, innerHeight - vv.height) : 0;
          d.rows = Math.max(d.rows, Math.floor((H + gap) / Math.max(1, this.cell().h)));
        }
        if (settling && !kb) { clearTimeout(this.kbT); this.kbT = setTimeout(() => this.layout(), KB_SETTLE - since + 50); }
      } else if (d) this.downRows = d.rows;
      if (d && d.cols > 1 && d.rows > 1 && this.ready && (d.cols !== this.cols || d.rows !== this.rows || !this.sent)) {

        const zoomed = performance.now() - (this.zoomAt || -1e9) < ZOOM_SETTLE;
        clearTimeout(this.rsT);
        this.rsT = setTimeout(() => {
          if (!this.sizing || !this.ready) return;
          if (performance.now() - (this.zoomAt || -1e9) < ZOOM_SETTLE - 20) { this.layout(); return; }

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
    this.fitHead();
    this.term.refresh(0, this.term.rows - 1);
    if (this.ready && !this.take && !user && this.fitted < 8 && !isDesk() && !hinted) {
      hinted = true;
      toast('Small text: pinch or A+ to zoom, or Size to fit the tab to this screen', 'info', { label: 'Size', fn: () => this.toggleTake() });
    }

    this.clampPan();
    this.showCursor();
    emit('pane-font', this);
  }

  anchor() {
    if (this.sizing) return;
    const th = this.term.element?.offsetHeight || 0;
    const H = document.body.classList.contains('kb') ? this.fullBH || this.body.clientHeight : this.body.clientHeight;
    const top = !isDesk() && th ? Math.max(0, Math.floor(H - th)) : 0;
    this.host.style.cssText = top ? `top:${top}px` : '';

    this.body.classList.toggle('roomy', top >= 60);
  }

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

  paintHint(W, H) {
    let txt = '';
    const t = tabById(this.id);
    if (this.take) txt = this.sent ? 'Give the size back' : '';
    else if (this.ready && this.id && this.yielded && autoOn()) {

      txt = (t?.size_device ? 'Sized by ' + t.size_device : 'Sized by the terminal UI') + ' · use this window\'s';
    } else if (this.ready && this.id && !autoOn()) {
      const f = this.term.options.fontSize, m = metrics(), dpr = devicePixelRatio || 1;
      const w = this.cols * (Math.floor(m.w * f * dpr) / dpr), hh = this.rows * (Math.ceil(m.h * f * dpr) / dpr);
      if (f >= prefFont() - 0.01 && w * hh < 0.6 * W * H && (W - w > 160 || H - hh > 120)) txt = 'Use this window\'s size';
    }
    this.hint.textContent = txt;
    this.hint.title = this.take ? 'The tab goes back to the terminal UI\'s size' : 'Resize the tab to fill this pane (the terminal UI gets it back when you release it)';
    this.hint.hidden = !txt;
    this.hint.classList.toggle('on', this.take);

    if (!isDesk() && !!txt && this.yielded && !this.take && this.saidYield !== txt && !$('#app').classList.contains('chat-open')) {
      this.saidYield = txt;

      setTimeout(() => { if (!this.term || !this.yielded || this.take || !this.body.offsetParent) return; this.anchor(); if (!this.body.classList.contains('roomy')) toast(txt.replace(/ · .*$/, ''), 'info', { label: 'Use this size', fn: () => this.toggleTake() }, 6000); }, 500);
    }
  }

  clampPan() {
    const b = this.body;
    b.scrollLeft = Math.min(b.scrollLeft, Math.max(0, b.scrollWidth - b.clientWidth));
    b.scrollTop = Math.min(b.scrollTop, Math.max(0, b.scrollHeight - b.clientHeight));
  }

  wake() {
    this.fz.classList.remove('faded');
    clearTimeout(this.fzT);
    this.fzT = setTimeout(() => this.fz.classList.add('faded'), 4000);
  }

  paintZoom() {
    const f = this.term.options.fontSize, user = fitOn() ? store.get('fontpref', null) : this.id && store.get('font.' + this.id, null);
    const txt = String(Math.round(f * 4) / 4);

    if (txt !== this.fzSize.textContent) this.wake();
    this.fzSize.textContent = txt;
    this.fz.classList.toggle('user', !!user);

    this.fzFit.textContent = fitOn() ? 'Reset' : 'Fit';
    this.fzFit.setAttribute('aria-label', fitOn() ? 'Back to the default text size' : 'Fit the text to the screen');
  }

  step(dir) {
    if (!this.id) return;
    this.wake();
    this.zoomAt = performance.now();

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

    if (!this.take) { send({ t: 'resize', tab: uidOf(this.id), release: true }); this.sent = false; this.yielded = autoOn(); }
    this.takeBtn.setAttribute('aria-pressed', this.take);
    emit('take', this);
    toast(this.take ? 'This pane now sets the tab size' : 'Back to the tab\'s own size (fit by font)', 'info');
    this.layout();
  }

  cellAt(x, y) {
    const r = this.term.element.querySelector('.xterm-screen').getBoundingClientRect();
    const c = this.cell();
    const col = Math.max(0, Math.min(this.term.cols - 1, Math.floor((x - r.left) / c.w)));
    const row = Math.max(0, Math.min(this.term.rows - 1, Math.floor((y - r.top) / c.h)));
    return { col, row: row + this.term.buffer.active.viewportY };
  }

  linkAt(x, y) {
    const { col, row } = this.cellAt(x, y);
    const line = this.term.buffer.active.getLine(row)?.translateToString(true) || '';
    for (const m of line.matchAll(/https?:\/\/[^\s"'<>`]+[^\s"'<>`.,;:!?)\]]/g)) {
      if (col >= m.index && col < m.index + m[0].length) return m[0];
    }
    return '';
  }

  touch() {
    const b = this.body;
    let sx, sy, lx, ly, t0, moved, acc = 0, pinch = null, vel = 0, lt = 0, raf = 0, lpT = 0, sel = null;
    const dist = (e) => Math.hypot(e.touches[0].clientX - e.touches[1].clientX, e.touches[0].clientY - e.touches[1].clientY);
    const scrollPx = (dy) => {

      const before = b.scrollTop;
      b.scrollTop = before - dy;
      dy -= before - b.scrollTop;
      if (Math.abs(dy) < 0.01) return b.scrollTop !== before;
      const ch = this.cell().h;
      acc += dy;
      const lines = Math.trunc(acc / ch);
      if (!lines) return true;
      acc -= lines * ch;

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

  paintJump() {
    if (!this.term) return;
    const buf = this.term.buffer.active;
    const up = this.ready && ((buf.type === 'normal' && buf.viewportY < buf.baseY) || (this.appBack > 0 && this.scrollPath() === 'app'));
    if (!this.jump) {
      this.jump = h('button.jump', { type: 'button', 'aria-label': 'Jump to the end', title: 'Jump to the end', hidden: true, onclick: (e) => {
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
      if (ty.includes('Files')) { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; this.el.classList.add('drop'); }
      else if (ty.includes('text/gt-tab') || ty.includes('text/gt-pane')) { e.preventDefault(); this.el.classList.add('drop'); }
    });
    this.el.addEventListener('dragleave', (e) => { if (!this.el.contains(e.relatedTarget)) this.el.classList.remove('drop'); });
    this.el.addEventListener('drop', (e) => {
      this.el.classList.remove('drop');
      if (e.dataTransfer.types.includes('Files')) {
        e.preventDefault();
        const files = [...e.dataTransfer.files];
        import('./drop.js').then((m) => m.dropOnPane(files, this));
        return;
      }
      const tab = e.dataTransfer.getData('text/gt-tab');
      const from = e.dataTransfer.getData('text/gt-pane');
      e.preventDefault();
      if (tab) panes.show(tab, this.i);
      else if (from !== '') panes.swap(+from, this.i);
    });
  }
}

const MINW = 360, MINH = 220;
export const panes = {
  list: [],
  n: 1,       
  want: 1,    
  dir: 'row',
  cur: 0,
  subs: new Set(),
  waiting: new Set(),

  roomFor(want) {
    const W = this.box.clientWidth, H = this.box.clientHeight;
    if (want <= 1 || !W || !H) return { n: Math.max(1, want), dir: want === 4 ? 'grid' : 'row' };
    const cols2 = W >= 2 * MINW, rows2 = H >= 2 * MINH;
    if (want >= 4 && cols2 && rows2) return { n: 4, dir: 'grid' };
    if (cols2) return { n: 2, dir: 'row' };
    if (rows2) return { n: 2, dir: 'col' };
    return { n: 1, dir: 'row' };
  },
  refit() {
    const r = this.roomFor(this.want);
    if (r.n === this.n && r.dir === this.dir) return;
    this.n = r.n; this.dir = r.dir;
    if (this.cur >= this.n) this.cur = 0;
    this.apply();
  },
  init() {
    this.box = $('#panes');
    const ids = store.get('panes', []);
    this.want = isDesk() ? store.get('npanes', 1) : 1;
    ({ n: this.n, dir: this.dir } = this.roomFor(this.want));
    for (let i = 0; i < 4; i++) {
      const p = new Pane(i);
      this.list.push(p);
      this.box.append(p.el);
      if (i < this.n) p.ensureTerm();
      if (ids[i]) p.setTab(ids[i]);
    }
    this.cur = Math.min(store.get('pfocus', 0), this.n - 1);
    this.apply();
    matchMedia(DESK).addEventListener('change', () => { this.want = isDesk() ? store.get('npanes', 1) : 1; this.n = 0; this.refit(); });

    new ResizeObserver(() => { cancelAnimationFrame(this.refitT); this.refitT = requestAnimationFrame(() => this.refit()); }).observe(this.box);
    on('msg:resume', (m) => {
      this.waiting.delete(uidOf(m.tab));
      const id = 't' + uidOf(m.tab);
      const ps = this.visible().filter((p) => p.id === id);

      if (!ps.map((p) => p.resume(m)).every(Boolean)) { ps.forEach((p) => { p.ready = false; p.from = null; }); this.again(uidOf(m.tab)); }
      else dlog('conn', `resumed ${id}`);
    });
    on('msg:snapshot', (m) => { this.waiting.delete(uidOf(m.tab)); const id = 't' + uidOf(m.tab); for (const p of this.visible()) if (p.id === id) p.snapshot(m); });
    on('msg:wheel', (m) => { if (!m.taken) { const id = 't' + uidOf(m.tab); for (const p of this.visible()) if (p.id === id && p.appBack) { p.appBack = 0; p.paintJump(); } if (!wheelSaid) { wheelSaid = true; dlog('term', `t${uidOf(m.tab)}: the app took no wheel (main screen, no mouse reports)`); } } });
    on('msg:resized', (m) => { const id = 't' + uidOf(m.tab); for (const p of this.visible()) if (p.id === id) p.resized(m); });
    on('bin:' + KIND.OUTPUT, (uid, b) => { const id = 't' + uid; for (const p of this.visible()) if (p.id === id) p.output(b); });
    on('state', () => {

      if (!this.list.some((p) => p.id) && S.focused != null && !this.picked) { this.picked = true; this.show('t' + S.focused, 0); }

      if (S.moved) for (let i = 0; i < this.list.length; i++) {
        const id = this.list[i].id, u = uidOf(id);
        const to = id && S.moved[u];
        if (to && !S.tabs.some((x) => x.id === id) && S.tabs.some((x) => uidOf(x.id) === to)) this.show('t' + to, i);
      }
      for (const p of this.list) p.paintHead();

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

    on('hello', (m) => {
      myClient = m.client;
      const th = m.theme || {};
      if (!th.bg) return;
      Object.assign(THEME, { background: th.bg, foreground: th.fg || THEME.foreground, cursorAccent: th.bg, selectionBackground: th.selection || THEME.selectionBackground });
      document.documentElement.style.setProperty('--term', th.bg);
      for (const p of this.list) if (p.term) p.term.options.theme = { ...THEME };
    });

    document.fonts?.ready?.then(() => { M = null; for (const p of this.list) { p.term?._core?._charSizeService?.measure(); p.layout(); } });
    on('conn', (s) => {

      if (s === 'live') {
        this.subs.clear(); this.waiting.clear();

        for (const p of this.list) { p.from = p.ready && p.sid ? { sid: p.sid, pos: p.pos } : null; p.ready = false; p.sent = false; }
        this.sync();
      }
    });
  },
  visible() { return this.list.slice(0, this.n); },
  focused() { return this.list[this.cur]; },
  apply() {
    this.box.dataset.n = this.n;
    this.box.dataset.dir = this.dir || 'row';
    this.list.forEach((p, i) => { p.el.hidden = i >= this.n; if (i < this.n) p.ensureTerm(); p.el.classList.toggle('focus', i === this.cur && this.n > 1); });
    this.sync();
    emit('focus', this.focused());
  },
  setN(n) { this.want = n; store.set('npanes', n); ({ n: this.n, dir: this.dir } = this.roomFor(n)); if (this.cur >= this.n) this.cur = 0; this.apply(); },
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
      const sub = Object.keys(resume).length ? { t: 'subscribe', tabs: add, resume } : { t: 'subscribe', tabs: add };
      if (gzOk()) sub.gz = true;
      send(sub);
      add.forEach((u) => this.waiting.add(u));
    }
    this.subs = want;
  },

  again(u) {
    send({ t: 'unsubscribe', tabs: [u] });
    send(gzOk() ? { t: 'subscribe', tabs: [u], gz: true } : { t: 'subscribe', tabs: [u] });
    this.waiting.add(u);
  },
};
