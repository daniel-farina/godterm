import { h, on, toast, debounce, idOf, skeleton, failRow, emptyState, hscroll } from './util.js';
import { tool } from './conn.js';
import { S } from './state.js';
import { closeSheet } from './sheet.js';
import { panes } from './term.js';
import { run } from './manage.js';
import { emailOf } from './emails.js';
import { hbadge } from './avatar.js';

const PAGE = 30;
const HARNESSES = [['all', 'All'], ['claude', 'Claude'], ['grok', 'Grok'], ['codex', 'Codex'], ['cursor', 'Cursor'], ['antigravity', 'Antigravity'], ['opencode', 'OpenCode']];

export const HARNESS_NAMES = Object.fromEntries(HARNESSES.slice(1));

export function plainConfirm(box, s, go) {
  const accts = S.accounts.filter((a) => a.harness === 'antigravity');
  let pick = (accts.find((a) => a.logged_in !== false) || accts[0])?.account;
  const where = h('div.mnote.plain-where');
  const say = () => {
    const a = accts.find((x) => x.account === pick);
    where.textContent = a
      ? `GodTerm accounts keep their own conversations, so this one is copied into ${a.label} and resumes there${s.folder || s.project ? ', in ' + (s.folder || s.project) : ''}. The original stays where it is.`
      : 'Resuming it needs an Antigravity account in GodTerm. Add one in Accounts, then open it again.';
  };
  say();
  const row = h('div.chips.fchips');
  if (accts.length > 1) {
    for (const a of accts) {
      row.append(h('button.chip' + (a.account === pick ? '.on' : ''), { type: 'button', onclick: (e) => {
        pick = a.account;
        for (const c of row.children) c.classList.toggle('on', c === e.currentTarget);
        say();
      } }, a.label));
    }
  }
  box.hidden = false;
  box.replaceChildren(...[
    h('div.lost-wt', {}, 'Made with agy in a plain terminal'),
    where,
    accts.length > 1 ? row : null,
    h('div.acts', {},
      h('button.btn', { type: 'button', onclick: () => { box.hidden = true; } }, 'Cancel'),
      accts.length ? h('button.btn.pri', { type: 'button', onclick: () => { box.hidden = true; go(pick); } }, 'Copy and resume') : null)].filter(Boolean));
  box.scrollIntoView?.({ block: 'nearest' });
}

export function lostSessions() {
  const f = { query: '', harness: 'all', account: 'all' };
  let seq = 0, shown = 0, busy = false;
  const labels = [];

  const q = h('input', { type: 'search', placeholder: 'Search name, prompt or folder', enterkeyhint: 'search', autocapitalize: 'off', spellcheck: 'false' });
  const info = h('div.mnote.lost-info');
  const list = h('div.list.slist.lost-list');
  const more = h('button.li.lost-more', { hidden: true, style: 'justify-content:center;color:var(--accent)' });
  const confirm = h('div.lost-warn', { hidden: true });

  const chips = (key, opts) => {
    const row = h('div.chips.fchips');
    hscroll(row);
    for (const [v, l, sub] of opts) {
      row.append(h('button.chip' + (f[key] === v ? '.on' : ''), { type: 'button', title: sub || l, onclick: (e) => {
        f[key] = v;
        for (const c of row.children) c.classList.toggle('on', c === e.currentTarget);
        reload();
      } }, l));
    }
    return row;
  };
  const accts = [['all', 'All'], ['main', 'Main'], ...S.accounts.map((a) => [String(a.account), a.label, emailOf(a)])];

  const acctText = (s) => {
    const a = S.accounts.find((x) => x.account === s.account);
    return [s.account_label || (s.account == null ? 'Main' : 'Account ' + s.account), emailOf(a)].filter(Boolean).join(' · ');
  };

  function row(s) {
    const acct = h('span.lost-acct', {}, acctText(s));
    labels.push([s, acct]);
    const tags = [];
    if (s.running) tags.push(h('span.tagl.warn', {}, s.external?.desktop ? 'open in Claude Desktop' : 'running in a terminal'));
    else if (s.closed_tab) tags.push(h('span.tagl', { title: 'Closed GodTerm tab: ' + s.closed_tab.name }, 'closed tab'));
    const d = h('div.srow', { role: 'button', tabindex: 0, onclick: () => pick(s), onkeydown: (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); pick(s); } } },
      h('div.st', {}, h('span.hbw', { html: hbadge(s.harness) }), h('b', {}, s.title || s.first_prompt || s.id), ...tags),
      s.title && s.first_prompt && s.first_prompt !== s.title ? h('div.sm.lost-fp', {}, s.first_prompt) : null,
      h('div.sm', {}, h('span.lost-fold', {}, s.folder || s.project || ''), ' · ', acct, ' · ', s.last_activity || ''));
    return d;
  }

  async function open(s, how, into) {
    closeSheet();
    toast('Opening ' + (s.title || 'session') + '...', 'info');
    const sid = s.session_id || s.id;
    const openArgs = { id: sid, harness: s.harness };
    if (s.account != null) openArgs.account = s.account;
    if (into != null) { openArgs.account = into; openArgs.copy = true; }
    let r;
    if (how === 'take') {
      r = await run('take_over_session', { id: sid }, 'Opened');
      if (/not running anywhere else/.test(String(r?.error || r?.result?.error || ''))) r = await run('open_session', openArgs, 'Opened');
    } else {
      r = await run('open_session', openArgs, 'Opened');
    }
    const id = r?.result?.tab || r?.result?.tab_id;
    if (id) setTimeout(() => panes.show(idOf(id)), 300);
  }

  function pick(s) {
    if (s.plain_terminal) return plainConfirm(confirm, s, (into) => open(s, 'open', into));
    if (!s.running) return open(s, 'open');
    confirm.hidden = false;
    const dk = !!s.external?.desktop;
    confirm.replaceChildren(
      h('div.lost-wt', {}, dk ? 'Open in Claude Desktop right now' : 'Still running in a terminal'),
      h('div.mnote', {}, dk
        ? 'Claude Desktop has this conversation open. Opening a second copy here can fork it: both would write to the same session. Close it in Claude Desktop first, or open a copy anyway.'
        : 'Opening a second copy here can fork the conversation: both would write to the same session. Taking it over stops the one in the terminal and continues it here.'),
      h('div.acts', {},
        h('button.btn', { onclick: () => { confirm.hidden = true; } }, 'Cancel'),
        h('button.btn' + (dk ? '.pri' : ''), { onclick: () => open(s, 'open') }, 'Open a copy'),
        dk ? null : h('button.btn.pri', { onclick: () => open(s, 'take') }, 'Take over')));
    confirm.scrollIntoView?.({ block: 'nearest' });
  }

  async function load(append) {
    if (busy && append) return;
    const my = ++seq;
    busy = true;
    if (!append) {
      shown = 0;
      info.textContent = 'Looking...';
      if (!list.querySelector('.srow')) list.replaceChildren(skeleton(4));
    }
    const a = { limit: PAGE, offset: shown };
    if (f.query) a.query = f.query;
    if (f.harness !== 'all') a.harness = f.harness;
    if (f.account !== 'all') a.account = /^\d+$/.test(f.account) ? +f.account : f.account;
    const r = await tool('list_lost_sessions', a);
    busy = false;
    if (my !== seq) return;
    const v = r.result || r;
    if (r.ok === false && !v.sessions) { info.textContent = ''; list.replaceChildren(failRow(r.error || 'Sessions are not available', () => load(false), 'The session list')); more.hidden = true; return; }
    const rows = v.sessions || [];
    if (!append) { list.replaceChildren(); labels.length = 0; }
    list.append(...rows.map(row));
    shown += rows.length;
    const total = v.matched ?? shown;
    info.textContent = `${total} not open in GodTerm${v.indexing ? ' · still indexing' : ''}`;
    if (v.indexing) setTimeout(() => my === seq && load(false), 1500);
    if (!shown) {
      const active = [f.query && `search "${f.query}"`, f.harness !== 'all' && 'agent ' + (HARNESS_NAMES[f.harness] || f.harness),
        f.account !== 'all' && 'account ' + (accts.find(([k]) => k === f.account)?.[1] || f.account)].filter(Boolean);
      list.replaceChildren(emptyState('No other sessions', active.length ? `Filters on: ${active.join(' · ')}. Fewer filters, or another word, may find it.` : 'Every session you have is open in a tab.'));
    }
    more.hidden = shown >= total;
    more.textContent = `Show more (${total - shown} left)`;
  }
  more.onclick = () => load(true);

  function reload() { confirm.hidden = true; load(false); }
  q.addEventListener('input', debounce(() => { f.query = q.value.trim(); reload(); }, 250));

  const off = on('emails', () => {
    if (!list.isConnected) return off();
    for (const [s, el] of labels) el.textContent = acctText(s);
  });

  const root = h('div.lost', {},
    h('div.mnote', {}, 'Sessions started in a plain terminal, or whose tab was closed. Tap one to resume it here.'),
    q,
    h('div.frow', {}, h('span.fl', {}, 'Agent'), chips('harness', HARNESSES)),
    h('div.frow', {}, h('span.fl', {}, 'Account'), chips('account', accts)),
    info, confirm, list, more);
  load(false);
  return root;
}
