// Management through `tool` frames: tab options, new tab, usage, jobs,
// loops, modes. A tool that answers needs_confirmation gets its question
// shown with a Yes; the yes calls it again with only confirm_token.
import { $, h, on, emit, toast, store, pctClass, idOf, icon, isDesk, li, group, SWATCHES } from './util.js';
import { tool } from './conn.js';
import { S, tabById } from './state.js';
import { openSheet, openMenu, closeSheet, sheetKind } from './sheet.js';
import { panes } from './term.js';
import { acctOf } from './state.js';
import { ownQuestion } from './chat.js';

const say = (r, fallback) => r?.result?.say || r?.say || fallback;

export async function run(name, args, done = 'Done', extra) {
  const r = await tool(name, args, 60000, extra);
  const conf = r.needs_confirmation ? r : r.result?.needs_confirmation ? r.result : null;
  if (conf) { if (conf.token) ownQuestion(conf.token); return confirmSheet(name, conf); }
  if (r.ok === false || r.error) { toast(r.error || r.result?.error || 'It did not work', 'error'); return r; }
  toast(say(r, done), 'ok');
  return r;
}

function confirmSheet(name, c) {
  const q = typeof c.needs_confirmation === 'string' ? c.needs_confirmation : c.question || 'Go ahead?';
  return new Promise((resolve) => {
    const yes = async () => {
      closeSheet();
      // With a token the yes is the token alone; without one, confirm: true.
      // The tap on Yes is the user's own answer (user_says), as typing it would be.
      resolve(await run(name, c.token ? { confirm_token: c.token } : { ...(c.args || {}), confirm: true }, 'Done', { user_says: 'yes' }));
    };
    openSheet({ kind: 'confirm' }, h('div.sh-title', {}, h('span', {}, 'Confirm')), h('div.appr-q', {}, q),
      h('div.appr-btns', {}, h('button.btn.pri', { onclick: yes }, 'Yes'), h('button.btn', { onclick: async () => {
        closeSheet();
        // A no with the token drops the question in GodTerm too.
        if (c.token) await tool(name, { confirm_token: c.token }, 60000, { user_says: 'no' });
        resolve(null);
      } }, 'No')));
  });
}

const field = (label, el) => h('label.field', {}, label, el);
const title = (t, extra) => h('div.sh-title', {}, h('span', {}, t), extra, h('button.ib', { 'aria-label': 'Close', onclick: closeSheet, html: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>' }));

function acctSelect(cur, filter) {
  const s = h('select');
  for (const a of S.accounts) {
    if (filter && !filter(a)) continue;
    const left = a.effective_left_pct ?? a.five_hour_left_pct;
    s.append(h('option', { value: a.account, disabled: a.available === false || a.logged_in === false, selected: a.account === cur },
      `${a.label}${a.harness ? ' (' + a.harness + ')' : ''}${left != null ? ' · ' + left + '% left' : ''}`));
  }
  return s;
}
const harnessOf = (n) => S.accounts.find((a) => a.account === n)?.harness || 'claude';

export { li, group };
const short = (p, n = 26) => (!p ? '' : p.length > n ? '…' + p.slice(-(n - 1)) : p);

// Where the tab menu is shown: the docked panel (a desktop that has it
// open), a popover at the click (a desktop), the bottom sheet (a phone).
// Submenus replace it in place: a drill-down in the panel.
let host = { pop: false, panel: false, at: null };
const PANEL = { pop: true, panel: true, at: null };
function show(...kids) {
  if (host.panel) paintPanel(kids, liveNow);
  else if (host.pop) openMenu(host.at, h('div.mbody.slide', {}, kids));
  else openSheet({ kind: 'tab' }, h('div.mbody.slide', {}, kids));
}
// Done in a submenu: the panel goes back to the tab's options, a menu or
// a sheet closes.
function leave() {
  if (host.panel) panelPaint('root');
  else closeSheet();
}
function sub(t, name, ...kids) {
  show(h('div.subhead', {}, h('button.ib.sm', { 'aria-label': 'Back', onclick: () => go('root', t), html: icon('back') }), h('span', {}, name)), ...kids);
}

// The sections of a tab's options, by name (the status bar opens them).
const SECTIONS = {
  root: (t) => menu(t.id),
  rename: (t) => renameSheet(t),
  color: (t) => colorSheet(t),
  group: (t) => groupSheet(t),
  move: (t) => moveSheet(t, 'move'),
  dup: (t) => moveSheet(t, 'dup'),
  copy: (t) => moveSheet(t, 'copy'),
  model: (t) => modelSheet(t),
  usage: (t) => usageSheet(t),
  loops: (t) => jobsSheet(t),
  browser: (t) => browserSheet(t),
  git: (t) => {
    const panel = host.panel;
    // (Mounted after the import: the panel is named, not the host then.)
    const inPanel = (fn) => () => { const prev = host; host = PANEL; try { fn(); } finally { host = prev; } };
    const place = panel ? { mount: (body) => inPanel(() => sub(t, 'Git', body))(), back: inPanel(() => panelPaint('root')) } : null;
    return import('./git.js').then((m) => m.gitSheet(t, place));
  },
};
// Painted again as the state changes (the others fetch, or hold typing).
const LIVE = new Set(['root', 'usage', 'model', 'loops', 'color', 'group', 'move', 'dup', 'copy']);
let view = 'root';
function go(key, t) {
  if (host.panel) view = key;
  (SECTIONS[key] || SECTIONS.root)(t);
}

export function tabActions(id, at = null) {
  const desk = isDesk();
  if (desk && !at) { const r = $('#title').getBoundingClientRect(); at = { x: r.left, y: r.bottom + 4 }; }
  host = { pop: desk, panel: false, at };
  menu(id);
}

// A status bar item: its section in the docked panel (a desktop opens
// the panel for it), else a popover above the item.
export function tabSection(id, key, r) {
  if (isDesk()) { setPanel(true); panelPaint(key); return; }
  const t = tabById(id);
  if (!t) return;
  host = { pop: true, panel: false, at: r ? { x: r.left, y: r.top - 6, up: true } : null };
  go(key, t);
}

// ---- the docked panel (desktop) ----
// The shown tab's options as a right hand panel instead of a dropdown:
// the Tab button toggles it (kept per browser), it follows the tab shown,
// and the terminals refit once when it opens or closes.
let panelOn = false, panelTab = '', panelSig = '';
export const panelIsOn = () => panelOn;
export function setPanel(on, save = true) {
  on = !!on;
  if (save) store.set('tabpanel', on);
  const was = panelOn;
  panelOn = on;
  const el = $('#tabpanel');
  el.hidden = !on;
  $('#b-tabmenu')?.setAttribute('aria-pressed', String(on));
  if (on && !was) { view = 'root'; panelPaint('root'); }
  if (!on) { $('#tp-body').replaceChildren(); panelTab = ''; }
  if (on !== was) emit('layout');
}
function paintPanel(kids, live) {
  const b = $('#tp-body');
  const top = b.scrollTop;
  b.replaceChildren(h('div.mbody' + (live ? '' : '.slide'), {}, kids));
  b.scrollTop = live ? top : 0;
}
// What a live section shows, to paint again only when it changed.
function sigOf(t) {
  if (!t) return '';
  const a = acctOf(t.account);
  return JSON.stringify([view, t, a, (S.loops || []).filter((l) => l.tab === t.id), (S.jobs?.jobs || []).filter((j) => j.tab === t.name), view === 'move' || view === 'dup' || view === 'copy' || view === 'usage' ? S.accounts : 0]);
}
function panelPaint(key = view, live = false) {
  if (!panelOn) return;
  const t = tabById(panes.focused()?.id);
  const prev = host;
  host = PANEL;
  try {
    panelTab = t?.id || '';
    if (!t) { view = 'root'; paintPanel([h('div.mnote', {}, 'No tab is shown.')], live); panelSig = ''; return; }
    view = SECTIONS[key] ? key : 'root';
    panelSig = sigOf(t);
    liveNow = live;
    SECTIONS[view](t);
  } finally { host = prev; liveNow = false; }
}
let liveNow = false;
{
  const el = $('#tabpanel');
  // A click or typing in the panel answers there (not in a popover).
  for (const ev of ['pointerdown', 'focusin', 'keydown']) el.addEventListener(ev, () => { host = PANEL; }, true);
  let down = false;
  el.addEventListener('pointerdown', () => { down = true; }, true);
  addEventListener('pointerup', () => { down = false; }, true);
  const typing = () => { const a = document.activeElement; return el.contains(a) && /^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName); };
  on('state', () => {
    if (!panelOn || down || typing() || !isDesk()) return;
    const t = tabById(panes.focused()?.id);
    if ((t?.id || '') !== panelTab) { panelPaint(view === 'rename' ? 'root' : view); return; }
    if (!LIVE.has(view) || sigOf(t) === panelSig) return;
    panelPaint(view, true);
  });
  on('focus', () => {
    if (!panelOn) return;
    const id = panes.focused()?.id || '';
    if (id !== panelTab) panelPaint(view === 'rename' ? 'root' : view);
  });
}

function menu(id) {
  const t = tabById(id);
  if (!t) return;
  const hs = harnessOf(t.account);
  const others = S.accounts.filter((a) => a.account !== t.account && a.logged_in !== false && (a.harness || 'claude') === hs);
  const noTarget = others.length ? '' : `no other ${hs} account`;
  const working = t.state === 'working' || t.busy;
  const act = (fn) => async () => { leave(); await fn(); };
  const copy = (text, said) => act(async () => { try { await navigator.clipboard.writeText(text); toast(said, 'ok'); } catch { toast(text, 'info'); } });
  const rename = () => go('rename', t);
  const pin = act(() => run(t.pinned ? 'unpin_tab' : 'pin_tab', { tab: id }, t.pinned ? 'Unpinned' : 'Pinned'));
  const stop = act(() => run('press_key', { tab: id, key: 'escape' }, 'Interrupted'));
  const move = () => go('move', t);
  const head = h('div.mhead', {}, h('div.mt', {}, h('span.dot.' + (t.state || '')), h('span', {}, t.name),
    host.panel ? h('button.ib.sm.tp-x', { 'aria-label': 'Close the panel', title: 'Close the panel (the Tab button opens it again)', onclick: () => setPanel(false), html: icon('x') }) : null),
    h('div.mm', {}, [t.account_label, short(t.folder, 30), t.model, t.state].filter(Boolean).join(' · ')));
  const quick = host.pop ? null : h('div.quick', {},
    h('button', { type: 'button', onclick: rename, html: icon('edit') }, 'Rename'),
    h('button', { type: 'button', onclick: pin, html: icon('pin') }, t.pinned ? 'Unpin' : 'Pin'),
    h('button', { type: 'button', onclick: stop, disabled: !working, title: working ? 'Send Esc: stop its current work' : 'not working', html: icon('stop') }, 'Interrupt'),
    h('button', { type: 'button', onclick: move, disabled: !!noTarget, title: noTarget, html: icon('move') }, 'Move'));
  const keys = h('div.keys', {}, h('span.gk', {}, 'Send keys'),
    h('button', { type: 'button', onclick: stop, title: 'Esc: stop its current work' }, 'Esc'),
    h('button', { type: 'button', onclick: act(() => run('press_key', { tab: id, key: 'ctrl_c' }, 'Sent Ctrl-C')), title: 'Ctrl-C' }, 'Ctrl-C'));
  show(head, quick,
    ...group(host.pop ? '' : null, host.pop ? [li('edit', 'Rename', { fn: rename }), li('pin', t.pinned ? 'Unpin' : 'Pin', { fn: pin, tip: 'Pinned: first in the list, asks before closing' })] : []),
    ...group('Conversation', [
      host.pop ? li('move', 'Move to', { fn: move, off: noTarget, chev: true, tip: 'Continue the conversation on another account' }) : null,
      li('dup', 'Duplicate to', { fn: () => go('dup', t), off: noTarget, chev: true, tip: 'Open it on another account too; this tab stays' }),
      li('copyto', 'Copy session to', { fn: () => go('copy', t), off: noTarget || (t.session_id ? '' : 'no conversation yet'), chev: true, tip: 'Copy the conversation to another account, no tab opened' }),
      li('model', 'Switch model', { fn: () => go('model', t), hint: [t.model, t.effort].filter(Boolean).join(' · '), chev: true }),
      li('usage', 'Usage', { fn: () => go('usage', t), hint: usageHint(t), chev: true, tip: 'This account\'s usage, and where to move the tab' }),
      li('reopen', 'Reopen', { fn: act(() => run('reopen_tab', { id }, 'Reopening')), tip: 'Start the tab again, resuming its conversation' }),
    ]),
    ...group('Tab', [
      li('color', 'Accent color', { fn: () => go('color', t), hint: t.color && t.color !== 'default' ? t.color : '', chev: true }),
      li('group', 'Group', { fn: () => go('group', t), hint: t.group || '', chev: true }),
      li('clock', 'Loops and jobs', { fn: () => go('loops', t), hint: t.loops ? t.loops + ' loops' : '', chev: true }),
      li('hash', 'Git', { fn: () => go('git', t), hint: t.git_warn ? '⚠ ' + t.git_warn : '', chev: true, tip: 'Status, commit, push and pull, branches, the GitHub repo; nothing changes without your yes' }),
      li('globe', 'Browser', { fn: () => go('browser', t), hint: t.browser ? describeBrowser(t.browser) : '', chev: true, tip: 'BrowserSkill status for agents' }),
      li('mic', 'Talk directly', { fn: act(() => run('talk_to_tab', { tab: id })), tip: 'Your voice goes to this tab and its replies are said' }),
    ]),
    ...group('Copy', [
      li('dup', 'Folder path', { fn: copy(t.folder || '', 'Copied ' + (t.folder || '')), hint: short(t.folder, 22) }),
      t.session_id ? li('hash', 'Session id', { fn: copy(t.session_id, 'Copied the session id'), hint: t.session_id.slice(0, 8) }) : null,
      li('ext', 'Open folder', { fn: act(() => run('open_path', { paths: [t.folder], tab: id }, 'Opened')), tip: 'Show the folder on the Mac' }),
    ]),
    keys,
    ...group('', [
      li('archive', 'Archive', { fn: () => archive(t), bad: true, tip: 'Stop its session and keep it under Archived' }),
      li('x', 'Close tab', { fn: act(() => run('close_tabs', { tab: id }, 'Closed')), bad: true, tip: 'Asks first when busy or pinned' }),
    ]));
}

// Archive; a working tab is archived only after a second yes (force).
async function archive(t) {
  leave();
  const r = await tool('archive_tabs', { tab: t.id });
  const err = r.error || r.result?.error || '';
  if (r.ok !== false && !err) { toast(say(r, 'Archived'), 'ok'); return; }
  if (!/force/.test(err)) { toast(err || 'It did not work', 'error'); return; }
  openSheet({ kind: 'confirm' }, h('div.sh-title', {}, h('span', {}, 'Archive ' + t.name + '?')),
    h('div.appr-q', {}, 'It is working: archiving stops what it is doing.'),
    h('div.appr-btns', {}, h('button.btn.bad', { onclick: async () => { closeSheet(); await run('archive_tabs', { tab: t.id, force: true }, 'Archived'); } }, 'Archive anyway'),
      h('button.btn', { onclick: closeSheet }, 'Cancel')));
}

function describeBrowser(b) {
  if (typeof b === 'string') return b;
  return [b.label, b.profile, b.state].filter(Boolean).join(' · ') || 'bound';
}

function renameSheet(t) {
  const inp = h('input', { type: 'text', value: t.name, enterkeyhint: 'done' });
  const go = async () => { leave(); await run('rename_tab', { tab: t.id, name: inp.value.trim() }, 'Renamed'); };
  inp.addEventListener('keydown', (e) => e.key === 'Enter' && go());
  sub(t, 'Rename', inp, h('div.acts', {}, h('button.btn.pri', { onclick: go }, 'Rename')));
  if (!liveNow) setTimeout(() => inp.select(), 50);
}

function moveSheet(t, kind) {
  const h0 = harnessOf(t.account);
  const verbs = { move: ['Move to', 'Moved'], dup: ['Duplicate to', 'Duplicated'], copy: ['Copy session to', 'Copied'] };
  const go = (to) => async () => {
    leave();
    if (kind === 'copy') await run('copy_session', { session: t.session_id, account: to }, verbs[kind][1]);
    else await run('move_tab', { tab: t.id, to, copy: kind === 'dup' || undefined }, verbs[kind][1]);
  };
  const rows = S.accounts.filter((a) => a.account !== t.account && (a.harness || 'claude') === h0).map((a) => {
    const left = a.effective_left_pct ?? a.five_hour_left_pct;
    return li('move', a.label, { fn: go(a.account), hint: left != null ? left + '% left' : '', off: a.available === false || a.logged_in === false ? 'not available' : '' });
  });
  if (!rows.length) { if (host.panel) sub(t, verbs[kind][0], h('div.mnote', {}, 'No other ' + h0 + ' account to use.')); else toast('No other ' + h0 + ' account to use', 'warn'); return; }
  sub(t, verbs[kind][0], h('div.mnote', {}, `Only ${h0} accounts can continue a ${h0} conversation.`), ...group('', rows));
}

// The swatches the terminal UI offers (its own, over its group's), then None.
const COLORS = [...SWATCHES, 'default'];
function colorSheet(t) {
  sub(t, 'Accent color', ...group('', COLORS.map((c) => {
    const row = li('color', c === 'default' ? 'None' : c[0].toUpperCase() + c.slice(1), { fn: async () => { leave(); await run('set_tab_color', { tab: t.id, color: c }, 'Color set'); }, hint: (t.color || 'default') === c ? 'now' : '' });
    row.firstChild.style.color = c === 'default' ? 'var(--line)' : `var(--sw-${c})`;
    return row;
  })));
}

function groupSheet(t) {
  const groups = [...new Set(S.tabs.filter((x) => x.account === t.account && x.group).map((x) => x.group))];
  const inp = h('input', { type: 'text', placeholder: 'New group name', value: '' });
  const add = async () => { if (!inp.value.trim()) return; leave(); await run('group_tabs', { tab: t.id, group: inp.value.trim() }, 'Grouped'); };
  inp.addEventListener('keydown', (e) => e.key === 'Enter' && add());
  sub(t, 'Group', ...group('', [
    ...groups.map((g) => li('group', g, { fn: async () => { leave(); await run('group_tabs', { tab: t.id, group: g }, 'Grouped'); }, hint: g === t.group ? 'now' : '' })),
    t.group ? li('x', 'Dissolve ' + t.group, { fn: async () => { leave(); await run('ungroup', { group: t.group, account: t.account }, 'Ungrouped'); } }) : null,
  ]), h('div.inrow', {}, inp, h('button.btn.sm.pri', { onclick: add }, 'Add')));
}

function modelSheet(t) {
  const hs = harnessOf(t.account);
  const acct = S.accounts.find((a) => a.account === t.account);
  // The tab's model bar as GodTerm has it (Claude: Haiku, Sonnet, Opus,
  // Fable; Grok's three), else the CLI picks its model itself.
  const models = Array.isArray(acct?.models) ? acct.models
    : (hs === 'grok' ? ['grok-4.7-build-fast', 'grok-4.7', 'grok-4.6'] : hs === 'claude' ? ['haiku', 'sonnet', 'opus', 'fable'] : []).map((m) => ({ arg: m, label: m }));
  if (!models.length) {
    sub(t, 'Switch model', h('div.mnote', {}, `${t.account_label || hs} runs ${hs}: its model is chosen inside the CLI (its own /model), not by GodTerm.`));
    return;
  }
  const inp = h('input', { type: 'text', placeholder: 'Or type a model name' });
  const set = async (m) => { leave(); await run('set_tab_model', { tab: t.id, model: m }, 'Model set'); };
  inp.addEventListener('keydown', (e) => e.key === 'Enter' && inp.value.trim() && set(inp.value.trim()));
  const now = (m) => !!t.model && [m.label, m.arg].some((x) => x && t.model.toLowerCase().includes(String(x).toLowerCase()));
  sub(t, 'Switch model', ...group('', models.map((m) => li('model', m.label, { fn: () => set(m.arg), hint: now(m) ? 'now' : m.desc || '' }))),
    h('div.inrow', {}, inp, h('button.btn.sm.pri', { onclick: () => inp.value.trim() && set(inp.value.trim()) }, 'Set')),
    t.model_queued ? h('div.mnote', {}, 'Switching to ' + t.model_queued + ' when it is ready.') : null,
    effortRow(t, hs));
}

// Claude Code's effort (its /effort command, sent as a prompt: after the
// turn when busy); other CLIs show theirs, set inside the CLI.
const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'];
function effortRow(t, hs) {
  if (hs !== 'claude') return t.effort ? h('div.mnote', {}, 'Effort ' + t.effort + ' (set inside the CLI)') : null;
  const set = async (e) => { leave(); await run('send_prompt', { tab: t.id, text: '/effort ' + e }, 'Effort ' + e); };
  return h('div.mbody', {}, h('div.gl', {}, 'Effort' + (t.effort ? ' · ' + t.effort + ' now' : '')),
    h('div.effs', {}, EFFORTS.map((e) => h('button.chip' + (t.effort === e ? '.on' : ''), { type: 'button', onclick: () => set(e), title: '/effort ' + e }, e))));
}

const usageHint = (t) => {
  const a = acctOf(t.account);
  const left = a?.effective_left_pct ?? a?.five_hour_left_pct;
  return [a?.label, left != null ? left + '% left' : ''].filter(Boolean).join(' · ');
};
// The tab's account: its usage bars, and the accounts it can move to.
function usageSheet(t) {
  const h0 = harnessOf(t.account);
  const others = S.accounts.filter((a) => a.account !== t.account && (a.harness || 'claude') === h0).map((a) => {
    const left = a.effective_left_pct ?? a.five_hour_left_pct;
    return li('move', a.label, { fn: () => go('move', t), hint: left != null ? left + '% left' : '', off: a.available === false || a.logged_in === false ? 'not available' : '', chev: true });
  });
  sub(t, 'Usage', h('div.list', {}, usageCards((a) => a.account === t.account)),
    ...group('Other ' + h0 + ' accounts', others),
    h('div.list', {}, li('usage', 'All accounts', { fn: () => { closeSheet(); manageSheet(); }, hint: 'Manage', chev: true })));
}

// A card row: a dot, a title with a right hint, and a line under it.
const card = (title, right, line, dot) => h('div.lrow', {},
  h('div.ct', {}, dot ? h('span.dot.' + dot) : null, h('b', {}, title), right ? h('span.cs', {}, right) : null),
  line ? h('div.cs', {}, line) : null);
const ago = (ms) => {
  const m = Math.round((Date.now() - ms) / 60000);
  return m < 1 ? 'just now' : m < 60 ? m + ' min ago' : Math.round(m / 60) + ' h ago';
};

// BrowserSkill for this tab: its Chrome profile, the browsers bsk sees,
// the profiles and their extension, and what the doctor found.
async function browserSheet(t) {
  const body = h('div', {}, h('div.mnote', {}, 'Checking BrowserSkill...'));
  const b = t.browser;
  const mine = b ? card(typeof b === 'string' ? b : b.label || b.profile || 'Chrome profile', typeof b === 'string' ? '' : b.state,
    'Agents in this tab use only this browser.', /ready/i.test(b.state || b) ? 'ready' : 'approval')
    : card('No browser set', '', 'Choose a Chrome profile for this tab in the desktop app: Browser on the tab\'s bottom bar, or Ctrl-a i.', 'approval');
  // The live view of the page the agent drives (bview.js): only while
  // the agent has a session there, or one the user starts on purpose.
  const live = b ? h('div', {}, h('div.mnote', {}, 'Looking for the agent\'s browser session...')) : null;
  sub(t, 'Browser', h('div.gl', {}, 'This tab'), h('div.list', {}, mine), live && h('div.gl', {}, 'Live view'), live, body);
  if (live) import('./bview.js').then((m) => live.replaceChildren(m.liveSlot(t))).catch(() => live.replaceChildren(h('div.mnote', {}, 'The live view did not load.')));
  const r = await tool('browser_status', {});
  const v = r.result || r;
  if (r.ok === false || !v.bsk) { body.replaceChildren(h('div.mnote', {}, v.say || r.error || 'BrowserSkill did not answer.')); return; }
  const d = v.daemon || {};
  const up = d.state === 'running';
  const doc = v.doctor?.problems || [];
  const profiles = v.chrome_profiles || [];
  const bound = typeof b === 'object' ? b.profile : '';
  body.replaceChildren(...[
    ...group('BrowserSkill', [
      card(v.bsk.installed ? 'bsk ' + (v.bsk.version || '') : 'bsk not installed', up ? 'running' : d.state || '',
        up ? [d.sessions != null && d.sessions + ' sessions', d.uptime_secs != null && 'up ' + Math.round(d.uptime_secs / 60) + ' min'].filter(Boolean).join(' · ') : d.why || '',
        v.bsk.installed && up ? 'ready' : 'error'),
    ]),
    ...group('Connected browsers', (v.browsers || []).map((x) => card(x.label || x.browser || x.instance_id,
      [x.browser, x.browser_version].filter(Boolean).join(' '),
      [x.sessions != null && x.sessions + ' sessions', x.connected_at_ms && 'connected ' + ago(x.connected_at_ms), x.extension_version && 'extension ' + x.extension_version,
        x.version_skew && 'version mismatch', x.unresponsive && 'not responding'].filter(Boolean).join(' · '),
      x.unresponsive ? 'error' : x.version_skew ? 'approval' : 'ready'))),
    (v.browsers || []).length ? null : h('div.mnote', {}, 'No browser is connected. Turn on the local connection in the extension popup.'),
    ...group('Chrome profiles', profiles.map((p) => card(p.name || p.profile_dir, (p.profile_dir === bound || p.name === bound) ? 'this tab' : '',
      [p.email, 'extension ' + (p.extension || 'not found')].filter(Boolean).join(' · '), /on|enabled/i.test(p.extension || '') ? 'ready' : 'approval'))),
    ...group('Problems', doc.map((p) => card(p.check, p.status, p.detail, /fail|err/i.test(p.status) ? 'error' : 'approval'))),
    v.checked_secs_ago != null ? h('div.mnote', {}, 'Checked ' + v.checked_secs_ago + ' s ago' + (v.doctor?.summary ? ' · ' + v.doctor.summary : '')) : null].filter(Boolean));
}

// Jobs and loops, optionally only one tab's.
export function jobRows(tabName) {
  const jobs = (S.jobs?.jobs || []).filter((j) => !tabName || j.tab === tabName);
  return jobs.map((j) => {
    const paused = /pause/i.test(j.status || '');
    return h('div.lrow', {}, h('div.ct', {}, h('b', {}, j.name), h('span.cs', {}, j.schedule || '')),
      h('div.cs', {}, [j.status, j.tab && 'tab ' + j.tab, j.next && 'next ' + j.next, j.last && 'last ' + j.last].filter(Boolean).join(' · ')),
      h('div.acts', {}, h('button.btn.sm', { onclick: () => run('run_job_now', { job: j.id }, 'Started') }, 'Run now'),
        h('button.btn.sm', { onclick: () => run('update_job', { job: j.id, action: paused ? 'resume' : 'pause' }, paused ? 'Resumed' : 'Paused') }, paused ? 'Resume' : 'Pause'),
        h('button.btn.sm.bad', { onclick: () => run('delete_job', { job: j.id }, 'Deleted') }, 'Delete')));
  });
}
export function loopRows(tabId) {
  return (S.loops || []).filter((l) => !tabId || l.tab === tabId).map((l) => h('div.lrow', {},
    h('div.ct', {}, h('b', {}, l.cadence || 'loop'), h('span.cs', {}, (tabById(l.tab)?.name || l.tab) + (l.next ? ' · next ' + l.next : ''))),
    h('div.cs', {}, l.prompt || ''),
    h('div.acts', {}, h('button.btn.sm', { onclick: () => run('adopt_loop', { job: l.job }, 'Kept as a GodTerm job') }, 'Keep as job'),
      h('button.btn.sm.bad', { onclick: () => run('stop_loops', { jobs: [l.job] }, 'Stopped') }, 'Stop'))));
}

function jobsSheet(t) {
  const name = h('input', { type: 'text', placeholder: 'Name' });
  const prompt = h('textarea.f', { rows: 3, placeholder: 'Prompt to send' });
  const every = h('input', { type: 'text', placeholder: 'every: 30 minutes, or cron: 0 9 * * 1-5' });
  const create = async () => {
    const e = every.value.trim();
    const args = { name: name.value.trim() || 'job', prompt: prompt.value.trim(), tab: t.id };
    if (/^[\d*\/,-]+(\s+[\d*\/,-]+){4}$/.test(e)) args.cron = e; else if (/^(in |\d{4}-)/.test(e)) args.at = e; else args.every = e || '1 hour';
    if (!args.prompt) return toast('A job needs a prompt', 'warn');
    leave();
    await run('create_job', args, 'Job created');
  };
  const L = (rows, empty) => (rows.length ? h('div.list', {}, rows) : h('div.mnote', { style: 'padding:0 4px' }, empty));
  const lists = [h('div.gl', {}, 'Loops'), L(loopRows(t.id), 'No loops'), h('div.gl', {}, 'GodTerm jobs'), L(jobRows(t.name), 'No jobs'), h('div.gl', {}, 'New job'), name, prompt, every];
  if (host.panel) return sub(t, 'Loops and jobs', ...lists, h('div.acts', {}, h('button.btn.pri', { onclick: create }, 'Create')));
  openSheet({ kind: 'tabjobs', tall: true }, title('Loops and jobs: ' + t.name), ...lists,
    h('div.acts', {}, h('button.btn', { onclick: () => menu(t.id) }, 'Back'), h('button.btn.pri', { onclick: create }, 'Create')));
}

export function newTabSheet() {
  const last = store.get('newtab.acct', null);
  const acct = acctSelect(S.accounts.some((a) => a.account === last) ? last : S.accounts.find((a) => a.available !== false)?.account);
  const dl = h('datalist', { id: 'folders' }, [...new Set(S.tabs.map((t) => t.folder).filter(Boolean))].map((f) => h('option', { value: f })));
  const dir = h('input', { type: 'text', placeholder: '~/project or a recent project name', list: 'folders', autocapitalize: 'off', spellcheck: 'false' });
  const name = h('input', { type: 'text', placeholder: 'Or a task name: makes a new folder' });
  const prompt = h('textarea.f', { rows: 3, placeholder: 'Optional first prompt' });
  // The folders in use, most used first, as one tap choices (a phone's
  // datalist is hard to reach); the last folder opened here comes first.
  const count = new Map();
  for (const t of S.tabs) if (t.folder) count.set(t.folder, (count.get(t.folder) || 0) + 1);
  const lastDir = store.get('newtab.dir', null);
  const recent = [...count.keys()].sort((a, b) => (b === lastDir) - (a === lastDir) || count.get(b) - count.get(a)).slice(0, 8);
  const chips = recent.length ? h('div.chips.dirs', {}, recent.map((f) => h('button.chip', { type: 'button', title: f, onclick: () => {
    dir.value = f;
    for (const c of chips.children) c.classList.toggle('on', c.title === f);
  } }, f.replace(/\/+$/, '').split('/').pop() || f))) : null;
  const go = async () => {
    store.set('newtab.acct', +acct.value);
    if (dir.value.trim()) store.set('newtab.dir', dir.value.trim());
    const args = { account: +acct.value };
    if (dir.value.trim()) args.dir = dir.value.trim();
    if (name.value.trim()) args.name = name.value.trim();
    if (prompt.value.trim()) args.prompt = prompt.value.trim();
    closeSheet();
    toast('Opening a tab...', 'info');
    const r = await run('open_tab', args, 'Tab opened');
    const id = r?.result?.tab || r?.result?.tab_id || r?.result?.tabs?.[0];
    if (id) setTimeout(() => panes.show(idOf(id)), 300);
  };
  openSheet(title('New tab'), field('Account', acct), field('Folder', dir), chips, dl, field('Name', name), field('Prompt', prompt),
    h('div.acts', {}, h('button.btn', { onclick: closeSheet }, 'Cancel'), h('button.btn.pri', { onclick: go }, 'Open tab')));
}

export function usageCards(only) {
  return S.accounts.filter((a) => !only || only(a)).map((a) => {
    const buckets = a.buckets?.length ? a.buckets : [
      a.five_hour_left_pct != null && { bucket: '5 hour', left_pct: a.five_hour_left_pct },
      a.week_left_pct != null && { bucket: 'week', left_pct: a.week_left_pct }].filter(Boolean);
    return h('div.lrow', {}, h('div.ct', {}, h('b', {}, a.label), h('span.cs', {}, [a.harness, `${a.tabs ?? 0} ${(a.tabs ?? 0) === 1 ? 'tab' : 'tabs'}`, a.assistant_runs_here && 'assistant', a.logged_in === false && 'logged out'].filter(Boolean).join(' · '))),
      buckets.map((b) => h('div.bar', {}, h('span.bn', {}, b.bucket), h('span.bt', { html: `<i class="${pctClass(b.left_pct)}" style="width:${Math.max(0, Math.min(100, b.left_pct ?? 0))}%"></i>` }),
        h('span.bv', {}, `${b.left_pct ?? '?'}%${b.resets_at ? ' · ' + b.resets_at.replace(/^\w+ /, '') : ''}`))),
      a.usage_error ? h('div.cs', {}, a.usage_error) : null,
      a.next_reset_that_unblocks ? h('div.cs', {}, 'Unblocks ' + a.next_reset_that_unblocks) : null);
  });
}

// The modes in plain words, as the terminal UI's buttons say them.
const VOICE = { off: 'Voice off', push: 'Push to talk', wake: 'Wake word', open: 'Open mic' };
function modesCard() {
  const f = S.mode_flags || {};
  const mode = S.voice?.mode || 'off';
  const voice = h('select.inl', { 'aria-label': 'Voice' }, Object.entries(VOICE).map(([v, l]) => h('option', { value: v, selected: mode === v }, l)));
  voice.onchange = () => run('set_mode', { voice: voice.value }, VOICE[voice.value]);
  const tog = (ic, k, l, on, tip) => li(ic, l, { fn: () => run('set_mode', { [k]: !on }, l + (on ? ' off' : ' on')), hint: on ? 'On' : 'Off', on, tip });
  const notes = [S.voice?.muted && 'Mic muted', f.direct && 'Talking straight to ' + f.direct, f.brainstorm && 'Brainstorm on', f.updates && 'Update mode on',
    !f.privacy && (f.emails_shown ? 'Emails shown' : 'Emails hidden')].filter(Boolean);
  return h('div.list', {}, h('label.li', {}, h('span', { html: icon('mic') }), h('span.ll', {}, 'Voice'), voice),
    tog('archive', 'memory_saver', 'Memory saver', f.memory_saver, 'Short history, idle background tabs paused, caches dropped'),
    tog('hash', 'privacy', 'Privacy', f.privacy, 'Hide email addresses'),
    li('stop', 'Free memory now', { fn: () => run('set_mode', { free_memory: true }, 'Freed memory'), tip: 'Pause idle background tabs and drop caches' }),
    li('reopen', 'New conversation', { fn: () => run('set_mode', { new_conversation: true }, 'New conversation'), tip: 'The assistant starts over after this turn' }),
    notes.length ? h('div.lrow', {}, h('div.cs', {}, notes.join(' · '))) : null);
}

function closedCards() {
  return (S.closed || []).map((c) => h('div.lrow', {}, h('div.ct', {}, h('b', {}, c.tab), h('span.cs', {}, c.account || '')),
    h('div.cs', { style: 'overflow-wrap:anywhere' }, String(c.cwd || '').replace(/^\/Users\/[^/]+/, '~')),
    h('div.acts', {}, h('button.btn.sm', { onclick: () => run('reopen_closed', { index: c.index }, 'Reopening') }, 'Reopen'))));
}

// Who archived or closed it, in words.
const BY = { web: 'from the web app', user: 'by you', assistant: 'by the assistant' };
function archivedCards() {
  return (S.archived?.recent || []).map((a) => h('div.lrow', {}, h('div.ct', {}, h('b', {}, a.name), h('span.cs', {}, [a.account_label, a.archived && [a.archived, BY[a.by] || (a.by ? 'by ' + a.by : '')].filter(Boolean).join(' ')].filter(Boolean).join(' · '))),
    h('div.cs', {}, a.folder || ''),
    h('div.acts', {}, h('button.btn.sm', { onclick: () => run('restore_archived', { id: a.id }, 'Restoring') }, 'Restore'))));
}

export function manageSheet() {
  const L = (rows, empty) => (rows.length ? h('div.list', {}, rows) : empty ? h('div.mnote', { style: 'padding:0 4px' }, empty) : null);
  const paint = () => [title('Manage'), h('div.gl', {}, 'Usage'), L(usageCards()),
    h('div.list', {}, li('usage', 'Accounts and logins', { fn: () => import('./accounts.js').then((m) => m.accountsSheet()), hint: 'log in, add, tokens', chev: true })),
    h('div.gl', {}, `Jobs${S.jobs?.next ? ' · next ' + S.jobs.next : ''}`), L(jobRows(), 'No jobs'),
    h('div.gl', {}, 'Loops'), L(loopRows(), 'No loops'),
    h('div.gl', {}, 'Modes'), modesCard(),
    ...(S.closed?.length ? [h('div.gl', {}, 'Recently closed'), L(closedCards())] : []),
    ...(S.archived?.recent?.length ? [h('div.gl', {}, `Archived${S.archived.count > S.archived.recent.length ? ' · ' + S.archived.count + ' in all' : ''}`), L(archivedCards())] : []),
    h('div.list', {}, li('gear', 'Settings', { fn: () => import('./settings.js').then((m) => m.settingsSheet()), hint: 'this browser, voice, GodTerm', chev: true }))];
  openSheet({ kind: 'manage', tall: true }, ...paint());
}

// Keep an open Manage sheet live (but not while typing in it).
let lastPaint = 0;
on('state', () => {
  if (performance.now() - lastPaint < 1000) return;
  lastPaint = performance.now();
  if (sheetKind === 'manage' && !$('#sheet-body').contains(document.activeElement)) {
    const top = $('#sheet-body').scrollTop;
    manageSheet();
    $('#sheet-body').scrollTop = top;
  }
});

// GodTerm's own update: the notes, and an update now that asks first
// (restart_to_update restarts GodTerm into the new version).
export function updateSheet(u, text) {
  const notes = h('div.list', {}, h('div.lrow', { style: 'white-space:pre-wrap;font-size:14px;max-height:45vh;overflow:auto' }, u.notes || 'No release notes.'));
  openSheet({ kind: 'update' }, title(text), u.state ? h('div.dim', {}, u.state) : null, notes,
    u.update_with ? h('div.dim', { style: 'font-size:13px' }, 'Updates with: ' + u.update_with) : null,
    h('div.acts', {}, u.page ? h('button.btn', { onclick: () => window.open(u.page, '_blank', 'noopener') }, 'Release page') : null,
      h('button.btn', { onclick: closeSheet }, 'Later'),
      h('button.btn.pri', { onclick: async () => { closeSheet(); await run('restart_to_update', {}, 'Updating'); } }, 'Update now')));
}
