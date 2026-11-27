import { $, h, on, emit, store, toast, skeleton, failRow, isDesk, DESK, uidOf, svgOf as svg } from './util.js';
import { I } from './icons.js';
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

const canPickInput = () => secure() && !!navigator.mediaDevices?.enumerateDevices;
const inputName = (l) => String(l || '').replace(/^Default - /, '').replace(/\s*\([0-9a-f]{4}:[0-9a-f]{4}\)$/i, '').trim();
const saved0 = store.get('voice.input', null);
let micDev = saved0 && typeof saved0 === 'object' && typeof saved0.id === 'string' && saved0.id ? { id: saved0.id, label: String(saved0.label || '') } : null;
let inputs = [], inputsKnown = false, inputFell = '';

const inputNow = () => (micDev ? inputs.find((d) => d.id === micDev.id) || (micDev.label ? inputs.find((d) => d.label === micDev.label) : null) || null : null);

async function refreshInputs() {
  if (!canPickInput()) return;
  let list;
  try { list = await navigator.mediaDevices.enumerateDevices(); } catch (e) { dlog('mic list failed: ' + (e?.message || e)); return; }
  const mics = list.filter((d) => d.kind === 'audioinput' && d.label);
  inputsKnown = mics.length > 0;
  inputs = mics.filter((d) => d.deviceId && d.deviceId !== 'default' && d.deviceId !== 'communications').map((d) => ({ id: d.deviceId, label: inputName(d.label) }));
  const d = inputNow();
  if (d && (d.id !== micDev.id || (d.label && d.label !== micDev.label))) { micDev = { id: d.id, label: d.label }; store.set('voice.input', micDev); }
  paintPickers();
}

async function inputChanged() {
  const was = inputFell;
  await refreshInputs();
  const tr = stream?.getAudioTracks()[0];
  const back = was === 'gone' && !!inputNow();
  if (back && !mode) { inputFell = ''; if (stream) release(); paintPickers(); return; }
  if (handsFree() && (back || (tr && tr.readyState === 'ended'))) switchMic();
}

function setInput(id) {
  const d = inputs.find((x) => x.id === id);
  if (id && !d && micDev?.id !== id) return;
  micDev = id ? { id, label: d?.label || micDev?.label || '' } : null;
  store.set('voice.input', micDev);
  inputFell = '';
  dlog('mic input: ' + (micDev ? micDev.label || 'a chosen device' : 'system default'));
  if (mode) switchMic();
  else if (stream) release();
  paintPickers();
}

async function getMic() {
  const want = { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true };
  let s = null, fell = '';
  if (micDev) {
    if (canPickInput()) await refreshInputs();
    const d = inputNow();
    if (inputsKnown && !d) fell = 'gone';
    else {
      try {
        s = await navigator.mediaDevices.getUserMedia({ audio: { ...want, deviceId: { exact: d ? d.id : micDev.id } } });
      } catch (e) {
        if (e?.name === 'NotAllowedError' || e?.name === 'SecurityError') throw e;
        fell = e?.name === 'OverconstrainedError' || e?.name === 'NotFoundError' ? 'gone' : 'failed';
        dlog(`chosen mic did not open (${e?.name || e}): using the system default`);
      }
    }
  }
  if (!s) {
    try {
      s = await navigator.mediaDevices.getUserMedia({ audio: want });
    } catch (e) {
      if (e?.name !== 'OverconstrainedError' && e?.name !== 'TypeError') throw e;
      s = await navigator.mediaDevices.getUserMedia({ audio: true });
    }
  }
  inputFell = fell;
  refreshInputs();
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
  return METHODS.includes(last) && last !== 'recorder' ? [last, ...METHODS.filter((m) => m !== last)] : [...METHODS];
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
      if (await reopenMic()) st.started = at;
    } catch (e) { dlog('reopen failed: ' + (e?.message || e)); }
  }, 1400);
}

let reopens = 0;
async function reopenMic() {
  const keep = mode, run = ++reopens;
  probing++;
  dropShadow();
  if (cap?.method === 'recorder' && cap.recording()) {
    try { await endRec(); } catch {}
    if (run !== reopens || mode !== keep) return false;
  }
  try { cap?.close(); } catch {}
  cap = null;
  stream?.getTracks().forEach((t) => { t.onended = t.onmute = t.onunmute = null; t.stop(); });
  const s = await getMic();
  if (run !== reopens) { s.getTracks().forEach((t) => t.stop()); return false; }
  stream = s;
  if (mode !== keep) { release(); return false; }
  watchTrack(stream);
  muteTrack();
  startShadow();
  const o = await openFirst(order());
  if (!o) {
    if (run !== reopens || mode !== keep) return false;
    throw Object.assign(new Error('no way to read the mic worked here'), { name: 'NoCapture' });
  }
  probeDone = probe(o);
  return true;
}

async function switchMic() {
  try {
    if (!(await reopenMic())) return;
    lost = '';
    sessionAt = performance.now();
    if (err && err.code !== 'server') err = null;
    dlog(`mic switched: ${stream?.getAudioTracks()[0]?.label || 'track'}, ${cap?.what}`);
    paint();
    checkAudio();
  } catch (e) {
    mode = '';
    phase = '';
    release();
    fail(...micError(e));
  }
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
  reopens++;
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
const SET_ICON = svg(I.tune);
function ensureBar() {
  if (bar) return bar;
  document.head.append(h('style', { id: 'voice-css' }, CSS));
  bar = h('div', { id: 'voice-bar', role: 'status', 'aria-live': 'polite', hidden: true },
    h('span.vb-dot'),
    h('span.vb-main', {}, h('span.vb-text'), h('span.vb-sub')),
    h('span.vb-lv', {}, h('span.vb-meter', {}, h('i')), h('span.vb-db')),
    h('button.vb-act', { type: 'button', hidden: true }),
    h('button.vb-set', { type: 'button', 'aria-label': 'Voice settings', title: 'Voice settings', html: SET_ICON }),
    h('button.vb-x', { type: 'button', 'aria-label': 'Dismiss', title: 'Dismiss', html: svg(I.x) }));
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
  return { mode, state, text: state ? `${mode} \u00b7 ${state}` : mode, short: on && mu ? 'Mic muted' : SHORT[m] || '', State: upFirst(state) };
}

const TEXT = new Proxy({}, { get: (_, k) => (STATE[k] ? upFirst(STATE[k]) : undefined) });

const wakeWord = () => S.voice?.wake_word || 'hey god';

const wakeLabel = () => wakeWord().replace(/\b\w/g, (c) => c.toUpperCase());

const MIC = I.mic;
const ICON = {
  off: I.micoff,
  push: I.mic,
  open: I.audio,
  wake: I.ear,
};

const MIC_OFF = ICON.off;

const srvMuted = () => S.voice?.speaker_muted === true;
const agentMuted = () => srvMuted();
const spokenOn = () => !srvMuted() && spkVolNow() > 0;
const spkVolNow = () => { const p = S.voice?.volume_pct; return Number.isFinite(p) ? Math.max(0, Math.min(1, p / 100)) : 1; };
const typedOn = () => S.voice?.typed_spoken === true;
const spkIcon = () => svg(agentMuted() || spkVolNow() === 0 ? I.volx : spkVolNow() < 0.5 ? I.vol1 : I.vol2);
const spkWord = () => (agentMuted() ? 'muted' : Math.round(spkVolNow() * 100) + '%');
const spkKey = () => (srvMuted() ? 'M' : spkVolNow() === 0 ? 'Z' : spkVolNow() < 0.5 ? 'L' : 'H');
const spkWhy = () => (srvMuted() ? 'Muted for the app and the web: tap to unmute' : 'Shared with the Mac app');

let headPick = null;
function paintHeader(state) {
  for (const id of ['#b-ptt', '#b-ptt2']) {
    const el = $(id);
    if (!el) continue;

    const mu = micMuted && pick !== 'off';
    const w = micWords(pick, state, micMuted);
    const am = agentMuted();
    if (el.dataset.mode !== pick || headPick !== pick + mu + w.text + spkKey()) {
      el.dataset.mode = pick;
      const sp = id === '#b-ptt' ? `<span class="ml-sp${am ? ' off' : ''}">${spkIcon()}${am ? '<span class="ml-spw">agent muted</span>' : ''}</span>` : '';
      el.innerHTML = svg(mu ? MIC_OFF : ICON[pick]) + (id === '#b-ptt' ? `<span class="ml-s">${w.short}</span><span class="ml-l">${w.text}</span>` : '') + sp;
    }
    el.classList.toggle('muted', mu);
    el.classList.toggle('amuted', am && id === '#b-ptt');
    const talking = mode === 'push' && (state === 'listening' || state === 'noaudio');
    el.classList.toggle('talking', talking);
    if (state && state !== 'paused') el.dataset.voice = state; else delete el.dataset.voice;
    const what = pick === 'push' && id === '#b-ptt2' ? (mu ? w.text + '. M unmutes' : mode === 'push' ? 'Listening: release to send' : 'Hold to talk') : `${w.text}. Agent voice ${am ? 'muted' : spkWord()}. Tap to change`;
    el.setAttribute('aria-label', what);
    el.title = what;
  }
  headPick = pick + (micMuted && pick !== 'off') + micWords(pick, state, micMuted).text + spkKey();
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
    h('button.ms-chg', { type: 'button', 'aria-label': 'Change the mic mode', title: 'Change the mic mode', html: svg(I.chevd) }));
  strip.addEventListener('click', (e) => {
    if (e.target.closest('.ms-mute')) { e.stopPropagation(); setMicMuted(!micMuted); return; }
    if (e.target.closest('.ms-spk')) { e.stopPropagation(); setSpeaker({ muted: !agentMuted() }); return; }
    const a = e.target.closest('.ms-act');
    if (a) {
      e.stopPropagation();
      hfAct(a.dataset.act);
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

function hfAct(act) {
  if (act === 'unmute') {
    send({ t: 'voice', unmute_mac: true });
    const m = resumeMode || pick; lost = ''; resumeMode = ''; err = null; unmuteAt = Date.now();
    setTimeout(() => start(m), 400);
  } else if (act === 'stop') { hush(); send({ t: 'voice', hush: true }); } else if (act === 'start') { const m = resumeMode || pick; lost = ''; resumeMode = ''; err = null; start(m); }
  paint();
}

function hfWords(s) {
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
  return { m, state, what, sub, act };
}

function paintStrip(s) {
  const el = ensureStrip();
  const on = pick === 'open' || pick === 'wake';
  if (el.hidden === on) el.hidden = !on;
  document.body.classList.toggle('hf', on);
  if (!on) return;
  const { m, state, what, sub, act } = hfWords(s);
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
  const sk = spkWord() + spkKey();
  if (sb.dataset.k !== sk) {
    sb.dataset.k = sk;
    sb.querySelector('.ms-spi').innerHTML = spkIcon();
    sb.querySelector('.ms-spv').textContent = agentMuted() ? 'agent muted' : spkVolNow() >= 1 ? '' : spkWord();
    sb.setAttribute('aria-pressed', String(agentMuted()));
    const st3 = agentMuted() ? 'The agent\'s voice is muted: tap to hear it' : `The agent's voice: ${spkWord()}. Tap to mute it`;
    sb.title = st3;
    sb.setAttribute('aria-label', st3);
  }
  const a = el.querySelector('.ms-act');
  a.hidden = !act;
  a.dataset.act = act;
  a.textContent = { stop: 'Stop', unmute: 'Unmute', start: err ? 'Retry' : lost === 'tap' ? 'Resume' : 'Start' }[act] || '';
}

let vbar = null, talk = null;
const vx = {};
const barOn = () => !!vbar && !vbar.hidden;
const tabName = (t) => t.label || t.name || t.title || 't' + t.uid;

function toWords() {
  const k = talkNow();
  if (!k.on) return { k, v: 'assistant', label: 'Assistant', icon: I.chat, tip: 'Your voice goes to the assistant' };
  if (k.follow) return { k, v: 'follow', label: 'Follow: ' + k.name, icon: I.eye, tip: `Your voice goes to ${k.name} and moves with the tab you open` };
  return { k, v: 'tab', label: k.name, icon: I.squareterminal, tip: `Your voice goes to ${k.name}` };
}

function pickTab(uid) {
  if (!send({ t: 'voice', direct: uid, follow: false })) toast('Not connected', 'warn');
}

function talkMenu(anchor) {
  if (menuSkip) { menuSkip = false; return; }
  menuAnchor = anchor;
  const r = anchor.getBoundingClientRect();
  const row = (label, hint, on, fn, o = {}) => h('button.mm-i.vx-row', { type: 'button', role: 'menuitemradio', 'aria-checked': String(!!on), disabled: !!o.off, 'data-v': o.v, onclick: () => { closeSheet(); fn(); } },
    h('span.mm-ic', { html: svg(o.icon || I.squareterminal) }),
    h('span.mm-t', {}, h('span.vx-rn', {}, label), hint ? h('small', {}, hint) : null),
    h('span.mm-ck', { html: on ? svg(I.tick) : '' }));
  const list = h('div.ctx-list.vx-list', { role: 'none' });
  const q = h('input.ctx-q', { type: 'search', placeholder: 'Filter tabs', 'aria-label': 'Filter tabs', autocomplete: 'off', spellcheck: false });
  const draw = () => {
    const k = talkNow();
    const w = q.value.trim().toLowerCase().split(/\s+/).filter(Boolean);
    const hit = (t) => w.every((x) => `${tabName(t)} ${t.folder || ''}`.toLowerCase().includes(x));
    const seen = new Set(panes.visible().map((p) => p.id).filter(Boolean));
    const fixed = w.length ? [] : [
      row('Assistant', 'It hears you and acts across your tabs', !k.on, () => pickTalk('assistant'), { v: 'assistant', icon: I.chat }),
      row('Follow active tab', k.t ? `Now ${k.t.name}, then the tab you open next` : 'Moves with the tab you open', k.follow, () => pickTalk('follow'), { v: 'follow', icon: I.eye, off: !k.can && !k.on }),
      h('div.mm-sep')];
    const tabs = (S.tabs || []).filter(hit).sort((a, b) => seen.has(b.id) - seen.has(a.id));
    const rows = tabs.map((t) => {
      const can = t.can_direct !== false;
      const hint = [seen.has(t.id) ? 'On screen' : '', can ? '' : 'Not running', t.folder || ''].filter(Boolean).join(' · ');
      return row(tabName(t), hint, k.on && !k.follow && k.uid === t.uid, () => pickTab(t.uid), { v: 't' + t.uid, off: !can });
    });
    list.replaceChildren(...fixed, ...(rows.length ? rows : [h('div.mnote', { style: 'padding:8px 12px' }, 'No tab matches')]));
  };
  q.oninput = draw;
  draw();
  openMenu({ x: r.left, y: r.top - 6, up: true }, h('div.mm.vx-menu', { role: 'none' }, h('div.mm-h', { role: 'none' }, h('span', {}, 'Talk to')), q, list));
  if (!document.body.classList.contains('touch')) q.focus({ preventScroll: true });
}

function ensureVbar() {
  if (vbar) return vbar;
  vbar = $('#vbar');
  talk = h('button', { id: 'talk', type: 'button', 'aria-label': 'Hold to talk' },
    h('span.tk-ic'), h('span.tk-l', {}, 'Talk'),
    h('span.tk-lv', { hidden: true }, h('span.tk-meter', {}, h('i')), h('span.tk-db')));
  ptt(talk);
  const chev = () => h('span.vx-chev', { 'aria-hidden': 'true', html: svg(I.chevd) });
  vx.to = h('button.vx-to', { id: 'vx-to', type: 'button', 'aria-haspopup': 'menu', onclick: () => talkMenu(vx.to) },
    h('span.vx-k', {}, 'Talk to'), h('span.vx-tic', { 'aria-hidden': 'true' }), h('b.vx-tl'), chev());
  vx.ctx = h('button.vx-to', { id: 'ctxbtn', type: 'button', 'aria-haspopup': 'menu', 'aria-label': 'Context: what the assistant looks at', title: 'Context: what the assistant looks at, all tabs or one tab',
    onclick: () => {
      if (menuSkip) { menuSkip = false; return; }
      menuAnchor = vx.ctx;
      const r = vx.ctx.getBoundingClientRect();
      import('./chat.js').then((m) => m.scopeMenu({ x: r.left, y: r.top - 6, up: true }));
    } }, h('span.vx-k', {}, 'Context'), h('b.cx-l'), chev());
  vx.seg = segGroup('sg-mic.vx-seg', 'Microphone mode', MIC_SEG, (m) => setMode(m));
  vx.mute = muteSwitch();
  vx.spk = spkSwitch();
  vx.vol = volRow('vx-vol', 'Volume');
  vx.lvl = lvlPick('vx-lvl', 'Length');
  vx.inp = h('div.mp-in.vx-in', { hidden: true });
  vx.set = h('button.vx-ib.vx-set', { type: 'button', 'aria-label': 'Voice settings', title: 'Voice settings: voice, speed, wake word, mic test', html: SET_ICON, onclick: openVoiceSheet });
  vx.more = h('button.vx-ib.vx-more', { type: 'button', 'aria-haspopup': 'dialog', 'aria-label': 'Voice options', title: 'Voice options: mode, mute, spoken replies', html: svg(I.dots), onclick: () => openMicMenu(vx.more) });
  vbar.append(talk, vx.to, vx.ctx, h('span.vx-gap'), vx.seg,
    h('span.vx-g.vx-gm', {}, h('span.vx-k', {}, 'Mic'), vx.mute),
    h('span.vx-g.vx-gr', {}, h('span.vx-k', {}, 'Replies'), vx.spk, vx.lvl, vx.vol),
    vx.inp, vx.set, vx.more);
  const paintCtx = () => import('./chat.js').then((m) => { vx.ctx.querySelector('.cx-l').textContent = m.scopeLabel(); });
  on('scope', paintCtx); on('state', paintCtx); paintCtx();
  on('layout', showVbar);
  matchMedia(DESK).addEventListener('change', () => paint());
  return vbar;
}

function homeVbar() {
  const main = $('#main'), chat = $('#chat');
  const covered = $('#app')?.classList.contains('chat-open') && getComputedStyle(main).visibility === 'hidden';
  if (covered) { if (vbar.parentElement !== chat) $('#att').before(vbar); } else if (vbar.parentElement !== main) $('#sbar').before(vbar);
}

function showVbar() {
  if (!vbar) return;
  const desk = isDesk();
  const show = desk || (pick === 'push' && !document.body.classList.contains('kb'));
  homeVbar();
  document.body.classList.toggle('vx-ph', show && !desk);
  if (vbar.hidden !== show) return;
  vbar.hidden = !show;
  if (!desk) queueMicrotask(() => emit('layout'));
}

function talkAlt() {
  if (pick === 'off') { setMode('push'); hint = 'Push to talk is on: hold Talk while you talk'; paint(); return; }
  const a = hfWords(shown()).act;
  if (a) hfAct(a);
}

function paintTarget(live) {
  const k = talkNow(), on = pick !== 'off';
  const uid = on && k.on ? k.uid : null;
  for (const p of panes.list || []) {
    const hit = uid != null && !!p.id && uidOf(p.id) === uid;
    p.el.classList.toggle('vox', hit);
    p.el.classList.toggle('vox-live', hit && live);
    if (hit && !p.vxb) { p.vxb = h('span.vx-b', { html: svg(MIC) + '<b>Voice</b>' }); p.name.after(p.vxb); }
    if (!p.vxb) continue;
    p.vxb.hidden = !hit;
    const tip = live ? 'Listening: your voice goes to this tab' : 'Your voice goes to this tab';
    if (p.vxb.title !== tip) p.vxb.title = tip;
  }
  const c = $('#chat');
  c.classList.toggle('vox', on && !k.on);
  c.classList.toggle('vox-live', on && !k.on && live);
}

function paintVbar(s) {
  const b = ensureVbar();
  showVbar();
  const off = pick === 'off', hf = pick === 'open' || pick === 'wake';
  b.dataset.mode = pick;
  let st2, label, tip, ic = ICON[pick], lv = false;
  if (off) { st2 = 'off'; label = 'Voice off'; tip = 'Voice is off. Click to turn on push to talk'; }
  else if (hf) {
    const w = hfWords(s);
    st2 = w.state || 'listening';
    label = w.act === 'start' ? (err ? 'Retry' : lost === 'tap' ? 'Resume' : 'Start listening') : w.act === 'unmute' ? 'Unmute the Mac' : upFirst(w.what);
    tip = `${modeName(w.m)}: ${w.what}${w.sub ? '. ' + w.sub : ''}${w.act === 'stop' ? '. Click to stop the reply' : ''}`;
    ic = micMuted ? MIC_OFF : ICON[w.m] || MIC;
    lv = !!mode && !micMuted;
  } else {
    st2 = err ? 'error' : micMuted ? 'muted' : mode === 'push' ? s || 'listening' : s === 'sending' || s === 'thinking' || s === 'speaking' ? s : 'idle';
    label = { starting: upFirst(STATE.starting), asking: upFirst(STATE.asking), listening: 'Release to send', noaudio: upFirst(STATE.noaudio), sending: upFirst(STATE.sending), muted: 'Mic muted' }[st2] || 'Hold to talk';
    tip = micMuted ? 'The mic is muted: M or the Mic switch unmutes' : mode === 'push' ? 'Listening: release to send' : 'Hold to talk';
    if (micMuted) ic = MIC_OFF;
    lv = mode === 'push';
  }
  talk.dataset.state = st2;
  const l = talk.querySelector('.tk-l');
  if (l.textContent !== label) l.textContent = label;
  const ti = talk.querySelector('.tk-ic');
  if (ti.dataset.k !== ic) { ti.dataset.k = ic; ti.innerHTML = svg(ic); }
  if (talk.title !== tip) { talk.title = tip; talk.setAttribute('aria-label', tip); }
  talk.querySelector('.tk-lv').hidden = !lv;
  if (pick === 'push') talk.setAttribute('aria-pressed', String(mode === 'push')); else talk.removeAttribute('aria-pressed');
  const live = !off && !micMuted && !err && (mode === 'push' ? s !== 'sending' : handsFree() && ['listening', 'hearing', 'heard', 'waiting'].includes(s));
  b.toggleAttribute('data-live', live);
  const t = toWords();
  vx.to.dataset.v = t.v;
  const tl = vx.to.querySelector('.vx-tl');
  if (tl.textContent !== t.label) tl.textContent = t.label;
  const tc = vx.to.querySelector('.vx-tic');
  if (tc.dataset.k !== t.v) { tc.dataset.k = t.v; tc.innerHTML = svg(t.icon); }
  const tt = `Talk to: ${t.label}. ${t.tip}. Click to change`;
  if (vx.to.title !== tt) { vx.to.title = tt; vx.to.setAttribute('aria-label', tt); }
  vx.ctx.hidden = t.k.on;
  paintModeSeg(vx.seg);
  paintMute(vx.mute);
  paintSpk(vx.spk);
  paintVol(vx.vol);
  paintLvl(vx.lvl);
  paintInput(vx.inp);
  paintTarget(live);
}

let menuAnchor = null, menuSkip = false;

const MIC_SEG = [
  { v: 'off', label: 'Off', icon: ICON.off, note: 'The mic stays closed' },
  { v: 'push', label: 'Push to talk', short: 'Push', icon: ICON.push, note: 'Hold Talk while you talk' },
  { v: 'open', label: 'Open mic', short: 'Open', icon: ICON.open, note: 'Always listening for commands' },
  { v: 'wake', label: 'Wake word', short: 'Wake', icon: ICON.wake, note: '' },
];
const TALK_SEG = [
  { v: 'assistant', label: 'Assistant' },
  { v: 'tab', label: 'This tab' },
  { v: 'follow', label: 'Follow active tab', short: 'Follow tab' },
];

function segGroup(cls, name, opts, onPick) {
  const g = h('div.sg.' + cls, { role: 'radiogroup', 'aria-label': name },
    opts.map((o) => h('button.sg-b', { type: 'button', role: 'radio', 'aria-checked': 'false', tabindex: -1, 'data-m': o.v, title: o.label, onclick: () => onPick(o.v) },
      o.icon ? h('span.sg-ic', { html: svg(o.icon) }) : null,
      h('span.sg-l', {}, o.label), h('span.sg-s', {}, o.short || o.label))));
  g.addEventListener('keydown', (e) => {
    const step = { ArrowRight: 1, ArrowLeft: -1 }[e.key];
    if (!step && e.key !== 'Home' && e.key !== 'End') return;
    const bs = [...g.querySelectorAll('.sg-b:not(:disabled)')];
    if (!bs.length) return;
    const i = bs.indexOf(document.activeElement);
    const n = e.key === 'Home' ? 0 : e.key === 'End' ? bs.length - 1 : (Math.max(0, i) + step + bs.length) % bs.length;
    e.preventDefault();
    e.stopPropagation();
    bs[n].focus();
  });
  return g;
}
function segSet(g, v, off = []) {
  const bs = [...g.querySelectorAll('.sg-b')];
  for (const b of bs) {
    const on = b.dataset.m === v;
    b.setAttribute('aria-checked', String(on));
    b.disabled = off.includes(b.dataset.m);
    b.tabIndex = on ? 0 : -1;
  }
  if (!bs.some((b) => b.tabIndex === 0 && !b.disabled)) { const f = bs.find((b) => !b.disabled); if (f) f.tabIndex = 0; }
}

function talkNow() {
  const f = S.mode_flags || {}, t = curTab();
  const on = !!f.direct;
  const follow = on && S.voice?.direct_follow === true;
  const here = on && !!t && (f.direct_tab == null || f.direct_tab === t.uid);
  return { on, follow, here, t, name: f.direct || '', uid: f.direct_tab, can: !!t && t.can, v: !on ? 'assistant' : follow ? 'follow' : here ? 'tab' : '' };
}
function pickTalk(v) {
  const k = talkNow();
  let ok = true;
  if (v === 'assistant') { if (k.on) ok = send({ t: 'voice', direct: false }); }
  else if (k.can) ok = send({ t: 'voice', direct: k.t.uid, follow: v === 'follow' });
  else if (k.on && v === 'follow') ok = send({ t: 'voice', follow: true });
  else { toast('Open a running tab first', 'warn'); return; }
  if (!ok) toast('Not connected', 'warn');
}

function paintInput(box) {
  if (!box) return;
  const can = canPickInput();
  if (box.hidden === can) box.hidden = !can;
  if (!can) return;
  const cur = inputNow();
  const gone = !!micDev && !cur;
  const note = gone && inputsKnown ? 'Not connected, using the system default'
    : inputFell === 'failed' ? 'Did not start, using the system default'
      : inputFell === 'gone' ? 'Not connected, using the system default'
        : !inputsKnown ? 'Names show after the mic is first used' : '';
  const sig = JSON.stringify([inputs, inputsKnown, micDev, note]);
  if (box.dataset.sig === sig) return;
  box.dataset.sig = sig;
  let ctl;
  if (!micDev && inputs.length < 2) {
    const name = inputs[0]?.label || 'System default';
    ctl = h('span.mp-in1', { title: name }, name);
  } else {
    ctl = h('select', { 'aria-label': 'Microphone input', onchange: (e) => setInput(e.target.value) },
      h('option', { value: '' }, 'System default'),
      inputs.map((d) => h('option', { value: d.id, selected: cur?.id === d.id }, d.label)),
      gone ? h('option', { value: micDev.id, selected: true }, (micDev.label || 'Saved mic') + (inputsKnown ? ' (not connected)' : '')) : null);
    ctl.value = cur ? cur.id : gone ? micDev.id : '';
    ctl.title = ctl.selectedOptions[0]?.textContent || '';
  }
  box.replaceChildren(h('label.mp-inr', {}, h('span.mp-inl', {}, 'Input'), ctl));
  if (note) box.append(h('p.mp-note.mp-innote', {}, note));
}

function paintModeSeg(g) {
  const mu = micMuted && pick !== 'off';
  segSet(g, pick);
  g.classList.toggle('muted', mu);
  for (const b of g.querySelectorAll('.sg-b')) {
    const want = mu && b.dataset.m === pick ? 'x' : '';
    if ((b.dataset.x || '') !== want) { b.dataset.x = want; b.querySelector('.sg-ic').innerHTML = svg(want ? MIC_OFF : ICON[b.dataset.m]); }
  }
}

const muteSwitch = () => h('button.mp-mute.mm-mute', { type: 'button', role: 'switch', 'aria-checked': 'false', onclick: () => setMicMuted(!micMuted) },
  h('span.mp-mic', { 'aria-hidden': 'true' }), h('b.mm-st'), h('span.mm-sw', { 'aria-hidden': 'true' }, h('i')));
function paintMute(mb) {
  const off = pick === 'off', mu = micMuted && !off;
  mb.disabled = off;
  mb.setAttribute('aria-checked', String(mu));
  const ms = off ? 'Off' : mu ? 'Muted' : 'Live';
  const st = mb.querySelector('.mm-st');
  if (st.textContent !== ms) { st.textContent = ms; mb.querySelector('.mp-mic').innerHTML = svg(mu ? MIC_OFF : MIC); }
  const ml = off ? 'Mute mic: the mic is off' : mu ? 'Mic muted: nothing is sent, replies still play. Unmute (M)' : 'Mic live. Mute mic (M): replies still play';
  if (mb.title !== ml) { mb.title = ml; mb.setAttribute('aria-label', ml); }
}

const spkSwitch = () => h('button.mp-spk', { type: 'button', role: 'switch', 'aria-checked': 'false', onclick: () => setSpeaker(spokenOn() ? { muted: true } : spkVolNow() > 0 ? { muted: false } : { muted: false, volume: volBack }) },
  h('span.mp-mic', { 'aria-hidden': 'true' }), h('b.mm-st'), h('span.mm-sw', { 'aria-hidden': 'true' }, h('i')));
function paintSpk(sb) {
  const son = spokenOn(), am = agentMuted();
  sb.setAttribute('aria-checked', String(son));
  const ss = son ? 'On' : 'Off';
  const sst = sb.querySelector('.mm-st');
  if (sst.textContent !== ss) { sst.textContent = ss; sb.querySelector('.mp-mic').innerHTML = svg(son ? I.vol2 : I.volx); }
  const sl = son ? 'Spoken replies are on. Turn off to get text only' : am ? 'Spoken replies are off: replies show as text only. Turn on' : 'Spoken replies are off: the volume is at 0. Turn on';
  if (sb.title !== sl) { sb.title = sl; sb.setAttribute('aria-label', sl); }
}

const LEVELS = [
  { v: 'brief', label: 'Brief', note: 'One short sentence, just the answer' },
  { v: 'normal', label: 'Normal', note: 'A couple of sentences' },
  { v: 'detailed', label: 'Detailed', note: 'The full explanation, read out' },
];
const lvlNow = () => (LEVELS.find((l) => l.v === S.voice?.talk_level) || LEVELS[1]);
function setTalkLevel(v) {
  if (!LEVELS.some((l) => l.v === v)) return;
  const sv = S.voice || (S.voice = {});
  const prev = sv.talk_level;
  sv.talk_level = v;
  paintLvls();
  tool('speaker', { talk_level: v }).then((r) => {
    if (!r?.error && r?.ok !== false) return;
    sv.talk_level = prev;
    toast(r.error || 'Could not change how much is said', 'warn');
    paintLvls();
  });
}
const lvlPick = (cls, label) => h('label.lvl.' + cls, {},
  h('span.lvl-ic', { 'aria-hidden': 'true', html: svg(I.lines) }),
  h('span.lvl-l', {}, label),
  h('select.lvl-s', { 'aria-label': 'How much is said out loud', onchange: (e) => setTalkLevel(e.target.value) },
    LEVELS.map((l) => h('option', { value: l.v }, l.label))));
function paintLvl(el) {
  if (!el) return;
  const s = el.querySelector('select'), l = lvlNow(), son = spokenOn();
  if (s.value !== l.v) s.value = l.v;
  s.disabled = !son;
  el.classList.toggle('off', !son);
  const tip = son ? `How much is said out loud: ${l.label}. ${l.note}. What is written stays the same` : 'How much is said out loud. Spoken replies are off';
  if (el.title !== tip) el.title = tip;
}
function paintLvls() { for (const el of document.querySelectorAll('.lvl')) paintLvl(el); }

function paintPickers() {
  if (vx.inp) paintInput(vx.inp);
  paintMenu();
}

let micUi = null;
function paintMenu() {
  const el = micUi;
  if (!el) return;
  const mu = micMuted && pick !== 'off';
  paintModeSeg(el.querySelector('.sg-mic'));
  const note = el.querySelector('.mp-micnote');
  const nt = mu ? 'Muted, nothing is sent. M unmutes' : pick === 'wake' ? `Say "${wakeLabel()}", then your request` : MIC_SEG.find((o) => o.v === pick)?.note || '';
  if (note.textContent !== nt) note.textContent = nt;
  note.classList.toggle('bad', mu);
  paintMute(el.querySelector('.mp-mute'));
  const tr = el.querySelector('.mm-typed'), ton = typedOn();
  tr.setAttribute('aria-checked', String(ton));
  const son = spokenOn(), am = agentMuted();
  paintSpk(el.querySelector('.mp-spk'));
  el.querySelector('.mm-vol').hidden = am;
  paintLvl(el.querySelector('.mm-lvl'));
  tr.hidden = !son;
  const sn = el.querySelector('.mp-spknote');
  sn.hidden = son;
  const st2 = am ? 'Replies show as text only' : 'Volume is at 0, so nothing is heard';
  if (sn.textContent !== st2) sn.textContent = st2;
  paintInput(el.querySelector('.mp-in'));
  const k = talkNow();
  const dis = [];
  if (!k.can) dis.push('tab');
  if (!k.can && !k.on) dis.push('follow');
  segSet(el.querySelector('.sg-talk'), k.v, dis);
  const who = el.querySelector('.mp-who');
  const sig = JSON.stringify([k.on, k.follow, k.here, k.name, k.can, !!k.t]);
  if (who.dataset.sig !== sig) {
    who.dataset.sig = sig;
    const name = h('b', {}, k.on ? k.name : 'the assistant');
    const tail = !k.on ? (k.t ? (k.can ? '' : '. This tab is not running') : '. Open a tab to talk to it')
      : k.follow ? (k.here ? ' and to the tab you open next' : k.t && !k.can ? '. This tab is not running' : '')
        : k.here ? '' : ', not the tab you see';
    who.replaceChildren('Talking to ', name, tail);
  }
}

function openMicMenu(anchor) {
  if (menuSkip) { menuSkip = false; return; }
  menuAnchor = anchor;
  const r = anchor.getBoundingClientRect();
  const w = Math.min(320, innerWidth - 16);
  const x = Math.max(8, r.right - w);
  const at = r.top > innerHeight / 2 ? { x, y: r.top - 6, up: true } : { x, y: r.bottom + 6 };
  const muteBtn = muteSwitch();
  const spkBtn = spkSwitch();
  const typedRow = h('button.mp-row.mm-tog.mm-typed', { type: 'button', role: 'switch', 'aria-checked': 'false' },
    h('span.mm-t', {}, 'Also speak replies to typed messages'),
    h('span.mm-sw', { 'aria-hidden': 'true' }, h('i')));
  typedRow.onclick = async () => {
    const want = !typedOn();
    const res = await tool('speaker', { speak_typed: want });
    if (res.error || res.ok === false) return toast(res.error || 'Could not change it', 'warn');
    if (S.voice) S.voice.typed_spoken = want;
    paintMenu();
  };
  micUi = h('div.mm.mp', { role: 'none' },
    h('div.mm-h.mp-h', { role: 'none' }, h('span', { id: 'mp-mic-l' }, 'Microphone'), muteBtn),
    segGroup('sg-mic', 'Microphone mode', MIC_SEG, (m) => setMode(m)),
    h('p.mp-note.mp-micnote', { 'aria-live': 'polite' }),
    h('div.mp-in', { hidden: true }),
    h('div.mm-sep'),
    h('div.mm-h', { role: 'none' }, h('span', {}, 'Spoken replies'), spkBtn),
    h('div.mm-vl', {}, volRow('mm-vol', 'Volume'), lvlPick('mm-lvl', 'Reply length')),
    typedRow,
    h('p.mp-note.mp-spknote', { 'aria-live': 'polite', hidden: true }),
    h('div.mm-sep'),
    h('div.mm-h', { role: 'none' }, h('span', {}, 'Talk to')),
    segGroup('sg-talk', 'Who your voice talks to', TALK_SEG, pickTalk),
    h('p.mp-note.mp-who', { 'aria-live': 'polite' }),
    h('div.mm-sep'),
    h('button.mp-row.mp-set', { type: 'button', onclick: () => { closeSheet(); openVoiceSheet(); } },
      h('span.mm-ic', { html: SET_ICON }), h('span.mm-t', {}, 'Voice settings', h('small', {}, 'Voice, speed, wake word, mic test'))));
  micW = w;
  paintMenu();
  openMenu(at, micUi);
  micPopSync();
  openMenu(at, micUi);
  if (document.body.classList.contains('kbd')) micUi.querySelector('.sg-mic .sg-b[tabindex="0"]')?.focus({ preventScroll: true });
}

let micW = 320;
function micPopSync() {
  const mine = !!micUi && micUi.isConnected;
  if (!mine) micUi = null;
  const p = document.getElementById('pop');
  if (!p || p.classList.contains('mic-pop') === mine) return;
  p.classList.toggle('mic-pop', mine);
  p.setAttribute('role', mine ? 'dialog' : 'menu');
  if (mine) { p.setAttribute('aria-label', 'Voice'); p.style.width = micW + 'px'; } else { p.removeAttribute('aria-label'); p.style.width = ''; }
}

let moveBar = null, moveGone = null;
function moveWant() {
  const k = talkNow();
  if (!k.on || k.follow || k.uid == null) { moveGone = null; return null; }
  if (!k.t || k.here) { if (k.here) moveGone = null; return null; }
  if (!k.can || moveGone === k.uid || sheetKind) return null;
  return k;
}
function placeMove() {
  if (!moveBar || moveBar.hidden) return;
  const vis = (e) => e && !e.hidden && e.getClientRects().length;
  const pill = [$('#b-ptt'), $('#mic-strip')].find(vis);
  const r = pill ? pill.getBoundingClientRect() : { left: 8, right: 8, bottom: 48 };
  let top = r.bottom;
  for (const e of [$('#mic-strip'), $('#voice-bar'), $('#top'), isDesk() ? $('#panes .pane-head') : null]) if (vis(e)) top = Math.max(top, e.getBoundingClientRect().bottom);
  const w = moveBar.offsetWidth;
  const mid = (r.left + r.right) / 2;
  moveBar.style.left = Math.round(Math.max(8, Math.min(mid - w / 2, innerWidth - w - 8))) + 'px';
  moveBar.style.top = Math.round(top + 6) + 'px';
}
function paintMove() {
  const k = moveWant();
  if (!k) { if (moveBar && !moveBar.hidden) moveBar.hidden = true; return; }
  if (!moveBar) {
    moveBar = h('div', { id: 'voice-move', role: 'status', 'aria-live': 'polite', hidden: true },
      h('span.vm-ic', { 'aria-hidden': 'true', html: svg(MIC) }),
      h('span.vm-t'),
      h('button.vm-go', { type: 'button' }, 'Move here'),
      h('button.vm-x', { type: 'button', 'aria-label': 'Dismiss', title: 'Dismiss', html: svg(I.x) }));
    moveBar.querySelector('.vm-go').addEventListener('click', () => {
      const n = talkNow();
      if (n.t && !send({ t: 'voice', direct: n.t.uid })) toast('Not connected', 'warn');
    });
    moveBar.querySelector('.vm-x').addEventListener('click', () => { moveGone = talkNow().uid ?? null; paintMove(); });
    $('#app').append(moveBar);
    addEventListener('resize', placeMove);
  }
  const t = moveBar.querySelector('.vm-t');
  const text = `Voice is talking to ${k.name}.`;
  if (t.textContent !== text) t.textContent = text;
  const go = moveBar.querySelector('.vm-go');
  const gl = `Move voice here, to ${k.t.name}`;
  if (go.title !== gl) { go.title = gl; go.setAttribute('aria-label', gl); }
  moveBar.hidden = false;
  placeMove();
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
  paintVbar(s);
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
  const model = S.voice?.model;
  if (text && !sub && (model === 'loading' || model === 'unloaded')) sub = model === 'loading' ? 'Loading the speech model' : 'Speech model unloaded; it loads when you talk';

  const inStrip = (pick === 'open' || pick === 'wake') && !engineErr && s !== 'locked';

  const inTalk = pick === 'push' && !err && !engineErr && barOn() && ['listening', 'sending', 'thinking', 'starting'].includes(s);
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
  micPopSync();
  if (micUi) paintMenu();
  paintMove();
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
    if (pick !== 'push') { tap = 'alt'; return; }
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
    if (tap) { const a = tap; tap = false; if (e?.type === 'pointerup') { if (a === 'alt') talkAlt(); else openMicMenu(btn); } return; }
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
const spkGain = () => (srvMuted() ? 0 : duck * spkVolNow());
function applyGain() {
  if (!outGain || !ctx) return;
  const g = spkGain();
  try { outGain.gain.setTargetAtTime(g, ctx.currentTime, 0.015); } catch { outGain.gain.value = g; }
}
export const outLevel = () => ({ gain: outGain ? outGain.gain.value : null, target: spkGain(), playing: playing.size, state: ctx ? ctx.state : '' });
function onDuck(m) {
  const l = Number(m?.level);
  const v = Number.isFinite(l) ? Math.max(0, Math.min(1, l)) : 1;
  if (v === duck) return;
  duck = v;
  if (playing.size) dlog(v < 1 ? `reply ducked to ${Math.round(v * 100)}% (is it a barge-in?)` : 'reply back to full volume');
  applyGain();
}
let volTimer = 0, volBack = 0.5;
export function setSpeaker({ volume, muted } = {}) {
  const v = S.voice || (S.voice = {});
  if (spkVolNow() > 0) volBack = spkVolNow();
  const prev = { speaker_muted: v.speaker_muted, volume_pct: v.volume_pct };
  const args = {};
  if (volume != null && Number.isFinite(+volume)) {
    v.volume_pct = Math.round(Math.max(0, Math.min(1, +volume)) * 100);
    args.volume = v.volume_pct;
    if (v.speaker_muted && v.volume_pct > 0 && muted == null) muted = false;
  }
  if (muted != null) { v.speaker_muted = !!muted; args.muted = !!muted; }
  const sendNow = () => {
    volTimer = 0;
    tool('speaker', args).then((r) => {
      if (r?.error || r?.ok === false) {
        v.speaker_muted = prev.speaker_muted; v.volume_pct = prev.volume_pct;
        toast(r.error || 'Could not change the agent voice', 'warn');
        applyGain(); for (const x of document.querySelectorAll('.vol-row')) paintVol(x); paint();
      }
    });
  };
  clearTimeout(volTimer);
  if (args.volume != null && args.muted == null) volTimer = setTimeout(sendNow, 160); else sendNow();
  applyGain();
  for (const r of document.querySelectorAll('.vol-row')) paintVol(r);
  paint();
}

function volRow(cls, label = 'Agent voice') {
  const b = h('button.vr-b', { type: 'button', onclick: (e) => { e.stopPropagation(); setSpeaker({ muted: !agentMuted() }); } });
  const r = h('input.vr-r', { type: 'range', min: 0, max: 100, step: 5, 'aria-label': 'The agent\'s voice volume here',
    oninput: (e) => setSpeaker({ volume: +e.target.value / 100 }) });
  const row = h('div.vol-row.' + cls, {}, b, h('span.vr-t', {}, label, h('small.vr-s')), r, h('span.vr-v'));
  paintVol(row);
  return row;
}
function paintVol(row) {
  const b = row.querySelector('.vr-b');
  b.innerHTML = spkIcon();
  const am = agentMuted();
  b.setAttribute('aria-pressed', String(am));
  b.setAttribute('aria-label', am ? 'Unmute the agent\'s voice' : 'Mute the agent\'s voice');
  b.title = b.getAttribute('aria-label');
  const r = row.querySelector('.vr-r');
  const v = String(Math.round(spkVolNow() * 100));
  if (r.value !== v && document.activeElement !== r) r.value = v;
  r.classList.toggle('off', am);
  row.classList.toggle('muted', am);
  row.querySelector('.vr-v').textContent = am ? 'Muted' : Math.round(spkVolNow() * 100) + '%';
  row.querySelector('.vr-s').textContent = spkWhy();
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
  if (m.preview !== undefined) { onPreview(m); return; }
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

const KEYS = ['voice.tts', 'voice.tts_engine', 'voice.kokoro_voice', 'voice.grok.voice', 'voice.tts_voice', 'voice.tts_speed', 'voice.tts_volume', 'voice.talk_level',
  'voice.wake_words', 'voice.wake_sensitivity', 'voice.wake_follow_up_s', 'voice.open_mic_sleep_min', 'voice.barge_in', 'voice.barge_margin_db',
  'voice.barge_in_min_words', 'voice.barge_in_duck', 'voice.direct_reply', 'voice.direct_follow', 'voice.instant_commands', 'voice.end_silence_ms', 'voice.engine', 'voice.language', 'voice.speaker_lock'];
let settings = null;

function curTab() {
  const p = panes.focused?.();
  const uid = p?.id ? +String(p.id).replace(/^t/, '') : S.focused;
  const t = uid != null ? S.tabs.find((x) => x.uid === uid) : null;
  return t ? { uid, name: tabName(t), can: t.can_direct !== false } : null;
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
  const follow = S.voice?.direct_follow === true;
  const sig = JSON.stringify([micMuted, cap?.method, store.get('voice.method', ''), store.get('voice.force', ''), pick, flags.direct, follow, flags.brainstorm, t, mode === 'test', wakeWord()]);
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
    h('span', {}, flags.direct ? `Talking to ${flags.direct} directly` : 'Direct mode', h('small', {}, flags.direct ? (follow ? 'What you say goes to that tab, and to the tab you open next; its replies are spoken.' : 'What you say goes to that tab; its replies are spoken.') : t ? `Talk to ${t.name} without the assistant in between.` : 'Open a tab to talk to it directly.')),
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
  const pv = PREVIEW_KEYS[s.key];
  const field = pv ? h('span.pv-wrap', {}, input, previewBtn(() => pv(input.value))) : input;
  return h('label.row', {}, h('span', {}, s.label, h('small', {}, s.help ? String(s.help).split('. ')[0] : '')), field);
}
const PREVIEW_KEYS = {
  'voice.tts_engine': () => ({ engine: 'mac', voice: '' }),
  'voice.kokoro_voice': (v) => ({ engine: 'kokoro', voice: v }),
  'voice.grok.voice': (v) => ({ engine: 'grok', voice: v }),
  'voice.tts_voice': (v) => ({ engine: 'say', voice: v }),
};

let previewing = false, previewPoll = 0;
function paintPreview() {
  for (const b of document.querySelectorAll('.pv-b')) {
    b.textContent = previewing ? 'Stop' : 'Preview';
    b.setAttribute('aria-pressed', String(previewing));
    b.title = previewing ? 'Stop the sample' : 'Hear a short sample with this choice';
  }
}
function previewEnd() {
  clearInterval(previewPoll); previewPoll = 0;
  previewing = false;
  paintPreview();
}
function previewStop() {
  send({ t: 'voice', preview_stop: true });
  hush();
  previewEnd();
}
function previewStart(choice) {
  if (previewing) { previewStop(); return; }
  if (agentMuted()) { toast('The agent voice is muted, so a sample would be silent. Unmute it first (tap the speaker in the voice pill).', 'warn'); return; }
  if (!choice || !choice.engine) return;
  unlock();
  if (!send({ t: 'voice', preview_tts: choice })) { toast('Not connected', 'warn'); return; }
  previewing = true;
  paintPreview();
  clearTimeout(previewPoll);
  previewPoll = setTimeout(previewEnd, 45000);
}
function onPreview(m) {
  if (m.preview === 'error') { previewEnd(); toast(m.error || 'No sample for this choice', 'warn'); return; }
  if (m.preview === 'done') {
    clearTimeout(previewPoll);
    previewPoll = setInterval(() => { if (!previewing) { clearInterval(previewPoll); return; } if (!playing.size && !held.length) previewEnd(); }, 300);
  }
}
const previewBtn = (get) => h('button.btn.pv-b', { type: 'button', onclick: (e) => { e.preventDefault(); e.stopPropagation(); previewStart(get()); } }, previewing ? 'Stop' : 'Preview');

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
  const pcur = () => (ttsChoice() || def);
  const en = L.find((x) => x.engine === cur.engine);
  const voices = en?.voices?.length
    ? h('select.tts-v', { 'aria-label': 'Voice', onchange: (e) => { setTts({ engine: cur.engine, voice: e.target.value }); paintTts(); } },
      en.voices.map((v) => h('option', { value: v.id, selected: v.id === cur.voice }, v.label || v.id)))
    : null;
  const mac = ttsList.mac ? `The Mac speaks with ${ttsLabel(ttsList.mac)}.` : '';
  box.replaceChildren(
    h('label.row', {}, h('span', {}, 'Engine', h('small', {}, en?.note || (cur.engine === 'mac' ? 'Replies here use the Mac\'s voice setting' : 'This device only: replies spoken in this browser'))), h('span.pv-wrap', {}, engines, previewBtn(pcur))),
    voices ? h('label.row', {}, h('span', {}, 'Voice', h('small', {}, cur.engine === def.engine && cur.voice === def.voice ? 'The default for browsers' : '')), h('span.pv-wrap', {}, voices, previewBtn(pcur))) : null,
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
      h('h4', {}, 'Agent voice (shared with the Mac app)'),
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
    hm.addEventListener('click', (e) => {
      const first = hm.dataset.sphit === '1'; delete hm.dataset.sphit;
      if (agentMuted() && (first || e.target.closest('.ml-sp'))) { setSpeaker({ muted: false }); return; }
      openMicMenu(hm);
    });
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
  on('focus', () => paint());
  on('layout', placeMove);
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
    applyGain();
    for (const r of document.querySelectorAll('.vol-row')) paintVol(r);
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
  if (canPickInput()) {
    try { navigator.mediaDevices.addEventListener('devicechange', inputChanged); } catch {}
    refreshInputs();
  }

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
