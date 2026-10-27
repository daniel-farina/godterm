import { h, on, toast, idOf, skeleton, failRow } from './util.js';
import { tool, send } from './conn.js';
import { closeSheet } from './sheet.js';
import { panes } from './term.js';
import { ownQuestion } from './chat.js';

let styled = false;
function style() {
  if (styled) return;
  styled = true;
  document.head.append(h('style', {}, `
.sy{display:flex;flex-direction:column;gap:10px;min-width:0}
.sy .sy-intro{border:1px solid var(--line);border-radius:12px;padding:12px 14px;font-size:13.5px;line-height:1.45;background:var(--panel)}
.sy .sy-intro p{margin:0 0 6px}.sy .sy-intro p:last-child{margin:0}
.sy .sy-intro b{font-weight:600}
.sy .sy-form{display:flex;gap:8px;flex-wrap:wrap;align-items:flex-end;padding:10px 12px}
.sy .sy-form .field{flex:1 1 170px;min-width:0}
.sy .sy-form input,.sy .sy-form select{min-height:38px;border:1px solid var(--line);border-radius:8px;background:var(--panel);padding:0 8px;min-width:0;width:100%;color:var(--text);font-size:14px}
.sy .sy-acts{display:flex;gap:8px;flex-wrap:wrap;padding:0 12px 10px}
.sy .sy-acts .btn{flex:0 1 auto}
.sy .sy-cards{display:grid;grid-template-columns:repeat(auto-fit,minmax(140px,1fr));gap:8px}
.sy .sy-card{border:1px solid var(--line);border-radius:12px;padding:10px 12px;background:var(--panel);min-width:0;display:flex;flex-direction:column;gap:3px}
.sy .sy-card small{font-size:11.5px;color:var(--dim);text-transform:uppercase;letter-spacing:.04em}
.sy .sy-card b{font-size:15px;font-weight:600;overflow-wrap:anywhere}
.sy .sy-card span{font-size:12.5px;color:var(--dim);overflow-wrap:anywhere}
.sy .sy-card.ok{border-left:3px solid var(--ok)}.sy .sy-card.warn{border-left:3px solid var(--warn)}.sy .sy-card.bad{border-left:3px solid var(--err)}.sy .sy-card.dim{border-left:3px solid var(--line)}
.sy .sy-card a{color:var(--accent);font-size:12.5px;cursor:pointer;text-decoration:none}
.sy .sy-badge{font-size:11.5px;border:1px solid var(--line);border-radius:6px;padding:1px 7px;flex:none;color:var(--dim);white-space:nowrap}
.sy .sy-badge.ok{color:var(--ok);border-color:color-mix(in srgb,var(--ok) 60%,var(--line))}
.sy .sy-badge.warn{color:var(--warn);border-color:var(--warn)}
.sy .sy-plan{border:1px solid var(--line);border-radius:12px;background:var(--bg);overflow:hidden}
.sy .sy-plan .sy-ph{padding:10px 12px;font-size:13.5px;border-bottom:1px solid var(--line);display:flex;gap:8px;align-items:flex-start}
.sy .sy-plan .sy-ph span{flex:1;min-width:0;overflow-wrap:anywhere}
.sy .sy-kv,dialog.sy-dlg .sy-kv{display:grid;grid-template-columns:minmax(110px,auto) 1fr;gap:4px 12px;padding:10px 12px;font-size:13px;margin:0}
.sy .sy-kv dt,dialog.sy-dlg .sy-kv dt{color:var(--dim)}.sy .sy-kv dd,dialog.sy-dlg .sy-kv dd{margin:0;overflow-wrap:anywhere}
.sy .sy-ws{display:flex;flex-direction:column;gap:4px;padding:8px 12px;border-top:1px solid var(--line);font-size:13px}
.sy .sy-ws div{color:var(--warn);overflow-wrap:anywhere}.sy .sy-ws div.i{color:var(--dim)}
.sy .sy-ws div::before{content:'\\26A0\\FE0E\\00a0';}.sy .sy-ws div.i::before{content:'';}
.sy details.sy-raw summary{cursor:pointer;padding:8px 12px;font-size:12.5px;color:var(--dim);border-top:1px solid var(--line)}
.sy details.sy-raw pre{margin:0;padding:8px 12px 12px;font-size:12px;white-space:pre-wrap;overflow-wrap:anywhere;font-family:ui-monospace,Menlo,monospace;color:var(--dim)}
.sy .sy-op{border:1px solid var(--line);border-radius:12px;padding:10px 12px;font-size:13.5px;display:flex;gap:10px;align-items:center;flex-wrap:wrap;background:var(--panel)}
.sy .sy-op.ok{border-left:3px solid var(--ok)}.sy .sy-op.bad{border-left:3px solid var(--err)}.sy .sy-op.run{border-left:3px solid var(--busy)}
.sy .sy-op span{flex:1;min-width:200px;overflow-wrap:anywhere}
.sy .sy-op ul{margin:4px 0 0;padding-left:18px;color:var(--dim);font-size:12.5px;width:100%}
.sy .sy-spin{width:14px;height:14px;border:2px solid var(--line);border-top-color:var(--busy);border-radius:50%;animation:syspin 1s linear infinite;flex:none}
@keyframes syspin{to{transform:rotate(360deg)}}
@media (prefers-reduced-motion:reduce){.sy .sy-spin{animation:none}}
.sy .li.st .sy-btns{display:flex;gap:6px;flex:none}
dialog.sy-dlg{border:1px solid var(--line);border-radius:14px;background:var(--panel);color:var(--text);max-width:min(560px,calc(100vw - 32px));width:100%;padding:16px;box-shadow:var(--shadow)}
dialog.sy-dlg::backdrop{background:var(--scrim)}
dialog.sy-dlg h3{margin:0 0 8px;font-size:16px}
dialog.sy-dlg p{margin:0 0 10px;font-size:14px;line-height:1.45}
dialog.sy-dlg .sy-kv{padding:8px 0;font-size:13.5px;border-top:1px solid var(--line);border-bottom:1px solid var(--line);margin-bottom:10px}
dialog.sy-dlg .sy-priv{display:inline-block;font-size:12px;font-weight:600;letter-spacing:.04em;border:1px solid var(--ok);color:var(--ok);border-radius:6px;padding:1px 7px}
dialog.sy-dlg .acts{display:flex;gap:8px;margin-top:12px}
`));
}

const btn = (label, fn, cls = '', attrs = {}) => h('button.btn.sm' + cls, { type: 'button', ...attrs, onclick: (e) => { e.preventDefault(); fn(e.currentTarget); } }, label);
const put = (el, ...kids) => el.replaceChildren(...kids.flat().filter((k) => k != null && k !== false));
const res = (r) => r?.result || r || {};
const errOf0 = (r) => r?.error || r?.result?.error || (r?.ok === false && !r?.needs_confirmation ? 'It did not work' : '');

const errOf = (r) => { const e = errOf0(r); return /^unknown tool sync_/.test(e) ? 'this needs a newer GodTerm: restart GodTerm, then try again' : e; };
const L = (rows) => { rows = rows.flat().filter(Boolean); return rows.length ? h('div.list', {}, rows) : null; };
const sub = (t, q = '') => h('div.st-sub', { 'data-q': ('sync ' + t + ' ' + q).toLowerCase() }, t);
const note = (t) => h('div.mnote.st-note', {}, t);
const row = (label, help, control, q = '') => h('div.li.st', { 'data-q': ('sync ' + label + ' ' + (help || '') + ' ' + q).toLowerCase() },
  h('span.stl', {}, h('span', {}, label), help ? h('small', {}, help) : null), control);
const lh = (t, cls = '') => h('span.lh' + cls, {}, t || '');
const ago = (ms) => {
  if (!ms) return 'never';
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  return s < 60 ? `${s}s ago` : s < 3600 ? `${Math.round(s / 60)} min ago` : s < 86400 ? `${Math.round(s / 3600)} h ago` : `${Math.round(s / 86400)} d ago`;
};
const mb = (b) => (b >= 1 << 30 ? (b / (1 << 30)).toFixed(1) + ' GB' : (b / (1 << 20)).toFixed(1) + ' MB');
const visBadge = (v) => !v ? h('span.sy-badge', {}, 'not checked')
  : v === 'PRIVATE' ? h('span.sy-badge.ok', {}, 'private')
  : v === 'CHECKING' ? h('span.sy-badge', {}, 'checking')
  : h('span.sy-badge.warn', {}, v.toLowerCase() + ': not private');

let seq = 0;
const waits = new Map();
on('msg:git', (m) => { const w = waits.get(m.id); if (w) { waits.delete(m.id); w(m); } });
function ghAccounts() {
  return new Promise((done) => {
    const id = 'sy' + (++seq) + '-' + Date.now();
    waits.set(id, (m) => done((m.view?.accounts || []).filter((a) => a.logged_in !== false).map((a) => a.name)));
    if (!send({ t: 'git', id, op: 'cfg_view' })) { waits.delete(id); done([]); }
    setTimeout(() => { if (waits.has(id)) { waits.delete(id); done([]); } }, 8000);
  });
}

function confirmDialog(name, c, title, details) {
  return new Promise((resolve) => {
    if (c.token) ownQuestion(c.token);
    const d = h('dialog.sy-dlg', { 'aria-label': title });
    const answer = async (yes) => {
      d.close();
      d.remove();
      const r = await tool(name, { confirm_token: c.token }, 60000, { user_says: yes ? 'yes' : 'no' });
      resolve(yes ? r : null);
    };
    d.append(...[h('h3', {}, title), details, h('p', {}, c.question || c.needs_confirmation || 'Go ahead?'),
      h('div.acts', {}, h('button.btn.pri', { type: 'button', onclick: () => answer(true) }, 'Yes, go ahead'), h('button.btn', { type: 'button', onclick: () => answer(false) }, 'No'))].filter(Boolean));
    d.addEventListener('cancel', (e) => { e.preventDefault(); answer(false); });
    document.body.append(d);
    d.showModal();
  });
}

const kv = (pairs) => h('dl.sy-kv', {}, pairs.filter(Boolean).flatMap(([k, v]) => [h('dt', {}, k), h('dd', {}, v)]));

function planCard(v, opts = {}) {
  const pf = v.preflight || {}, p = v.plan;
  const ws = v.warnings || [];
  const pairs = [
    ['State repo', h('span', {}, pf.state_repo || '', ' ', pf.exists ? visBadge(pf.visibility) : h('span.sy-badge', {}, pf.exists === false ? 'to be created, private' : 'not checked'))],
    pf.convos ? ['Conversation repos', pf.convos + ' (private, one a month)'] : null,
    ['gh account', (pf.account || 'none') + (pf.github ? (pf.logged_in ? ' (logged in)' : ' (not logged in)') : '')],
    pf.github ? null : ['Remote', pf.remote || ''],
  ];
  if (p) {
    pairs.push(
      ['Conversations', `${p.sessions} (${p.hot} recent), ${mb(p.session_bytes || 0)}`],
      ['To upload', `${mb(p.new_bytes || 0)} in ${p.batches} sync${p.batches === 1 ? '' : 's'}`],
      ['Account files', `${p.files} (${mb(p.file_bytes || 0)}): settings, skills, memory, assistant history`],
      ['.env files', `${p.env_files} in ${p.env_projects} projects`],
      ['Config and stores', (p.config ? 'portable config.toml keys; ' : '') + Object.entries(p.stores || {}).map(([k, n]) => `${k} ${n}`).join(', ')],
      p.left_out?.length ? ['Left out', `${p.left_out.length} (vault mentions, temp folders)`] : null,
      p.projects_not_backed_up?.length ? ['Projects not backed up', `${p.projects_not_backed_up.length} (sync never commits or pushes them)`] : null,
    );
  }
  return h('div.sy-plan', { 'data-q': 'sync plan dry run preview repo account conversations env secrets warnings ' + ws.join(' ').toLowerCase() },
    h('div.sy-ph', {}, h('span', {}, v.say || ''), opts.extra || null),
    kv(pairs),
    ws.length ? h('div.sy-ws', {}, ws.map((w) => h('div', {}, w))) : h('div.sy-ws', {}, h('div.i', {}, 'No warnings.')),
    v.text ? h('details.sy-raw', {}, h('summary', {}, 'The full plan'), h('pre', {}, v.text)) : null);
}

function opCard(op, refresh) {
  if (!op) return null;
  const what = op.kind === 'init' ? 'Enabling sync' : 'Joining';
  if (op.ok == null) {
    return h('div.sy-op.run', { 'data-q': 'sync setup running init clone' }, h('div.sy-spin'),
      h('span', {}, `${what} with ${op.target}: running since ${ago(op.started_ms)}. A first upload can take a while; this updates on its own.`));
  }
  if (op.finished_ms && Date.now() - op.finished_ms > 3600e3 && op.ok) return null;
  const s = op.summary || {};
  return h('div.sy-op' + (op.ok ? '.ok' : '.bad'), { 'data-q': 'sync setup result init clone ' + (op.error || '') },
    h('span', {}, op.ok ? (s.say || `${what} with ${op.target}: done.`) : `${what} with ${op.target} failed: ${op.error}`),
    op.ok ? null : btn('Check again', () => refresh()),
    op.ok && s.logins?.length ? h('ul', {}, s.logins.map((l) => h('li', {}, l))) : null,
    op.ok && s.missing_paths?.length ? h('ul', {}, s.missing_paths.map((m) => h('li', {}, `${m} has no folder here: add it under [sync.paths] in config.toml`))) : null);
}

function offView(root, st, x, refresh) {
  const accts = st.gh || [];
  const owner = h('input', { type: 'text', value: st.owner ?? (x.owner || accts[0] || ''), placeholder: 'GitHub user or org', autocomplete: 'off', spellcheck: 'false', 'aria-label': 'Owner' });
  const acct = h('select', { 'aria-label': 'gh account' },
    (accts.length ? accts : [x.account || '']).filter((a, i, l) => l.indexOf(a) === i).map((a) => h('option', { value: a, selected: a === (st.account || x.account || accts[0]) }, a || 'no gh login yet')));
  const planBox = h('div');
  const curArgs = () => ({ owner: owner.value.trim() || undefined, account: acct.value || undefined });
  owner.oninput = () => { st.owner = owner.value; if (accts.includes(owner.value.trim())) acct.value = owner.value.trim(); };
  acct.onchange = () => { st.account = acct.value; };
  const showPlan = (v) => {
    st.plan = v;
    st.planArgs = JSON.stringify(curArgs());
    put(planBox, planCard(v));
    enable.disabled = !v.preflight?.can_init;
  };
  const preview = async (b) => {
    b.disabled = true;
    const was = b.textContent;
    b.textContent = 'Checking...';
    put(planBox, note('Building the dry run: reading what would be synced and asking gh about the repo...'));
    const r = await tool('sync_plan', curArgs(), 70000);
    b.disabled = false;
    b.textContent = was;
    if (errOf(r)) { put(planBox, note('The plan did not finish: ' + errOf(r))); return null; }
    showPlan(res(r));
    return res(r);
  };
  const enable = btn('Enable sync on this machine', async (b) => {

    let v = st.plan;
    if (!v || st.planArgs !== JSON.stringify(curArgs())) v = await preview(prev);
    if (!v?.preflight?.can_init) { if (v) toast('Enable is blocked: see the warnings', 'warn'); return; }
    b.disabled = true;
    const r = await tool('sync_init', curArgs());
    b.disabled = false;
    const c = r.needs_confirmation ? r : r.result?.needs_confirmation ? r.result : null;
    if (!c) { toast(errOf(r) || 'It did not work', 'error'); return; }
    const pf = v.preflight;
    const got = await confirmDialog('sync_init', c, 'Enable GodTerm Sync on this machine?', kv([
      ['Repo', h('span', {}, pf.state_repo + ' ', h('span.sy-priv', {}, 'PRIVATE'))],
      ['gh account', pf.account],
      pf.convos ? ['Then', pf.convos + ', private'] : null,
      v.plan ? ['First backup', `${v.plan.sessions} conversations, ${mb(v.plan.new_bytes || 0)} to upload, ${v.plan.env_files} .env files`] : null,
    ]));
    if (!got) return toast('Nothing was created', 'info');
    if (errOf(got)) return toast(errOf(got), 'error');
    toast(res(got).say || 'Started', 'ok');
    refresh();
  }, '.pri', { disabled: !st.plan?.preflight?.can_init });
  const prev = btn('Preview (dry run)', preview);
  if (st.plan) put(planBox, planCard(st.plan));

  const guess = st.repo ?? ((accts[0] || x.owner) ? `${accts[0] || x.owner}/godterm-state` : '');
  const repo = h('input', { type: 'text', value: guess, placeholder: 'OWNER/godterm-state', autocomplete: 'off', spellcheck: 'false', 'aria-label': 'State repo to join' });
  const jacct = h('select', { 'aria-label': 'gh account for joining' },
    (accts.length ? accts : ['']).map((a) => h('option', { value: a }, a || 'the owner')));
  const label = h('input', { type: 'text', value: st.label || '', placeholder: 'This machine\'s name (optional)', autocomplete: 'off', 'aria-label': 'This machine\'s name' });
  repo.oninput = () => { st.repo = repo.value; const o = repo.value.split('/')[0]; if (accts.includes(o)) jacct.value = o; };
  label.oninput = () => { st.label = label.value; };
  const o0 = guess.split('/')[0];
  if (accts.includes(o0)) jacct.value = o0;
  const joinBox = h('div');
  const jargs = () => ({ repo: repo.value.trim(), account: jacct.value || undefined });
  const check = async (b) => {
    if (!/^[^/\s]+\/[^/\s]+$/.test(repo.value.trim())) { toast('Give the state repo as OWNER/NAME', 'warn'); return null; }
    if (b) b.disabled = true;
    put(joinBox, note('Asking gh about ' + repo.value.trim() + '...'));
    const r = await tool('sync_plan', jargs(), 70000);
    if (b) b.disabled = false;
    if (errOf(r)) { put(joinBox, note('The check did not finish: ' + errOf(r))); return null; }
    put(joinBox, planCard(res(r)));
    return res(r);
  };
  const join = btn('Join', async (b) => {
    const v = await check(null);
    if (!v) return;
    if (!v.preflight?.can_clone) { toast('Join is blocked: see the warnings', 'warn'); return; }
    b.disabled = true;
    const r = await tool('sync_clone', { ...jargs(), label: label.value.trim() || undefined });
    b.disabled = false;
    const c = r.needs_confirmation ? r : r.result?.needs_confirmation ? r.result : null;
    if (!c) { toast(errOf(r) || 'It did not work', 'error'); return; }
    const got = await confirmDialog('sync_clone', c, 'Join GodTerm Sync from this machine?', kv([
      ['Repo', h('span', {}, v.preflight.state_repo + ' ', visBadge(v.preflight.visibility))],
      ['gh account', v.preflight.account],
      label.value.trim() ? ['This machine', label.value.trim()] : null,
    ]));
    if (!got) return toast('Nothing was changed', 'info');
    if (errOf(got)) return toast(errOf(got), 'error');
    toast(res(got).say || 'Started', 'ok');
    refresh();
  }, '.pri');

  put(root,
    h('div.sy-intro', { 'data-q': 'sync off what is sync backup github private machines enable join' },
      h('p', {}, h('b', {}, 'Sync is off on this machine. '), 'GodTerm Sync keeps GodTerm\'s own state in private GitHub repos you own: config, accounts (never their logins), jobs, archive, skills and memory, project .env files and recent conversations. Another machine can then join and pick up where you left off; one machine owns each conversation at a time.'),
      h('p', {}, 'It never commits to, pushes or creates a project repo, never copies a login or keychain item, and never reads the private vault. Preview shows exactly what would go in first.')),
    opCard(st.op, refresh),
    sub('Enable sync on this machine', 'first machine init create'),
    h('div.list', {},
      h('div.sy-form', { 'data-q': 'sync owner gh account github user org' },
        h('label.field', {}, 'Owner (GitHub user or org)', owner),
        h('label.field', {}, 'gh account that reads and writes', acct)),
      h('div.sy-acts', { 'data-q': 'sync preview dry run plan enable init' }, prev, enable)),
    planBox,
    sub('Join from another machine', 'clone new machine restore'),
    h('div.list', {},
      h('div.sy-form', { 'data-q': 'sync join clone repo owner godterm-state label machine name' },
        h('label.field', {}, 'State repo', repo),
        h('label.field', {}, 'gh account', jacct),
        h('label.field', {}, 'This machine', label)),
      h('div.sy-acts', { 'data-q': 'sync check join clone' }, btn('Check', check), join)),
    joinBox,
    tokenRow(res(st.status)),
  );
}

function tokenRow(x) {
  const ts = x.token_sync || {};
  return L([row('Long-lived tokens', ts.on ? 'Synced to your other machines (only into a repo checked private)' : 'Not synced: turn it on per token in Accounts',
    btn('Open Accounts', () => import('./accounts.js').then((m) => m.accountsSheet())), 'token tokens accounts keychain')]);
}

function convRow(c, here, refresh) {
  const name = c.name || String(c.session || '').slice(0, 8);
  const tab = c.tab;
  const show = tab ? btn('Show tab', () => { closeSheet(); setTimeout(() => panes.show(idOf(tab)), 50); }) : null;
  let act = null;
  if (tab && here) act = btn('Hand over', () => planned('release_session', { tab }, refresh));
  if (tab && !here) act = btn('Take over', () => planned('take_over', { tab }, refresh));
  const help = here ? (c.asked_by ? `${c.asked_by} asks for it` : 'owned here') : `${c.owner}, ${c.live ? c.active : 'offline'}`;
  return row(name, help, h('span.sy-btns', {}, show, act), 'conversation lease ' + (here ? 'owned here' : 'owned elsewhere ' + (c.owner || '')));
}

async function planned(name, args, refresh) {
  const r = await tool(name, args);
  const c = r.needs_confirmation ? r : r.result?.needs_confirmation ? r.result : null;
  if (!c) { if (errOf(r)) toast(errOf(r), 'error'); else toast(res(r).say || 'Done', 'ok'); return refresh(); }
  const got = await confirmDialog(name, c, name === 'take_over' ? 'Take over this conversation?' : 'Hand this conversation over?');
  if (got && errOf(got)) toast(errOf(got), 'error');
  else if (got) toast(res(got).say || 'Done', 'ok');
  refresh();
}

function onView(root, st, x, refresh) {
  const last = x.last || {}, ls = x.leases || {};
  const failing = last.ok === false;
  const here = ls.owned_here || [], away = ls.owned_elsewhere || [];
  const convAnchor = h('div');
  const cards = h('div.sy-cards', { 'data-q': 'sync status last next machine repo conversations' },
    h('div.sy-card' + (x.paused ? '.dim' : failing ? '.bad' : '.ok'), {}, h('small', {}, 'Status'),
      h('b', {}, x.paused ? 'Paused' : failing ? 'Failing' : x.worker?.running ? 'Syncing now' : 'On'),
      h('span', {}, x.paused ? 'Background sync waits' : failing ? (last.error || 'the last sync failed') : `state repo ${x.state_repo}`)),
    h('div.sy-card' + (failing ? '.warn' : ''), {}, h('small', {}, 'Last sync'),
      h('b', {}, ago(last.at_ms)), h('span', {}, failing ? `last good ${ago(last.last_ok_ms)}` : last.pushed ? `pushed ${last.sessions_pushed || 0} conversations, ${mb(last.bytes_pushed || 0)}` : 'nothing new to push')),
    h('div.sy-card', {}, h('small', {}, 'Next sync'), h('b', {}, x.next?.words || 'soon'),
      h('span', {}, [x.next?.note, last.waiting ? `${last.waiting} changes waiting` : 'nothing waiting'].filter(Boolean).join(', '))),
    h('div.sy-card', {}, h('small', {}, 'This machine'), h('b', {}, x.machine?.label || ''), h('span', {}, x.machine?.id || '')),
    h('div.sy-card', {}, h('small', {}, 'Conversations'), h('b', {}, `${here.length} here, ${away.length} elsewhere`),
      h('a', { href: '#', onclick: (e) => { e.preventDefault(); convAnchor.scrollIntoView({ block: 'start', behavior: 'smooth' }); } }, 'Leases below')));
  const repos = (x.repos || []).map((r) => row(r.name, r.role === 'state' ? 'State repo: config, stores, .env files, leases' : `Conversations${r.size ? ', ' + r.size : ''}${r.sealed ? ', sealed' : ''}`, visBadge(r.visibility), 'repo private visibility ' + (r.visibility || '')));
  const ws = x.warnings || [];
  const paused = !!x.paused;
  const actions = h('div.sy-acts', { 'data-q': 'sync now pause resume plan view' },
    btn('Sync now', async () => {
      const r = await tool('sync_now', {});
      toast(errOf(r) || res(r).say || 'Syncing', errOf(r) ? 'error' : 'ok');
      setTimeout(refresh, 1500);
    }, '.pri'),
    btn(paused ? 'Resume' : 'Pause', async () => {
      const r = await tool('sync_pause', { pause: !paused });
      toast(errOf(r) || res(r).say, errOf(r) ? 'error' : 'ok');
      refresh();
    }),
    btn(st.plan ? 'Hide plan' : 'View plan', async (b) => {
      if (st.plan) { st.plan = null; return refresh(true); }
      b.disabled = true;
      b.textContent = 'Building...';
      const r = await tool('sync_plan', {}, 70000);
      if (errOf(r)) { toast('The plan did not finish: ' + errOf(r), 'error'); b.disabled = false; b.textContent = 'View plan'; return; }
      st.plan = res(r);
      refresh(true);
    }));
  const mach = (x.leases?.machines?.length ? x.leases.machines : (x.machines || []).map((m) => ({ label: m.label, id: String(m.id || '').slice(0, 8), active: m.seen, live: true })))
    .map((m) => row(m.label + (m.this ? ' (this one)' : ''), m.id || '', lh(m.active || '', m.live ? '' : '.sy-dim'), 'machine heartbeat ' + (m.label || '')));
  const pr = x.projects_not_backed_up || [];
  put(root,
    opCard(st.op, refresh),
    cards,
    h('div.list', {}, actions),
    st.plan ? planCard(st.plan) : null,
    ws.length ? sub('Warnings', 'warning') : null,
    ws.length ? h('div.sy-plan', { 'data-q': 'sync warnings ' + ws.join(' ').toLowerCase() }, h('div.sy-ws', { style: 'border-top:0' }, ws.map((w) => h('div', {}, w)))) : null,
    sub('Repos', 'private visibility github'), L(repos) || note('No repo yet'),
    mach.length ? sub('Machines', 'heartbeat') : null, L(mach),
    convAnchor,
    sub('Conversations owned here', 'lease leases'),
    here.length ? L(here.map((c) => convRow(c, true, refresh))) : note(ls.fetched ? 'None' : 'Not fetched yet'),
    away.length ? sub('Owned elsewhere', 'lease leases take over') : null, L(away.map((c) => convRow(c, false, refresh))),
    (ls.requests || []).length ? sub('Take over requests', 'request') : null,
    L((ls.requests || []).map((c) => row(c.name || String(c.session).slice(0, 8), c.direction === 'in' ? `${c.by} asks for it` : `asked ${c.to}`, lh(''), 'request take over'))),
    (ls.paused_jobs || []).length ? sub('Paused jobs', 'job') : null,
    L((ls.paused_jobs || []).map((j) => row(j.name, j.why, lh(''), 'job paused'))),
    pr.length ? sub('Projects not backed up', 'project backup github') : null,
    L(pr.map((p) => row(p.path, (p.warnings || []).join(', '), lh('', '.sy-warn'), 'project backup warning'))),
    tokenRow(x),
    note(x.never || 'Project repos are never committed to, pushed or created by the sync.'),
  );
}

export function syncSection() {
  style();
  const root = h('div.sy', {}, skeleton(2));
  const st = { status: null, op: null, plan: null, gh: null, timer: 0 };
  const refresh = async (keep) => {
    clearTimeout(st.timer);
    if (!root.isConnected && st.status) return;
    if (!keep || !st.status) st.status = await tool('sync_status', {});
    const r = st.status;
    if (errOf(r)) { put(root, failRow(errOf(r), () => refresh(), 'Sync')); return; }
    const x = res(r);
    st.op = x.op;

    if (st.on != null && st.on !== !!x.enabled) st.plan = null;
    st.on = !!x.enabled;
    if (!x.enabled && st.gh == null) st.gh = await ghAccounts();

    if (root.contains(document.activeElement) && document.activeElement.matches('input,select') && !keep) {
      st.timer = setTimeout(refresh, 2000);
      return;
    }
    (x.enabled ? onView : offView)(root, st, x, refresh);
    root.dispatchEvent(new CustomEvent('sy-render', { bubbles: true }));
    const busy = x.op && x.op.ok == null;
    if (busy || x.worker?.running) st.timer = setTimeout(refresh, busy ? 2000 : 10000);
  };
  refresh();
  return root;
}
