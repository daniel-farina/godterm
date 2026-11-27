export async function appsChecks(c, { sleep, origin }) {
  const { page, step, audit } = c;
  const OV = ['#sheet', '#pop', '#tabpanel'];
  const open = () => page.evaluate(async () => (await window.__g.mod('apps')).appsSheet());
  const pause = (ms) => new Promise((r) => setTimeout(r, ms));
  const loaded = async () => { await page.waitForSelector('#sheet[data-kind=apps] .acard, #sheet[data-kind=apps] .empty, #sheet[data-kind=apps] .fail', { timeout: 6000 }).catch(() => {}); await pause(450); };
  const dev = async () => (await fetch(origin + '/dev/apps')).json();
  const tabs = () => page.evaluate(() => window.__g.mods.state.S.tabs.length);
  const waitFor = async (fn, arg, ms = 5000) => { try { await page.waitForFunction(fn, arg, { timeout: ms, polling: 100 }); return true; } catch { return false; } };
  const seen = () => page.evaluate(() => {
    const vis = (e) => !e.closest('[hidden]');
    const cards = [...document.querySelectorAll('#sheet-body .acard')];
    return {
      kind: document.querySelector('#sheet').dataset.kind,
      all: cards.length,
      shown: cards.filter(vis).map((x) => x.querySelector('.ac-h b').textContent),
      goneShown: cards.filter((x) => vis(x) && x.classList.contains('gone')).length,
      badges: cards.filter((x) => x.querySelector('.ac-miss')).length,
      groups: [...document.querySelectorAll('#sheet-body .ac-g')].map((g) => g.textContent),
      chips: [...document.querySelectorAll('#sheet-body .ac-chips .chip')].map((b) => b.textContent),
      folds: [...document.querySelectorAll('#sheet-body .ac-fold')].map((b) => b.textContent + '|' + b.getAttribute('aria-expanded')),
      info: document.querySelector('#sheet-body .ac-info')?.textContent || '',
    };
  });
  const chip = async (re) => { await page.locator('#sheet-body .ac-chips .chip', { hasText: re }).first().click({ timeout: 3000 }); await sleep(200); };
  const card = (name) => page.locator('#sheet-body .acard', { has: page.locator('.ac-h b', { hasText: new RegExp('^' + name + '$') }) }).filter({ visible: true }).first();
  const press = async (name, re) => { const b = card(name).locator('button', { hasText: re }).first(); await b.scrollIntoViewIfNeeded({ timeout: 3000 }); await b.click({ timeout: 3000 }); };
  const reset = async () => {
    await page.evaluate(async () => { (await window.__g.mod('util')).store.set('appsf', { text: '', kind: '', sort: 'kind' }); });
    await page.evaluate(() => { const q = document.querySelector('#sheet[data-kind=apps] input[type=search]'); if (q) { q.value = ''; q.dispatchEvent(new Event('input')); } });
    const all = page.locator('#sheet[data-kind=apps] .ac-chips .chip', { hasText: /^All / }).first();
    if (await all.count()) await all.click({ timeout: 2000 }).catch(() => {});
    const sort = page.locator('#sheet[data-kind=apps] .ac-sort select');
    if (await sort.count()) await sort.selectOption('kind').catch(() => {});
    await pause(350);
  };

  await step('apps', async () => {
    const name = 'apps';
    const need = (ok, kind, detail = '') => { if (!ok) c.R.fail(name, kind, '#sheet[data-kind=apps]', typeof detail === 'string' ? detail : JSON.stringify(detail)); return ok; };
    await open(); await loaded(); await reset();
    await audit('apps', OV);
    let s = await seen();
    need(s.kind === 'apps', 'the Apps sheet did not open', s.kind);
    need(s.all === 13, 'not every entry is listed', s.all);
    need(s.goneShown === 0 && s.shown.length === 8, 'missing entries are not folded away by default', s.shown);
    need(s.groups.some((g) => /^Undescribed/.test(g)) && s.groups.some((g) => /^Games/.test(g)), 'not grouped by kind', s.groups);
    need(s.chips.includes('Missing 5') && s.chips[0] === 'All 13', 'the kind chips or their counts are off', s.chips);
    need(s.folds.length >= 2 && s.folds.every((f) => f.endsWith('|false')), 'missing sections are not collapsed', s.folds);
    need(/13 entries/.test(s.info) && /5 missing/.test(s.info), 'the summary line is off', s.info);

    await page.fill('#sheet-body input[type=search]', 'roguelike'); await pause(400);
    s = await seen();
    need(s.shown.length === 1 && s.shown[0] === 'Lantern Run', 'search by tag did not narrow the list', s.shown);
    await page.fill('#sheet-body input[type=search]', ''); await pause(400);

    await chip(/^Games /);
    s = await seen();
    need(s.shown.length === 1 && s.shown[0] === 'Lantern Run', 'the kind chip did not filter', s.shown);
    await chip(/^All /);

    await page.selectOption('#sheet-body .ac-sort select', 'name'); await sleep(250);
    s = await seen();
    need(!s.groups.length && s.shown[0] === 'Buoy Map' && s.folds.length === 1, 'sort by name is not one flat list with the missing ones last', [s.groups, s.shown, s.folds]);
    await page.selectOption('#sheet-body .ac-sort select', 'last'); await sleep(250);
    s = await seen();
    need(s.shown[0] === 'Buoy Map' || s.shown[0] === 'Tide Clock', 'sort by last worked on does not start with the newest', s.shown);
    await page.selectOption('#sheet-body .ac-sort select', 'kind'); await sleep(250);

    await page.locator('#sheet-body .ac-fold').last().click(); await sleep(250);
    s = await seen();
    need(s.goneShown >= 1, 'a missing section did not unfold', s.folds);
    await audit('apps unfolded', OV);
    await page.locator('#sheet-body .ac-fold').last().click(); await sleep(200);

    await chip(/^Missing /);
    s = await seen();
    need(s.shown.length === 5 && s.goneShown === 5 && s.badges === 5, 'the Missing chip does not show every missing entry with its badge', s);
    await audit('apps missing', OV);

    await card('paper-kite').locator('.ac-pick').click(); await sleep(200);
    const rows = await card('paper-kite').locator('.ac-s').allTextContents();
    need(rows.length === 2 && /Kite sail outline/.test(rows[0]) && /folder gone/.test(rows[0]), 'a missing entry does not list its conversations', rows);
    await audit('apps conversations', OV);

    await chip(/^All /);
    await card('Tide Clock').locator('.ac-p').click(); await sleep(300);
    const toast = await page.evaluate(() => document.querySelector('#toasts')?.textContent || '');
    need(/Path copied|Could not copy/.test(toast), 'clicking the path did nothing', toast);
    c.R.ok(name, 'list, search, chips, sort, missing');
  });

  await step('apps open and describe', async () => {
    const name = 'apps open and describe';
    const need = (ok, kind, detail = '') => { if (!ok) c.R.fail(name, kind, '#sheet[data-kind=apps]', typeof detail === 'string' ? detail : JSON.stringify(detail)); return ok; };
    let n = await tabs();
    const grew = async () => { const ok = await waitFor((k) => window.__g.mods.state.S.tabs.length > k, n, 5000); n = await tabs(); await sleep(500); return ok; };

    await open(); await loaded(); await reset();
    await press('Lantern Run', /^Open conversation$/);
    need(await grew(), 'Open conversation did not open a tab');

    await open(); await loaded();
    await press('Kiln Timer', /^New chat here$/);
    need(await grew(), 'New chat here did not open a tab');
    need((await dev()).prefills.length === 0, 'a plain new chat got a prompt', await dev());

    await open(); await loaded();
    await press('moss-garden', /^Describe$/);
    need(await grew(), 'Describe did not open a tab');
    await sleep(400);
    let d = await dev();
    need(d.prefills.length === 1 && d.prefills[0].what === 'describe' && /moss-garden$/.test(d.prefills[0].path), 'the describe prompt was not placed', d);

    await open(); await loaded();
    await page.click('#b-apps-refresh');
    need(await grew(), 'Refresh catalog did not open a tab');
    await sleep(400);
    d = await dev();
    need(d.prefills.length === 2 && d.prefills[1].what === 'refresh', 'the refresh prompt was not placed', d);
    c.R.ok(name, 'resume, new chat, describe, refresh');
  });

  await step('apps missing folders', async () => {
    const name = 'apps missing folders';
    const need = (ok, kind, detail = '') => { if (!ok) c.R.fail(name, kind, '#sheet', typeof detail === 'string' ? detail : JSON.stringify(detail)); return ok; };
    let n = await tabs();
    const before = (await dev()).prefills.length;
    const kind = () => page.evaluate(() => document.querySelector('#sheet').dataset.kind);

    await open(); await loaded(); await reset();
    await chip(/^Missing /);
    await press('paper-kite', /^Open conversation$/); await sleep(350);
    need((await kind()) === 'apps-confirm', 'no confirmation before a folder is made', await kind());
    await audit('apps confirm', OV);
    need((await dev()).restored.length === 0 && (await tabs()) === n, 'something happened before the yes');
    await page.click('#b-apps-no'); await sleep(350);
    need((await kind()) === 'apps' && (await dev()).restored.length === 0 && (await tabs()) === n, 'Back did not just go back', await dev());

    await loaded();
    await press('paper-kite', /^Open conversation$/); await sleep(350);
    await page.click('#b-apps-yes');
    need(await waitFor((k) => window.__g.mods.state.S.tabs.length > k, n, 5000), 'the conversation of a missing folder did not open after the yes');
    n = await tabs();
    need((await dev()).restored.includes('5a1c0de0-0070'), 'the folder was not made again', await dev());
    await sleep(500);

    await open(); await loaded();
    await press('snow-globe', /^Rebuild from history$/); await sleep(350);
    need((await kind()) === 'apps-confirm', 'Rebuild did not ask first', await kind());
    await page.click('#b-apps-yes');
    need(await waitFor((k) => window.__g.mods.state.S.tabs.length > k, n, 5000), 'Rebuild did not open the conversation');
    n = await tabs();
    await sleep(500);
    let d = await dev();
    const p = d.prefills[before];
    need(p && p.what === 'rebuild' && p.resumed === true && p.session === '5a1c0de0-0080' && /snow-globe$/.test(p.path), 'the rebuild prompt was not placed in the resumed conversation', d);

    await open(); await loaded();
    await press('reed-flute', /^Open conversation$/); await sleep(350);
    await page.click('#b-apps-yes'); await sleep(900);
    d = await dev();
    need(!d.restored.includes('5a1c0de0-0090') && (await tabs()) === n, 'a folder outside the home folder was made or opened', d);
    const toast = await page.evaluate(() => document.querySelector('#toasts')?.textContent || '');
    need(/only made inside the home folder/.test(toast), 'no reason shown when the folder cannot be made', toast);

    await open(); await loaded();
    const bell = await card('Harbor Bell').locator('button').allTextContents();
    need(bell.includes('New chat in project') && bell.includes('Open conversation') && !bell.includes('New chat here'), 'an app missing inside a live project has the wrong actions', bell);
    await press('Harbor Bell', /^Open conversation$/);
    need(await waitFor((k) => window.__g.mods.state.S.tabs.length > k, n, 5000), 'a conversation whose own folder exists asked for a folder');
    await sleep(400);
    await open(); await loaded(); await reset();
    c.R.ok(name, 'confirm, back, reopen, rebuild, refused');
  });
}
