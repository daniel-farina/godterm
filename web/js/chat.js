// The assistant chat panel and the approvals sheet.
import { $, h, on, emit, toast, isDesk, isTouch, store, emptyState, words, icon } from './util.js';
import { send } from './conn.js';
import { S, tabById } from './state.js';
import { md } from './md.js';
import { openSheet, closeSheet, sheetKind } from './sheet.js';
import { panes } from './term.js';

// id -> {el, body, text, at?, thumbs?, role}. The panel keeps at most
// MAX messages on the page: older ones go (the server's log is capped
// at 200 entries anyway, and the whole of it comes with `hello`).
const msgs = new Map();
const MAX = 500;
// `offline` until the socket is live; `connSeen` once it reported, so a
// page still connecting does not flash "Not connected".
let local = 0, stick = true, offline = true, connSeen = false, unseen = 0;
// The last question typed here (for Retry) and whether its reply is
// still to come (the typing indicator).
let lastSaid = null, waiting = 0;

const box = () => $('#msgs');
function scrollDown(force) {
  const m = box();
  if (force) { stick = true; unseen = 0; }
  if (stick) m.scrollTop = m.scrollHeight;
  paintJump();
}
// "New messages" when something arrived while scrolled up; a plain arrow
// while only scrolled up.
function paintJump() {
  const j = $('#jump');
  if (!j) return;
  j.hidden = stick;
  if (!stick) { const m = box(), c = $('#chat'); j.style.bottom = (c.clientHeight - (m.offsetTop + m.offsetHeight) + 10) + 'px'; }
  j.classList.toggle('new', unseen > 0);
  j.querySelector('span').textContent = unseen > 0 ? (unseen > 1 ? unseen + ' new messages' : 'New message') : '';
  j.setAttribute('aria-label', unseen > 0 ? 'Scroll to the new messages' : 'Scroll to the end');
}

const SR_MAX = 600;
const hhmm = (t) => new Date(t).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
// A day divider before the first message of each day ("Today",
// "Yesterday", else the date); `lastDay` is the day of the last one placed.
let lastDay = '';
const dayKey = (t) => { const d = new Date(t); return d.getFullYear() + '-' + d.getMonth() + '-' + d.getDate(); };
function dayWords(t) {
  const d = new Date(t), now = new Date(), k = dayKey(t);
  if (k === dayKey(now)) return 'Today';
  if (k === dayKey(now - 864e5)) return 'Yesterday';
  return d.toLocaleDateString([], now.getFullYear() === d.getFullYear() ? { weekday: 'short', day: 'numeric', month: 'short' } : { day: 'numeric', month: 'short', year: 'numeric' });
}
function dayDivider(at) {
  if (!at) return null;
  const k = dayKey(at);
  if (k === lastDay) return null;
  lastDay = k;
  return h('div.day', { role: 'separator', 'data-day': k }, h('span', {}, dayWords(at)));
}

// A tab the text names (its id "t12" or its name), for Open.
function mentioned(text) {
  const tabs = S.tabs || [];
  const m = text.match(/\bt(\d{1,6})\b/g);
  for (const id of m || []) { const t = tabById(id); if (t) return t; }
  const low = text.toLowerCase();
  let best = null;
  for (const t of tabs) {
    const n = String(t.name || '').toLowerCase();
    if (n.length < 3) continue;
    const i = low.indexOf(n);
    if (i < 0) continue;
    const b = i === 0 || /[^a-z0-9]/.test(low[i - 1]), a = i + n.length >= low.length || /[^a-z0-9]/.test(low[i + n.length]);
    if (b && a && (!best || n.length > best.name.length)) best = t;
  }
  return best;
}

// The row under a message: the time it arrived (hover on a desktop, a
// long press on a phone), Copy, Retry on the last question, Open for a
// tab it names.
function acts(e, id) {
  e.acts?.remove();
  const row = h('div.acts', { role: 'group', 'aria-label': 'Message actions' });
  if (e.at) row.append(h('time', { datetime: new Date(e.at).toISOString(), title: new Date(e.at).toLocaleString() }, hhmm(e.at)));
  row.append(h('button.ab', { type: 'button', onclick: () => copyText(e.text, 'Message copied') }, 'Copy'));
  if (e.role === 'user' && lastSaid && lastSaid.id === id) row.append(h('button.ab', { type: 'button', title: 'Ask the same again', onclick: () => resay() }, 'Retry'));
  const t = e.role !== 'user' && mentioned(e.text);
  if (t) row.append(h('button.ab', { type: 'button', onclick: () => showTab(t.id, true) }, 'Open ' + t.name));
  e.acts = row;
  e.el.append(row);
}
async function copyText(text, said) {
  try { await navigator.clipboard.writeText(text); toast(said, 'ok'); } catch { toast('Could not copy', 'warn'); }
}

// {id, role, text, done}; text is the whole message so far, or `delta`
// carries only the new part. `hist`: from the log at connect (no time).
function upsert(m, hist) {
  const b = box();
  // What GodTerm's own turns carry (tab events, their refused calls): the
  // terminal UI's user never needs it, so neither does this chat.
  if (m.hidden) return;
  let e = msgs.get(m.id);
  if (!e && m.role === 'user') {
    // Adopt our own optimistic echo of the same turn.
    for (const [k, x] of msgs) {
      if (!String(k).startsWith('local')) continue;
      // With images the turn comes back with their chips after the text.
      if (x.text === m.text || (x.thumbs?.length && m.text.startsWith(x.text))) { msgs.delete(k); e = x; if (lastSaid?.id === k) lastSaid.id = m.id; break; }
    }
  }
  // The final reply takes over the streamed one's element: no blink
  // between the live text going and the numbered message coming.
  if (!e && m.role === 'assistant' && m.id !== 'live' && msgs.get('live')?.ending) { e = msgs.get('live'); msgs.delete('live'); clearTimeout(e.endT); e.el.classList.remove('streaming'); if (typeof m.at === 'number' && m.at > 0) e.at = m.at; }
  const added = !e;
  if (!e) {
    e = { el: h('div.msg.' + (m.role || 'assistant'), { 'data-id': m.id }), text: '', thumbs: m.thumbs, role: m.role || 'assistant' };
    // When it was written: the server's time (GodTerm 0.9.x on), else the
    // moment it arrived here (none for the log at connect from an older
    // server).
    e.at = typeof m.at === 'number' && m.at > 0 ? m.at : hist ? undefined : Date.now();
    e.body = h('div.body');
    e.el.append(e.body);
    hideEmpty();
    const div = dayDivider(e.at);
    if (div) b.append(div);
    b.append(e.el);
  }
  msgs.set(m.id, e);
  e.text = m.delta != null ? e.text + m.delta : m.text ?? e.text;
  const streaming = m.done === false || (m.id === 'live' && !!e.text);
  // The streaming reply ("live") ends empty; the final text comes as its
  // own message right after (it adopts this element, above).
  if (m.id === 'live' && m.done !== false && !e.text) {
    e.ending = true;
    e.endT = setTimeout(() => { if (msgs.get('live') === e) { e.el.remove(); msgs.delete('live'); showEmptyIfNone(); } }, 1500);
    return;
  }
  if (m.role === 'assistant' || m.id === 'live') { waiting = 0; typing(false); }
  if (['tool', 'note', 'preamble'].includes(m.role)) e.el.className = 'msg system ' + m.role;
  if (m.role === 'tool') {
    chip(e);
    // The same call again, right after itself: one chip that counts.
    const prev = e.el.previousElementSibling;
    if (prev?.classList.contains('tool') && prev.dataset.chip === e.el.dataset.chip) {
      prev.dataset.n = String((+prev.dataset.n || 1) + 1);
      const lbl = prev.querySelector('.chipb > span');
      if (lbl) lbl.textContent = prev.dataset.chip + ' ×' + prev.dataset.n;
      e.el.remove();
      msgs.set(m.id, { el: prev, text: e.text, dup: true });
      return scrollDown();
    }
  }
  else if (m.role === 'note') e.body.textContent = humanNote(e.text);
  else if (m.role === 'preamble') e.body.textContent = e.text;
  else if (m.role === 'system') e.body.textContent = e.text;
  else if (m.role === 'user') {
    e.body.textContent = e.text;
    if (e.thumbs?.length && !e.el.querySelector('.thumbs')) e.el.insertBefore(h('div.thumbs', {}, e.thumbs.map((u) => h('img', { src: u, alt: '' }))), e.body.nextSibling);
  }
  else paintBody(e, streaming);
  e.el.classList.toggle('streaming', streaming);
  // A reply in the making is not read out: the finished one is, once.
  e.el.toggleAttribute('aria-hidden', streaming);
  if (m.id === 'live') b.append(e.el);
  if (!streaming && (m.role === 'assistant' || m.role === 'user')) { acts(e, m.id); e.el.classList.toggle('mine', m.role === 'user'); }
  if (m.role === 'assistant' && !streaming && !hist) {
    if (!stick) unseen++;
    const sr = $('#chat-sr');
    if (sr) sr.textContent = e.text.length > SR_MAX ? e.text.slice(0, SR_MAX) + '…' : e.text;
  }
  if (added) prune();
  scrollDown();
  if (m.role === 'assistant' && m.done !== false && !chatOpen()) emit('unread');
}

// The live reply paints once a frame at most, however fast the text
// comes: one markdown pass over the text so far, never the whole log.
let raf = 0, dirty = null;
function paintBody(e, streaming) {
  if (!streaming) { if (dirty === e) { cancelAnimationFrame(raf); raf = 0; dirty = null; } e.body.innerHTML = md(e.text); return; }
  dirty = e;
  if (raf) return;
  raf = requestAnimationFrame(() => { raf = 0; const x = dirty; dirty = null; if (x && x.el.isConnected) { x.body.innerHTML = md(x.text); scrollDown(); } });
}

// Past MAX messages the oldest leave the page (their ids too).
function prune() {
  const b = box();
  let n = b.querySelectorAll('.msg').length - MAX;
  while (n-- > 0) {
    const el = b.querySelector('.msg');
    if (!el) break;
    for (const [k, x] of msgs) if (x.el === el) msgs.delete(k);
    el.remove();
  }
  for (const d of b.querySelectorAll('.day')) if (!d.nextElementSibling || d.nextElementSibling.classList.contains('day')) d.remove();
}

// Three dots while the assistant thinks: after a question from here
// until its reply starts, and while GodTerm says it is busy.
function typing(onIt) {
  let t = $('#typing');
  if (!onIt) { t?.remove(); return; }
  if (t) return;
  hideEmpty();
  t = h('div.msg.assistant.typing', { id: 'typing', 'aria-hidden': 'true' }, h('span.dots', {}, h('i'), h('i'), h('i')));
  box().append(t);
  scrollDown();
}
function paintTyping() {
  const live = msgs.get('live');
  const busy = !!S.assistant?.busy;
  typing((waiting && Date.now() - waiting < 90000 || busy) && !(live && live.text && !live.ending));
}

// Tool names as what they did; a tab's id as its name.
const nameOf = (ref) => { const t = tabById(typeof ref === 'number' ? 't' + ref : ref); return t ? t.name : String(ref ?? ''); };
const withNames = (s) => String(s).replace(/\bt(\d{1,6})\b/g, (m) => nameOf(m) === m ? m : nameOf(m));
function humanNote(text) {
  let m;
  if ((m = text.match(/^Prompt to (\S+): (.*)$/))) {
    const who = nameOf(m[1]), what = m[2];
    if (/^delivered/.test(what)) return 'Sent to ' + who;
    if (/^queued/.test(what)) return 'Queued for ' + who + ' (it is busy)';
    return 'Prompt to ' + who + ': ' + what;
  }
  if ((m = text.match(/^Prompt #(\d+) to (\S+) (.*)$/))) return 'Prompt ' + m[1] + ' to ' + nameOf(m[2]) + ' ' + m[3];
  return withNames(text);
}
function humanTool(v) {
  const a = v.args || {}, tab = a.tab != null ? nameOf(a.tab) : '';
  const st = String(v.status || '');
  const bad = st && !/^ok\b/.test(st) && !/^(applied|done|queued|delivered)/.test(st);
  const short = (s, n = 48) => (s.length > n ? s.slice(0, n - 1) + '…' : s);
  const map = {
    send_prompt: () => 'Prompt to ' + (tab || 'a tab') + (/^queued/.test(st) ? ' (queued)' : ''),
    talk_to_tab: () => 'Talked to ' + tab,
    get_state: () => 'Looked at the tabs', search_tabs: () => 'Searched the tabs', find: () => 'Searched', sessions: () => 'Searched the sessions',
    read_tab: () => 'Read ' + tab, tab_history: () => 'Read ' + tab + "'s history", recent_turns: () => 'Read the recent turns',
    open_tab: () => 'Opened a tab' + (a.name ? ': ' + a.name : ''), reopen_tab: () => 'Reopened ' + tab, close_tabs: () => 'Closed ' + (tab || 'tabs'),
    rename_tab: () => 'Renamed ' + tab + (a.name ? ' to ' + a.name : ''), pin_tab: () => 'Pinned ' + tab, unpin_tab: () => 'Unpinned ' + tab,
    press_key: () => 'Pressed ' + (a.key || 'a key') + (tab ? ' in ' + tab : ''), answer_prompt: () => 'Answered ' + tab,
    show: () => 'Showed ' + tab, web_show: () => 'Showed ' + tab + ' here', web_layout: () => 'Changed the panes here', web_view: () => 'Opened ' + (a.view || 'a view') + ' here', web_font: () => 'Changed the text size here',
    set_tab_model: () => 'Set ' + tab + "'s model", archive_tabs: () => 'Archived ' + (tab || 'tabs'), move_tab: () => 'Moved ' + tab,
    create_job: () => 'Created a job' + (a.name ? ': ' + a.name : ''), run_job_now: () => 'Ran a job', adopt_loop: () => 'Adopted a loop',
    speak: () => 'Spoke', git_status: () => 'Checked git' + (tab ? ' in ' + tab : ''), git_log: () => 'Read the git log', git_diff: () => 'Read the diff',
    read_file: () => 'Read ' + short(String(a.path || 'a file')), list_dir: () => 'Listed ' + short(String(a.path || 'a folder')),
    open_path: () => 'Opened ' + short(String(a.path || 'a path')), open_desktop: () => 'Brought GodTerm to the front',
  };
  let s = map[v.tool] ? map[v.tool]() : words(v.tool) + (tab ? ' · ' + tab : '');
  if (bad) s += ' · ' + short(st.split(/[:.]/)[0], 30);
  return { text: s, bad };
}

// A tool call as one short line (as the terminal UI's chips); a tap shows
// the call in full.
function chip(e) {
  let v = null;
  try { v = JSON.parse(e.text); } catch {}
  if (!v || !v.tool) { e.body.textContent = e.text; return; }
  const { text, bad } = humanTool(v);
  e.el.classList.add('tool');
  e.el.classList.toggle('bad', bad);
  e.el.dataset.chip = text;
  const paintChip = (open) => {
    e.body.replaceChildren(h('button.chipb', { type: 'button', 'aria-expanded': String(open), title: open ? 'Hide the call' : 'Show the call' }, h('span', {}, text), h('span.chev', { html: icon('chev') })),
      ...(open ? [h('pre.call', {}, JSON.stringify(v, null, 1))] : []));
  };
  paintChip(false);
  e.body.addEventListener('click', (ev) => {
    if (!ev.target.closest('.chipb')) return;
    const open = e.el.classList.toggle('open');
    paintChip(open);
  });
}

export const chatOpen = () => $('#app').classList.contains('chat-open');
export function setChat(openIt) {
  $('#app').classList.toggle('chat-open', openIt);
  $('#b-chat').classList.toggle('on', openIt);
  if (openIt) { $('#b-chat').classList.remove('dot-new'); scrollDown(true); requestAnimationFrame(grow); if (isDesk()) $('#say').focus(); }
  emit('layout');
}

// Nothing said yet: what to ask, as prompts a tap sends.
function examples() {
  const t = (S.tabs || []).find((x) => x.running && x.name) || (S.tabs || [])[0];
  return ['What is every tab doing?', 'Which tabs need me?', t ? `Tell ${t.name} to run the tests` : 'Open a new tab for me'];
}
function showEmptyIfNone() {
  const b = box();
  if (b.querySelector('.msg:not(#typing)') || $('#chat-empty')) return;
  b.append(h('div.chat-empty', { id: 'chat-empty' }, h('div.ce-t', {}, 'Ask about your tabs'), h('div.ce-h', {}, 'The assistant sees every tab: what each one runs, what waits for you, what it last said.'),
    h('div.ce-ex', {}, examples().map((x) => h('button.ex', { type: 'button', onclick: () => { $('#say').value = x; grow(); submit(); } }, x)))));
}
const hideEmpty = () => $('#chat-empty')?.remove();

function say(text, images) {
  if (!send(images.length ? { t: 'say', text, images } : { t: 'say', text })) return false;
  const id = 'local' + ++local;
  // Retry moves to this question: the one before loses its button.
  const prev = lastSaid && msgs.get(lastSaid.id), prevId = lastSaid?.id;
  lastSaid = { id, text, images };
  if (prev?.acts) acts(prev, prevId);
  waiting = Date.now();
  upsert({ id, role: 'user', text, done: true, thumbs: images });
  paintTyping();
  return true;
}
function resay() {
  if (!lastSaid) return;
  if (!say(lastSaid.text, lastSaid.images || [])) toast('Not connected', 'warn');
}
function submit() {
  const ta = $('#say');
  const text = ta.value.trim();
  if (!text && !attached.length) return;
  if (!say(text, attached.map((a) => a.url))) return toast('Not connected: your message stays in the box', 'warn');
  ta.value = '';
  store.set('draft.say', null);
  attached = [];
  paintAtt();
  grow();
  paintSend();
  scrollDown(true);
}
// Send is for something to send over a live connection.
function paintSend() {
  const b = $('#b-send'), ta = $('#say');
  const empty = !ta.value.trim() && !attached.length, off = offline && connSeen;
  b.disabled = offline || empty;
  b.title = off ? 'Not connected' : empty ? 'Type a message to send' : isDesk() ? 'Send (Enter)' : 'Send';
  $('#composer').classList.toggle('off', off);
  // (A squeezed 300 px panel leaves the box about 150 px: a short placeholder, no wrap.)
  ta.placeholder = off ? 'Not connected' : $('#chat').clientWidth < 340 ? 'Ask' : 'Ask the assistant';
}

// Images for the next turn: pasted, dropped or picked. Anything that is
// not PNG, JPEG, GIF or WebP, or is large, is redrawn as a JPEG of at most
// 2000 px on the long side first (phone photos, HEIC).
let attached = [];
async function addImage(file) {
  if (!file || !/^image\//.test(file.type)) return;
  if (attached.length >= 6) return toast('At most 6 images per message', 'warn');
  let url;
  try { url = await (await import('./images.js')).prepare(file); } catch { return toast('That image could not be read', 'error'); }
  attached.push({ url, name: file.name || '' });
  paintAtt();
}
// Images for the next turn from elsewhere (shared to the app): attached,
// and the chat opens on them.
export async function attachFiles(files) {
  for (const f of files) await addImage(f);
  setChat(true);
}
function paintAtt() {
  const b = $('#att');
  b.hidden = !attached.length;
  b.replaceChildren(...attached.map((a, i) => h('span.thumb', { title: a.name }, h('img', { src: a.url, alt: 'image ' + (i + 1) }),
    h('button', { type: 'button', 'aria-label': 'Remove image ' + (i + 1), title: 'Remove', onclick: () => { attached.splice(i, 1); paintAtt(); $('#say').focus(); } }, '×'))));
  paintSend();
  paintJump();
  emit('layout');
}
// The box grows with the text to about six lines (less on a short screen).
function grow() { const ta = $('#say'); ta.style.height = 'auto'; ta.style.height = Math.min(ta.scrollHeight, 168, innerHeight * 0.4) + 'px'; paintJump(); }

// Approvals: from `approval` frames and get_state's pending_confirmations.
const approvals = new Map();
function addApproval(a) {
  if (a.done || a.resolved) { approvals.delete(a.id); return paintBadge(); }
  const isNew = !approvals.has(a.id);
  approvals.set(a.id, a);
  paintBadge();
  if (isNew) { fresh.push(a); clearTimeout(freshT); freshT = setTimeout(announce, 300); }
}
// Questions this browser's own tool calls raised: their sheet asks them
// already, so no toast for them.
const mine = new Set();
export const ownQuestion = (token) => mine.add('confirm:' + token);
// New approvals that arrive together get one toast.
let fresh = [], freshT = 0, lastToast = null;
function announce() {
  const list = fresh.filter((a) => approvals.has(a.id) && !mine.has(a.id));
  fresh = [];
  if (!list.length || sheetKind === 'approvals') return;
  const a = list[0];
  const t = tabById(typeof a.tab === 'number' ? 't' + a.tab : a.tab);
  const one = (t ? t.name + ': ' : a.name ? a.name + ': ' : '') + (a.question || 'needs a yes');
  // One approvals toast at a time: a newer one says how many wait.
  const still = lastToast?.isConnected;
  lastToast?.remove();
  const n = [...approvals.keys()].filter((k) => !mine.has(k)).length;
  // One approval: answered right from the banner (Yes, No), the text
  // opens the full question. Several: Review lists them. It stays until
  // answered, dismissed, or answered elsewhere.
  // The Approvals sheet open lists it already: no banner over it.
  if (sheetKind === 'approvals') return;
  if (list.length > 1 || still) lastToast = toast(`${n} approvals wait for you`, 'warn', { label: 'Review', fn: approvalsSheet }, 12000);
  else {
    const cs = choicesOf(a);
    const pick = (re) => cs.find((c) => re.test(c.id));
    const yes = pick(/^(yes|approve|allow)/i), no = pick(/^(no|deny|reject)/i);
    const answer = (c) => () => { send({ t: 'answer', id: a.id, choice: c.id }); approvals.delete(a.id); paintBadge(); toast('Answered ' + c.label.toLowerCase(), 'ok'); };
    // A prompt GodTerm cannot answer (a CLI menu it cannot read) has no
    // choices: the banner shows the tab instead, to answer it there.
    const acts = yes && no ? [{ label: 'No', fn: answer(no) }, { label: 'Yes', cls: 'pri', fn: answer(yes) }]
      : !cs.length && t ? [{ label: 'Show', cls: 'pri', fn: () => showTab(t.id) }] : [{ label: 'Review', fn: approvalsSheet }];
    lastToast = toast(one, 'warn appr', acts, 0);
    lastToast.dataset.appr = a.id;
    lastToast.querySelector('span').addEventListener('click', (e) => { e.stopPropagation(); lastToast?.remove(); approvalsSheet(); });
  }
  if (navigator.userActivation?.hasBeenActive) navigator.vibrate?.(60);
}
function paintBadge() {
  const n = approvals.size;
  // A banner for an approval answered elsewhere (or for none left) goes.
  if (lastToast?.isConnected && (lastToast.dataset.appr ? !approvals.has(lastToast.dataset.appr) : !n)) lastToast.remove();
  const b = $('#appr-n');
  b.hidden = !n;
  b.textContent = n;
  if (sheetKind === 'approvals') approvalsSheet();
}
// Show a tab (to answer its prompt there with the keys). `keep`: the chat
// stays open where it sits beside the terminal (a desktop).
function showTab(id, keep) {
  closeSheet();
  if (!keep || !isDesk()) setChat(false);
  panes.show(id);
  emit('picked', id);
}
function choicesOf(a) {
  // GodTerm lists what a tab's prompt can take (none: answer it in the tab).
  const c = Array.isArray(a.choices) && (a.choices.length || a.kind === 'tab') ? a.choices : ['yes', 'always', 'no'];
  return c.map((x) => (typeof x === 'string' ? { id: x, label: x[0].toUpperCase() + x.slice(1) } : { id: x.id ?? x.value ?? x.label, label: x.label ?? x.id }));
}
export function approvalsSheet() {
  const cards = [...approvals.values()].map((a) => {
    const t = tabById(typeof a.tab === 'number' ? 't' + a.tab : a.tab);
    return h('div.lrow.appr', {}, h('div.ct', {}, h('b', {}, t ? t.name : a.name || (a.kind === 'confirm' || a.kind === 'switch' ? 'Assistant' : 'GodTerm')), h('span.cs', {}, t?.account_label || '')),
      h('div.appr-q', {}, a.question || ''),
      h('div.appr-btns', {}, choicesOf(a).length ? choicesOf(a).map((c) => h('button.btn' + (/^(yes|approve|allow)/i.test(c.id) ? '.pri' : /^(no|deny|reject)/i.test(c.id) ? '.bad' : ''), {
        onclick: () => { send({ t: 'answer', id: a.id, choice: c.id }); approvals.delete(a.id); paintBadge(); if (!approvals.size) closeSheet(); },
      }, c.label)) : [h('span.dim', {}, 'GodTerm cannot read this prompt: answer it in the tab'), t ? h('button.btn.pri', { onclick: () => showTab(t.id) }, 'Show the tab') : null]));
  });
  openSheet({ kind: 'approvals' }, h('div.sh-title', {}, h('span', {}, 'Approvals'), h('button.ib', { 'aria-label': 'Close', onclick: closeSheet, html: '<svg viewBox="0 0 24 24"><path d="M6 6l12 12M18 6L6 18"/></svg>' })),
    cards.length ? h('div.list', {}, cards) : emptyState('Nothing waits for you', 'A tab at a permission prompt, or a yes the assistant asks for, shows up here.', { label: 'Close', fn: closeSheet }));
}

// An error from GodTerm about the chat itself (not a tab's, not a tool
// reply's): one line in the chat, with Retry for the last question.
function failLine(text) {
  hideEmpty();
  const el = h('div.msg.system.err', { role: 'alert' }, h('div.body', {}, text), lastSaid ? h('button.ab', { type: 'button', onclick: resay }, 'Retry') : null);
  box().append(el);
  waiting = 0;
  typing(false);
  scrollDown();
}

export function initChat() {
  on('msg:assistant', (m) => upsert(m));
  const reset = (list, live, liveAt) => {
    if (!Array.isArray(list)) return;
    box().replaceChildren();
    msgs.clear();
    stick = true; unseen = 0; lastDay = '';
    list.forEach((x, i) => upsert({ id: x.id ?? 'h' + i, done: true, ...x }, true));
    if (live) upsert({ id: 'live', role: 'assistant', text: live, done: false, at: liveAt });
    showEmptyIfNone();
    paintTyping();
  };
  on('hello', (m) => reset(m.messages || m.assistant?.messages, m.live, m.live_at));
  on('msg:assistant_reset', (m) => reset(m.messages));
  on('msg:answered', (m) => { if (m.ok === false) toast(m.error || 'That answer did not go through', 'error'); });
  on('msg:approval', addApproval);
  on('msg:approval_done', (m) => { approvals.delete(m.id); paintBadge(); });
  on('msg:toast', (m) => toast(m.text, m.level || 'info'));
  on('msg:error', (m) => { if (m.id == null && m.tab == null && m.error) failLine(String(m.error)); });
  on('conn', (s) => { offline = s !== 'live'; if (s !== 'connecting') connSeen = true; paintSend(); if (offline) { waiting = 0; typing(false); } });
  on('state', () => {
    paintTyping();
    // GodTerm's list is the authority when it sends one.
    if (Array.isArray(S.approvals)) {
      const ids = new Set(S.approvals.map((a) => a.id));
      for (const k of [...approvals.keys()]) if (!ids.has(k)) approvals.delete(k);
      for (const a of S.approvals) if (!approvals.has(a.id)) approvals.set(a.id, a);
      return paintBadge();
    }
    // Older shape: get_state's pending_confirmations.
    const live = new Set();
    for (const p of S.pending_confirmations || []) {
      if (p.answerable === false) continue;
      live.add('c:' + p.token);
      if (!approvals.has('c:' + p.token) && ![...approvals.values()].some((a) => a.token === p.token))
        approvals.set('c:' + p.token, { id: p.token, token: p.token, question: p.question, choices: ['yes', 'no'], fromState: true });
    }
    for (const [k, a] of approvals) if (a.fromState && !live.has(k)) approvals.delete(k);
    paintBadge();
  });
  const ta = $('#say');
  // A phone's return key makes a new line (Send sends), so it says so;
  // what is typed is kept on this device until it is sent (iOS may reload
  // the app while it is away).
  if (!isDesk()) ta.enterKeyHint = 'enter';
  ta.value = store.get('draft.say', '') || '';
  if (ta.value) requestAnimationFrame(grow);
  ta.addEventListener('input', grow);
  ta.addEventListener('input', () => { store.set('draft.say', ta.value || null); paintSend(); });
  new ResizeObserver(() => paintSend()).observe($('#chat'));
  ta.addEventListener('paste', (e) => {
    const files = [...(e.clipboardData?.files || [])].filter((f) => f.type.startsWith('image/'));
    if (!files.length) return;
    e.preventDefault();
    files.forEach(addImage);
  });
  // Images dropped anywhere on the panel (a desktop): a dashed drop zone
  // says so while they hover.
  const chat = $('#chat');
  let drags = 0;
  const hasFiles = (e) => !!e.dataTransfer?.types?.includes('Files');
  chat.addEventListener('dragenter', (e) => { if (!hasFiles(e)) return; e.preventDefault(); drags++; chat.classList.add('drop'); });
  chat.addEventListener('dragover', (e) => { if (hasFiles(e)) { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; } });
  chat.addEventListener('dragleave', (e) => { if (!hasFiles(e)) return; if (--drags <= 0) { drags = 0; chat.classList.remove('drop'); } });
  chat.addEventListener('drop', (e) => {
    drags = 0; chat.classList.remove('drop');
    const files = [...(e.dataTransfer?.files || [])].filter((f) => f.type.startsWith('image/'));
    if (!files.length) return;
    e.preventDefault();
    files.forEach(addImage);
    ta.focus();
  });
  $('#b-img').addEventListener('click', () => $('#f-img').click());
  $('#f-img').addEventListener('change', (e) => { [...e.target.files].forEach(addImage); e.target.value = ''; });
  ta.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && isDesk()) { e.preventDefault(); submit(); }
  });
  $('#composer').addEventListener('submit', (e) => { e.preventDefault(); submit(); });
  $('#b-send').addEventListener('click', (e) => { e.preventDefault(); submit(); ta.focus(); });
  const b = box();
  // Scrolling up unpins the log; a change of the log's own height (the
  // composer grows, the keyboard comes up, the window resizes) is not a
  // scroll: the end stays in view when it was.
  let lastH = b.clientHeight;
  const pinIfStuck = () => { if (stick) b.scrollTop = b.scrollHeight; paintJump(); };
  b.addEventListener('scroll', () => {
    if (b.clientHeight !== lastH) { lastH = b.clientHeight; return pinIfStuck(); }
    const was = stick;
    stick = b.scrollHeight - b.scrollTop - b.clientHeight < 40;
    if (stick) unseen = 0;
    if (was !== stick || !stick) paintJump();
  });
  new ResizeObserver(() => { lastH = b.clientHeight; pinIfStuck(); }).observe(b);
  $('#jump').addEventListener('click', () => { scrollDown(true); b.scrollTo({ top: b.scrollHeight, behavior: 'smooth' }); });
  // Copy on a code block; a long press on a message (a touch screen) shows
  // its actions, a tap elsewhere hides them.
  b.addEventListener('click', async (e) => {
    const c = e.target.closest('.cb-copy');
    if (c) {
      const code = c.closest('.cb')?.querySelector('code')?.textContent || '';
      try { await navigator.clipboard.writeText(code); c.textContent = 'Copied'; setTimeout(() => { c.textContent = 'Copy'; }, 1500); } catch { toast('Could not copy', 'warn'); }
    }
  });
  if (isTouch()) {
    let pressT = 0, pressed = null;
    b.addEventListener('pointerdown', (e) => {
      const m = e.target.closest('.msg.assistant, .msg.user');
      if (!m || e.target.closest('a, button, input')) return;
      pressed = m;
      pressT = setTimeout(() => { for (const o of b.querySelectorAll('.msg.show')) if (o !== m) o.classList.remove('show'); m.classList.toggle('show'); navigator.vibrate?.(20); pressed = null; }, 450);
    }, { passive: true });
    const cancel = () => { clearTimeout(pressT); pressed = null; };
    for (const ev of ['pointerup', 'pointercancel', 'pointermove', 'scroll']) b.addEventListener(ev, cancel, { passive: true });
  }
  $('#b-chat').addEventListener('click', () => setChat(!chatOpen()));
  $('#b-chat-x').addEventListener('click', () => setChat(false));
  $('#b-appr').addEventListener('click', approvalsSheet);
  paintSend();
  showEmptyIfNone();
}
