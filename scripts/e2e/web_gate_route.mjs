export async function routeChecks(c, { sleep }) {
  const { page, step, audit, isDesk } = c;
  const name = 'route';
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);
  const need = (ok, kind, detail = '') => { if (!ok) c.R.fail(name, kind, '#b-route', typeof detail === 'string' ? detail : JSON.stringify(detail)); return ok; };
  const until = async (fn, arg, ms = 5000) => { try { await page.waitForFunction(fn, arg, { timeout: ms, polling: 100 }); return true; } catch { return false; } };
  const call = (args) => open(`const { tool } = await mod('conn'); return tool('tab_route', a[0])`, args);
  const st = (id) => open(`const { S } = await mod('state'); const t = S.tabs.find((x) => x.id === a[0]); const b = document.querySelector('#b-route'); const p = [...document.querySelectorAll('#panes .pane:not([hidden])')].map((e) => e.querySelector('.rt-b')).find((e) => e && !e.hidden);
    return { route: t?.route || null, chip: b?.dataset.route, label: b?.querySelector('.rt-l')?.textContent, chipShown: !!b && b.getClientRects().length > 0, aria: b?.getAttribute('aria-label'), badge: p ? { text: p.textContent, s: p.dataset.s } : null, sub: document.querySelector('#t-sub')?.textContent, more: document.querySelector('#b-tabmenu')?.dataset.route,
      block: !!document.querySelector('#panes .pane:not([hidden]) .rt-block'), sw: document.documentElement.scrollWidth, iw: innerWidth }`, id);
  const wide = async (what) => { const s = await open('return { sw: document.documentElement.scrollWidth, iw: innerWidth, b: document.querySelector("#sheet-body")?.scrollWidth, bw: document.querySelector("#sheet-body")?.clientWidth }'); need(s.sw <= s.iw && (!s.b || s.b <= s.bw + 1), what + ' overflows sideways', s); };
  const menu = async (id) => {
    if (isDesk) await page.locator('#b-route').click({ timeout: 4000 });
    else await open(`(await mod('route')).routeMenu(a[0])`, id);
    await until(() => document.querySelectorAll('.rt-menu[data-kind=route] .li[data-route]').length >= 5, null, 6000);
    await sleep(250);
  };
  const pick = async (id, route) => {
    await menu(id);
    await page.locator(`.rt-menu .li[data-route="${route}"]`).click({ timeout: 4000 });
    await sleep(350);
  };

  await step(name, async () => {
    await open(`await mod('route')`);
    await call({ op: '_reset' });
    const tabs = await open(`const { S } = await mod('state'); return S.tabs.map((t) => ({ id: t.id, name: t.name, state: t.state }))`);
    const A = tabs.find((t) => t.state === 'ready');
    const W = tabs.find((t) => t.state === 'working');
    if (!need(!!A, 'the mock has no ready tab')) return;
    await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), A.id);
    await sleep(400);

    let s = await st(A.id);
    if (isDesk) {
      need(s.chipShown && s.chip === 'off' && s.label === 'Direct', 'the chip does not say Direct for an unrouted tab', s);
      need(/Direct connection/.test(s.aria || ''), 'the chip has no spoken label', s);
      need(!s.badge, 'an unrouted pane shows a route badge', s);
      await audit('route chip off', ['#top']);
    } else {
      need(!s.chipShown && s.more === 'off', 'the chip shows on a phone, where the top row has no room', s);
    }
    need(s.sw <= s.iw, 'the page overflows sideways', s);

    if (!isDesk) {
      await page.locator('#b-tabmenu').click({ timeout: 4000 });
      await sleep(400);
      const item = page.locator('#sheet .li', { hasText: 'Route' }).first();
      if (need(await item.count() > 0, 'the tab options have no Route item')) {
        await item.scrollIntoViewIfNeeded();
        await item.click({ timeout: 4000 });
        need(await until(() => document.querySelectorAll('.rt-menu[data-kind=route] .li[data-route]').length >= 5, null, 6000), 'Route in the tab options does not open the route list');
      }
      await c.closeAll();
    }

    await menu(A.id);
    const m = await open(`const rows = [...document.querySelectorAll('.rt-menu .li[data-route]')].map((e) => ({ r: e.dataset.route, on: e.classList.contains('on'), st: e.querySelector('.rt-st')?.textContent || '' }));
      return { rows, manage: !!document.querySelector('.rt-menu .li .lc'), limit: document.querySelector('.rt-menu .rt-limit')?.textContent || '' }`);
    need(m.rows[0]?.r === '' && m.rows[0].on, 'Direct is not the marked choice', m);
    const want = { office: 'ready', exit: 'ready', travel: 'login needed', lab: 'unreachable' };
    for (const [r, w] of Object.entries(want)) need(m.rows.find((x) => x.r === r)?.st === w, `the ${r} route does not say ${w}`, m.rows);
    need(m.manage, 'the menu has no Manage routes row', m);
    need(/not a kill switch/.test(m.limit) && !/all traffic/i.test(m.limit), 'the menu does not state the limits plainly', m.limit);
    await audit('route menu', [isDesk ? '#pop' : '#sheet']);
    await wide('the route menu');
    await c.closeAll();

    await pick(A.id, 'office');
    need(await until((id) => window.__g.mods.state.S.tabs.find((t) => t.id === id)?.route?.state === 'on', A.id), 'choosing a route did not route the tab', await st(A.id));
    await sleep(300);
    s = await st(A.id);
    if (isDesk) {
      need(s.chip === 'on' && s.label === 'via office', 'the chip is not highlighted with the route name', s);
      need(s.badge?.text === 'via office' && s.badge.s === 'on', 'the pane header has no route badge', s);
      await audit('route chip on', ['#top']);
    } else {
      need(/^via office/.test(s.sub || '') && s.more === 'on', 'the top bar does not mark a routed tab on a phone', s);
      await audit('route top bar on', ['#top .tr2']);
      await page.locator('#b-menu').click({ timeout: 4000 });
      await sleep(400);
    }
    const rowBadge = await open(`const e = document.querySelector('#list .row[data-id="' + a[0] + '"] .rtb'); return e ? { text: e.textContent, s: e.dataset.s, w: e.getBoundingClientRect().width } : null`, A.id);
    need(rowBadge?.text === 'via office' && rowBadge.s === 'on' && rowBadge.w > 0, 'the tab list row has no route badge', rowBadge);
    await audit('route badges', ['#side']);
    await wide('the tab list with a routed tab');
    await c.closeAll();

    await pick(A.id, 'lab');
    need(await until((id) => window.__g.mods.state.S.tabs.find((t) => t.id === id)?.route?.state === 'blocked', A.id), 'an unreachable route did not block the tab', await st(A.id));
    need(await until(() => !!document.querySelector('#panes .pane:not([hidden]) .rt-block')), 'the blocked tab shows no Retry card', await st(A.id));
    s = await st(A.id);
    if (isDesk) need(s.chip === 'warn' && s.badge?.s === 'warn', 'the chip and badge do not warn while blocked', s);
    else need(s.more === 'warn', 'the top bar does not warn on a phone while blocked', s);
    const card = await open(`const e = document.querySelector('#panes .pane:not([hidden]) .rt-block'); return { text: e.textContent, ops: [...e.querySelectorAll('button')].map((b) => b.dataset.op) }`);
    need(/not launched without its route/.test(card.text) && card.ops.join() === 'retry,route,launch_direct', 'the blocked card lacks Retry or Launch without the route', card);
    await audit('route blocked', ['#panes .pane:not([hidden]) .pane-msg']);
    await wide('the blocked card');
    await page.locator('#panes .pane:not([hidden]) .rt-block [data-op=retry]').click({ timeout: 4000 });
    await sleep(400);
    need((await st(A.id)).route?.state === 'blocked', 'Retry launched the tab although the proxy is down', await st(A.id));
    await page.locator('#panes .pane:not([hidden]) .rt-block [data-op=launch_direct]').click({ timeout: 4000 });
    need(await until((id) => window.__g.mods.state.S.tabs.find((t) => t.id === id)?.route?.state === 'bypassed', A.id), 'Launch without the route did not start the tab', await st(A.id));
    await sleep(300);
    s = await st(A.id);
    need(!s.block && (!isDesk || s.chip === 'warn'), 'a tab running without its route is not marked', s);
    await c.closeAll();

    if (W) {
      await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), W.id);
      await sleep(300);
      await pick(W.id, 'exit');
      if (need(await until(() => !!document.querySelector('.rt-menu[data-kind=route-confirm] [data-go]')), 'a working tab is rerouted without asking')) {
        need(!(await st(W.id)).route, 'the route changed before the user confirmed', await st(W.id));
        await audit('route confirm', [isDesk ? '#pop' : '#sheet']);
        await page.locator('.rt-menu [data-go]').click({ timeout: 4000 });
        need(await until((id) => window.__g.mods.state.S.tabs.find((t) => t.id === id)?.route?.name === 'exit', W.id), 'confirming did not reroute the working tab', await st(W.id));
      }
      await call({ op: 'set', tab: W.id, route: '', force: true });
      await c.closeAll();
      await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), A.id);
    }

    await open(`(await mod('route')).routesSheet()`);
    need(await until(() => document.querySelectorAll('#sheet .rt-card').length >= 4), 'Manage routes lists no routes');
    await sleep(300);
    let v = await open(`return { cards: [...document.querySelectorAll('#sheet .rt-card')].map((e) => ({ r: e.dataset.route, st: e.querySelector('.rt-st')?.textContent, u: e.querySelector('.rt-u')?.textContent, ops: [...e.querySelectorAll('button')].map((b) => b.dataset.op).join() })),
      login: document.querySelector('#sheet .rt-card[data-route=travel] .rt-login a')?.href || '', limit: document.querySelector('#sheet .rt-limit')?.textContent || '', text: document.querySelector('#sheet').textContent }`);
    need(v.cards.find((x) => x.r === 'office')?.u === 'http://***@proxy.example:3128', 'a saved password is not masked', v.cards);
    need(v.login.startsWith('https://login.example.test/'), 'the login link of a route that needs login is not shown', v.login);
    need(/not a kill switch/.test(v.limit), 'the sheet does not state the limits', v.limit);
    need(v.cards.every((x) => /test/.test(x.ops) && /edit/.test(x.ops) && /delete/.test(x.ops)), 'a route lacks Test, Edit or Delete', v.cards);
    await audit('route manage', ['#sheet']);
    await wide('Manage routes');

    await page.locator('#sheet .rt-card[data-route=office] [data-op=test]').click({ timeout: 4000 });
    need(await until(() => /TLS handshake only/.test(document.querySelector('#sheet .rt-card[data-route=office] .rt-t[data-s=ok]')?.textContent || '')), 'Test does not report a private handshake check');

    await page.locator('#sheet [data-op=add]').click({ timeout: 4000 });
    await sleep(250);
    await page.locator('#sheet input[aria-label="Route name"]').fill('gate-1');
    await page.locator('#sheet input[aria-label="Proxy URL"]').fill('http://gate:hunter2pw@proxy.example:8080');
    await audit('route add custom', ['#sheet']);
    await wide('the add route form');
    await page.locator('#sheet [data-op=save]').click({ timeout: 4000 });
    need(await until(() => !!document.querySelector('#sheet .rt-card[data-route="gate-1"]')), 'saving did not add the route');
    v = await open(`return { u: document.querySelector('#sheet .rt-card[data-route="gate-1"] .rt-u')?.textContent, leak: document.documentElement.innerHTML.includes('hunter2pw') }`);
    need(v.u === 'http://***@proxy.example:8080' && !v.leak, 'the password of a new route stays on the page', v);

    await page.locator('#sheet .rt-card[data-route="gate-1"] [data-op=edit]').click({ timeout: 4000 });
    await sleep(250);
    v = await open(`const u = document.querySelector('#sheet input[aria-label="Proxy URL"]'); return { value: u.value, ph: u.placeholder, name: document.querySelector('#sheet input[aria-label="Route name"]').value }`);
    need(v.value === '' && /unchanged/.test(v.ph) && v.name === 'gate-1', 'editing a route shows its saved URL', v);
    await page.locator('#sheet .rt-acts .btn', { hasText: 'Cancel' }).click({ timeout: 4000 });
    await sleep(250);
    await page.locator('#sheet .rt-card[data-route="gate-1"] [data-op=delete]').click({ timeout: 4000 });
    need(await until(() => !document.querySelector('#sheet .rt-card[data-route="gate-1"]')), 'Delete did not remove the route');

    await page.locator('#sheet [data-op=add]').click({ timeout: 4000 });
    await sleep(250);
    await page.locator('#sheet .rt-kind [data-k=tailscale]').click({ timeout: 4000 });
    await sleep(250);
    v = await open(`return { nodes: [...document.querySelectorAll('#sheet .rt-nodes .chip')].map((e) => e.textContent), url: document.querySelector('#sheet input[aria-label="Proxy URL"]').getClientRects().length, text: document.querySelector('#sheet').textContent }`);
    need(v.nodes.includes('exit-node-1') && !v.url, 'the tailscale form does not offer exit nodes', v.nodes);
    need(/separate device on your tailnet/.test(v.text), 'the tailscale form does not say it is a separate device', '');
    await audit('route add tailscale', ['#sheet']);
    await wide('the tailscale route form');
    await page.locator('#sheet .rt-acts .btn', { hasText: 'Cancel' }).click({ timeout: 4000 });

    await call({ op: '_missing', on: true });
    need(await until(() => /tailscaled missing/.test(document.querySelector('#sheet .rt-card[data-route=exit] .rt-st')?.textContent || ''), null, 6000), 'a missing tailscaled is not shown on the route');
    v = await open(`return { d: document.querySelector('#sheet .rt-card[data-route=exit] .rt-d')?.textContent || '', bins: document.querySelector('#sheet .rt-bins')?.open, pill: document.querySelector('#sheet .rt-bins .rt-st')?.textContent }`);
    need(/brew install tailscale/.test(v.d) && v.pill === 'not found', 'the missing state does not say how to install tailscaled', v);
    await audit('route tailscaled missing', ['#sheet']);
    await wide('Manage routes with tailscaled missing');
    await call({ op: '_missing', on: false });
    await c.closeAll();

    await call({ op: '_reset' });
    await call({ op: 'set', tab: A.id, route: '', force: true });
    await sleep(300);
    c.R.ok(name, 'chip, menu, badges, blocked card and Manage routes');
  });
}
