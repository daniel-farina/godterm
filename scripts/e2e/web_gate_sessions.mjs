export async function sessionsChecks(c, { sleep, origin }) {
  const { page, step, audit } = c;
  const name = 'sessions agents';
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);
  const need = (ok, kind, detail = '') => { if (ok) c.R.ok(name, kind); else c.R.fail(name, 'not so: ' + kind, '#sheet', typeof detail === 'string' ? detail : JSON.stringify(detail)); return ok; };
  const get = async (p) => (await fetch(origin + p)).text();
  const settle = async () => { await page.waitForFunction(() => { const i = document.querySelector('#sheet-body .mnote'); return !!document.querySelector('#sheet-body .srow, #sheet-body .empty') && !/Searching|Looking/.test(i?.textContent || ''); }, null, { timeout: 6000, polling: 100 }).catch(() => {}); await sleep(250); };
  const sheet = async (f) => {
    await open(`
      const { store } = await mod('util');
      store.set('sessf-open', true);
      (await mod('sessions')).sessionsSheet(Object.assign({ text: '', harness: 'all', source: '', state: 'all', since: '', project: '', sort: 'modified', reverse: false }, a[0]));`, f);
    await settle();
  };
  const look = () => page.evaluate(() => {
    const t = (e) => (e?.textContent || '').trim();
    const rows = [...document.querySelectorAll('#sheet-body .slist .srow')].map((r) => ({ title: t(r.querySelector('.st b')), tag: [...r.querySelectorAll('.st .tagl')].map(t), meta: [...r.querySelectorAll('.sm')].map(t).join(' | ') }));
    const frow = [...document.querySelectorAll('#sheet-body .frow')].find((r) => t(r.querySelector('.fl')) === 'Agent');
    const warn = document.querySelector('#sheet-body .lost-warn');
    return {
      chips: frow ? [...frow.querySelectorAll('.chip')].map((b) => ({ t: t(b), on: b.classList.contains('on') })) : [],
      rows, info: t(document.querySelector('#sheet-body > .mnote')),
      empty: [...document.querySelectorAll('#sheet-body .slist .empty')].map(t),
      hint: t(document.querySelector('#sheet-body .plain-hint .eh')), hintBtn: t(document.querySelector('#sheet-body .plain-hint .btn')),
      warn: warn && !warn.hidden ? { text: t(warn), btns: [...warn.querySelectorAll('.btn')].map(t) } : null,
      sw: document.documentElement.scrollWidth, iw: innerWidth,
    };
  });
  const chip = async (label) => { await page.evaluate((l) => { const f = [...document.querySelectorAll('#sheet-body .frow')].find((r) => r.querySelector('.fl')?.textContent.trim() === 'Agent'); [...f.querySelectorAll('.chip')].find((b) => b.textContent.trim() === l)?.click(); }, label); await sleep(200); await settle(); };

  await step(name, async () => {
    await sheet({});
    let s = await look();
    const labels = s.chips.map((x) => x.t);
    need(labels[0] === 'All agents' && s.chips[0].on, 'the first agent chip is All agents and it is on', labels);
    need(['Claude', 'Grok', 'Codex', 'Cursor', 'Antigravity'].every((l) => labels.includes(l)) && !labels.includes('Claude + Grok'), 'every agent with an account or sessions has a chip', labels);
    need(!labels.includes('OpenCode'), 'an agent with no account and no sessions has no chip', labels);
    need(s.sw <= s.iw, 'the sheet does not overflow sideways', s);

    await chip('Antigravity');
    s = await look();
    need(s.rows.length === 3 && s.rows.every((r) => r.tag.includes('antigravity')), 'the Antigravity chip lists Antigravity sessions only', s.rows);
    const own = s.rows.find((r) => /Gravity checks the acme api/.test(r.title));
    need(!!own && /~\/work\/acme-api/.test(own.meta) && /Gravity/.test(own.meta) && /ago/.test(own.meta), 'an account session shows title, folder, account and time', own);
    const plain = s.rows.find((r) => /Acme importer tidy up/.test(r.title));
    need(!!plain && /Antigravity \(plain terminal\)/.test(plain.meta) && /~\/work\/acme-import/.test(plain.meta), 'a plain terminal conversation names its source and folder', plain);
    await audit('sessions antigravity', ['#sheet']);
    await c.shot('sessions antigravity');

    await sheet({ source: '5', text: 'importer', since: '3 days' });
    s = await look();
    need(s.rows.length === 0 && /plain terminal/.test(s.hint) && /Gravity/.test(s.hint) && s.hintBtn === 'Show them', 'an Antigravity account with no match offers the plain terminal conversations', s);
    const said = s.empty.join(' ');
    need(/search "importer"/.test(said) && /time 3 days/.test(said) && /account Gravity/.test(said), 'the empty state names the filters that are on', said);
    need(s.sw <= s.iw, 'the empty state does not overflow sideways', s);
    await audit('sessions empty hint', ['#sheet']);
    await c.shot('sessions empty hint');
    await page.locator('#sheet-body .plain-hint .btn').click({ timeout: 4000 });
    await settle();
    s = await look();
    need(s.rows.length === 1 && /Acme importer tidy up/.test(s.rows[0].title), 'Show them lists the plain terminal conversations that match', s.rows);
    need(s.chips.find((x) => x.on)?.t === 'Antigravity', 'and the Antigravity chip is on', s.chips);

    const before = JSON.parse(await get('/dev/opened')).length;
    await page.locator('#sheet-body .slist .srow').first().click({ timeout: 4000 });
    await sleep(300);
    s = await look();
    need(!!s.warn && /copied into Gravity/.test(s.warn.text) && /resumes there/.test(s.warn.text) && /original stays/.test(s.warn.text), 'opening one asks first and says where it will resume', s.warn);
    need(!!s.warn && s.warn.btns.join() === 'Cancel,Copy and resume' && !/null|undefined/.test(s.warn.text), 'with Cancel and Copy and resume', s.warn);
    need(JSON.parse(await get('/dev/opened')).length === before, 'nothing is opened before the answer');
    const fit = await page.evaluate(() => { const e = document.querySelector('#sheet-body .lost-warn'); const r = e.getBoundingClientRect(); return { sw: document.documentElement.scrollWidth, iw: innerWidth, l: r.left, r: r.right, cut: e.scrollWidth > e.clientWidth + 1, btns: [...e.querySelectorAll('.btn')].every((b) => b.getBoundingClientRect().right <= innerWidth && b.getBoundingClientRect().height >= 32) }; });
    need(fit.sw <= fit.iw && fit.l >= 0 && fit.r <= fit.iw && !fit.cut && fit.btns, 'the question fits the screen', fit);
    await c.shot('sessions copy question');
    await page.locator('#sheet-body .lost-warn .btn.pri').click({ timeout: 4000 });
    await sleep(600);
    const sent = JSON.parse(await get('/dev/opened')).slice(before);
    need(sent.length === 1 && sent[0].copy === true && sent[0].account === 5 && sent[0].harness === 'antigravity', 'Copy and resume opens it on the Antigravity account with the copy confirmed', sent);
    await c.closeAll();

    await open(`
      const { openSheet } = await mod('sheet');
      const { lostSessions } = await mod('lost');
      openSheet({ kind: 'newtab', tall: true }, lostSessions());`);
    await settle();
    await page.evaluate(() => [...document.querySelectorAll('#sheet-body .lost .frow .chip')].find((b) => b.textContent.trim() === 'Antigravity')?.click());
    await sleep(200);
    await settle();
    const lost = await page.evaluate(() => [...document.querySelectorAll('#sheet-body .lost-list .srow')].map((r) => r.textContent));
    need(lost.some((t) => /Acme importer tidy up/.test(t) && /Antigravity \(plain terminal\)/.test(t)), 'Existing sessions lists the plain terminal conversations', lost);
    const q = page.locator('#sheet-body .lost input[type=search]');
    await q.fill('acme-import');
    await page.waitForFunction(() => /^1 not open/.test(document.querySelector('#sheet-body .lost-info')?.textContent || ''), null, { timeout: 6000, polling: 100 }).catch(() => {});
    await sleep(250);
    const found = await page.evaluate(() => [...document.querySelectorAll('#sheet-body .lost-list .srow b')].map((r) => r.textContent));
    need(found.length === 1 && /Acme importer tidy up/.test(found[0]), 'and finds one by its folder', found);
    const b2 = JSON.parse(await get('/dev/opened')).length;
    await page.locator('#sheet-body .lost-list .srow').first().click({ timeout: 4000 });
    await sleep(300);
    const w = await page.evaluate(() => { const e = document.querySelector('#sheet-body .lost-warn'); return e && !e.hidden ? e.textContent : ''; });
    need(/plain terminal/.test(w) && /copied into Gravity/.test(w) && JSON.parse(await get('/dev/opened')).length === b2, 'Existing sessions asks before copying too', w);
    await page.locator('#sheet-body .lost-warn .btn', { hasText: 'Cancel' }).click({ timeout: 4000 });
    await sleep(200);
    need(JSON.parse(await get('/dev/opened')).length === b2, 'Cancel opens nothing');
    await sheet({});
    await c.closeAll();
  });
}
