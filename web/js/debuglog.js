// The debug log: what happens on this device (console messages, errors,
// the socket, the viewport and keyboard, taps and keys in summary) goes to
// GodTerm in `log` frames, written to web-client.log on the Mac (read it
// with `godterm web-log`). On by default; Settings > This browser > Debug log
// turns it off on this device. Typed text, clipboard contents and audio
// are never sent: keys only by name (Esc, Ctrl, arrows), text as a count.
//
// Other modules: import { dlog } from './debuglog.js'; dlog('voice', 'started').
//
// This module is the small core (the queue, the socket's lines, errors
// from the first moment); the console copy, the viewport, taps and keys
// and the report are debughooks.js, loaded after the first paint (and
// only while the log is on), so they cost the first screen nothing.
import { on, store } from './util.js';
import { send, conn } from './conn.js';

const MAX_LINES = 500, MAX_BYTES = 64 * 1024, BATCH = 50, EVERY = 1000;
let enabled = store.get('debuglog', true);
let queue = [], bytes = 0, dropped = 0, timer = 0, busy = false;
export const errors = [];

export const debugOn = () => enabled;
export function setDebug(v) {
  enabled = !!v;
  store.set('debuglog', enabled);
  if (!enabled) { queue = []; bytes = 0; dropped = 0; }
  else { dlog('log', 'debug log on'); hooks(); }
}
// The rest of the log (debughooks.js), once; after the first paint by default.
let hooked = null;
export const hooks = () => (hooked ||= import('./debughooks.js'));
if (enabled) { const go = () => setTimeout(hooks, 1200); if (document.readyState === 'complete') go(); else addEventListener('load', go); }

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

export function safeJson(v) {
  try { return JSON.stringify(v) ?? String(v); } catch { return String(v); }
}

export function flush() {
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

// Errors and rejections from the first moment (the rest of the hooks come later).
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

// The first line: who and what this is (the screen and viewport follow with the hooks).
dlog('boot', `${device()} | ${navigator.userAgent}`);

// A state dump, as one `report` block (no screenshot): in debughooks.js.
// Resolves to whether it went out at once (the socket was open).
export function report() { return hooks().then((m) => m.report()); }
