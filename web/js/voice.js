// Voice: this browser as GodTerm's mic and speakers, in every mode the Mac
// has: push to talk, open mic (every utterance is a command) and wake word,
// plus direct mode (talk to the tab shown) and brainstorm. The mic goes to
// GodTerm as 16 kHz mono PCM16 (binary kind 3) or, when Web Audio gives no
// samples, as a recording GodTerm decodes (kind 4); the assistant's speech
// comes back as WAV clips (kind 2), played back to back.
//
// What the user sees is the truth: the mic buttons and a voice bar show
// starting / listening / hearing you / thinking / speaking with a live mic
// level, hands free has a big clear state, and a failure stays on screen
// with what went wrong until it is dismissed or voice works again.
//
// The mic is opened by the first method of mic.js that gives sound
// (worklet, then script processor, then recorder), chosen automatically,
// logged, and remembered on this device. ?micmethod=script (or worklet,
// recorder, auto) forces one; ?micbreak=worklet,script makes those give
// nothing, to test the fallbacks.
//
// Debugging: ?voicedebug=1 (remembered, ?voicedebug=0 turns it off) logs
// the mic once a second to the console and GodTerm's log, and GodTerm logs
// what it gets; &voicetest=5 holds push to talk for 5 s after connecting.
import { $, h, on, store, toast } from './util.js';
import { send, sendBin, KIND, tool } from './conn.js';
import { S } from './state.js';
import { dlog as clog } from './debuglog.js';
import { openSheet, openMenu, closeSheet, menuOpen, sheetKind } from './sheet.js';
import { panes } from './term.js';
import { METHODS, openCapture, recorderType } from './mic.js';

const KIND_REC = 4;
const q = new URLSearchParams(location.search);

// Playback: its own context, started inside a touch (iOS).
let ctx = null, playAt = 0;
const playing = new Set();
// Clips that came while the audio output was locked (iOS): played on a tap.
let held = [];
// The agent's voice here: a gain every clip goes through (volume 0..1 and
// a speaker mute), kept per browser. Replies still come while muted.
const vol0 = +store.get('voice.spkvol', 1);
let spkVol = Number.isFinite(vol0) ? Math.max(0, Math.min(1, vol0)) : 1;
let spkMuted = store.get('voice.spkmute', false) === true;
let outGain = null;
// The mic mute: the mode stays, the capture track is disabled (the
// browser's recording indicator says so) and only silence goes to GodTerm,
// so its engine keeps its timing. Replies still play. Kept per browser.
let micMuted = store.get('voice.micmute', false) === true;

// The mic: the stream, the capture on it (mic.js) and the mode it serves:
// '' (not sending), push, open, wake, or test (the settings' mic test).
let stream = null, cap = null, mode = '';
// The phase shown for push to talk: '' starting asking listening sending
// thinking. `err` stays until dismissed or the next working start.
let phase = '', err = null, hint = '';
// The page is in the background or the phone locked: iOS stopped the mic.
let away = false, lost = '';
// Mic level (0..1) and counts for the checks and the debug log.
let level = 0, peakAt = 0, levelDb = -90;
const st = { frames: 0, nonzero: 0, sumsq: 0, n: 0, peak: 0, since: 0, started: 0, any: false, sound: false, total: 0 };
// The last few frames per second (the meter's proof that frames exist).
let fps = 0, fpsN = 0, fpsAt = 0;
// What the capture said about itself (worklet / script processor).
let diag = null;

const DEBUG_KEY = 'gt-voice-debug';
let debug = false;
// (Read now: main.js takes the query off the address.)
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
// The hands free mode the toggle starts: open or wake.
let hfMode = store.get('voice.hf', 'open') === 'wake' ? 'wake' : 'open';
// The mode picked in the mic menu (the header's mic): off, push (a Talk
// button stays on screen), open or wake (a strip under the header says
// so, with the live level). Kept on this device.
const PICKS = ['off', 'push', 'open', 'wake'];
let pick = PICKS.includes(store.get('voice.pick', 'off')) ? store.get('voice.pick', 'off') : 'off';
// When the last mic audio came (samples, or GodTerm's level of a
// recording): with none for a moment the page says "no audio", never
// "listening".
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

// iOS plays nothing until audio started inside a gesture: a silent buffer.
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

function fail(code, text) {
  err = { code, text };
  dlog('error ' + code + ': ' + text);
  paint();
}

// GodTerm's side: `voice {on, mode}` makes this browser its mic and
// speakers; push to talk then presses and releases with `voice {talk}`.
// Push to talk stays on for a while after the last press, so the reply is
// heard here, then gives voice back to the Mac.
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
  try {
    return await navigator.mediaDevices.getUserMedia({ audio: want });
  } catch (e) {
    if (e?.name !== 'OverconstrainedError' && e?.name !== 'TypeError') throw e;
    return navigator.mediaDevices.getUserMedia({ audio: true });
  }
}

const dbOf = (ss, n) => Math.max(-90, 20 * Math.log10(Math.sqrt(ss / Math.max(1, n)) / 32768 + 1e-9));

// A level for the meter (dBFS -60..0 as 0..1, fast up, slow down).
function setLevel(db) {
  levelDb = db;
  const l = Math.max(0, Math.min(1, (db + 60) / 60));
  const now = performance.now();
  level = l > level ? l : Math.max(l, level - (now - peakAt) / 900);
  peakAt = now;
  meter();
}

// Mic samples (PCM16 at 16 kHz) from the worklet or the script processor:
// counted always (the checks need them between presses), sent while a mode
// is on.
let zeros = null;
function onPcm(buf) {
  let a = new Int16Array(buf);
  // Muted: silence in place of what the mic gave (the track is disabled
  // too; this holds where disabling it does not silence it).
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

// Recorder pieces: to GodTerm (it decodes them and sends the level back).
let recPieces = 0;
function onChunk(bytes) {
  recPieces++;
  if (!st.any) { st.any = true; paint(); }
  if (mode !== 'test') sendBin(KIND_REC, 0, bytes);
}
// GodTerm's level of the decoded recording.
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

// The method order: forced, else the one that worked here last, then the rest.
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

// Open the capture by the first method that gives sound. Resolves once one
// is open (`first`), then keeps checking in the background: a method that
// gives no samples (or only zeros) in its first moment is replaced by the
// next, and the one that works is remembered.
let probing = 0, probeDone = null;
// While no method is known to work here, a recorder runs beside the first
// tries, keeping its pieces: when Web Audio gives nothing, it becomes the
// capture with everything said since the press (nothing is lost to the
// tries).
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
        // The recorder that ran beside: what it kept goes now.
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

// The background check of the method just opened.
async function probe(o) {
  if (!o) return;
  let { c, rest, run } = o;
  for (;;) {
    if (c.method === 'recorder') {
      // Its proof is GodTerm's level of the decoded audio.
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
      // Frames but only zeros: a quiet room is never exact zeros for long.
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

// A recording for GodTerm (the recorder method): starts now.
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
  // Inside the gesture: the playback context (iOS starts it only here).
  if (!audioCtx()) { mode = ''; phase = ''; fail('noaudio', 'This browser has no Web Audio.'); return false; }
  unlock();
  // Talking over a reply cuts it off (GodTerm stops it too).
  if (m === 'push' && (playing.size || held.length)) hush();
  try {
    const t0 = performance.now();
    // Still open from the last press (kept while voice is on: iOS asks for
    // the mic again on every getUserMedia in an installed app, and that
    // prompt takes the touch away).
    const live = stream && cap && stream.getAudioTracks().some((t) => t.readyState === 'live');
    if (live) {
      await cap.resume();
      if (cap.method === 'recorder') beginRec();
      const ok = began(m, t0);
      keptCheck();
      return ok;
    }
    release();
    // A prompt is up when this takes a while: say what to do.
    const ask = setTimeout(() => { if (phase === 'starting') { phase = 'asking'; paint(); } }, 700);
    try { stream = await getMic(); } finally { clearTimeout(ask); }
    if (phase === 'asking') phase = 'starting';
    if (!mode) { release(); return false; }
    watchTrack(stream);
    startShadow();
    // The capture context is made now, after the mic opened (on iOS one
    // made before can run without ever getting the mic's samples).
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

// The phone's log showed a kept mic that stayed "live" but gave nothing on
// the next press (iOS after playing the reply): after 0.6 s of nothing the
// capture is built again (a fresh context, from the same method on), after
// 1.4 s the mic itself is opened again and every method gets its try.
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

// No sound a moment after the start, after every method had its try: say so
// (the mic is muted by the system, another app has it).
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

// iOS stops the mic in the background or when the screen locks: the track
// ends or mutes. Shown, and resumed when the app is back.
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
  // Hands free: the mic again (it may need a tap: then the bar asks).
  probing++;
  mode = '';
  release();
  if (!(await start(m))) { hint = ''; lost = 'tap'; resumeMode = m; err = null; paint(); }
}
let resumeMode = '';

// The mic itself closes (the browser's mic indicator goes off): when voice
// goes off here, a failure, or the connection drops.
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
    // A little more audio after the release, so the last word is not cut.
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
    // (Once the method is settled: a recorder that took over ends its file.)
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

// After a release: "thinking" until GodTerm shows it transcribing or the
// assistant busy, then until both are done (or a few seconds of neither).
let thinkingUntil = 0;
// Hands free: what was heard last, shown for a moment.
let heardText = '', heardUntil = 0, lastHeard = null;

function shown() {
  const v = S.voice || {};
  if (lost && mode) return 'paused';
  if (lost === 'tap') return 'paused';
  if (playing.size) return 'speaking';
  if (held.length) return 'locked';
  if (mode === 'push') return phase === 'listening' && noAudio() ? 'noaudio' : phase === 'listening' && !st.any ? 'starting' : phase;
  if (mode === 'test') return 'test';
  const busy = serverOn && (v.status === 'transcribing' || S.assistant?.busy);
  if (handsFree()) {
    if (phase === 'starting' || phase === 'asking') return phase;
    if (noAudio()) return 'noaudio';
    if (micMuted) return 'muted';
    // Nothing from the mic yet: not "listening" until it is.
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
    h('button.vb-set', { type: 'button', 'aria-label': 'Voice settings', html: SET_ICON }),
    h('button.vb-x', { type: 'button', 'aria-label': 'Dismiss', html: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 6l12 12M18 6L6 18"/></svg>' }));
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

// The level as text: "-42 dB" (dBFS, held and falling back as the bar
// does), or "no audio" while none comes.
const dbText = () => (noAudio() ? 'no audio' : !st.any && !audioAt ? '' : `${Math.max(-90, Math.round(Math.max(levelDb, level > 0.005 ? level * 60 - 60 : -90)))} dB`);
let dbAt = 0;
function meter() {
  const s = `scaleX(${mode ? Math.max(0.02, level) : 0})`;
  for (const sel of ['#voice-bar .vb-meter i', '#mic-strip .ms-meter i', '#talk .tk-meter i', '.vs .vs-live .m i']) {
    const m = document.querySelector(sel);
    if (m) m.style.transform = s;
  }
  // The numbers a few times a second (readable, and cheap).
  const now = performance.now();
  if (now - dbAt < 120) return;
  dbAt = now;
  const t = mode ? dbText() : '';
  for (const sel of ['#voice-bar .vb-db', '#mic-strip .ms-db', '#talk .tk-db']) {
    const el = document.querySelector(sel);
    if (el && el.textContent !== t) el.textContent = t;
  }
}

const TEXT = {
  starting: 'Starting the mic...',
  asking: 'Allow the microphone in the prompt',
  listening: 'Listening',
  hearing: 'Hearing you',
  heard: 'Heard you',
  sending: 'Sending',
  thinking: 'Thinking',
  speaking: 'Speaking',
  waiting: 'Waiting for the wake word',
  locked: 'A reply is waiting',
  paused: 'Mic paused',
  test: 'Mic test',
  noaudio: 'No audio from the mic',
  muted: 'Mic muted',
};

const wakeWord = () => S.voice?.wake_word || 'hey god';
// "hey god" -> "Hey God", for labels.
const wakeLabel = () => wakeWord().replace(/\b\w/g, (c) => c.toUpperCase());
const modeName = (m) => ({ push: 'Push to talk', open: 'Open mic', wake: 'Wake word', off: 'Voice off' }[m] || '');

// The mic icon for each mode (the header's mic shows the one picked).
const MIC = '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3"/>';
const ICON = {
  off: MIC + '<path d="M4 3l16 18"/>',
  push: '<rect x="9" y="3" width="6" height="11" rx="3"/><path d="M5 11a7 7 0 0 0 14 0M12 18v3M8.5 21h7"/>',
  open: MIC + '<path d="M2 8v6M22 8v6"/>',
  wake: MIC + '<path d="M2.5 6.5l1.5 1.5M21.5 6.5L20 8"/>',
};
// The mic mute and the agent's voice here (speaker, quieter, muted).
const MIC_OFF = ICON.off;
const SPK = '<path d="M4 9.5v5h3.5L12 18V6L7.5 9.5H4z"/>';
const spkIcon = () => svg(SPK + (spkMuted || spkVol === 0 ? '<path d="M16 9.5l5 5M21 9.5l-5 5"/>' : spkVol < 0.5 ? '<path d="M15.5 9.5a3.5 3.5 0 0 1 0 5"/>' : '<path d="M15.5 9.5a3.5 3.5 0 0 1 0 5M18 7a7 7 0 0 1 0 10"/>'));
const spkWord = () => (spkMuted ? 'muted' : Math.round(spkVol * 100) + '%');
// The header's mic says the mode in a word (the whole name on a desktop).
const SHORT = { off: 'Off', push: 'PTT', open: 'Open', wake: 'Wake' };
const svg = (inner) => `<svg viewBox="0 0 24 24" aria-hidden="true">${inner}</svg>`;
const CHECK = svg('<path d="M5 12.5l4.5 4.5L19 7.5"/>');

// The header's mic: the mode at a glance (icon, a short badge, a color).
let headPick = null;
function paintHeader(state) {
  for (const id of ['#b-ptt', '#b-ptt2']) {
    const el = $(id);
    if (!el) continue;
    // Muted: the slashed mic in clay, the mode's word kept.
    const mu = micMuted && pick !== 'off';
    if (el.dataset.mode !== pick || headPick !== pick + mu) {
      el.dataset.mode = pick;
      el.innerHTML = svg(mu ? MIC_OFF : ICON[pick]) + (id === '#b-ptt' ? `<span class="ml-s">${mu ? 'Muted' : SHORT[pick]}</span><span class="ml-l">${modeName(pick)}${mu ? ' · muted' : ''}</span>` : '');
    }
    el.classList.toggle('muted', mu);
    const talking = mode === 'push' && (state === 'listening' || state === 'noaudio');
    el.classList.toggle('talking', talking);
    if (state && state !== 'paused') el.dataset.voice = state; else delete el.dataset.voice;
    const what = pick === 'push' && id === '#b-ptt2' ? (mu ? 'The mic is muted' : mode === 'push' ? 'Listening: release to send' : 'Hold to talk') : `Voice: ${pick === 'off' ? 'off' : modeName(pick) + (mu ? ', mic muted' : '')}. Tap to change`;
    el.setAttribute('aria-label', what);
    el.title = what;
  }
  headPick = pick + (micMuted && pick !== 'off');
}

// The strip under the header while open mic or wake word is picked: the
// mode, what it does now, the live level in dB, and a tap to change.
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
    h('button.ms-chg', { type: 'button', 'aria-label': 'Change the mic mode', html: svg('<path d="M6 9l6 6 6-6"/>') }));
  strip.addEventListener('click', (e) => {
    if (e.target.closest('.ms-mute')) { e.stopPropagation(); setMicMuted(!micMuted); return; }
    if (e.target.closest('.ms-spk')) { e.stopPropagation(); setSpeaker({ muted: !spkMuted }); return; }
    const a = e.target.closest('.ms-act');
    if (a) {
      e.stopPropagation();
      if (a.dataset.act === 'stop') { hush(); send({ t: 'voice', hush: true }); } else if (a.dataset.act === 'start') { const m = resumeMode || pick; lost = ''; resumeMode = ''; err = null; start(m); }
      paint();
      return;
    }
    openMicMenu(strip);
  });
  // A wide screen keeps it in the actions row, at the right next to the
  // mic button; narrower ones show it as a strip under the header.
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
  if (err) { state = 'error'; what = 'not listening'; sub = err.text; act = 'start'; }
  else if (!handsFree()) { state = 'paused'; what = lost === 'tap' ? 'paused' : 'not started'; sub = lost === 'tap' ? 'The phone stopped the mic in the background' : 'Tap Start to open the mic'; act = 'start'; }
  else if (s === 'paused') { what = 'paused'; sub = away ? 'Back when you return to GodTerm' : 'The phone stopped the mic (a call, the screen lock, another app)'; }
  else if (s === 'noaudio') what = 'no audio';
  else if (micMuted) { state = 'muted'; what = s === 'speaking' ? 'muted · speaking' : 'muted'; sub = 'Nothing is sent; replies still play'; if (s === 'speaking') act = 'stop'; }
  else if (s === 'waiting') what = `say "${wakeLabel()}"`;
  else if (s === 'listening') what = mode === 'wake' ? 'listening for your request' : 'listening';
  else if (s === 'heard') { what = 'heard you'; if (heardText) sub = '"' + heardText + '"'; }
  else if (s === 'speaking') { what = 'speaking'; act = 'stop'; }
  else what = (TEXT[s] || s || 'listening').toLowerCase();
  el.dataset.state = state || 'idle';
  el.dataset.mode = m;
  const ic = el.querySelector('.ms-ic');
  if (ic.dataset.m !== m) { ic.dataset.m = m; ic.innerHTML = svg(ICON[m] || MIC); }
  el.querySelector('.ms-t').textContent = `${modeName(m)} · ${what}`;
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
  a.textContent = { stop: 'Stop', start: err ? 'Retry' : lost === 'tap' ? 'Resume' : 'Start' }[act] || '';
}

// Push to talk: a big Talk button stays on screen (hold it while you
// talk), with its state and the level while held.
let talk = null;
function ensureTalk() {
  if (talk) return talk;
  talk = h('button', { id: 'talk', type: 'button', hidden: true, 'aria-label': 'Hold to talk' },
    h('span.tk-ic', { html: svg(MIC) }), h('span.tk-l', {}, 'Talk'),
    h('span.tk-lv', {}, h('span.tk-meter', {}, h('i')), h('span.tk-db')));
  ptt(talk);
  ($('#panes') || $('#main')).append(talk);
  return talk;
}

function paintTalk(s) {
  const t = ensureTalk();
  const on = pick === 'push';
  if (t.hidden === on) t.hidden = !on;
  document.body.classList.toggle('ptt', on);
  if (!on) return;
  const st2 = err ? 'error' : mode === 'push' ? s || 'listening' : s === 'sending' || s === 'thinking' || s === 'speaking' ? s : 'idle';
  t.dataset.state = st2;
  const label = { idle: 'Talk', starting: 'Starting', asking: 'Allow the mic', listening: 'Release to send', noaudio: 'No audio', sending: 'Sending', thinking: 'Talk', speaking: 'Talk', error: 'Talk' }[st2] || 'Talk';
  const l = t.querySelector('.tk-l');
  if (l.textContent !== label) l.textContent = label;
  t.querySelector('.tk-lv').hidden = mode !== 'push';
  t.setAttribute('aria-pressed', String(mode === 'push'));
}

// The mic menu: the four modes (the current one checked), direct mode for
// the tab shown, and the voice settings.
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
    item('off', 'Off', 'The mic stays closed'),
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
    om.textContent = pick === 'off' ? 'Voice off' : modeName(pick);
  }
  paintStrip(s);
  paintTalk(s);
  const here = serverOn && S.voice?.web === S.client;
  const engineErr = here && S.voice?.status === 'off' && S.voice?.error;
  const note = $('#voice-note');
  if (note) note.textContent = err ? 'voice failed' : !secure() ? 'mic needs HTTPS' : TEXT[s]?.toLowerCase() || (serverOn ? 'voice here' : '');
  const b = ensureBar();
  let text = '', sub = '', act = '';
  const flags = S.mode_flags || {};
  const extra = [flags.direct ? 'talking to ' + flags.direct : '', flags.brainstorm ? 'brainstorm' : ''].filter(Boolean).join(' · ');
  if (err) text = err.text;
  else if (engineErr) text = 'GodTerm\'s voice engine stopped: ' + S.voice.error;
  else if (s === 'paused') {
    text = 'Mic paused';
    sub = lost === 'tap' ? 'The phone stopped the mic in the background. Tap Resume.' : away ? 'GodTerm is in the background; the mic comes back when you return' : 'The phone stopped the mic (a call, the screen lock, another app)';
    if (lost === 'tap') act = 'resume';
  } else if (s === 'listening') text = 'Listening: release to send';
  else if (s === 'noaudio') { text = 'No audio from the mic'; sub = 'Nothing reaches GodTerm: release and hold again'; }
  else if (s === 'locked') { text = 'A reply is waiting'; act = 'play'; }
  else if (s === 'speaking') { text = 'Speaking'; act = 'stop'; sub = extra; }
  else if (s) { text = TEXT[s]; sub = extra; }
  else if (hint) text = hint;
  // Open mic and wake word say it all in the strip; the floating bar is
  // for push to talk, a reply waiting, and failures.
  const inStrip = (pick === 'open' || pick === 'wake') && !engineErr && s !== 'locked';
  b.hidden = !text || inStrip || sheetKind === 'voice';
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

// Hold to talk on `btn` (the Talk button, the chat's mic while push to
// talk is picked). The chat's mic opens the mic menu in the other modes.
function ptt(btn, menuElse = false) {
  if (!btn) return;
  let tap = false;
  btn.addEventListener('pointerdown', async (e) => {
    e.preventDefault();
    tap = false;
    if (menuElse && pick !== 'push') { tap = true; return; }
    if (handsFree()) {
      // Hands free already listens: a press while it speaks cuts in.
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
      // Released while the mic was starting (a permission prompt takes
      // the touch away): nothing was said yet.
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
  // No text selection or callout on a long press (iOS).
  btn.addEventListener('touchstart', (e) => e.preventDefault(), { passive: false });
}

// The mode picked in the mic menu (or the voice sheet, Settings).
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
    if (m === 'off' && serverOn) voiceOff();
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

// The mic mute (the strip's mic button, the menu, M, the voice sheet).
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

// The agent's voice here: volume (0..1) and mute, applied to the gain.
const spkGain = () => (spkMuted ? 0 : spkVol);
export function setSpeaker({ volume, muted } = {}) {
  if (volume != null && Number.isFinite(+volume)) {
    spkVol = Math.max(0, Math.min(1, +volume));
    store.set('voice.spkvol', spkVol);
    // Turning it up from muted means hearing it.
    if (spkMuted && spkVol > 0 && muted == null) { spkMuted = false; store.set('voice.spkmute', false); }
  }
  if (muted != null) { spkMuted = !!muted; store.set('voice.spkmute', spkMuted); }
  if (outGain && ctx) {
    try { outGain.gain.setTargetAtTime(spkGain(), ctx.currentTime, 0.015); } catch { outGain.gain.value = spkGain(); }
  }
  for (const r of document.querySelectorAll('.vol-row')) paintVol(r);
  paint();
}
// A volume row: the speaker (a tap mutes) and a slider, live.
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

// Speech from GodTerm: WAV clips (PCM16 mono), played back to back in the
// order they came, with no gap. One log line per reply.
let chain = Promise.resolve();
const reply = { clips: 0, secs: 0, late: 0, t0: 0, quietT: 0 };
function enqueue(bytes) {
  chain = chain.then(() => play(bytes)).catch((e) => dlog('play failed: ' + (e?.message || e)));
}

// A WAV clip read directly (no decodeAudioData: in order and at once).
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

async function play(bytes) {
  const c = audioCtx();
  if (!c) return;
  if (c.state !== 'running') {
    await wait(c.resume().catch(() => {}), 300);
    if (c.state !== 'running') {
      // Locked (iOS without a recent tap): wait for a tap on Play.
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
  if (!buf.duration) return;
  const s = c.createBufferSource();
  s.buffer = buf;
  s.connect(outGain || c.destination);
  const now = c.currentTime;
  if (!playing.size || playAt < now) {
    // The first clip, or the next one came after the last ended (late).
    if (playing.size || reply.clips) reply.late++;
    playAt = now + 0.03;
  }
  s.start(playAt);
  playAt += buf.duration;
  playing.add(s);
  if (!reply.clips) reply.t0 = performance.now();
  reply.clips++;
  reply.secs += buf.duration;
  clearTimeout(reply.quietT);
  s.onended = () => {
    playing.delete(s);
    if (!playing.size) reply.quietT = setTimeout(replyDone, 700);
    paint();
  };
  paint();
  if (serverOn === 'push' && !mode) voiceOffSoon();
}

function replyDone() {
  if (playing.size || !reply.clips) return;
  dlog(`reply played: ${reply.clips} clips, ${reply.secs.toFixed(1)} s${reply.late > 1 ? `, ${reply.late - 1} late (a gap)` : ''}`);
  Object.assign(reply, { clips: 0, secs: 0, late: 0 });
}

// The reply was cut off: drop what is queued.
function hush() {
  for (const s of playing) { try { s.onended = null; s.stop(); } catch {} }
  if (playing.size) dlog(`reply stopped after ${reply.clips} clips`);
  playing.clear();
  held = [];
  playAt = 0;
  Object.assign(reply, { clips: 0, secs: 0, late: 0 });
  paint();
}

// GodTerm's answer to a `voice` frame.
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
    fail('server', 'GodTerm refused voice: ' + (m.error || 'no reason given'));
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

// --- The voice sheet: mode, direct, brainstorm, the mic and settings.

// Settings that make sense from a phone (the rest are the Mac's devices).
const KEYS = ['voice.tts', 'voice.tts_engine', 'voice.kokoro_voice', 'voice.grok.voice', 'voice.tts_voice', 'voice.tts_speed', 'voice.tts_volume',
  'voice.wake_words', 'voice.wake_sensitivity', 'voice.wake_follow_up_s', 'voice.open_mic_sleep_min', 'voice.barge_in', 'voice.barge_in_min_words',
  'voice.direct_reply', 'voice.instant_commands', 'voice.end_silence_ms', 'voice.engine', 'voice.language', 'voice.speaker_lock'];
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
    segBtn('Off', pick === 'off', () => setMode('off')),
    segBtn('Push to talk', pick === 'push', () => setMode('push')),
    segBtn('Open mic', pick === 'open', () => setMode('open')),
    segBtn('Wake word', pick === 'wake', () => setMode('wake')));
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
  // Saved by GodTerm at once (the user's own change, as in its Settings).
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

// The voice replies to this browser are spoken in (GodTerm's Kokoro
// af_heart unless picked here), kept on this device and sent to GodTerm
// on every connect. The Mac's own voice is its setting below.
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
// The voice choice for this browser, as a box for a sheet (the voice
// sheet, Settings). Filled once GodTerm lists the voices.
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
  if (!L) { box.replaceChildren(h('p.note', {}, 'Loading the voices...')); return; }
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
      // Reopen the mic with that method.
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
  // The header's mic: a tap opens the mic menu (the mode, direct, the
  // settings). A tap on it while the menu is open closes it.
  const hm = $('#b-ptt');
  if (hm) {
    addEventListener('pointerdown', (e) => { if (menuOpen() && menuAnchor && menuAnchor.contains(e.target)) menuSkip = true; }, true);
    hm.addEventListener('click', () => openMicMenu(hm));
  }
  // The chat's mic: hold to talk in push to talk, else the menu.
  ptt($('#b-ptt2'), true);
  const om = $('#b-openmic');
  if (om) om.addEventListener('click', () => openMicMenu(om));
  $('#b-voice')?.addEventListener('click', openVoiceSheet);
  // M mutes or unmutes the mic while a mode is on, unless typing (a text
  // field, the terminal, the chat).
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
  on('msg:voice', onVoice);
  on('msg:voice_level', onRecLevel);
  on('open-voice', openVoiceSheet);
  on('sheet', () => paint());
  on('conn', (s) => {
    if (s === 'live') sendTts();
    if (s === 'live' && debug) {
      send({ t: 'voice', debug: true });
      // A test with no touch (the iOS Simulator): ?voicedebug=1&voicetest=5
      // holds push to talk for 5 s once connected.
      const n = voiceTest;
      voiceTest = 0;
      if (n > 0 && !mode) setTimeout(async () => { if (await start('push')) setTimeout(stop, n * 1000); }, 1500);
    }
    if (s === 'live' && handsFree() && !serverOn) {
      // Back after a reconnect: GodTerm has no voice from here yet.
      const m = mode;
      voiceOn(m);
    }
    if (s !== 'live') {
      // The socket dropped: hands free keeps its mic for the reconnect.
      if (!handsFree()) { mode = ''; release(); phase = ''; }
      serverOn = '';
      hush();
    }
    paint();
  });
  // GodTerm says whose voice it is (another browser may have taken it).
  on('state', () => {
    const v = S.voice || {};
    if (serverOn && v.web != null && v.web !== S.client) { serverOn = ''; mode = ''; release(); phase = ''; }
    if (v.heard && v.heard !== lastHeard) {
      if (lastHeard !== null) { heardText = v.heard; heardUntil = performance.now() + 2500; }
      lastHeard = v.heard;
    }
    paint();
  });
  // iOS stops the mic in the background and when the screen locks.
  document.addEventListener('visibilitychange', () => {
    if (document.hidden) {
      if (mode || serverOn) { away = true; if (serverOn) send({ t: 'voice', away: true }); dlog('to the background'); paint(); }
    } else backAgain();
  });
  addEventListener('pageshow', () => { if (!document.hidden) backAgain(); });
  if (!secure()) for (const b of ['#b-openmic', '#b-ptt', '#b-ptt2']) $(b)?.classList.add('off');
  // Open mic or wake word picked before: the strip offers Start (the mic
  // and audio need a tap on iOS).
  if ((pick === 'open' || pick === 'wake') && !mode) resumeMode = pick;
  // Unlock audio output on the first touch (iOS needs a gesture), and
  // again on any tap while a reply waits.
  addEventListener('pointerdown', () => { if (!ctx || ctx.state !== 'running' || held.length) unlock(); }, { capture: true });
  // The level falls back while nothing comes (the recorder's is GodTerm's).
  setInterval(() => {
    if (!mode) return;
    const quiet = cap?.method === 'recorder' ? performance.now() - recLevelAt > 400 : performance.now() - peakAt > 300;
    if (quiet && level > 0) { setLevel(-90); }
    if (handsFree()) paint();
  }, 250);
  paint();
}
