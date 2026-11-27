import { createRequire } from 'module';
import fs from 'fs';
import path from 'path';
import { appsChecks } from './web_gate_apps.mjs';
import { voiceBarChecks } from './web_gate_voicebar.mjs';
import { routeChecks } from './web_gate_route.mjs';
import { asksChecks } from './web_gate_asks.mjs';
import { sessionsChecks } from './web_gate_sessions.mjs';

const argv = process.argv.slice(2);
const arg = (k, d) => { const i = argv.indexOf('--' + k); return i < 0 ? d : (argv[i + 1]?.startsWith('--') || argv[i + 1] == null ? true : argv[i + 1]); };
const URL0 = arg('url');
if (!URL0) { console.error('need --url (the dev mock)'); process.exit(2); }
const OUT = path.resolve(arg('out', path.join('e2e-results', 'gate-' + new Date().toISOString().replace(/[-:T]/g, '').slice(0, 14))));
const ENGINES = arg('engine', 'chromium') === 'both' ? ['chromium', 'webkit'] : [arg('engine', 'chromium')];
const ONLY = arg('only', '');
const PW = process.env.PW_DIR || path.join(path.dirname(new URL(import.meta.url).pathname), '..', '..', 'web', 'dev', 'node_modules');
const { chromium, webkit, devices } = createRequire(PW.endsWith('/') ? PW : PW + '/')('playwright');
const origin = new URL(URL0).origin;

const SLOW = +(process.env.GATE_SLOW || 0.35);
const sleep = (ms) => new Promise((r) => setTimeout(r, Math.max(40, ms * SLOW)));

const SIZES = [
  { id: '320x640', w: 320, h: 640, touch: true },
  { id: '390x844', w: 390, h: 844, touch: true },
  { id: '820x1180', w: 820, h: 1180, touch: true },
  { id: '1440x900', w: 1440, h: 900, touch: false },
  { id: '2560x1440', w: 2560, h: 1440, touch: false },
].filter((s) => { const f = String(arg('sizes', '')); return !f || f.split(',').some((x) => s.id.startsWith(x)); });

const problems = [];
const passed = [];
const notes = [];
fs.mkdirSync(OUT, { recursive: true });

function pageSide() {
  window.__g = {
    mods: {},
    async mod(n) {
      const l = document.querySelector('link[rel=modulepreload][href*="js/main.js"]');
      const u = l.href.replace(/\?.*$/, '').replace(/js\/[a-z]+\.js$/, `js/${n}.js`);
      return (this.mods[n] ||= await import(u));
    },

    audit(roots) {
      const out = [];
      const vw = innerWidth, vh = innerHeight;
      const SEL = 'button, input, select, textarea, a[href], [role=button], [role=menuitem], [role=menuitemradio], [role=menuitemcheckbox], summary';
      const desc = (e) => {
        const t = (e.getAttribute('aria-label') || e.textContent || e.placeholder || e.title || '').trim().replace(/\s+/g, ' ').slice(0, 40);
        return `${e.tagName.toLowerCase()}${e.id ? '#' + e.id : ''}${e.className && typeof e.className === 'string' ? '.' + e.className.trim().split(/\s+/).slice(0, 2).join('.') : ''} "${t}"`;
      };
      const shown = (e) => {

        if (e.closest('[hidden], [aria-hidden=true], .sr-only, .xterm, template, .msg:not(:last-child) .acts')) return false;
        const dt = e.closest('details:not([open])'); if (dt && !(e.tagName === 'SUMMARY' && e.parentElement === dt)) return false;
        if (e.tagName === 'INPUT' && (e.type === 'hidden' || e.type === 'file')) return false;
        for (let x = e; x && x !== document.documentElement; x = x.parentElement) {
          const cs = getComputedStyle(x);
          if (cs.display === 'none' || cs.visibility === 'hidden') return false;
        }
        return true;
      };
      const seen = new Set();

      const modal = document.querySelector('#sheet.on') ? '#sheet' : (document.querySelector('#pop:not([hidden])') ? '#pop' : '');
      for (const sel of roots) {
        const root = typeof sel === 'string' ? document.querySelector(sel) : sel;
        if (!root || !shown(root)) continue;
        if (modal && typeof sel === 'string' && sel !== modal && sel !== '#toasts') continue;

        const rr = root.getBoundingClientRect();
        if (rr.right > vw + 1 || rr.left < -1 || rr.bottom > vh + 1 || rr.top < -1) out.push({ el: desc(root), kind: 'container outside the viewport', detail: `${Math.round(rr.left)},${Math.round(rr.top)} ${Math.round(rr.width)}x${Math.round(rr.height)} in ${vw}x${vh}` });
        const body = root.querySelector('#sheet-body') || root;
        if (body.scrollWidth > body.clientWidth + 1 && getComputedStyle(body).overflowX === 'visible' && [...body.querySelectorAll('*')].some((x) => x.getBoundingClientRect().right > body.getBoundingClientRect().right + 1 && shown(x) && !x.closest('.chips, .st-nav, #keybar, .side-grip'))) { const br = body.getBoundingClientRect(); const w = [...body.querySelectorAll('*')].find((x) => x.getBoundingClientRect().right > br.right + 1 && shown(x) && !x.closest('.chips, .st-nav, #keybar, .side-grip')); out.push({ el: desc(body), kind: 'sideways overflow', detail: `${body.scrollWidth} > ${body.clientWidth}${w ? '; first to stick out: ' + desc(w) + ' to ' + Math.round(w.getBoundingClientRect().right) + ' of ' + Math.round(br.right) : ''}` }); }
        for (const e of root.querySelectorAll(SEL)) {
          if (seen.has(e) || !shown(e)) continue;
          seen.add(e);
          try { e.scrollIntoView({ block: 'nearest', inline: 'nearest' }); } catch {}
          const r = e.getBoundingClientRect(), cs = getComputedStyle(e);
          const d = desc(e);
          const bad = (kind, detail = '') => out.push({ el: d, kind, detail });
          if (r.width < 4 || r.height < 4) { bad('zero size', `${r.width}x${r.height}`); continue; }
          let op = 1;
          for (let x = e; x && x !== document.documentElement; x = x.parentElement) op *= +getComputedStyle(x).opacity;
          if (op < 0.5 && !e.disabled && e.getAttribute('aria-disabled') !== 'true') bad('invisible', 'opacity ' + op.toFixed(2));
          if (cs.pointerEvents === 'none' || (e.parentElement && getComputedStyle(e.parentElement).pointerEvents === 'none')) bad('not clickable', 'pointer-events: none');
          if (r.left < -1 || r.top < -1 || r.right > vw + 1 || r.bottom > vh + 1) bad('outside the viewport', `${Math.round(r.left)},${Math.round(r.top)}..${Math.round(r.right)},${Math.round(r.bottom)} in ${vw}x${vh}`);

          for (let x = e.parentElement; x && x !== document.body && x !== document.documentElement; x = x.parentElement) {
            const c = getComputedStyle(x);
            if (c.overflowX === 'visible' && c.overflowY === 'visible') continue;
            const a = x.getBoundingClientRect();
            const cutX = c.overflowX !== 'visible' && (r.left < a.left - 1.5 || r.right > a.right + 1.5);
            const cutY = c.overflowY !== 'visible' && (r.top < a.top - 1.5 || r.bottom > a.bottom + 1.5);
            if (cutX || cutY) { bad('clipped by ' + desc(x), `control ${Math.round(r.left)},${Math.round(r.top)}..${Math.round(r.right)},${Math.round(r.bottom)} in ${Math.round(a.left)},${Math.round(a.top)}..${Math.round(a.right)},${Math.round(a.bottom)}`); break; }
          }
          const cx = Math.min(vw - 1, Math.max(0, r.left + r.width / 2)), cy = Math.min(vh - 1, Math.max(0, r.top + r.height / 2));
          const hit = document.elementFromPoint(cx, cy);
          if (!hit || !(e.contains(hit) || hit.contains(e) || (e.closest('label') && e.closest('label') === hit.closest('label')))) bad('covered by ' + (hit ? desc(hit) : 'nothing'), `at ${Math.round(cx)},${Math.round(cy)}`);
          const isBtn = e.tagName === 'BUTTON' || e.getAttribute('role') === 'button';
          if (isBtn) {
            const ell = cs.textOverflow === 'ellipsis';
            const must = e.matches('.btn, .quick button, .appr-btns button, .acts button, #keybar button, .tr2 .tb, #b-ksend, .mm-i, .chip');
            if ((!ell || must) && e.scrollWidth > e.clientWidth + 1 && !(ell && !must)) bad('text clipped', `scrollWidth ${e.scrollWidth} > ${e.clientWidth}`);
            for (const s of e.querySelectorAll('span, b, small')) {
              const sc = getComputedStyle(s);
              if (sc.textOverflow !== 'ellipsis' && s.clientWidth && s.scrollWidth > s.clientWidth + 1 && sc.overflow !== 'visible') bad('text clipped', `inner ${desc(s)} ${s.scrollWidth} > ${s.clientWidth}`);
            }
            if (e.scrollHeight > e.clientHeight + 3 && cs.overflowY !== 'visible') bad('text clipped', `height ${e.scrollHeight} > ${e.clientHeight}`);
          }
        }
      }
      return out;
    },
  };
}

const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '');

async function run() {
  for (const engine of ENGINES) {
    const bt = engine === 'webkit' ? webkit : chromium;
    for (const size of SIZES) {
      const browser = await bt.launch(engine === 'chromium' ? { args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream'] } : {});
      const opts = size.touch
        ? { viewport: { width: size.w, height: size.h }, deviceScaleFactor: 2, isMobile: true, hasTouch: true, userAgent: devices['iPhone 15 Pro'].userAgent }
        : { viewport: { width: size.w, height: size.h }, deviceScaleFactor: 1 };
      if (engine === 'chromium') opts.permissions = ['microphone', 'clipboard-read', 'clipboard-write'];
      if (engine === 'chromium' && opts.userAgent) {  }
      const ctx = await browser.newContext(opts);
      const page = await ctx.newPage();
      const label = `${engine} ${size.id}`;
      const shotDir = path.join(OUT, engine === 'chromium' && ENGINES.length === 1 ? '' : engine, size.id);
      fs.mkdirSync(shotDir, { recursive: true });
      const errors = [];
      page.on('pageerror', (e) => { if (!/ResizeObserver loop/.test(e.message)) errors.push((e.stack || e.message).split('\n').slice(0, 3).join(' / ')); });
      page.on('console', (m) => { if (m.type() === 'error' && !/GL Driver|WebGL|favicon|WebSocket connection|ResizeObserver loop|Failed to load resource/.test(m.text())) errors.push('console: ' + m.text()); });
      await fetch(origin + '/dev/reset').catch(() => {});
      const t0 = Date.now();
      await page.goto(URL0);
      await page.waitForFunction(() => document.querySelector('#conn')?.classList.contains('live'), null, { timeout: 20000 });
      await page.evaluate(pageSide);
      await page.evaluate(async () => { for (const n of ['util', 'state', 'term', 'sheet', 'manage']) await window.__g.mod(n); });
      for (let i = 0; i < 50 && !(await page.evaluate(async () => (await window.__g.mod('state')).S.tabs.length)); i++) await sleep(100);
      await sleep(600);
      const ctxo = { page, engine, size, label, shotDir, isDesk: await page.evaluate(() => matchMedia('(min-width:760px) and (min-height:560px)').matches) };
      const S = () => page.evaluate(async () => { const { S } = await window.__g.mod('state'); return { tabs: S.tabs.map((t) => ({ id: t.id, uid: t.uid, name: t.name, state: t.state, account: t.account, pinned: !!t.pinned })), accounts: S.accounts, approvals: S.approvals || [], moved: S.moved || {} }; });
      ctxo.S = S;
      const R = {
        ok: (screen, what) => passed.push(`${label} ${screen}${what ? ': ' + what : ''}`),
        fail: (screen, kind, el, detail = '') => problems.push({ label, screen, kind, el, detail }),
      };
      ctxo.R = R;
      const shot = async (screen) => { await page.screenshot({ path: path.join(shotDir, slug(screen) + '.png') }).catch(() => {}); };
      ctxo.shot = shot;

      const audit = async (screen, roots) => {
        if (!/message/.test(screen)) await page.evaluate(() => document.querySelector('#toasts')?.replaceChildren());

        await page.evaluate(() => new Promise((res) => {
          const sels = ['#sheet', '#side', '#pop', '#tabpanel', '#chat'];
          const snap = () => sels.map((s) => { const e = document.querySelector(s); if (!e) return ''; const r = e.getBoundingClientRect(); return [r.left, r.top, r.width, r.height].map((v) => v.toFixed(1)).join(','); }).join('|');
          let last = '', same = 0, n = 0;
          const tick = () => { const k = snap(); same = k === last ? same + 1 : 0; last = k; if (same >= 4 || ++n > 90) res(); else requestAnimationFrame(tick); };
          requestAnimationFrame(tick);
        }));
        const found = await page.evaluate((r) => window.__g.audit(r), roots);
        await shot(screen);
        const keys = new Set();
        for (const f of found) { const k = f.kind + f.el; if (keys.has(k)) continue; keys.add(k); R.fail(screen, f.kind, f.el, f.detail); }
        if (!found.length) R.ok(screen);
        return found.length;
      };
      ctxo.audit = audit;
      const want = (name) => !ONLY || name.toLowerCase().includes(String(ONLY).toLowerCase());
      ctxo.want = want;
      const closeAll = async () => {
        await page.evaluate(async () => {
          const { closeSheet } = await window.__g.mod('sheet');
          closeSheet();
          (await window.__g.mod('manage')).setPanel(false);
          (await window.__g.mod('chat')).setChat(false);
          document.querySelector('#side')?.classList.remove('open');
          document.querySelector('#toasts')?.replaceChildren();
        }).catch(() => {});
        await sleep(120);
      };
      ctxo.closeAll = closeAll;
      const step = async (name, fn) => {
        if (!want(name)) return;
        try { await fn(); } catch (e) { R.fail(name, 'the check itself broke', '', String(e.message).split('\n')[0].slice(0, 300)); await shot(name + '-error'); }
        await closeAll();
      };
      ctxo.step = step;

      await screens(ctxo);
      await combos(ctxo);
      await flows(ctxo);
      await appsChecks(ctxo, { sleep, origin });
      await voiceBarChecks(ctxo, { sleep, problems });
      await routeChecks(ctxo, { sleep });
      await asksChecks(ctxo, { sleep, origin });
      await sessionsChecks(ctxo, { sleep, origin });

      if (errors.length) for (const e of [...new Set(errors)].slice(0, 5)) R.fail('page', 'page error', '', e.slice(0, 300));
      else R.ok('page', 'no page errors');
      console.log(`[${label}] done in ${((Date.now() - t0) / 1000).toFixed(0)} s`);
      await browser.close();
    }
  }
}

async function clickText(c, re, scopes = '#sheet-body, #tp-body, #pop', tag = 'button, [role=menuitem], .li', timeout = 4000) {
  const loc = c.page.locator(scopes.split(',').map((s) => `${s.trim()} :is(${tag})`).join(', '), { hasText: re }).filter({ visible: true }).first();
  await loc.click({ timeout });
}

const movable = (s) => s.tabs.find((x) => x.state === 'ready' && !x.pinned && (s.accounts.find((a) => a.account === x.account)?.harness || 'claude') === 'claude');
const target = (s, t) => t && s.accounts.find((a) => a.account !== t.account && (a.harness || 'claude') === 'claude' && a.available !== false && a.logged_in !== false);

async function screens(c) {
  const { page, isDesk, step, audit } = c;
  const OV = ['#sheet', '#pop', '#tabpanel'];
  const tabId = async () => (await c.S()).tabs.find((t) => t.state === 'ready' && !t.pinned)?.id || (await c.S()).tabs[0].id;
  const show = async (id) => { await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), id); await sleep(200); };
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);

  await step('home', async () => {
    const roots = ['#top', '#keyrow', '#sbar', '#panes .pane-head'];
    await audit('home', roots);
    if (isDesk) await audit('home-tab-list', ['#side']);
    else { await page.click('#b-menu'); await sleep(350); await audit('home-tab-list', ['#side']); await c.closeAll(); }
  });
  await step('settings', async () => {
    await open(`(await mod('settings')).settingsSheet()`);
    await sleep(700);
    const names = await page.evaluate(() => [...document.querySelectorAll('#sheet .st-nav button')].map((b) => b.textContent.trim()));
    if (!names.length) c.R.fail('settings', 'no section strip', '#sheet .st-nav');
    for (let i = 0; i < names.length; i++) {
      await page.evaluate((i) => document.querySelectorAll('#sheet .st-nav button')[i]?.click(), i);
      await sleep(350);
      await audit('settings ' + names[i], OV);
    }
  });
  await step('sessions', async () => { await open(`(await mod('sessions')).sessionsSheet()`); await page.waitForSelector('#sheet-body .srow, #sheet-body .empty, #sheet-body .fail', { timeout: 6000 }).catch(() => {}); await sleep(300); await audit('sessions', OV); });
  await step('manage', async () => { await open(`(await mod('manage')).manageSheet()`); await sleep(500); await audit('manage', OV); });
  await step('accounts', async () => {
    await open(`(await mod('accounts')).accountsSheet()`); await sleep(700); await audit('accounts', OV);

    const b = page.locator('#sheet-body button', { hasText: /^(Add|Edit|New)/ }).filter({ visible: true }).first();
    if (await b.count()) { await b.click({ timeout: 3000 }).catch(() => {}); await sleep(400); await audit('accounts form', OV); }
  });
  await step('git', async () => { const id = await tabId(); await open(`const { S } = await mod('state'); (await mod('git')).gitSheet(S.tabs.find((t) => t.id === a[0]))`, id); await sleep(900); await audit('git', OV); });
  await step('new tab', async () => { await open(`(await mod('manage')).newTabSheet()`); await sleep(400); await audit('new tab', OV); });
  await step('antigravity', async () => {
    const name = 'antigravity';
    const s = await c.S();
    const a = s.accounts.find((x) => x.harness === 'antigravity');
    const t = a && s.tabs.find((x) => x.account === a.account);
    if (!a || !t) { c.R.fail(name, 'no Antigravity account with a tab in the state', '', JSON.stringify(s.accounts.map((x) => x.harness))); return; }
    if (!isDesk) { await page.click('#b-menu'); await sleep(350); }
    const row = await page.evaluate(([id, label]) => {
      const el = document.querySelector(`#side [data-id="${id}"], #side [data-tab="${id}"]`);
      const scope = el?.closest('[data-id]') || el;
      const badge = scope?.querySelector('.hb-antigravity');
      const head = [...document.querySelectorAll('#side .acct .an')].find((e) => e.textContent.includes(label));
      return { found: !!el, badge: !!badge, title: badge?.getAttribute('title') || '', glyph: !!badge?.querySelector('svg path'), head: head?.textContent || '' };
    }, [t.id, a.label]);
    if (!row.found) c.R.fail(name, 'its tab is not in the tab list', '#side', t.id);
    if (!row.badge || !row.glyph) c.R.fail(name, 'its tab has no Antigravity mark', '.hb-antigravity', JSON.stringify(row));
    if (row.title !== 'Antigravity') c.R.fail(name, 'the mark is not named Antigravity', '.hb-antigravity', row.title);
    if (!/antigravity/.test(row.head)) c.R.fail(name, 'the account heading does not say antigravity', '#side .acct .an', row.head);
    await audit('antigravity tab list', ['#side']);
    if (!isDesk) await c.closeAll();
    await open(`(await mod('manage')).newTabSheet()`);
    await sleep(400);
    const pick = await page.evaluate((n) => {
      const sel = document.querySelector('#sheet-body select');
      const o = [...(sel?.options || [])].find((x) => Number(x.value) === n);
      if (!o) return null;
      sel.value = String(n);
      sel.dispatchEvent(new Event('change', { bubbles: true }));
      return { text: o.textContent, disabled: o.disabled, picked: Number(sel.value) === n };
    }, a.account);
    if (!pick) c.R.fail(name, 'the new tab account picker does not list it', '#sheet-body select');
    else {
      if (!pick.text.includes(a.label) || !/antigravity/.test(pick.text)) c.R.fail(name, 'the picker entry does not name the account and its agent', '#sheet-body select', pick.text);
      if (pick.disabled || !pick.picked) c.R.fail(name, 'the account cannot be chosen for a new tab', '#sheet-body select', JSON.stringify(pick));
    }
    await audit('antigravity new tab', OV);
    await c.closeAll();
  });
  await step('approvals', async () => { await fetch(origin + '/dev/approval'); await sleep(900); await open(`(await mod('chat')).approvalsSheet()`); await sleep(300); await audit('approvals', OV); });
  await step('help', async () => { await open(`(await mod('keys')).helpSheet()`); await sleep(250); await audit('help', OV); });
  await step('prompt box', async () => { await show(await tabId()); await open(`(await mod('keybar')).promptSheet('prompt')`); await sleep(350); await audit('prompt box', OV); });
  await step('paste box', async () => { await show(await tabId()); await open(`(await mod('keybar')).promptSheet('paste')`); await sleep(350); await audit('paste box', OV); });
  await step('update', async () => { await open(`(await mod('manage')).updateSheet({ state: 'downloaded', notes: 'Faster startup and a new sessions view.', update_with: 'Update and restart', page: 'https://example.com/r' }, 'Update available')`); await sleep(300); await audit('update', OV); });
  await step('install', async () => { await open(`(await mod('install')).install()`); await sleep(400); await audit('install', OV); });
  await step('voice sheet', async () => { await open(`(await mod('voice')).openVoiceSheet()`); await sleep(500); await audit('voice sheet', OV); });
  await step('mic menu', async () => { await page.click('#b-ptt'); await sleep(450); await audit('mic menu', OV); });
  await step('mic menu controls', async () => {
    const name = 'mic menu controls';
    const st = () => open(`const { S } = await mod('state'); const { voicePick, micIsMuted } = await mod('voice'); return { tab: S.mode_flags?.direct_tab ?? null, name: S.mode_flags?.direct ?? null, follow: S.voice?.direct_follow === true, pick: voicePick(), muted: micIsMuted() }`);
    const until = async (f, ms = 4000) => { for (let i = 0; i < ms / 100; i++) { if (f(await st())) return true; await new Promise((r) => setTimeout(r, 100)); } return false; };
    const need = (ok, kind, detail = '') => { if (!ok) c.R.fail(name, kind, '#pop.mic-pop', detail); return ok; };
    const menu = () => page.evaluate(() => {
      const g = (sel) => { const e = document.querySelector('#pop.mic-pop ' + sel); return e ? { role: e.getAttribute('role'), n: e.querySelectorAll('[role=radio]').length, on: [...e.querySelectorAll('[role=radio][aria-checked=true]')].map((b) => b.dataset.m), tab: [...e.querySelectorAll('[role=radio]')].filter((b) => b.tabIndex === 0).length } : null; };
      const p = document.querySelector('#pop.mic-pop');
      return { mic: g('.sg-mic'), talk: g('.sg-talk'), mute: p?.querySelector('.mp-mute')?.getAttribute('aria-checked'), muteText: p?.querySelector('.mp-mute .mm-st')?.textContent, note: p?.querySelector('.mp-micnote')?.textContent, who: p?.querySelector('.mp-who')?.textContent, vol: p?.querySelector('.vol-row .vr-v')?.textContent, volShown: !!p?.querySelector('.vol-row')?.getClientRects().length, typedShown: !!p?.querySelector('.mm-typed')?.getClientRects().length, typedText: p?.querySelector('.mm-typed .mm-t')?.textContent, spk: p?.querySelector('.mp-spk[role=switch]')?.getAttribute('aria-checked'), spkText: p?.querySelector('.mp-spk .mm-st')?.textContent, spkNote: p?.querySelector('.mp-spknote')?.getClientRects().length ? p.querySelector('.mp-spknote').textContent : '', ph: p?.getBoundingClientRect().height, typed: !!p?.querySelector('.mm-typed[role=switch]'), settings: !!p?.querySelector('.mp-set'), h: p ? Math.round(p.getBoundingClientRect().height) : 0, focus: document.activeElement?.dataset?.m || '' };
    });
    const seg = (g, m) => page.locator(`#pop.mic-pop .${g} .sg-b[data-m="${m}"]`).click({ timeout: 4000 });
    const tabs = await open(`const { S } = await mod('state'); return S.tabs.filter((t) => t.can_direct && t.state === 'ready').map((t) => ({ id: t.id, uid: t.uid, name: t.name }))`);
    if (!need(tabs.length >= 2, 'the mock has no two tabs that can take direct mode')) return;
    const [A, B] = tabs;
    await show(A.id);
    await page.click('#b-ptt'); await sleep(450);
    let m = await menu();
    need(m.mic?.role === 'radiogroup' && m.mic.n === 4 && m.mic.on.length === 1 && m.mic.tab === 1, 'the microphone control is not a radio group of four', JSON.stringify(m.mic));
    need(m.talk?.role === 'radiogroup' && m.talk.n === 3 && m.talk.on.join() === 'assistant' && m.talk.tab === 1, 'the Talk to control is not a radio group of three on Assistant', JSON.stringify(m.talk));
    need(/^\d+%$/.test(m.vol || '') && m.typed && m.settings, 'a row of the old menu is missing', JSON.stringify(m));
    need(m.spk === 'true' && m.spkText === 'On' && m.volShown && m.typedShown && !m.spkNote && /^Also speak replies to typed messages$/.test(m.typedText || ''), 'Spoken replies is not On with its volume and typed option', JSON.stringify(m));
    const onH = m.ph;
    await page.locator('#pop.mic-pop .mp-spk').click({ timeout: 4000 }); await sleep(300);
    m = await menu();
    need(m.spk === 'false' && m.spkText === 'Off' && !m.volShown && !m.typedShown && m.spkNote === 'Replies show as text only' && m.ph <= onH, 'Spoken replies off does not hide the volume and the typed option', JSON.stringify(m));
    need(await open(`const { S } = await mod('state'); return S.voice?.speaker_muted === true`), 'Spoken replies off did not mute the speaker', '');
    await audit('mic menu spoken replies off', OV);
    await page.locator('#pop.mic-pop .mp-spk').click({ timeout: 4000 }); await sleep(300);
    m = await menu();
    need(m.spk === 'true' && m.spkText === 'On' && m.volShown && m.typedShown && !m.spkNote, 'Spoken replies did not turn back on', JSON.stringify(m));
    need(m.h > 0 && m.h <= 480, 'the popup is taller than 480 px', String(m.h));
    await seg('sg-mic', 'push'); await sleep(500);
    m = await menu();
    need(m.mic?.on.join() === 'push' && (await st()).pick === 'push' && /Hold Talk/.test(m.note || ''), 'Push to talk did not take', JSON.stringify(m));
    await audit('mic menu push to talk', OV);
    if (!c.size.touch) {
      await page.locator('#pop.mic-pop .sg-mic .sg-b[data-m="push"]').focus();
      await page.keyboard.press('ArrowRight');
      m = await menu();
      need(m.focus === 'open' && m.mic.on.join() === 'push', 'the right arrow does not move inside the radio group', JSON.stringify(m));
    }
    await page.locator('#pop.mic-pop .mp-mute').click({ timeout: 4000 }); await sleep(300);
    m = await menu();
    need(m.mute === 'true' && m.muteText === 'Muted' && /^Muted/.test(m.note || '') && (await st()).muted, 'the mute switch does not say Muted', JSON.stringify(m));
    await audit('mic menu muted', OV);
    await page.locator('#pop.mic-pop .mp-mute').click({ timeout: 4000 }); await sleep(200);
    m = await menu();
    need(m.mute === 'false' && m.muteText === 'Live', 'the mute switch does not say Live', JSON.stringify(m));
    await seg('sg-talk', 'tab');
    need(await until((x) => x.tab === A.uid && !x.follow), 'This tab did not start direct mode here', JSON.stringify(await st()));
    await sleep(200);
    m = await menu();
    need(m.talk.on.join() === 'tab' && (m.who || '').includes('Talking to ' + A.name), 'This tab is not shown as picked', JSON.stringify(m));
    await audit('mic menu this tab', OV);
    await seg('sg-talk', 'follow');
    need(await until((x) => x.tab === A.uid && x.follow), 'Follow active tab did not turn following on', JSON.stringify(await st()));
    await sleep(200);
    m = await menu();
    need(m.talk.on.join() === 'follow', 'Follow active tab is not shown as picked', JSON.stringify(m));
    await audit('mic menu follow active tab', OV);
    await c.closeAll();
    await show(B.id);
    need(await until((x) => x.tab === B.uid), 'voice did not follow to the tab shown', JSON.stringify(await st()));
    const bar = () => page.evaluate(() => { const e = document.querySelector('#voice-move'); if (!e || e.hidden) return null; const r = e.getBoundingClientRect(); return { text: e.querySelector('.vm-t').textContent, in: r.left >= 0 && r.top >= 0 && r.right <= innerWidth + 1 && r.bottom <= innerHeight + 1 }; });
    need(!(await bar()), 'the Move here bar shows while following');
    await page.click('#b-ptt'); await sleep(400);
    await seg('sg-talk', 'tab');
    need(await until((x) => x.tab === B.uid && !x.follow), 'This tab did not make it sticky again', JSON.stringify(await st()));
    await c.closeAll();
    await show(A.id); await sleep(400);
    let b = await bar();
    need(b && b.in && b.text === `Voice is talking to ${B.name}.`, 'no Move here bar on another tab', JSON.stringify(b));
    need((await st()).tab === B.uid, 'voice moved although following is off');
    await audit('move here bar', ['#voice-move']);
    await page.click('#b-ptt'); await sleep(400);
    m = await menu();
    need(m.talk.on.length === 0 && /not the tab you see/.test(m.who || ''), 'the popup does not say voice is with another tab', JSON.stringify(m));
    await c.closeAll();
    await show(B.id); await sleep(400);
    need(!(await bar()), 'the Move here bar stays on the tab voice talks to');
    await show(A.id); await sleep(400);
    await page.locator('#voice-move .vm-x').click({ timeout: 4000 }); await sleep(200);
    need(!(await bar()), 'Dismiss does not hide the Move here bar');
    await show(B.id); await sleep(300);
    await show(A.id); await sleep(400);
    need(!!(await bar()), 'the Move here bar does not come back after a visit to the old tab');
    await page.locator('#voice-move .vm-go').click({ timeout: 4000 });
    need(await until((x) => x.tab === A.uid), 'Move here did not move voice', JSON.stringify(await st()));
    await sleep(300);
    need(!(await bar()), 'the Move here bar stays after the move');
    await page.click('#b-ptt'); await sleep(400);
    await seg('sg-talk', 'assistant');
    need(await until((x) => x.tab === null), 'Assistant did not end direct mode', JSON.stringify(await st()));
    await seg('sg-mic', 'off'); await sleep(300);
    need((await st()).pick === 'off', 'Off did not take');
    if (!problems.some((p) => p.label === c.label && p.screen === name)) c.R.ok(name);
  });
  await step('mic input picker', async () => {
    const name = 'mic input picker';
    if (c.engine !== 'chromium') return;
    const need = (ok, kind, detail = '') => { if (!ok) c.R.fail(name, kind, '#pop.mic-pop .mp-in', detail); return ok; };
    const menu = async () => { if (!(await page.locator('#pop.mic-pop').count())) { await page.click('#b-ptt'); await sleep(450); } };
    const seg = (m) => page.locator(`#pop.mic-pop .sg-mic .sg-b[data-m="${m}"]`).click({ timeout: 4000 });
    const get = () => page.evaluate(() => {
      const p = document.querySelector('#pop.mic-pop'), b = p?.querySelector('.mp-in'), s = b?.querySelector('select');
      const r = b?.getBoundingClientRect(), pr = p?.getBoundingClientRect();
      let saved = null;
      try { saved = JSON.parse(localStorage.getItem('gt.voice.input')); } catch {}
      return { shown: !!b && !b.hidden, opts: s ? [...s.options].map((o) => o.textContent) : null, val: s ? s.selectedOptions[0]?.textContent : null, one: b?.querySelector('.mp-in1')?.textContent ?? null,
        note: b?.querySelector('.mp-innote')?.textContent ?? '', h: r ? Math.round(r.height) : 0, inside: !!r && r.left >= pr.left && r.right <= pr.right + 1 && pr.right <= innerWidth,
        saved: saved?.label ?? null, calls: (window.__mic?.calls || []).slice(), strip: document.querySelector('#mic-strip .ms-t')?.textContent || '' };
    });
    const until = async (f, ms = 5000) => { let g; for (let i = 0; i < ms / 100; i++) { g = await get(); if (f(g)) return g; await new Promise((r) => setTimeout(r, 100)); } return null; };
    const J = (x) => JSON.stringify(x);
    await page.evaluate(() => {
      try { localStorage.removeItem('gt.voice.input'); } catch {}
      const md = navigator.mediaDevices, en = md.enumerateDevices.bind(md), gum = md.getUserMedia.bind(md);
      const M = window.__mic = { asked: true, drop: '', only: '', long: false, calls: [], en, gum };
      md.enumerateDevices = async () => {
        let l = (await en()).map((d) => ({ kind: d.kind, deviceId: d.deviceId, label: d.label, groupId: d.groupId }));
        if (!M.asked) l = l.map((d) => ({ ...d, deviceId: '', label: '' })).slice(0, 1);
        if (M.only) l = l.filter((d) => d.kind !== 'audioinput' || d.deviceId === 'default' || d.label === M.only);
        if (M.drop) l = l.filter((d) => d.label !== M.drop);
        if (M.long) l = l.map((d) => (d.label === 'Fake Audio Input 2' ? { ...d, label: 'Rehearsal Room Wireless Lavalier Receiver Channel B' } : d));
        return l;
      };
      md.getUserMedia = async (q) => { const s = await gum(q); M.asked = true; M.calls.push({ exact: !!q?.audio?.deviceId?.exact, label: s.getAudioTracks()[0]?.label || '' }); return s; };
      M.say = () => md.dispatchEvent(new Event('devicechange'));
    });
    const set = (o) => page.evaluate((o) => { Object.assign(window.__mic, o); window.__mic.say(); }, o);
    try {
      await menu();
      let g = await until((x) => x.opts?.length === 3);
      need(g && g.opts[0] === 'System default' && g.val === 'System default' && !g.note && g.inside, 'the picker does not list System default first and the inputs', J(await get()));
      await audit('mic menu input', OV);
      await set({ asked: false });
      g = await until((x) => !x.opts && x.one === 'System default');
      need(g && /first used/.test(g.note), 'before permission it is not System default with a hint', J(await get()));
      await audit('mic menu input before permission', OV);
      await seg('open'); await menu();
      g = await until((x) => x.opts?.length === 3 && /Open mic/.test(x.strip));
      need(g && !g.note && g.calls.length === 1 && !g.calls[0].exact, 'the list did not fill in after the first capture', J(await get()));
      await page.selectOption('#pop.mic-pop .mp-in select', { label: 'Fake Audio Input 2' });
      g = await until((x) => x.calls.length === 2);
      await menu();
      need(g && g.calls[1].exact && g.calls[1].label === 'Fake Audio Input 2' && g.saved === 'Fake Audio Input 2', 'picking an input while live did not switch capture to it', J(await get()));
      g = await until((x) => /Open mic · (listening|hearing you|heard you)/.test(x.strip));
      need(!!g, 'open mic is not listening after the switch', J(await get()));
      await set({ drop: 'Fake Audio Input 2' });
      g = await until((x) => /not connected/.test(x.val || ''));
      need(g && /^Not connected, using the system default$/.test(g.note) && g.saved === 'Fake Audio Input 2', 'a missing input is not said in one line or the choice was dropped', J(await get()));
      await audit('mic menu input not connected', OV);
      await seg('off'); await sleep(300);
      await seg('open'); await menu();
      g = await until((x) => x.calls.length === 3);
      need(g && !g.calls[2].exact && g.saved === 'Fake Audio Input 2', 'capture did not fall back to the default input', J(await get()));
      await set({ drop: '' });
      g = await until((x) => x.calls.length === 4);
      await menu();
      need(g && g.calls[3].exact && g.calls[3].label === 'Fake Audio Input 2' && (await until((x) => x.val === 'Fake Audio Input 2' && !x.note)), 'the saved input did not come back when it returned', J(await get()));
      await set({ long: true });
      g = await until((x) => /^Rehearsal Room/.test(x.val || ''));
      need(g && g.inside, 'a long device name does not fit the row', J(await get()));
      await audit('mic menu input long name', OV);
      await set({ long: false });
      await until((x) => x.val === 'Fake Audio Input 2');
      await page.selectOption('#pop.mic-pop .mp-in select', { label: 'System default' });
      g = await until((x) => x.calls.length === 5);
      need(g && !g.calls[4].exact && g.saved === null, 'System default did not clear the choice', J(await get()));
      await menu();
      await seg('off'); await sleep(300);
      await set({ only: 'Fake Audio Input 1' });
      g = await until((x) => !x.opts && x.one === 'Fake Audio Input 1');
      need(g && !g.note && g.h <= 48, 'one input is not shown as one compact line', J(await get()));
      await audit('mic menu input one device', OV);
    } finally {
      await page.evaluate(() => {
        const M = window.__mic, md = navigator.mediaDevices;
        if (!M) return;
        md.enumerateDevices = M.en; md.getUserMedia = M.gum;
        try { localStorage.removeItem('gt.voice.input'); } catch {}
        delete window.__mic;
        md.dispatchEvent(new Event('devicechange'));
      });
      await menu().catch(() => {});
      await seg('off').catch(() => {});
      await sleep(300);
    }
    if (!problems.some((p) => p.label === c.label && p.screen === name)) c.R.ok(name);
  });
  await step('assistant', async () => { await open(`(await mod('chat')).setChat(true)`); await sleep(500); await audit('assistant', ['#chat', '#top', '#keyrow']); });

  await step('tab options', async () => {
    const id = await tabId();
    await show(id);
    if (isDesk) { await open(`(await mod('manage')).setPanel(true)`); await sleep(500); }
    else { await open(`(await mod('manage')).tabActions(a[0])`, id); await sleep(450); }
    await audit('tab options', OV);
    const sections = [['Rename', /^Rename$/], ['Move', /^Move( to)?$/], ['Duplicate', /^Duplicate to/], ['Copy session', /^Copy session/], ['Switch model', /^Switch model/], ['Usage', /^Usage/], ['Accent color', /^Accent color/], ['Group', /^Group/], ['Loops and jobs', /^Loops and jobs/], ['Browser', /^Browser/], ['Git', /^Git/]];
    for (const [name, re] of sections) {
      const loc = page.locator('#sheet-body :is(button,.li), #tp-body :is(button,.li), #pop :is(button,.li)', { hasText: re }).filter({ visible: true }).first();
      if (!(await loc.count())) { if (['Rename', 'Move', 'Switch model'].includes(name)) c.R.fail('tab options ' + name, 'row missing', String(re)); continue; }
      if (await loc.isDisabled().catch(() => false)) continue;
      await loc.click({ timeout: 4000 });
      await sleep(name === 'Git' ? 1100 : 450);
      await audit('tab options ' + name, OV);
      const back = page.locator('#sheet-body button[aria-label=Back], #tp-body button[aria-label=Back], #pop button[aria-label=Back]').filter({ visible: true }).first();
      if (await back.count()) { await back.click({ timeout: 3000 }); await sleep(250); }
      else { await closeAndReopen(); }
    }
    async function closeAndReopen() {
      await c.closeAll(); await show(id);
      if (isDesk) { await open(`(await mod('manage')).setPanel(true)`); await sleep(400); } else { await open(`(await mod('manage')).tabActions(a[0])`, id); await sleep(400); }
    }

    if (isDesk) { await c.closeAll(); await open(`(await mod('manage')).tabActions(a[0])`, id); await sleep(400); await audit('tab options popover', OV); }
  });

  await step('confirm close', async () => {
    const pinned = (await c.S()).tabs.find((t) => t.pinned);
    if (!pinned) { c.R.ok('confirm close', 'no pinned tab in this world: skipped'); return; }
    await show(pinned.id);
    if (isDesk) { await open(`(await mod('manage')).setPanel(true)`); await sleep(400); } else { await open(`(await mod('manage')).tabActions(a[0])`, pinned.id); await sleep(400); }
    await clickText(c, /^Close tab/);
    await page.waitForFunction(() => document.querySelector('#sheet')?.classList.contains('on') && /Confirm/.test(document.querySelector('#sheet .sh-title')?.textContent || ''), null, { timeout: 4000 });
    await sleep(250);
    await audit('confirm close', OV);
    await clickText(c, /^No$/, '#sheet-body');
  });

  await step('move picker and move now', async () => {
    const s = await c.S();
    const t = movable(s);
    const to = target(s, t);
    if (!t || !to) { c.R.fail('move picker', 'no tab or target account in the mock', JSON.stringify(s.tabs.map((x) => [x.id, x.state, x.account, x.pinned]))); return; }
    await fetch(`${origin}/dev/busy?tab=${t.id}`); await sleep(500);
    await show(t.id);
    if (isDesk) { await open(`(await mod('manage')).setPanel(true)`); await sleep(400); } else { await open(`(await mod('manage')).tabActions(a[0])`, t.id); await sleep(400); }
    await clickText(c, /^Move( to)?$/); await sleep(400);
    await audit('move picker', OV);
    await clickText(c, new RegExp('^' + to.label)); 
    await page.waitForFunction(() => /busy/.test(document.querySelector('#sheet .sh-title')?.textContent || '') && document.querySelector('#sheet').classList.contains('on'), null, { timeout: 5000 });
    await sleep(350);
    await audit('move now sheet', OV);
    await clickText(c, /^Move now/, '#sheet-body');
    await page.waitForFunction(() => /Confirm/.test(document.querySelector('#sheet .sh-title')?.textContent || ''), null, { timeout: 5000 });
    await sleep(300);
    await audit('move now confirm', OV);
    await clickText(c, /^No$/, '#sheet-body');
    await fetch(`${origin}/dev/busy?tab=${t.id}&off=1`);
  });
}

async function combos(c) {
  const { page, isDesk, step, audit, size } = c;
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);
  const geometry = (sel) => page.evaluate((sel) => { const e = document.querySelector(sel); if (!e) return null; const r = e.getBoundingClientRect(), cs = getComputedStyle(e); return { hidden: e.hidden || cs.display === 'none' || cs.visibility === 'hidden', x: r.left, y: r.top, w: r.width, h: r.height, vw: innerWidth, vh: innerHeight }; }, sel);
  const talkOk = (g) => g && !g.hidden && g.w >= 40 && g.h >= (size.touch ? 40 : 28) && g.x >= 0 && g.y >= 0 && g.x + g.w <= g.vw + 1 && g.y + g.h <= g.vh + 1;

  await step('combo push to talk + assistant', async () => {
    await open(`(await mod('voice')).setMode('push')`); await sleep(700);
    const before = await geometry('#talk');
    if (!talkOk(before)) c.R.fail('combo push to talk', 'Talk button missing or off screen without the assistant', '#talk', JSON.stringify(before));
    await open(`(await mod('chat')).setChat(true)`); await sleep(700);
    const g = await geometry('#talk');
    if (!talkOk(g)) c.R.fail('combo push to talk + assistant', 'Talk button missing or off screen while the assistant is open', '#talk', JSON.stringify(g));
    await audit('combo push to talk + assistant', ['#chat', '#vbar', '#voice-bar', '#top', '#keyrow']);

    await open(`(await mod('voice')).setMicMuted(true)`); await sleep(500);
    await audit('combo mic muted + assistant', ['#chat', '#vbar', '#voice-bar', '#mic-strip', '#top']);
    await open(`(await mod('voice')).setMicMuted(false)`);
    await open(`(await mod('voice')).setMode('off')`); await sleep(300);
  });

  if (isDesk) await step('combo panel + assistant + squeeze', async () => {
    const vp = page.viewportSize();
    await page.setViewportSize({ width: 1000, height: 700 }); await sleep(500);
    await open(`(await mod('manage')).setPanel(true)`); await sleep(350);
    await open(`(await mod('chat')).setChat(true)`); await sleep(800);
    const g = await geometry('#panes');
    if (!g || g.w < 520) c.R.fail('combo panel + assistant + squeeze', 'the terminal is narrower than 520 px', '#panes', JSON.stringify(g));
    await audit('combo panel + assistant + squeeze', ['#tabpanel', '#chat', '#sbar', '#top', '#panes .pane-head']);
    await page.click('#b-menu'); await sleep(450);
    await audit('combo squeezed tab list', ['#side']);
    await page.setViewportSize(vp); await sleep(500);
  });

  await step('combo message over a sheet', async () => {
    const id = (await c.S()).tabs[0].id;
    await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), id);
    await open(`(await mod('keybar')).promptSheet('prompt')`); await sleep(350);
    await open(`(await mod('util')).toast('A message while a dialog is open', 'info', { label: 'Undo', fn() {} })`); await sleep(350);
    await audit('combo message over a sheet', ['#sheet']);
    const z = await page.evaluate(() => ({ toasts: +getComputedStyle(document.querySelector('#toasts')).zIndex, sheet: +getComputedStyle(document.querySelector('#sheet')).zIndex }));
    if (!(size.touch && !c.isDesk) || z.toasts < z.sheet) c.R.ok('combo message over a sheet', 'toast z ' + z.toasts + ' sheet z ' + z.sheet);
    else c.R.fail('combo message over a sheet', 'a message is over the sheet', '#toasts', JSON.stringify(z));
  });
}

async function flows(c) {
  const { page, isDesk, step, S } = c;
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);
  const show = async (id) => { await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), id); await sleep(250); };
  const focusedId = () => page.evaluate(async () => (await window.__g.mod('term')).panes.focused().id);
  const options = async (id) => {
    await show(id);
    if (isDesk) { await open(`(await mod('manage')).setPanel(true)`); await sleep(400); } else { await open(`(await mod('manage')).tabActions(a[0])`, id); await sleep(450); }
  };
  const waitFor = async (fn, arg, ms = 5000) => { try { await page.waitForFunction(fn, arg, { timeout: ms, polling: 100 }); return true; } catch { return false; } };

  await step('flow move', async () => {
    const s = await S();
    const t = movable(s), to = target(s, t);
    await options(t.id);
    await clickText(c, /^Move( to)?$/); await sleep(300);
    await clickText(c, new RegExp('^' + to.label));
    const gone = await waitFor((id) => !window.__g.mods.state.S.tabs.some((x) => x.id === id), t.id, 5000);
    await sleep(500);
    const s2 = await S();
    const now = s2.moved[t.id.replace(/^t/, '')] != null ? 't' + s2.moved[t.id.replace(/^t/, '')] : null;
    const fid = await focusedId();
    const label = (await page.evaluate(() => document.querySelector('#t-name')?.textContent)) || '';
    if (gone && now && fid === now && s2.tabs.some((x) => x.id === now && x.account === to.account)) c.R.ok('flow move', `${t.id} -> ${now}, the pane shows ${fid}`);
    else c.R.fail('flow move', 'the pane did not follow the moved tab', t.id, `old gone ${gone}, new id ${now}, pane shows ${fid}, header "${label}"`);
    await c.shot('flow move');
  });
  await step('flow move now', async () => {
    const s = await S();
    const t = movable(s), to = target(s, t);
    await fetch(`${origin}/dev/busy?tab=${t.id}`); await sleep(500);
    await options(t.id);
    await clickText(c, /^Move( to)?$/); await sleep(300);
    await clickText(c, new RegExp('^' + to.label));
    await page.waitForFunction(() => /busy/.test(document.querySelector('#sheet .sh-title')?.textContent || ''), null, { timeout: 5000 });
    await sleep(300);
    await clickText(c, /^Move now/, '#sheet-body');
    await page.waitForFunction(() => /Confirm/.test(document.querySelector('#sheet .sh-title')?.textContent || ''), null, { timeout: 5000 });
    await sleep(250);
    await clickText(c, /^Yes$/, '#sheet-body');
    const gone = await waitFor((id) => !window.__g.mods.state.S.tabs.some((x) => x.id === id), t.id, 5000);
    await sleep(600);
    const s2 = await S();
    const now = s2.moved[t.id.replace(/^t/, '')] != null ? 't' + s2.moved[t.id.replace(/^t/, '')] : null;
    const fid = await focusedId();
    if (gone && now && fid === now) c.R.ok('flow move now', `${t.id} -> ${now}, the pane shows ${fid}`);
    else c.R.fail('flow move now', 'the pane did not follow the tab after Move now', t.id, `old gone ${gone}, new ${now}, pane ${fid}`);
    await c.shot('flow move now');
  });

  await step('flow send a prompt', async () => {
    const id = (await S()).tabs.find((x) => x.state === 'ready')?.id;
    await show(id);
    await waitFor(() => window.__g.mods.term.panes.focused().ready === true, null, 5000);
    if (c.size.touch && !c.isDesk) await page.locator('#keybar button', { hasText: /^Prompt$/ }).first().click({ timeout: 4000 });
    else await open(`(await mod('keybar')).promptSheet('prompt')`);
    await page.waitForSelector('#sheet-body textarea', { timeout: 4000 });
    await page.fill('#sheet-body textarea', 'gate prompt 42');
    await page.locator('#sheet-body button.btn.pri', { hasText: 'Send' }).first().click({ timeout: 4000 });
    const got = await waitFor(() => { const { panes } = window.__g.mods.term; const b = panes.focused().term.buffer.active; for (let i = 0; i < b.length; i++) if ((b.getLine(i)?.translateToString(true) || '').includes('gate prompt 42')) return true; return false; }, null, 6000);
    if (got) c.R.ok('flow send a prompt'); else c.R.fail('flow send a prompt', 'the prompt never reached the tab', id);
    await c.shot('flow send a prompt');
  });

  await step('flow approve and deny', async () => {
    for (const [label, re, expectGone] of [['approve', /^Yes$/, true], ['deny', /^No$/, true]]) {
      await c.closeAll();
      await fetch(origin + '/dev/approval'); await sleep(900);
      const n0 = (await S()).approvals.length;
      if (!n0) { c.R.fail('flow ' + label, 'no approval appeared', ''); continue; }
      await page.click('#b-appr', { timeout: 4000 }); await sleep(400);
      const card = page.locator('#sheet-body .lrow.appr').first();
      await card.locator('button', { hasText: re }).first().click({ timeout: 4000 });
      let n1 = n0;
      for (let i = 0; i < 25 && n1 >= n0; i++) { await sleep(200); n1 = (await S()).approvals.length; }
      if (n1 < n0) c.R.ok('flow ' + label, `${n0} -> ${n1} open`); else c.R.fail('flow ' + label, 'the approval is still waiting', '', `${n0} -> ${n1}`);
      await c.shot('flow ' + label);
    }
  });

  await step('flow layouts', async () => {
    if (!isDesk) { const vis = await page.evaluate(() => { const b = document.querySelector('#b-split'); return !!b && getComputedStyle(b).display !== 'none'; }); if (vis) c.R.fail('flow layouts', 'the Layout button shows in the phone layout', '#b-split'); else c.R.ok('flow layouts', 'phone layout: no Layout button'); return; }
    await open(`const { panes } = await mod('term'); panes.setN(1)`); await sleep(400);
    const seen = [];
    for (let i = 0; i < 3; i++) {
      await page.click('#b-split', { timeout: 4000 }); await sleep(700);
      const g = await page.evaluate(async () => {
        const { panes } = await window.__g.mod('term');
        return { n: panes.n, want: panes.want, dir: panes.dir, boxes: panes.visible().map((p) => { const r = p.el.getBoundingClientRect(); return [Math.round(r.width), Math.round(r.height)]; }) };
      });
      seen.push(`${g.n}${g.n !== g.want ? '(asked ' + g.want + ')' : ''}`);
      if (g.boxes.some(([w, h]) => w < 200 || h < 120)) c.R.fail('flow layouts', 'a pane is too small', `n=${g.n}`, JSON.stringify(g.boxes));
      await c.audit('layout ' + seen.length + ' (' + g.n + ' panes)', ['#panes .pane-head', '#top', '#sbar']);
    }
    if (seen[2].startsWith('1')) c.R.ok('flow layouts', seen.join(' > ')); else c.R.fail('flow layouts', 'cycling does not come back to one pane', '#b-split', seen.join(' > '));
  });

  await step('flow rename', async () => {
    const t = (await S()).tabs.find((x) => x.state === 'ready' && !x.pinned);
    await options(t.id);
    await clickText(c, /^Rename$/); await sleep(300);
    await page.locator('#sheet-body input[type=text], #tp-body input[type=text], #pop input[type=text]').filter({ visible: true }).first().fill('gate-renamed');
    await clickText(c, /^Rename$/, '#sheet-body, #tp-body, #pop', 'button.pri, button.btn');
    const ok = await waitFor((id) => window.__g.mods.state.S.tabs.find((x) => x.id === id)?.name === 'gate-renamed', t.id, 4000);
    await sleep(300);
    const head = await page.evaluate(() => document.querySelector('#t-name')?.textContent || '');
    if (ok && /gate-renamed/.test(head)) c.R.ok('flow rename'); else c.R.fail('flow rename', 'the new name did not show', t.id, `state ${ok}, header "${head}"`);
    await c.shot('flow rename');
    await open(`await (await mod('conn')).tool('rename_tab', { tab: a[0], name: a[1] }, 20000, { user_says: 'rename it' })`, t.id, t.name);
  });

  await step('flow archive and restore', async () => {
    const t = (await S()).tabs.filter((x) => x.state === 'ready' && !x.pinned).at(-1);
    await options(t.id);
    await clickText(c, /^Archive$/);
    const gone = await waitFor((id) => !window.__g.mods.state.S.tabs.some((x) => x.id === id), t.id, 5000);
    if (!gone) { c.R.fail('flow archive', 'the tab is still open after Archive', t.id); return; }
    c.R.ok('flow archive', t.id + ' gone');
    await c.closeAll();
    if (!isDesk) { await page.click('#b-menu'); await sleep(350); }
    await page.fill('#q', t.name);
    const row = page.locator('#list .row', { has: page.locator('.tagl', { hasText: 'Archived' }) }).first();
    await row.waitFor({ timeout: 6000 }).catch(() => {});
    if (!(await row.count())) { c.R.fail('flow restore', 'the archived tab is not listed under the filter', t.name); await c.shot('flow restore'); await page.fill('#q', ''); return; }
    await row.locator('.tx').click({ timeout: 4000 });
    const back = await waitFor((name) => window.__g.mods.state.S.tabs.some((x) => x.name === name), t.name, 6000);
    if (back) c.R.ok('flow restore', t.name + ' is back'); else c.R.fail('flow restore', 'the tab did not come back', t.name);
    await page.fill('#q', '').catch(() => {});
    await c.shot('flow restore');
  });
}

const t0 = Date.now();
await run().catch((e) => { problems.push({ label: 'gate', screen: 'driver', kind: 'the gate itself broke', el: '', detail: e.stack || e.message }); });
const secs = ((Date.now() - t0) / 1000).toFixed(0);
const lines = [`# Web gate ${new Date().toISOString()}`, '', `${passed.length} screens and flows passed, ${problems.length} problems, ${secs} s, ${ENGINES.join(' + ')}.`, ''];
if (problems.length) {
  lines.push('## Problems', '');
  for (const p of problems) lines.push(`- [${p.label}] ${p.screen}: ${p.kind}${p.el ? ' | ' + p.el : ''}${p.detail ? ' | ' + p.detail : ''}`);
  lines.push('');
}
lines.push('## Passed', '', ...passed.map((x) => '- ' + x), '');
fs.writeFileSync(path.join(OUT, 'report.md'), lines.join('\n'));
fs.writeFileSync(path.join(OUT, 'results.json'), JSON.stringify({ problems, passed, secs: +secs }, null, 1));
console.log('\n' + lines.slice(0, 3).join('\n'));
if (problems.length) {
  console.log('\nGATE FAILED:');
  for (const p of problems) console.log(`  FAIL [${p.label}] ${p.screen}: ${p.kind}${p.el ? ' | ' + p.el : ''}${p.detail ? ' | ' + p.detail : ''}`);
}
console.log(`\nreport: ${OUT}/report.md (screenshots in ${OUT}/<size>/)`);
process.exit(problems.length ? 1 : 0);
