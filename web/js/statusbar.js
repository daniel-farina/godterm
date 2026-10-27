import { $, h, on, emit, icon, pctClass, isDesk, tcOf, tcStyle, words } from './util.js';
import { S, tabById, acctOf } from './state.js';
import { panes } from './term.js';
import { openMenu, closeSheet } from './sheet.js';

const BROWSER = {
  ready: ['browser ready', 'ok', 'BrowserSkill is ready: this tab\'s agent can drive its Chrome profile'],
  needs_install: ['browser: add BrowserSkill', 'warn', 'BrowserSkill is not in this tab\'s Chrome profile yet'],
  needs_enable: ['browser: turn on', 'warn', 'BrowserSkill is in the profile but turned off'],
  none: ['no browser', 'faint', 'No Chrome profile chosen for this tab\'s agent'],
};

const item = (key, cls, title, ...kids) => h('button.sb-i' + (cls ? '.' + cls : ''), { type: 'button', 'data-k': key, title }, ...kids.flat().filter((k) => k != null && k !== ''));
const ic = (name) => h('span.sb-ic', { html: icon(name) });
const plural = (n, w) => `${n} ${w}${n === 1 ? '' : 's'}`;

function usage(t) {
  const a = acctOf(t.account);
  const label = t.account_label || a?.label || 'account';
  if (!a) return item('usage', '', label, h('span.sb-v', {}, label));
  const left = a.effective_left_pct ?? a.five_hour_left_pct;
  const buckets = (a.buckets?.length ? a.buckets : [{ bucket: '5 hour', left_pct: a.five_hour_left_pct }, { bucket: 'week', left_pct: a.week_left_pct }])
    .filter((b) => b.left_pct != null).map((b) => `${b.bucket} ${b.left_pct}% left`);
  const tip = [label, ...buckets, a.logged_in === false && 'logged out', a.usage_error && !(a.logged_in === false && /logged/i.test(a.usage_error)) && words(a.usage_error)].filter(Boolean).join(' · ');
  const cls = a.logged_in === false ? 'warn' : pctClass(left) === 'out' ? 'bad' : pctClass(left) === 'lo' ? 'warn' : '';
  return item('usage', cls, tip, h('span.sb-v', {}, label),
    left != null ? h('span.sb-meter', { html: `<i class="${pctClass(left)}" style="width:${Math.max(0, Math.min(100, left))}%"></i>` }) : null,
    left != null ? h('span.sb-d', {}, left + '% left') : a.logged_in === false ? h('span.sb-d', {}, 'logged out') : null);
}

function model(t) {
  const m = t.model || (acctOf(t.account)?.harness ? acctOf(t.account).harness + ' picks' : '');
  const tip = [t.model ? 'Model ' + t.model : 'The CLI picks its model', t.effort && 'effort ' + t.effort, t.model_queued && 'switching to ' + t.model_queued].filter(Boolean).join(' · ') + ': click to switch';
  return item('model', '', tip, ic('model'), h('span.sb-v', {}, m || 'model'),
    t.effort ? h('span.sb-d', {}, '· ' + t.effort) : null,
    t.model_queued ? h('span.sb-q', {}, '→ ' + t.model_queued) : null);
}

function browser(t) {
  const st = typeof t.browser === 'object' && t.browser ? t.browser.state || 'none' : t.browser ? 'ready' : 'none';
  const [word, cls, tip] = BROWSER[st] || [String(st).replace(/_/g, ' '), '', ''];
  const prof = typeof t.browser === 'object' && t.browser?.profile;
  const last = typeof t.browser === 'object' && t.browser?.last_action;
  return item('browser', cls, [tip, prof && 'profile ' + prof, last && 'last: ' + last].filter(Boolean).join(' · '), ic('globe'), h('span.sb-v', {}, word));
}

function loops(t) {
  const n = t.loops || 0;
  const jobs = (S.jobs?.jobs || []).filter((j) => j.tab === t.name).length;
  const words = [n || !jobs ? plural(n, 'loop') : '', jobs ? plural(jobs, 'job') : ''].filter(Boolean).join(' · ');
  return item('loops', n || jobs ? '' : 'faint', `This tab's loops and jobs: ${words}`, ic('clock'), h('span.sb-v', {}, words));
}

function git(t) {
  const g = t.git_facts;
  const warn = t.git_warn;
  const tip = [t.git, warn && '⚠ ' + warn].filter(Boolean).join(' · ') || 'Git';
  if (!g) {

    if (!t.git && !warn) return item('git', 'faint', 'Git: status, commit, push and pull', ic('hash'), h('span.sb-v', {}, 'git'));
    const word = t.git === 'not a git repo' || /not in git/.test(warn || '') ? 'no git' : 'git';
    return item('git', warn ? 'warn' : 'faint', tip, ic('hash'), h('span.sb-v', {}, word), warn ? h('span.sb-w', {}, '⚠') : null);
  }
  const bits = [g.error ? 'git error' : null, g.dirty ? g.dirty + ' changed' : null, g.unpushed ? '↑' + g.unpushed : null, g.behind ? '↓' + g.behind : null].filter(Boolean);
  return item('git', g.error ? 'bad' : warn ? 'warn' : '', tip, ic('hash'), h('span.sb-v', {}, g.branch || '?'),
    bits.length ? h('span.sb-d', {}, '· ' + bits.join(' ')) : null, warn ? h('span.sb-w', {}, '⚠') : null);
}

function state(t) {
  const extra = [t.label && t.label !== t.state ? t.label : '', t.queued_prompts ? t.queued_prompts + ' queued' : '', t.move_pending ? 'moving to ' + t.move_pending.to + ' when ' + t.move_pending.waiting_for + ' end' : ''].filter(Boolean).join(' · ');
  const tip = [t.state, t.waiting_for, t.background_work, extra].filter(Boolean).join(' · ') + ': click for the tab\'s options';
  return item('root', 'sb-state', tip, h('span.dot.' + (t.state || 'idle').split(' ')[0]), h('span.sb-v', {}, t.state || 'idle'), extra ? h('span.sb-d', {}, '· ' + extra) : null);
}

function color(t) {
  const c = tcOf(t);
  if (!c) return null;
  const b = item('color', '', `Accent color ${c}: click to change`, h('span.sb-sw'), h('span.sb-d', {}, c));
  b.setAttribute('style', tcStyle(t));
  return b;
}

const DROP = ['color', 'loops', 'browser', 'git', 'model', 'usage'];
function fit(bar, items, st) {
  const W = bar.clientWidth;
  if (!W) return;
  bar.classList.add('measuring');
  const w = new Map(items.map((i) => [i, i.offsetWidth]));
  const sw = st.offsetWidth;
  bar.classList.remove('measuring');
  const GAP = 1, MORE = 36;
  let total = 8 + 12 + sw + items.reduce((a, i) => a + w.get(i) + GAP, 0);
  const dropped = [];
  for (const k of DROP) {
    if (total <= W) break;
    const it = items.find((i) => i.dataset.k === k);
    if (!it) continue;
    if (!dropped.length) total += MORE;
    total -= w.get(it) + GAP;
    dropped.push(it);
    it.remove();
  }
  if (!dropped.length) return;
  const more = item('more', '', 'More: ' + dropped.map((d) => d.querySelector('.sb-v')?.textContent || d.dataset.k).join(' · '), ic('more'));
  more.onclick = (e) => {
    e.stopPropagation();
    const r = more.getBoundingClientRect();
    openMenu({ x: r.left, y: r.top - 6, up: true }, h('div.sb-more', { onclick: (ev) => { const b = ev.target.closest('.sb-i'); if (!b) return; closeSheet(); emit('tabsection', b.dataset.k, r); } }, h('div.gl', {}, 'This tab'), dropped));
  };
  bar.insertBefore(more, bar.querySelector('.sb-gap'));
}

let sig = '';
function paint() {
  const bar = $('#sbar');
  if (!bar) return;
  const t = isDesk() ? tabById(panes.focused()?.id) : null;
  bar.hidden = !t;
  if (!t) { sig = ''; return; }
  const a = acctOf(t.account);
  const jobs = (S.jobs?.jobs || []).filter((j) => j.tab === t.name).length;
  const s = JSON.stringify([t, a?.effective_left_pct, a?.five_hour_left_pct, a?.week_left_pct, a?.buckets, a?.logged_in, jobs, bar.clientWidth]);
  if (s === sig) return;
  sig = s;
  const items = [color(t), usage(t), model(t), browser(t), loops(t), git(t)].filter(Boolean);
  const st = state(t);

  const had = bar.contains(document.activeElement) ? document.activeElement.closest('.sb-i')?.dataset.k : null;
  bar.replaceChildren(...items, h('span.sb-gap'), st);
  fit(bar, items, st);
  if (had) (bar.querySelector(`.sb-i[data-k="${had}"]`) || bar.querySelector('.sb-i'))?.focus({ preventScroll: true });
}

export function initStatusBar() {
  const bar = $('#sbar');
  bar.addEventListener('click', (e) => {
    const b = e.target.closest('.sb-i');
    if (!b) return;
    emit('tabsection', b.dataset.k, b.getBoundingClientRect());
  });
  on('state', paint);
  on('focus', paint);
  on('layout', () => { sig = ''; paint(); });
  addEventListener('resize', () => { sig = ''; paint(); });

  let rf = 0;
  new ResizeObserver(() => { cancelAnimationFrame(rf); rf = requestAnimationFrame(() => { sig = ''; paint(); }); }).observe(bar);
  paint();
}
