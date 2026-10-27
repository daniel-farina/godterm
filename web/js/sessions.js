import { $, h, store, toast, debounce, idOf, isDesk, skeleton, failRow, emptyState, hscroll } from './util.js';
import { tool } from './conn.js';
import { S } from './state.js';
import { openSheet, closeSheet } from './sheet.js';
import { panes } from './term.js';
import { run, li } from './manage.js';

const SORTS = [['modified', 'Modified'], ['created', 'Created'], ['messages', 'Messages'], ['tokens', 'Tokens'], ['context', 'Context'], ['project', 'Project'], ['source', 'Account'], ['title', 'Title'], ['oldest', 'Oldest']];
const WHEN = [['', 'Any time'], ['today', 'Today'], ['yesterday', 'Since yesterday'], ['3 days', '3 days'], ['last week', 'Last week'], ['30 days', '30 days']];
const PAGE = 50;
let f = store.get('sessf', { text: '', harness: 'all', source: '', state: 'all', since: '', project: '', sort: 'modified', reverse: false });
let limit = PAGE, seq = 0;

const kfmt = (n) => (n == null ? '' : n >= 1e6 ? (n / 1e6).toFixed(1) + 'M' : n >= 1e3 ? Math.round(n / 1e3) + 'k' : String(n));

function args() {
  const a = { limit, sort: f.sort };
  if (f.reverse) a.reverse = true;
  for (const k of ['text', 'since', 'project']) if (f[k]) a[k] = f[k];
  if (f.harness && f.harness !== 'all') a.harness = f.harness;
  if (f.source) a.source = /^\d+$/.test(f.source) ? +f.source : f.source;
  if (f.state && f.state !== 'all') a.state = f.state;
  if (f.headless) a.include_headless = true;
  return a;
}

function chipRow(key, opts) {
  const row = h('div.chips.fchips');
  hscroll(row);
  for (const [v, l] of opts) {
    row.append(h('button.chip' + ((f[key] || '') === v ? '.on' : ''), { onclick: (e) => {
      f[key] = v;
      for (const c of row.children) c.classList.toggle('on', c === e.currentTarget);
      paintSummary();
      reload();
    } }, l));
  }
  return row;
}

let box, info, paintSummary = () => {};
function reload() { store.set('sessf', f); limit = PAGE; query(); }

async function query() {
  const my = ++seq;
  info.textContent = 'Searching...';
  if (!box.querySelector('.srow')) box.replaceChildren(skeleton(5));
  const r = await tool('sessions', args());
  if (my !== seq) return;
  const v = r.result || r;
  if (r.ok === false && !v.sessions) { info.textContent = ''; box.replaceChildren(failRow(r.error || 'Sessions are not available', query, 'The Sessions screen')); return; }
  const rows = v.sessions || [];
  info.textContent = `${v.matched ?? rows.length} sessions${v.did_you_mean ? ' · did you mean: ' + [].concat(v.did_you_mean).map((d) => d.title || d).slice(0, 2).join(', ') : ''}${v.indexing ? ' · still indexing' : ''}`;

  if (v.headless_hidden || f.headless) {
    info.append(' · ', h('button.linkb', { onclick: () => { f.headless = !f.headless; reload(); } },
      f.headless ? 'hide scripted and temp' : `${v.headless_hidden} scripted or temp hidden: show`));
  }
  if (v.indexing) setTimeout(() => my === seq && query(), 1500);
  box.replaceChildren(...rows.map(row));
  if ((v.matched ?? 0) > rows.length) box.append(h('button.li', { style: 'justify-content:center;color:var(--accent)', onclick: () => { limit += PAGE; query(); } }, `Load more (${v.matched - rows.length} left)`));
  const filtered = !!(f.text || f.since || f.project || f.source || (f.harness && f.harness !== 'all') || (f.state && f.state !== 'all'));
  if (!rows.length) box.append(emptyState('No sessions match', filtered ? 'Fewer filters, or another word, may find it.' : 'Sessions appear here once an agent has run in a tab.',
    filtered ? { label: 'Clear filters', fn: () => { f = { text: '', harness: 'all', source: '', state: 'all', since: '', project: '', sort: f.sort, reverse: f.reverse }; store.set('sessf', f); limit = PAGE; sessionsSheet(); } } : null));
  box.hidden = false;
}

function row(s) {
  const acct = typeof s.source === 'number' ? S.accounts.find((a) => a.account === s.source)?.label || 'Account ' + s.source : s.source;
  const tok = s.tokens ? kfmt((s.tokens.new_input || 0) + (s.tokens.output || 0)) + ' tok' : '';
  return h('div.srow', { role: 'button', tabindex: 0, onclick: () => open(s) },
    h('div.st', {}, h('b', {}, s.title || s.first_prompt || s.id), s.live ? h('span.tagl.live', {}, s.open_in_tab ? 'open' : 'live') : null, h('span.tagl', {}, s.harness || '')),
    h('div.sm', {}, [s.project, acct, s.last_activity].filter(Boolean).join(' · ')),
    h('div.sm', {}, [s.messages != null && s.messages + ' msgs', tok, s.subagents && s.subagents + ' subagents', s.cost_usd && '$' + s.cost_usd].filter(Boolean).join(' · ')));
}

async function open(s) {
  if (s.open_in_tab) { closeSheet(); panes.show(idOf(s.open_in_tab)); return; }
  closeSheet();
  toast('Opening ' + (s.title || 'session') + '...', 'info');
  const r = await run(s.external ? 'take_over_session' : 'open_session', { id: s.id }, 'Opened');
  const id = r?.result?.tab || r?.result?.tab_id;
  if (id) setTimeout(() => panes.show(idOf(id)), 300);
}

export function sessionsSheet() {
  const q = h('input', { type: 'search', placeholder: 'Search sessions: name, prompts, folder', value: f.text, enterkeyhint: 'search', autocapitalize: 'off', spellcheck: 'false' });
  q.addEventListener('input', debounce(() => { f.text = q.value.trim(); reload(); }, 250));
  const proj = h('input', { type: 'search', placeholder: 'Project folder contains', value: f.project, autocapitalize: 'off', spellcheck: 'false' });
  proj.addEventListener('input', debounce(() => { f.project = proj.value.trim(); paintSummary(); reload(); }, 300));
  const sort = h('select', { 'aria-label': 'Sort' }, SORTS.map(([v, l]) => h('option', { value: v, selected: f.sort === v }, l)));
  sort.onchange = () => { f.sort = sort.value; paintSummary(); reload(); };
  const rev = h('button.tog', { 'aria-pressed': String(!!f.reverse), title: 'Reverse order', onclick: () => { f.reverse = !f.reverse; rev.setAttribute('aria-pressed', f.reverse); paintSummary(); reload(); } }, 'Reverse');
  const accts = [['', 'All accounts'], ['main', 'Main'], ...S.accounts.map((a) => [String(a.account), a.label])];
  info = h('div.mnote', { style: 'padding:0 4px' });
  box = h('div.list.slist');

  const fr = (label, el) => h('div.frow', {}, h('span.fl', {}, label), el);
  const filters = h('div.fbody', { hidden: !store.get('sessf-open', isDesk()) },
    fr('Sort', h('div.inrow', {}, sort, rev)),
    fr('Agent', chipRow('harness', [['all', 'Claude + Grok'], ['claude', 'Claude'], ['grok', 'Grok']])),
    fr('State', chipRow('state', [['all', 'All'], ['active', 'Active'], ['recent', 'Recent']])),
    fr('Time', chipRow('since', WHEN)), fr('Account', chipRow('source', accts)), fr('Folder', proj));
  const sum = h('span.lh');
  paintSummary = () => {
    const name = (list, v) => list.find(([k]) => k === (v || ''))?.[1];
    const set = [f.sort !== 'modified' && name(SORTS, f.sort), f.reverse && 'reversed', f.harness !== 'all' && name([['claude', 'Claude'], ['grok', 'Grok']], f.harness),
      f.state !== 'all' && f.state, f.since && name(WHEN, f.since), f.source && name(accts, f.source), f.project && f.project].filter(Boolean);
    sum.textContent = set.length ? set.join(' · ') : 'none';
  };
  paintSummary();
  const tog = li('filter', 'Filters', { hint: '', chev: true, fn: () => { filters.hidden = !filters.hidden; tog.classList.toggle('open', !filters.hidden); store.set('sessf-open', !filters.hidden); } });
  tog.querySelector('.ll').after(sum);
  tog.classList.toggle('open', !filters.hidden);
  openSheet({ kind: 'sessions', tall: true },
    h('div.sh-title', {}, h('span', {}, 'Sessions'), h('button.ib', { 'aria-label': 'Close', title: 'Close', onclick: closeSheet, html: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>' })),
    q, h('div.list', {}, tog, filters), info, box);
  query();
}
