// The tab filter's other sections: archived tabs (list_archived) and every
// other session on the Mac (the server's session index, through the
// `sessions` tool), below the open tabs that match. Queried 150 ms after
// the last key, each section capped with a "show more".
import { esc, toast, debounce, idOf, emit } from './util.js';
import { tool } from './conn.js';
import { S } from './state.js';
import { panes } from './term.js';
import { run } from './manage.js';

const FIRST = { arch: 5, other: 8 };
const MORE = 20;
const HARNESS = { claude: 'Claude', grok: 'Grok', codex: 'Codex', cursor: 'Cursor', antigravity: 'Antigravity', opencode: 'OpenCode', gemini: 'Gemini' };
let q = '', seq = 0, redraw = () => {};
let lim = { ...FIRST };
const res = { arch: null, other: null };

export const onFound = (fn) => (redraw = fn);

async function query() {
  const my = ++seq, text = q;
  if (!text) { res.arch = res.other = null; redraw(); return; }
  const [a, o] = await Promise.all([
    tool('list_archived', { text, limit: lim.arch }),
    // A few more than shown: those open in a tab or archived drop out.
    tool('sessions', { text, limit: lim.other + 10, include_headless: false }),
  ]);
  if (my !== seq) return;
  const av = a.result || a;
  res.arch = a.ok === false && !av.archived ? { rows: [], total: 0, err: a.error } : { rows: av.archived || [], total: av.total ?? (av.archived || []).length };
  const ov = o.result || o;
  if (o.ok === false && !ov.sessions) res.other = { rows: [], total: 0, err: o.error || 'Sessions are not available' };
  else {
    // Sessions already listed above (open in a tab, or archived) are left out.
    const known = new Set([...S.tabs.map((t) => t.session_id || t.session).filter(Boolean),
      ...(res.arch.rows || []).map((r) => r.session).filter(Boolean)]);
    const all = ov.sessions || [];
    const rows = all.filter((s) => !s.open_in_tab && !known.has(s.id));
    res.other = { rows: rows.slice(0, lim.other), total: Math.max(0, (ov.matched ?? all.length) - (all.length - rows.length)), indexing: ov.indexing };
    if (ov.indexing) setTimeout(() => my === seq && query(), 1500);
  }
  redraw();
}
const later = debounce(query, 150);

// The filter text changed: reset the caps and ask again shortly.
export function findText(text) {
  if (text === q) return;
  q = text;
  lim = { ...FIRST };
  if (!q) { seq++; res.arch = res.other = null; return; }
  later();
}

const head = (label, n, wait) => `<div class="acct fhead"><span class="an">${esc(label)}</span><span>${wait ? '...' : esc(n)}</span></div>`;
const more = (k, left) => left > 0 ? `<button class="li fmore" data-more="${k}">Show more (${left})</button>` : '';

function archRow(a) {
  const sub = [a.account_label, a.last_active && 'active ' + a.last_active, a.folder].filter(Boolean).join(' · ');
  return `<div class="row frow" role="listitem" tabindex="0" data-arch="${esc(a.id)}">` +
    `<span class="dot"></span><span class="tx"><span class="n"><span class="ft">${esc(a.name || 'Archived tab')}</span><span class="tagl">Archived</span></span>` +
    `<span class="s">${esc(sub)}</span></span><span class="fgo">Restore</span></div>`;
}

function otherRow(s, i) {
  const acct = typeof s.source === 'number' ? S.accounts.find((a) => a.account === s.source)?.label || 'Account ' + s.source : s.source;
  const title = s.title || s.first_prompt || s.id;
  const sub = [acct, s.last_activity, s.project].filter(Boolean).join(' · ');
  return `<div class="row frow" role="listitem" tabindex="0" data-sess="${i}">` +
    `<span class="dot${s.live ? ' working' : ''}"></span><span class="tx"><span class="n"><span class="ft">${esc(title)}</span><span class="tagl">${esc(HARNESS[s.harness] || s.harness || '')}</span></span>` +
    `<span class="s">${esc(sub)}</span></span><span class="fgo">Resume</span></div>`;
}

// The sections' HTML, for the tab list to append (empty without a filter).
export function foundHtml() {
  if (!q) return '';
  let html = '';
  const a = res.arch, o = res.other;
  html += head('Archived', a?.total ?? 0, !a);
  if (a) html += a.rows.map(archRow).join('') + more('arch', a.total - a.rows.length);
  html += head('Other sessions', o?.total ?? 0, !o);
  if (o) {
    if (o.err) html += `<div class="empty">${esc(o.err)}</div>`;
    else if (!o.rows.length) html += `<div class="empty">${o.indexing ? 'Still indexing sessions...' : 'No other session matches'}</div>`;
    else html += o.rows.map(otherRow).join('') + more('other', o.total - o.rows.length);
  }
  return html;
}

const showTab = (id) => { if (!id) return; setTimeout(() => { panes.show(idOf(id)); emit('picked', idOf(id)); }, 300); };

// A click in the tab list on one of these rows; true when it was one.
export function foundClick(e) {
  const m = e.target.closest('[data-more]');
  if (m) { lim[m.dataset.more] += MORE; query(); return true; }
  const row = e.target.closest('.frow');
  if (!row) return false;
  if (row.dataset.arch) {
    const a = res.arch?.rows.find((x) => x.id === row.dataset.arch);
    toast('Restoring ' + (a?.name || 'tab') + '...', 'info');
    run('restore_archived', { id: row.dataset.arch }, 'Restored').then((r) => { showTab(r?.result?.tabs?.[0]?.tab); later(); });
  } else {
    const s = res.other?.rows[+row.dataset.sess];
    if (!s) return true;
    if (s.open_in_tab) { showTab(s.open_in_tab); return true; }
    toast('Opening ' + (s.title || 'session') + '...', 'info');
    run(s.external ? 'take_over_session' : 'open_session', { id: s.id }, 'Opened').then((r) => showTab(r?.result?.tab || r?.result?.tab_id));
  }
  return true;
}
