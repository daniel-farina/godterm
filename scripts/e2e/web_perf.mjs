// GodTerm Web performance harness (Playwright for Node), the numbers in
// docs/WEB_APP.md "Performance": a cold load, a warm reload through the
// service worker, key latency, a reconnect, idle and streaming frame
// rates, a flood, resizes, and a soak with memory samples.
//
//   PW_DIR=<node_modules with playwright> node scripts/e2e/web_perf.mjs \
//     --url 'http://127.0.0.1:7901/?t=TOKEN' [--engine chromium|webkit]
//     [--device desktop|phone] [--throttle] [--stream S] [--flood MB]
//     [--soak MIN --panes 4] [--nogl] [--label name] [--out file.json]
//
// The instance: a demo or the matrix's fake one (scripts/e2e/web_matrix.py
// --serve --instance fake), never the live GodTerm. --throttle is Chrome's
// Fast 3G with a 4x CPU slowdown (Chromium only; so are the CDP metrics).
// One headless browser at a time.
import path from 'node:path';
import fs from 'node:fs';
const PW = process.env.PW_DIR || path.join(path.dirname(new URL(import.meta.url).pathname), '..', '..', 'web', 'dev', 'node_modules');
const { chromium, webkit, devices } = await import(path.join(PW, 'playwright', 'index.mjs'));
const args = Object.fromEntries(process.argv.slice(2).map((a, i, all) => a.startsWith('--') ? [a.slice(2), all[i + 1]?.startsWith('--') || all[i + 1] == null ? true : all[i + 1]] : []).filter((x) => x.length));
const URL0 = args.url;
if (!URL0) { console.error('need --url'); process.exit(2); }
const ENGINE = args.engine || 'chromium';
const DEVICE = args.device || 'desktop';
const THROTTLE = !!args.throttle;
const LABEL = args.label || 'run';
const KEYS = +(args.keys || 30);
const STREAM = +(args.stream || 0);
const FLOOD = +(args.flood || 0);
const SOAK = +(args.soak || 0);
const PANES = +(args.panes || 1);
const NOGL = !!args.nogl;
const out = { label: LABEL, engine: ENGINE, device: DEVICE, throttle: THROTTLE, at: new Date().toISOString() };

const INIT = `
(() => {
  const P = window.__gtPerf = { longtasks: [], keydownAt: 0, keyLat: [], ws: { in: 0, inMsgs: 0, byType: {}, bin: 0, binMsgs: 0, out: 0, outMsgs: 0 }, firstFrame: 0, firstSnapshot: 0, hello: 0, sends: [], layouts: [], resizeSentAt: 0, fps: null, firstOutput: 0 };
  try {
    new PerformanceObserver((l) => { for (const e of l.getEntries()) P.longtasks.push({ s: e.startTime, d: e.duration, n: e.attribution?.[0]?.name || '' }); }).observe({ type: 'longtask', buffered: true });
  } catch {}
  addEventListener('keydown', () => { P.keydownAt = performance.now(); }, true);
  const send = WebSocket.prototype.send;
  WebSocket.prototype.send = function (d) {
    const now = performance.now();
    if (typeof d === 'string') { P.ws.out += d.length; P.ws.outMsgs++; if (d.startsWith('{"t":"resize"')) P.resizeSentAt = now; }
    else { P.ws.out += d.byteLength; P.ws.outMsgs++; if (P.keydownAt) { P.keyLat.push(now - P.keydownAt); P.keydownAt = 0; } }
    return send.call(this, d);
  };
  const desc = Object.getOwnPropertyDescriptor(WebSocket.prototype, 'onmessage');
  Object.defineProperty(WebSocket.prototype, 'onmessage', { configurable: true, get() { return desc.get.call(this); }, set(fn) {
    desc.set.call(this, (e) => {
      const now = performance.now();
      if (typeof e.data === 'string') {
        P.ws.in += e.data.length; P.ws.inMsgs++;
        const i = e.data.lastIndexOf('"t":"');
        const t = i >= 0 ? e.data.slice(i + 5, e.data.indexOf('"', i + 5)) : '?';
        const b = (P.ws.byType[t] ||= { n: 0, bytes: 0, max: 0 });
        b.n++; b.bytes += e.data.length; if (e.data.length > b.max) b.max = e.data.length;
        if (t === 'hello' && !P.hello) P.hello = now;
        if (t === 'snapshot' && !P.firstSnapshot) P.firstSnapshot = now;
      } else { P.ws.bin += e.data.byteLength; P.ws.binMsgs++; if (!P.firstOutput) P.firstOutput = now; }
      return fn(e);
    });
  } });
  // The first pane's "Loading..." overlay goes once xterm has drawn the snapshot.
  new MutationObserver((ms) => {
    if (P.firstFrame) return;
    for (const m of ms) if (m.target.classList?.contains('pane-msg') && m.target.hidden) { P.firstFrame = performance.now(); break; }
  }).observe(document, { subtree: true, attributes: true, attributeFilter: ['hidden'] });
})();
`;

function ctxOptions() {
  if (DEVICE === 'phone') return { viewport: { width: 430, height: 932 }, deviceScaleFactor: 3, isMobile: true, hasTouch: true, userAgent: devices['iPhone 14 Pro Max'].userAgent };
  return { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 };
}

async function cdp(page) {
  if (ENGINE !== 'chromium') return null;
  const s = await page.context().newCDPSession(page);
  await s.send('Performance.enable');
  if (THROTTLE) {
    await s.send('Network.enable');
    await s.send('Network.emulateNetworkConditions', { offline: false, latency: 562.5, downloadThroughput: 1.6 * 1024 * 1024 / 8 * 0.9, uploadThroughput: 750 * 1024 / 8 * 0.9 });
    await s.send('Emulation.setCPUThrottlingRate', { rate: 4 });
  }
  return s;
}

async function metrics(s) {
  if (!s) return {};
  const r = await s.send('Performance.getMetrics');
  const m = Object.fromEntries(r.metrics.map((x) => [x.name, x.value]));
  return { heapMB: +(m.JSHeapUsedSize / 1048576).toFixed(1), heapTotalMB: +(m.JSHeapTotalSize / 1048576).toFixed(1), nodes: m.Nodes, listeners: m.JSEventListeners, scriptS: +m.ScriptDuration.toFixed(3), layoutS: +m.LayoutDuration.toFixed(3), styleS: +m.RecalcStyleDuration.toFixed(3), taskS: +m.TaskDuration.toFixed(3), docs: m.Documents, frames: m.Frames, layouts: m.LayoutCount, styles: m.RecalcStyleCount };
}

async function loadStats(page) {
  return page.evaluate(() => {
    const P = window.__gtPerf;
    const nav = performance.getEntriesByType('navigation')[0] || {};
    const paints = Object.fromEntries(performance.getEntriesByType('paint').map((p) => [p.name, +p.startTime.toFixed(0)]));
    const res = performance.getEntriesByType('resource');
    const sum = (k) => res.reduce((a, r) => a + (r[k] || 0), 0);
    const by = {};
    for (const r of res) {
      const ext = (new URL(r.name).pathname.split('.').pop() || '?');
      const b = (by[ext] ||= { n: 0, transfer: 0, encoded: 0, decoded: 0, sw: 0, cache: 0 });
      b.n++; b.transfer += r.transferSize || 0; b.encoded += r.encodedBodySize || 0; b.decoded += r.decodedBodySize || 0;
      if (r.workerStart > 0) b.sw++;
      if (r.transferSize === 0 && r.decodedBodySize > 0) b.cache++;
    }
    const files = res.map((r) => ({ f: new URL(r.name).pathname.replace(/^\//, ''), t: r.transferSize, e: r.encodedBodySize, d: r.decodedBodySize, start: +r.startTime.toFixed(0), end: +r.responseEnd.toFixed(0), sw: r.workerStart > 0 }));
    return {
      ttfb: +(nav.responseStart || 0).toFixed(0), domInteractive: +(nav.domInteractive || 0).toFixed(0), dcl: +(nav.domContentLoadedEventEnd || 0).toFixed(0), load: +(nav.loadEventEnd || 0).toFixed(0),
      fp: paints['first-paint'], fcp: paints['first-contentful-paint'],
      hello: +P.hello.toFixed(0), firstSnapshot: +P.firstSnapshot.toFixed(0), firstFrame: +P.firstFrame.toFixed(0), firstOutput: +P.firstOutput.toFixed(0),
      requests: res.length, transferKB: +(sum('transferSize') / 1024).toFixed(1), encodedKB: +(sum('encodedBodySize') / 1024).toFixed(1), decodedKB: +(sum('decodedBodySize') / 1024).toFixed(1),
      byExt: by, files, longtasks: P.longtasks.length, longtaskMs: +P.longtasks.reduce((a, t) => a + t.d, 0).toFixed(0), longtaskMax: +Math.max(0, ...P.longtasks.map((t) => t.d)).toFixed(0),
      ws: P.ws, renderer: window.GT?.panes?.list?.[0]?.renderer, build: window.GT?.S?.build,
    };
  });
}

async function waitFirstFrame(page, ms = 30000) {
  const t0 = Date.now();
  while (Date.now() - t0 < ms) {
    const ok = await page.evaluate(() => !!window.__gtPerf?.firstFrame).catch(() => false);
    if (ok) return true;
    await page.waitForTimeout(25);
  }
  return false;
}

async function typeLine(page, text) {
  await page.evaluate(() => { const p = window.GT.panes.focused(); p.term.focus(); });
  for (const ch of text) await page.keyboard.type(ch, { delay: 5 });
  await page.keyboard.press('Enter');
}

async function keyLatency(page, n) {
  await page.evaluate(() => { window.__gtPerf.keyLat = []; window.GT.panes.focused().term.focus(); });
  for (let i = 0; i < n; i++) { await page.keyboard.press('a'); await page.waitForTimeout(40); }
  const lat = await page.evaluate(() => window.__gtPerf.keyLat.slice());
  lat.sort((a, b) => a - b);
  const q = (p) => +(lat[Math.min(lat.length - 1, Math.floor(p * lat.length))] || 0).toFixed(2);
  // Clear the typed text.
  for (let i = 0; i < n; i++) await page.keyboard.press('Backspace');
  return { n: lat.length, p50: q(0.5), p90: q(0.9), max: +(lat[lat.length - 1] || 0).toFixed(2) };
}

async function fps(page, ms) {
  return page.evaluate((ms) => new Promise((res) => {
    let n = 0, worst = 0, last = performance.now(); const t0 = last;
    const lt0 = window.__gtPerf.longtasks.length;
    const step = (t) => { n++; worst = Math.max(worst, t - last); last = t; if (t - t0 < ms) requestAnimationFrame(step); else res({ fps: +(n * 1000 / (t - t0)).toFixed(1), worstFrameMs: +worst.toFixed(0), longtasks: window.__gtPerf.longtasks.length - lt0, longtaskMs: +window.__gtPerf.longtasks.slice(lt0).reduce((a, x) => a + x.d, 0).toFixed(0) }); };
    requestAnimationFrame(step);
  }), ms);
}

async function resizeCost(page) {
  if (DEVICE !== 'desktop') return null;
  await page.evaluate(() => {
    const P = window.__gtPerf; P.layouts = [];
    for (const p of window.GT.panes.list) { if (p.__wrapped) continue; p.__wrapped = true; const o = p.layout.bind(p); p.layout = () => { const t = performance.now(); o(); P.layouts.push(performance.now() - t); }; }
  });
  const sizes = [[1200, 800], [1440, 900], [1000, 700], [1440, 900]];
  const res = [];
  for (const [w, h] of sizes) {
    await page.evaluate(() => { window.__gtPerf.layouts = []; window.__gtPerf.resizeSentAt = 0; window.__gtPerf.t0 = performance.now(); window.__gtPerf.lt0 = window.__gtPerf.longtasks.length; });
    await page.setViewportSize({ width: w, height: h });
    await page.waitForTimeout(700);
    res.push(await page.evaluate(() => { const P = window.__gtPerf; return { layouts: P.layouts.length, layoutMs: +P.layouts.reduce((a, b) => a + b, 0).toFixed(1), layoutMax: +Math.max(0, ...P.layouts).toFixed(1), sentAfterMs: P.resizeSentAt ? +(P.resizeSentAt - P.t0).toFixed(0) : null, longtasks: P.longtasks.length - P.lt0 }; }));
  }
  return res;
}

async function main() {
  const bt = ENGINE === 'webkit' ? webkit : chromium;
  const browser = await bt.launch(ENGINE === 'chromium' ? { channel: 'chromium-headless-shell' } : {});
  const ctx = await browser.newContext(ctxOptions());
  await ctx.addInitScript(INIT);
  if (NOGL) await ctx.addInitScript(() => { try { localStorage.setItem('gt.webgl', 'false'); } catch {} });
  const page = await ctx.newPage();
  page.on('pageerror', (e) => (out.errors ||= []).push(String(e)));
  const s = await cdp(page);
  const t0 = Date.now();
  await page.goto(URL0, { waitUntil: 'commit' });
  const gotFrame = await waitFirstFrame(page, 60000);
  await page.waitForTimeout(500);
  out.cold = await loadStats(page);
  out.cold.gotFrame = gotFrame;
  out.cold.wall = Date.now() - t0;
  Object.assign(out.cold, { m: await metrics(s) });
  // The service worker: give it a moment to install and precache.
  await page.waitForTimeout(THROTTLE ? 8000 : 2500);
  out.cold.sw = await page.evaluate(async () => { const r = await navigator.serviceWorker?.getRegistration(); return r ? (r.active ? 'active' : r.installing ? 'installing' : r.waiting ? 'waiting' : 'none') : 'no-sw'; });
  // Warm reload (service worker and HTTP cache).
  const t1 = Date.now();
  await page.reload({ waitUntil: 'commit' });
  const gotFrame2 = await waitFirstFrame(page, 60000);
  await page.waitForTimeout(500);
  out.warm = await loadStats(page);
  out.warm.gotFrame = gotFrame2;
  out.warm.wall = Date.now() - t1;
  Object.assign(out.warm, { m: await metrics(s) });
  out.warm.controlled = await page.evaluate(() => !!navigator.serviceWorker?.controller);
  // Input latency.
  if (gotFrame2) out.keys = await keyLatency(page, KEYS);
  // Reconnect after a dropped socket (as after sleep): time until the pane is live again, by a resume or a snapshot.
  if (gotFrame2) {
    out.reconnect = [];
    for (let i = 0; i < 3; i++) {
      const r = await page.evaluate(() => new Promise((res) => {
        const P = window.__gtPerf, G = window.GT, p = G.panes.list[0];
        const r0 = P.ws.byType.resume?.n || 0, s0 = P.ws.byType.snapshot?.n || 0, t0 = performance.now();
        G.conn.drop();
        const tick = () => { if (p.ready && G.conn.open) res({ ms: +(performance.now() - t0).toFixed(0), resumes: (P.ws.byType.resume?.n || 0) - r0, snapshots: (P.ws.byType.snapshot?.n || 0) - s0 }); else if (performance.now() - t0 > 15000) res({ ms: -1 }); else setTimeout(tick, 5); };
        setTimeout(tick, 5);
      }));
      out.reconnect.push(r);
      await page.waitForTimeout(500);
    }
  }
  // Idle fps and long tasks for 3 s.
  out.idle = await fps(page, 3000);
  // Resize.
  out.resize = await resizeCost(page);
  // Streaming.
  if (STREAM && gotFrame2) {
    await typeLine(page, `stream ${STREAM}`);
    await page.waitForTimeout(500);
    const ts = Date.now();
    out.stream = await fps(page, Math.min(STREAM * 1000 - 800, 8000));
    out.stream.m = await metrics(s);
    out.stream.binMsgs = await page.evaluate(() => window.__gtPerf.ws.binMsgs);
    await page.waitForTimeout(Math.max(0, STREAM * 1000 + 800 - (Date.now() - ts)));
  }
  if (FLOOD && gotFrame2) {
    const m0 = await metrics(s);
    const w0 = await page.evaluate(() => ({ ...window.__gtPerf.ws }));
    await typeLine(page, `flood ${FLOOD}`);
    const tf = Date.now();
    const f = await fps(page, 6000);
    // Wait until the flood is through (the output rate drops).
    let last = -1;
    for (let i = 0; i < 120; i++) { await page.waitForTimeout(500); const b = await page.evaluate(() => window.__gtPerf.ws.bin); if (b === last) break; last = b; }
    const w1 = await page.evaluate(() => ({ ...window.__gtPerf.ws }));
    out.flood = { ...f, wallMs: Date.now() - tf, binMB: +((w1.bin - w0.bin) / 1048576).toFixed(2), binMsgs: w1.binMsgs - w0.binMsgs, snapshots: (w1.byType.snapshot?.n || 0) - (w0.byType.snapshot?.n || 0), m0, m1: await metrics(s) };
  }
  if (SOAK) {
    // Several panes streaming for SOAK minutes; memory sampled each minute.
    if (PANES > 1 && DEVICE === 'desktop') {
      await page.evaluate((n) => { const G = window.GT; G.panes.setN(n); const ids = G.S.tabs.map((t) => t.id); for (let i = 0; i < n; i++) G.panes.list[i].setTab(ids[i % ids.length]); G.panes.save(); G.panes.sync(); }, PANES);
      await page.waitForTimeout(2000);
    }
    const tabs = await page.evaluate(() => window.GT.panes.visible().map((p) => p.id));
    for (let i = 0; i < new Set(tabs).size; i++) {
      await page.evaluate((i) => window.GT.panes.focus(i), i);
      await typeLine(page, `stream ${Math.round(SOAK * 60 + 30)}`);
    }
    out.soak = { minutes: SOAK, panes: PANES, tabs, samples: [] };
    const tS = Date.now();
    while (Date.now() - tS < SOAK * 60000) {
      await page.waitForTimeout(60000);
      const m = await metrics(s);
      const f = await fps(page, 2000);
      const ws = await page.evaluate(() => ({ ...window.__gtPerf.ws, lt: window.__gtPerf.longtasks.length }));
      out.soak.samples.push({ min: +((Date.now() - tS) / 60000).toFixed(1), ...m, fps: f.fps, worstFrameMs: f.worstFrameMs, inMsgs: ws.inMsgs, binMB: +(ws.bin / 1048576).toFixed(1), stateFrames: ws.byType.state?.n, stateKB: +((ws.byType.state?.bytes || 0) / 1024).toFixed(0), longtasks: ws.lt });
      console.error(JSON.stringify(out.soak.samples.at(-1)));
    }
    if (s) { await s.send('HeapProfiler.collectGarbage').catch(() => {}); await page.waitForTimeout(1000); out.soak.afterGC = await metrics(s); }
  }
  out.final = { ws: await page.evaluate(() => window.__gtPerf.ws), longtasks: await page.evaluate(() => window.__gtPerf.longtasks.length), m: await metrics(s) };
  await browser.close();
  const json = JSON.stringify(out, null, 1);
  if (args.out) fs.writeFileSync(args.out, json);
  const c = out.cold, w = out.warm;
  const line = (n, x) => `${n}: fp ${x.fp} fcp ${x.fcp} dcl ${x.dcl} hello ${x.hello} snap ${x.firstSnapshot} frame ${x.firstFrame} | req ${x.requests} xfer ${x.transferKB}KB enc ${x.encodedKB}KB dec ${x.decodedKB}KB | longtasks ${x.longtasks} (${x.longtaskMs}ms, max ${x.longtaskMax}) | script ${x.m.scriptS}s heap ${x.m.heapMB}MB nodes ${x.m.nodes} | ws in ${(x.ws.in / 1024).toFixed(0)}KB ${x.ws.inMsgs} msgs, state ${x.ws.byType.state?.n || 0}x~${Math.round((x.ws.byType.state?.bytes || 0) / Math.max(1, x.ws.byType.state?.n || 1) / 1024)}KB, snapshot ${x.ws.byType.snapshot?.n || 0}x max ${Math.round((x.ws.byType.snapshot?.max || 0) / 1024)}KB | renderer ${x.renderer}`;
  console.log(`== ${LABEL} ${ENGINE} ${DEVICE}${THROTTLE ? ' throttled' : ''}`);
  console.log(line('cold', c));
  console.log(line('warm', w), `| sw ${c.sw} controlled ${w.controlled}`);
  if (out.keys) console.log(`keys: n ${out.keys.n} p50 ${out.keys.p50}ms p90 ${out.keys.p90}ms max ${out.keys.max}ms`);
  console.log(`idle: ${JSON.stringify(out.idle)}`);
  if (out.reconnect) console.log(`reconnect: ${JSON.stringify(out.reconnect)}`);
  if (out.resize) console.log(`resize: ${JSON.stringify(out.resize)}`);
  if (out.stream) console.log(`stream: ${JSON.stringify(out.stream)}`);
  if (out.flood) console.log(`flood: ${JSON.stringify({ ...out.flood, m0: undefined, m1: undefined })} heap ${out.flood.m0.heapMB} -> ${out.flood.m1.heapMB} MB`);
  if (out.soak) console.log(`soak: ${JSON.stringify(out.soak.samples.map((x) => [x.min, x.heapMB, x.nodes, x.listeners, x.fps]))} afterGC ${out.soak.afterGC?.heapMB}`);
  if (out.errors) console.log('errors:', out.errors);
}
main().catch((e) => { console.error(e); process.exit(1); });
