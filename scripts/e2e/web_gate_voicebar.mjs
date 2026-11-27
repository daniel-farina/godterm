export async function voiceBarChecks(c, { sleep, problems }) {
  const { page, step, audit, isDesk, size } = c;
  const name = 'voice bar';
  const open = (expr, ...a) => page.evaluate(async ([e, a]) => { const f = new Function('mod', 'a', `return (async () => { ${e} })()`); return f(window.__g.mod.bind(window.__g), a); }, [expr, a]);
  const need = (ok, kind, detail = '') => { if (!ok) c.R.fail(name, kind, '#vbar', typeof detail === 'string' ? detail : JSON.stringify(detail)); return ok; };
  const until = async (fn, arg, ms = 5000) => { try { await page.waitForFunction(fn, arg, { timeout: ms, polling: 100 }); return true; } catch { return false; } };
  const geo = () => page.evaluate(() => {
    const r = (s) => { const e = document.querySelector(s); if (!e || !e.getClientRects().length) return null; const b = e.getBoundingClientRect(); return { x: b.left, y: b.top, w: b.width, h: b.height, r: b.right, b: b.bottom }; };
    const bar = document.querySelector('#vbar');
    const br = bar.getBoundingClientRect();
    const kids = [...bar.querySelectorAll('button, input, select')].filter((e) => e.getClientRects().length).map((e) => { const b = e.getBoundingClientRect(); return { id: e.id || e.className, x: b.left, r: b.right, w: b.width, h: b.height }; });
    return { bar: r('#vbar'), panes: r('#panes'), sbar: r('#sbar'), body: r('#panes .pane:not([hidden]) .pane-body'), talk: r('#talk'), to: r('#vx-to'), more: r('#vbar .vx-more'), seg: r('#vbar .sg-mic'), spk: r('#vbar .mp-spk'),
      out: kids.filter((k) => k.x < br.left - 1 || k.r > br.right + 1).map((k) => k.id), small: kids.filter((k) => /^(talk|vx-|ctxbtn|sg-b|mp-)/.test(k.id) && (k.h < 40 || k.w < 40)).map((k) => k.id + ' ' + Math.round(k.w) + 'x' + Math.round(k.h)),
      sw: document.documentElement.scrollWidth, iw: innerWidth, parent: bar.parentElement.id };
  });
  const st = () => open(`const { S } = await mod('state'); const v = await mod('voice'); const b = document.querySelector('#vbar'); return { pick: v.voicePick(), tab: S.mode_flags?.direct_tab ?? null, follow: S.voice?.direct_follow === true, muted: S.voice?.speaker_muted === true, label: b.querySelector('#vx-to .vx-tl')?.textContent, to: b.querySelector('#vx-to')?.dataset.v, mode: b.dataset.mode, seg: [...b.querySelectorAll('.sg-mic [aria-checked=true]')].map((x) => x.dataset.m).join(), spk: b.querySelector('.mp-spk')?.getAttribute('aria-checked'), chip: document.querySelector('#b-ptt')?.dataset.mode, vox: [...document.querySelectorAll('#panes .pane.vox')].length, chatVox: document.querySelector('#chat').classList.contains('vox') }`);
  const setMode = async (m) => { await open(`(await mod('voice')).setMode(a[0])`, m); await sleep(500); };
  const pickTo = async (v) => {
    await page.locator('#vx-to').click({ timeout: 4000 });
    await sleep(350);
    await page.locator(`#pop .vx-row[data-v="${v}"]`).click({ timeout: 4000 });
  };
  const micMenu = async () => { if (!(await page.locator('#pop.mic-pop').count())) { await open(`(await mod('voice')).openMicMenu(document.querySelector('#b-ptt'))`); await sleep(450); } };

  await step(name, async () => {
    await until(() => document.querySelector('#vbar')?.childElementCount > 0, null, 12000);
    const tabs = await open(`const { S } = await mod('state'); return S.tabs.filter((t) => t.can_direct && t.state === 'ready').map((t) => ({ id: t.id, uid: t.uid, name: t.name }))`);
    if (!need(tabs.length >= 2, 'the mock has no two tabs that can take direct mode')) return;
    const [A, B] = tabs;
    await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), A.id);
    await setMode('off');
    let g = await geo();
    const offPanes = g.panes;
    if (isDesk) {
      need(g.bar && g.panes && g.bar.y >= g.panes.b - 1 && Math.abs(g.bar.x - g.panes.x) <= 1 && Math.abs(g.bar.w - g.panes.w) <= 1, 'the bar is not one row right below the panes while voice is off', g);
      need(!g.sbar || g.bar.b <= g.sbar.y + 1, 'the bar is not above the status bar', g);
      need(g.bar && g.bar.h <= (size.touch ? 56 : 46), 'the bar is taller than its slim height', g);
      need((await st()).mode === 'off' && (await st()).seg === 'off', 'the bar does not show Off', await st());
      await audit('voice bar off', ['#vbar', '#sbar']);
    } else {
      need(!g.bar, 'the bar shows on a phone while voice is off', g);
    }

    await setMode('push');
    g = await geo();
    need(g.bar && g.talk && g.to, 'the bar, Talk or Talk to is missing in push to talk', g);
    need(g.sw <= g.iw && !g.out.length, 'the bar overflows sideways', g);
    need(g.bar.y >= g.panes.b - 1 && (!g.body || g.body.b <= g.bar.y + 1), 'the bar overlaps the terminal', g);
    if (size.touch) need(!g.small.length, 'a control in the bar is under 40 px', g.small.join(', '));
    if (isDesk) need(Math.abs(g.panes.h - offPanes.h) <= 1, 'turning voice on changed the terminal size', { off: offPanes, on: g.panes });
    else need(g.more && g.panes.h < offPanes.h, 'the phone bar has no overflow button or took no room', g);
    need((await st()).chip === 'push', 'the top chip does not say push to talk', await st());
    await audit('voice bar push', ['#vbar', '#sbar', '#keyrow']);

    await pickTo('t' + B.uid);
    need(await until((uid) => window.__g.mods.state.S.mode_flags?.direct_tab === uid, B.uid), 'picking a tab in Talk to did not move voice to it', await st());
    await sleep(300);
    let s = await st();
    need(s.label === B.name && s.to === 'tab' && !s.follow, 'the Talk to label does not name the picked tab', s);
    if (isDesk) {
      await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), B.id);
      await sleep(400);
      need((await st()).vox === 1, 'the pane voice goes to is not marked', await st());
      await page.evaluate(async (id) => (await window.__g.mod('term')).panes.show(id), A.id);
      await sleep(300);
    }
    await page.locator('#vx-to').click({ timeout: 4000 });
    await sleep(350);
    await audit('voice bar talk to menu', ['#pop']);
    await page.locator('#pop .vx-row[data-v="follow"]').click({ timeout: 4000 });
    need(await until(() => window.__g.mods.state.S.voice?.direct_follow === true), 'Follow active tab did not turn following on', await st());
    await sleep(300);
    s = await st();
    need(/^Follow: /.test(s.label || '') && s.to === 'follow', 'the Talk to label does not say Follow', s);
    await pickTo('assistant');
    need(await until(() => window.__g.mods.state.S.mode_flags?.direct_tab == null), 'Assistant did not end direct mode', await st());
    await sleep(300);
    s = await st();
    need(s.label === 'Assistant' && s.to === 'assistant' && s.chatVox && s.vox === 0, 'the Talk to label does not say Assistant', s);

    g = await geo();
    if (g.seg) {
      await page.locator('#vbar .sg-mic .sg-b[data-m="off"]').click({ timeout: 4000 });
      await sleep(400);
      s = await st();
      need(s.pick === 'off' && s.chip === 'off' && s.seg === 'off', 'Off in the bar did not reach the top chip', s);
      await page.locator('#vbar .sg-mic .sg-b[data-m="push"]').click({ timeout: 4000 });
      await sleep(400);
      s = await st();
      need(s.pick === 'push' && s.chip === 'push' && s.seg === 'push', 'Push in the bar did not reach the top chip', s);
      await micMenu();
      need((await page.locator('#pop.mic-pop .sg-mic .sg-b[aria-checked=true]').getAttribute('data-m')) === 'push', 'the chip menu does not show the mode picked in the bar');
      await page.locator('#pop.mic-pop .sg-mic .sg-b[data-m="off"]').click({ timeout: 4000 });
      await sleep(400);
      s = await st();
      need(s.pick === 'off' && s.seg === 'off' && s.mode === 'off', 'Off in the chip menu did not reach the bar', s);
      await page.locator('#pop.mic-pop .sg-mic .sg-b[data-m="push"]').click({ timeout: 4000 });
      await sleep(400);
      need((await st()).seg === 'push', 'Push in the chip menu did not reach the bar', await st());
      await c.closeAll();
    } else {
      await page.locator('#vbar .vx-more').click({ timeout: 4000 });
      await sleep(450);
      need((await page.locator('#pop.mic-pop .sg-mic .sg-b[aria-checked=true]').getAttribute('data-m')) === 'push', 'the overflow button does not open the voice menu on push');
      await audit('voice bar overflow menu', ['#pop']);
      await c.closeAll();
    }

    if (g.spk) {
      await page.locator('#vbar .mp-spk').click({ timeout: 4000 });
      await sleep(400);
      s = await st();
      need(s.muted && s.spk === 'false', 'the bar switch did not turn spoken replies off', s);
      await micMenu();
      need((await page.locator('#pop.mic-pop .mp-spk').getAttribute('aria-checked')) === 'false', 'the chip menu still shows spoken replies on');
      await page.locator('#pop.mic-pop .mp-spk').click({ timeout: 4000 });
      await sleep(400);
      s = await st();
      need(!s.muted && s.spk === 'true', 'turning spoken replies on in the chip menu did not reach the bar', s);
      await c.closeAll();
    }

    {
      const lv = () => page.evaluate(() => ({ state: window.__g.mods.state.S.voice?.talk_level, bar: document.querySelector('#vbar .vx-lvl select')?.value ?? null, shown: !!document.querySelector('#vbar .vx-lvl')?.getClientRects().length, off: document.querySelector('#vbar .vx-lvl select')?.disabled === true, menu: document.querySelector('#pop.mic-pop .mm-lvl select')?.value ?? null, menuOff: document.querySelector('#pop.mic-pop .mm-lvl select')?.disabled === true }));
      const reaches = (v) => until((w) => window.__g.mods.state.S.voice?.talk_level === w, v);
      const sent = [];
      const cdp = await page.context().newCDPSession(page).catch(() => null);
      if (cdp) { await cdp.send('Network.enable'); cdp.on('Network.webSocketFrameSent', (e) => { const p = e.response?.payloadData || ''; if (p.includes('talk_level')) sent.push(p); }); }
      const before = (await lv()).state || 'normal';
      const wide = size.w >= 1200;
      let l = await lv();
      if (wide) {
        need(l.shown && l.bar === before && !l.off, 'the reply length dropdown is not in the bar at desktop width', l);
        await page.locator('#vbar .vx-lvl select').selectOption('brief');
        need(await reaches('brief'), 'picking Brief in the bar did not change the setting', await lv());
        await sleep(300);
        if (cdp) need(sent.some((p) => /"talk_level"\s*:\s*"brief"/.test(p)), 'the bar did not send talk_level brief', sent.slice(-2));
        g = await geo();
        need(g.sw <= g.iw && !g.out.length, 'the bar overflows sideways with the reply length dropdown', g);
        await audit('voice bar reply length', ['#vbar']);
      } else if (size.w < 640) {
        need(!l.shown, 'the reply length dropdown shows in the narrow bar', l);
      }
      await micMenu();
      l = await lv();
      need(l.menu === (wide ? 'brief' : before), 'the voice menu does not show the reply length picked in the bar', l);
      await page.locator('#pop.mic-pop .mm-lvl select').selectOption('detailed');
      need(await reaches('detailed'), 'picking Detailed in the voice menu did not change the setting', await lv());
      await sleep(300);
      l = await lv();
      need(l.menu === 'detailed' && (!l.shown || l.bar === 'detailed'), 'the bar and the voice menu do not agree on the reply length', l);
      g = await geo();
      const pop = await page.evaluate(() => { const b = document.querySelector('#pop.mic-pop')?.getBoundingClientRect(); const s = document.querySelector('#pop.mic-pop .mm-lvl select')?.getBoundingClientRect(); return b && s ? { l: b.left, r: b.right, sl: s.left, sr: s.right, iw: innerWidth } : null; });
      need(g.sw <= g.iw && pop && pop.l >= 0 && pop.r <= pop.iw + 1 && pop.sl >= pop.l && pop.sr <= pop.r + 1, 'the reply length row overflows the voice menu or the page', { g: { sw: g.sw, iw: g.iw }, pop });
      await audit('voice menu reply length', ['#pop']);
      await page.locator('#pop.mic-pop .mp-spk').click({ timeout: 4000 });
      await sleep(400);
      l = await lv();
      need(l.menuOff && (!l.shown || l.off), 'the reply length dropdown stays enabled while spoken replies are off', l);
      await page.locator('#pop.mic-pop .mp-spk').click({ timeout: 4000 });
      await sleep(400);
      need(!(await lv()).menuOff, 'the reply length dropdown did not come back with spoken replies', await lv());
      await open(`(await mod('conn')).tool('speaker', { talk_level: a[0] })`, before);
      await reaches(before);
      if (cdp) await cdp.detach().catch(() => {});
      await c.closeAll();
    }

    {
      const lvl = () => open(`return (await mod('voice')).outLevel()`);
      const settles = async (want) => { for (let i = 0; i < 30; i++) { const l = await lvl(); if (l.playing > 0 && l.gain != null && Math.abs(l.gain - want) < 0.03 && Math.abs(l.target - want) < 0.005) return true; await sleep(100); } return false; };
      const slide = async (pct) => { await micMenu(); await page.evaluate((v) => { const r = document.querySelector('#pop.mic-pop .vol-row .vr-r'); r.value = String(v); r.dispatchEvent(new Event('input', { bubbles: true })); }, pct); };
      const before = await open(`const { S } = await mod('state'); return { vol: S.voice?.volume_pct ?? 100, muted: S.voice?.speaker_muted === true }`);
      await open(`const { emit } = await mod('util'); const n = 24000 * 30; const b = new Uint8Array(44 + n * 2); const dv = new DataView(b.buffer); dv.setUint32(0, 0x52494646); dv.setUint32(4, 36 + n * 2, true); dv.setUint32(8, 0x57415645); dv.setUint32(12, 0x666d7420); dv.setUint32(16, 16, true); dv.setUint16(20, 1, true); dv.setUint16(22, 1, true); dv.setUint32(24, 24000, true); dv.setUint32(28, 48000, true); dv.setUint16(32, 2, true); dv.setUint16(34, 16, true); dv.setUint32(36, 0x64617461); dv.setUint32(40, n * 2, true); emit('bin:2', 0, b);`);
      if (need(await until(() => window.__g.mods.voice.outLevel().playing > 0, null, 6000), 'a reply clip did not start playing', await lvl())) {
        await slide(80);
        need(await settles(0.8), 'the output gain is not the volume while a reply plays', await lvl());
        await slide(40);
        need(await settles(0.4), 'moving the volume slider did not change the gain of the reply that is playing', await lvl());
        await slide(0);
        need(await settles(0), 'volume 0 is not silent', await lvl());
        need(!(await st()).muted, 'volume 0 flipped the spoken replies setting', await st());
        await slide(60);
        need(await settles(0.6), 'the gain did not come back from 0', await lvl());
        await sleep(400);
        await micMenu();
        await page.locator('#pop.mic-pop .mp-spk').click({ timeout: 4000 });
        need(await settles(0), 'turning spoken replies off did not silence the reply that is playing', await lvl());
        need((await st()).muted, 'the switch did not mute', await st());
        await sleep(400);
        await micMenu();
        await page.locator('#pop.mic-pop .mp-spk').click({ timeout: 4000 });
        need(await settles(0.6), 'turning spoken replies on did not bring the gain back', await lvl());
      }
      await open(`const { emit } = await mod('util'); emit('msg:audio_stop', {});`);
      await open(`(await mod('voice')).setSpeaker({ volume: a[0] / 100, muted: a[1] })`, before.vol, before.muted);
      await sleep(400);
      await c.closeAll();
    }

    await setMode('off');
    g = await geo();
    need(Math.abs(g.panes.h - offPanes.h) <= 1 && (!g.body || !g.bar || g.body.b <= g.bar.y + 1), 'the terminal did not get its size back after voice went off', { off: offPanes, now: g.panes });
    if (!isDesk) need(!g.bar, 'the bar stays on a phone after voice went off', g);
    if (!problems.some((p) => p.label === c.label && p.screen === name)) c.R.ok(name);
  });
}
