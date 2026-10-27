import { on } from './util.js';
import { conn } from './conn.js';
import { dlog, flush, safeJson, device, standalone, errors } from './debuglog.js';

for (const lvl of ['log', 'info', 'warn', 'error']) {
  const orig = console[lvl];
  if (typeof orig !== 'function') continue;
  console[lvl] = function (...a) {
    orig.apply(console, a);
    dlog(lvl === 'error' ? 'error' : lvl === 'warn' ? 'warn' : 'console', a.map((x) => (typeof x === 'string' ? x : x instanceof Error ? `${x.name}: ${x.message}${x.stack ? ' ' + x.stack.split('\n').slice(0, 4).join(' | ') : ''}` : safeJson(x))).join(' '));
  };
}

document.addEventListener('visibilitychange', () => dlog('page', document.visibilityState));
addEventListener('pageshow', (e) => dlog('page', 'show' + (e.persisted ? ' (from cache)' : '')));
addEventListener('pagehide', () => { dlog('page', 'hide'); flush(); });
addEventListener('online', () => dlog('net', 'online'));
addEventListener('offline', () => dlog('net', 'offline'));

const vv = window.visualViewport;

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

new MutationObserver(() => dlog('theme', document.documentElement.dataset.theme || '?'))
  .observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });

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

addEventListener('pointerup', (e) => {
  const b = e.target.closest?.('#keybar button');
  if (b) count('key ' + (b.getAttribute('aria-label') || b.textContent || '?').trim().slice(0, 16));
}, true);
addEventListener('keydown', (e) => {
  const mods = (e.ctrlKey ? 'Ctrl+' : '') + (e.altKey ? 'Alt+' : '') + (e.metaKey ? 'Cmd+' : '');
  if (SPECIAL.has(e.key) || (mods && e.key.length === 1)) count('key ' + mods + (e.key.length === 1 ? e.key.toUpperCase() : e.key));
  else if (e.key.length === 1) { typed++; keyAt = Date.now(); if (!sumT) sumT = setTimeout(sumUp, 2000); }
}, true);

let keyAt = 0;
addEventListener('input', (e) => {
  if (Date.now() - keyAt < 100 || !e.data) return;
  typed += e.data.length;
  if (!sumT) sumT = setTimeout(sumUp, 2000);
}, true);
addEventListener('paste', () => count('paste'), true);

const theme = () => document.documentElement.dataset.theme || '?';
dlog('boot', `screen ${screen.width}x${screen.height} dpr ${devicePixelRatio} ${vpText()} theme ${theme()} touch ${matchMedia('(pointer:coarse)').matches} lang ${navigator.language} sw ${navigator.serviceWorker?.controller ? 'controlled' : 'none'}`);
lastVp = vpText();

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
