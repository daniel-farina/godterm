export const METHODS = ['worklet', 'script', 'recorder'];

export function recorderType() {
  if (!window.MediaRecorder) return '';
  const want = ['audio/mp4;codecs=mp4a.40.2', 'audio/mp4', 'audio/webm;codecs=opus', 'audio/webm', 'audio/ogg;codecs=opus'];
  for (const t of want) { try { if (MediaRecorder.isTypeSupported(t)) return t; } catch {} }
  return '';
}

const wait = (p, ms) => Promise.race([p, new Promise((r) => setTimeout(r, ms))]);

const WORKLET = new URL('./mic-worklet.js', import.meta.url).href;

function newContext(stream) {
  const C = window.AudioContext || window.webkitAudioContext;
  if (!C) throw new Error('no Web Audio');

  const rate = stream.getAudioTracks()[0]?.getSettings?.().sampleRate;
  try { return rate ? new C({ sampleRate: rate }) : new C(); } catch { return new C(); }
}

function scriptNode(c, onPcm, diag) {
  const p = c.createScriptProcessor(2048, 1, 1);
  const ratio = c.sampleRate / 16000;
  let pos = 0, prev = 0, out = new Int16Array(320), n = 0, calls = 0, peak = 0;
  p.onaudioprocess = (e) => {
    const x = e.inputBuffer.getChannelData(0);
    for (let i = 0; i < x.length; i++) {
      const s = x[i];
      const m = s < 0 ? -s : s;
      if (m > peak) peak = m;
      while (pos <= 1) {
        const v = prev + (s - prev) * pos;
        out[n++] = Math.max(-1, Math.min(1, v)) * 32767;
        if (n === out.length) { onPcm(out.buffer); out = new Int16Array(320); n = 0; }
        pos += ratio;
      }
      pos -= 1;
      prev = s;
    }
    if (++calls % 6 === 0) { diag({ calls: calls * 16, empty: 0, peak }); peak = 0; }
  };
  return p;
}

export async function openCapture(method, stream, { onPcm, onChunk, onDiag, broken, log }) {
  if (method === 'recorder') return openRecorder(stream, { onChunk, log, broken });
  const ctx = newContext(stream);
  try {
    if (ctx.state !== 'running') await wait(ctx.resume().catch(() => {}), 800);
    const src = ctx.createMediaStreamSource(stream);
    const pcm = broken ? () => {} : onPcm;
    let node;
    if (method === 'worklet') {
      if (!ctx.audioWorklet || !window.AudioWorkletNode) throw new Error('no AudioWorklet here');
      await ctx.audioWorklet.addModule(WORKLET);
      node = new AudioWorkletNode(ctx, 'mic16k', { numberOfInputs: 1, numberOfOutputs: 1, outputChannelCount: [1] });
      node.port.onmessage = (e) => { if (e.data instanceof ArrayBuffer) pcm(e.data); else onDiag(e.data); };
    } else {
      node = scriptNode(ctx, pcm, onDiag);
    }
    src.connect(node);
    node.connect(ctx.destination);
    ctx.onstatechange = () => log?.(`${method} context ${ctx.state}`);
    return {
      method,
      ctx,
      what: `${method}, context ${ctx.state} ${ctx.sampleRate} Hz`,
      resume: () => (ctx.state === 'running' ? Promise.resolve() : wait(ctx.resume().catch(() => {}), 800)),
      close() {
        try { src.disconnect(); node.disconnect(); } catch {}
        if (node.port) node.port.onmessage = null;
        if (node.onaudioprocess) node.onaudioprocess = null;
        ctx.close().catch(() => {});
      },
    };
  } catch (e) {
    ctx.close().catch(() => {});
    throw e;
  }
}

function openRecorder(stream, { onChunk, log, broken }) {
  if (!window.MediaRecorder) throw new Error('no MediaRecorder here');
  const type = recorderType();
  let rec = null, chain = Promise.resolve(), ended = null, n = 0, bytes = 0;
  const cap = {
    method: 'recorder',
    mime: type,
    ctx: null,
    get what() { return `recorder, ${cap.mime || 'default type'}, ${n} pieces ${Math.round(bytes / 1024)} KB`; },
    resume: () => Promise.resolve(),
    recording: () => !!rec && rec.state === 'recording',

    start(begin) {
      if (cap.recording()) return;
      const r = new MediaRecorder(stream, type ? { mimeType: type, audioBitsPerSecond: 64000 } : undefined);
      cap.mime = r.mimeType || type || 'audio/mp4';
      rec = r;
      n = bytes = 0;
      begin(cap.mime);
      let done;
      ended = new Promise((res) => (done = res));
      r.ondataavailable = (e) => {
        if (!e.data || !e.data.size) return;

        chain = chain.then(() => e.data.arrayBuffer()).then((b) => {
          n++;
          bytes += b.byteLength;
          if (!broken) onChunk(new Uint8Array(b), cap.mime);
        }).catch((err) => log?.('recorder piece lost: ' + (err?.message || err)));
      };
      r.onstop = () => { chain.then(done); };
      r.onerror = (e) => log?.('recorder error: ' + (e?.error?.message || e?.error?.name || 'unknown'));
      r.start(250);
    },

    stop() {
      const r = rec;
      rec = null;
      if (!r || r.state === 'inactive') return Promise.resolve();
      try { r.requestData?.(); } catch {}
      try { r.stop(); } catch {}
      return wait(ended, 2000);
    },
    close() { cap.stop(); },
  };
  return cap;
}
