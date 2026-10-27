import { emit } from './util.js';

export const KIND = { OUTPUT: 1, AUDIO: 2, AUDIO_IN: 3, SNAPSHOT_GZ: 6 };
const enc = new TextEncoder();

let gz = typeof DecompressionStream === 'function';
export const gzOk = () => gz;
let hold = false, held = [], gen = 0;
async function inflate(uid, bytes) {
  const g = ++gen;
  hold = true;
  let m = null;
  try {
    const text = await new Response(new Blob([bytes]).stream().pipeThrough(new DecompressionStream('gzip'))).text();
    m = JSON.parse(text);
  } catch (e) { console.error('snapshot could not be unpacked', e); gz = false; }
  if (g !== gen) return;
  hold = false;
  if (m) emit('msg:' + m.t, m);
  else emit('msg:error', { tab: uid, error: 'snapshot could not be unpacked: pick the tab again' });
  const q = held;
  held = [];
  for (let i = 0; i < q.length; i++) {
    if (hold) { held = q.slice(i).concat(held); break; }
    dispatch(q[i]);
  }
}
function dispatch(e) {
  if (typeof e.data === 'string') {
    let m;
    try { m = JSON.parse(e.data); } catch { return; }
    if ((m.t === 'result' || m.t === 'error') && pending.has(m.id)) {
      const p = pending.get(m.id);
      pending.delete(m.id);
      clearTimeout(p.timer);

      const r = m.t === 'error' ? { ok: false, error: m.error } : m.result && typeof m.result === 'object' && 'ok' in m.result ? { id: m.id, ...m.result } : m;
      p.resolve(r);
    }
    emit('msg:' + m.t, m);
  } else {
    const b = new Uint8Array(e.data);
    if (b.length < 5) return;
    const uid = ((b[1] << 24) | (b[2] << 16) | (b[3] << 8) | b[4]) >>> 0;
    if (b[0] === KIND.SNAPSHOT_GZ) { inflate(uid, b.subarray(5)); return; }
    emit('bin:' + b[0], uid, b.subarray(5));
  }
}
let ws = null, tries = 0, timer = 0, toolSeq = 0, everOpen = false, lastHeard = 0;
const pending = new Map();
export const conn = { open: false, connects: 0, drop: () => replace() };

function url() {
  const u = new URL('ws', location.href);
  u.protocol = location.protocol === 'https:' ? 'wss:' : 'ws:';
  u.search = '';
  u.hash = '';
  return u.href;
}

export function connect() {
  clearTimeout(timer);
  if (ws && (ws.readyState === 0 || ws.readyState === 1)) return;
  emit('conn', 'connecting');
  let s;
  try { s = new WebSocket(url()); } catch { return retry(); }
  ws = s;
  let opened = false;
  s.binaryType = 'arraybuffer';
  s.onopen = () => {
    opened = true;
    tries = 0;
    lastHeard = Date.now();
    conn.open = true;
    conn.connects++;
    emit('conn', 'live', everOpen);
    everOpen = true;
    if (queue.length) flushQueue();
  };
  s.onmessage = (e) => {
    lastHeard = Date.now();
    if (probing) { probing = false; clearTimeout(probeT); if (queue.length) flushQueue(); }
    if (hold) held.push(e);
    else dispatch(e);
  };
  s.onclose = (e) => {
    if (ws !== s) return;
    ws = null;
    conn.open = false;
    probing = false;

    hold = false; held = []; gen++;
    clearTimeout(probeT);
    for (const [, p] of pending) p.resolve({ ok: false, error: 'connection lost' });
    pending.clear();
    emit('conn', e.code === 4401 || e.code === 1008 ? 'auth' : 'down', e.code);

    if (!opened) fetch('app.css', { cache: 'no-store' }).then((r) => { if (r.status === 401) emit('conn', 'auth'); }).catch(() => {});

    if (e.code === 4000 && !document.hidden) { tries = 0; connect(); return; }
    retry();
  };
  s.onerror = () => {};
}

function retry() {
  clearTimeout(timer);
  let ms = Math.min(15000, 400 * 2 ** Math.min(tries++, 6)) * (0.7 + Math.random() * 0.6);

  if (document.hidden) ms = Math.max(ms, 60000);
  timer = setTimeout(connect, ms);
}

function replace() {
  const s = ws;
  if (!s) return;
  try { s.close(); } catch {}
  s.onclose?.({ code: 4000 });
}

addEventListener('online', () => { tries = 0; connect(); });

let hiddenAt = 0, probing = false, probeT = 0;
function wake() {
  if (document.hidden) { hiddenAt = Date.now(); return; }
  const away = hiddenAt ? Date.now() - hiddenAt : 0;
  hiddenAt = 0;
  if (!conn.open) { tries = 0; connect(); return; }
  if (away > 20000) { replace(); return; }
  const t = Date.now();
  probing = true;
  send({ t: 'ping', id: t });
  clearTimeout(probeT);
  probeT = setTimeout(() => { if (probing && ws && ws.readyState === 1 && lastHeard < t) replace(); }, 1500);
}
document.addEventListener('visibilitychange', wake);
addEventListener('pageshow', (e) => e.persisted && wake());

let lastPing = 0;
setInterval(() => {
  if (!ws || ws.readyState !== 1 || document.hidden) return;
  if (Date.now() - lastHeard > 45000) { const s = ws; try { s.close(); } catch {} s.onclose?.({ code: 4000 }); return; }
  if (Date.now() - lastPing >= 19000) { lastPing = Date.now(); send({ t: 'ping', id: lastPing }); }
}, 5000);

export function send(obj) {
  if (!ws || ws.readyState !== 1) return false;
  ws.send(JSON.stringify(obj));
  return true;
}

function binFrame(kind, uid, payload) {
  const p = typeof payload === 'string' ? enc.encode(payload) : payload;
  const b = new Uint8Array(5 + p.byteLength);
  b[0] = kind;
  b[1] = uid >>> 24; b[2] = (uid >>> 16) & 255; b[3] = (uid >>> 8) & 255; b[4] = uid & 255;
  b.set(p instanceof Uint8Array ? p : new Uint8Array(p.buffer || p, p.byteOffset || 0, p.byteLength), 5);
  return b;
}
export function sendBin(kind, uid, payload) {
  if (!ws || ws.readyState !== 1) return false;
  ws.send(binFrame(kind, uid, payload));
  return true;
}

const QUEUE_MS = 10000;
let queue = [];
export function sendInput(uid, payload) {
  if (!probing && !queue.length && sendBin(KIND.OUTPUT, uid, payload)) return 'sent';
  const now = Date.now();
  queue = queue.filter((q) => now - q.at < QUEUE_MS);
  const size = queue.reduce((a, q) => a + q.b.byteLength, 0);
  if (size > 4096) return 'dropped';
  queue.push({ at: now, b: binFrame(KIND.OUTPUT, uid, payload) });
  if (probing) return 'held';
  if (ws && ws.readyState === 1) { flushQueue(); return 'sent'; }
  connect();
  return 'queued';
}
function flushQueue() {
  const now = Date.now();
  const fresh = queue.filter((q) => now - q.at < QUEUE_MS);
  const lost = queue.length - fresh.length;
  queue = [];
  if (!ws || ws.readyState !== 1) return;
  for (const q of fresh) ws.send(q.b);
  if (lost) emit('input-lost', lost);
}

export function tool(name, args = {}, ms = 60000, extra = {}) {
  const id = 'w' + ++toolSeq;
  return new Promise((resolve) => {
    if (!send({ t: 'tool', name, args, id, ...extra })) return resolve({ ok: false, error: 'not connected' });
    const timer = setTimeout(() => { pending.delete(id); resolve({ ok: false, error: 'no reply' }); }, ms);
    pending.set(id, { resolve, timer });
  });
}
