// The assistant chat panel and the approvals sheet.
import { $, h, on, emit, toast, isDesk, store } from './util.js';
import { send } from './conn.js';
import { S, tabById } from './state.js';
import { md } from './md.js';
import { prepare } from './images.js';
import { openSheet, closeSheet, sheetKind } from './sheet.js';
import { panes } from './term.js';

const msgs = new Map();
let local = 0, stick = true;

function scrollDown() { const m = $('#msgs'); if (stick) m.scrollTop = m.scrollHeight; }

// {id, role, text, done}; text is the whole message so far, or `delta`
// carries only the new part.
function upsert(m) {
  const box = $('#msgs');
  // What GodTerm's own turns carry (tab events, their refused calls): the
  // terminal UI's user never needs it, so neither does this chat.
  if (m.hidden) return;
  let e = msgs.get(m.id);
  if (!e && m.role === 'user') {
    // Adopt our own optimistic echo of the same turn.
    for (const [k, x] of msgs) {
      if (!String(k).startsWith('local')) continue;
      // With images the turn comes back with their chips after the text.
      if (x.text === m.text || (x.thumbs?.length && m.text.startsWith(x.text))) { msgs.delete(k); e = x; break; }
    }
  }
  if (!e) {
    e = { el: h('div.msg.' + (m.role || 'assistant')), text: '', thumbs: m.thumbs };
    box.append(e.el);
  }
  msgs.set(m.id, e);
  e.text = m.delta != null ? e.text + m.delta : m.text ?? e.text;
  // The streaming reply ("live") ends empty; the final text comes as its own message.
  if (m.id === 'live' && m.done !== false && !e.text) { e.el.remove(); msgs.delete('live'); return; }
  if (['tool', 'note', 'preamble'].includes(m.role)) e.el.className = 'msg system';
  if (m.role === 'tool') {
    chip(e);
    // The same call again, right after itself: one chip that counts.
    const prev = e.el.previousElementSibling;
    if (prev?.classList.contains('tool') && prev.dataset.chip === e.el.dataset.chip) {
      prev.dataset.n = String((+prev.dataset.n || 1) + 1);
      prev.setAttribute('data-count', '×' + prev.dataset.n);
      e.el.remove();
      msgs.set(m.id, { el: prev, text: e.text, dup: true });
      return scrollDown();
    }
  }
  else if (m.role === 'user') {
    e.el.textContent = e.text;
    if (e.thumbs?.length) e.el.append(h('div.thumbs', {}, e.thumbs.map((u) => h('img', { src: u, alt: '' }))));
  }
  else e.el.innerHTML = md(e.text);
  e.el.classList.toggle('streaming', m.done === false || (m.id === 'live' && !!e.text));
  if (m.id === 'live') box.append(e.el);
  scrollDown();
  if (m.role === 'assistant' && m.done !== false && !chatOpen()) emit('unread');
}

// A tool call as one short line (as the terminal UI's chips); a tap shows
// the call in full.
function chip(e) {
  let v = null;
  try { v = JSON.parse(e.text); } catch {}
  if (!v || !v.tool) { e.el.textContent = e.text; return; }
  const a = v.args || {};
  const what = a.tab || a.view || a.name || a.text || a.key || '';
  const st = String(v.status || '').split(/[:.]/)[0];
  e.el.classList.add('tool');
  e.el.textContent = [v.tool, typeof what === 'string' ? what.slice(0, 60) : JSON.stringify(what).slice(0, 60), st && st !== 'ok' ? st : ''].filter(Boolean).join(' · ');
  e.el.dataset.chip = e.el.textContent;
  e.el.title = 'Tap for details';
  e.el.onclick = () => {
    const open = e.el.classList.toggle('open');
    e.el.textContent = open ? JSON.stringify(v, null, 1) : '';
    if (!open) chip(e);
  };
}

export const chatOpen = () => $('#app').classList.contains('chat-open');
export function setChat(openIt) {
  $('#app').classList.toggle('chat-open', openIt);
  $('#b-chat').classList.toggle('on', openIt);
  if (openIt) { $('#b-chat').classList.remove('dot-new'); stick = true; scrollDown(); requestAnimationFrame(grow); if (isDesk()) $('#say').focus(); }
  emit('layout');
}

function submit() {
  const ta = $('#say');
  const text = ta.value.trim();
  if (!text && !attached.length) return;
  const images = attached.map((a) => a.url);
  if (!send(images.length ? { t: 'say', text, images } : { t: 'say', text })) return toast('Not connected', 'warn');
  const id = 'local' + ++local;
  upsert({ id, role: 'user', text, done: true, thumbs: images });
  ta.value = '';
  store.set('draft.say', null);
  attached = [];
  paintAtt();
  grow();
}

// Images for the next turn: pasted, dropped or picked. Anything that is
// not PNG, JPEG, GIF or WebP, or is large, is redrawn as a JPEG of at most
// 2000 px on the long side first (phone photos, HEIC).
let attached = [];
async function addImage(file) {
  if (!file || !/^image\//.test(file.type)) return;
  if (attached.length >= 6) return toast('At most 6 images per message', 'warn');
  let url;
  try { url = await prepare(file); } catch { return toast('That image could not be read', 'error'); }
  attached.push({ url });
  paintAtt();
}
// Images for the next turn from elsewhere (shared to the app): attached,
// and the chat opens on them.
export async function attachFiles(files) {
  for (const f of files) await addImage(f);
  setChat(true);
}
function paintAtt() {
  const box = $('#att');
  box.hidden = !attached.length;
  box.replaceChildren(...attached.map((a, i) => h('span.thumb', {}, h('img', { src: a.url, alt: 'image ' + (i + 1) }),
    h('button', { type: 'button', 'aria-label': 'Remove image', onclick: () => { attached.splice(i, 1); paintAtt(); } }, '×'))));
  emit('layout');
}
function grow() { const ta = $('#say'); ta.style.height = 'auto'; ta.style.height = Math.min(ta.scrollHeight, innerHeight * 0.4) + 'px'; }

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
// Show a tab (to answer its prompt there with the keys).
function showTab(id) {
  closeSheet();
  setChat(false);
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
    cards.length ? h('div.list', {}, cards) : h('div.empty', {}, 'Nothing waits for you'));
}

export function initChat() {
  on('msg:assistant', upsert);
  const reset = (list, live) => {
    if (!Array.isArray(list)) return;
    $('#msgs').replaceChildren();
    msgs.clear();
    list.forEach((x, i) => upsert({ id: x.id ?? 'h' + i, done: true, ...x }));
    if (live) upsert({ id: 'live', role: 'assistant', text: live, done: false });
  };
  on('hello', (m) => reset(m.messages || m.assistant?.messages, m.live));
  on('msg:assistant_reset', (m) => reset(m.messages));
  on('msg:answered', (m) => { if (m.ok === false) toast(m.error || 'That answer did not go through', 'error'); });
  on('msg:approval', addApproval);
  on('msg:approval_done', (m) => { approvals.delete(m.id); paintBadge(); });
  on('msg:toast', (m) => toast(m.text, m.level || 'info'));
  on('state', () => {
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
  ta.addEventListener('input', () => store.set('draft.say', ta.value || null));
  ta.addEventListener('paste', (e) => {
    const files = [...(e.clipboardData?.files || [])].filter((f) => f.type.startsWith('image/'));
    if (!files.length) return;
    e.preventDefault();
    files.forEach(addImage);
  });
  const chat = $('#chat');
  chat.addEventListener('dragover', (e) => { if (e.dataTransfer?.types.includes('Files')) e.preventDefault(); });
  chat.addEventListener('drop', (e) => {
    const files = [...(e.dataTransfer?.files || [])].filter((f) => f.type.startsWith('image/'));
    if (!files.length) return;
    e.preventDefault();
    files.forEach(addImage);
  });
  $('#b-img').addEventListener('click', () => $('#f-img').click());
  $('#f-img').addEventListener('change', (e) => { [...e.target.files].forEach(addImage); e.target.value = ''; });
  ta.addEventListener('keydown', (e) => {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && isDesk()) { e.preventDefault(); submit(); }
  });
  $('#composer').addEventListener('submit', (e) => { e.preventDefault(); submit(); });
  $('#b-send').addEventListener('click', (e) => { e.preventDefault(); submit(); ta.focus(); });
  $('#msgs').addEventListener('scroll', (e) => { const m = e.target; stick = m.scrollHeight - m.scrollTop - m.clientHeight < 40; });
  $('#b-chat').addEventListener('click', () => setChat(!chatOpen()));
  $('#b-chat-x').addEventListener('click', () => setChat(false));
  $('#b-appr').addEventListener('click', approvalsSheet);
  if (!$('#msgs').children.length) upsert({ id: 'hi', role: 'system', text: 'Talk to the assistant here: it sees every tab.', done: true });
}
