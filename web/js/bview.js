// The live view of a tab's browser (the Browser section and its big
// dialog): the page the tab's agent drives, as pictures GodTerm sends
// while the view is open (`bview` frames, binary kind 5), with clicks,
// scrolling, back/forward/reload, a URL to open and typing sent back.
// GodTerm never starts a browser session for it on its own: it attaches
// to the session the agent has, or the user presses Start.
import { h, on, toast, icon, isDesk, failRow } from './util.js';
import { send, conn } from './conn.js';

export const KIND_PICTURE = 5;

let seq = 0;
const waits = new Map();
// Views by tab uid: one `open` per tab while any view of it is up.
const views = new Map();
// The newest page facts and picture per tab (a view that joins starts
// with them).
const lastMeta = new Map(), lastPic = new Map();
// GodTerm answers "unknown frame type" when it is older than the view.
let unsupported = false;

on('msg:bview', (m) => {
  const w = m.id != null && waits.get(m.id);
  if (w) { waits.delete(m.id); w(m); }
  if (m.op === 'meta') lastMeta.set(m.tab, m);
  for (const v of views.get(m.tab) || []) v.onFrame(m);
});
on('msg:error', (m) => {
  if (!/unknown frame type "bview"/.test(m.error || '')) return;
  unsupported = true;
  for (const [id, w] of waits) { waits.delete(id); w({ ok: false, unsupported: true }); }
});
on('bin:' + KIND_PICTURE, (uid, bytes) => {
  const vs = views.get(uid);
  if (!vs || !vs.size) return;
  const type = bytes[0] === 0xff && bytes[1] === 0xd8 ? 'image/jpeg' : 'image/png';
  const url = URL.createObjectURL(new Blob([bytes], { type }));
  lastPic.set(uid, url);
  for (const v of vs) v.onPicture(url);
  // Each view keeps its own reference until its next picture.
  setTimeout(() => URL.revokeObjectURL(url), 30000);
});
// Back after a lost connection: the views attach again.
on('conn', (s) => { if (s === 'live') for (const [uid, vs] of views) if (vs.size) openFor(uid); });
document.addEventListener('visibilitychange', () => {
  for (const [uid, vs] of views) {
    if (!vs.size) continue;
    if (document.hidden) send({ t: 'bview', op: 'close', tab: uid });
    else openFor(uid);
  }
});
// Still watching: every 20 s (GodTerm drops a view silent for 90 s).
setInterval(() => {
  for (const [uid, vs] of views) {
    for (const v of [...vs]) if (!v.alive()) v.detach();
    if (vs.size && !document.hidden) send({ t: 'bview', op: 'keep', tab: uid });
  }
}, 1000 * 20);
// A view whose element left the page (the sheet closed) stops at once.
setInterval(() => { for (const [, vs] of views) for (const v of [...vs]) if (!v.alive()) v.detach(); }, 1000);

function ask(frame, ms = 60000) {
  if (unsupported) return Promise.resolve({ ok: false, unsupported: true });
  return new Promise((res) => {
    const id = 'b' + (++seq) + '-' + Date.now();
    waits.set(id, res);
    if (!send({ t: 'bview', id, ...frame })) { waits.delete(id); return res({ ok: false, error: 'not connected to GodTerm' }); }
    setTimeout(() => { if (waits.has(id)) { waits.delete(id); res({ ok: false, error: 'GodTerm did not answer in time' }); } }, ms);
  });
}

async function openFor(uid) {
  const r = await ask({ op: 'open', tab: uid }, 30000);
  for (const v of views.get(uid) || []) v.onOpen(r);
  return r;
}

let styled = false;
function style() {
  if (styled) return;
  styled = true;
  document.head.append(h('style', {}, `
.bv{display:flex;flex-direction:column;gap:8px;min-width:0}
.bv-top{display:flex;align-items:center;gap:6px;min-width:0}
.bv-meta{flex:1;min-width:0;display:flex;flex-direction:column}
.bv-title{font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.bv-url{font-size:12px;color:var(--dim);white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.bv-dot{flex:none;width:8px;height:8px;border-radius:50%;background:var(--dim)}
.bv-dot.live{background:var(--ok)}
.bv-dot.wait{background:var(--busy)}
.bv-dot.err{background:var(--err)}
.bv-tools{display:flex;align-items:center;gap:4px;min-width:0}
.bv-tools input{flex:1;min-width:0;height:36px;padding:0 10px;border:1px solid var(--line);border-radius:8px;background:var(--bg);color:var(--text);font-size:13px}
.bv-screen{position:relative;border:1px solid var(--line);border-radius:8px;overflow:hidden;background:var(--bg);touch-action:none;user-select:none;-webkit-user-select:none;cursor:pointer;min-height:120px;display:grid;place-items:center}
.bv-screen img{display:block;width:100%;height:auto;-webkit-user-drag:none;pointer-events:none}
.bv-screen .bv-empty{padding:24px 12px;color:var(--dim);font-size:13px;text-align:center}
.bv-tap{position:absolute;width:22px;height:22px;margin:-11px 0 0 -11px;border-radius:50%;border:2px solid var(--accent);opacity:.9;pointer-events:none;transition:opacity .5s,transform .5s}
.bv-tap.go{opacity:0;transform:scale(1.8)}
.bv-type{display:flex;gap:4px}
.bv-type input{flex:1;min-width:0;height:36px;padding:0 10px;border:1px solid var(--line);border-radius:8px;background:var(--bg);color:var(--text);font-size:13px}
.bv-state{font-size:12px;color:var(--dim)}
.bv-dlg{position:fixed;inset:0;z-index:50;display:grid;place-items:center;background:var(--scrim)}
.bv-dlg-box{display:flex;flex-direction:column;gap:8px;width:min(1200px,92vw);max-height:92vh;padding:12px;background:var(--panel);border:1px solid var(--line);border-radius:12px;box-shadow:var(--shadow);overflow:hidden}
.bv-dlg-box .bv{flex:1;min-height:0}
.bv-dlg-box .bv-screen{flex:1;min-height:0}
.bv-dlg-box .bv-screen img{width:auto;max-width:100%;max-height:calc(92vh - 190px);margin:auto}
@media (max-width:759px),(max-height:559px){
  .bv-dlg{place-items:stretch;background:var(--panel)}
  .bv-dlg-box{width:100%;max-height:none;height:var(--vh,100dvh);border:0;border-radius:0;padding:calc(8px + var(--sat)) max(10px,var(--sar)) calc(8px + var(--sab)) max(10px,var(--sal))}
  .bv-dlg-box .bv-screen{display:block;overflow:auto;touch-action:none}
  .bv-dlg-box .bv-screen img{width:100%;max-height:none}
}
`));
}

const btn = (ic, label, fn) => h('button.ib.sm', { type: 'button', 'aria-label': label, title: label, onclick: fn, html: icon(ic) });
const ICONS = {
  fwd: '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6l6 6-6 6"/></svg>',
};

// One view of tab `t`'s browser. `big`: the dialog's.
class View {
  constructor(t, big) {
    this.t = t;
    this.uid = typeof t.uid === 'number' ? t.uid : parseInt(String(t.id).replace(/^t/, ''), 10);
    this.big = big;
    this.url = null;
    this.frames = 0;
    this.firstAt = 0;
    this.lastAt = 0;
    style();
    this.dot = h('span.bv-dot.wait');
    this.title = h('div.bv-title', {}, 'Live browser');
    this.urlEl = h('div.bv-url', {}, 'Connecting...');
    this.img = h('img', { alt: 'The browser page', draggable: 'false' });
    this.empty = h('div.bv-empty', {}, 'Waiting for the first picture...');
    this.screen = h('div.bv-screen', { role: 'img', 'aria-label': 'Live browser page: click to click, drag or scroll to scroll' }, this.empty);
    this.addr = h('input', { type: 'url', placeholder: 'Open a web address', enterkeyhint: 'go', autocapitalize: 'off', spellcheck: false });
    this.addr.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); this.go(); } });
    const fwd = h('button.ib.sm', { type: 'button', 'aria-label': 'Forward', title: 'Forward', onclick: () => this.act({ do: 'forward' }), html: ICONS.fwd });
    this.tools = h('div.bv-tools', {}, btn('back', 'Back', () => this.act({ do: 'back' })), fwd,
      btn('reopen', 'Reload', () => this.act({ do: 'reload' })), this.addr, h('button.btn.sm', { type: 'button', onclick: () => this.go() }, 'Go'));
    this.typed = h('input', { type: 'text', placeholder: 'Type into the page', enterkeyhint: 'send', autocapitalize: 'off', spellcheck: false });
    this.typed.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); this.type(); } });
    this.typeRow = h('div.bv-type', {}, this.typed, h('button.btn.sm', { type: 'button', onclick: () => this.type() }, 'Type'),
      h('button.btn.sm', { type: 'button', title: 'Press Enter in the page', onclick: () => this.act({ do: 'key', key: 'Enter' }) }, 'Enter'));
    this.state = h('div.bv-state', {}, '');
    const expand = big ? btn('x', 'Close', () => closeDialog()) : btn('max', 'Expand', () => openDialog(this.t));
    this.top = h('div.bv-top', {}, this.dot, h('div.bv-meta', {}, this.title, this.urlEl), expand);
    this.el = h('div.bv', {}, this.top, this.tools, this.screen, this.typeRow, this.state);
    this.input();
  }

  attach() {
    if (!views.has(this.uid)) views.set(this.uid, new Set());
    const vs = views.get(this.uid);
    const first = !vs.size;
    vs.add(this);
    if (first) openFor(this.uid);
    else {
      if (View.last?.[this.uid]) this.onOpen(View.last[this.uid]);
      if (lastMeta.has(this.uid)) this.onFrame(lastMeta.get(this.uid));
      if (lastPic.has(this.uid)) this.onPicture(lastPic.get(this.uid));
    }
    return this;
  }

  // Still in the page (its slot, or its own element in the dialog).
  alive() { return (this.host || this.el).isConnected; }

  detach() {
    const vs = views.get(this.uid);
    if (!vs || !vs.delete(this)) return;
    if (!vs.size) { send({ t: 'bview', op: 'close', tab: this.uid }); lastPic.delete(this.uid); }
  }

  onOpen(r) {
    (View.last ||= {})[this.uid] = r;
    if (this.onState) this.onState(r);
    if (r.ok === false && !r.state) {
      this.setDot('err');
      this.state.replaceChildren(failRow(r.unsupported ? 'unknown frame type bview' : r.error || 'It did not work.', () => openFor(this.uid), 'The live view'));
      return;
    }
    if (r.state === 'live') {
      this.setDot(this.frames ? 'live' : 'wait');
      this.state.textContent = 'Attached to the agent\'s session' + (r.session ? ' ' + r.session : '') + '.';
    }
  }

  onFrame(m) {
    if (m.op === 'meta') {
      this.title.textContent = m.title || m.url || 'Untitled page';
      this.urlEl.textContent = m.url || '';
      this.url = m.url || '';
      if (document.activeElement !== this.addr) this.addr.value = m.url || '';
    } else if (m.op === 'ended') {
      this.setDot('err');
      this.state.textContent = m.say || 'The session ended.';
      if (this.onState) this.onState({ ok: true, state: 'none', say: 'No active session: the agent closed the browser session.', can_start: false });
    } else if (m.op === 'error' && m.error) {
      this.setDot('err');
      this.state.textContent = m.error;
    }
  }

  onPicture(url) {
    const now = Date.now();
    if (!this.firstAt) this.firstAt = now;
    this.frames++;
    this.lastAt = now;
    this.img.src = url;
    if (this.empty.isConnected) this.screen.replaceChildren(this.img);
    this.setDot('live');
    const secs = (now - this.firstAt) / 1000;
    if (this.frames > 2 && secs > 0) this.state.textContent = `Live · ${(this.frames / secs).toFixed(1)} pictures a second`;
  }

  setDot(c) { this.dot.className = 'bv-dot ' + c; }

  async act(a, quiet) {
    const r = await ask({ op: 'act', tab: this.uid, ...a }, 45000);
    if (r.ok === false) toast(r.unsupported ? 'The live view needs a GodTerm restart' : r.error || 'It did not work', 'warn');
    else if (!quiet && r.say) this.state.textContent = r.say;
    return r;
  }

  go() {
    const u = this.addr.value.trim();
    if (!u) return;
    this.addr.blur();
    this.act({ do: 'navigate', url: u });
  }

  async type() {
    const s = this.typed.value;
    if (!s) return;
    const r = await this.act({ do: 'type', text: s });
    if (r.ok !== false) this.typed.value = '';
  }

  // Click: a tap where it is; drag or the wheel: scroll (fractions of the
  // picture's height, GodTerm turns them into page pixels).
  input() {
    let down = null, dy = 0, dx = 0, flushT = 0;
    const rect = () => this.img.getBoundingClientRect();
    const flush = () => {
      flushT = 0;
      if (Math.abs(dy) < 0.002 && Math.abs(dx) < 0.002) return;
      const a = { do: 'wheel', dy: +dy.toFixed(4), dx: +dx.toFixed(4) };
      dy = dx = 0;
      this.act(a, true);
    };
    const queue = () => { if (!flushT) flushT = setTimeout(flush, 140); };
    this.screen.addEventListener('pointerdown', (e) => {
      if (!this.frames || e.button > 0) return;
      down = { x: e.clientX, y: e.clientY, lx: e.clientX, ly: e.clientY, drag: false, id: e.pointerId };
      try { this.screen.setPointerCapture(e.pointerId); } catch {}
    });
    this.screen.addEventListener('pointermove', (e) => {
      if (!down || e.pointerId !== down.id) return;
      if (!down.drag && Math.hypot(e.clientX - down.x, e.clientY - down.y) > 8) down.drag = true;
      if (!down.drag) return;
      const r = rect();
      dy -= (e.clientY - down.ly) / (r.height || 1);
      dx -= (e.clientX - down.lx) / (r.width || 1);
      down.lx = e.clientX;
      down.ly = e.clientY;
      queue();
    });
    const up = (e) => {
      if (!down || e.pointerId !== down.id) return;
      const d = down;
      down = null;
      if (d.drag || e.type === 'pointercancel') return;
      const r = rect();
      const x = (e.clientX - r.left) / r.width, y = (e.clientY - r.top) / r.height;
      if (!(x >= 0 && x <= 1 && y >= 0 && y <= 1)) return;
      const tap = h('div.bv-tap', { style: `left:${e.clientX - this.screen.getBoundingClientRect().left + this.screen.scrollLeft}px;top:${e.clientY - this.screen.getBoundingClientRect().top + this.screen.scrollTop}px` });
      this.screen.append(tap);
      requestAnimationFrame(() => tap.classList.add('go'));
      setTimeout(() => tap.remove(), 600);
      this.act({ do: 'click', x: +x.toFixed(4), y: +y.toFixed(4) }, true);
    };
    this.screen.addEventListener('pointerup', up);
    this.screen.addEventListener('pointercancel', up);
    this.screen.addEventListener('wheel', (e) => {
      if (!this.frames) return;
      e.preventDefault();
      const r = rect();
      const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? r.height : 1;
      dy += (e.deltaY * k) / (r.height || 1);
      dx += (e.deltaX * k) / (r.width || 1);
      queue();
    }, { passive: false });
  }
}

// The live view for the Browser section: a slot that shows the view when
// the agent has a session, or why not (with Start, which the user presses
// on purpose). `onState(r)` hears every answer (the section keeps its
// cards whatever it is).
export function liveSlot(t) {
  const slot = h('div.list', {}, h('div.mnote', {}, 'Looking for the agent\'s browser session...'));
  if (unsupported) { slot.replaceChildren(h('div.mnote', {}, 'The live view needs a GodTerm restart.')); return slot; }
  const v = new View(t, false);
  let shown = '';
  v.onState = (r) => {
    if (r.unsupported) { shown = 'old'; slot.replaceChildren(h('div.mnote', {}, 'The live browser view needs a GodTerm restart.')); v.detach(); return; }
    if (r.state === 'live') {
      if (shown !== 'live') { shown = 'live'; slot.replaceChildren(h('div.lrow.bv-row', {}, v.el)); }
      return;
    }
    shown = r.state || 'error';
    const kids = [h('div.ct', {}, h('span.dot.' + (r.state === 'none' ? 'approval' : 'error')), h('b', {}, r.state === 'none' ? 'No active session' : 'Live view'),
      h('span.cs', {}, '')), h('div.cs', {}, r.say || r.error || 'It did not work.')];
    if (r.can_start) {
      kids.push(h('div.acts', {}, h('button.btn.sm', { type: 'button', onclick: async (e) => {
        e.currentTarget.disabled = true;
        e.currentTarget.textContent = 'Starting...';
        const s = await ask({ op: 'start', tab: v.uid }, 120000);
        for (const x of views.get(v.uid) || []) x.onOpen(s.ok === false ? { ok: true, state: 'none', say: s.error || 'It did not start.', can_start: true } : s);
      } }, 'Start a session'), h('button.btn.sm', { type: 'button', onclick: () => openFor(v.uid) }, 'Check again')));
    } else {
      kids.push(h('div.acts', {}, h('button.btn.sm', { type: 'button', onclick: () => openFor(v.uid) }, 'Check again')));
    }
    slot.replaceChildren(h('div.lrow', {}, ...kids));
  };
  v.host = slot;
  // Attach once the slot is in the page (the section mounts it next).
  const t0 = Date.now();
  const tick = () => {
    if (slot.isConnected) v.attach();
    else if (Date.now() - t0 < 5000) requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
  return slot;
}

// The big view: a dialog on a desktop, the whole screen on a phone.
let dlg = null;
function openDialog(t) {
  closeDialog();
  const v = new View(t, true);
  const box = h('div.bv-dlg-box', {}, v.el);
  dlg = { el: h('div.bv-dlg', { role: 'dialog', 'aria-modal': 'true', 'aria-label': 'Live browser', onclick: (e) => { if (e.target === dlg?.el) closeDialog(); } }, box), v };
  document.body.append(dlg.el);
  v.attach();
  addEventListener('keydown', escClose, true);
  if (!isDesk()) history.pushState({ bview: 1 }, '');
}
function escClose(e) { if (e.key === 'Escape' && dlg) { e.stopPropagation(); closeDialog(); } }
function closeDialog() {
  if (!dlg) return;
  const d = dlg;
  dlg = null;
  removeEventListener('keydown', escClose, true);
  d.el.remove();
  d.v.detach();
}
addEventListener('popstate', () => { if (dlg) closeDialog(); });
export { closeDialog };
export const _views = views;
