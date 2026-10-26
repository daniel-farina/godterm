// GodTerm Web: wires the modules together. The debug log first, so it
// sees what the others log while they start.
import './debuglog.js';
import { $, h, on, emit, toast, paintIcons, isTouch, isDesk, DESK, store, uidOf, debounce, tcPaint } from './util.js';
import { connect, conn, send, tool } from './conn.js';
import { S, tabById } from './state.js';
import { panes } from './term.js';
import { initTabs, setTabActions, tabOrder, renderTabs } from './tabs.js';
import { initKeybar } from './keybar.js';
import { initChat, setChat, chatOpen, approvalsSheet } from './chat.js';
import { initInstall } from './install.js';
// Loaded after the first screen (none of these is needed for the first
// terminal frame): the keyboard's F6 / ? after the first paint, push
// notifications after hello, and voice (the largest module, with the
// mic) at hello when a mode is on (push to talk, open mic or the wake
// word start by themselves as before), else on the first tap on a mic
// button or 3 s after hello, whichever comes first. Each catches up
// with a connection that opened before it.
const once = (fn) => { let p = null; return () => (p ||= Promise.resolve(fn())); };
const initKeys = () => { const go = once(() => import('./keys.js').then((m) => m.initKeys())); requestAnimationFrame(() => setTimeout(go, 0)); setTimeout(go, 1500); };
const initPush = () => { const go = once(() => import('./push.js')); on('hello', go); setTimeout(go, 2500); };
// The status bar is a desktop's: loaded there, or once a window grows into one.
const initStatusBar = () => { const go = once(() => import('./statusbar.js').then((m) => m.initStatusBar())); if (isDesk()) go(); else matchMedia(DESK).addEventListener('change', (e) => e.matches && go()); };
const initVoice = () => {
  const go = once(() => import('./voice.js').then((m) => m.initVoice()));
  const want = store.get('voice.pick', 'off') !== 'off';
  on('hello', () => (want ? go() : setTimeout(go, 3000)));
  on('open-voice', go);
  on('msg:voice', go);
  for (const id of ['#b-ptt', '#b-ptt2', '#b-openmic', '#b-voice']) {
    const b = $(id);
    if (!b) continue;
    // A tap before voice is here: load it, then the tap again for its handler.
    b.addEventListener('pointerdown', go, { once: true, passive: true });
    const tap = (e) => { if (b.dataset.live) return; e.stopImmediatePropagation(); go().then(() => { b.dataset.live = 1; b.click(); }); };
    b.addEventListener('click', tap, { capture: true, once: true });
  }
  setTimeout(go, 8000);
};
// Management and sessions load on first use, keeping the first page small.
const manage = () => import('./manage.js');
const tabActions = (id, at) => manage().then((m) => m.tabActions(id, at));
const newTabSheet = () => manage().then((m) => m.newTabSheet());
const manageSheet = () => manage().then((m) => m.manageSheet());
const sessionsSheet = () => import('./sessions.js').then((m) => m.sessionsSheet());
const settingsSheet = (at) => import('./settings.js').then((m) => m.settingsSheet(at));
import { closeSheet, sheetOpen, sheetKind } from './sheet.js';

const root = document.documentElement;
paintIcons();
if (isTouch()) document.body.classList.add('touch');
if (location.search.includes('t=')) history.replaceState(null, '', location.pathname);

// The visible viewport (the on-screen keyboard shrinks it). The page
// itself never scrolls: iOS scrolls it to show a focused field (by 200 to
// 350 px when the keyboard opens in the installed app), which moved the
// header off screen. It is put back at once, and the app is sized to the
// visible part, so the key bar sits on the keyboard.
const vv = window.visualViewport;
// The keyboard is up when a text field has the focus and the viewport
// is well short of its tallest in this orientation (Android resizes the
// whole page, iOS only the visual viewport, so neither alone tells).
const tallest = {};
const typing = () => { const a = document.activeElement; return !!a && (a.tagName === 'TEXTAREA' || (a.tagName === 'INPUT' && !/^(checkbox|radio|button|file|range)$/.test(a.type)) || a.isContentEditable); };
const se = () => document.scrollingElement || document.documentElement;
function unscroll() {
  if (scrollY || se().scrollTop || document.body.scrollTop) { scrollTo(0, 0); se().scrollTop = 0; document.body.scrollTop = 0; }
}
let lastVp = '';
function viewport() {
  unscroll();
  const hgt = Math.round(vv ? vv.height : innerHeight);
  // What iOS still pans after the scroll is undone (the visual viewport
  // inside the layout one): the app follows it.
  const top = Math.max(0, Math.round(vv ? vv.offsetTop : 0));
  const k = (innerWidth > hgt + 150 ? 'l' : 'p') + innerWidth;
  tallest[k] = typing() ? Math.max(tallest[k] || 0, hgt) : hgt;
  const key = hgt + ',' + top;
  if (key !== lastVp) {
    lastVp = key;
    root.style.setProperty('--vh', hgt + 'px');
    root.style.setProperty('--vtop', top + 'px');
  }
  const kb = isTouch() && typing() && (hgt < tallest[k] - 120 || (!!vv && vv.height < innerHeight - 100));
  if (kb !== document.body.classList.contains('kb')) { document.body.classList.toggle('kb', kb); emit('layout'); }
}
// While the keyboard slides (about 300 ms) the viewport is followed every
// frame, so the key bar rides on it instead of jumping after it.
let follow = 0, followUntil = 0;
function track(ms = 700) {
  followUntil = Math.max(followUntil, performance.now() + ms);
  if (follow) return;
  const step = () => { viewport(); follow = performance.now() < followUntil ? requestAnimationFrame(step) : 0; };
  follow = requestAnimationFrame(step);
}
vv?.addEventListener('resize', () => { viewport(); track(); });
vv?.addEventListener('scroll', () => { viewport(); track(300); });
addEventListener('resize', () => { viewport(); track(); });
addEventListener('focusin', () => { unscroll(); track(); });
addEventListener('focusout', () => track());
addEventListener('scroll', unscroll, { passive: true });
document.body.addEventListener('scroll', unscroll, { passive: true });
viewport();

// What sits under the header (the update banner, toasts) pushes the
// phone's chat and drawer down by its height; the terminals refit.
let pushh = -1;
const pushObs = new ResizeObserver(() => {
  const n = (isDesk() ? ['#under', '#toasts'] : ['#under']).reduce((a, s) => a + ($(s).offsetHeight || 0), 0);
  if (n === pushh) return;
  pushh = n;
  root.style.setProperty('--pushh', n + 'px');
  emit('layout');
});
pushObs.observe($('#toasts'));
pushObs.observe($('#under'));
// Phones: the floating messages sit above the chat's composer and its
// attachments, whatever their height (a long draft, two thumbnails).
const chatObs = new ResizeObserver(() => root.style.setProperty('--chath', ($('#composer').offsetHeight + $('#att').offsetHeight) + 'px'));
chatObs.observe($('#composer'));
chatObs.observe($('#att'));

// Drawer (phone) or collapsible side list (desktop).
const side = $('#side'), scrim = $('#scrim');
// A squeezed desktop (the terminal would drop under 520 px with the list
// beside it) keeps the list as an overlay, opened and shut like the drawer.
const squeezedSide = () => isDesk() && $('#app').classList.contains('sq-side');
function drawer(open) {
  if (isDesk()) {
    if (open && $('#app').classList.contains('noside')) { $('#app').classList.remove('noside'); store.set('noside', false); squeeze(); }
    if (!squeezedSide()) { $('#app').classList.toggle('noside', !open); store.set('noside', !open); renderTabs(); emit('layout'); return; }
  }
  side.classList.toggle('open', open);
  scrim.classList.toggle('on', open || sheetOpen());
  if (open) renderTabs();
}
$('#b-menu').onclick = () => drawer(isDesk() && !squeezedSide() ? $('#app').classList.contains('noside') : !side.classList.contains('open'));
if (isDesk() && store.get('noside', false)) $('#app').classList.add('noside');
// The scrim closes the sheet first, then the drawer.
scrim.onclick = () => { if (sheetOpen()) closeSheet(); else drawer(false); };

// Swipes: from the left edge opens the drawer, leftwards on it closes it.
// The drawer follows the finger and settles open or shut.
{
  let g = null;
  const W = () => side.getBoundingClientRect().width;
  $('#app').addEventListener('touchstart', (e) => {
    if (isDesk() || e.touches.length !== 1 || sheetOpen()) return;
    const t = e.touches[0], open = side.classList.contains('open');
    if (!open && t.clientX > 22 + (parseFloat(getComputedStyle(root).getPropertyValue('--sal')) || 0)) return;
    if (open && !side.contains(e.target) && e.target !== scrim) return;
    // The status chips scroll sideways under the finger: not a close.
    if (open && e.target.closest('#chips')) return;
    g = { x: t.clientX, y: t.clientY, open, on: false };
    if (!open) e.stopPropagation();
  }, { capture: true, passive: true });
  $('#app').addEventListener('touchmove', (e) => {
    if (!g) return;
    const t = e.touches[0], dx = t.clientX - g.x, dy = t.clientY - g.y;
    if (!g.on) {
      if (Math.abs(dy) > 12 && Math.abs(dy) > Math.abs(dx)) { g = null; return; }
      if (Math.abs(dx) < 10) { if (!g.open) e.stopPropagation(); return; }
      g.on = true;
      side.style.transition = 'none';
    }
    e.stopPropagation();
    if (e.cancelable) e.preventDefault();
    const w = W(), off = g.open ? Math.min(0, dx) : Math.min(0, dx - w);
    side.style.transform = `translateX(${off}px)`;
    scrim.classList.add('on');
    scrim.style.opacity = String(1 + off / w);
    g.dx = dx;
  }, { capture: true, passive: false });
  const end = () => {
    if (!g) return;
    const was = g;
    g = null;
    if (!was.on) return;
    side.style.transition = side.style.transform = scrim.style.opacity = '';
    const w = W();
    drawer(was.open ? was.dx > -w * 0.3 : was.dx > w * 0.3);
  };
  $('#app').addEventListener('touchend', end, { capture: true });
  $('#app').addEventListener('touchcancel', end, { capture: true });
}
// The side list's width on a desktop: drag its right edge (the
// terminals refit once, when the drag ends), double click for the
// default 280 px. Kept per browser; 200 px to half the window.
{
  const app = $('#app');
  const clamp = (w) => Math.round(Math.max(200, Math.min(innerWidth * 0.5, w)));
  const apply = (w) => { if (w) app.style.setProperty('--sidew', clamp(w) + 'px'); else app.style.removeProperty('--sidew'); };
  const save = () => { store.set('sidew', parseFloat(app.style.getPropertyValue('--sidew')) || null); emit('layout'); };
  apply(store.get('sidew', null));
  const grip = h('div.side-grip', { role: 'separator', tabindex: 0, 'aria-orientation': 'vertical', 'aria-label': 'Resize the tab list', title: 'Drag to resize, double click to reset' });
  side.append(grip);
  let d = null;
  grip.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || !isDesk()) return;
    e.preventDefault();
    grip.setPointerCapture(e.pointerId);
    d = { x: e.clientX, w: side.getBoundingClientRect().width };
    root.classList.add('side-drag');
  });
  grip.addEventListener('pointermove', (e) => { if (d) apply(d.w + e.clientX - d.x); });
  const end = () => {
    if (!d) return;
    d = null;
    root.classList.remove('side-drag');
    save();
  };
  for (const ev of ['pointerup', 'pointercancel', 'lostpointercapture']) grip.addEventListener(ev, end);
  grip.addEventListener('dblclick', () => { apply(null); save(); });
  const nudge = debounce(save, 300);
  grip.addEventListener('keydown', (e) => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    e.preventDefault();
    apply(side.getBoundingClientRect().width + (e.key === 'ArrowLeft' ? -16 : 16));
    nudge();
  });
  // A narrower window keeps the list within half of it.
  addEventListener('resize', debounce(() => apply(store.get('sidew', null)), 100));
}
// The docked tab panel's width (a desktop): drag its left edge, double
// click for the default 420 px. Kept per browser; 300 px to half the window.
{
  const app = $('#app');
  const panel = $('#tabpanel');
  const clamp = (w) => Math.round(Math.max(300, Math.min(innerWidth * 0.5, w)));
  const apply = (w) => { if (w) app.style.setProperty('--tpw', clamp(w) + 'px'); else app.style.removeProperty('--tpw'); };
  const save = () => { store.set('tpw', parseFloat(app.style.getPropertyValue('--tpw')) || null); emit('layout'); };
  apply(store.get('tpw', null));
  const grip = h('div.side-grip', { role: 'separator', tabindex: 0, 'aria-orientation': 'vertical', 'aria-label': 'Resize the tab panel', title: 'Drag to resize, double click to reset' });
  panel.append(grip);
  let d = null;
  grip.addEventListener('pointerdown', (e) => {
    if (e.button !== 0 || !isDesk()) return;
    e.preventDefault();
    grip.setPointerCapture(e.pointerId);
    d = { x: e.clientX, w: panel.getBoundingClientRect().width };
    root.classList.add('side-drag');
  });
  grip.addEventListener('pointermove', (e) => { if (d) apply(d.w - (e.clientX - d.x)); });
  const end = () => {
    if (!d) return;
    d = null;
    root.classList.remove('side-drag');
    save();
  };
  for (const ev of ['pointerup', 'pointercancel', 'lostpointercapture']) grip.addEventListener(ev, end);
  grip.addEventListener('dblclick', () => { apply(null); save(); });
  const nudge = debounce(save, 300);
  grip.addEventListener('keydown', (e) => {
    if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
    e.preventDefault();
    apply(panel.getBoundingClientRect().width + (e.key === 'ArrowLeft' ? 16 : -16));
    nudge();
  });
  addEventListener('resize', debounce(() => apply(store.get('tpw', null)), 100));
}
// A narrow desktop window (an iPad upright, a 1000 px window with the tab
// panel or the chat open): the terminal keeps about 520 px. In order: the
// panel and the chat narrow to 300 px (--tpfit, --chfit); if that is not
// enough the side list folds into an overlay (sq-side); with both open
// and still no room the one opened last stays, and the one left floats
// over the terminal (sq-panel, sq-chat) when even 300 px are too wide.
// The widths the user set stay.
const MIN_MAIN = 520, MIN_PANEL = 300;
let squeeze = () => {};
{
  const app = $('#app');
  let last = '', wasPanel = false, wasChat = false;
  // A width: the one dragged (inline), else the stylesheet's default for this window.
  const px = (v, d) => parseFloat(app.style.getPropertyValue(v)) || parseFloat(getComputedStyle(app).getPropertyValue(v)) || d;
  squeeze = () => {
    const panel = !$('#tabpanel').hidden, chat = chatOpen();
    if (panel && !wasPanel) last = 'panel';
    if (chat && !wasChat) last = 'chat';
    wasPanel = panel; wasChat = chat;
    const cls = { 'sq-side': false, 'sq-panel': false, 'sq-chat': false };
    let tpfit = 0, chfit = 0;
    if (isDesk()) {
      const W = innerWidth;
      const sideW = app.classList.contains('noside') ? 0 : px('--sidew', 280);
      const tp0 = panel ? px('--tpw', 420) : 0, ch0 = chat ? px('--chatw', 380) : 0;
      // The panel and the chat down to 300 px against the room beside the side list.
      const shrink = (avail) => {
        let tp = tp0, ch = ch0, over = tp + ch - (avail - MIN_MAIN);
        if (over > 0 && tp) { const d = Math.min(over, Math.max(0, tp - MIN_PANEL)); tp -= d; over -= d; }
        if (over > 0 && ch) { const d = Math.min(over, Math.max(0, ch - MIN_PANEL)); ch -= d; over -= d; }
        return { tp, ch, over };
      };
      let r = shrink(W - sideW);
      // Still short: the side list folds, and the two get its room back.
      cls['sq-side'] = sideW > 0 && r.over > 0;
      if (cls['sq-side']) r = shrink(W);
      let { tp, ch, over } = r;
      if (over > 0 && tp && ch) {
        if (last === 'chat') manage().then((m) => m.setPanel(false)); else setChat(false);
        return;
      }
      if (over > 0) { if (tp) cls['sq-panel'] = true; else cls['sq-chat'] = true; }
      else { tpfit = tp; chfit = ch; }
    }
    if (tpfit) app.style.setProperty('--tpfit', tpfit + 'px'); else app.style.removeProperty('--tpfit');
    if (chfit) app.style.setProperty('--chfit', chfit + 'px'); else app.style.removeProperty('--chfit');
    let changed = false;
    for (const [k, v] of Object.entries(cls)) { if (app.classList.contains(k) !== v) changed = true; app.classList.toggle(k, v); }
    if (isDesk() && !cls['sq-side'] && side.classList.contains('open')) { side.classList.remove('open'); scrim.classList.toggle('on', sheetOpen()); }
    if (changed) renderTabs();
  };
  on('layout', squeeze);
  addEventListener('resize', debounce(squeeze, 120));
}
// A swipe left or right on the header goes to the next or previous tab
// (the drawer's order); a pill under the header says which one.
{
  let g = null, tipT = 0;
  // (Row 1 only: row 2 scrolls sideways.)
  const top = $('#top .tr1');
  const tip = h('div', { id: 'swtip', 'aria-live': 'polite', hidden: true });
  document.body.append(tip);
  top.addEventListener('touchstart', (e) => {
    if (isDesk() || e.touches.length !== 1 || panes.n !== 1) { g = null; return; }
    const t = e.touches[0];
    g = { x: t.clientX, y: t.clientY, done: false };
  }, { passive: true });
  top.addEventListener('touchmove', (e) => {
    if (!g || g.done) return;
    const t = e.touches[0], dx = t.clientX - g.x, dy = t.clientY - g.y;
    if (Math.abs(dy) > 30) { g = null; return; }
    if (Math.abs(dx) < 56 || Math.abs(dx) < Math.abs(dy) * 2) return;
    g.done = true;
    const ids = tabOrder(), cur = panes.focused()?.id;
    if (ids.length < 2) return;
    const i = ids.indexOf(cur), n = ids.length;
    const next = ids[((i < 0 ? 0 : i) + (dx < 0 ? 1 : -1) + n) % n];
    panes.show(next, 0);
    emit('picked', next);
    const tb = tabById(next);
    tip.textContent = `${ids.indexOf(next) + 1} / ${n}  ${tb ? tb.name : next}`;
    tip.hidden = false;
    tip.classList.remove('out');
    clearTimeout(tipT);
    tipT = setTimeout(() => { tip.classList.add('out'); tipT = setTimeout(() => { tip.hidden = true; }, 220); }, 900);
  }, { passive: true });
  top.addEventListener('touchend', () => { g = null; }, { passive: true });
}
addEventListener('keydown', (e) => { if (e.key === 'Escape' && sheetOpen()) { closeSheet(); e.preventDefault(); } });
on('picked', () => {
  if (!isDesk() || squeezedSide()) { side.classList.remove('open'); scrim.classList.toggle('on', sheetOpen()); if (!isDesk() && chatOpen()) setChat(false); }
});

// Header: the focused tab and the connection.
function title() {
  const p = panes.focused();
  const t = p && tabById(p.id);
  $('#t-name').textContent = t ? t.name : 'GodTerm';
  $('#t-dot').className = 'dot' + (t ? ' ' + (t.state || 'idle') : '');
  tcPaint($('#t-tc'), t);
  $('#t-sub').textContent = t ? [t.account_label, t.model, t.state].filter(Boolean).join(' · ') : `${S.tabs.length} tabs`;
  $('#title').title = t ? [t.name, t.account_label, t.model, t.state, t.folder].filter(Boolean).join(' · ') : '';
  document.title = t ? `${t.name} · GodTerm` : 'GodTerm';
}
on('state', title);
on('focus', title);
// On a desktop the tab's options are a docked panel at the right: the
// Tab button (and the title) toggle it.
const togglePanel = () => manage().then((m) => m.setPanel(!m.panelIsOn()));
$('#title').onclick = () => { const p = panes.focused(); if (!p?.id) return; if (isDesk()) togglePanel(); else tabActions(p.id); };
let downTimer = 0, authSaid = false;
on('conn', (s) => {
  const c = $('#conn');
  c.className = 'conn ' + (s === 'live' ? 'live' : s === 'connecting' ? '' : 'down');
  c.title = s;
  clearTimeout(downTimer);
  if (s === 'live') authSaid = false;
  if (s === 'auth') { if (!authSaid) toast('Not signed in on this device', 'error', { label: 'Sign in', fn: () => location.assign('./') }); authSaid = true; }
  else if (s === 'down' && !authSaid) downTimer = setTimeout(() => !conn.open && toast('GodTerm is not reachable: retrying', 'warn'), 2500);
});

// Buttons.
$('#b-split').onclick = () => {
  const n = panes.want === 1 ? 2 : panes.want === 2 ? 4 : 1;
  panes.setN(n);
  if (panes.n < n) toast(`Room for ${panes.n === 1 ? 'one pane' : panes.n + ' panes'} at this size`, 'info');
};
$('#b-sessions').onclick = sessionsSheet;
$('#b-sess2').onclick = () => { drawer(false); sessionsSheet(); };
$('#b-new').onclick = () => { drawer(false); newTabSheet(); };
$('#b-more').onclick = manageSheet;
$('#b-newtab').onclick = newTabSheet;
$('#b-gear').onclick = () => settingsSheet();
$('#b-tabmenu').onclick = () => { if (isDesk()) return togglePanel(); const p = panes.focused(); if (!p?.id) return toast('Pick a tab first', 'warn'); tabActions(p.id, null); };
// The status bar's items open their section of the tab's options.
on('tabsection', (key, r) => { const p = panes.focused(); if (p?.id) manage().then((m) => m.tabSection(p.id, key, r)); });
// Find: the tab list's filter (the drawer on a phone).
$('#b-find').onclick = () => {
  if (isDesk() && !squeezedSide()) { if ($('#app').classList.contains('noside')) drawer(true); } else if (!side.classList.contains('open')) drawer(true);
  setTimeout(() => { $('#q').focus(); $('#q').select(); }, isDesk() ? 0 : 200);
};
on('unread', () => $('#b-chat').classList.add('dot-new'));
on('layout', () => requestAnimationFrame(() => panes.visible().forEach((p) => p.layout())));

// Long press on a tab row opens its options (iOS has no contextmenu).
let lp = 0, lpFired = false;
$('#list').addEventListener('touchstart', (e) => {
  const row = e.target.closest('.row');
  lpFired = false;
  if (!row) return;
  lp = setTimeout(() => { lpFired = true; navigator.vibrate?.(20); tabActions(row.dataset.id); }, 520);
}, { passive: true });
// (Cleared on the window, in the capture phase: the drawer's swipe
// handler stops the event before it reaches the list, and a swipe that
// starts on a row must not open that row's options.)
for (const ev of ['touchmove', 'touchend', 'touchcancel']) addEventListener(ev, () => clearTimeout(lp), { capture: true, passive: true });
$('#list').addEventListener('click', (e) => { if (lpFired) { e.stopPropagation(); e.preventDefault(); lpFired = false; } }, true);
on('tabmenu', (id, at) => tabActions(id, at));
// Open on Mac: the GodTerm app's own window to the front (the web app
// stays as it is).
$('#b-mac')?.addEventListener('click', async () => {
  const r = await tool('open_desktop', {});
  if (r.ok === false || r.error) toast(r.error || 'Could not open GodTerm on the Mac', 'warn');
  else toast(r.result?.say || 'GodTerm is at the front on the Mac', 'ok');
});

// Update available: a newer web build, or GodTerm's own update news.
// The build this page was served as (its own ?v=), else the first hello's.
let myBuild = new URL(import.meta.url).searchParams.get('v'), swReg = null;
// A newer web build: a button in the top bar (no row of its own). App
// update news keeps the banner, since it has more to say.
function banner(text, act) {
  if (!act) {
    const b = $('#b-upd');
    b.title = text;
    b.setAttribute('aria-label', 'Update available: reload');
    b.onclick = reload;
    b.hidden = false;
    return;
  }
  $('#update-text').textContent = text;
  $('#update-go').textContent = act.label;
  $('#update-go').onclick = act.fn;
  $('#update').hidden = false;
  emit('layout');
}
$('#update-x').onclick = () => { $('#update').hidden = true; emit('layout'); };
function reload() {
  const w = swReg?.waiting;
  if (w) { w.postMessage('skip'); navigator.serviceWorker.addEventListener('controllerchange', () => location.reload()); setTimeout(() => location.reload(), 1500); }
  else location.reload();
}
function seeBuild(b) {
  if (!b) return;
  if (myBuild == null) { myBuild = b; return; }
  if (b !== myBuild) { swReg?.update().catch(() => {}); banner('Update available: tap to reload'); }
}
on('hello', (m) => seeBuild(m.build));
on('msg:build', (m) => seeBuild(m.build || m.hash));
function appUpdate(u) {
  if (!u || !u.available || store.get('upd-seen') === u.version) return;
  const text = u.text || `GodTerm ${u.version || ''} is available${u.current ? ' (you have ' + u.current + ')' : ''}`;
  banner(text, { label: 'Details', fn: () => {
    store.set('upd-seen', u.version);
    $('#update').hidden = true;
    emit('layout');
    manage().then((m) => m.updateSheet(u, text));
  } });
}
on('msg:update', appUpdate);
on('hello', (m) => appUpdate(m.update));

// What this browser shows, for the assistant: a turn typed here gets it
// as context, and its web_* tools change it with `ui` frames.
let lastView = '';
const sendView = debounce(() => {
  if (!conn.open) return;
  const v = { t: 'view', device: isDesk() ? 'desktop' : 'phone', panes: panes.n, tabs: panes.visible().map((p) => (p.id ? uidOf(p.id) : null)),
    focused: panes.cur, open: sheetKind || (chatOpen() ? 'chat' : '') };
  const j = JSON.stringify(v);
  if (j !== lastView && send(v)) lastView = j;
}, 150);
for (const ev of ['focus', 'layout', 'sheet', 'picked']) on(ev, sendView);
on('conn', (s) => { if (s === 'live') { lastView = ''; sendView(); } });
on('msg:ui', (m) => {
  const phone = !isDesk();
  if (m.action === 'layout') {
    const tabs = (m.tabs || []).map((u) => 't' + uidOf(u));
    if (!phone) panes.setN(m.panes || Math.max(1, tabs.length));
    tabs.slice(0, panes.n).forEach((id, i) => panes.list[i].setTab(id));
    panes.focus(0);
    panes.save(); panes.sync();
    emit('focus', panes.focused());
    if (phone && tabs.length) setChat(false);
  } else if (m.action === 'show') {
    let i = m.pane ? m.pane - 1 : panes.cur;
    if (phone) i = 0;
    else if (i >= panes.n) panes.setN(i < 2 ? 2 : 4);
    panes.list[i].setTab('t' + uidOf(m.tab));
    panes.focus(i); panes.save(); panes.sync();
    emit('focus', panes.focused());
    if (phone) setChat(false);
  } else if (m.action === 'view') {
    const v = m.view;
    if (v === 'terminal') { closeSheet(); setChat(false); }
    else if (v === 'chat') { closeSheet(); setChat(true); }
    else if (v === 'sessions') sessionsSheet();
    else if (v === 'manage') manageSheet();
    else if (v === 'settings') settingsSheet();
    else if (v === 'accounts') import('./accounts.js').then((x) => x.accountsSheet());
    else if (v === 'approvals') approvalsSheet();
  } else if (m.action === 'font') {
    store.set('fontpref', m.fit ? null : m.size);
    emit('layout');
  }
  sendView();
});

// Boot. The socket first: its handshake (a round trip or two) overlaps
// the modules' setup below, which all runs before its first event.
connect();
initTabs();
setTabActions(tabActions);
panes.init();
initKeybar();
initChat();
initVoice();
initPush();
initInstall();
initStatusBar();
initKeys();
title();
if (isDesk() && store.get('tabpanel', false)) manage().then((m) => m.setPanel(true, false));
if (isDesk() && store.get('chat', false)) setChat(true);
squeeze();
on('layout', () => store.set('chat', chatOpen()));

if ('serviceWorker' in navigator && window.isSecureContext) {
  navigator.serviceWorker.register('sw.js').then((r) => { swReg = r; }).catch(() => {});
}
window.GT = { S, panes, conn };
