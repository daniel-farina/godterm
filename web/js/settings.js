// Settings (the gear in the top bar): every setting in one place, with a
// search box. This browser's own (kept on the device), then GodTerm's
// (list_settings, saved with set_setting as the Settings screen does),
// grouped as the user thinks of them: Voice, Terminal, Assistant,
// Accounts and usage, Jobs and loops, Web app, General, About.
import { $, h, on, emit, toast, store, icon, isDesk, isTouch, hscroll } from './util.js';
import { tool, send } from './conn.js';
import { S } from './state.js';
import { openSheet, closeSheet, sheetKind } from './sheet.js';
import { panes, prefFont, fitOn, deskFit } from './term.js';
import { keybarPref, applyKeybar } from './keybar.js';
import { install, standalone, ios, safari } from './install.js';
import { debugOn, setDebug, report } from './debuglog.js';
import { pushCard } from './push.js';
import { run, usageCards, jobRows, loopRows, updateSheet } from './manage.js';
import { ttsBox, voicePick, setMode, openVoiceSheet } from './voice.js';
import { gitcfgSection } from './gitcfg.js';
import { syncSection } from './syncui.js';

const SECTIONS = [
  ['browser', 'This browser'], ['voice', 'Voice'], ['terminal', 'Terminal'], ['assistant', 'Assistant'],
  ['accounts', 'Accounts and usage'], ['jobs', 'Jobs and loops'], ['web', 'Web app'], ['general', 'General'], ['about', 'About'], ['sync', 'Sync'],
];
SECTIONS.splice(5, 0, ['github', 'GitHub accounts and repos']);
// Where each GodTerm setting goes: by its key, else by its Settings
// screen section.
function sectionOf(s, from) {
  const k = s.key || '';
  if (k.startsWith('web.')) return 'web';
  if (k.startsWith('voice.') || /voice/i.test(from)) return 'voice';
  if (k.startsWith('assistant.') || from === 'Assistant') return 'assistant';
  if (k.startsWith('jobs.') || k.startsWith('loops.')) return 'jobs';
  if (from === 'Accounts') return 'accounts';
  if (from === 'Layout' || from === 'Memory') return 'terminal';
  return 'general';
}

// One row: a label with a line of help, and its control.
const row = (label, help, control, q = '') => h('label.li.st', { 'data-q': (label + ' ' + help + ' ' + q).toLowerCase() },
  h('span.stl', {}, h('span', {}, label), help ? h('small', {}, help) : null), control);
const check = (on, fn) => { const c = h('input', { type: 'checkbox', checked: !!on }); c.onchange = () => fn(c.checked); return c; };
const select = (opts, cur, fn, aria) => { const s = h('select.inl', { 'aria-label': aria }, opts.map(([v, l]) => h('option', { value: v, selected: String(cur) === String(v) }, l))); s.onchange = () => fn(s.value); return s; };
const btn = (label, fn, cls = '') => h('button.btn.sm' + cls, { type: 'button', onclick: (e) => { e.preventDefault(); fn(); } }, label);

// A GodTerm setting (list_settings' row), saved by set_setting (a risky
// one asks first, as everywhere).
function godRow(s) {
  const k = s.kind;
  const v = String(s.value ?? '');
  const args = (value) => (s.account ? { key: s.key, value, account: s.account } : { key: s.key, value });
  // The tap is the user's own request (as typing it to the assistant
  // would be): GodTerm runs admin changes only from the user.
  const save = (value) => run('set_setting', args(value), `${s.label}: ${value}`, { user_says: `set ${s.label} to ${value}` })
    // (The confirm sheet took Settings' place: back to it, same search.)
    .then(() => { if (!sheetKind || sheetKind === 'confirm') settingsSheet(lastQ); });
  let input;
  if (k === 'on/off') input = check(/^(on|true|yes)$/i.test(v), (b) => save(b ? 'on' : 'off'));
  else if (k && typeof k === 'object' && k['one of']) {
    const opts = k['one of'].map((o) => (Array.isArray(o) ? o : [o, o]));
    input = select(opts, v, save, s.label);
    if (!opts.some(([o]) => String(o) === v)) input.prepend(h('option', { value: v, selected: true, disabled: true }, v || 'not set'));
  } else if (k && typeof k === 'object' && (k.integer || k.number)) {
    const [min, max] = k.integer || k.number;
    input = h('input.inl', { type: 'number', min, max, step: k.integer ? 1 : 0.05, value: v, onchange: (e) => save(e.target.value) });
  } else if (typeof k === 'string' && k.startsWith('secret')) {
    input = h('span.lh', {}, v || 'not set');
  } else input = h('input.inl', { type: 'text', value: v, onchange: (e) => save(e.target.value) });
  const help = s.help ? String(s.help).split(/(?<=\.)\s/)[0] : '';
  return row(s.label + (s.account ? ` (account ${s.account})` : ''), help, input, s.key);
}

function browserRows() {
  const now = window.gtTheme?.get() || 'dark';
  const font = prefFont();
  const fontCtl = h('span.stepper', {},
    btn('A-', () => { store.set('fontpref', Math.max(8, Math.round(prefFont()) - 1)); emit('layout'); paintFont(); }),
    h('span.fv', {}, String(font)),
    btn('A+', () => { store.set('fontpref', Math.min(30, Math.round(prefFont()) + 1)); emit('layout'); paintFont(); }),
    btn('Reset', () => { store.set('fontpref', null); emit('layout'); paintFont(); }));
  const paintFont = () => { fontCtl.querySelector('.fv').textContent = String(prefFont()); };
  return [
    row('Theme', 'Dark, light, or as the system is', select([['dark', 'Dark'], ['light', 'Light'], ['system', 'Match system']], now, (v) => window.gtTheme?.set(v), 'Theme')),
    row('Text size', 'The terminal\'s font on this device (px)', fontCtl, 'font zoom'),
    isDesk() ? null : row('Fit text to this screen', 'The tab takes this screen\'s size: zooming rewraps the text', check(fitOn(), (b) => {
      store.set('fitscreen', b);
      if (!b) panes.list.forEach((p) => p.giveBack());
      emit('layout');
    })),
    isDesk() ? row('Fit tabs to this window', 'The tab shown takes this pane\'s size while you use it; the Mac\'s own pane gets it back when typed in', check(deskFit(), (b) => {
      store.set('deskfit', b);
      if (!b) panes.list.forEach((p) => p.giveBack());
      emit('layout');
    }), 'autofit fit size resize columns rows') : null,
    isTouch() ? row('Key bar', 'Esc, Tab, arrows, Ctrl and Send under the terminal; Automatic hides it once a hardware keyboard is used', select([['auto', 'Automatic'], ['always', 'Always'], ['never', 'Never']], keybarPref(), (v) => { store.set('keybar', v); applyKeybar(); }, 'Key bar'), 'keyboard keys hardware ipad') : null,
    isDesk() ? null : row('Return key', 'In a tab: a new line in the prompt (Send submits), or Enter', select([['newline', 'New line'], ['send', 'Send']], store.get('retkey', 'newline'), (v) => store.set('retkey', v), 'Return key')),
    row('Voice replies here', 'The voice the assistant speaks in, in this browser', h('span'), 'tts kokoro grok say voice'),
    h('div.st-tts', { 'data-q': 'voice replies tts kokoro grok say engine' }, ttsBox()),
    isTouch() ? null : row('Keyboard shortcuts', 'F6 moves between the areas, Shift+Esc leaves the terminal, ? lists them all', btn('Show', () => import('./keys.js').then((m) => m.helpSheet())), 'keys shortcuts f6 focus'),
    row('Debug log', 'What this page does, sent to the Mac\'s log', check(debugOn(), setDebug)),
    row('Send a debug report', 'The page\'s state now, no screenshot', btn('Send', () => {
      if (!debugOn()) return toast('Turn on Debug log first', 'warn');
      report().then((ok) => toast(ok ? 'Debug report sent' : 'Debug report queued: not connected'));
    })),
    // iOS Safari: there is no install prompt, and notifications need the
    // home screen app, so the two rows say the same thing.
    standalone() ? null : ios() && safari() ? row('Add to Home Screen', 'Share, then Add to Home Screen: full screen, and notifications work from there', btn('How', () => { closeSheet(); install(); }), 'install app')
      : row('Install as an app', 'On the home screen, full screen, with notifications', btn('Install', () => { closeSheet(); install(); })),
  ];
}

function voiceRows() {
  const flags = S.mode_flags || {};
  const pick = voicePick();
  const seg = h('span.seg.st-seg', {}, [['off', 'Off'], ['push', 'Push to talk'], ['open', 'Open mic'], ['wake', 'Wake word']].map(([m, l]) =>
    h('button', { type: 'button', 'aria-pressed': String(pick === m), onclick: (e) => { e.preventDefault(); setMode(m); for (const b of seg.children) b.setAttribute('aria-pressed', String(b === e.currentTarget)); } }, l)));
  const p = panes.focused();
  const t = p?.id && S.tabs.find((x) => x.id === p.id);
  return [
    h('div.st-block', { 'data-q': 'voice mode push to talk open mic wake word off' }, h('div.stl', {}, h('span', {}, 'Mode'), h('small', {}, 'The same as the mic in the top bar')), seg),
    row('Direct to this tab', flags.direct ? `Talking to ${flags.direct}` : 'What you say goes to the tab shown, its replies are spoken',
      check(!!flags.direct, (b) => (b ? t ? send({ t: 'voice', direct: t.uid }) : toast('Open a tab first', 'warn') : send({ t: 'voice', direct: false })))),
    row('Mic, mic method and test', 'How the mic is read here, a live level test', btn('Open', () => { closeSheet(); openVoiceSheet(); }), 'microphone worklet recorder'),
  ];
}

function terminalRows() {
  const p = panes.focused();
  return [
    row('WebGL renderer', `Faster drawing on the GPU (now ${p?.renderer || 'dom'}); off draws with the DOM`, check(store.get('webgl', true), (b) => {
      store.set('webgl', b);
      toast('Reload to switch the renderer', 'info', { label: 'Reload', fn: () => location.reload() });
    }), 'gpu'),
    row('Scrollback', 'Lines of history each terminal keeps in this browser', select([[1000, '1,000'], [5000, '5,000'], [10000, '10,000'], [50000, '50,000']], store.get('scrollback', 5000), (v) => {
      store.set('scrollback', +v);
      for (const q of panes.list) q.term.options.scrollback = +v;
    }, 'Scrollback'), 'history lines'),
    p?.id ? row('Take size for this tab', 'This pane sets the tab\'s columns and rows (the Mac gets them back after)', check(!!p.take, () => p.toggleTake()), 'resize pty') : null,
  ];
}

function aboutRows() {
  const u = S.update;
  return [
    row('GodTerm', S.version || '', h('span.lh', {}, ''), 'version'),
    row('Web build', String(S.build || 'dev').slice(0, 12), h('span.lh', {}, ''), 'build'),
    row('This device', `${navigator.userAgent.match(/iPhone|iPad|Android|Macintosh|Windows|Linux/)?.[0] || 'browser'} · ${innerWidth}x${innerHeight}@${devicePixelRatio}`, h('span.lh', {}, ''), 'device'),
    u?.available ? row(`GodTerm ${u.version || ''} is available`, u.current ? 'You have ' + u.current : '', btn('Details', () => updateSheet(u, `GodTerm ${u.version || ''} is available`), '.pri'), 'update')
      : row('Updates', 'GodTerm says when a new version is out', h('span.lh', {}, 'up to date'), 'update'),
  ];
}

const L = (rows) => { rows = rows.flat().filter(Boolean); return rows.length ? h('div.list', {}, rows) : null; };
const note = (t) => h('div.mnote.st-note', {}, t);

let sheet = null, lastQ = '';
export async function settingsSheet(query = '') {
  const q = h('input.st-q', { value: typeof query === 'string' ? query : '', type: 'search', placeholder: 'Search settings', autocomplete: 'off', spellcheck: 'false', enterkeyhint: 'search', 'aria-label': 'Search settings' });
  const secs = {};
  for (const [id, name] of SECTIONS) secs[id] = h('section.st-sec', { id: 'st-' + id, 'data-name': name.toLowerCase() }, h('div.gl', {}, name), h('div.st-body'));
  const body = (id) => secs[id].querySelector('.st-body');
  body('browser').append(L(browserRows()), h('div', { 'data-q': 'notifications push alerts' }, pushCard()));
  body('voice').append(L(voiceRows()), h('div.list.st-god'));
  body('terminal').append(L(terminalRows()), h('div.list.st-god'));
  body('assistant').append(h('div.list.st-god'));
  body('accounts').append(L([row('Accounts and logins', 'Log in from here, add or remove accounts, long-lived tokens', btn('Open', () => import('./accounts.js').then((m) => m.accountsSheet())), 'login logout token sign in add remove')]),
    L(usageCards()), h('div.list.st-god'));
  const jr = jobRows(), lr = loopRows();
  body('jobs').append(jr.length ? L(jr) : note('No jobs'), lr.length ? L(lr) : note('No loops'), h('div.list.st-god'));
  body('github').append(gitcfgSection());
  body('web').append(h('div.list.st-god'));
  body('general').append(h('div.list.st-god'));
  body('about').append(L(aboutRows()));
  // Settings > Sync: its own screen (web/js/syncui.js), filtered again
  // each time it draws.
  body('sync').append(syncSection());
  secs.sync.addEventListener('sy-render', () => filter());
  // One section shows at a time (kept per browser); a search looks through
  // all of them and shows every match, by section.
  let cur = store.get('st-sec', 'browser');
  if (!secs[cur]) cur = 'browser';
  const tabs = {};
  const nav = h('nav.st-nav', { 'aria-label': 'Sections' }, SECTIONS.map(([id, name]) => (tabs[id] = h('button', { type: 'button', onclick: () => {
    cur = id;
    store.set('st-sec', id);
    if (q.value) { q.value = ''; lastQ = ''; }
    filter();
    sheet?.closest('#sheet-body, .sheet-body')?.scrollTo?.({ top: 0 });
    secs[id].scrollIntoView({ block: 'nearest' });
  } }, h('span', {}, name), h('span.st-n')))));
  // What a row is found by: its words, its labels, its options' text.
  const words = (r) => ((r.dataset.q || '') + ' ' + r.textContent + ' ' + [...r.querySelectorAll('option, [placeholder], [aria-label], [title]')].map((e) => e.textContent + ' ' + (e.placeholder || '') + ' ' + (e.getAttribute('aria-label') || '') + ' ' + (e.title || '')).join(' ')).toLowerCase();
  const filter = () => {
    const t = q.value.trim().toLowerCase();
    for (const [id, sec] of Object.entries(secs)) {
      let n = 0;
      if (!t) {
        for (const r of sec.querySelectorAll('[hidden][data-st]')) { r.hidden = false; delete r.dataset.st; }
        sec.hidden = id !== cur;
      } else {
        // A section's name matching shows all of it.
        const whole = sec.dataset.name.includes(t);
        for (const r of sec.querySelectorAll('[data-stm]')) delete r.dataset.stm;
        for (const r of sec.querySelectorAll('[data-q], .st-body > .list > *, .st-body > .mnote')) {
          // Inside a match counts as a match (its words are the match's).
          const hit = words(r).includes(t);
          if (hit) r.dataset.stm = '1';
          const show = whole || hit || !!r.parentElement?.closest('[data-stm]');
          if (!show) { r.hidden = true; r.dataset.st = '1'; } else if (r.dataset.st) { r.hidden = false; delete r.dataset.st; }
          if (show) n++;
        }
        for (const l of sec.querySelectorAll('.st-body .list')) {
          const empty = ![...l.children].some((c) => !c.hidden);
          if (empty && !l.hidden) { l.hidden = true; l.dataset.st = '1'; } else if (!empty && l.dataset.st) { l.hidden = false; delete l.dataset.st; }
        }
        sec.hidden = !n;
      }
      tabs[id].classList.toggle('on', !t && id === cur);
      tabs[id].classList.toggle('none', !!t && !n);
      tabs[id].querySelector('.st-n').textContent = t && n ? String(n) : '';
    }
    none.hidden = !t || Object.values(secs).some((x) => !x.hidden);
    none.textContent = 'No setting matches "' + q.value.trim() + '"';
  };
  const none = h('div.mnote.st-none', { hidden: true });
  q.addEventListener('input', () => { lastQ = q.value; filter(); });
  lastQ = q.value;
  const head = h('div.sh-title', {}, h('span', {}, 'Settings'),
    h('button.ib', { 'aria-label': 'Close', title: 'Close', onclick: closeSheet, html: icon('x') }));
  sheet = h('div.stg', {}, h('div.st-top', {}, q, nav), h('div.st-main', {}, none, Object.values(secs)));
  hscroll(nav);
  openSheet({ kind: 'settings', tall: true, wide: true }, head, sheet);
  filter();
  // GodTerm's own settings, by section of its Settings screen.
  const r = await tool('list_settings', {});
  const names = r.result?.sections || r.sections || ['General', 'Layout', 'Accounts', 'Voice and Audio', 'Assistant', 'Memory'];
  const all = await Promise.all(names.map((n) => tool('list_settings', { section: n }).then((x) => [n, x.result?.settings || x.settings || []])));
  if (sheetKind !== 'settings') return;
  const seen = new Set();
  for (const [from, rows] of all) {
    for (const s of rows) {
      const id = (s.key || '') + '/' + (s.account || '');
      if (seen.has(id)) continue;
      seen.add(id);
      secs[sectionOf(s, from)].querySelector('.st-god')?.append(godRow(s));
    }
  }
  for (const g of sheet.querySelectorAll('.st-god')) {
    if (!g.children.length) g.remove();
    else g.before(h('div.st-sub', {}, 'GodTerm (all devices)'));
  }
  if (r.error) body('general').append(note('GodTerm\'s settings did not load: ' + r.error));
  filter();
}

on('sheet', (k) => { if (k !== 'settings') sheet = null; });
