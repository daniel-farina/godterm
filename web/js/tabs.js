// The tab list: grouped (by account, agent, folder or status, or not at
// all), pinned first, with a filter box, state chips and a sort.
import { $, h, esc, on, emit, store, icon, isDesk, pctClass, debounce, hscroll, tcOf, tcStyle } from './util.js';
import { S } from './state.js';
import { panes } from './term.js';
import { findText, foundHtml, foundClick, onFound } from './find.js';

const CHIPS = [
  ['all', 'All'], ['busy', 'Working'], ['wait', 'Needs you'], ['ready', 'Ready'], ['pinned', 'Pinned'],
];
const BUSY = new Set(['working', 'background', 'busy']);
const WAIT = new Set(['approval', 'waiting', 'blocked', 'confirm']);
let chip = store.get('chip', 'all');
let sort = store.get('tsort', 'pos');
let grp = store.get('tgroup', 'account');
let q = '';
let tabActions = () => {};
export const setTabActions = (fn) => (tabActions = fn);

const posOf = (t) => parseInt(String(t.position || '').replace(/^tab /, ''), 10) || 0;

function fuzzy(hay, needle) {
  // Every word of the needle appears, in any order; or the letters in order.
  hay = hay.toLowerCase();
  const words = needle.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.every((w) => hay.includes(w))) return true;
  let i = 0;
  const n = needle.toLowerCase().replace(/\s+/g, '');
  for (const c of hay) if (c === n[i]) i++;
  return i === n.length && n.length > 2;
}

function keep(t) {
  if (chip === 'busy' && !(BUSY.has(t.state) || t.busy)) return false;
  if (chip === 'wait' && !(WAIT.has(t.state) || WAIT.has(t.label))) return false;
  if (chip === 'ready' && t.state !== 'ready') return false;
  if (chip === 'pinned' && !t.pinned) return false;
  if (q && !fuzzy([t.name, t.folder, t.account_label, t.id, t.group, t.model, t.label].filter(Boolean).join(' '), q)) return false;
  return true;
}

const ORDER = { approval: 0, waiting: 1, working: 2, background: 3, ready: 4 };
function cmp(a, b) {
  if (!!b.pinned - !!a.pinned) return !!b.pinned - !!a.pinned;
  if (sort === 'name') return String(a.name).localeCompare(b.name);
  if (sort === 'folder') return String(a.folder).localeCompare(b.folder);
  if (sort === 'state') return (ORDER[a.state] ?? 9) - (ORDER[b.state] ?? 9);
  return posOf(a) - posOf(b);
}

// The groups the list shows, each with its tabs in the sort's order.
const HARNESS = { claude: 'Claude', grok: 'Grok', codex: 'Codex', opencode: 'OpenCode', gemini: 'Gemini' };
const acctOf = (t) => S.accounts.find((a) => a.account === t.account);
const harnessOf = (t) => t.harness || acctOf(t)?.harness || 'claude';
const STATUS = ['Needs you', 'Working', 'Ready', 'Other'];
const statusOf = (t) => WAIT.has(t.state) || WAIT.has(t.label) ? 0 : BUSY.has(t.state) || t.busy ? 1 : t.state === 'ready' ? 2 : 3;
function groups(tabs) {
  if (grp === 'none') return [{ label: '', tabs: [...tabs].sort(cmp) }];
  const by = new Map();
  for (const t of tabs) {
    let k = t.account_label || 'Other', label = k, rank = 0;
    if (grp === 'harness') { k = harnessOf(t); label = HARNESS[k] || k; }
    else if (grp === 'folder') k = label = t.folder || 'No folder';
    else if (grp === 'state') { rank = statusOf(t); k = label = STATUS[rank]; }
    if (!by.has(k)) by.set(k, { label, rank, acct: t.account, tabs: [] });
    by.get(k).tabs.push(t);
  }
  const out = [...by.values()];
  if (grp === 'state') out.sort((a, b) => a.rank - b.rank);
  if (grp === 'folder') out.sort((a, b) => a.label.localeCompare(b.label));
  for (const g of out) g.tabs.sort(cmp);
  return out;
}

function sub(t) {
  const bits = [];
  if (t.label && t.label !== t.state) bits.push(t.label);
  else if (t.state && t.state !== 'ready') bits.push(t.state);
  if (t.waiting_for) bits.push(t.waiting_for);
  if (t.folder && grp !== 'folder') bits.push(t.folder);
  if (grp === 'harness' && t.account_label) bits.push(t.account_label);
  return bits.join(' · ');
}

let lastHtml = '', dirty = true;
// The list is drawn only while it can be seen (the drawer open, or the
// desktop side list): state comes up to ten times a second.
const seen = () => isDesk() ? !$('#app').classList.contains('noside') : $('#side').classList.contains('open');
function render() {
  if (!seen()) { dirty = true; return; }
  dirty = false;
  const list = $('#list');
  const shown = new Set(panes.visible().map((p) => p.id));
  let html = '';
  const open = S.tabs.filter(keep);
  // While filtering, the open tabs are the first of three sections.
  if (q) html += `<div class="acct fhead"><span class="an">Open tabs</span><span>${open.length}</span></div>`;
  for (const g of groups(open)) {
    if (grp === 'account') {
      const a = S.accounts.find((x) => x.account === g.acct);
      const left = a?.effective_left_pct ?? a?.five_hour_left_pct;
      html += `<div class="acct"><span class="an">${esc(g.label)}${a?.harness && a.harness !== 'claude' ? ' · ' + esc(a.harness) : ''}</span>`;
      if (left != null) html += `<span class="meter" title="${esc(left)}% left"><i class="${pctClass(left)}" style="width:${Math.max(0, Math.min(100, left))}%"></i></span>`;
      html += `<span>${g.tabs.length}</span></div>`;
    } else if (grp !== 'none') {
      html += `<div class="acct"><span class="an" title="${esc(g.label)}">${esc(g.label)}</span><span>${g.tabs.length}</span></div>`;
    }
    for (const t of g.tabs) {
      html += `<div class="row${shown.has(t.id) ? ' on' : ''}${tcOf(t) ? ' tc" style="' + tcStyle(t) : ''}" role="listitem" tabindex="0" aria-current="${shown.has(t.id)}" data-id="${esc(t.id)}" draggable="${isDesk()}">` +
        `<span class="dot ${esc(t.state)}"></span><span class="tx"><span class="n">${esc(t.name)}${t.pinned ? ' <span class="pin">pinned</span>' : ''}${t.git_warn ? ` <span class="gitw" style="color:var(--warn)" title="Git: ${esc(t.git_warn)}">⚠</span>` : ''}</span>` +
        `<span class="s">${esc(sub(t))}</span></span><button class="ib" data-act="1" aria-label="Tab actions">${icon('dots')}</button></div>`;
    }
  }
  if (q && !open.length) html += '<div class="empty fnone">No open tab matches</div>';
  if (!html) html = `<div class="empty">${S.tabs.length ? 'No tab matches' : S.version ? 'No tabs yet: New tab opens one' : 'Connecting to GodTerm...'}</div>`;
  html += foundHtml();
  if (html === lastHtml) return;
  lastHtml = html;
  list.innerHTML = html;
}
export const renderTabs = () => { if (dirty || seen()) render(); };

// Every tab in the drawer's order (its grouping, then the sort), for
// switching to the next or previous one with a swipe.
export const tabOrder = () => groups(S.tabs).flatMap((g) => g.tabs).map((t) => t.id);

export function initTabs() {
  const chips = $('#chips');
  for (const [k, l] of CHIPS) chips.append(h('button.chip' + (k === chip ? '.on' : ''), { 'data-k': k, onclick: () => {
    chip = k; store.set('chip', k);
    for (const c of chips.children) c.classList.toggle('on', c.dataset.k === k);
    render();
  } }, l));
  hscroll(chips);
  $('#tsort').value = sort;
  $('#tsort').onchange = (e) => { sort = e.target.value; store.set('tsort', sort); render(); };
  if (![...$('#tgroup').options].some((o) => o.value === grp)) grp = 'account';
  $('#tgroup').value = grp;
  $('#tgroup').onchange = (e) => { grp = e.target.value; store.set('tgroup', grp); render(); };
  $('#q').addEventListener('input', debounce((e) => { q = e.target.value.trim(); render(); }, 80));
  $('#q').addEventListener('input', (e) => findText(e.target.value.trim()));
  onFound(render);
  const list = $('#list');
  list.addEventListener('click', (e) => {
    if (foundClick(e)) return;
    const row = e.target.closest('.row');
    if (!row) return;
    const b = e.target.closest('[data-act]');
    if (b) { const r = b.getBoundingClientRect(); return tabActions(row.dataset.id, { x: r.left, y: r.bottom + 4 }); }
    panes.show(row.dataset.id);
    emit('picked', row.dataset.id);
  });
  list.addEventListener('keydown', (e) => {
    const row = e.target.closest('.row');
    if (row && e.target === row && (e.key === 'Enter' || e.key === ' ')) { e.preventDefault(); row.click(); }
  });
  list.addEventListener('contextmenu', (e) => {
    const row = e.target.closest('.row:not(.frow)');
    if (row) { e.preventDefault(); tabActions(row.dataset.id, { x: e.clientX, y: e.clientY }); }
  });
  list.addEventListener('dragstart', (e) => {
    const row = e.target.closest('.row');
    if (row) e.dataTransfer.setData('text/gt-tab', row.dataset.id);
  });
  on('state', render);
  on('focus', render);
}
