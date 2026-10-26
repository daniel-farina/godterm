// The web matrix's browser half (scripts/e2e/web_matrix.py runs it): one
// engine (WebKit or Chromium) at one size (a phone, 430x932 at 3x with
// touch, or a 1440x900 desktop) against one GodTerm, every check for
// every harness tab, written as JSON lines to the result file.
//
//   node scripts/e2e/web_matrix.mjs <config.json>
//
// config: {url, engine, device, out, groups: [..], safe, harnesses: [..],
//          tabs: {"<uid>": {"harness", "typing"}}, soak_s, shots}
// `safe` (real accounts): only keys that cannot start or change anything
// (no Esc, no Shift+Tab, no Enter on a real composer).
import { createRequire } from 'module';
import fs from 'fs';
import path from 'path';

const cfg = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const PW = process.env.PW_DIR || path.join(path.dirname(new URL(import.meta.url).pathname), '..', '..', 'web', 'dev', 'node_modules');
const { chromium, webkit, devices } = createRequire(PW.endsWith('/') ? PW : PW + '/')('playwright');
const out = fs.createWriteStream(cfg.out, { flags: 'a' });
const ENGINE = cfg.engine, DEVICE = cfg.device, PHONE = DEVICE === 'phone';
const want = (g) => !cfg.groups || cfg.groups.includes(g);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const latin1 = (s) => Buffer.from(s, 'utf8').toString('latin1');
const show = (s) => JSON.stringify(s).replace(/\\u001b/g, 'ESC').slice(0, 160);

function rec(group, check, harness, status, detail = '', extra = {}) {
  const r = { engine: ENGINE, device: DEVICE, group, check, harness, status, detail: String(detail).slice(0, 600), ...extra };
  out.write(JSON.stringify(r) + '\n');
  console.log(`${status === 'pass' ? 'ok  ' : status === 'skip' ? 'skip' : 'FAIL'} ${ENGINE}/${DEVICE} ${group}/${check} ${harness || ''} ${r.detail}`);
}

// The page side: every frame sent (binary input by tab, JSON) and the
// frames received, counted; the sockets, to drop them.
function hooks() {
  const G = (window.__gt = { sent: [], got: [], snaps: {}, resumes: {}, outB: {}, msgs: {}, socks: [], errors: [] });
  const s8 = (b) => { let s = ''; for (let i = 0; i < b.length; i++) s += String.fromCharCode(b[i]); return s; };
  const Orig = window.WebSocket;
  class W extends Orig {
    constructor(...a) {
      super(...a);
      G.socks.push(this);
      this.__n = 0;
      this.addEventListener('message', (e) => {
        this.__n++;
        if (typeof e.data === 'string') {
          let m;
          try { m = JSON.parse(e.data); } catch { return; }
          G.msgs[m.t] = (G.msgs[m.t] || 0) + 1;
          if (m.t === 'snapshot') G.snaps[m.tab] = (G.snaps[m.tab] || 0) + 1;
          if (m.t === 'resume') G.resumes[m.tab] = (G.resumes[m.tab] || 0) + 1;
          if (!['state', 'hello', 'snapshot'].includes(m.t)) G.got.push({ ...m, data: undefined, at: performance.now() });
          if (G.got.length > 2000) G.got.splice(0, 1000);
        } else {
          const b = new Uint8Array(e.data);
          if (b[0] === 1) { const u = ((b[1] << 24) | (b[2] << 16) | (b[3] << 8) | b[4]) >>> 0; G.outB[u] = (G.outB[u] || 0) + b.length - 5; }
        }
      });
    }
    send(d) {
      try {
        if (typeof d === 'string') G.sent.push({ j: JSON.parse(d), at: performance.now() });
        else {
          const b = d instanceof Uint8Array ? d : new Uint8Array(d.buffer || d, d.byteOffset || 0, d.byteLength);
          if (b[0] === 1) G.sent.push({ uid: ((b[1] << 24) | (b[2] << 16) | (b[3] << 8) | b[4]) >>> 0, s: s8(b.subarray(5)), at: performance.now() });
          else G.sent.push({ kind: b[0], n: b.length, at: performance.now() });
        }
        if (G.sent.length > 5000) G.sent.splice(0, 2500);
      } catch {}
      return super.send(d);
    }
  }
  window.WebSocket = W;
  addEventListener('error', (e) => G.errors.push(String(e.message || e)));
  addEventListener('unhandledrejection', (e) => G.errors.push('rejection: ' + String(e.reason?.message || e.reason)));
}

// Helpers defined in the page once the app is up.
function pageHelpers() {
  const href = (n) => {
    const l = document.querySelector(`link[rel=modulepreload][href*="js/${n}.js"]`) || document.querySelector('link[rel=modulepreload][href*="js/main.js"]');
    return l.href.replace(/js\/[a-z]+\.js/, `js/${n}.js`);
  };
  const mods = {};
  const mod = async (n) => (mods[n] ||= await import(href(n)));
  window.__h = {
    mod,
    async S() { return (await mod('state')).S; },
    // Synchronous (waitForFunction takes a promise for a yes).
    readyNow(id) { const p = mods.term?.panes.focused(); return !!p && p.id === id && p.ready && (p.msg.hidden || !!p.idleMsg); },
    async allText() {
      const { panes } = await mod('term');
      const b = panes.focused().term.buffer.active, out = [];
      for (let i = 0; i < b.length; i++) out.push(b.getLine(i)?.translateToString(true) || '');
      return out.join('\n');
    },
    async pane() {
      const { panes } = await mod('term');
      const p = panes.focused(), t = p.term, b = t.buffer.active;
      return { id: p.id, ready: p.ready, msgHidden: p.msg.hidden, idleMsg: p.idleMsg ? p.msg.textContent : '', path: p.scrollPath(), type: b.type, baseY: b.baseY, viewportY: b.viewportY,
        mouse: t.modes.mouseTrackingMode, bpm: t.modes.bracketedPasteMode, appCursor: t.modes.applicationCursorKeysMode,
        cols: t.cols, rows: t.rows, appBack: p.appBack || 0, jump: p.jump ? !p.jump.hidden : false, font: t.options.fontSize,
        text: (() => { const lines = []; for (let i = Math.max(0, b.length - t.rows - 3); i < b.length; i++) lines.push(b.getLine(i)?.translateToString(true) || ''); return lines.join('\n'); })() };
    },
    async show(id) { const { panes } = await mod('term'); panes.show(id); const { emit } = await mod('util'); emit('picked', id); },
    async tool(name, args = {}, extra = {}) { const { tool } = await mod('conn'); return tool(name, args, 30000, extra); },
    sentFor(uid, since) { return window.__gt.sent.filter((x) => x.uid === uid && x.at >= since).map((x) => x.s).join(''); },
    jsonSince(since, t) { return window.__gt.sent.filter((x) => x.j && x.at >= since && (!t || x.j.t === t)).map((x) => x.j); },
    gotSince(since, t) { return window.__gt.got.filter((x) => x.at >= since && (!t || x.t === t)); },
    now() { return performance.now(); },
    ta() { return document.querySelector('.pane:not([hidden]).focus .xterm-helper-textarea') || [...document.querySelectorAll('.pane')].find((p) => !p.hidden)?.querySelector('.xterm-helper-textarea'); },
    // iOS's text events on xterm's textarea (web/dev/dictation.mjs).
    fire(type, init = {}) {
      const ta = this.ta();
      let ev;
      if (type.startsWith('composition')) ev = new CompositionEvent(type, { bubbles: true, cancelable: true, data: init.data || '' });
      else if (type === 'beforeinput' || type === 'input') ev = new InputEvent(type, { bubbles: true, cancelable: type === 'beforeinput', inputType: init.inputType, data: init.data ?? null, isComposing: !!init.isComposing });
      else if (type.startsWith('key')) ev = new KeyboardEvent(type, { bubbles: true, cancelable: true, key: init.key, code: init.code || '', keyCode: init.keyCode || 0, which: init.keyCode || 0, charCode: type === 'keypress' ? (init.key === 'Enter' ? 13 : (init.key || '').charCodeAt(0)) : 0 });
      else if (type === 'paste') { const dt = new DataTransfer(); dt.setData('text/plain', init.text); ev = new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: dt }); }
      ta.dispatchEvent(ev);
      return ev;
    },
    replace(s, e, text, inputType = 'insertText', isComposing = false) {
      const ta = this.ta();
      ta.setSelectionRange(s, e);
      const b = this.fire('beforeinput', { inputType, data: text, isComposing });
      if (!b.defaultPrevented) ta.setRangeText(text, s, e, 'end');
      this.fire('input', { inputType, data: text, isComposing });
    },
    key(k) {
      const ta = this.ta();
      const kc = k === ' ' ? 32 : k.toUpperCase().charCodeAt(0);
      const d = this.fire('keydown', { key: k, keyCode: kc });
      const p = this.fire('keypress', { key: k, keyCode: kc });
      if (!d.defaultPrevented && !p.defaultPrevented) { const n = ta.value.length; this.replace(ta.selectionStart ?? n, ta.selectionEnd ?? n, k); }
      this.fire('keyup', { key: k, keyCode: kc });
    },
    // The soft keyboard's Return: keypress only when keydown was not
    // taken (a browser fires none after a prevented keydown).
    enter() {
      const d = this.fire('keydown', { key: 'Enter', code: 'Enter', keyCode: 13 });
      if (!d.defaultPrevented) this.fire('keypress', { key: 'Enter', code: 'Enter', keyCode: 13 });
      this.fire('keyup', { key: 'Enter', code: 'Enter', keyCode: 13 });
    },
    async dictate(parts) {
      const ta = this.ta(); const s = ta.value.length; let prev = '';
      for (const p of parts) { this.replace(s, s + prev.length, p); prev = p; await new Promise((r) => setTimeout(r, 30)); }
    },
    // A one finger drag on the focused pane's body: dy > 0 moves the
    // finger down (towards older text).
    async drag(dy, steps = 12, x0 = null, y0 = null) {
      const { panes } = await mod('term');
      const b = panes.focused().body, r = b.getBoundingClientRect();
      const x = x0 ?? r.left + r.width / 2, y = y0 ?? r.top + r.height * (dy > 0 ? 0.25 : 0.75);
      // Plain objects as touches: WebKit has no Touch constructor, and the
      // app reads only clientX / clientY / identifier.
      const T = (yy) => ({ identifier: 7, target: b, clientX: x, clientY: yy, pageX: x, pageY: yy, screenX: x, screenY: yy });
      const ev = (type, yy) => window.__h.touch(b, type, type === 'touchend' ? [] : [T(yy)], [T(yy)]);
      ev('touchstart', y);
      for (let i = 1; i <= steps; i++) { await new Promise((r) => setTimeout(r, 16)); ev('touchmove', y + (dy * i) / steps); }
      await new Promise((r) => setTimeout(r, 120));
      ev('touchend', y + dy);
    },
    async touchPane() {
      const { panes } = await mod('term');
      const b = panes.focused().body, r = b.getBoundingClientRect();
      const t = { identifier: 9, target: b, clientX: r.left + 20, clientY: r.top + 20, pageX: r.left + 20, pageY: r.top + 20 };
      this.touch(b, 'touchstart', [t], [t]);
      this.touch(b, 'touchend', [], [t]);
    },
    touch(el, type, touches, changed) {
      const e = new Event(type, { bubbles: true, cancelable: true });
      for (const [k, v] of [['touches', touches], ['targetTouches', touches], ['changedTouches', changed]]) Object.defineProperty(e, k, { value: Object.assign([...v], { item: (i) => v[i] }) });
      el.dispatchEvent(e);
      return e;
    },
    hide(on) {
      Object.defineProperty(document, 'hidden', { configurable: true, get: () => on });
      Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => (on ? 'hidden' : 'visible') });
      document.dispatchEvent(new Event('visibilitychange'));
    },
    sockets() { return window.__gt.socks.map((s) => s.readyState); },
    dropSocket() { const s = window.__gt.socks.filter((x) => x.readyState === 1).pop(); s?.close(); return !!s; },
  };
}

// Applied as a line editor would (DEL deletes one character).
function applyLine(s) {
  s = s.replace(/\x1b\[20[01]~/g, '');
  const line = [];
  for (const ch of Array.from(Buffer.from(s, 'latin1').toString('utf8'))) { if (ch === '\x7f') line.pop(); else line.push(ch); }
  return line.join('');
}

const NEWLINE = { codex: '\n', opencode: '\n' };
const nlFor = (h) => NEWLINE[h] || '\x1b\r';

let browser = null;
async function main() {
  const eng = ENGINE === 'webkit' ? webkit : chromium;
  const args = ENGINE === 'chromium' ? ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--autoplay-policy=no-user-gesture-required'] : [];
  browser = await eng.launch({ args });
  const phone = { ...devices['iPhone 15 Pro'], viewport: { width: 430, height: 932 }, screen: { width: 430, height: 932 }, deviceScaleFactor: 3 };
  if (ENGINE === 'chromium') delete phone.defaultBrowserType;
  const opts = PHONE ? phone : { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 };
  if (ENGINE === 'chromium') opts.permissions = ['microphone', 'clipboard-read', 'clipboard-write'];
  const ctx = await browser.newContext(opts);
  await ctx.addInitScript(hooks);
  const page = await ctx.newPage();
  const pageErrors = [];
  page.on('pageerror', (e) => pageErrors.push((e.stack || e.message).split('\n').slice(0, 4).join(' / ')));
  // (The reconnect checks drop the socket on purpose: WebKit logs that.)
  page.on('console', (m) => { if (m.type() === 'error' && !/GL Driver|WebGL|favicon|WebSocket connection to .* failed/.test(m.text())) pageErrors.push('console: ' + m.text()); });
  const t0 = Date.now();
  await page.goto(cfg.url);
  await page.waitForFunction(() => document.querySelector('#conn')?.classList.contains('live'), null, { timeout: 20000 });
  await page.evaluate(pageHelpers);
  await page.evaluate(() => window.__h.mod('term'));
  rec('page', 'load', '', 'pass', `${Date.now() - t0} ms to live`);
  const H = (fn, ...a) => page.evaluate(fn, ...a);
  const S = () => H(async () => { const S = await window.__h.S(); return { tabs: S.tabs, accounts: S.accounts, approvals: S.approvals }; });
  await page.evaluate(() => window.__h.mod('state'));
  for (let i = 0; i < 100 && !(await page.evaluate(async () => (await window.__h.S()).tabs.length)); i++) await sleep(150);
  const harnessOf = (st, t) => st.accounts.find((a) => a.account === t.account)?.harness || 'claude';
  let st = await S();
  const tabs = st.tabs.filter((t) => !cfg.tabs || cfg.tabs[t.uid]).map((t) => ({ ...t, harness: harnessOf(st, t), typing: cfg.tabs?.[t.uid]?.typing ?? true }));
  const shot = async (name) => { if (cfg.shots) await page.screenshot({ path: path.join(cfg.shots, `${ENGINE}-${DEVICE}-${name}.png`) }).catch(() => {}); };

  const openDrawer = async (on) => {
    if (!PHONE) return;
    const open = await H(() => document.querySelector('#side').classList.contains('open'));
    if (open !== on) await H(() => document.querySelector('#b-menu').click());
    await sleep(250);
  };
  const waitReady = async (id, ms = 8000) => {
    try {
      await page.waitForFunction((id) => window.__h.readyNow(id), id, { timeout: ms, polling: 100 });
      return true;
    } catch (e) { console.log('waitReady: ' + e.message.split('\n')[0]); return false; }
  };
  const focusTerm = async () => {
    if (PHONE) await H(() => window.__h.touchPane());
    else {
      const box = await page.locator('.pane.focus .pane-body, .pane:not([hidden]) .pane-body').first().boundingBox();
      await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
    }
    await H(() => window.__h.ta()?.focus());
    await sleep(150);
  };
  // The Prompt box (the key bar's Prompt on a phone).
  const openPrompt = async () => {
    await closeSheets();
    if (PHONE) await page.locator('#keybar button', { hasText: /^Prompt$/ }).first().click();
    else await H(async () => (await window.__h.mod('keybar')).promptSheet('prompt'));
    await page.waitForSelector('#sheet-body textarea', { timeout: 5000 });
  };
  // Close a sheet or popover left open.
  const closeSheets = async () => {
    for (let i = 0; i < 3; i++) {
      const open = await H(() => document.querySelector('#sheet')?.classList.contains('on') || !!document.querySelector('#pop:not([hidden])'));
      if (!open) break;
      await H(async () => (await window.__h.mod('sheet')).closeSheet());
      await sleep(150);
    }
  };
  const sentSince = (uid, t) => H(([u, t]) => window.__h.sentFor(u, t), [uid, t]);
  const now = () => H(() => window.__h.now());
  const clearLine = async (uid) => { await H(([u]) => import(document.querySelector('link[href*="js/conn.js"]').href).then((m) => m.sendInput(u, '\x15')), [uid]); await sleep(200); };

  for (const t of tabs) {
    const h = t.harness, uid = t.uid;
    const R = (g, c, s, d, x) => rec(g, c, h, s, d, { tab: t.id, ...x });
    const tOpen = await now();
    // Tab list and state.
    if (want('tabs')) {
      try {
        await openDrawer(true);
        const row = await H((id) => { const r = document.querySelector(`#list .row[data-id="${id}"]`); return r ? { dot: r.querySelector('.dot').className, n: r.querySelector('.n').textContent } : null; }, t.id);
        const cur = (await S()).tabs.find((x) => x.id === t.id);
        if (!row) R('tabs', 'listed', 'fail', 'no row in the tab list');
        else {
          const words = String(cur.state || '').split(/\s+/).filter(Boolean);
          const ok = words.every((w) => row.dot.split(/\s+/).includes(w));
          R('tabs', 'listed', ok ? 'pass' : 'fail', `${cur.state}${cur.label ? ' / ' + cur.label : ''} (dot "${row.dot}")`);
        }
        // Opening it.
        const tt = await now();
        // (A JS click: a banner or toast may float over a long list.)
        await H((id) => document.querySelector(`#list .row[data-id="${id}"] .tx`).click(), t.id);
        const ok = await waitReady(t.id);
        const p = await H(() => window.__h.pane());
        const snaps = await H((u) => window.__gt.snaps[u] || 0, uid);
        const title = await H(() => document.querySelector('#t-name').textContent);
        const resumed = await H((u) => window.__gt.resumes[u] || 0, uid);
        const why = ok && (snaps || resumed) ? '' : ' ' + JSON.stringify(await H(async ([t0, u]) => { const { panes } = await window.__h.mod('term'); const p = panes.focused(); return { sent: window.__gt.sent.filter((x) => x.j && x.at > t0 && x.j.t !== 'log').map((x) => x.j), got: window.__gt.got.filter((x) => x.at > t0).map((x) => x.t + ':' + x.tab), sid: p.sid, from: p.from, waiting: [...panes.waiting], subs: [...panes.subs], stash: [...p.stash.keys()], socks: window.__gt.socks.map((s) => [s.readyState, s.__n]), msgs: window.__gt.msgs }; }, [tt, uid]));
        R('tabs', 'open', ok && (snaps > 0 || resumed > 0) && title === t.name ? 'pass' : 'fail', `${(await now() - tt).toFixed(0)} ms, ${snaps} snapshots, ${resumed} resumes, ${p.cols}x${p.rows} ${p.type} buffer, ${p.baseY} history lines, mouse ${p.mouse}, title "${title}"${why}`);
        const text = p.text.trim();
        R('tabs', 'opens at its end', p.viewportY === p.baseY ? 'pass' : 'fail', `at ${p.viewportY} of ${p.baseY}`);
        // Not running and blank: the pane says so.
        if (!text && cur.running === false) R('tabs', 'not running says so', p.idleMsg ? 'pass' : 'fail', p.idleMsg || 'a blank pane');
        else R('tabs', 'snapshot drawn', text.length > 0 ? 'pass' : 'fail', JSON.stringify(text.split('\n').filter((l) => l.trim()).slice(-2).join(' | ')).slice(0, 120));
      } catch (e) { R('tabs', 'open', 'fail', e.message); }
      await openDrawer(false);
    } else {
      await H((id) => window.__h.show(id), t.id);
      await waitReady(t.id);
    }
    const p0 = await H(() => window.__h.pane());
    if (p0.id !== t.id) { R('tabs', 'focused', 'fail', `pane shows ${p0.id}`); continue; }

    if (want('live')) {
      const b0 = await H((u) => window.__gt.outB[u] || 0, uid);
      const st0 = (await S()).tabs.find((x) => x.id === t.id)?.state;
      await sleep(3000);
      const b1 = await H((u) => window.__gt.outB[u] || 0, uid);
      const st1 = (await S()).tabs.find((x) => x.id === t.id)?.state;
      const busy = st0 === 'working' && st1 === 'working';
      if (b1 > b0) R('live', 'output', 'pass', `${b1 - b0} bytes in 3 s (${st0})`);
      else R('live', 'output', busy ? 'fail' : 'skip', busy ? 'a working tab sent nothing in 3 s' : `${st0} -> ${st1}: nothing to stream`);
    }

    const canType = t.typing && t.running !== false;
    if (want('typing') && canType) {
      await focusTerm();
      // Plain text.
      let s0 = await now();
      if (PHONE) { for (const k of 'qa1 ') await H((k) => window.__h.key(k), k); } else await page.keyboard.type('qa1 ');
      await sleep(250);
      let got = await sentSince(uid, s0);
      R('typing', 'plain text', applyLine(got) === 'qa1 ' && !got.includes('qa1qa1') ? 'pass' : 'fail', `sent ${show(got)}`);
      if (PHONE) {
        // iOS dictation: each partial result replaces the phrase so far.
        const P = ['the', 'the interaction', 'the interaction of', 'the interaction of the microphone', 'the interaction of the microphone was confusing.'];
        s0 = await now();
        await H((P) => window.__h.dictate(P), P);
        await sleep(250);
        got = await sentSince(uid, s0);
        const fin = P[P.length - 1];
        R('typing', 'dictation diff', applyLine(got) === fin && Array.from(got).length <= fin.length + 30 ? 'pass' : 'fail', `${Array.from(got).length} chars for ${fin.length}: ${show(applyLine(got))}`);
        // Autocorrect: the last word replaced.
        s0 = await now();
        await H(async () => { for (const k of ' teh') window.__h.key(k); const ta = window.__h.ta(); const n = ta.value.length; window.__h.replace(n - 3, n, 'the', 'insertReplacementText'); });
        await sleep(200);
        got = await sentSince(uid, s0);
        R('typing', 'autocorrect', applyLine(got) === ' the' ? 'pass' : 'fail', `sent ${show(got)}`);
        // A composition (IME, some dictation) goes once, when it ends.
        s0 = await now();
        await H(async () => { const h = window.__h, ta = h.ta(); const s = ta.value.length; h.fire('compositionstart'); let prev = ''; for (const p of [' ni', ' nih', ' 你好']) { h.fire('compositionupdate', { data: p }); h.replace(s, s + prev.length, p, 'insertCompositionText', true); prev = p; } h.fire('compositionend', { data: prev }); });
        await sleep(200);
        got = await sentSince(uid, s0);
        R('typing', 'composition', applyLine(got) === ' 你好' ? 'pass' : 'fail', `sent ${show(Buffer.from(got, 'latin1').toString('utf8'))}`);
      }
      // Return: a new line (phone, the default) or Enter (desktop).
      if (!cfg.safe || PHONE) {
        s0 = await now();
        if (PHONE) await H(() => window.__h.enter()); else if (!cfg.safe) await page.keyboard.press('Enter');
        await sleep(250);
        got = await sentSince(uid, s0);
        const wantB = PHONE ? nlFor(h) : '\r';
        R('typing', PHONE ? 'Return = new line' : 'Enter = submit', got === wantB ? 'pass' : 'fail', `sent ${show(got)} want ${show(wantB)}`);
      }
      if (!PHONE) {
        // Shift+Enter: the harness's new line, as in the terminal UI.
        s0 = await now();
        await page.keyboard.press('Shift+Enter');
        await sleep(250);
        got = await sentSince(uid, s0);
        R('typing', 'Shift+Enter = new line', got === nlFor(h) ? 'pass' : 'fail', `sent ${show(got)} want ${show(nlFor(h))}`);
      }
      // Paste: bracketed when the app turned it on.
      const pm = await H(() => window.__h.pane());
      s0 = await now();
      await H(() => window.__h.fire('paste', { text: 'pasted words' }));
      if (!PHONE) await H(async () => { const { panes } = await window.__h.mod('term'); /* xterm owns paste on desktop */ });
      await sleep(250);
      got = await sentSince(uid, s0);
      const wantP = pm.bpm ? '\x1b[200~pasted words\x1b[201~' : 'pasted words';
      R('typing', `paste (${pm.bpm ? 'bracketed' : 'plain'})`, got === wantP ? 'pass' : 'fail', `sent ${show(got)}`);
      // Keys.
      const press = async (label) => { if (PHONE) await page.locator('#keybar button', { hasText: new RegExp('^' + label.replace(/[.*+?^${}()|[\]\\]/g, '\\$&') + '$') }).first().click(); };
      const keyCase = async (name, act, wantB) => {
        const s = await now();
        await act();
        await sleep(200);
        const g = await sentSince(uid, s);
        R('keys', name, g === wantB ? 'pass' : 'fail', `sent ${show(g)}${g === wantB ? '' : ' want ' + show(wantB)}`);
      };
      if (PHONE) {
        await keyCase('Ctrl latch + u', async () => { await press('Ctrl'); await H(() => window.__h.key('u')); }, '\x15');
        await keyCase('Ctrl locked: a then e', async () => { await press('Ctrl'); await press('Ctrl'); await H(() => { window.__h.key('a'); window.__h.key('e'); }); await press('Ctrl'); }, '\x01\x05');
        const ac = (await H(() => window.__h.pane())).appCursor;
        await keyCase('arrow up', () => press('↑'), ac ? '\x1bOA' : '\x1b[A');
        await keyCase('arrow down', () => press('↓'), ac ? '\x1bOB' : '\x1b[B');
        await keyCase('Tab', () => press('Tab'), '\t');
        if (!cfg.safe) {
          await keyCase('Esc', () => press('Esc'), '\x1b');
          await keyCase('Shift+Tab', () => press('⇧Tab'), '\x1b[Z');
        }
        const sp = (await H(() => window.__h.pane())).path;
        await keyCase(`PgUp (${sp})`, () => press('PgUp'), sp === 'app' ? '\x1b[5~' : '');
        await keyCase(`PgDn (${sp})`, () => press('PgDn'), sp === 'app' ? '\x1b[6~' : '');
        await keyCase('Send = Enter', async () => { if (cfg.safe) return; await page.click('#b-ksend'); }, cfg.safe ? '' : '\r');
      } else {
        await keyCase('Ctrl+U', () => page.keyboard.press('Control+U'), '\x15');
        const ac = (await H(() => window.__h.pane())).appCursor;
        await keyCase('arrow up', () => page.keyboard.press('ArrowUp'), ac ? '\x1bOA' : '\x1b[A');
        if (!cfg.safe) {
          await keyCase('Esc', () => page.keyboard.press('Escape'), '\x1b');
          await keyCase('Shift+Tab', () => page.keyboard.press('Shift+Tab'), '\x1b[Z');
        }
      }
      // The Prompt box with "Press Enter after it": the text, then Enter
      // as its own frame once the tab shows it (a CR in the same read is
      // a new line for Claude Code, a paste for Codex).
      if (!cfg.safe) {
        await clearLine(uid);
        const s = await now();
        await openPrompt();
        await page.fill('#sheet-body textarea', 'qa prompt box');
        await page.locator('#sheet-body button.btn.pri', { hasText: 'Send' }).first().click();
        await sleep(1800);
        const fr = await H(([u, t]) => window.__gt.sent.filter((x) => x.uid === u && x.at >= t).map((x) => [x.s, x.at]), [uid, s]);
        const pi = fr.findIndex((f) => f[0].includes('qa prompt box')), ci = fr.findIndex((f, i) => i > pi && f[0] === '\r');
        const gap = pi >= 0 && ci > pi ? fr[ci][1] - fr[pi][1] : -1;
        R('typing', 'Prompt box: Enter on its own', pi >= 0 && ci > pi && gap >= 50 ? 'pass' : 'fail', `${fr.length} frames, Enter ${gap >= 0 ? gap.toFixed(0) + ' ms after the text' : 'missing or in the same frame'}: ${show(fr.map((f) => f[0]).join('|'))}`);
      }
      await clearLine(uid);
    } else if (want('typing')) R('typing', 'all', 'skip', t.running === false ? 'tab not running' : 'not typed into (a login or start up screen)');

    if (want('scroll')) {
      try {
        const p = await H(() => window.__h.pane());
        // Wheel notches are input: none to a tab that gets no keys (a
        // login screen takes any key as "log in").
        if (!canType && (p.path === 'app' || p.mouse !== 'none')) R('scroll', 'drag', 'skip', `no wheel to a tab that gets no keys (${p.type}, mouse ${p.mouse})`);
        else if (PHONE) {
          const s = await now();
          await H(() => window.__h.drag(260));
          await sleep(300);
          const q = await H(() => window.__h.pane());
          const wheels = await H(([t]) => window.__h.jsonSince(t, 'wheel'), [s]);
          if (p.path === 'app') {
            const up = wheels.filter((w) => w.up).reduce((a, w) => a + w.n, 0);
            R('scroll', `drag -> wheel to app (${p.type}, mouse ${p.mouse})`, up > 0 && q.jump ? 'pass' : 'fail', `${up} notches up in ${wheels.length} frames, jump button ${q.jump}`);
            const s2 = await now();
            if (q.jump) await H(() => document.querySelector('.pane:not([hidden]) .jump')?.click());
            await sleep(300);
            const down = (await H(([t]) => window.__h.jsonSince(t, 'wheel'), [s2])).filter((w) => !w.up).reduce((a, w) => a + w.n, 0);
            R('scroll', 'jump to end (app)', down >= up && down > 0 ? 'pass' : 'fail', `${down} notches down for ${up} up`);
          } else if (p.baseY > 3) {
            const moved = q.viewportY < p.viewportY;
            R('scroll', 'drag -> local history', moved && wheels.length === 0 && q.jump ? 'pass' : 'fail', `viewport ${p.viewportY} -> ${q.viewportY} of ${q.baseY}, ${wheels.length} wheel frames, jump ${q.jump}`);
            await sleep(1500);
            const r2 = await H(() => window.__h.pane());
            R('scroll', 'no snap back', r2.viewportY <= q.viewportY + 1 && r2.viewportY < r2.baseY ? 'pass' : 'fail', `after 1.5 s at ${r2.viewportY} of ${r2.baseY}`);
            if (q.jump) await H(() => document.querySelector('.pane:not([hidden]) .jump')?.click());
            await sleep(200);
            const r3 = await H(() => window.__h.pane());
            R('scroll', 'jump to end', r3.viewportY === r3.baseY && !r3.jump ? 'pass' : 'fail', `${r3.viewportY} of ${r3.baseY}`);
          } else R('scroll', 'drag', 'skip', `main screen with no history (${p.type})`);
        } else {
          const box = await page.locator('.pane.focus .xterm-screen, .pane:not([hidden]) .xterm-screen').first().boundingBox();
          await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
          const s = await now();
          await page.mouse.wheel(0, -400);
          await sleep(300);
          const q = await H(() => window.__h.pane());
          const g = await sentSince(uid, s);
          if (p.mouse !== 'none') R('scroll', `wheel -> mouse reports (${p.mouse})`, /\x1b\[<6[45];|\x1b\[M/.test(g) ? 'pass' : 'fail', `sent ${show(g)}`);
          else if (p.type === 'alternate') R('scroll', 'wheel on the alternate screen', g.length > 0 || q.viewportY === p.viewportY ? 'pass' : 'fail', `sent ${show(g)} (xterm: arrow keys or nothing)`);
          else if (p.baseY > 0) R('scroll', 'wheel -> local history', q.viewportY < p.viewportY && !g ? 'pass' : 'fail', `${p.viewportY} -> ${q.viewportY}, sent ${show(g)}`);
          else R('scroll', 'wheel', 'skip', 'no history');
          await page.mouse.wheel(0, 4000);
          await sleep(200);
        }
      } catch (e) { R('scroll', 'drag', 'fail', e.message); }
    }

    if (want('resize')) {
      try {
        if (PHONE) {
          await H(() => window.__h.touchPane());
          await sleep(1300);
          let rs = await H(([t]) => window.__h.jsonSince(t, 'resize'), [tOpen]);
          const mine = rs.filter((r) => r.tab === uid && !r.release);
          // Shown already when the page opened: sized then.
          const cur = (await S()).tabs.find((x) => x.id === t.id), pq = await H(() => window.__h.pane());
          const held = cur?.size_taken_by != null && cur.cols === pq.cols && cur.rows === pq.rows;
          R('resize', 'fit to screen takes the size', (mine.length >= 1 && mine.every((r) => r.by)) || held ? 'pass' : 'fail', mine.length ? `${mine[mine.length - 1].cols}x${mine[mine.length - 1].rows} by ${mine[0].by}` : held ? `held since the page opened: ${pq.cols}x${pq.rows}` : `no resize (${rs.length} frames), tab ${cur?.cols}x${cur?.rows} taken by ${cur?.size_taken_by}`);
          // Three quick zoom steps: one resize, after they settle.
          const s2 = await now();
          // (A tap on the faded control only wakes it: wake it first.)
          await H(() => window.__h.touchPane());
          await sleep(100);
          for (let i = 0; i < 3; i++) { await H(() => document.querySelector('.pane:not([hidden]) .fzoom button[aria-label="Larger text"]')?.click()); await sleep(120); }
          await sleep(1500);
          rs = (await H(([t]) => window.__h.jsonSince(t, 'resize'), [s2])).filter((r) => r.tab === uid && !r.release);
          const pz = await H(() => window.__h.pane());
          // One resize, and a zoom never leaves the tab scrolled back.
          R('resize', 'zoom steps debounced', rs.length === 1 && !pz.jump ? 'pass' : 'fail', `${rs.length} resizes for 3 steps${rs[0] ? ' -> ' + rs[0].cols + 'x' + rs[0].rows : ''}, at ${pz.viewportY} of ${pz.baseY}`);
          await H(() => document.querySelector('.pane:not([hidden]) .fz-fit')?.click());
          await sleep(900);
          const st2 = (await S()).tabs.find((x) => x.id === t.id);
          R('resize', 'server says sized by this browser', st2?.size_taken_by != null ? 'pass' : 'fail', `size_taken_by ${st2?.size_taken_by}`);
        } else {
          const s = await now();
          await sleep(800);
          const rs = await H(([t]) => window.__h.jsonSince(t, 'resize'), [s]);
          R('resize', 'desktop keeps the tab size', rs.length === 0 ? 'pass' : 'fail', `${rs.length} resize frames`);
          const s2 = await now();
          await page.locator('.pane:not([hidden]) .pane-head button[data-i="max"]').first().click();
          await sleep(900);
          const take = (await H(([t]) => window.__h.jsonSince(t, 'resize'), [s2])).filter((r) => r.tab === uid);
          await page.locator('.pane:not([hidden]) .pane-head button[data-i="max"]').first().click();
          await sleep(500);
          const rel = (await H(([t]) => window.__h.jsonSince(t, 'resize'), [s2])).filter((r) => r.tab === uid && r.release);
          R('resize', 'take size, then give it back', take.some((r) => r.cols) && rel.length ? 'pass' : 'fail', `${take.filter((r) => r.cols).map((r) => r.cols + 'x' + r.rows).join(',')}; ${rel.length} release`);
        }
      } catch (e) { R('resize', 'fit', 'fail', e.message); }
    }

    if (want('reconnect')) {
      try {
        const r0 = await H((u) => window.__gt.resumes[u] || 0, uid), n0 = await H((u) => window.__gt.snaps[u] || 0, uid);
        await H(() => window.__h.dropSocket());
        await page.waitForFunction(() => document.querySelector('#conn').classList.contains('live') && window.__gt.socks.some((s) => s.readyState === 1), null, { timeout: 15000 });
        await sleep(1500);
        const r1 = await H((u) => window.__gt.resumes[u] || 0, uid), n1 = await H((u) => window.__gt.snaps[u] || 0, uid);
        const ok = await waitReady(t.id, 5000);
        R('reconnect', 'resume after a lost socket', ok && (r1 > r0 || n1 > n0) ? 'pass' : 'fail', `${r1 - r0} resume, ${n1 - n0} snapshot`, { resumed: r1 > r0 });
      } catch (e) { R('reconnect', 'resume', 'fail', e.message); }
      try {
        const s = await now();
        await H(() => window.__h.hide(true));
        await sleep(1500);
        await H(() => window.__h.hide(false));
        await sleep(1200);
        const pong = (await H(([t]) => window.__h.gotSince(t, 'pong'), [s])).length;
        const live = await H(() => window.__gt.socks.filter((x) => x.readyState === 1).length);
        const ok = await waitReady(t.id, 3000);
        R('reconnect', 'hidden then shown', ok && live === 1 && pong >= 1 ? 'pass' : 'fail', `${pong} pong, ${live} open sockets`);
      } catch (e) { R('reconnect', 'visibility', 'fail', e.message); }
    }

    if (want('image') && canType) {
      try {
        const s = await now();
        const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEklEQVR4nGP4z8CAFWEXHbQSACj/P8Fu7N9hAAAAAElFTkSuQmCC', 'base64');
        const chooser = page.waitForEvent('filechooser', { timeout: 5000 });
        if (PHONE) await page.locator('#keybar button[data-i="image"]').first().click();
        else await H(async () => { const { pickImages } = await window.__h.mod('images'); const { panes } = await window.__h.mod('term'); pickImages(panes.focused()); });
        const fc = await chooser;
        await fc.setFiles({ name: 'qa.png', mimeType: 'image/png', buffer: png });
        await sleep(400);
        // The phone opens the image box first: Send (no Enter after it).
        if (PHONE) {
          const enter = page.locator('#sheet-body label.bar input[type=checkbox]');
          if (await enter.count()) await enter.first().uncheck().catch(() => {});
          const send = page.locator('#sheet-body button.btn.pri', { hasText: 'Send' });
          if (await send.count()) await send.first().click();
        }
        await sleep(1500);
        const img = (await H(([t]) => window.__h.gotSince(t, 'image'), [s]))[0];
        const g = await sentSince(uid, s);
        const pfx = h === 'grok' ? 'Image: ' : '';
        const ok = img && g.includes(pfx + img.path);
        R('image', 'attach into the tab', ok ? 'pass' : 'fail', img ? `typed ${show(g)}` : 'no image reply');
        await clearLine(uid);
      } catch (e) { R('image', 'attach', 'fail', e.message); }
    }

    if (want('options')) {
      try {
        // Switch model: the tab's own list (Fable for Claude), or a note.
        await closeSheets();
        await page.click('#b-tabmenu');
        await sleep(350);
        await page.locator('#sheet-body li, .menu li, [role=menu] li, .mbody .li, .mbody button', { hasText: 'Switch model' }).first().click();
        await sleep(300);
        const txt = await H(() => (document.querySelector('.menu.open, #menu, .pop') || document.querySelector('#sheet-body'))?.innerText || document.body.innerText);
        const acct = (await S()).accounts.find((a) => a.account === t.account);
        const labels = (acct.models || []).map((m) => m.label);
        if (h === 'claude') R('options', 'model list', labels.includes('Fable') && /Fable/.test(txt) ? 'pass' : 'fail', `models [${labels}]`);
        else if (h === 'grok') R('options', 'model list', labels.length >= 2 && labels.every((l) => txt.includes(l)) ? 'pass' : 'fail', `models [${labels}]`);
        else R('options', 'model refused gracefully', /inside the CLI/.test(txt) && !/Sonnet|Opus/.test(txt) ? 'pass' : 'fail', JSON.stringify(txt.split('\n').find((l) => /CLI|model/i.test(l)) || txt.slice(0, 80)));
        await page.keyboard.press('Escape');
        await sleep(200);
        if (h !== 'claude' && h !== 'grok') {
          const r = await H((id) => window.__h.tool('set_tab_model', { tab: id, model: 'opus' }), t.id);
          R('options', 'set_tab_model refused', r.ok === false && /inside the CLI/.test(r.error || '') ? 'pass' : 'fail', r.error || JSON.stringify(r).slice(0, 100));
        }
        if (!cfg.safe) {
          // Rename and back.
          let r = await H((id) => window.__h.tool('rename_tab', { tab: id, name: 'qa-renamed' }, { user_says: 'rename it' }), t.id);
          await sleep(400);
          const nm = (await S()).tabs.find((x) => x.id === t.id)?.name;
          r = await H(([id, n]) => window.__h.tool('rename_tab', { tab: id, name: n }, { user_says: 'rename it' }), [t.id, t.name]);
          R('options', 'rename', nm === 'qa-renamed' ? 'pass' : 'fail', `renamed to ${nm}`);
          // Pin and unpin.
          await H((id) => window.__h.tool('pin_tab', { tab: id }), t.id);
          await sleep(300);
          const pinned = (await S()).tabs.find((x) => x.id === t.id)?.pinned;
          await H((id) => window.__h.tool('unpin_tab', { tab: id }), t.id);
          R('options', 'pin', pinned ? 'pass' : 'fail', `pinned ${pinned}`);
          // Move: only within the same CLI, refused for the four others.
          const other = (await S()).accounts.find((a) => a.account !== t.account);
          if (other) {
            const mv = await H(([id, to]) => window.__h.tool('move_tab', { tab: id, to }), [t.id, other.account]);
            const refused = mv.ok === false || mv.needs_confirmation;
            R('options', 'move to another CLI refused', refused ? 'pass' : 'fail', (mv.error || mv.question || JSON.stringify(mv)).slice(0, 140));
            if (mv.needs_confirmation) await H((tok) => window.__h.tool('move_tab', { confirm_token: tok }, { user_says: 'no' }), mv.token);
          }
          if (!['claude', 'grok'].includes(h)) {
            const ar = await H((id) => window.__h.tool('archive_tabs', { tab: id }), t.id);
            const msg = ar.error || ar.result?.error || JSON.stringify(ar.result || ar);
            R('options', 'archive without a session id', /session id|session/.test(msg) || ar.ok !== false ? 'pass' : 'fail', msg.slice(0, 140));
          }
        }
      } catch (e) { R('options', 'menu', 'fail', e.message); await page.keyboard.press('Escape').catch(() => {}); }
    }
  }


  // Approvals: watched for a while (the demo agents ask now and then);
  // each harness's first one is answered Yes from the approvals sheet.
  if (want('approval')) {
    const asked = new Set(), done = new Set();
    const wantH = new Set(tabs.map((t) => t.harness));
    const until = Date.now() + (cfg.approval_s || 90) * 1000;
    while (Date.now() < until && done.size < wantH.size) {
      const cur = await S();
      for (const ap of cur.approvals || []) {
        if (ap.kind !== 'tab') continue;
        const t = tabs.find((x) => x.uid === ap.tab);
        if (!t || done.has(t.harness)) continue;
        const R = (c, st, d) => rec('approval', c, t.harness, st, d, { tab: t.id });
        done.add(t.harness);
        if (cfg.safe) { R('listed', 'pass', `${ap.question} [${ap.choices}]`); continue; }
        try {
          await closeSheets();
          const s = await now();
          await page.click('#b-appr');
          await sleep(300);
          const card = page.locator('#sheet-body .lrow.appr', { hasText: t.name }).first();
          const btns = await card.locator('button').allTextContents();
          const yes = card.locator('button', { hasText: /^Yes$/ });
          if (!(await yes.count())) {
            R('answer from the web', ap.choices.length ? 'fail' : 'pass', `buttons [${btns}], choices [${ap.choices}]: ${ap.question}`);
          } else {
            await yes.click();
            await sleep(400);
            const ans = (await H(([t]) => window.__h.gotSince(t, 'answered'), [s])).find((a) => a.id === ap.id);
            let left = false;
            for (let i = 0; i < 30 && !left; i++) { await sleep(200); left = !((await S()).approvals || []).some((a) => a.id === ap.id); }
            R('answer Yes from the web', ans?.ok && left ? 'pass' : 'fail', `${ans ? (ans.ok ? ans.result : ans.error) : 'no answer'}; ${left ? 'prompt gone' : 'still waiting'}; buttons [${btns}]`);
          }
          await closeSheets();
        } catch (e) { R('answer', 'fail', e.message); await closeSheets(); }
      }
      await sleep(500);
    }
    for (const h of wantH) if (!done.has(h)) rec('approval', 'answer from the web', h, 'skip', `no approval in ${cfg.approval_s || 90} s`);
  }

  // One tiny real turn per CLI, typed through the Prompt box (the text,
  // then Enter once it shows) and its reply on the screen.
  if (want('turn')) {
    for (const t of tabs) {
      if (!t.typing) { rec('turn', 'reply with the word ok', t.harness, 'skip', 'no composer (login or start up screen)', { tab: t.id }); continue; }
      try {
        await H((id) => window.__h.show(id), t.id);
        await waitReady(t.id);
        await clearLine(t.uid);
        await openPrompt();
        await page.fill('#sheet-body textarea', 'reply with the word ok');
        const t0 = Date.now();
        await page.locator('#sheet-body button.btn.pri', { hasText: 'Send' }).first().click();
        let worked = false, done = false, txt = '';
        while (Date.now() - t0 < 150000 && !done) {
          await sleep(300);
          const st = (await S()).tabs.find((x) => x.id === t.id)?.state;
          if (st === 'working' || st === 'background') worked = true;
          // The whole buffer: the reply after the last copy of the prompt.
          txt = await H(() => window.__h.allText());
          const after = txt.split('reply with the word ok').slice(-1)[0] || '';
          const okLine = /^\W{0,6}ok\b/im;
          done = st === 'ready' && Date.now() - t0 > 1500 && (txt.includes('reply with the word ok') ? okLine.test(after) : worked && okLine.test(txt));
        }
        rec('turn', 'reply with the word ok', t.harness, done ? 'pass' : 'fail', `${((Date.now() - t0) / 1000).toFixed(0)} s, working seen ${worked}${done ? '' : ': ' + JSON.stringify(txt.split('\n').filter((l) => l.trim()).slice(-4).join(' | ')).slice(0, 200)}`, { tab: t.id });
      } catch (e) { rec('turn', 'reply with the word ok', t.harness, 'fail', e.message, { tab: t.id }); }
    }
  }

  // Close with confirm, on a tab opened for it.
  if (want('options') && !cfg.safe && cfg.open_account) {
    try {
      const before = (await S()).tabs.map((x) => x.id);
      const r = await H((a) => window.__h.tool('open_tab', { account: a, name: 'qa-close' }, { user_says: 'open a tab' }), cfg.open_account);
      let nt = null;
      for (let i = 0; i < 30 && !nt; i++) { await sleep(200); nt = (await S()).tabs.find((x) => !before.includes(x.id)); }
      if (!nt) rec('options', 'close with confirm', '', 'fail', 'no tab opened: ' + JSON.stringify(r).slice(0, 120));
      else {
        await H((id) => window.__h.tool('pin_tab', { tab: id }), nt.id);
        const c1 = await H((id) => window.__h.tool('close_tabs', { tab: id }), nt.id);
        let gone = false, asked = !!c1.needs_confirmation;
        if (asked) await H((tok) => window.__h.tool('close_tabs', { confirm_token: tok }, { user_says: 'yes' }), c1.token);
        for (let i = 0; i < 30 && !gone; i++) { await sleep(200); gone = !(await S()).tabs.some((x) => x.id === nt.id); }
        rec('options', 'close a pinned tab asks first', '', asked && gone ? 'pass' : 'fail', `asked ${asked}: ${(c1.question || c1.error || '').slice(0, 100)}; closed ${gone}`);
      }
    } catch (e) { rec('options', 'close with confirm', '', 'fail', e.message); }
  }

  // The assistant chat, a web turn, and its web_* tools.
  if (want('chat')) {
    try {
      if (PHONE) await page.click('#b-chat'); else await page.click('#b-chat');
      await sleep(300);
      const s = await now();
      await page.fill('#say', 'which tabs are working?');
      await page.click('#b-send');
      let reply = null;
      for (let i = 0; i < 100 && !reply; i++) { await sleep(300); reply = (await H(([t]) => window.__h.gotSince(t, 'assistant'), [s])).find((m) => m.role === 'assistant' && m.done && m.id !== 'live'); }
      const said = (await H(([t]) => window.__h.jsonSince(t, 'say'), [s]))[0];
      rec('chat', 'web turn', '', reply ? 'pass' : 'fail', reply ? reply.text.slice(0, 100) : `no reply (say sent: ${!!said})`);
      const s2 = await now();
      if (!(await page.locator('#say').isVisible())) await page.click('#b-chat');
      await sleep(200);
      await page.fill('#say', 'show two panes');
      await page.click('#b-send');
      let ui = null;
      for (let i = 0; i < 60 && !ui; i++) { await sleep(300); ui = (await H(([t]) => window.__h.gotSince(t, 'ui'), [s2]))[0]; }
      rec('chat', 'web_layout reaches this browser', '', ui ? 'pass' : 'skip', ui ? JSON.stringify(ui).slice(0, 120) : 'no ui frame (the assistant did not call web_layout)');
      // An image into the chat.
      const png = Buffer.from('iVBORw0KGgoAAAANSUhEUgAAAAgAAAAICAIAAABLbSncAAAAEklEQVR4nGP4z8CAFWEXHbQSACj/P8Fu7N9hAAAAAElFTkSuQmCC', 'base64');
      for (let i = 0; i < 120 && (await H(async () => (await window.__h.S()).assistant?.busy)); i++) await sleep(500);
      const s3 = await now();
      if (!(await page.locator('#say').isVisible())) await page.click('#b-chat');
      await sleep(200);
      await page.setInputFiles('#f-img', { name: 'qa.png', mimeType: 'image/png', buffer: png });
      await sleep(300);
      await page.fill('#say', 'what is in this image?');
      await page.click('#b-send');
      await sleep(800);
      const sayImg = (await H(([t]) => window.__h.jsonSince(t, 'say'), [s3]))[0];
      let um = null;
      for (let i = 0; i < 160 && !um; i++) { await sleep(250); um = (await H(([t]) => window.__h.gotSince(t, 'assistant'), [s3])).find((m) => m.role === 'user' && /image 1/.test(m.text || '')); }
      rec('chat', 'image into the chat', '', sayImg?.images?.length === 1 && um ? 'pass' : 'fail', `say images ${sayImg?.images?.length}; ${um ? um.text.slice(0, 80) : 'no user message naming it'}`);
      await page.click('#b-chat-x').catch(() => {});
      if (!PHONE) await H(async () => { const { panes } = await window.__h.mod('term'); panes.setN(1); });
    } catch (e) { rec('chat', 'web turn', '', 'fail', e.message); }
  }

  // Voice: push to talk with Chromium's fake microphone.
  if (want('voice')) {
    if (ENGINE !== 'chromium') rec('voice', 'push to talk', '', 'skip', 'fake microphone: Chromium only');
    else {
      try {
        await H(async () => (await window.__h.mod('chat')).setChat(false));
        await closeSheets();
        await sleep(300);
        const s = await now();
        await page.click('#b-ptt');
        await sleep(300);
        await page.locator('button.mm-i[data-m="push"]').first().click();
        await sleep(800);
        const talk = page.locator('#talk').first();
        const box = await talk.boundingBox();
        if (!box) throw new Error('no Talk button');
        await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
        await page.mouse.down();
        await sleep(2000);
        await page.mouse.up();
        await sleep(1200);
        const sent = await H(([t]) => window.__gt.sent.filter((x) => x.at >= t), [s]);
        const mic = sent.filter((x) => x.kind === 3 || x.kind === 4).length;
        const v = sent.filter((x) => x.j?.t === 'voice').map((x) => JSON.stringify(x.j)).join(' ');
        const replies = (await H(([t]) => window.__h.gotSince(t, 'voice'), [s])).map((m) => JSON.stringify(m)).join(' ');
        rec('voice', 'push to talk', '', mic > 0 && /talk":true/.test(v) && /talk":false/.test(v) ? 'pass' : 'fail', `${mic} mic frames; sent ${v.slice(0, 160)}; got ${replies.slice(0, 200)}`);
        await page.click('#b-ptt');
        await sleep(200);
        await page.locator('button.mm-i[data-m="off"]').first().click().catch(() => {});
      } catch (e) { rec('voice', 'push to talk', '', 'fail', e.message); }
    }
  }

  // Stress in the page: rapid tab switching, rotation, keyboard loops.
  if (want('stress')) {
    try {
      const ids = tabs.map((t) => t.id);
      const s = await now();
      for (let i = 0; i < 40; i++) { await H((id) => window.__h.show(id), ids[i % ids.length]); await sleep(40); }
      const last = ids[39 % ids.length];
      const ok = await waitReady(last, 8000);
      const subs = (await H(([t]) => window.__h.jsonSince(t, 'subscribe'), [s])).length;
      rec('stress', 'rapid tab switching', '', ok ? 'pass' : 'fail', `40 switches over ${ids.length} tabs, ${subs} subscribes, last ready ${ok}`);
      if (PHONE) {
        await page.setViewportSize({ width: 932, height: 430 });
        await sleep(1200);
        const p1 = await H(() => window.__h.pane());
        await page.setViewportSize({ width: 430, height: 932 });
        await sleep(1200);
        const p2 = await H(() => window.__h.pane());
        rec('stress', 'rotation', '', p1.ready && p2.ready && p2.cols < p1.cols ? 'pass' : 'fail', `landscape ${p1.cols}x${p1.rows}, portrait ${p2.cols}x${p2.rows}`);
        const s2 = await now();
        for (let i = 0; i < 10; i++) {
          await H(() => { window.__h.ta()?.focus(); document.body.classList.add('kb'); });
          await sleep(120);
          await H(() => { window.__h.ta()?.blur(); document.body.classList.remove('kb'); });
          await sleep(120);
        }
        await sleep(1500);
        const rs = (await H(([t]) => window.__h.jsonSince(t, 'resize'), [s2])).filter((r) => !r.release);
        const rows = new Set(rs.map((r) => r.rows));
        rec('stress', 'keyboard open/close loops', '', rows.size <= 1 ? 'pass' : 'fail', `${rs.length} resizes, rows ${[...rows]}`);
      }
    } catch (e) { rec('stress', 'page', '', 'fail', e.message); }
  }

  // A long run: the page's memory and the frames it keeps.
  if (cfg.soak_s) {
    const samples = [];
    const end = Date.now() + cfg.soak_s * 1000;
    const ids = tabs.map((t) => t.id);
    let i = 0;
    while (Date.now() < end) {
      if (i % 6 === 0) await H((id) => window.__h.show(id), ids[(i / 6) % ids.length | 0]);
      const m = await H(() => ({ heap: performance.memory?.usedJSHeapSize || 0, nodes: document.getElementsByTagName('*').length, got: window.__gt.got.length }));
      let cdp = null;
      if (ENGINE === 'chromium') {
        try { const c = await ctx.newCDPSession(page); await c.send('Performance.enable'); const r = await c.send('Performance.getMetrics'); cdp = Object.fromEntries(r.metrics.map((x) => [x.name, x.value])); await c.detach(); } catch {}
      }
      samples.push({ t: Date.now(), ...m, js: cdp?.JSHeapUsedSize, docs: cdp?.Documents, listeners: cdp?.JSEventListeners });
      fs.writeFileSync(cfg.out.replace(/\.jsonl$/, '') + `-soak-page-${ENGINE}-${DEVICE}.json`, JSON.stringify(samples));
      i++;
      await sleep(10000);
    }
    const a = samples.slice(0, Math.max(1, samples.length / 4 | 0)), b = samples.slice(-Math.max(1, samples.length / 4 | 0));
    const avg = (xs, k) => xs.reduce((s, x) => s + (x[k] || 0), 0) / xs.length;
    const heap0 = avg(a, 'js') || avg(a, 'heap'), heap1 = avg(b, 'js') || avg(b, 'heap');
    const n0 = avg(a, 'nodes'), n1 = avg(b, 'nodes');
    const grow = heap0 ? heap1 / heap0 : 1;
    rec('soak', 'page memory', '', !heap0 ? 'skip' : grow < 1.5 && n1 < n0 * 1.5 ? 'pass' : 'fail', heap0 ? `JS heap ${(heap0 / 1e6).toFixed(1)} -> ${(heap1 / 1e6).toFixed(1)} MB (x${grow.toFixed(2)}), DOM nodes ${n0.toFixed(0)} -> ${n1.toFixed(0)}, ${samples.length} samples` : 'no heap numbers in this engine');
  }

  const errs = [...pageErrors, ...(await H(() => window.__gt.errors))];
  rec('page', 'no page errors', '', errs.length ? 'fail' : 'pass', errs.slice(0, 5).join(' | '));
  await shot('end');
  await browser.close();
  out.end();
}

main().catch(async (e) => { rec('page', 'driver', '', 'fail', e.stack || e.message); out.end(); process.exitCode = 1; await browser?.close().catch(() => {}); });
