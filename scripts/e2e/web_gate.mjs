// The web app's merge gate (scripts/e2e/gate.sh runs it): one browser, the
// dev mock, a couple of minutes. It fails loudly on what slipped past
// before: dialogs whose buttons were invisible (a CSS class shared by two
// features), a confirm button clipped at 390 px, a moved tab left on screen
// under its old id, a Talk button missing while the Assistant is open.
//
//   node scripts/e2e/web_gate.mjs --url http://127.0.0.1:7993/ [--out DIR]
//        [--engine chromium|webkit|both] [--sizes 320,390,820,1440,2560]
//        [--only text] [--keep-going]
//
// (a) every sheet, dialog, popover and menu is opened at 320x640 and
//     390x844 (touch), 820x1180 (touch), 1440x900 and 2560x1440, and every
//     button and input in it must be visible, inside the viewport and inside
//     what clips it, not covered (elementFromPoint at its centre), not
//     text-clipped, and clickable;
// (b) feature combinations (push to talk with the Assistant open, a muted mic
//     with the chat, panel + chat in a squeezed window, a message over a
//     sheet);
// (c) flows through the UI with real clicks (move a tab and the pane follows
//     its new id, move now, a prompt, approve and deny, the pane layouts,
//     rename, archive and restore);
// (d) a screenshot per screen in DIR/<size>/.
import { createRequire } from 'module';
import fs from 'fs';
import path from 'path';

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
// (GATE_SLOW=1 for slow machines: the settle waits are scaled.)
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

// ---- in the page ------------------------------------------------------
function pageSide() {
  window.__g = {
    mods: {},
    async mod(n) {
      const l = document.querySelector('link[rel=modulepreload][href*="js/main.js"]');
      const u = l.href.replace(/\?.*$/, '').replace(/js\/[a-z]+\.js$/, `js/${n}.js`);
      return (this.mods[n] ||= await import(u));
    },
    // What the user would call a broken control, for every control in `roots`.
    audit(roots) {
      const out = [];
      const vw = innerWidth, vh = innerHeight;
      const SEL = 'button, input, select, textarea, a[href], [role=button], [role=menuitem], [role=menuitemradio], [role=menuitemcheckbox], summary';
      const desc = (e) => {
        const t = (e.getAttribute('aria-label') || e.textContent || e.placeholder || e.title || '').trim().replace(/\s+/g, ' ').slice(0, 40);
        return `${e.tagName.toLowerCase()}${e.id ? '#' + e.id : ''}${e.className && typeof e.className === 'string' ? '.' + e.className.trim().split(/\s+/).slice(0, 2).join('.') : ''} "${t}"`;
      };
      const shown = (e) => {
        // (A message's actions show on hover or under the last reply only.)
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
      // A modal sheet or menu owns the screen: what is behind its scrim is not
      // reachable by design.
      const modal = document.querySelector('#sheet.on') ? '#sheet' : (document.querySelector('#pop:not([hidden])') ? '#pop' : '');
      for (const sel of roots) {
        const root = typeof sel === 'string' ? document.querySelector(sel) : sel;
        if (!root || !shown(root)) continue;
        if (modal && typeof sel === 'string' && sel !== modal && sel !== '#toasts') continue;
        // The container itself: on screen, no sideways scroll.
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
          // Clipped by an ancestor that hides overflow (the root's own edge counts).
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

// ---- helpers ---------------------------------------------------------
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
      if (engine === 'chromium' && opts.userAgent) { /* the UA only, no webkit-only fields */ }
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
      // Audit the roots, report once per distinct problem.
      const audit = async (screen, roots) => {
        if (!/message/.test(screen)) await page.evaluate(() => document.querySelector('#toasts')?.replaceChildren());
        // Let a sliding sheet, drawer or menu come to rest first.
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

      if (errors.length) for (const e of [...new Set(errors)].slice(0, 5)) R.fail('page', 'page error', '', e.slice(0, 300));
      else R.ok('page', 'no page errors');
      console.log(`[${label}] done in ${((Date.now() - t0) / 1000).toFixed(0)} s`);
      await browser.close();
    }
  }
}

// click a control by its text in the open overlay (a real click: it must be clickable)
async function clickText(c, re, scopes = '#sheet-body, #tp-body, #pop', tag = 'button, [role=menuitem], .li', timeout = 4000) {
  const loc = c.page.locator(scopes.split(',').map((s) => `${s.trim()} :is(${tag})`).join(', '), { hasText: re }).filter({ visible: true }).first();
  await loc.click({ timeout });
}

// A tab that can be moved (idle, not pinned) and a claude account to move it to.
const movable = (s) => s.tabs.find((x) => x.state === 'ready' && !x.pinned && (s.accounts.find((a) => a.account === x.account)?.harness || 'claude') === 'claude');
const target = (s, t) => t && s.accounts.find((a) => a.account !== t.account && (a.harness || 'claude') === 'claude' && a.available !== false && a.logged_in !== false);

// ---- (a) every screen -------------------------------------------------
async function screens(c) {
  const { page, isDesk, step, audit } = c;
  const OV = ['#sheet', '#pop', '#tabpanel'];
  const tabId = async () => (await c.S()).tabs.find((t) => t.state === 'ready' && !t.pinned)?.id || (await c.S()).tabs[0].id;
  const show = async (id) => { await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), id); await sleep(200); };
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);

  // The page as it stands (the header, the key bar or status bar, the list).
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
    // Its forms: the first "Add" / "Edit" button, if the list has one.
    const b = page.locator('#sheet-body button', { hasText: /^(Add|Edit|New)/ }).filter({ visible: true }).first();
    if (await b.count()) { await b.click({ timeout: 3000 }).catch(() => {}); await sleep(400); await audit('accounts form', OV); }
  });
  await step('git', async () => { const id = await tabId(); await open(`const { S } = await mod('state'); (await mod('git')).gitSheet(S.tabs.find((t) => t.id === a[0]))`, id); await sleep(900); await audit('git', OV); });
  await step('new tab', async () => { await open(`(await mod('manage')).newTabSheet()`); await sleep(400); await audit('new tab', OV); });
  await step('approvals', async () => { await fetch(origin + '/dev/approval'); await sleep(900); await open(`(await mod('chat')).approvalsSheet()`); await sleep(300); await audit('approvals', OV); });
  await step('help', async () => { await open(`(await mod('keys')).helpSheet()`); await sleep(250); await audit('help', OV); });
  await step('prompt box', async () => { await show(await tabId()); await open(`(await mod('keybar')).promptSheet('prompt')`); await sleep(350); await audit('prompt box', OV); });
  await step('paste box', async () => { await show(await tabId()); await open(`(await mod('keybar')).promptSheet('paste')`); await sleep(350); await audit('paste box', OV); });
  await step('update', async () => { await open(`(await mod('manage')).updateSheet({ state: 'downloaded', notes: 'Faster startup and a new sessions view.', update_with: 'Update and restart', page: 'https://example.com/r' }, 'Update available')`); await sleep(300); await audit('update', OV); });
  await step('install', async () => { await open(`(await mod('install')).install()`); await sleep(400); await audit('install', OV); });
  await step('voice sheet', async () => { await open(`(await mod('voice')).openVoiceSheet()`); await sleep(500); await audit('voice sheet', OV); });
  await step('mic menu', async () => { await page.click('#b-ptt'); await sleep(450); await audit('mic menu', OV); });
  await step('assistant', async () => { await open(`(await mod('chat')).setChat(true)`); await sleep(500); await audit('assistant', ['#chat', '#top', '#keyrow']); });

  // A tab's options: the sheet (phone) or the docked panel (desktop), and each section.
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
    // The right-click popover on a desktop.
    if (isDesk) { await c.closeAll(); await open(`(await mod('manage')).tabActions(a[0])`, id); await sleep(400); await audit('tab options popover', OV); }
  });

  // Confirm sheets: closing a pinned tab asks first.
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

  // The move picker and the move-now sheet (a busy tab).
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
    await clickText(c, new RegExp('^' + to.label)); // queued: "Move now" / "Wait for them"
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

// ---- (b) combinations -----------------------------------------------
async function combos(c) {
  const { page, isDesk, step, audit, size } = c;
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);
  const geometry = (sel) => page.evaluate((sel) => { const e = document.querySelector(sel); if (!e) return null; const r = e.getBoundingClientRect(), cs = getComputedStyle(e); return { hidden: e.hidden || cs.display === 'none' || cs.visibility === 'hidden', x: r.left, y: r.top, w: r.width, h: r.height, vw: innerWidth, vh: innerHeight }; }, sel);
  const talkOk = (g) => g && !g.hidden && g.w >= 40 && g.h >= 40 && g.x >= 0 && g.y >= 0 && g.x + g.w <= g.vw + 1 && g.y + g.h <= g.vh + 1;

  await step('combo push to talk + assistant', async () => {
    await open(`(await mod('voice')).setMode('push')`); await sleep(700);
    const before = await geometry('#talk');
    if (!talkOk(before)) c.R.fail('combo push to talk', 'Talk button missing or off screen without the assistant', '#talk', JSON.stringify(before));
    await open(`(await mod('chat')).setChat(true)`); await sleep(700);
    const g = await geometry('#talk');
    if (!talkOk(g)) c.R.fail('combo push to talk + assistant', 'Talk button missing or off screen while the assistant is open', '#talk', JSON.stringify(g));
    await audit('combo push to talk + assistant', ['#chat', '#talk', '#voice-bar', '#top', '#keyrow']);
    // mic muted with the chat open
    await open(`(await mod('voice')).setMicMuted(true)`); await sleep(500);
    await audit('combo mic muted + assistant', ['#chat', '#talk', '#voice-bar', '#mic-strip', '#top']);
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

// ---- (c) flows -------------------------------------------------------
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

  // Move: the pane follows the tab to its new id.
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

  // A prompt through the Prompt box reaches the tab.
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

  // Approvals: approve one, deny another.
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

  // The pane layouts (a desktop): 1 -> 2 -> 4 -> 1, each with its panes on screen.
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

  // Rename and back.
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

  // Archive, then restore it from the tab list's filter.
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

// ---- run + report ------------------------------------------------------
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
