export async function asksChecks(c, { sleep, origin }) {
  const { page, step, audit } = c;
  const name = 'asks';
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);
  const need = (ok, kind, detail = '') => { if (ok) c.R.ok(name, kind); else c.R.fail(name, 'not so: ' + kind, '.ask-bar', typeof detail === 'string' ? detail : JSON.stringify(detail)); return ok; };
  const until = async (fn, arg, ms = 6000) => { try { await page.waitForFunction(fn, arg, { timeout: ms, polling: 100 }); return true; } catch { return false; } };
  const get = async (p) => (await fetch(origin + p)).text();
  const show = async (id) => { await open(`(await mod('term')).panes.show(a[0])`, id); await sleep(350); };
  const noToasts = () => page.evaluate(() => document.querySelector('#toasts')?.replaceChildren());
  const look = (id) => open(`
    const { panes } = await mod('term');
    const { S } = await mod('state');
    const p = panes.focused();
    const bar = p.el.querySelector('.ask-bar');
    const ask = bar && !bar.hidden ? bar.querySelector('.ask[data-id="' + a[0] + '"]') : null;
    const r = (e) => { const b = e.getBoundingClientRect(); return { x: b.left, y: b.top, w: b.width, h: b.height, r: b.right, b: b.bottom }; };
    const body = r(p.body), badge = document.querySelector('#appr-n'), btn = document.querySelector('#b-appr');
    return {
      pane: p.id, shown: !!ask, bars: [...document.querySelectorAll('#panes .pane:not([hidden]) .ask-bar:not([hidden]) .ask')].length,
      head: ask?.querySelector('.ask-t')?.textContent || '', q: ask?.querySelector('.ask-q')?.textContent || '', note: ask?.querySelector('.ask-n')?.textContent || '',
      btns: ask ? [...ask.querySelectorAll('.ask-b .btn')].map((b) => ({ t: b.textContent, ...r(b) })) : [],
      bar: bar && !bar.hidden ? { ...r(bar), sw: bar.scrollWidth, cw: bar.clientWidth } : null, body,
      sw: document.documentElement.scrollWidth, iw: innerWidth,
      badge: badge.hidden ? 0 : +badge.textContent, badgeSeen: !badge.hidden && badge.getClientRects().length > 0, aria: btn.getAttribute('aria-label'),
      n: (S.approvals || []).length, sub: document.querySelector('#t-sub')?.textContent || '',
    }`, id);
  const rows = async (chip) => {
    await page.evaluate(() => { const s = document.querySelector('#side'); const a = document.querySelector('#app'); a.classList.remove('noside'); if (!s.getClientRects().length || getComputedStyle(s).visibility === 'hidden' || s.getBoundingClientRect().right <= 0) s.classList.add('open'); });
    await page.evaluate((k) => document.querySelector(`#chips .chip[data-k="${k}"]`).click(), chip);
    await sleep(250);
    const out = await page.evaluate(() => [...document.querySelectorAll('#list .row[data-id]')].map((r) => ({ id: r.dataset.id, s: r.querySelector('.s')?.textContent || '', need: !!r.querySelector('.s.need'), cut: (() => { const s = r.querySelector('.s'); return !!s && s.scrollWidth > s.clientWidth + 1; })() })));
    return out;
  };
  const side = async () => { const sw = await page.evaluate(() => ({ sw: document.documentElement.scrollWidth, iw: innerWidth })); await page.evaluate(() => { document.querySelector('#chips .chip[data-k="all"]').click(); document.querySelector('#side').classList.remove('open'); }); await sleep(150); return sw; };

  await step(name, async () => {
    await get('/dev/jobask-expire');
    await sleep(400);
    const tabs = (await c.S()).tabs;
    const A = tabs.find((t) => t.state === 'ready') || tabs[0];
    const B = tabs.find((t) => t.id !== A.id && t.state !== 'approval') || tabs.find((t) => t.id !== A.id);
    const P = tabs.find((t) => t.state === 'approval');
    await show(B.id);
    const before = await look('none');
    const id = await get('/dev/jobask?tab=' + A.uid);
    await until((i) => !!document.querySelector('#appr-n') && !document.querySelector('#appr-n').hidden && window.__g.mods.state.S.approvals?.some((x) => x.id === i), id);
    await sleep(500);
    await noToasts();

    let s = await look(id);
    need(!s.shown && s.bars === 0, 'a pane showing another tab has no banner', s);
    need(s.badgeSeen && s.badge === s.n && s.badge === before.badge + 1, 'the top bar counts the waiting approvals', { badge: s.badge, n: s.n, before: before.badge });
    need(/wait/.test(s.aria || ''), 'the top bar button says how many wait', s.aria);

    await page.click('#b-appr', { timeout: 4000 });
    await sleep(400);
    const card = page.locator(`#sheet-body .lrow.appr[data-id="${id}"]`);
    const kind = await card.locator('.appr-k').textContent({ timeout: 4000 }).catch(() => '');
    need(/GodTerm asks: create job/.test(kind), 'the approvals list says what GodTerm asks', kind);
    await audit('asks approvals list', ['#sheet']);
    await card.locator('button', { hasText: 'Show the tab' }).click({ timeout: 4000 });
    await sleep(500);
    s = await look(id);
    need(s.pane === A.id, 'Show the tab jumps to the tab that asks', { pane: s.pane, want: A.id });
    need(s.shown, 'the banner shows on the pane of the tab that asks', s);
    need(/nightly dependency audit/.test(s.q) && /Run the dependency audit/.test(s.q), 'the banner has the full question', s.q);
    need(/needs your OK: create job/.test(s.head) && /agent in this tab/.test(s.head), 'the banner says what is asked and who asks', s.head);
    need(/ago/.test(s.head) && /expires in/.test(s.head), 'the banner says its age and the time left', s.head);
    need(/terminal/.test(s.note), 'the banner says it is not a terminal prompt', s.note);
    need(s.btns.length === 2 && s.btns.every((b) => b.h >= 40 && b.w >= 44), 'Yes and No are at least 40px tall', s.btns);
    need(s.sw <= s.iw && s.bar && s.bar.sw <= s.bar.cw + 1 && s.bar.r <= s.iw + 0.5 && s.btns.every((b) => b.r <= s.iw + 0.5 && b.x >= 0), 'the banner does not overflow sideways', { sw: s.sw, iw: s.iw, bar: s.bar });
    need(s.bar && s.bar.b <= s.body.y + 0.5 && s.body.h >= 80, 'the banner sits above the terminal and leaves it room', { bar: s.bar, body: s.body });
    need(/needs your OK/.test(s.sub), 'the title says the tab needs your OK', s.sub);
    await audit('asks banner', ['#panes']);

    let list = await rows('all');
    let row = list.find((r) => r.id === A.id);
    need(!!row && /needs your OK: create job/.test(row.s) && row.need && !row.cut, 'the list row says what it waits for in words, not cut short', row);
    if (P) { const pr = list.find((r) => r.id === P.id); need(!!pr && /prompt in the terminal/.test(pr.s) && !/needs your OK/.test(pr.s), 'a harness prompt is worded as a prompt in the terminal', pr); }
    list = await rows('wait');
    need(list.some((r) => r.id === A.id), 'Needs you includes the tab', list.map((r) => r.id));
    await c.shot('asks list');
    const w = await side();
    need(w.sw <= w.iw, 'the tab list does not overflow sideways', w);

    await show(A.id);
    await noToasts();
    await c.shot('asks banner on the tab');
    await page.locator('#panes .pane.focus .ask [data-c=yes], #panes .pane:not([hidden]) .ask [data-c=yes]').first().click({ timeout: 4000 });
    await until((i) => !window.__g.mods.state.S.approvals?.some((x) => x.id === i), id);
    await sleep(400);
    const answers = JSON.parse(await get('/dev/answers'));
    need(answers.some((x) => x.id === id && x.choice === 'yes'), 'Yes sends the answer', answers);
    s = await look(id);
    need(!s.shown && !s.bar, 'the banner is gone once answered', s);
    need(s.badge === before.badge, 'the top bar count drops once answered', { badge: s.badge, before: before.badge });
    list = await rows('all');
    row = list.find((r) => r.id === A.id);
    need(!!row && !/needs your OK/.test(row.s), 'the list row is back to normal once answered', row);
    await side();

    const id2 = await get('/dev/jobask?tab=' + A.uid);
    await until((i) => !!document.querySelector(`#panes .ask[data-id="${i}"]`), id2);
    await noToasts();
    s = await look(id2);
    need(s.shown, 'a second request shows its banner', s);
    await get('/dev/jobask-expire');
    const gone = await until((i) => !document.querySelector(`#panes .ask[data-id="${i}"]`), id2);
    s = await look(id2);
    need(gone && !s.bar && s.badge === before.badge, 'an expired request disappears everywhere', s);

    const id3 = await get('/dev/jobask?tab=none');
    await until((i) => window.__g.mods.state.S.approvals?.some((x) => x.id === i), id3);
    await sleep(400);
    await noToasts();
    s = await look(id3);
    need(s.badge === before.badge + 1 && s.bars === 0, 'a request with no open tab still counts in the top bar', s);
    await page.click('#b-appr', { timeout: 4000 });
    await sleep(400);
    const lone = page.locator(`#sheet-body .lrow.appr[data-id="${id3}"]`);
    need(/closed/.test(await lone.locator('.appr-k').textContent({ timeout: 4000 }).catch(() => '')), 'it is listed with its tab named as closed');
    await lone.locator('button', { hasText: /^No$/ }).click({ timeout: 4000 });
    await until((i) => !window.__g.mods.state.S.approvals?.some((x) => x.id === i), id3);
    const a2 = JSON.parse(await get('/dev/answers'));
    need(a2.some((x) => x.id === id3 && x.choice === 'no'), 'No from the list sends the answer', a2);
    await c.closeAll();
  });
}
