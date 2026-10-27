import { h, on, toast, skeleton, failRow } from './util.js';
import { send } from './conn.js';

let seq = 0;
const waits = new Map();
on('msg:git', (m) => {
  const w = waits.get(m.id);
  if (w) { waits.delete(m.id); w(m); }
});

function ask(frame, ms = 20000) {
  return new Promise((res) => {
    const id = 'gc' + (++seq) + '-' + Date.now();
    waits.set(id, res);
    if (!send({ t: 'git', id, ...frame })) { waits.delete(id); return res({ ok: false, error: 'not connected to GodTerm' }); }
    setTimeout(() => { if (waits.has(id)) { waits.delete(id); res({ ok: false, error: 'GodTerm did not answer in time' }); } }, ms);
  });
}

let styled = false;
function style() {
  if (styled) return;
  styled = true;
  document.head.append(h('style', {}, `
.gc{display:flex;flex-direction:column;gap:10px;min-width:0}
.gc .gc-ban{border:1px solid var(--warn);border-radius:12px;padding:10px 12px;font-size:13.5px;display:flex;gap:10px;align-items:center;flex-wrap:wrap}
.gc .gc-ban span{flex:1;min-width:200px}
.gc .gc-accts{display:flex;gap:6px;flex-wrap:wrap;align-items:center;font-size:12.5px;color:var(--dim)}
.gc .gc-chip{border:1px solid var(--line);border-radius:999px;padding:2px 9px;color:var(--text)}
.gc .gc-chip.off{color:var(--warn);border-color:var(--warn)}
.gc .gc-row{display:flex;gap:8px;align-items:center;padding:8px 10px;border-top:1px solid var(--line);flex-wrap:wrap;background:var(--bg)}
.gc .list>.gc-row:first-child{border-top:0}
.gc .gc-row input[type=text],.gc .gc-row select{min-height:36px;border:1px solid var(--line);border-radius:8px;background:var(--panel);padding:0 8px;min-width:0}
.gc .gc-o{flex:1 1 150px}.gc .gc-a{flex:1 1 150px}
.gc .gc-g{flex:2 1 200px;font-family:ui-monospace,Menlo,monospace;font-size:13px}
.gc .gc-n{width:22px;text-align:right;color:var(--dim);font-size:12.5px;flex:none}
.gc .gc-h{cursor:grab;color:var(--dim);flex:none;user-select:none;padding:0 2px}
.gc .gc-row.drag{opacity:.5}.gc .gc-row.over{box-shadow:inset 0 2px 0 var(--accent)}
.gc .gc-sub{display:flex;gap:10px;flex-wrap:wrap;align-items:center;width:100%;padding-left:30px;font-size:13px;color:var(--dim)}
.gc .gc-sub label{display:flex;gap:6px;align-items:center}
.gc .gc-sub select{min-height:34px;border:1px solid var(--line);border-radius:8px;background:var(--panel);padding:0 6px;color:var(--text);max-width:100%}
.gc .gc-sub select.gc-pick{max-width:150px}
.gc .gc-sub select.gc-own{flex:1 1 160px;max-width:260px}
.gc .gc-tag{font-size:11.5px;border:1px solid var(--warn);color:var(--warn);border-radius:6px;padding:1px 6px;flex:none}
.gc .gc-ok{color:var(--ok);font-size:12.5px;flex:none}.gc .gc-bad{color:var(--warn);font-size:12.5px;flex:none}
.gc .gc-ib{min-height:32px;min-width:32px;border:1px solid var(--line);border-radius:8px;background:var(--bg);flex:none}
.gc .gc-ib:disabled{opacity:.35}
.gc .gc-msgs{font-size:13px;display:flex;flex-direction:column;gap:3px}
.gc .gc-err{color:var(--err)}.gc .gc-warn{color:var(--warn)}
.gc .gc-pv{font-family:ui-monospace,Menlo,monospace;font-size:12.5px;padding:6px 12px;border-top:1px solid var(--line);overflow-wrap:anywhere}
.gc .gc-ph{font-size:12.5px;color:var(--dim);padding:8px 12px 2px;border-top:1px solid var(--line)}
.gc .gc-und{border-left:3px solid var(--warn);background:color-mix(in srgb,var(--warn) 8%,var(--bg))}
.gc .gc-und.gc-ph{color:var(--warn)}
.gc .gc-test{display:flex;gap:8px;align-items:center;flex-wrap:wrap;padding:8px 10px}
.gc .gc-test input{flex:1 1 220px;min-height:36px;border:1px solid var(--line);border-radius:8px;background:var(--panel);padding:0 8px;font-family:ui-monospace,Menlo,monospace;font-size:13px}
.gc .gc-test div{width:100%;font-family:ui-monospace,Menlo,monospace;font-size:12.5px;color:var(--dim);overflow-wrap:anywhere}
.gc .gc-b{flex:1 1 220px;min-width:0;font-size:13.5px;overflow-wrap:anywhere}
.gc .gc-b small{display:block;color:var(--dim);font-size:12px}
.gc .gc-acts{display:flex;gap:8px;flex-wrap:wrap}
.gc .gc-edit{display:flex;gap:8px;flex-wrap:wrap;width:100%}
dialog.gc-dlg{border:1px solid var(--line);border-radius:14px;background:var(--panel);color:var(--text);max-width:min(640px,calc(100vw - 32px));width:100%;padding:16px;box-shadow:var(--shadow)}
dialog.gc-dlg::backdrop{background:var(--scrim)}
dialog.gc-dlg h3{margin:0 0 6px;font-size:16px}
dialog.gc-dlg .gc-diff{font-family:ui-monospace,Menlo,monospace;font-size:12.5px;max-height:50vh;overflow:auto;background:var(--bg);border:1px solid var(--line);border-radius:10px;padding:8px 10px;white-space:pre-wrap;overflow-wrap:anywhere}
dialog.gc-dlg .gc-diff .p{color:var(--ok)}dialog.gc-dlg .gc-diff .m{color:var(--err)}dialog.gc-dlg .gc-diff .c{color:var(--warn)}dialog.gc-dlg .gc-diff .w{color:var(--warn)}
dialog.gc-dlg .acts{display:flex;gap:8px;margin-top:12px}
`));
}

const clone = (x) => JSON.parse(JSON.stringify(x));
const isPh = (o, view) => (view.placeholder_owners || []).some((p) => p.toLowerCase() === String(o || '').trim().toLowerCase());
const ib = (label, title, fn, disabled) => h('button.gc-ib', { type: 'button', title, 'aria-label': title, disabled: !!disabled, onclick: (e) => { e.preventDefault(); fn(); } }, label);
const btn = (label, fn, cls = '') => h('button.btn.sm' + cls, { type: 'button', onclick: (e) => { e.preventDefault(); fn(); } }, label);
const put = (el, ...kids) => el.replaceChildren(...kids.flat().filter((k) => k != null && k !== false));

export function gitcfgSection() {
  style();
  const root = h('div.gc', { 'data-q': 'github git owner owners account accounts gh rule rules folder glob repo binding bindings placeholder state' }, skeleton(3));
  const st = { view: null, saved: null, draft: null, starter: false, test: '', checked: null, tries: 0 };
  load(root, st);
  return root;
}

async function load(root, st, keep) {
  const r = await ask({ op: 'cfg_view' });
  if (!r.ok) { put(root, failRow(r.error || 'The GitHub settings did not load', () => load(root, st, keep), 'GitHub accounts and repos')); return; }
  st.view = r.view;
  st.saved = clone(r.view.draft);
  if (!keep) {
    st.starter = !!r.view.empty;
    st.draft = clone(st.starter ? r.view.placeholders : r.view.draft);
    st.draft.binds = [];
  }
  render(root, st);

  if (!r.view.accounts && st.tries++ < 5) setTimeout(() => refreshAccounts(root, st), 1500);
}

async function refreshAccounts(root, st) {
  const r = await ask({ op: 'cfg_view' });
  if (!r.ok) return;

  const same = (a, b) => JSON.stringify({ o: a.owners, r: a.rules }) === JSON.stringify({ o: b.owners, r: b.rules });
  if (st.starter && r.view.placeholders && same(st.draft, st.view.placeholders)) {
    st.draft = clone(r.view.placeholders);
    st.draft.binds = [];
  }
  st.view.accounts = r.view.accounts;
  st.view.placeholders = r.view.placeholders;
  st.view.placeholder_owners = r.view.placeholder_owners;
  if (!r.view.accounts && st.tries++ < 5) return setTimeout(() => refreshAccounts(root, st), 1500);
  render(root, st);
}

function accountState(view, name) {
  const a = (view.accounts || []).find((x) => x.name === name);
  if (!view.accounts) return null;
  return a ? a.logged_in : false;
}

function render(root, st) {
  const v = st.view, d = st.draft;
  const parts = [];
  if (st.starter) {
    parts.push(h('div.gc-ban', {},
      h('span', {}, h('b', {}, 'Placeholders. '), '[git] in config.toml is empty, so these are a suggested start. Edit anything, then Use these to save them (it shows what it writes first).'),
      btn('Use these', () => save(root, st), '.pri')));
  }

  const acc = v.accounts;
  parts.push(h('div.gc-accts', {}, h('span', {}, 'gh accounts:'),
    acc == null ? h('span', {}, 'asking gh...') : acc.length === 0 ? h('span.gc-chip.off', {}, 'none logged in (gh auth login)')
      : acc.map((a) => h('span.gc-chip' + (a.logged_in ? '' : '.off'), { title: a.logged_in ? 'logged in' : 'not logged in' }, (a.logged_in ? '✓ ' : '✗ ') + a.name + (a.active ? ' (active)' : ''))),
    btn('Refresh', () => { st.tries = 0; refreshAccounts(root, st); })));
  const names = (acc || []).map((a) => a.name);
  const dl = h('datalist', { id: 'gc-accts' }, names.map((n) => h('option', { value: n })));
  const pl = h('datalist', { id: 'gc-folders' }, (v.projects || []).map((p) => h('option', { value: p + '/**' })));
  parts.push(dl, pl);

  parts.push(h('div.st-sub', {}, 'Owners: GitHub owner (user or org) -> gh account that pushes for it'));
  const owners = h('div.list', {}, d.owners.map((o, i) => {
    const on = accountState(v, o.account.trim());
    const ph = isPh(o.owner, v);
    return h('div.gc-row', {},
      h('input.gc-o', { type: 'text', value: o.owner, placeholder: 'GitHub owner', 'aria-label': 'GitHub owner', spellcheck: 'false', autocapitalize: 'off', onfocus: (e) => { e.target.dataset.was = o.owner; }, oninput: (e) => { o.owner = e.target.value; changed(root, st); },

        onchange: (e) => { const was = e.target.dataset.was, now = o.owner.trim(); if (was && now && was !== now) { for (const r of d.rules) { if (r.owner === was) r.owner = now; r.choices = (r.choices || []).map((c) => (c === was ? now : c)); } e.target.dataset.was = now; } render(root, st); changed(root, st); } }),
      h('span.dim', {}, '->'),
      h('input.gc-a', { type: 'text', value: o.account, list: 'gc-accts', placeholder: 'gh account', 'aria-label': 'gh account for ' + o.owner, spellcheck: 'false', autocapitalize: 'off', oninput: (e) => { o.account = e.target.value; changed(root, st); }, onchange: () => render(root, st) }),
      ph ? h('span.gc-tag', { title: 'Set the real name' }, 'placeholder') : null,
      on == null ? null : on ? h('span.gc-ok', {}, '✓ logged in') : h('span.gc-bad', {}, '⚠ not logged in'),
      ib('×', 'Remove ' + o.owner, () => { d.owners.splice(i, 1); render(root, st); changed(root, st); }));
  }), h('div.gc-row', {}, btn('+ Add owner', () => { d.owners.push({ owner: '', account: names[0] || '' }); render(root, st); })));
  parts.push(owners);

  parts.push(h('div.st-sub', {}, 'Folder rules, in order: the most specific glob wins, ties go to the higher rule'));
  const ownerNames = d.owners.map((o) => o.owner).filter(Boolean);
  let dragFrom = -1;
  const rules = h('div.list', {}, d.rules.map((r, i) => {
    const choose = r.owner === 'choose';
    const sel = h('select.gc-own', { 'aria-label': 'Owner for ' + r.match, onchange: (e) => {
      r.owner = e.target.value;
      if (r.owner === 'choose' && !(r.choices || []).length) r.choices = ownerNames.slice(0, 2);
      render(root, st); changed(root, st);
    } }, [...new Set([...ownerNames, ...(choose || !r.owner ? [] : [r.owner])])].map((o) => h('option', { value: o, selected: o === r.owner }, o)),
    h('option', { value: 'choose', selected: choose }, 'choose per project...'));
    const pick = h('select.gc-pick', { 'aria-label': 'Pick a known project folder', title: 'Pick a known project folder', onchange: (e) => { if (e.target.value) { r.match = e.target.value + '/**'; render(root, st); changed(root, st); } } },
      h('option', { value: '' }, 'Pick a folder...'), (v.projects || []).map((p) => h('option', { value: p }, p)));
    const row = h('div.gc-row', { draggable: 'true',
      ondragstart: (e) => { dragFrom = i; row.classList.add('drag'); e.dataTransfer.effectAllowed = 'move'; e.dataTransfer.setData('text/plain', String(i)); },
      ondragend: () => row.classList.remove('drag'),
      ondragover: (e) => { e.preventDefault(); row.classList.add('over'); },
      ondragleave: () => row.classList.remove('over'),
      ondrop: (e) => { e.preventDefault(); row.classList.remove('over'); const from = dragFrom >= 0 ? dragFrom : +e.dataTransfer.getData('text/plain'); if (from !== i) { const [m] = d.rules.splice(from, 1); d.rules.splice(i, 0, m); render(root, st); changed(root, st); } } },
      h('span.gc-h', { title: 'Drag to reorder', 'aria-hidden': 'true' }, '⠿'),
      h('span.gc-n', {}, String(i + 1) + '.'),
      h('input.gc-g', { type: 'text', value: r.match, list: 'gc-folders', placeholder: '~/work/**', 'aria-label': 'Folder glob ' + (i + 1), spellcheck: 'false', autocapitalize: 'off', oninput: (e) => { r.match = e.target.value; changed(root, st); } }),
      ib('↑', 'Move up', () => { [d.rules[i - 1], d.rules[i]] = [d.rules[i], d.rules[i - 1]]; render(root, st); changed(root, st); }, i === 0),
      ib('↓', 'Move down', () => { [d.rules[i + 1], d.rules[i]] = [d.rules[i], d.rules[i + 1]]; render(root, st); changed(root, st); }, i === d.rules.length - 1),
      ib('×', 'Remove this rule', () => { d.rules.splice(i, 1); render(root, st); changed(root, st); }),
      h('div.gc-sub', {}, pick, h('span', {}, '->'), sel),
      h('div.gc-sub', {},
        ...(choose ? [h('span', {}, 'Choose from:'), ...ownerNames.map((o) => h('label', {}, h('input', { type: 'checkbox', checked: (r.choices || []).includes(o), onchange: (e) => {
          r.choices = (r.choices || []).filter((c) => c !== o);
          if (e.target.checked) r.choices.push(o);
          r.choices.sort((a, b) => ownerNames.indexOf(a) - ownerNames.indexOf(b));
          changed(root, st);
        } }), o))] : []),
        h('label', {}, h('input', { type: 'checkbox', checked: r.state === 'exclude', onchange: (e) => { r.state = e.target.checked ? 'exclude' : null; changed(root, st); } }),
          'Keep this folder\'s conversations and .env out of the state repo')));
    return row;
  }), h('div.gc-row', {}, btn('+ Add rule', () => { d.rules.push({ match: '', owner: ownerNames[0] || '', choices: [], state: null }); render(root, st); })));
  parts.push(rules);

  st.msgs = h('div.gc-msgs');
  st.pv = h('div.list');
  st.testOut = h('div', {}, '');
  const test = h('input', { type: 'text', value: st.test, placeholder: '~/work/team-hub', 'aria-label': 'Test a folder', spellcheck: 'false', autocapitalize: 'off', oninput: (e) => { st.test = e.target.value; changed(root, st); } });
  parts.push(st.msgs);
  parts.push(h('div.st-sub', {}, 'Preview'), h('div.list', {}, h('div.gc-test', {}, h('span.dim', {}, 'Test a folder'), test, st.testOut)), st.pv);

  parts.push(h('div.st-sub', {}, 'Per project bindings'));
  st.binds = h('div.list');
  parts.push(st.binds);

  parts.push(h('div.gc-acts', {},
    btn(st.starter ? 'Use these...' : 'Save...', () => save(root, st), '.pri'),
    btn('Discard changes', () => load(root, st)),
    h('span.mnote', {}, 'Saved to ' + (v.config || 'config.toml') + ' (comments kept) and the bindings file, after you confirm.')));

  const fa = document.activeElement;
  const lab = root.contains(fa) ? fa.getAttribute('aria-label') : null;
  put(root, parts);
  if (lab) [...root.querySelectorAll('[aria-label]')].find((e) => e.getAttribute('aria-label') === lab)?.focus();
  paintBinds(root, st);
  changed(root, st, 0);
}

let timer = 0;
function changed(root, st, wait = 250) {
  clearTimeout(timer);
  timer = setTimeout(() => check(root, st), wait);
}

async function check(root, st) {
  const r = await ask({ op: 'cfg_check', draft: st.draft, test: st.test });
  if (!r.ok) { put(st.msgs, h('div.gc-err', {}, r.error || 'The check did not answer')); return; }
  st.checked = r;
  const c = r.check || {};
  put(st.msgs, (c.errors || []).map((e) => h('div.gc-err', {}, '✗ ' + e)), (c.warnings || []).map((w) => h('div.gc-warn', {}, '⚠ ' + w)));
  st.testOut.textContent = r.test ? r.test.line : '';
  paintPreview(st, r.preview || []);
  st.preview = r.preview || [];
  paintBinds(root, st);
}

function paintPreview(st, rows) {
  const d = st.draft;
  const groups = new Map();
  for (const r of rows) {
    const k = r.rule == null ? 'u' : r.rule;
    if (!groups.has(k)) groups.set(k, []);
    groups.get(k).push(r);
  }
  const keys = [...groups.keys()].sort((a, b) => (a === 'u' ? 1e9 : a) - (b === 'u' ? 1e9 : b));
  if (!rows.length) return put(st.pv, h('div.gc-ph', {}, 'No known projects yet (open a tab in one, or the project index finds them).'));
  put(st.pv, keys.map((k) => {
    const und = k === 'u';
    const rule = und ? null : d.rules[k];
    return [
      h('div.gc-ph' + (und ? '.gc-und' : ''), {}, und ? `Undefined: no rule covers these (${groups.get(k).length})` : `${k + 1}. ${rule?.match || ''}`),
      groups.get(k).map((r) => h('div.gc-pv' + (und ? '.gc-und' : ''), {}, r.line + (r.bound ? `  ·  bound: ${r.bound}` : ''))),
    ];
  }).flat(2));
}

function paintBinds(root, st) {
  if (!st.binds) return;
  const v = st.view, d = st.draft;
  const saved = new Map((v.bindings || []).map((b) => [b.key, b]));
  for (const e of d.binds) {
    if (e.binding) saved.set(e.key, { key: e.key, ...e.binding, pending: true });
    else saved.delete(e.key);
  }
  const unbound = (d.binds || []).filter((e) => !e.binding).map((e) => e.key);
  const rows = [];
  for (const [k, b] of saved) {
    rows.push(h('div.gc-row', {},
      h('div.gc-b', {}, k, h('small', {}, `${b.owner}/${b.repo} as ${b.account}${b.pending ? ' · unsaved' : b.source ? ' · ' + b.source : ''}`)),
      btn('Edit', () => editBind(root, st, k, b)),
      btn('Unbind', () => { d.binds = d.binds.filter((e) => e.key !== k); if ((v.bindings || []).some((x) => x.key === k)) d.binds.push({ key: k, binding: null }); paintBinds(root, st); changed(root, st); })));
  }
  for (const k of unbound) rows.push(h('div.gc-row', {}, h('div.gc-b', {}, k, h('small', {}, 'unbinds on save')),
    btn('Keep', () => { d.binds = d.binds.filter((e) => e.key !== k); paintBinds(root, st); changed(root, st); })));
  const todo = (st.preview || []).filter((r) => !saved.has(r.key) && (r.rule == null || (r.choices || []).length));
  for (const r of todo) {
    rows.push(h('div.gc-row' + (r.rule == null ? '.gc-und' : ''), {},
      h('div.gc-b', {}, r.key, h('small', {}, r.rule == null ? 'undefined: no rule covers it' : 'choose: ' + r.choices.join(' or '))),
      btn('Define', () => editBind(root, st, r.key, { owner: r.owner || r.choices?.[0] || '', repo: r.key.split('/').pop().toLowerCase().replace(/\s+/g, '-').replace(/[^a-z0-9._-]/g, ''), account: r.account || '' }))));
  }
  put(st.binds, rows.length ? rows : h('div.gc-ph', {}, 'No saved bindings, and every known project has a rule.'));
}

function editBind(root, st, key, b) {
  const owners = [...new Set([b.owner, ...st.draft.owners.map((o) => o.owner)].filter(Boolean))];
  const owner = h('select', { 'aria-label': 'Owner' }, owners.map((o) => h('option', { value: o, selected: o === b.owner }, o)));
  const repo = h('input', { type: 'text', value: b.repo || '', 'aria-label': 'Repo name', placeholder: 'repo', spellcheck: 'false', autocapitalize: 'off' });
  const acct = h('input', { type: 'text', value: b.account || '', list: 'gc-accts', 'aria-label': 'gh account', placeholder: 'gh account', spellcheck: 'false', autocapitalize: 'off' });
  owner.onchange = () => { const o = st.draft.owners.find((x) => x.owner === owner.value); if (o) acct.value = o.account; };
  if (!acct.value) owner.onchange();
  const row = h('div.gc-row', {}, h('div.gc-b', {}, key, h('small', {}, 'owner / repo / account (saved with Save)')),
    h('div.gc-edit', {}, owner, h('span.dim', {}, '/'), repo, h('span.dim', {}, 'as'), acct,
      btn('Done', () => {
        st.draft.binds = st.draft.binds.filter((e) => e.key !== key);
        st.draft.binds.push({ key, binding: { owner: owner.value, repo: repo.value.trim(), account: acct.value.trim(), source: 'manual', defined_at: '' } });
        paintBinds(root, st); changed(root, st);
      }, '.pri'),
      btn('Cancel', () => paintBinds(root, st))));
  put(st.binds, row);
  repo.focus();
}

async function save(root, st) {
  clearTimeout(timer);
  const r = await ask({ op: 'cfg_check', draft: st.draft, test: st.test });
  if (!r.ok) return toast(r.error || 'The check did not answer', 'warn');
  const c = r.check || {};
  if ((c.errors || []).length) { await check(root, st); return toast('Not saved: ' + c.errors[0], 'warn'); }
  if (!r.plan_id) return toast('Nothing to save: no changes');
  const cls = (l) => (l.startsWith('+') ? 'p' : l.startsWith('-') ? 'm' : l.startsWith('~') ? 'c' : '');
  const dlg = h('dialog.gc-dlg', { 'aria-label': 'Save the GitHub settings' },
    h('h3', {}, 'Save these changes?'),
    h('div.mnote', {}, `To ${st.view.config || 'config.toml'} (its comments and other settings stay) and the bindings file.`),
    h('div.gc-diff', {}, r.diff.map((l) => h('div' + (cls(l) ? '.' + cls(l) : ''), {}, l)), (c.warnings || []).map((w) => h('div.w', {}, '⚠ ' + w))),
    h('div.acts', {},
      h('button.btn.pri', { type: 'button', onclick: async () => {
        dlg.close(); dlg.remove();
        const s = await ask({ op: 'cfg_save', plan_id: r.plan_id });
        if (!s.ok) return toast('Not saved: ' + (s.error || 'no answer'), 'warn');
        toast(s.said || 'Saved');
        load(root, st);
      } }, 'Confirm and save'),
      h('button.btn', { type: 'button', onclick: () => { dlg.close(); dlg.remove(); } }, 'Cancel')));
  document.body.append(dlg);
  dlg.showModal();
}
