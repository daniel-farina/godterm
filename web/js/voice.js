import { $, h, on, store, toast, skeleton, failRow } from './util.js';
import { send, sendBin, KIND, tool, conn } from './conn.js';
import { S } from './state.js';
import { dlog as clog } from './debuglog.js';
import { openSheet, openMenu, closeSheet, menuOpen, sheetKind } from './sheet.js';
import { panes } from './term.js';
import { METHODS, openCapture, recorderType } from './mic.js';

const KIND_REC = 4;
const q = new URLSearchParams(location.search);

let ctx = null, playAt = 0;
const playing = new Set();

let held = [];

const vol0 = +store.get('voice.spkvol', 1);
let spkVol = Number.isFinite(vol0) ? Math.max(0, Math.min(1, vol0)) : 1;
let spkMuted = store.get('voice.spkmute', false) === true;
let outGain = null;

let micMuted = store.get('voice.micmute', false) === true;

let stream = null, cap = null, mode = '';

let phase = '', err = null, hint = '';

let away = false, lost = '';

let level = 0, peakAt = 0, levelDb = -90;
const st = { frames: 0, nonzero: 0, sumsq: 0, n: 0, peak: 0, since: 0, started: 0, any: false, sound: false, total: 0 };

let fps = 0, fpsN = 0, fpsAt = 0;

let diag = null;

const DEBUG_KEY = 'gt-voice-debug';
let debug = false;

let voiceTest = +q.get('voicetest') || 0;
try {
  const d = q.get('voicedebug');
  if (d === '1' || d === '0') localStorage.setItem(DEBUG_KEY, d);
  debug = localStorage.getItem(DEBUG_KEY) === '1';
} catch {}
{
  const m = q.get('micmethod');
  if (m === 'auto') store.set('voice.force', '');
  else if (METHODS.includes(m)) store.set('voice.force', m);
  const b = q.get('micbreak');
  if (b != null) store.set('voice.break', b === '0' ? '' : b);
}
const broken = () => String(store.get('voice.break', '') || '').split(',').filter(Boolean);

let hfMode = store.get('voice.hf', 'open') === 'wake' ? 'wake' : 'open';

const PICKS = ['off', 'push', 'open', 'wake'];
let pick = PICKS.includes(store.get('voice.pick', 'off')) ? store.get('voice.pick', 'off') : 'off';

let audioAt = 0, sessionAt = 0;
const NOAUDIO_MS = 2000;
const noAudio = () => !!mode && performance.now() - Math.max(audioAt, sessionAt) > NOAUDIO_MS;

function dlog(line) {
  clog('voice', line);
  if (!debug) return;
  console.log('[voice]', line);
  send({ t: 'voice', log: line });
}

const secure = () => window.isSecureContext && !!navigator.mediaDevices?.getUserMedia;
const handsFree = () => mode === 'open' || mode === 'wake';
const wait = (p, ms) => Promise.race([p, new Promise((r) => setTimeout(r, ms))]);

function audioCtx() {
  if (!ctx) {
    const C = window.AudioContext || window.webkitAudioContext;
    if (!C) return null;
    ctx = new C();
    ctx.onstatechange = () => { dlog('audio context ' + ctx.state); paint(); };
    try { outGain = ctx.createGain(); outGain.gain.value = spkGain(); outGain.connect(ctx.destination); } catch { outGain = null; }
  }
  if (ctx.state !== 'running') ctx.resume().catch(() => {});
  return ctx;
}

function unlock() {
  const c = audioCtx();
  if (!c) return;
  try {
    const b = c.createBuffer(1, 1, c.sampleRate);
    const s = c.createBufferSource();
    s.buffer = b;
    s.connect(outGain || c.destination);
    s.start(0);
  } catch {}
  if (held.length) { const h2 = held; held = []; for (const b of h2) enqueue(b); }
}

let unmuteAt = 0;
function fail(code, text) {
  err = { code, text };
  dlog('error ' + code + ': ' + text);
  paint();
}

const IDLE_MS = 90000;
let serverOn = '', idleT = 0;

function voiceOn(m) {
  clearTimeout(idleT);
  if (serverOn !== m) { send({ t: 'voice', on: true, mode: m }); serverOn = m; if (micMuted) send({ t: 'voice', muted: true }); }
}
function voiceOff() {
  clearTimeout(idleT);
  if (!mode) release();
  if (serverOn) send({ t: 'voice', on: false });
  serverOn = '';
  paint();
}
function voiceOffSoon() {
  clearTimeout(idleT);
  idleT = setTimeout(() => { if (serverOn && !mode) voiceOff(); }, IDLE_MS);
}

function micError(e) {
  const n = e?.name || '';
  if (n === 'NotAllowedError' || n === 'SecurityError') {
    return ['denied', 'Mic permission denied. Allow the microphone for this site (Safari: aA > Website Settings > Microphone, or Settings > Safari > Microphone) and try again.'];
  }
  if (n === 'NotFoundError' || n === 'OverconstrainedError') return ['nomic', 'No microphone found on this device.'];
  if (n === 'NotReadableError' || n === 'AbortError') return ['busy', 'The mic is in use by another app (a call, a recording). Close it and try again.'];
  return ['mic', 'The mic did not start: ' + (e?.message || n || e)];
}

async function getMic() {
  const want = { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true };
  let s;
  try {
    s = await navigator.mediaDevices.getUserMedia({ audio: want });
  } catch (e) {
    if (e?.name !== 'OverconstrainedError' && e?.name !== 'TypeError') throw e;
    s = await navigator.mediaDevices.getUserMedia({ audio: true });
  }
  try {
    const g = s.getAudioTracks()[0]?.getSettings?.() || {};
    const f = (k) => (g[k] === undefined ? '?' : g[k] ? 'on' : 'off');
    dlog(`mic opened: echo cancellation ${f('echoCancellation')}, noise suppression ${f('noiseSuppression')}, gain control ${f('autoGainControl')}`);
  } catch {}
  return s;
}

const dbOf = (ss, n) => Math.max(-90, 20 * Math.log10(Math.sqrt(ss / Math.max(1, n)) / 32768 + 1e-9));

function setLevel(db) {
  levelDb = db;
  const l = Math.max(0, Math.min(1, (db + 60) / 60));
  const now = performance.now();
  level = l > level ? l : Math.max(l, level - (now - peakAt) / 900);
  peakAt = now;
  meter();
}

let zeros = null;
function onPcm(buf) {
  let a = new Int16Array(buf);

  if (micMuted) { if (zeros?.length !== a.length) zeros = new Int16Array(a.length); a = zeros; }
  let ss = 0, pk = 0, nz = 0;
  for (let i = 0; i < a.length; i++) {
    const v = a[i];
    if (v) nz++;
    ss += v * v;
    const m = v < 0 ? -v : v;
    if (m > pk) pk = m;
  }
  st.frames++; st.total++; st.nonzero += nz; st.sumsq += ss; st.n += a.length; st.peak = Math.max(st.peak, pk);
  const now = performance.now();
  audioAt = now;
  fpsN++;
  if (now - fpsAt >= 1000) { fps = fpsN; fpsN = 0; fpsAt = now; liveInfo(); }
  if (!st.any) { st.any = true; paint(); }
  if (nz && !st.sound) { st.sound = true; if (err?.code === 'silent' || err?.code === 'noaudio') err = null; paint(); }
  setLevel(dbOf(ss, a.length));
  if (debug && now - st.since >= 1000) {
    const rms = st.n ? dbOf(st.sumsq, st.n) : -90;
    const tr = stream?.getAudioTracks()[0];
    dlog(`mic ${st.frames} frames/s, level ${rms.toFixed(0)} dBFS, peak ${(20 * Math.log10(st.peak / 32768 + 1e-9)).toFixed(0)}, ${st.n ? Math.round(st.nonzero * 100 / st.n) : 0}% non zero, ${cap?.what || 'no capture'}, track ${tr ? tr.readyState + (tr.muted ? ' muted' : '') : 'none'}, ${mode || 'not sending'}`);
    st.frames = st.nonzero = st.sumsq = st.n = st.peak = 0;
    st.since = now;
  }
  if (mode && mode !== 'test') sendBin(KIND.AUDIO_IN, 0, a);
}

let recPieces = 0;
function onChunk(bytes) {
  recPieces++;
  if (!st.any) { st.any = true; paint(); }
  if (mode !== 'test') sendBin(KIND_REC, 0, bytes);
}

let recLevelAt = 0, recMs = 0, recHeard = false;
function onRecLevel(m) {
  recLevelAt = audioAt = performance.now();
  recMs = m.ms || 0;
  if (cap?.method === 'recorder' && !recHeard) { recHeard = true; remember(cap, `GodTerm decoded ${m.ms} ms`); }
  if (m.db > -89 && !st.sound) { st.sound = true; if (err?.code === 'silent' || err?.code === 'noaudio') err = null; paint(); }
  setLevel(m.db);
  liveInfo();
}

function onDiag(d) {
  if (d?.hello) { dlog(`worklet running at ${d.rate} Hz`); return; }
  diag = d;
}

function order() {
  const f = store.get('voice.force', '');
  if (METHODS.includes(f)) return [f];
  const last = store.get('voice.method', '');
  return METHODS.includes(last) ? [last, ...METHODS.filter((m) => m !== last)] : [...METHODS];
}

function resetStats() {
  Object.assign(st, { frames: 0, nonzero: 0, sumsq: 0, n: 0, peak: 0, since: performance.now(), started: performance.now(), any: false, sound: false });
  diag = null;
  recPieces = 0;
  recLevelAt = 0;
  audioAt = 0;
}

let probing = 0, probeDone = null;

let shadow = null;
function startShadow() {
  if (shadow || !window.MediaRecorder || store.get('voice.method', '') || !order().includes('recorder') || order()[0] === 'recorder') return;
  const box = { buffer: [] };
  openCapture('recorder', stream, { onChunk: (b) => (box.buffer ? box.buffer.push(b) : onChunk(b)), broken: broken().includes('recorder'), log: dlog })
    .then((c) => {
      if (!stream) { c.close(); return; }
      c.start(() => {});
      c.box = box;
      shadow = c;
    })
    .catch((e) => dlog('no recorder beside: ' + (e?.message || e)));
}
function dropShadow() {
  if (!shadow) return;
  shadow.close();
  shadow = null;
}

async function openFirst(list) {
  const run = ++probing;
  for (let i = 0; i < list.length; i++) {
    const m = list[i];
    let c;
    try {
      if (m === 'recorder' && shadow) {

        c = shadow;
        shadow = null;
        cap = c;
        const kept = c.box.buffer;
        c.box.buffer = null;
        const sending = handsFree() || (mode === 'push' && pressed);
        if (sending) send({ t: 'voice', rec: c.mime });
        for (const b of kept) onChunk(b);
        if (!sending) c.stop();
        dlog(`mic method recorder: the one beside, ${kept.length} pieces kept`);
        return { c, rest: list.slice(i + 1), run };
      }
      c = await openCapture(m, stream, { onPcm, onChunk, onDiag, broken: broken().includes(m), log: dlog });
    } catch (e) {
      dlog(`mic method ${m} failed to open: ${e?.message || e}`);
      continue;
    }
    if (run !== probing || !stream) { c.close(); return null; }
    cap = c;
    resetStats();
    if (m === 'recorder' && (handsFree() || mode === 'test' || (mode === 'push' && pressed))) beginRec();
    return { c, rest: list.slice(i + 1), run };
  }
  return null;
}

async function probe(o) {
  if (!o) return;
  let { c, rest, run } = o;
  for (;;) {
    if (c.method === 'recorder') {

      setTimeout(() => {
        if (run !== probing || cap !== c || !mode || mode === 'test') return;
        if (!recLevelAt && recPieces) dlog(`recorder: ${recPieces} pieces sent, GodTerm decoded nothing yet`);
      }, 3000);
      recHeard = false;
      return;
    }
    const t0 = performance.now();
    for (;;) {
      await new Promise((r) => setTimeout(r, 100));
      if (run !== probing || cap !== c) return;
      const t = performance.now() - t0;
      if (st.sound) { dropShadow(); remember(c, `${st.total} frames`); return; }

      if (t >= 700 && !st.any) break;
      if (t >= 2000) break;
    }
    const why = !st.any
      ? `no samples in 700 ms${diag ? ` (process ran ${diag.calls} times, ${diag.empty} with no input)` : ' (the processor never ran)'}`
      : 'only zeros for 2 s';
    dlog(`mic method ${c.method}: ${why}, ${c.what}; trying ${rest[0] || 'nothing else'}`);
    if (!rest.length) { dropShadow(); return; }
    c.close();
    if (cap === c) cap = null;
    const n = await openFirst(rest);
    if (!n) return;
    ({ c, rest, run } = n);
    dlog(`mic method now ${c.method}: ${c.what}`);
    paint();
  }
}

function remember(c, why) {
  if (store.get('voice.method', '') !== c.method) store.set('voice.method', c.method);
  dlog(`mic method ${c.method} works (${why})`);
  liveInfo();
}

function beginRec() {
  if (!cap || cap.method !== 'recorder' || cap.recording()) return;
  cap.start((mime) => { if (mode !== 'test') send({ t: 'voice', rec: mime }); });
}
async function endRec() {
  if (!cap || cap.method !== 'recorder') return;
  await cap.stop();
  if (mode !== 'test') send({ t: 'voice', rec_end: true });
}

let starting = null, pressed = false;
async function start(m) {
  if (!secure()) {
    fail('https', location.protocol === 'https:' ? 'This browser has no microphone access.' : 'Voice needs HTTPS: open the https tailnet link (tailscale serve) or localhost. Plain http cannot use the mic.');
    return false;
  }
  if (mode) return true;
  mode = m;
  phase = 'starting';
  hint = '';
  lost = '';
  paint();

  if (!audioCtx()) { mode = ''; phase = ''; fail('noaudio', 'This browser has no Web Audio.'); return false; }
  unlock();

  if (m === 'push' && (playing.size || held.length || S.voice?.speaking)) hush();
  try {
    const t0 = performance.now();

    const live = stream && cap && stream.getAudioTracks().some((t) => t.readyState === 'live');
    if (live) {
      await cap.resume();
      if (cap.method === 'recorder') beginRec();
      const ok = began(m, t0);
      keptCheck();
      return ok;
    }
    release();

    const ask = setTimeout(() => { if (phase === 'starting') { phase = 'asking'; paint(); } }, 700);
    try { stream = await getMic(); } finally { clearTimeout(ask); }
    if (phase === 'asking') phase = 'starting';
    if (!mode) { release(); return false; }
    watchTrack(stream);
    startShadow();

    const o = await openFirst(order());
    if (!o) throw Object.assign(new Error('no way to read the mic worked here'), { name: 'NoCapture' });
    if (!mode) { release(); return false; }
    const ok = began(m, t0);
    probeDone = probe(o);
    return ok;
  } catch (e) {
    mode = '';
    phase = '';
    release();
    fail(...micError(e));
    return false;
  }
}

function keptCheck() {
  const at = st.started;
  const quiet = () => mode && st.started === at && !st.any && cap && cap.method !== 'recorder';
  setTimeout(async () => {
    if (!quiet()) return;
    const from = METHODS.indexOf(cap.method);
    dlog(`no audio from the kept mic (${cap.what}): built again`);
    cap.close();
    cap = null;
    const o = await openFirst(METHODS.slice(Math.max(0, from)));
    if (o) { st.started = at; probeDone = probe(o); }
  }, 600);
  setTimeout(async () => {
    if (!mode || st.started !== at || st.any || cap?.method === 'recorder') return;
    dlog('still no audio: opening the mic again');
    try {
      const keep = mode;
      probing++;
      dropShadow();
      try { cap?.close(); } catch {}
      cap = null;
      stream?.getTracks().forEach((t) => { t.onended = t.onmute = t.onunmute = null; t.stop(); });
      stream = await getMic();
      if (mode !== keep) { release(); return; }
      watchTrack(stream);
      muteTrack();
      startShadow();
      const o = await openFirst(order());
      if (o) { st.started = at; probeDone = probe(o); }
    } catch (e) { dlog('reopen failed: ' + (e?.message || e)); }
  }, 1400);
}

function began(m, t0) {
  resetStats();
  muteTrack();
  sessionAt = performance.now();
  const tr = stream.getAudioTracks()[0];
  dlog(`mic on (${m}) in ${Math.round(performance.now() - t0)} ms: ${tr?.label || 'track'} ${JSON.stringify(tr?.getSettings?.() || {})}, ${cap?.what}, playback ${ctx?.state} ${ctx?.sampleRate} Hz${navigator.audioSession ? ', audio session ' + navigator.audioSession.type : ''}`);
  if (err && err.code !== 'server') err = null;
  if (m === 'push') { send({ t: 'voice', talk: true }); serverOn = serverOn || 'push'; }
  else if (m !== 'test') voiceOn(m);
  phase = 'listening';
  paint();
  checkAudio();
  return true;
}

function checkAudio() {
  const at = st.started;
  setTimeout(() => {
    if (!mode || st.started !== at) return;
    if (cap && cap.method === 'recorder' && recPieces && !recLevelAt) {
      if (mode !== 'test') fail('noaudio', 'The mic records, but GodTerm decoded no audio from it yet. Check GodTerm\'s ffmpeg, or release and press again.');
      return;
    }
    if (!st.any) {
      fail('noaudio', `No audio from the mic (${cap?.what || 'no capture'}). Release and press again; if it stays, reload the app.`);
    } else if (!st.sound && !micMuted) {
      fail('silent', 'The mic gives only silence. Check that no other app or the system has muted it (a call, Control Center, a hardware switch).');
    }
  }, 4500);
}

function watchTrack(s) {
  const tr = s.getAudioTracks()[0];
  if (!tr) return;
  tr.onended = () => { dlog('mic track ended'); if (stream === s) { lost = 'ended'; paint(); } };
  tr.onmute = () => { dlog('mic track muted'); if (stream === s) { lost = 'muted'; paint(); } };
  tr.onunmute = () => { dlog('mic track unmuted'); if (stream === s) { lost = ''; paint(); } };
}

async function backAgain() {
  if (!away) return;
  away = false;
  if (serverOn) send({ t: 'voice', away: false });
  const m = mode;
  const tr = stream?.getAudioTracks()[0];
  const ok = tr && tr.readyState === 'live' && !tr.muted;
  dlog(`back in the foreground: mic ${tr ? tr.readyState + (tr.muted ? ' muted' : '') : 'none'}, ${m || 'not sending'}`);
  if (ok) { await cap?.resume(); lost = ''; paint(); return; }
  if (!handsFree()) { release(); lost = ''; paint(); return; }

  probing++;
  mode = '';
  release();
  if (!(await start(m))) { hint = ''; lost = 'tap'; resumeMode = m; err = null; paint(); }
}
let resumeMode = '';

function release() {
  probing++;
  dropShadow();
  try { cap?.close(); } catch {}
  cap = null;
  stream?.getTracks().forEach((t) => { t.onended = t.onmute = t.onunmute = null; t.stop(); });
  stream = null;
  level = 0;
  levelDb = -90;
  meter();
}

function stop() {
  const was = mode;
  if (!was) return;
  if (was === 'push') {

    phase = 'sending';
    paint();
    const done = () => {
      mode = '';
      send({ t: 'voice', talk: false });
      phase = 'thinking';
      thinkingUntil = performance.now() + 4000;
      paint();
      voiceOffSoon();
    };

    setTimeout(async () => {
      await wait(probeDone || Promise.resolve(), 1600);
      if (cap?.method === 'recorder') await endRec();
      done();
    }, 250);
    return;
  }
  const rec = cap?.method === 'recorder';
  if (rec && was !== 'test') endRec();
  mode = '';
  phase = '';
  release();
  if (was !== 'test') voiceOff();
  paint();
}

let thinkingUntil = 0;

let heardText = '', heardUntil = 0, lastHeard = null;

function shown() {
  const v = S.voice || {};
  if (lost && mode) return 'paused';
  if (lost === 'tap') return 'paused';
  if (playing.size) return 'speaking';
  if (held.length) return 'locked';
  if (mode === 'push') return phase === 'listening' && noAudio() ? 'noaudio' : phase === 'listening' && !st.any ? 'starting' : phase;
  if (mode === 'test') return 'test';

  if (pick === 'off' && !err) return '';
  const busy = serverOn && (v.status === 'transcribing' || S.assistant?.busy);
  if (handsFree()) {
    if (phase === 'starting' || phase === 'asking') return phase;
    if (noAudio()) return 'noaudio';
    if (micMuted) return 'muted';

    if (!st.any) return 'starting';
    if (v.speaking) return 'speaking';
    if (v.status === 'transcribing') return 'heard';
    if (S.assistant?.busy) return 'thinking';
    if (performance.now() < heardUntil) return 'heard';
    if (levelDb > -38) return 'hearing';
    if (mode === 'wake' && !v.awake) return 'waiting';
    return 'listening';
  }
  if (busy) return 'thinking';
  if (phase === 'thinking' && performance.now() < thinkingUntil) return 'thinking';
  return '';
}

const CSS = `
#voice-bar{flex-wrap:wrap}
#voice-bar .vb-main{flex:1;min-width:0;display:flex;flex-direction:column;gap:2px}
#voice-bar .vb-sub{font-size:12px;color:var(--dim);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
#voice-bar .vb-sub:empty{display:none}
#voice-bar .vb-set{flex:none;display:grid;place-items:center;width:32px;height:32px;border:0;border-radius:8px;background:none;color:var(--dim)}
#voice-bar .vb-set svg,#voice-bar .vb-x svg{width:18px;height:18px;fill:none;stroke:currentColor;stroke-width:1.8;stroke-linecap:round}
#voice-bar[data-hf]{padding-top:12px;padding-bottom:12px}
#voice-bar[data-hf] .vb-text{font-size:17px;font-weight:600}
#voice-bar[data-hf] .vb-dot{width:14px;height:14px}
#voice-bar[data-hf] .vb-meter{width:96px;height:8px;border-radius:4px}
#voice-bar[data-state=hearing] .vb-dot{background:var(--warn)}
#voice-bar[data-state=heard] .vb-dot{background:var(--busy)}
#voice-bar[data-state=waiting] .vb-dot,#voice-bar[data-state=paused] .vb-dot{background:var(--dim)}
#voice-bar[data-state=listening][data-hf] .vb-dot{background:var(--ok)}
#voice-bar[data-state=paused]{border-color:var(--warn)}
.vs{display:flex;flex-direction:column;gap:14px}
.vs h3{margin:0;font-size:17px}
.vs h4{margin:4px 0 0;font-size:13px;color:var(--dim);font-weight:600;text-transform:uppercase;letter-spacing:.04em}
.vs .seg{display:flex;border:1px solid var(--line);border-radius:10px;overflow:hidden}
.vs .seg button{flex:1;min-height:40px;border:0;background:none;color:inherit;font:inherit}
.vs .seg button+button{border-left:1px solid var(--line)}
.vs .seg button[aria-pressed=true]{background:var(--line);font-weight:600}
.vs .row{display:flex;align-items:center;gap:10px;min-height:36px}
.vs .row>span{flex:1;min-width:0}
.vs .row small,.vs .note{display:block;color:var(--dim);font-size:12px;line-height:1.35}
.vs .vs-live{display:flex;align-items:center;gap:10px;font-size:13px;color:var(--dim)}
.vs .vs-live .m{flex:none;width:120px;height:8px;border-radius:4px;background:var(--line);overflow:hidden}
.vs .vs-live .m i{display:block;height:100%;background:var(--ok);transform-origin:left;transform:scaleX(0);transition:transform 60ms linear}
.vs select,.vs input[type=text],.vs input[type=number]{min-height:34px;max-width:46%;border:1px solid var(--line);border-radius:8px;background:none;color:inherit;font:inherit;padding:0 8px}
.vs .btn{min-height:36px}
.touch .vs .btn,.touch .vs .seg button,.touch .vs .row{min-height:44px}
.touch .vs select,.touch .vs input[type=text],.touch .vs input[type=number]{min-height:44px}
`;

let bar = null;
const SET_ICON = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h10M18 7h2M4 17h4M12 17h8"/><circle cx="16" cy="7" r="2"/><circle cx="10" cy="17" r="2"/></svg>';
function ensureBar() {
  if (bar) return bar;
  document.head.append(h('style', { id: 'voice-css' }, CSS));
  bar = h('div', { id: 'voice-bar', role: 'status', 'aria-live': 'polite', hidden: true },
    h('span.vb-dot'),
    h('span.vb-main', {}, h('span.vb-text'), h('span.vb-sub')),
    h('span.vb-lv', {}, h('span.vb-meter', {}, h('i')), h('span.vb-db')),
    h('button.vb-act', { type: 'button', hidden: true }),
    h('button.vb-set', { type: 'button', 'aria-label': 'Voice settings', title: 'Voice settings', html: SET_ICON }),
    h('button.vb-x', { type: 'button', 'aria-label': 'Dismiss', title: 'Dismiss', html: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>' }));
  bar.querySelector('.vb-x').addEventListener('click', () => {
    if (err) { err = null; hint = ''; paint(); return; }
    hint = '';
    if (lost === 'tap') { lost = ''; resumeMode = ''; voiceOff(); }
    else if (mode && mode !== 'push') stop();
    else if (serverOn) voiceOff();
    paint();
  });
  bar.querySelector('.vb-set').addEventListener('click', openVoiceSheet);
  bar.querySelector('.vb-act').addEventListener('click', () => {
    const a = bar.querySelector('.vb-act').dataset.act;
    if (a === 'stop') { hush(); send({ t: 'voice', hush: true }); }
    else if (a === 'resume') { const m = resumeMode || (pick === 'open' || pick === 'wake' ? pick : hfMode); lost = ''; resumeMode = ''; start(m); }
    else { audioCtx(); unlock(); }
    paint();
  });
  $('#app').append(bar);
  return bar;
}

const dbText = () => (noAudio() ? 'no audio' : !st.any && !audioAt ? '' : `${Math.max(-90, Math.round(Math.max(levelDb, level > 0.005 ? level * 60 - 60 : -90)))} dB`);
let dbAt = 0;
function meter() {
  const s = `scaleX(${mode ? Math.max(0.02, level) : 0})`;
  for (const sel of ['#voice-bar .vb-meter i', '#mic-strip .ms-meter i', '#talk .tk-meter i', '.vs .vs-live .m i']) {
    const m = document.querySelector(sel);
    if (m) m.style.transform = s;
  }

  const now = performance.now();
  if (now - dbAt < 120) return;
  dbAt = now;
  const t = mode ? dbText() : '';
  for (const sel of ['#voice-bar .vb-db', '#mic-strip .ms-db', '#talk .tk-db']) {
    const el = document.querySelector(sel);
    if (el && el.textContent !== t) el.textContent = t;
  }
}

const MODE = { off: 'Voice off', push: 'Push to talk', open: 'Open mic', wake: 'Wake word' };
const STATE = {
  starting: 'starting the mic',
  asking: 'allow the mic',
  listening: 'listening',
  hearing: 'hearing you',
  heard: 'heard you',
  sending: 'sending',
  thinking: 'thinking',
  speaking: 'speaking',
  waiting: 'waiting for the wake word',
  locked: 'a reply is waiting',
  paused: 'paused',
  test: 'mic test',
  noaudio: 'no audio',
  muted: 'muted',
  error: 'not listening',
};
const SHORT = { off: 'Off', push: 'PTT', open: 'Open', wake: 'Wake' };
const upFirst = (w) => (w ? w[0].toUpperCase() + w.slice(1) : '');
const modeName = (m) => MODE[m] || '';
export function micWords(m = pick, s = shown(), mu = micMuted) {
  const mode = MODE[m] || '';
  const on = m && m !== 'off';
  let state = !on ? '' : mu ? (s === 'speaking' ? 'muted \u00b7 speaking' : 'muted') : STATE[s] || '';
  if (s === 'error') state = STATE.error;
  return { mode, state, text: state ? `${mode} \u00b7 ${state}` : mode, short: on && mu ? 'Muted' : SHORT[m] || '', State: upFirst(state) };
}

const TEXT = new Proxy({}, { get: (_, k) => (STATE[k] ? upFirst(STATE[k]) : undefined) });

const wakeWord = () => S.voice?.wake_word || 'hey god';

const wakeLabel = () => wakeWord().replace(/\b\w/g, (c) => c.toUpperCase());

const MIC = '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>';
const ICON = {
  off: MIC + '<path d="M4 3l16 18"/>',
  push: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3M8.5 21h7"/>',
  open: MIC + '<path d="M2 8v6M22 8v6"/>',
  wake: MIC + '<path d="M2.5 6.5l1.5 1.5M21.5 6.5L20 8"/>',
};

const MIC_OFF = ICON.off;
const SPK = '<path d="M4 9.5v5h3.5L12 18V6L7.5 9.5H4z"/>';
const spkIcon = () => svg(SPK + (spkMuted || spkVol === 0 ? '<path d="M16 9.5l5 5M21 9.5l-5 5"/>' : spkVol < 0.5 ? '<path d="M15.5 9.5a3.5 3.5 0 0 1 0 5"/>' : '<path d="M15.5 9.5a3.5 3.5 0 0 1 0 5M18 7a7 7 0 0 1 0 10"/>'));
const spkWord = () => (spkMuted ? 'muted' : Math.round(spkVol * 100) + '%');

const svg = (inner) => `<svg viewBox="0 0 24 24" aria-hidden="true">${inner}</svg>`;
const CHECK = svg('<path d="M5 12.5l4.5 4.5L19 7.5"/>');

let headPick = null;
function paintHeader(state) {
  for (const id of ['#b-ptt', '#b-ptt2']) {
    const el = $(id);
    if (!el) continue;

    const mu = micMuted && pick !== 'off';
    const w = micWords(pick, state, micMuted);
    if (el.dataset.mode !== pick || headPick !== pick + mu + w.text) {
      el.dataset.mode = pick;
      el.innerHTML = svg(mu ? MIC_OFF : ICON[pick]) + (id === '#b-ptt' ? `<span class="ml-s">${w.short}</span><span class="ml-l">${w.text}</span>` : '');
    }
    el.classList.toggle('muted', mu);
    const talking = mode === 'push' && (state === 'listening' || state === 'noaudio');
    el.classList.toggle('talking', talking);
    if (state && state !== 'paused') el.dataset.voice = state; else delete el.dataset.voice;
    const what = pick === 'push' && id === '#b-ptt2' ? (mu ? w.text + '. M unmutes' : mode === 'push' ? 'Listening: release to send' : 'Hold to talk') : `${w.text}. Tap to change`;
    el.setAttribute('aria-label', what);
    el.title = what;
  }
  headPick = pick + (micMuted && pick !== 'off') + micWords(pick, state, micMuted).text;
}

let strip = null;
function ensureStrip() {
  if (strip) return strip;
  strip = h('div', { id: 'mic-strip', role: 'status', 'aria-live': 'polite', hidden: true },
    h('span.ms-ic', { 'aria-hidden': 'true' }),
    h('span.ms-main', {}, h('span.ms-t'), h('span.ms-sub')),
    h('span.ms-lv', {}, h('span.ms-meter', {}, h('i')), h('span.ms-db')),
    h('button.ms-act', { type: 'button', hidden: true }),
    h('button.ms-mute', { type: 'button', 'aria-pressed': 'false', html: svg(MIC_OFF) }),
    h('button.ms-spk', { type: 'button', 'aria-pressed': 'false' }, h('span.ms-spi'), h('span.ms-spv')),
    h('button.ms-chg', { type: 'button', 'aria-label': 'Change the mic mode', title: 'Change the mic mode', html: svg('<path d="M6 9l6 6 6-6"/>') }));
  strip.addEventListener('click', (e) => {
    if (e.target.closest('.ms-mute')) { e.stopPropagation(); setMicMuted(!micMuted); return; }
    if (e.target.closest('.ms-spk')) { e.stopPropagation(); setSpeaker({ muted: !spkMuted }); return; }
    const a = e.target.closest('.ms-act');
    if (a) {
      e.stopPropagation();
      if (a.dataset.act === 'unmute') {

        send({ t: 'voice', unmute_mac: true });
        const m = resumeMode || pick; lost = ''; resumeMode = ''; err = null; unmuteAt = Date.now();
        setTimeout(() => start(m), 400);
      } else if (a.dataset.act === 'stop') { hush(); send({ t: 'voice', hush: true }); } else if (a.dataset.act === 'start') { const m = resumeMode || pick; lost = ''; resumeMode = ''; err = null; start(m); }
      paint();
      return;
    }
    openMicMenu(strip);
  });

  const wide = matchMedia('(min-width:1200px)');
  const place = () => {
    const row = $('.tr2'), mic = $('#b-ptt'), gap = $('.tr2-gap');
    if (wide.matches && row && gap) { gap.after(strip); strip.after(mic); }
    else { ($('#under') || $('#app')).prepend(strip); if (row && mic && row.firstElementChild !== mic) row.prepend(mic); }
  };
  place();
  wide.addEventListener('change', place);
  return strip;
}

function paintStrip(s) {
  const el = ensureStrip();
  const on = pick === 'open' || pick === 'wake';
  if (el.hidden === on) el.hidden = !on;
  document.body.classList.toggle('hf', on);
  if (!on) return;
  const m = handsFree() ? mode : pick;
  const flags = S.mode_flags || {};
  const extra = [flags.direct ? 'to ' + flags.direct : '', flags.brainstorm ? 'brainstorm' : ''].filter(Boolean).join(' · ');
  let state = s, what = '', sub = extra, act = '';
  if (err) { state = 'error'; what = 'not listening'; sub = err.text; act = err.code === 'mac-muted' ? 'unmute' : 'start'; }
  else if (!handsFree()) { state = 'paused'; what = lost === 'tap' ? 'paused' : 'not started'; sub = lost === 'tap' ? 'The phone stopped the mic in the background' : 'Tap Start to open the mic'; act = 'start'; }
  else if (s === 'paused') { what = 'paused'; sub = away ? 'Back when you return to GodTerm' : 'The phone stopped the mic (a call, the screen lock, another app)'; }
  else if (s === 'noaudio') what = 'no audio';
  else if (micMuted) { state = 'muted'; what = micWords(m, s, true).state; sub = 'Nothing is sent; replies still play'; if (s === 'speaking') act = 'stop'; }
  else if (s === 'waiting') what = `say "${wakeLabel()}"`;
  else if (s === 'listening') what = mode === 'wake' ? 'listening for your request' : STATE.listening;
  else if (s === 'heard') { what = STATE.heard; if (heardText) sub = '"' + heardText + '"'; }
  else if (s === 'speaking') { what = STATE.speaking; act = 'stop'; }
  else what = STATE[s] || s || STATE.listening;
  el.dataset.state = state || 'idle';
  el.dataset.mode = m;
  const ic = el.querySelector('.ms-ic');
  if (ic.dataset.m !== m) { ic.dataset.m = m; ic.innerHTML = svg(ICON[m] || MIC); }
  el.querySelector('.ms-t').textContent = `${modeName(m)} \u00b7 ${what}`;
  el.querySelector('.ms-sub').textContent = sub;
  el.querySelector('.ms-lv').hidden = !mode || micMuted;
  el.dataset.muted = String(micMuted);
  const mb = el.querySelector('.ms-mute');
  mb.setAttribute('aria-pressed', String(micMuted));
  const mt = micMuted ? 'Unmute the mic (M)' : 'Mute the mic (M): replies still play';
  if (mb.title !== mt) { mb.title = mt; mb.setAttribute('aria-label', mt); }
  const sb = el.querySelector('.ms-spk');
  const sk = spkWord();
  if (sb.dataset.k !== sk) {
    sb.dataset.k = sk;
    sb.querySelector('.ms-spi').innerHTML = spkIcon();
    sb.querySelector('.ms-spv').textContent = spkMuted || spkVol >= 1 ? '' : sk;
    sb.setAttribute('aria-pressed', String(spkMuted));
    const st3 = spkMuted ? 'The agent\'s voice is muted here: tap to hear it' : `The agent's voice here: ${sk}. Tap to mute it`;
    sb.title = st3;
    sb.setAttribute('aria-label', st3);
  }
  const a = el.querySelector('.ms-act');
  a.hidden = !act;
  a.dataset.act = act;
  a.textContent = { stop: 'Stop', unmute: 'Unmute', start: err ? 'Retry' : lost === 'tap' ? 'Resume' : 'Start' }[act] || '';
}

let talk = null, talkBox = null, ctxBtn = null;
function ensureTalk() {
  if (talk) return talk;
  talk = h('button', { id: 'talk', type: 'button', hidden: true, 'aria-label': 'Hold to talk' },
    h('span.tk-ic', { html: svg(MIC) }), h('span.tk-l', {}, 'Talk'),
    h('span.tk-lv', {}, h('span.tk-meter', {}, h('i')), h('span.tk-db')));
  ptt(talk);

  ctxBtn = h('button', { id: 'ctxbtn', type: 'button', 'aria-label': 'Context: what the assistant looks at', title: 'Context: all tabs or one tab' }, h('span.cx-l'));
  ctxBtn.onclick = () => { const r = ctxBtn.getBoundingClientRect(); import('./chat.js').then((m) => m.scopeMenu({ x: r.left, y: r.top - 6, up: true })); };
  const grip = h('div', { id: 'talkgrip', role: 'separator', tabindex: 0, 'aria-label': 'Move the Talk button', title: 'Drag to move, double click to put back' }, h('i'));
  talkBox = h('div', { id: 'talkbox' }, grip, talk, ctxBtn);
  dragTalk(grip);
  const paintCtx = () => import('./chat.js').then((m) => { ctxBtn.firstChild.textContent = m.scopeLabel(); });
  on('scope', paintCtx); on('state', paintCtx); paintCtx();
  homeTalk();

  on('layout', homeTalk);
  return talk;
}

function dragTalk(grip) {
  const set = (x, y) => { talkBox.style.setProperty('--tdx', x + 'px'); talkBox.style.setProperty('--tdy', y + 'px'); };
  const off = () => ({ x: parseFloat(talkBox.style.getPropertyValue('--tdx')) || 0, y: parseFloat(talkBox.style.getPropertyValue('--tdy')) || 0 });
  const fit = (x, y) => {
    const host = talkBox.parentElement?.getBoundingClientRect(), r = talkBox.getBoundingClientRect(), o = off();
    if (!host) return { x, y };

    const l0 = r.left - o.x, t0 = r.top - o.y;
    return { x: Math.min(Math.max(x, host.left - l0), host.right - r.width - l0), y: Math.min(Math.max(y, host.top - t0), host.bottom - r.height - t0) };
  };
  const saved = store.get('talkpos', null);
  if (saved) set(saved.x, saved.y);
  let d = null;
  grip.addEventListener('pointerdown', (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    grip.setPointerCapture(e.pointerId);
    d = { px: e.clientX, py: e.clientY, ...off() };
  });
  grip.addEventListener('pointermove', (e) => { if (d) { const p = fit(d.x + e.clientX - d.px, d.y + e.clientY - d.py); set(p.x, p.y); } });
  const end = () => { if (!d) return; d = null; store.set('talkpos', off()); };
  for (const ev of ['pointerup', 'pointercancel', 'lostpointercapture']) grip.addEventListener(ev, end);
  grip.addEventListener('dblclick', () => { set(0, 0); store.set('talkpos', null); });
  grip.addEventListener('keydown', (e) => {
    const k = { ArrowLeft: [-16, 0], ArrowRight: [16, 0], ArrowUp: [0, -16], ArrowDown: [0, 16] }[e.key];
    if (!k) return;
    e.preventDefault();
    const o = off(), p = fit(o.x + k[0], o.y + k[1]);
    set(p.x, p.y);
    store.set('talkpos', p);
  });

  addEventListener('resize', () => { const o = off(), p = fit(o.x, o.y); set(p.x, p.y); });
}
function homeTalk() {
  if (!talk) return;
  const main = $('#main');
  const covered = $('#app')?.classList.contains('chat-open') && main && getComputedStyle(main).visibility === 'hidden';
  const host = covered ? $('#chat') : ($('#panes') || main);
  if (host && talkBox.parentElement !== host) host.append(talkBox);
}

function paintTalk(s) {
  const t = ensureTalk();
  const on = pick === 'push';
  if (t.hidden === on) t.hidden = !on;
  talkBox.hidden = !on;
  document.body.classList.toggle('ptt', on);
  if (!on) return;
  const st2 = err ? 'error' : mode === 'push' ? s || 'listening' : s === 'sending' || s === 'thinking' || s === 'speaking' ? s : 'idle';
  t.dataset.state = st2;
  const label = { idle: 'Talk', starting: upFirst(STATE.starting), asking: upFirst(STATE.asking), listening: 'Release to send', noaudio: upFirst(STATE.noaudio), sending: upFirst(STATE.sending), thinking: 'Talk', speaking: 'Talk', error: 'Talk' }[st2] || 'Talk';
  const l = t.querySelector('.tk-l');
  if (l.textContent !== label) l.textContent = label;
  t.querySelector('.tk-lv').hidden = mode !== 'push';
  t.setAttribute('aria-pressed', String(mode === 'push'));
}

let menuAnchor = null, menuSkip = false;
function openMicMenu(anchor) {
  if (menuSkip) { menuSkip = false; return; }
  menuAnchor = anchor;
  const r = anchor.getBoundingClientRect();
  const w = Math.min(300, innerWidth - 16);
  const at = { x: Math.max(8, r.right - w), y: r.bottom + 6 };
  const item = (m, label, sub) => h('button.mm-i', { type: 'button', role: 'menuitemradio', 'aria-checked': String(pick === m), 'data-m': m,
    onclick: () => { closeSheet(); setMode(m); } },
  h('span.mm-ic', { html: svg(ICON[m]) }), h('span.mm-t', {}, label, h('small', {}, sub)), h('span.mm-ck', { html: pick === m ? CHECK : '' }));
  const flags = S.mode_flags || {};
  const t = curTab();
  const direct = !!flags.direct;
  const dRow = h('button.mm-i.mm-tog', { type: 'button', role: 'menuitemcheckbox', 'aria-checked': String(direct), disabled: !direct && !t,
    onclick: () => { closeSheet(); if (direct) send({ t: 'voice', direct: false }); else if (t) send({ t: 'voice', direct: t.uid }); } },
  h('span.mm-ic', { html: svg('<path d="M4 12h12M12 6l6 6-6 6"/>') }),
  h('span.mm-t', {}, 'Direct to this tab', h('small', {}, direct ? `Talking to ${flags.direct}` : t ? `Talk to ${t.name} itself` : 'Open a tab first')),
  h('span.mm-sw', { 'aria-hidden': 'true' }, h('i')));
  const muteRow = h('button.mm-i.mm-tog.mm-mute', { type: 'button', role: 'menuitemcheckbox', 'aria-checked': String(micMuted), disabled: pick === 'off',
    onclick: () => { closeSheet(); setMicMuted(!micMuted); } },
  h('span.mm-ic', { html: svg(MIC_OFF) }),
  h('span.mm-t', {}, 'Mute mic', h('small', {}, pick === 'off' ? 'The mic is off' : micMuted ? 'Nothing is sent; replies still play (M)' : 'Keep listening to replies (M)')),
  h('span.mm-sw', { 'aria-hidden': 'true' }, h('i')));
  openMenu(at, h('div.mm', { role: 'none' },
    item('off', MODE.off, 'The mic stays closed'),
    item('push', 'Push to talk', 'Hold Talk while you talk'),
    item('open', 'Open mic', 'Always listening for commands'),
    item('wake', 'Wake word', `Say "${wakeLabel()}", then your request`),
    h('div.mm-sep'),
    muteRow,
    volRow('mm-vol'),
    h('div.mm-sep'),
    dRow,
    h('button.mm-i', { type: 'button', onclick: () => { closeSheet(); openVoiceSheet(); } },
      h('span.mm-ic', { html: SET_ICON }), h('span.mm-t', {}, 'Voice settings', h('small', {}, 'Voice, speed, wake word, mic test')), h('span.mm-ck'))));
  const pop = document.getElementById('pop');
  if (pop) { pop.classList.add('mic-pop'); pop.style.width = w + 'px'; }
}

function paint() {
  const s = shown();
  const state = err ? 'error' : s;
  paintHeader(state);
  const om = $('#b-openmic');
  if (om) {
    om.setAttribute('aria-pressed', String(handsFree()));

    om.textContent = micWords(pick, state, micMuted).text;
  }
  paintStrip(s);
  paintTalk(s);
  const here = serverOn && S.voice?.web === S.client;
  const engineErr = here && S.voice?.status === 'off' && S.voice?.error;
  const note = $('#voice-note');
  if (note) note.textContent = err ? 'voice failed' : !secure() ? 'mic needs HTTPS' : serverOn && !s ? 'voice here' : '';
  const b = ensureBar();
  let text = '', sub = '', act = '';
  const flags = S.mode_flags || {};
  const extra = [flags.direct ? 'talking to ' + flags.direct : '', flags.brainstorm ? 'brainstorm' : ''].filter(Boolean).join(' · ');
  if (err) text = err.text;
  else if (engineErr) text = 'GodTerm\'s voice engine stopped: ' + S.voice.error;
  else if (s === 'paused') {
    text = TEXT.paused;
    sub = lost === 'tap' ? 'The phone stopped the mic in the background. Tap Resume.' : away ? 'GodTerm is in the background; the mic comes back when you return' : 'The phone stopped the mic (a call, the screen lock, another app)';
    if (lost === 'tap') act = 'resume';
  } else if (s === 'listening') text = 'Listening: release to send';
  else if (s === 'noaudio') { text = TEXT.noaudio; sub = 'Nothing reaches GodTerm: release and hold again'; }
  else if (s === 'locked') { text = TEXT.locked; act = 'play'; }
  else if (s === 'speaking') { text = TEXT.speaking; act = 'stop'; sub = extra; }
  else if (s) { text = TEXT[s] || upFirst(s); sub = extra; }
  else if (hint) text = hint;

  const inStrip = (pick === 'open' || pick === 'wake') && !engineErr && s !== 'locked';

  const inTalk = pick === 'push' && !err && !engineErr && talk && !talk.hidden && ['listening', 'sending', 'thinking', 'starting'].includes(s);
  b.hidden = !text || inStrip || inTalk || sheetKind === 'voice';
  b.dataset.state = err || engineErr ? 'error' : s || 'idle';
  delete b.dataset.hf;
  b.querySelector('.vb-text').textContent = text;
  b.querySelector('.vb-sub').textContent = sub;
  const a = b.querySelector('.vb-act');
  a.hidden = !act;
  a.dataset.act = act;
  a.textContent = { stop: 'Stop', play: 'Play', resume: 'Resume' }[act] || '';
  b.querySelector('.vb-lv').hidden = !mode;
  dbAt = 0;
  meter();
  if (sheetKind === 'voice') paintSheet();
}

function ptt(btn, menuElse = false) {
  if (!btn) return;
  let tap = false;
  btn.addEventListener('pointerdown', async (e) => {
    e.preventDefault();
    tap = false;
    if (menuElse && pick !== 'push') { tap = true; return; }
    if (handsFree()) {

      if (playing.size || S.voice?.speaking) { hush(); send({ t: 'voice', hush: true }); }
      return;
    }
    if (starting) return;
    if (micMuted) { hint = 'The mic is muted: M or the mic menu unmutes'; paint(); return; }
    pressed = true;
    btn.setPointerCapture?.(e.pointerId);
    const asked = performance.now();
    if (mode === 'test') stop();
    starting = start('push');
    const ok = await starting;
    starting = null;
    if (!ok) return;
    if (!pressed) {

      if (cap?.method === 'recorder') endRec();
      mode = '';
      phase = '';
      send({ t: 'voice', talk: false });
      voiceOffSoon();
      if (performance.now() - asked > 600) hint = 'The mic is ready: hold Talk while you talk';
      paint();
    }
  });
  const up = (e) => {
    if (tap) { tap = false; if (e?.type === 'pointerup') openMicMenu(btn); return; }
    if (!pressed) return;
    pressed = false;
    if (!starting && mode === 'push') stop();
  };
  btn.addEventListener('pointerup', up);
  btn.addEventListener('pointercancel', up);
  btn.addEventListener('lostpointercapture', up);
  btn.addEventListener('contextmenu', (e) => e.preventDefault());

  btn.addEventListener('touchstart', (e) => e.preventDefault(), { passive: false });
}

export const voicePick = () => pick;
export { openVoiceSheet, openMicMenu };
export function setMode(m) {
  if (!PICKS.includes(m)) return;
  pick = m;
  store.set('voice.pick', m);
  lost = '';
  resumeMode = '';
  hint = '';
  if (m === 'off' || m === 'push') {
    if (mode) stop();
    if (m === 'off') { phase = ''; thinkingUntil = 0; if (serverOn) voiceOff(); }
    paint();
    return;
  }
  hfMode = m;
  store.set('voice.hf', m);
  if (handsFree() && mode === m) { paint(); return; }
  if (mode) stop();
  err = null;
  start(m);
}

function muteTrack() {
  for (const t of stream?.getAudioTracks() || []) t.enabled = !micMuted;
}
export const micIsMuted = () => micMuted;
export function setMicMuted(on) {
  on = !!on;
  if (on === micMuted) return;
  micMuted = on;
  store.set('voice.micmute', on);
  muteTrack();
  if (on) { level = 0; levelDb = -90; meter(); }
  if (serverOn) send({ t: 'voice', muted: on });
  dlog(on ? 'mic muted' : 'mic unmuted');
  if (on) hint = '';
  paint();
}

let duck = 1;
const spkGain = () => (spkMuted ? 0 : spkVol * duck);
function applyGain() {
  if (!outGain || !ctx) return;
  try { outGain.gain.setTargetAtTime(spkGain(), ctx.currentTime, 0.015); } catch { outGain.gain.value = spkGain(); }
}
function onDuck(m) {
  const l = Number(m?.level);
  const v = Number.isFinite(l) ? Math.max(0, Math.min(1, l)) : 1;
  if (v === duck) return;
  duck = v;
  if (playing.size) dlog(v < 1 ? `reply ducked to ${Math.round(v * 100)}% (is it a barge-in?)` : 'reply back to full volume');
  applyGain();
}
export function setSpeaker({ volume, muted } = {}) {
  if (volume != null && Number.isFinite(+volume)) {
    spkVol = Math.max(0, Math.min(1, +volume));
    store.set('voice.spkvol', spkVol);

    if (spkMuted && spkVol > 0 && muted == null) { spkMuted = false; store.set('voice.spkmute', false); }
  }
  if (muted != null) { spkMuted = !!muted; store.set('voice.spkmute', spkMuted); }
  applyGain();
  for (const r of document.querySelectorAll('.vol-row')) paintVol(r);
  paint();
}

function volRow(cls) {
  const b = h('button.vr-b', { type: 'button', onclick: (e) => { e.stopPropagation(); setSpeaker({ muted: !spkMuted }); } });
  const r = h('input.vr-r', { type: 'range', min: 0, max: 100, step: 5, 'aria-label': 'The agent\'s voice volume here',
    oninput: (e) => setSpeaker({ volume: +e.target.value / 100 }) });
  const row = h('div.vol-row.' + cls, {}, b, h('span.vr-t', {}, 'Agent voice', h('small.vr-s')), r, h('span.vr-v'));
  paintVol(row);
  return row;
}
function paintVol(row) {
  const b = row.querySelector('.vr-b');
  b.innerHTML = spkIcon();
  b.setAttribute('aria-pressed', String(spkMuted));
  b.setAttribute('aria-label', spkMuted ? 'Unmute the agent\'s voice here' : 'Mute the agent\'s voice here');
  b.title = b.getAttribute('aria-label');
  const r = row.querySelector('.vr-r');
  const v = String(Math.round(spkVol * 100));
  if (r.value !== v && document.activeElement !== r) r.value = v;
  r.classList.toggle('off', spkMuted);
  row.querySelector('.vr-v').textContent = spkMuted ? 'muted' : Math.round(spkVol * 100) + '%';
  row.querySelector('.vr-s').textContent = spkMuted ? 'Muted in this browser: replies show as text' : 'In this browser';
}

let chain = Promise.resolve(), gen = 0;
const reply = { clips: 0, secs: 0, late: 0, t0: 0, quietT: 0 };
function enqueue(bytes) {
  const g = gen;
  chain = chain.then(() => (g === gen ? play(bytes, g) : null)).catch((e) => dlog('play failed: ' + (e?.message || e)));
}

function wavBuffer(c, bytes) {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (bytes.byteLength < 44 || dv.getUint32(0) !== 0x52494646) return null;
  let p = 12, rate = 0, chans = 1, bits = 16;
  while (p + 8 <= bytes.byteLength) {
    const id = dv.getUint32(p), len = dv.getUint32(p + 4, true);
    if (id === 0x666d7420) { chans = dv.getUint16(p + 10, true); rate = dv.getUint32(p + 12, true); bits = dv.getUint16(p + 22, true); }
    if (id === 0x64617461) {
      if (bits !== 16 || !rate) return null;
      const n = Math.floor(Math.min(len, bytes.byteLength - p - 8) / 2 / chans);
      if (!n) return 'empty';
      const buf = c.createBuffer(1, n, rate);
      const ch = buf.getChannelData(0);
      for (let i = 0; i < n; i++) ch[i] = dv.getInt16(p + 8 + i * 2 * chans, true) / 32768;
      return buf;
    }
    p += 8 + len + (len & 1);
  }
  return null;
}

let hotClips = 0;
function smooth(buf) {
  const ch = buf.getChannelData(0), n = ch.length;
  let peak = 0;
  for (let i = 0; i < n; i++) { const a = ch[i] < 0 ? -ch[i] : ch[i]; if (a > peak) peak = a; }
  if (peak > 0.97) {
    const k = 0.94 / peak;
    for (let i = 0; i < n; i++) ch[i] *= k;
    if (++hotClips <= 3) dlog(`clip peak ${peak.toFixed(2)}: scaled by ${k.toFixed(2)} to avoid clipping`);
  }
  const f = Math.min(n >> 1, Math.round(buf.sampleRate * 0.004));
  for (let i = 0; i < f; i++) { const w = i / f; ch[i] *= w; ch[n - 1 - i] *= w; }
}

async function play(bytes, g = gen) {
  const c = audioCtx();
  if (!c) return;
  if (c.state !== 'running') {
    await wait(c.resume().catch(() => {}), 300);
    if (g !== gen) return;
    if (c.state !== 'running') {

      held.push(bytes);
      if (held.length === 1) dlog('reply held: audio context ' + c.state);
      paint();
      return;
    }
  }
  let buf = wavBuffer(c, bytes);
  if (buf === 'empty') return;
  if (!buf) {
    try {
      buf = await new Promise((res, rej) => {
        const p = c.decodeAudioData(bytes.slice().buffer, res, rej);
        if (p?.then) p.then(res, rej);
      });
    } catch {
      const rate = S.voice?.sample_rate || 24000;
      const n = bytes.byteLength >> 1;
      if (!n) return;
      const dv = new DataView(bytes.buffer, bytes.byteOffset, n * 2);
      buf = c.createBuffer(1, n, rate);
      const ch = buf.getChannelData(0);
      for (let i = 0; i < n; i++) ch[i] = dv.getInt16(i * 2, true) / 32768;
    }
  }
  if (!buf.duration || g !== gen) return;
  smooth(buf);
  const s = c.createBufferSource();
  s.buffer = buf;
  s.connect(outGain || c.destination);
  const now = c.currentTime;
  if (!playing.size || playAt < now) {

    if (playing.size || reply.clips) reply.late++;
    playAt = now + 0.03;
  }
  s.start(playAt);
  playAt += buf.duration;
  playing.add(s);
  sendPlaying(true);
  if (!reply.clips) reply.t0 = performance.now();
  reply.clips++;
  reply.secs += buf.duration;
  clearTimeout(reply.quietT);
  s.onended = () => {
    playing.delete(s);
    if (!playing.size) {
      reply.quietT = setTimeout(replyDone, 700);

      clearTimeout(playOffT);
      playOffT = setTimeout(() => { if (!playing.size) sendPlaying(false); }, 250);
    }
    paint();
  };
  paint();
  if (serverOn === 'push' && !mode) voiceOffSoon();
}

let playSent = false, playOffT = 0;
function sendPlaying(on) {
  clearTimeout(playOffT);
  if (on === playSent) return;
  if (on && !serverOn) return;
  playSent = on;
  send({ t: 'voice', playing: on });
}

function replyDone() {
  if (!playing.size && duck !== 1) { duck = 1; applyGain(); }
  if (playing.size || !reply.clips) return;
  dlog(`reply played: ${reply.clips} clips, ${reply.secs.toFixed(1)} s${reply.late > 1 ? `, ${reply.late - 1} late (a gap)` : ''}`);
  Object.assign(reply, { clips: 0, secs: 0, late: 0 });
}

function hush() {
  gen++;
  for (const s of playing) { try { s.onended = null; s.stop(); } catch {} }
  if (playing.size) dlog(`reply stopped after ${reply.clips} clips`);
  playing.clear();
  held = [];
  playAt = 0;
  if (duck !== 1) { duck = 1; applyGain(); }
  sendPlaying(false);
  Object.assign(reply, { clips: 0, secs: 0, late: 0 });
  paint();
}

function onVoice(m) {
  if (m.web_tts_list !== undefined) { ttsList = m; paintTts(); return; }
  if (m.web_tts !== undefined) {
    if (m.ok === false) toast('Voice for this browser not set: ' + (m.error || 'refused'), 'warn');
    else if (ttsToast) toast('Replies here: ' + ttsLabel(m.web_tts), 'info');
    ttsToast = false;
    paintTts();
    return;
  }
  if (m.set) {
    const label = settings?.find((x) => x.key === m.set)?.label || m.set;
    toast(m.ok ? `${label}: ${m.value}` : `${label} not saved: ${m.error}`, m.ok ? 'info' : 'warn');
    return;
  }
  if (m.ok === false) {
    const was = mode;
    mode = '';
    release();
    phase = '';
    serverOn = '';

    if (m.code === 'muted' && Date.now() - unmuteAt < 5000) fail('mac-muted', 'Still muted: press Ctrl-a X on the Mac (unmuting from here needs a GodTerm restart)');
    else if (m.code === 'muted' && was) {

      send({ t: 'voice', unmute_mac: true });
      unmuteAt = Date.now();
      setTimeout(() => start(was), 400);
      return;
    } else if (m.code === 'muted') fail('mac-muted', 'GodTerm\'s mic is muted on the Mac');
    else fail('server', 'GodTerm refused voice: ' + (m.error || 'no reason given'));
    if (was) dlog('refused while ' + was);
    return;
  }
  if (m.moved) {
    mode = '';
    release();
    phase = '';
    serverOn = '';
    hint = 'Voice moved to another browser';
    paint();
    return;
  }
  if (m.on === true) serverOn = m.mode || serverOn || 'push';
  if (m.on === false) serverOn = '';
  paint();
}

const KEYS = ['voice.tts', 'voice.tts_engine', 'voice.kokoro_voice', 'voice.grok.voice', 'voice.tts_voice', 'voice.tts_speed', 'voice.tts_volume',
  'voice.wake_words', 'voice.wake_sensitivity', 'voice.wake_follow_up_s', 'voice.open_mic_sleep_min', 'voice.barge_in', 'voice.barge_margin_db',
  'voice.barge_in_min_words', 'voice.barge_in_duck', 'voice.direct_reply', 'voice.instant_commands', 'voice.end_silence_ms', 'voice.engine', 'voice.language', 'voice.speaker_lock'];
let settings = null;

function curTab() {
  const p = panes.focused?.();
  const uid = p?.id ? +String(p.id).replace(/^t/, '') : S.focused;
  const t = uid != null ? S.tabs.find((x) => x.uid === uid) : null;
  return t ? { uid, name: t.label || t.name || t.title || 't' + uid } : null;
}

function liveInfo() {
  const el = document.querySelector('.vs .vs-live > span:last-child');
  if (!el) return;
  if (!cap) { el.textContent = 'Mic closed'; return; }
  const lv = levelDb > -89 ? `${levelDb.toFixed(0)} dBFS` : 'silent';
  el.textContent = cap.method === 'recorder'
    ? `recorder, ${recPieces} pieces sent${recLevelAt ? `, GodTerm hears ${lv} (${(recMs / 1000).toFixed(1)} s)` : ''}`
    : `${cap.method}, ${fps} frames/s, ${lv}`;
}

function segBtn(label, on, fn) {
  return h('button', { type: 'button', 'aria-pressed': String(!!on), onclick: fn }, label);
}

function paintSheet() {
  const root = document.querySelector('#sheet-body .vs');
  if (!root) return;
  const flags = S.mode_flags || {};
  const t = curTab();
  const sig = JSON.stringify([micMuted, cap?.method, store.get('voice.method', ''), store.get('voice.force', ''), pick, flags.direct, flags.brainstorm, t, mode === 'test', wakeWord()]);
  if (root.dataset.sig === sig) { liveInfo(); meter(); return; }
  root.dataset.sig = sig;
  root.querySelector('.seg-mode').replaceChildren(
    segBtn(MODE.off, pick === 'off', () => setMode('off')),
    segBtn(MODE.push, pick === 'push', () => setMode('push')),
    segBtn(MODE.open, pick === 'open', () => setMode('open')),
    segBtn(MODE.wake, pick === 'wake', () => setMode('wake')));
  root.querySelector('.mode-note').textContent = {
    off: 'The mic stays closed. Pick a mode here or from the mic in the header.',
    push: 'Hold the Talk button while you talk.',
    open: 'Everything you say is a command, no wake word. Say "sleep" or turn it off here.',
    wake: `Listening for "${wakeWord()}"; say it, then your request.`,
  }[pick];
  const d = root.querySelector('.direct');
  d.replaceChildren(
    h('span', {}, flags.direct ? `Talking to ${flags.direct} directly` : 'Direct mode', h('small', {}, flags.direct ? 'What you say goes to that tab; its replies are spoken.' : t ? `Talk to ${t.name} without the assistant in between.` : 'Open a tab to talk to it directly.')),
    flags.direct
      ? h('button.btn', { type: 'button', onclick: () => send({ t: 'voice', direct: false }) }, 'Back to the assistant')
      : h('button.btn', { type: 'button', disabled: !t, onclick: () => t && send({ t: 'voice', direct: t.uid }) }, 'Talk to this tab'));
  const bs = root.querySelector('.brain');
  bs.replaceChildren(
    h('span', {}, 'Brainstorm', h('small', {}, 'Talk as long as you like; nothing is sent until you say "send it".')),
    h('button.btn', { type: 'button', onclick: () => send({ t: 'voice', brainstorm: !flags.brainstorm }) }, flags.brainstorm ? 'End' : 'Start'));
  const mm = root.querySelector('.mmute');
  mm.replaceChildren(
    h('span', {}, 'Mute mic', h('small', {}, micMuted ? 'Nothing is sent to GodTerm; replies still play. M unmutes.' : 'Stop sending the mic and keep the mode; replies still play. Shortcut: M.')),
    h('button.btn', { type: 'button', 'aria-pressed': String(micMuted), onclick: () => setMicMuted(!micMuted) }, micMuted ? 'Unmute' : 'Mute'));
  const mt = root.querySelector('.mtest');
  mt.textContent = mode === 'test' ? 'Stop the test' : 'Test the mic';
  const f = store.get('voice.force', '');
  const sel = root.querySelector('.method');
  if (sel.value !== (f || 'auto')) sel.value = f || 'auto';
  root.querySelector('.method-note').textContent = `In use: ${cap ? cap.method : 'none'}. Worked here last: ${store.get('voice.method', '') || 'not yet'}. Recording type: ${recorderType() || 'none'}.`;
  liveInfo();
  meter();
}

function settingRow(s) {
  const k = s.kind;

  const save = (value) => { if (!send({ t: 'voice', set: { key: s.key, value } })) toast('Not connected: not saved'); };
  let input;
  const v = String(s.value ?? '');
  if (k === 'on/off') {
    const onv = /^(on|true|yes)$/i.test(v);
    input = h('input', { type: 'checkbox', checked: onv, onchange: (e) => save(e.target.checked) });
  } else if (k && typeof k === 'object' && k['one of']) {
    const opts = k['one of'].map((o) => (Array.isArray(o) ? o : [o, o]));
    input = h('select', { onchange: (e) => save(e.target.value) }, opts.map(([o, l]) => h('option', { value: o, selected: String(o) === v }, String(l))));
    opts.splice(0, opts.length, ...opts.map((o) => o[0]));
    if (!opts.map(String).includes(v)) input.prepend(h('option', { value: v, selected: true, disabled: true }, v));
  } else if (k && typeof k === 'object' && (k.integer || k.number)) {
    const [min, max] = k.integer || k.number;
    input = h('input', { type: 'number', min, max, step: k.integer ? 1 : 0.05, value: v, onchange: (e) => save(+e.target.value) });
  } else {
    input = h('input', { type: 'text', value: v, onchange: (e) => save(e.target.value) });
  }
  return h('label.row', {}, h('span', {}, s.label, h('small', {}, s.help ? String(s.help).split('. ')[0] : '')), input);
}

const TTS_DEFAULT = { engine: 'kokoro', voice: 'af_heart' };
const ttsChoice = () => { const c = store.get('voice.webtts', null); return c && typeof c === 'object' && c.engine ? c : null; };
let ttsList = null, ttsToast = false;
function ttsLabel(c) {
  if (!c || c.engine === 'mac') return 'the Mac\'s voice';
  const e = ttsList?.web_tts_list?.find((x) => x.engine === c.engine);
  const v = e?.voices?.find((x) => x.id === c.voice);
  return [e?.label || c.engine, v?.label || c.voice].filter(Boolean).join(' · ');
}
function sendTts() {
  const c = ttsChoice();
  if (c) send({ t: 'voice', web_tts: c.engine === 'mac' ? null : c });
}
function setTts(c) {
  store.set('voice.webtts', c);
  ttsToast = true;
  sendTts();
}

export function ttsBox() {
  const box = h('div.tts-box');
  if (!ttsList) send({ t: 'voice', web_tts_list: true });
  setTimeout(paintTts, 0);
  return box;
}
function paintTts() {
  for (const box of document.querySelectorAll('.tts-box')) paintTtsBox(box);
}
function paintTtsBox(box) {
  const L = ttsList?.web_tts_list;
  if (!L) {

    if (!box.waited) { box.waited = setTimeout(() => { if (!ttsList && box.isConnected) box.replaceChildren(failRow('GodTerm did not list the voices', () => { send({ t: 'voice', web_tts_list: true }); box.waited = 0; paintTtsBox(box); }, 'The voice list')); }, 6000); }
    box.replaceChildren(skeleton(1, '.sm'));
    return;
  }
  clearTimeout(box.waited); box.waited = 0;
  const def = ttsList.default || TTS_DEFAULT;
  const cur = ttsChoice() || def;
  const engines = h('select.tts-e', { 'aria-label': 'Engine', onchange: (e) => {
    const id = e.target.value;
    if (id === 'mac') { setTts({ engine: 'mac', voice: '' }); paintTts(); return; }
    const en = L.find((x) => x.engine === id);
    const keep = en?.voices?.some((v) => v.id === cur.voice) ? cur.voice : id === def.engine ? def.voice : en?.voices?.[0]?.id || '';
    setTts({ engine: id, voice: keep });
    paintTts();
  } },
  h('option', { value: 'mac', selected: cur.engine === 'mac' }, 'Same as the Mac'),
  L.map((x) => h('option', { value: x.engine, selected: x.engine === cur.engine, disabled: x.available === false && x.engine !== cur.engine }, x.label + (x.available === false ? ' (not here)' : ''))));
  const en = L.find((x) => x.engine === cur.engine);
  const voices = en?.voices?.length
    ? h('select.tts-v', { 'aria-label': 'Voice', onchange: (e) => { setTts({ engine: cur.engine, voice: e.target.value }); paintTts(); } },
      en.voices.map((v) => h('option', { value: v.id, selected: v.id === cur.voice }, v.label || v.id)))
    : null;
  const mac = ttsList.mac ? `The Mac speaks with ${ttsLabel(ttsList.mac)}.` : '';
  box.replaceChildren(
    h('label.row', {}, h('span', {}, 'Engine', h('small', {}, en?.note || (cur.engine === 'mac' ? 'Replies here use the Mac\'s voice setting' : 'Replies spoken in this browser'))), engines),
    voices ? h('label.row', {}, h('span', {}, 'Voice', h('small', {}, cur.engine === def.engine && cur.voice === def.voice ? 'The default for browsers' : '')), voices) : null,
    h('p.note', {}, mac + ' Changing this does not change the Mac.'));
}

async function loadSettings(box) {
  const r = await tool('list_settings', { section: 'Voice' });
  const rows = (r.result?.settings || r.settings || []).filter((s) => KEYS.includes(s.key));
  rows.sort((a, b) => KEYS.indexOf(a.key) - KEYS.indexOf(b.key));
  settings = rows;
  box.replaceChildren(...(rows.length ? rows.map(settingRow) : [h('p.note', {}, r.error ? 'Settings did not load: ' + r.error : 'No voice settings here.')]));
}

function openVoiceSheet() {
  const box = h('div', {}, h('p.note', {}, 'Loading the settings...'));
  const sel = h('select.method', { onchange: (e) => {
    const v = e.target.value;
    store.set('voice.force', v === 'auto' ? '' : v);
    if (v === 'auto') store.set('voice.method', '');
    if (cap && v !== 'auto' && cap.method !== v) {

      const m = mode;
      stop();
      if (m && m !== 'push') start(m);
    }
    paint();
  } }, ['auto', ...METHODS].map((m) => h('option', { value: m }, m === 'auto' ? 'Automatic' : { worklet: 'AudioWorklet', script: 'Script processor', recorder: 'Recorder (GodTerm decodes)' }[m])));
  openSheet({ kind: 'voice', tall: true },
    h('div.vs', {},
      h('h3', {}, 'Voice'),
      h('div.seg.seg-mode'),
      h('p.note.mode-note'),
      h('h4', {}, 'Hands free'),
      h('div.row.direct'),
      h('div.row.brain'),
      h('h4', {}, 'Microphone'),
      h('div.vs-live', {}, h('span.m', {}, h('i')), h('span')),
      h('div.row', {}, h('span', {}, 'How the mic is read', h('small.method-note')), sel),
      h('button.btn.mtest', { type: 'button', onclick: () => (mode === 'test' ? stop() : mode ? null : start('test')) }),
      h('div.row.mmute'),
      h('h4', {}, 'Voice in this browser'),
      volRow('vs-vol'),
      ttsBox(),
      h('h4', {}, 'GodTerm settings (the Mac)'),
      box));
  paintSheet();
  send({ t: 'voice', web_tts_list: true });
  loadSettings(box);
}

export function initVoice() {

  const hm = $('#b-ptt');
  if (hm) {
    addEventListener('pointerdown', (e) => { if (menuOpen() && menuAnchor && menuAnchor.contains(e.target)) menuSkip = true; }, true);
    hm.addEventListener('click', () => openMicMenu(hm));
  }

  ptt($('#b-ptt2'), true);
  const om = $('#b-openmic');
  if (om) om.addEventListener('click', () => openMicMenu(om));
  $('#b-voice')?.addEventListener('click', openVoiceSheet);

  addEventListener('keydown', (e) => {
    if (e.key !== 'm' && e.key !== 'M') return;
    if (e.ctrlKey || e.metaKey || e.altKey || e.repeat || e.isComposing) return;
    const el = document.activeElement;
    if (el && (el.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(el.tagName) || el.closest?.('.xterm, .xt'))) return;
    if (pick === 'off') return;
    e.preventDefault();
    setMicMuted(!micMuted);
    toast(micMuted ? 'Mic muted: replies still play (M unmutes)' : 'Mic on', 'info');
  });
  on('bin:' + KIND.AUDIO, (_uid, b) => enqueue(b));
  on('msg:audio_stop', hush);
  on('msg:audio_duck', onDuck);
  on('msg:voice', onVoice);
  on('msg:voice_level', onRecLevel);
  on('open-voice', openVoiceSheet);
  on('sheet', () => paint());
  const onConn = (s) => {
    if (s === 'live') sendTts();
    if (s === 'live' && debug) {
      send({ t: 'voice', debug: true });

      const n = voiceTest;
      voiceTest = 0;
      if (n > 0 && !mode) setTimeout(async () => { if (await start('push')) setTimeout(stop, n * 1000); }, 1500);
    }
    if (s === 'live' && handsFree() && !serverOn) {

      const m = mode;
      voiceOn(m);
    }
    if (s !== 'live') {

      if (!handsFree()) { mode = ''; release(); phase = ''; }
      serverOn = '';
      hush();
    }
    paint();
  };
  on('conn', onConn);

  if (conn.open) onConn('live');

  on('state', () => {
    const v = S.voice || {};
    if (serverOn && v.web != null && v.web !== S.client) { serverOn = ''; mode = ''; release(); phase = ''; }
    if (v.heard && v.heard !== lastHeard) {
      if (lastHeard !== null) { heardText = v.heard; heardUntil = performance.now() + 2500; }
      lastHeard = v.heard;
    }
    paint();
  });

  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      if (mode || serverOn) { away = true; if (serverOn) send({ t: 'voice', away: true }); dlog('to the background'); paint(); }
    } else backAgain();
  });
  addEventListener('pageshow', () => { if (!document.hidden) backAgain(); });
  if (!secure()) for (const b of ['#b-openmic', '#b-ptt', '#b-ptt2']) $(b)?.classList.add('off');

  if ((pick === 'open' || pick === 'wake') && !mode) resumeMode = pick;

  addEventListener('pointerdown', () => { if (!ctx || ctx.state !== 'running' || held.length) unlock(); }, { capture: true });

  setInterval(() => {
    if (!mode) return;
    const quiet = cap?.method === 'recorder' ? performance.now() - recLevelAt > 400 : performance.now() - peakAt > 300;
    if (quiet && level > 0) { setLevel(-90); }
    if (handsFree()) paint();
  }, 250);
  paint();
}
