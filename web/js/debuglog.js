// The debug log: what happens on this device (console messages, errors,
// the socket, the viewport and keyboard, taps and keys in summary) goes to
// GodTerm in `log` frames, written to web-client.log on the Mac (read it
// with `godterm web-log`). On by default; Settings > This browser > Debug log
// turns it off on this device. Typed text, clipboard contents and audio
// are never sent: keys only by name (Esc, Ctrl, arrows), text as a count.
//
// Other modules: import { dlog } from './debuglog.js'; dlog('voice', 'started').
import { on, store } from './util.js';
import { send, conn } from './conn.js';

const MAX_LINES = 500, MAX_BYTES = 64 * 1024, BATCH = 50, EVERY = 1000;
let enabled = store.get('debuglog', true);
let queue = [], bytes = 0, dropped = 0, timer = 0, busy = false;
const errors = [];

export const debugOn = () => enabled;
export function setDebug(v) {
  enabled = !!v;
  store.set('debuglog', enabled);
  if (!enabled) { queue = []; bytes = 0; dropped = 0; }
  else dlog('log', 'debug log on');
}

// One line for the log. `text` may be anything; objects become JSON.
export function dlog(cat, text) {
  if (!enabled || busy) return;
  busy = true;
  try {
    let s = typeof text === 'string' ? text : text instanceof Error ? `${text.name}: ${text.message}` : safeJson(text);
    if (s.length > 2000) s = s.slice(0, 2000) + '...';
    const line = { at: Date.now(), cat: String(cat || 'log'), text: s };
    if (cat === 'error') { errors.push(line); if (errors.length > 10) errors.shift(); }
    queue.push(line);
    bytes += s.length + 40;
    while (queue.length > MAX_LINES || bytes > MAX_BYTES) { const x = queue.shift(); bytes -= x.text.length + 40; dropped++; }
    if (queue.length >= BATCH) flush();
    else if (!timer) timer = setTimeout(flush, EVERY);
  } catch {} finally { busy = false; }
}

function safeJson(v) {
  try { return JSON.stringify(v) ?? String(v); } catch { return String(v); }
}

function flush() {
  clearTimeout(timer);
  timer = 0;
  if (!enabled || !queue.length || !conn.open) return;
  while (queue.length) {
    const lines = queue.slice(0, BATCH);
    if (dropped) lines.unshift({ at: Date.now(), cat: 'log', text: `${dropped} lines dropped (queue full)` });
    if (!send({ t: 'log', device: device(), lines })) return;
    queue = queue.slice(BATCH);
    bytes = queue.reduce((a, l) => a + l.text.length + 40, 0);
    dropped = 0;
  }
}

// "iPhone Safari standalone 402x874@3"
export const standalone = () => matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
export function device() {
  const ua = navigator.userAgent;
  const touchMac = /Macintosh/.test(ua) && navigator.maxTouchPoints > 1;
  const kind = /iPhone/.test(ua) ? 'iPhone' : /iPad/.test(ua) || touchMac ? 'iPad' : /Android/.test(ua) ? 'Android' : /Mac OS X/.test(ua) ? 'Mac' : /Windows/.test(ua) ? 'Windows' : /Linux/.test(ua) ? 'Linux' : 'device';
  const br = /CriOS|Chrome\//.test(ua) && !/Edg/.test(ua) ? 'Chrome' : /FxiOS|Firefox\//.test(ua) ? 'Firefox' : /Edg/.test(ua) ? 'Edge' : /Safari\//.test(ua) ? 'Safari' : 'browser';
  return `${kind} ${br} ${standalone() ? 'standalone' : 'tab'} ${screen.width}x${screen.height}@${Math.round(devicePixelRatio * 100) / 100}`;
}

// Console: kept as it was, and copied here.
for (const lvl of ['log', 'info', 'warn', 'error']) {
  const orig = console[lvl];
  if (typeof orig !== 'function') continue;
  console[lvl] = function (...a) {
    orig.apply(console, a);
    dlog(lvl === 'error' ? 'error' : lvl === 'warn' ? 'warn' : 'console', a.map((x) => (typeof x === 'string' ? x : x instanceof Error ? `${x.name}: ${x.message}${x.stack ? ' ' + x.stack.split('\n').slice(0, 4).join(' | ') : ''}` : safeJson(x))).join(' '));
  };
}
addEventListener('error', (e) => {
  if (e.target && e.target !== window) { dlog('error', `failed to load ${e.target.tagName?.toLowerCase()} ${e.target.src || e.target.href || ''}`); return; }
  const st = e.error?.stack ? ' ' + String(e.error.stack).split('\n').slice(0, 4).join(' | ') : '';
  dlog('error', `${e.message} at ${e.filename || '?'}:${e.lineno}:${e.colno}${st}`);
}, true);
addEventListener('unhandledrejection', (e) => {
  const r = e.reason;
  dlog('error', 'unhandled rejection: ' + (r instanceof Error ? `${r.name}: ${r.message}${r.stack ? ' ' + r.stack.split('\n').slice(0, 4).join(' | ') : ''}` : safeJson(r)));
});

// The socket: opens, closes (with the code), retries, ping round trips.
let downAt = 0, tries = 0;
on('conn', (s, x) => {
  if (s === 'live') {
    dlog('conn', `open (connect ${conn.connects}${downAt ? ', down ' + ((Date.now() - downAt) / 1000).toFixed(1) + ' s, ' + tries + ' tries' : ''})`);
    downAt = 0; tries = 0;
    flush();
  } else if (s === 'connecting') { if (downAt) tries++; }
  else { if (!downAt) downAt = Date.now(); if (tries === 0) dlog('conn', `${s}${x != null ? ' code ' + x : ''}`); }
});
on('msg:pong', (m) => { const ms = Date.now() - Number(m.id); if (ms >= 0 && ms < 600000) dlog('ping', `${ms} ms`); });
on('input-lost', (n) => dlog('conn', `${n} queued inputs dropped`));
on('hello', (m) => dlog('conn', `hello client ${m.client} build ${m.build} version ${m.version}`));

// The page: shown or hidden, back from the cache.
document.addEventListener('visibilitychange', () => dlog('page', document.visibilityState));
addEventListener('pageshow', (e) => dlog('page', 'show' + (e.persisted ? ' (from cache)' : '')));
addEventListener('pagehide', () => { dlog('page', 'hide'); flush(); });
addEventListener('online', () => dlog('net', 'online'));
addEventListener('offline', () => dlog('net', 'offline'));

// The viewport and the on-screen keyboard (main.js sets body.kb), once
// things settle.
const vv = window.visualViewport;
// Also where the page is scrolled to (it should never be) and where the
// app and its key bar end up, to see the keyboard's effect on a phone.
const vpText = () => {
  const r = (sel) => { const e = document.querySelector(sel); if (!e || !e.offsetParent && sel !== '#app') return ''; const b = e.getBoundingClientRect(); return `${Math.round(b.top)}-${Math.round(b.bottom)}`; };
  const kbar = r('#keybar');
  return `${innerWidth}x${innerHeight} visual ${vv ? Math.round(vv.width) + 'x' + Math.round(vv.height) + ' top ' + Math.round(vv.offsetTop) + ' scale ' + Math.round(vv.scale * 100) / 100 : 'none'} ${screen.orientation?.type || ''} scroll ${Math.round(scrollY)} app ${r('#app')}${kbar ? ' keys ' + kbar : ''}`.trim();
};
let lastVp = '', vpT = 0;
const vpChanged = () => { clearTimeout(vpT); vpT = setTimeout(() => { const t = vpText(); if (t !== lastVp) { lastVp = t; dlog('viewport', t); } }, 400); };
vv?.addEventListener('resize', vpChanged);
vv?.addEventListener('scroll', vpChanged);
addEventListener('resize', vpChanged);
screen.orientation?.addEventListener?.('change', vpChanged);
let kbWas = false;
new MutationObserver(() => {
  const kb = document.body.classList.contains('kb');
  if (kb !== kbWas) { kbWas = kb; dlog('keyboard', kb ? `open (visual height ${Math.round(vv?.height || innerHeight)})` : 'closed'); }
}).observe(document.body, { attributes: true, attributeFilter: ['class'] });

// Theme changes (data-theme on <html>).
new MutationObserver(() => dlog('theme', document.documentElement.dataset.theme || '?'))
  .observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

// Touches and keys, summed up every 2 s: taps and swipes by area, special
// keys by name, typed text only as a count.
const SPECIAL = new Set(['Escape', 'Tab', 'Enter', 'Backspace', 'Delete', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown', 'Insert']);
let sum = {}, typed = 0, sumT = 0;
function count(k) {
  sum[k] = (sum[k] || 0) + 1;
  if (!sumT) sumT = setTimeout(sumUp, 2000);
}
function sumUp() {
  sumT = 0;
  const parts = Object.entries(sum).map(([k, n]) => (n > 1 ? `${k} x${n}` : k));
  if (typed) parts.push(`typed ${typed} chars`);
  sum = {}; typed = 0;
  if (parts.length) dlog('ui', parts.join(', '));
}
function area(el) {
  for (let e = el; e && e !== document.body; e = e.parentElement) {
    if (e.id === 'keybar') return 'keybar';
    if (e.classList?.contains('xterm')) return 'term';
    if (e.id) return e.id;
  }
  return 'page';
}
let t0 = null;
addEventListener('touchstart', (e) => { const t = e.touches[0]; t0 = t && e.touches.length === 1 ? { x: t.clientX, y: t.clientY, at: Date.now(), el: e.target } : null; if (e.touches.length > 1) count('pinch'); }, { capture: true, passive: true });
addEventListener('touchend', (e) => {
  if (!t0) return;
  const t = e.changedTouches[0], dx = t.clientX - t0.x, dy = t.clientY - t0.y, a = area(t0.el);
  const kind = Math.abs(dx) + Math.abs(dy) < 12 ? (Date.now() - t0.at > 500 ? 'long press' : 'tap') : Math.abs(dx) > Math.abs(dy) ? (dx > 0 ? 'swipe right' : 'swipe left') : dy > 0 ? 'swipe down' : 'swipe up';
  t0 = null;
  count(`${kind} ${a}`);
}, { capture: true, passive: true });
// Key bar buttons: their labels (Esc, Ctrl, arrows...), never text.
addEventListener('pointerup', (e) => {
  const b = e.target.closest?.('#keybar button');
  if (b) count('key ' + (b.getAttribute('aria-label') || b.textContent || '?').trim().slice(0, 16));
}, true);
addEventListener('keydown', (e) => {
  const mods = (e.ctrlKey ? 'Ctrl+' : '') + (e.altKey ? 'Alt+' : '') + (e.metaKey ? 'Cmd+' : '');
  if (SPECIAL.has(e.key) || (mods && e.key.length === 1)) count('key ' + mods + (e.key.length === 1 ? e.key.toUpperCase() : e.key));
  else if (e.key.length === 1) { typed++; keyAt = Date.now(); if (!sumT) sumT = setTimeout(sumUp, 2000); }
}, true);
// Text from a phone keyboard (its keydowns say "Unidentified"), dictation
// or autocorrect arrives as input events: counted unless a key just was.
let keyAt = 0;
addEventListener('input', (e) => {
  if (Date.now() - keyAt < 100 || !e.data) return;
  typed += e.data.length;
  if (!sumT) sumT = setTimeout(sumUp, 2000);
}, true);
addEventListener('paste', () => count('paste'), true);

// The first lines: who and what this is.
const theme = () => document.documentElement.dataset.theme || '?';
dlog('boot', `${device()} | ${navigator.userAgent}`);
dlog('boot', `screen ${screen.width}x${screen.height} dpr ${devicePixelRatio} ${vpText()} theme ${theme()} touch ${matchMedia('(pointer:coarse)').matches} lang ${navigator.language} sw ${navigator.serviceWorker?.controller ? 'controlled' : 'none'}`);
lastVp = vpText();

// A state dump, as one `report` block (no screenshot).
export function report() {
  const L = (t) => dlog('report', t);
  const G = window.GT || {};
  L('---- debug report ----');
  L(`device ${device()}`);
  L(`ua ${navigator.userAgent}`);
  L(`build ${G.S?.build || '?'} version ${G.S?.version || '?'} theme ${theme()} standalone ${standalone()}`);
  L(`conn open ${conn.open} connects ${conn.connects} online ${navigator.onLine} visibility ${document.visibilityState}`);
  L(`viewport ${vpText()} dpr ${devicePixelRatio} keyboard ${document.body.classList.contains('kb')}`);
  const ps = G.panes;
  if (ps) {
    L(`panes ${ps.n} focused ${ps.cur} subscribed [${[...(ps.subs || [])].join(', ')}] tabs ${G.S?.tabs?.length ?? '?'}`);
    ps.visible().forEach((p, i) => {
      const t = p.term, r = p.host?.getBoundingClientRect?.();
      let cell = '';
      try { const c = t?._core?._renderService?.dimensions?.css?.cell; if (c) cell = ` cell ${c.width.toFixed(2)}x${c.height.toFixed(2)}`; } catch {}
      L(`pane ${i + 1} tab ${p.id || '-'} ready ${!!p.ready} pty ${p.cols}x${p.rows} xterm ${t?.cols}x${t?.rows} font ${t?.options?.fontSize}px ${String(t?.options?.fontFamily || '').split(',')[0]}${cell} box ${r ? Math.round(r.width) + 'x' + Math.round(r.height) : '?'} renderer ${p.renderer || '?'} take ${!!p.take}`);
    });
  }
  L(`recent errors ${errors.length}`);
  for (const e of errors) L(`  ${new Date(e.at).toISOString()} ${e.text}`);
  L('---- end of report ----');
  flush();
  return conn.open;
}
