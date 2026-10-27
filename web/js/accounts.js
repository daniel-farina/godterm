import { h, on, toast, pctClass, icon, emit, words, skeleton, failRow } from './util.js';
import { send } from './conn.js';
import { openSheet, closeSheet, sheetKind } from './sheet.js';

let seq = 0;
const waits = new Map();
let view = null;
on('msg:accounts', (m) => {
  if (m.view) view = m.view;
  const w = m.id != null && waits.get(m.id);
  if (w) { waits.delete(m.id); w(m); return; }

  if (m.op === 'view' && sheetKind === 'accounts') paint();
});
on('conn', (s) => { if (s === 'live' && sheetKind === 'accounts') ask({ op: 'watch', on: true }).then(() => paint()); });

function ask(frame, ms = 60000) {
  return new Promise((res) => {
    const id = 'a' + (++seq) + '-' + Date.now();
    waits.set(id, res);
    if (!send({ t: 'accounts', id, ...frame })) { waits.delete(id); return res({ ok: false, error: 'not connected to GodTerm' }); }
    setTimeout(() => { if (waits.has(id)) { waits.delete(id); res({ ok: false, error: 'GodTerm did not answer in time' }); } }, ms);
  });
}

async function act(frame, done) {
  const r = await ask(frame);
  if (!r.ok) { toast(r.error || 'It did not work', 'error'); paint(); return r; }
  if (r.said || done) toast(r.said || done, 'ok');
  if (r.confirm) return confirmSheet(r.confirm);
  paint();
  return r;
}

let styled = false;
function style() {
  if (styled) return;
  styled = true;
  document.head.append(h('style', {}, `
.acc{display:flex;flex-direction:column;gap:12px;min-width:0}
.acc .card{border:1px solid var(--line);border-radius:14px;background:var(--bg);padding:12px;display:flex;flex-direction:column;gap:10px;min-width:0}
.acc .card.removed{opacity:.55}
.acc .top{display:flex;align-items:center;gap:10px;min-width:0}
.acc .dotc{width:12px;height:12px;border-radius:50%;flex:none}
.acc .nm{font-weight:600;font-size:16px;flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.acc .pill{flex:none;line-height:1.5;font-size:12px;border:1px solid var(--chip-line);background:var(--chip);color:var(--chip-dim);border-radius:999px;padding:2px 9px;white-space:nowrap}
.acc .pill.ok{color:var(--ok);border-color:var(--ok)}
.acc .pill.warn{color:var(--warn);border-color:var(--warn)}
.acc .pill.err{color:var(--err);border-color:var(--err)}
.acc .sub{font-size:13px;color:var(--dim);overflow-wrap:anywhere}
.acc .row{display:flex;gap:8px;flex-wrap:wrap}
.acc .row .btn{flex:1 1 auto}
.acc .flow{border:1px solid var(--line);border-radius:12px;background:var(--panel);padding:12px;display:flex;flex-direction:column;gap:10px}
.acc .flow .st{font-weight:600}
.acc .flow a.btn{text-decoration:none;min-height:56px;font-size:17px}
.acc .ucode{font:600 26px/1.2 ui-monospace,Menlo,monospace;letter-spacing:2px;text-align:center;padding:6px;border:1px dashed var(--line);border-radius:10px;user-select:all}
.acc .codebox{display:flex;gap:8px}
.acc .codebox input{flex:1;min-width:0;font-size:16px;padding:10px 12px;border:1px solid var(--line);border-radius:10px;background:var(--bg);color:var(--text)}
.acc .flow .bad{color:var(--err);font-size:13.5px;overflow-wrap:anywhere}
.acc .tok{display:flex;flex-direction:column;gap:8px;border-top:1px solid var(--line);padding-top:10px}
.acc .tok .mono{font-family:ui-monospace,Menlo,monospace;font-size:13px}
.acc label.ck{display:flex;gap:10px;align-items:center;font-size:14px}
.acc label.ck input{width:20px;height:20px;accent-color:var(--accent)}
.acc .note{font-size:12.5px;color:var(--dim)}
.acc .agents{display:grid;grid-template-columns:repeat(auto-fill,minmax(150px,1fr));gap:8px}
.acc .agent{border:1px solid var(--line);border-radius:12px;padding:10px;text-align:left;background:var(--bg);display:flex;flex-direction:column;gap:2px;min-height:64px}
.acc .agent[aria-pressed=true]{border-color:var(--accent);background:var(--panel2)}
.acc .agent small{color:var(--dim);font-size:12px}
.acc .agent:disabled{opacity:.5}
.acc .sw{display:flex;gap:8px;flex-wrap:wrap}
.acc .sw button{width:34px;height:34px;border-radius:50%;border:2px solid transparent}
.acc .sw button[aria-pressed=true]{border-color:var(--text)}
.acc .field input{padding:10px 12px;border:1px solid var(--line);border-radius:10px;background:var(--bg)}
`));
}

const put = (el, ...kids) => el.replaceChildren(...kids.flat().filter((k) => k != null && k !== false));
const title = (t) => h('div.sh-title', {}, h('span', {}, t), h('button.ib', { 'aria-label': 'Close', title: 'Close', onclick: closeSheet, html: icon('x') }));
const btn = (label, fn, cls = '', extra = {}) => h('button.btn' + cls, { type: 'button', onclick: (e) => { e.preventDefault(); fn(); }, ...extra }, label);

const STATE = {
  logged_in: ['Logged in', 'ok'], logged_out: ['Logged out', 'warn'], expired: ['Expired', 'err'], cli: ['Login in its CLI', ''],
};
const STAGE = {
  starting: 'Starting the login...', sign_in: 'Sign in on the page', code: 'Paste the code from the page',
  checking: 'Checking the code...', done: 'Done', failed: 'It did not work',
};

const drafts = {};
let body = null;

export async function accountsSheet() {
  style();
  const mine = h('div.acc', {}, skeleton(3));

  openSheet({ kind: 'accounts', tall: true, onClose: () => { if (body === mine) { ask({ op: 'watch', on: false }); body = null; } } },
    title('Accounts'), mine);
  body = mine;
  const r = await ask({ op: 'watch', on: true });
  if (!r.ok) { put(body, failRow(r.error, accountsSheet, 'The Accounts screen')); return; }
  paint();
}

function paint() {
  if (!body || !view || sheetKind !== 'accounts') return;

  const a = document.activeElement;
  if (a && body.contains(a) && (a.tagName === 'INPUT' || a.tagName === 'TEXTAREA')) { setTimeout(paint, 700); return; }
  const top = body.parentElement?.scrollTop;
  put(body,
    h('div.row', {}, btn('Add account', addSheet, '.pri', { 'data-i': 'plus' })),
    view.accounts.map(card),
    syncBox(),
    h('div.note', {}, 'Logins run each CLI\'s own sign in, in a tab of that account. Long-lived tokens stay in the Mac\'s keychain: this page only ever sees their last four characters.'));
  if (top != null && body.parentElement) body.parentElement.scrollTop = top;
}

function usage(a) {
  return (a.buckets || []).map((b) => h('div.bar', {}, h('span.bn', {}, b.bucket),
    h('span.bt', { html: `<i class="${pctClass(b.left_pct)}" style="width:${Math.max(0, Math.min(100, b.left_pct ?? 0))}%"></i>` }),
    h('span.bv', {}, `${b.left_pct ?? '?'}%${b.resets_at ? ' · ' + b.resets_at : ''}`)));
}

function card(a) {
  const [sl, sc] = STATE[a.state] || [words(a.state), ''];
  const who = [a.email, a.plan, a.org].filter(Boolean).join(' · ');
  const n = a.account;
  const busy = a.flow && !['done', 'failed'].includes(a.flow.stage);
  const loginLabel = a.state === 'logged_in' ? 'Re-login' : 'Log in';
  return h('div.card' + (a.removed ? '.removed' : ''), { 'data-account': n },
    h('div.top', {}, h('span.dotc', { style: `background:${a.color}` }), h('span.nm', {}, a.label),
      a.assistant ? h('span.pill.ok', {}, 'Assistant') : a.assistant_next ? h('span.pill', {}, 'Assistant next') : null,
      h('span.pill.' + (sc || 'x'), {}, a.removed ? 'Removed' : sl)),
    h('div.sub', {}, [a.harness_label, `${a.tabs} ${a.tabs === 1 ? 'tab' : 'tabs'}`, a.installed ? '' : 'CLI not installed'].filter(Boolean).join(' · ')),
    who ? h('div.sub', {}, who) : null,
    a.state === 'logged_out' ? null : usage(a),
    a.usage_error && a.state !== 'logged_out' ? h('div.sub', {}, words(a.usage_error)) : null,
    a.flow ? flowBox(a) : null,
    a.removed ? h('div.note', {}, 'It goes away when GodTerm restarts.') : [
      h('div.row', {},
        busy ? null : btn(loginLabel, () => act({ op: 'login', account: n }), a.state === 'logged_in' ? '.sm' : '.pri'),
        a.integrated && a.state === 'logged_in' ? btn('Log out', () => act({ op: 'logout', account: n }), '.sm.bad') : null,
        !a.assistant && (a.harness === 'claude' || a.harness === 'grok') && a.state === 'logged_in' ? btn('Use for the assistant', () => act({ op: 'assistant', account: n }), '.sm') : null),
      h('div.row', {},
        btn('Rename', () => renameSheet(a), '.sm'),
        btn('Color', () => colorSheet(a), '.sm'),
        btn('Remove', () => removeSheet(a), '.sm.bad')),
      a.can_token ? tokenBox(a) : null,
      a.state === 'cli' ? h('div.note', {}, `${a.harness_label} keeps its login itself (shared on this Mac). Log in opens its login in a tab.`) : null,
    ]);
}

function flowBox(a) {
  const f = a.flow, n = a.account;
  const what = f.kind === 'token' ? 'Long-lived token' : 'Login';
  const done = f.stage === 'done', failed = f.stage === 'failed';
  const kids = [h('div.st', {}, `${what}: ${f.note && (done || failed) ? f.note : STAGE[f.stage] || f.stage}`)];
  if (!done && !failed) {
    if (f.url && !f.open_on_mac) kids.push(h('a.btn.pri', { href: f.url, target: '_blank', rel: 'noopener noreferrer' }, 'Open sign-in page'));
    if (f.url && f.open_on_mac) kids.push(h('div.note', {}, 'This sign in comes back to the Mac itself, so it opens there.'), btn('Open on the Mac', () => act({ op: 'open_mac', account: n }, 'Opened on the Mac'), '.pri'));
    if (f.user_code) kids.push(h('div.note', {}, 'Check the page shows this code:'), h('div.ucode', {}, f.user_code));
    if (f.wants_code) {
      const inp = h('input', { type: 'text', value: drafts[n] || '', placeholder: 'Paste the code here', autocomplete: 'one-time-code', autocapitalize: 'off', autocorrect: 'off', spellcheck: 'false', enterkeyhint: 'send', 'aria-label': 'Login code' });
      const go = async () => {
        const code = inp.value.trim();
        if (!code) return toast('Paste the code from the sign in page first', 'warn');
        inp.blur();
        delete drafts[n];
        await act({ op: 'code', account: n, code });
      };
      inp.addEventListener('input', () => { drafts[n] = inp.value; });
      inp.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); go(); } });
      kids.push(h('div.codebox', {}, inp, btn('Send', go, '.pri.sm')));
    }
    if (f.error) kids.push(h('div.bad', {}, f.error));
    if (f.retry) kids.push(btn('Try again', () => act({ op: 'retry', account: n }), '.sm'));
  }
  kids.push(h('div.row', {},
    f.tab ? btn('Show the tab', () => { closeSheet(); emit('msg:ui', { action: 'show', tab: f.tab }); }, '.sm') : null,
    !done && !failed ? btn('Stop', () => act({ op: 'cancel', account: n }), '.sm') : null));
  return h('div.flow', {}, kids);
}

function syncBox() {
  const ts = view.token_sync || {};
  if (!view.accounts.some((a) => a.token)) return null;
  const on = h('input', { type: 'checkbox', checked: !!ts.on });
  on.onchange = () => act({ op: 'token_sync_all', on: on.checked });
  return h('div.card', {},
    h('label.ck', {}, on, 'Sync tokens to my other machines'),
    h('div.note', {}, ts.sync_enabled
      ? `Only the tokens you mark, into ${ts.repo}, and only while GitHub says it is private (checked before every push). Other machines keep them in their own keychain.`
      : 'GodTerm Sync is off on this Mac: nothing syncs until it is set up.'),
    ts.warning ? h('div.flow', {}, h('div.bad', {}, ts.warning)) : null);
}

function tokenBox(a) {
  const n = a.account, t = a.token;
  if (!t) {
    return h('div.tok', {},
      h('div.sub', {}, 'Long-lived token: none. One lasts a year, for headless use on this Mac (inference only; Remote Control needs the normal login).'),
      h('div.row', {}, btn('Create long-lived token', () => act({ op: 'token_create', account: n }), '.sm')));
  }
  const use = h('input', { type: 'checkbox', checked: !!t.use });
  use.onchange = () => act({ op: 'token_use', account: n, on: use.checked });
  const sync = h('input', { type: 'checkbox', checked: !!t.sync });
  sync.onchange = () => act({ op: 'token_sync', account: n, on: sync.checked });
  return h('div.tok', {},
    h('div.sub', {}, 'Long-lived token ', h('span.mono', {}, 'sk-ant-…' + t.last4), t.created ? ' · made ' + t.created : '', t.from ? ` · from ${t.from}` : ''),
    h('label.ck', {}, use, 'Use it for the assistant on this account'),
    h('label.ck', {}, sync, 'Sync this token to my other machines' + (view.token_sync?.on ? '' : ' (turn on syncing below too)')),
    h('div.row', {},
      btn('Copy to the Mac\'s clipboard', () => act({ op: 'token_copy', account: n }), '.sm'),
      btn('Delete', () => act({ op: 'token_delete', account: n }), '.sm.bad')));
}

function confirmSheet(c) {
  return new Promise((resolve) => {
    openSheet({ kind: 'confirm' }, h('div.sh-title', {}, h('span', {}, 'Confirm')), h('div.appr-q', {}, c.question),
      h('div.appr-btns', {},
        h('button.btn.bad', { onclick: async () => { closeSheet(); resolve(await act({ op: 'confirm', confirm_id: c.id })); reopen(); } }, c.yes || 'Yes'),
        h('button.btn', { onclick: async () => { closeSheet(); await ask({ op: 'confirm', confirm_id: c.id, answer: 'no' }); resolve(null); reopen(); } }, 'Cancel')));
  });
}

function reopen() { if (!sheetKind) accountsSheet(); }

function renameSheet(a) {
  style();
  const inp = h('input', { type: 'text', value: a.label, maxlength: 40, enterkeyhint: 'done' });
  const go = async () => { closeSheet(); await act({ op: 'rename', account: a.account, label: inp.value.trim() }); accountsSheet(); };
  inp.addEventListener('keydown', (e) => e.key === 'Enter' && go());
  openSheet({ kind: 'acct-edit' }, title('Rename ' + a.label), h('div.acc', {}, h('label.field', {}, 'Name', inp),
    h('div.acts', {}, btn('Cancel', () => { closeSheet(); accountsSheet(); }), btn('Save', go, '.pri'))));
  setTimeout(() => inp.focus(), 50);
}

function swatches(cur, pick) {
  const box = h('div.sw', {}, (view?.swatches || []).map((s) => h('button', { type: 'button', 'aria-label': s.name, title: s.name, 'aria-pressed': String(s.name === cur), style: `background:${s.hex}`,
    onclick: () => { pick(s.name); for (const b of box.children) b.setAttribute('aria-pressed', String(b.title === s.name)); } })));
  return box;
}

function colorSheet(a) {
  style();
  let cur = a.color_name;
  openSheet({ kind: 'acct-edit' }, title('Color of ' + a.label), h('div.acc', {}, swatches(cur, (c) => { cur = c; }),
    h('div.acts', {}, btn('Cancel', () => { closeSheet(); accountsSheet(); }), btn('Save', async () => { closeSheet(); await act({ op: 'color', account: a.account, color: cur }); accountsSheet(); }, '.pri'))));
}

function removeSheet(a) {
  style();
  const del = h('input', { type: 'checkbox' });
  openSheet({ kind: 'acct-edit' }, title('Remove ' + a.label), h('div.acc', {},
    h('div.appr-q', {}, `${a.label} leaves GodTerm; its tabs stop. Its folder (${a.config_dir}) with the CLI's login, conversations and settings is kept unless you tick this:`),
    h('label.ck', {}, del, 'Also delete its folder (asked again, on its own)'),
    h('div.acts', {}, btn('Cancel', () => { closeSheet(); accountsSheet(); }),
      btn('Continue', async () => { closeSheet(); await act({ op: 'remove', account: a.account, delete_data: del.checked }); reopen(); }, '.bad'))));
}

function addSheet() {
  style();
  const agents = view?.agents || [];
  let agent = (agents.find((x) => x.installed) || agents[0] || {}).id || 'claude';
  let color = view?.swatches?.[(view?.accounts?.length || 0) % (view?.swatches?.length || 1)]?.name || 'sage';
  const name = h('input', { type: 'text', value: `Account ${(view?.accounts?.length || 0) + 1}`, maxlength: 40, autocapitalize: 'words' });
  const folder = h('input', { type: 'text', value: '~', autocapitalize: 'off', autocorrect: 'off', spellcheck: 'false' });
  const how = h('div.note');
  const cards = h('div.agents', {}, agents.map((g) => h('button.agent', { type: 'button', 'aria-pressed': String(g.id === agent), 'data-id': g.id,
    onclick: () => { agent = g.id; for (const b of cards.children) b.setAttribute('aria-pressed', String(b.dataset.id === agent)); how.textContent = g.installed ? '' : `${g.name} is not installed. ${g.how || ''}`; } },
  h('span', {}, g.name), h('small', {}, g.installed ? g.sub : 'not installed'))));
  const go = async () => {
    const r = await ask({ op: 'add', label: name.value.trim(), harness: agent, color, folder: folder.value.trim() || '~' });
    if (!r.ok) return toast(r.error || 'It did not work', 'error');
    toast(r.said || 'Added', 'ok');
    closeSheet();
    accountsSheet();
  };
  openSheet({ kind: 'acct-edit', tall: true }, title('Add account'), h('div.acc', {},
    h('label.field', {}, 'Name', name),
    h('div.field', {}, 'Agent', cards), how,
    h('div.field', {}, 'Color', swatches(color, (c) => { color = c; })),
    h('label.field', {}, 'Folder new tabs start in', folder),
    h('div.note', {}, 'Adding it starts its login in a tab of its own; the sign in page then shows on the Accounts screen.'),
    h('div.acts', {}, btn('Cancel', () => { closeSheet(); accountsSheet(); }), btn('Add and log in', go, '.pri'))));
}
