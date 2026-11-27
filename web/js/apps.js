import { h, on, store, toast, debounce, idOf, skeleton, failRow, emptyState, hscroll, icon } from './util.js';
import { send } from './conn.js';
import { S } from './state.js';
import { openSheet, closeSheet, sheetKind } from './sheet.js';
import { panes } from './term.js';
import { run, openSession } from './manage.js';

const KIND_NAMES = { app: 'Apps', game: 'Games', utility: 'Utilities', dashboard: 'Dashboards', page: 'Pages', tool: 'Tools', library: 'Libraries', other: 'Other', undescribed: 'Undescribed' };
const KIND_ONE = { app: 'App', game: 'Game', utility: 'Utility', dashboard: 'Dashboard', page: 'Page', tool: 'Tool', library: 'Library', other: 'Other', undescribed: 'Undescribed' };
const ORDER = Object.keys(KIND_NAMES);
const SORTS = [['kind', 'Kind'], ['name', 'Name'], ['last', 'Last worked on']];

const waits = new Map();
let seq = 0;
let f = { text: '', kind: '', sort: 'kind', ...store.get('appsf', {}) };
let data = null, box, info, chips, loadSeq = 0;
const open = new Set();
const unfolded = new Set();

on('msg:apps', (m) => {
  const w = m.id != null && waits.get(m.id);
  if (w) { waits.delete(m.id); w(m); }
});

function ask(frame) {
  return new Promise((res) => {
    const id = 'a' + (++seq) + '-' + Date.now();
    waits.set(id, res);
    if (!send({ t: 'apps', id, ...frame })) { waits.delete(id); return res({ ok: false, error: 'not connected to GodTerm' }); }
    setTimeout(() => { if (waits.has(id)) { waits.delete(id); res({ ok: false, error: 'GodTerm did not answer in time' }); } }, 20000);
  });
}

function copy(text) {
  const legacy = () => {
    const t = h('textarea', { readOnly: true, 'aria-hidden': 'true', tabindex: '-1', style: 'position:fixed;left:-9999px;top:0;opacity:0' });
    t.value = text;
    let ok = false;
    try { document.body.append(t); t.select(); ok = document.execCommand('copy'); } catch { ok = false; }
    t.remove();
    return ok;
  };
  const done = (ok) => toast(ok ? 'Path copied' : 'Could not copy the path', ok ? 'ok' : 'warn');
  if (navigator.clipboard?.writeText) {
    let p;
    try { p = navigator.clipboard.writeText(text); } catch { return done(legacy()); }
    p.then(() => done(true), () => done(legacy()));
    return;
  }
  done(legacy());
}

function account() {
  const ok = (a) => a && a.available !== false && a.logged_in !== false;
  const last = store.get('newtab.acct', null);
  const a = S.accounts.find((x) => x.account === last && ok(x)) || S.accounts.find(ok);
  return a ? a.account : null;
}

async function place(pre) {
  const m = await ask({ op: 'prefill', ...pre });
  if (m.ok) toast('The prompt is waiting in the tab: read it, then send it', 'info', null, 6000);
  else toast(m.error || 'The prompt could not be placed in the tab', 'error');
}

async function openTab(dir, pre) {
  const acct = account();
  if (acct == null) return toast('No account is ready for a new tab', 'warn');
  closeSheet();
  toast('Opening a tab...', 'info');
  const r = await run('open_tab', { account: acct, dir }, 'Tab opened');
  const id = r?.result?.tab || r?.result?.tab_id || r?.result?.tabs?.[0];
  if (!id) return;
  setTimeout(() => panes.show(idOf(id)), 300);
  if (pre) await place({ ...pre, tab: id });
}

function confirmFolder(s, title, yes) {
  return new Promise((resolve) => {
    let done = false;
    const end = (v) => { if (!done) { done = true; resolve(v); } };
    openSheet({ kind: 'apps-confirm', onClose: () => end(false) },
      h('div.sh-title', {}, h('span', {}, title)),
      h('div.appr-q.ac-q', {}, 'The folder ', h('b', {}, s.folder), ' is no longer on disk.'),
      h('div.mnote', {}, 'A conversation can only be reopened in the folder it ran in. Saying yes makes an empty folder at that same path and resumes there. Nothing else is created or changed, and nothing is created if you go back.'),
      h('div.acts', {},
        h('button.btn.pri', { type: 'button', id: 'b-apps-yes', onclick: () => { end(true); closeSheet(); } }, yes),
        h('button.btn', { type: 'button', id: 'b-apps-no', onclick: () => { end(false); appsSheet(); } }, 'Back')));
  });
}

async function ensureFolder(s, title, yes) {
  if (!s.gone) return 'ok';
  if (!(await confirmFolder(s, title, yes))) return 'no';
  const m = await ask({ op: 'restore', session: s.id, confirm: true });
  if (m.ok) {
    s.gone = false;
    if (m.created) toast('Made the empty folder ' + m.folder, 'ok');
    return 'ok';
  }
  toast(m.error || 'The folder could not be made again', 'error', null, 7000);
  return 'failed';
}

async function shownTab(s) {
  toast('Opening ' + (s.title || 'the conversation') + '...', 'info');
  const r = await openSession(s);
  const id = r?.result?.tab || r?.result?.tab_id;
  if (id) setTimeout(() => panes.show(idOf(id)), 300);
  return id || null;
}

async function resume(s) {
  if ((await ensureFolder(s, 'Reopen this conversation', 'Make the folder and open')) !== 'ok') return;
  closeSheet();
  await shownTab(s);
}

export async function rebuild(a) {
  const s = a.sessions[0];
  const state = await ensureFolder(s, 'Rebuild ' + a.name, 'Make the folder and rebuild');
  if (state === 'no') return;
  closeSheet();
  const pre = { what: 'rebuild', path: a.path, session: s.id };
  const id = state === 'ok' ? await shownTab(s) : null;
  if (id) return place({ ...pre, tab: id, resumed: true });
  toast('The conversation could not be resumed: a new tab will read its history instead', 'warn', null, 6000);
  await openTab(state === 'ok' ? s.cwd : data?.workspace || '~', { ...pre, resumed: false });
}

export const describe = (a) => openTab(a.path, { what: 'describe', path: a.path });
export const refreshCatalog = () => openTab(data?.workspace || '~', { what: 'refresh' });

const hay = (a) => [a.name, a.description, a.shown_path, a.path, a.tech, a.project_name, ...(a.tags || [])].join(' ').toLowerCase();

function shown() {
  const words = f.text.toLowerCase().split(/\s+/).filter(Boolean);
  const rows = (data?.apps || []).filter((a) => (!f.kind || (f.kind === 'missing' ? a.missing : a.kind === f.kind)) && words.every((w) => hay(a).includes(w)));
  const byName = (a, b) => a.name.localeCompare(b.name, undefined, { sensitivity: 'base' });
  const byLast = (a, b) => (b.last_ms || 0) - (a.last_ms || 0) || byName(a, b);
  if (f.sort === 'name') rows.sort(byName);
  else if (f.sort === 'last') rows.sort(byLast);
  else rows.sort((a, b) => ORDER.indexOf(a.kind) - ORDER.indexOf(b.kind) || byName(a, b));
  return rows;
}

function sessionRow(s) {
  return h('button.ac-s', { type: 'button', onclick: () => resume(s) },
    h('b', {}, s.title || s.id), h('small', {}, [s.when, s.harness, s.source, s.gone && 'folder gone'].filter(Boolean).join(' · ')));
}

function card(a) {
  const key = a.kind + '|' + a.path + '|' + a.name;
  const n = a.conversations || 0;
  const older = h('div.ac-list', { hidden: !open.has(key) }, (a.sessions || []).map(sessionRow),
    n > (a.sessions || []).length ? h('div.ac-more', {}, `${n - a.sessions.length} older ones are in Sessions`) : null);
  const pick = n > 1 ? h('button.btn.sm.ac-pick', { type: 'button', 'aria-expanded': String(open.has(key)), title: 'Pick another conversation', onclick: () => {
    older.hidden = !older.hidden;
    pick.setAttribute('aria-expanded', String(!older.hidden));
    if (older.hidden) open.delete(key); else open.add(key);
  } }, `All ${n}`) : null;
  const meta = [a.project_name && a.project !== a.path && 'in ' + a.project_name, a.last ? 'worked on ' + a.last : 'never worked on with an agent', n === 1 ? '1 conversation' : n + ' conversations'].filter(Boolean);
  return h('div.acard' + (a.missing ? '.gone' : ''), { 'data-kind': a.kind },
    h('div.ac-h', {}, h('b', {}, a.name), a.missing ? h('span.tagl.ac-miss', { title: 'This folder is no longer on disk' }, 'Missing') : null, h('span.tagl', {}, KIND_ONE[a.kind] || a.kind)),
    a.description || !a.missing ? h('div.ac-d' + (a.description ? '' : '.none'), {}, a.description || 'Not described yet.') : null,
    a.tech ? h('div.ac-t', {}, a.tech) : null,
    a.path ? h('button.ac-p', { type: 'button', title: 'Copy the path', 'aria-label': 'Copy the path ' + a.shown_path, onclick: () => copy(a.path) }, h('span', {}, a.shown_path), h('i', { html: icon('copy') })) : null,
    h('div.ac-m', {}, meta.join(' · ')),
    h('div.acts.ac-acts', {},
      n ? h('button.btn.sm.pri', { type: 'button', title: a.sessions[0].title || '', onclick: () => resume(a.sessions[0]) }, 'Open conversation') : null,
      pick,
      a.path && !a.missing ? h('button.btn.sm', { type: 'button', onclick: () => openTab(a.path) }, 'New chat here') : null,
      a.missing && n ? h('button.btn.sm', { type: 'button', title: 'Ask an agent to restore this project from what its newest conversation shows', onclick: () => rebuild(a) }, 'Rebuild from history') : null,
      a.missing && !a.project_missing && a.project ? h('button.btn.sm', { type: 'button', title: a.project, onclick: () => openTab(a.project) }, 'New chat in project') : null,
      !a.described && !a.missing ? h('button.btn.sm', { type: 'button', title: 'Ask an agent to describe this project in the catalog', onclick: () => describe(a) }, 'Describe') : null),
    older);
}

function paintChips() {
  const count = {};
  for (const a of data?.apps || []) count[a.kind] = (count[a.kind] || 0) + 1;
  const all = (data?.apps || []).length;
  const gone = (data?.apps || []).filter((a) => a.missing).length;
  const opts = [['', 'All', all], ...ORDER.filter((k) => count[k]).map((k) => [k, KIND_NAMES[k], count[k]])];
  if (gone) { count.missing = gone; opts.push(['missing', 'Missing', gone]); }
  if (f.kind && !count[f.kind]) f.kind = '';
  chips.replaceChildren(...opts.map(([k, l, c]) => h('button.chip' + (f.kind === k ? '.on' : ''), { type: 'button', 'aria-pressed': String(f.kind === k), onclick: () => { f.kind = k; save(); paintChips(); paint(); } }, `${l} ${c}`)));
}

function paint() {
  if (!data) return;
  const rows = shown();
  const all = data.apps.length, un = data.apps.filter((a) => !a.described).length;
  const lost = data.apps.filter((a) => a.missing).length;
  info.textContent = [`${rows.length === all ? all : rows.length + ' of ' + all} ${all === 1 ? 'entry' : 'entries'}`, un && un + ' not described', lost && lost + ' missing', data.indexing && 'still indexing conversations'].filter(Boolean).join(' · ');
  if (!rows.length) {
    const filtered = !!(f.text || f.kind);
    box.replaceChildren(emptyState(filtered ? 'Nothing matches' : 'Nothing here yet',
      filtered ? 'Another word, or fewer filters, may find it.' : 'Projects appear once an agent has worked in them. Refresh catalog asks an agent to describe what you have built.',
      filtered ? { label: 'Clear filters', fn: () => { f.text = ''; f.kind = ''; save(); appsSheet(); } } : null));
    return;
  }
  const section = (key, list) => {
    if (f.kind === 'missing') return [h('div.agrid', {}, list.map(card))];
    const live = list.filter((x) => !x.missing), gone = list.filter((x) => x.missing);
    const out = live.length ? [h('div.agrid', {}, live.map(card))] : [];
    if (!gone.length) return out;
    const grid = h('div.agrid', { hidden: !unfolded.has(key) }, gone.map(card));
    const fold = h('button.ac-fold', { type: 'button', 'aria-expanded': String(unfolded.has(key)), onclick: () => {
      grid.hidden = !grid.hidden;
      fold.setAttribute('aria-expanded', String(!grid.hidden));
      if (grid.hidden) unfolded.delete(key); else unfolded.add(key);
    } }, h('i', { html: icon('chev') }), h('span', {}, `${gone.length} missing`), h('small', {}, 'folder gone, conversations kept'));
    return [...out, fold, grid];
  };
  if (f.sort !== 'kind') { box.replaceChildren(...section('flat', rows)); return; }
  const out = [];
  for (const k of ORDER) {
    const g = rows.filter((x) => x.kind === k);
    if (g.length) out.push(h('div.gl.ac-g', {}, `${KIND_NAMES[k]} · ${g.length}`), ...section(k, g));
  }
  box.replaceChildren(...out);
}

function save() { store.set('appsf', f); }

async function load() {
  const my = ++loadSeq;
  if (!data) box.replaceChildren(skeleton(4));
  const m = await ask({ op: 'list' });
  if (my !== loadSeq || sheetKind !== 'apps') return;
  if (!m.ok) { info.textContent = ''; box.replaceChildren(failRow(m.error || 'The app list is not available', load, 'The Apps page')); return; }
  const same = data && JSON.stringify(data.apps) === JSON.stringify(m.apps) && !!data.indexing === !!m.indexing;
  data = m;
  if (!same) { paintChips(); paint(); }
  if (m.indexing) setTimeout(() => my === loadSeq && sheetKind === 'apps' && load(), 1500);
}

export function appsSheet() {
  const q = h('input', { type: 'search', placeholder: 'Search apps: name, description, path, tags', value: f.text, enterkeyhint: 'search', autocapitalize: 'off', spellcheck: 'false', 'aria-label': 'Search apps' });
  q.addEventListener('input', debounce(() => { f.text = q.value.trim(); save(); paint(); }, 150));
  const sort = h('select', { 'aria-label': 'Sort apps' }, SORTS.map(([v, l]) => h('option', { value: v, selected: f.sort === v }, l)));
  sort.onchange = () => { f.sort = sort.value; save(); paint(); };
  chips = h('div.chips.fchips.ac-chips', { role: 'group', 'aria-label': 'Kind' });
  hscroll(chips);
  info = h('div.mnote.ac-info');
  box = h('div.ac-box');
  openSheet({ kind: 'apps', tall: true, wide: true },
    h('div.sh-title', {}, h('span', {}, 'Apps'),
      h('button.btn.sm', { type: 'button', id: 'b-apps-refresh', title: 'Ask an agent to survey your projects and update the catalog', onclick: refreshCatalog }, 'Refresh catalog'),
      h('button.ib', { 'aria-label': 'Close', title: 'Close', onclick: closeSheet, html: icon('x') })),
    h('div.ac-bar', {}, q, h('label.ac-sort', {}, h('span', {}, 'Sort'), sort)),
    chips, info, box);
  if (data) { paintChips(); paint(); }
  load();
}
