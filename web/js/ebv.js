import { h, on, icon, store, uidOf, isDesk } from './util.js';
import { send } from './conn.js';
import { S } from './state.js';
import { openExternal } from './ebv_open.js';
import { showContextMenu, copyText } from './ebv_menu.js';

export const KIND_FRAME = 7;
const MAX_TABS = 6;

export const ebvAvailable = () => S.embedded_browser === true;
export const ebvOpen = (id) => Array.isArray(S.embedded_open) && S.embedded_open.includes(uidOf(id));
export const ebvMode = (id) => store.get('ebv.' + id, null);
export const ebvShown = (id) => !!id && ebvAvailable() && (ebvMode(id) === 'on' || (ebvMode(id) !== 'off' && ebvOpen(id)));

const all = new Set();
const byUid = new Map();
const waits = new Map();
let seq = 0;

function ask(frame, ms = 30000) {
  return new Promise((res) => {
    const id = 'e' + (++seq) + '-' + Date.now();
    waits.set(id, res);
    if (!send({ t: 'ebv', id, ...frame })) { waits.delete(id); return res({ ok: false, error: 'not connected to GodTerm' }); }
    setTimeout(() => { if (waits.has(id)) { waits.delete(id); res({ ok: false, error: 'GodTerm did not answer in time' }); } }, ms);
  });
}

on('msg:ebv', (m) => {
  const w = m.id != null && waits.get(m.id);
  if (w) { waits.delete(m.id); w(m); }
  for (const v of byUid.get(m.tab) || []) v.onFrame(m);
});

on('bin:' + KIND_FRAME, (uid, bytes) => {
  const vs = byUid.get(uid);
  if (!vs || !vs.size) return;
  const url = URL.createObjectURL(new Blob([bytes], { type: 'image/jpeg' }));
  for (const v of vs) v.onPicture(url);
  setTimeout(() => URL.revokeObjectURL(url), 20000);
});

on('state', () => { for (const v of all) v.sync(); });
on('conn', (s) => { if (s === 'live') for (const v of all) v.reattach(); });
document.addEventListener('visibilitychange', () => { for (const v of all) v.visibility(); });
setInterval(() => { if (!document.hidden) for (const v of all) v.keepalive(); }, 20000);

export function setEbvMode(id, mode) {
  store.set('ebv.' + id, mode);
  for (const v of all) if (v.pane.id === id) v.sync();
}

export function toggleEbv(id) {
  if (!id) return;
  const was = ebvShown(id);
  setEbvMode(id, was ? 'off' : 'on');
}

export function stopEbv(id) {
  send({ t: 'ebv', op: 'kill', tab: uidOf(id) });
  setEbvMode(id, 'off');
}

let styled = false;
function style() {
  if (styled) return;
  styled = true;
  document.head.append(h('style', {}, `
.pane-split{flex:1;min-height:0;display:flex;flex-direction:row;min-width:0}
.pane-split>.pane-body{flex:1;min-width:0}
.ebv-grip{flex:none;width:6px;cursor:col-resize;background:var(--line);touch-action:none}
.ebv-grip:hover,.ebv-grip.drag{background:var(--accent)}
.ebv{flex:none;display:flex;flex-direction:column;min-width:0;min-height:0;width:var(--ebvw,46%);background:var(--panel);border-left:1px solid var(--line)}
.ebv[hidden],.ebv-grip[hidden]{display:none}
.ebv{position:relative}
.ebv-tabs{flex:none;display:flex;align-items:flex-end;gap:2px;padding:4px 4px 0;border-bottom:1px solid var(--line);min-width:0}
.ebv-tablist{position:relative;flex:1;min-width:0;display:flex;gap:2px;overflow-x:auto;scrollbar-width:none;-webkit-overflow-scrolling:touch}
.ebv-tablist::-webkit-scrollbar{display:none}
.ebv-tab{flex:0 0 auto;display:flex;align-items:center;width:150px;max-width:60vw;height:30px;border:1px solid transparent;border-bottom:0;border-radius:8px 8px 0 0;color:var(--dim)}
.ebv-tab.on{background:var(--bg);border-color:var(--line);color:var(--text)}
.ebv-tab:not(.on):hover{background:var(--panel2)}
.ebv-tab-b{flex:1;min-width:0;height:100%;padding:0 4px 0 10px;border:0;background:none;color:inherit;font-size:12.5px;text-align:left;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.ebv-tab.asleep .ebv-tab-b{opacity:.55;font-style:italic}
.ebv-tab-x{flex:none;display:grid;place-items:center;width:24px;height:24px;margin-right:3px;border:0;border-radius:6px;background:none;color:inherit}
.ebv-tab-x:hover{background:var(--panel2)}
.ebv-tab-x svg{width:12px;height:12px}
.ebv-tabs>.ib{flex:none;min-width:30px;height:30px;margin-bottom:1px}
.ebv-tabs>.ib svg{width:16px;height:16px}
.ebv-tabs>.ib[disabled]{opacity:.35}
.ebv-bar{display:flex;align-items:center;gap:2px;padding:4px;border-bottom:1px solid var(--line);min-width:0}
.ebv-bar .ib{flex:none;min-width:30px;height:30px}
.ebv-bar .ib[disabled]{opacity:.35}
.ebv-bar .ib svg{width:16px;height:16px}
.ebv-addr{flex:1;min-width:0;height:30px;padding:0 10px;border:1px solid var(--line);border-radius:8px;background:var(--bg);color:var(--text);font-size:13px}
.ebv-prog{height:2px;background:transparent;flex:none;overflow:hidden}
.ebv-prog i{display:block;height:100%;width:30%;background:var(--accent);opacity:0;transform:translateX(-100%)}
.ebv.loading .ebv-prog i{opacity:.8;animation:ebvp 1.1s ease-in-out infinite}
@keyframes ebvp{0%{transform:translateX(-100%)}100%{transform:translateX(340%)}}
.ebv-note{flex:none;padding:6px 10px;font-size:12.5px;color:var(--err);border-bottom:1px solid var(--line);overflow-wrap:anywhere}
.ebv-note[hidden]{display:none}
.ebv-screen{position:relative;flex:1;min-height:0;overflow:hidden;background:var(--bg);touch-action:none;user-select:none;-webkit-user-select:none;outline:none;cursor:default}
.ebv-screen:focus-visible{box-shadow:inset 0 0 0 2px var(--ring)}
.ebv-screen img{position:absolute;inset:0;width:100%;height:100%;display:block;-webkit-user-drag:none;pointer-events:none;object-fit:fill}
.ebv-msg{position:absolute;inset:0;display:grid;place-items:center;align-content:center;gap:10px;padding:20px;text-align:center;color:var(--dim);font-size:13px}
.ebv-msg[hidden]{display:none}
.ebv-src{position:absolute;inset:0;z-index:5;display:flex;flex-direction:column;background:var(--bg)}
.ebv-src[hidden]{display:none}
.ebv-src-head{flex:none;display:flex;align-items:center;gap:6px;padding:6px 8px;border-bottom:1px solid var(--line);min-width:0}
.ebv-src-t{flex:1;min-width:0;font-size:12.5px;color:var(--dim);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.ebv-src pre{flex:1;min-height:0;margin:0;padding:10px;overflow:auto;font:12px/1.5 ui-monospace,Menlo,Consolas,monospace;color:var(--text);white-space:pre-wrap;overflow-wrap:anywhere;user-select:text;-webkit-user-select:text;tab-size:2}
.ebv-kb{position:absolute;left:0;bottom:0;width:1px;height:1px;opacity:0;border:0;padding:0;resize:none;pointer-events:none}
@media (max-width:759px),(max-height:559px){
  .pane-split{flex-direction:column}
  .ebv{width:auto;height:var(--ebvh,52%);border-left:0;border-top:1px solid var(--line)}
  .ebv-grip{width:auto;height:8px;cursor:row-resize}
  .ebv-bar .ib{min-width:40px;height:40px}
  .ebv-tab{height:38px;width:128px}
  .ebv-tab-x{width:34px;height:34px;margin-right:2px}
  .ebv-tabs>.ib{min-width:40px;height:38px}
  .ebv-addr{height:38px;font-size:15px}
}
`));
}

const btn = (ic, label, fn, extra = {}) => h('button.ib', { type: 'button', 'aria-label': label, title: label, onclick: fn, html: icon(ic), ...extra });

export class Ebv {
  constructor(pane) {
    style();
    this.pane = pane;
    this.uid = 0;
    this.attached = false;
    this.opening = false;
    this.vp = { w: 0, h: 0 };
    this.sentSize = { w: 0, h: 0 };
    this.meta = { url: '', title: '', loading: false, back: false, fwd: false };
    this.back = btn('back', 'Back', () => this.op('back'));
    this.fwd = btn('arrowr', 'Forward', () => this.op('forward'));
    this.reloadB = btn('reload', 'Reload', () => this.op(this.meta.loading ? 'stop' : 'reload'));
    this.addr = h('input.ebv-addr', { type: 'text', placeholder: 'localhost:3000 or an address', autocomplete: 'off', autocapitalize: 'off', spellcheck: false, enterkeyhint: 'go', 'aria-label': 'Address' });
    this.kbB = btn('kbdown', 'Keyboard', () => this.kb.focus());
    this.kbB.hidden = isDesk();
    this.stopB = btn('stop', 'Stop the browser', () => stopEbv(this.pane.id));
    this.hideB = btn('x', 'Hide the browser', () => setEbvMode(this.pane.id, 'off'));
    this.bar = h('div.ebv-bar', {}, this.back, this.fwd, this.reloadB, this.addr, openExternal(() => this.meta.url), this.kbB, this.stopB, this.hideB);
    this.tabList = h('div.ebv-tablist', { role: 'tablist', 'aria-label': 'Browser tabs' });
    this.newTab = btn('plus', 'New tab', () => this.openTab());
    this.tabBar = h('div.ebv-tabs', {}, this.tabList, this.newTab);
    this.tabSig = '';
    this.activeTab = 0;
    this.prog = h('div.ebv-prog', {}, h('i'));
    this.note = h('div.ebv-note', { hidden: true, role: 'status' });
    this.img = h('img', { alt: 'The embedded browser', draggable: false });
    this.msg = h('div.ebv-msg', {}, 'Starting the browser');
    this.kb = h('textarea.ebv-kb', { autocapitalize: 'off', autocomplete: 'off', spellcheck: false, 'aria-label': 'Type into the page', tabindex: '-1' });
    this.screen = h('div.ebv-screen', { tabindex: '0', role: 'application', 'aria-label': 'The embedded browser page' }, this.img, this.msg, this.kb);
    this.srcT = h('div.ebv-src-t', {});
    this.srcPre = h('pre', { tabindex: '0', 'aria-label': 'Page source' });
    this.srcCopy = h('button.btn', { type: 'button', onclick: () => copyText(this, this.srcText || '', 'Source copied') }, 'Copy');
    this.srcClose = h('button.btn', { type: 'button', onclick: () => this.closeSource() }, 'Close');
    this.src = h('div.ebv-src', { hidden: true, role: 'dialog', 'aria-label': 'Page source' }, h('div.ebv-src-head', {}, this.srcT, this.srcCopy, this.srcClose), this.srcPre);
    this.src.addEventListener('keydown', (e) => { if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); this.closeSource(); } });
    this.tabCount = 1;
    this.el = h('div.ebv', { hidden: true }, this.tabBar, this.bar, this.prog, this.note, this.screen, this.src);
    this.grip = h('div.ebv-grip', { role: 'separator', 'aria-orientation': 'vertical', 'aria-label': 'Resize the browser', tabindex: '0', hidden: true });
    this.wire();
    all.add(this);
    new ResizeObserver(() => this.resized()).observe(this.screen);
  }

  ask(frame, ms) { return ask({ tab: this.uid, ...frame }, ms); }

  go(url) {
    return ask({ op: 'go', tab: this.uid, url }, 40000).then((m) => { if (m.ok === false) this.say(m.error || 'That did not open'); else this.say(''); return m; });
  }

  openInTab(url) { return this.tabAsk('tab_new', { url }); }

  async viewSource() {
    const m = await this.ask({ op: 'source' }, 20000);
    if (m.ok === false) return this.say(m.error || 'The page source could not be read');
    this.srcText = (m.html || '') + (m.truncated ? '\n\n[cut: the page is longer than this view]' : '');
    this.srcT.textContent = 'Source of ' + (this.meta.url || 'the page');
    this.srcPre.textContent = this.srcText;
    this.srcPre.scrollTop = 0;
    this.src.hidden = false;
    this.srcClose.focus();
  }

  closeSource() {
    this.src.hidden = true;
    this.srcPre.textContent = '';
    this.srcText = '';
    this.screen.focus({ preventScroll: true });
  }

  op(op, extra = {}) { if (this.uid) send({ t: 'ebv', op, tab: this.uid, ...extra }); }

  say(text, ms = 7000) {
    clearTimeout(this.noteT);
    this.note.textContent = text || '';
    this.note.hidden = !text;
    if (text) this.noteT = setTimeout(() => { this.note.hidden = true; }, ms);
  }

  sync() {
    const id = this.pane.id;
    const want = ebvShown(id);
    const uid = id ? uidOf(id) : 0;
    if (this.uid && this.uid !== uid) this.detach();
    this.uid = uid;
    this.el.hidden = !want;
    this.grip.hidden = !want;
    if (!want) { this.detach(); }
    else if (!this.attached && !this.opening && !document.hidden) this.attach();
    else if (this.attached && !ebvOpen(id) && !this.opening) { this.attached = false; this.attach(); }
    this.pane.ebvPaint?.();
  }

  visibility() {
    if (!this.uid || this.el.hidden) return;
    if (document.hidden) this.detach(true); else this.attach();
  }

  reattach() { if (!this.el.hidden && this.uid) { this.attached = false; this.opening = false; this.attach(); } }

  keepalive() { if (this.attached) this.op('keep'); }

  register() {
    if (!byUid.has(this.uid)) byUid.set(this.uid, new Set());
    byUid.get(this.uid).add(this);
  }

  unregister() {
    for (const [u, s] of byUid) { s.delete(this); if (!s.size) byUid.delete(u); }
  }

  async attach() {
    if (!this.uid || this.opening) return;
    this.opening = true;
    this.register();
    this.msg.hidden = false;
    this.msg.textContent = 'Starting the browser';
    const r = this.screen.getBoundingClientRect();
    const w = Math.round(r.width), h2 = Math.round(r.height);
    this.sentSize = { w, h: h2 };
    const m = await ask({ op: 'open', tab: this.uid, w, h: h2 });
    this.opening = false;
    if (this.el.hidden) { this.op('close'); return; }
    if (m.ok === false) {
      this.attached = false;
      this.msg.hidden = false;
      this.msg.replaceChildren(h('span', {}, m.error || 'The browser did not start'), h('button.btn', { type: 'button', onclick: () => this.attach() }, 'Try again'));
      return;
    }
    this.attached = true;
    this.msg.hidden = !!this.img.src && !!this.hasFrame;
    if (!this.hasFrame) this.msg.textContent = 'Waiting for the page';
    this.paintMeta(m);
  }

  detach(keepMode) {
    if (this.attached || this.opening) this.op('close');
    this.attached = false;
    this.opening = false;
    this.hasFrame = false;
    if (!keepMode) this.unregister();
  }

  onFrame(m) {
    if (m.op === 'meta') this.paintMeta(m);
    else if (m.op === 'ended') {
      this.attached = false;
      this.hasFrame = false;
      this.msg.hidden = false;
      this.msg.replaceChildren(h('span', {}, 'The browser closed'), h('button.btn', { type: 'button', onclick: () => this.attach() }, 'Start it again'));
    }
  }

  onPicture(url) {
    this.hasFrame = true;
    this.msg.hidden = true;
    const old = this.img.src;
    this.img.src = url;
    if (old && old.startsWith('blob:')) this.img.addEventListener('load', () => URL.revokeObjectURL(old), { once: true });
  }

  tabLabel(t) {
    const u = t.url || '';
    if (!u || u === 'about:blank') return 'New tab';
    if (t.title) return t.title;
    try { return new URL(u).host || u; } catch { return u; }
  }

  renderTabs(tabs, active) {
    const sig = JSON.stringify([tabs.map((t) => [t.id, t.title, t.url, !!t.asleep]), active]);
    if (sig === this.tabSig) return;
    this.tabSig = sig;
    this.activeTab = active;
    this.tabList.replaceChildren(...tabs.map((t) => {
      const label = this.tabLabel(t);
      const on = t.id === active;
      const b = h('button.ebv-tab-b', { type: 'button', role: 'tab', 'aria-selected': on ? 'true' : 'false', tabindex: on ? '0' : '-1', title: (t.url && t.url !== 'about:blank' ? label + '\n' + t.url : label) + (t.asleep ? '\nAsleep to save memory. Click to wake it (the page reloads).' : ''), onclick: () => this.pickTab(t.id, !!t.asleep) }, label);
      b.addEventListener('auxclick', (e) => { if (e.button === 1) { e.preventDefault(); this.closeTab(t.id); } });
      b.addEventListener('keydown', (e) => {
        if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight') return;
        const i = tabs.findIndex((x) => x.id === t.id) + (e.key === 'ArrowRight' ? 1 : -1);
        const n = tabs[(i + tabs.length) % tabs.length];
        if (n && n.id !== t.id) { e.preventDefault(); this.pickTab(n.id, !!n.asleep); }
      });
      const x = h('button.ebv-tab-x', { type: 'button', 'aria-label': 'Close tab ' + label, title: 'Close tab', onclick: (e) => { e.stopPropagation(); this.closeTab(t.id); }, html: icon('x') });
      const el = h('div.ebv-tab', { 'data-id': String(t.id) }, b, x);
      el.classList.toggle('on', on);
      el.classList.toggle('asleep', !!t.asleep);
      return el;
    }));
    this.tabCount = tabs.length;
    this.newTab.disabled = tabs.length >= MAX_TABS;
    const a = this.tabList.querySelector('.ebv-tab.on');
    if (a) {
      const l = this.tabList;
      if (a.offsetLeft < l.scrollLeft) l.scrollLeft = a.offsetLeft;
      else if (a.offsetLeft + a.offsetWidth > l.scrollLeft + l.clientWidth) l.scrollLeft = a.offsetLeft + a.offsetWidth - l.clientWidth;
    }
  }

  tabAsk(op, extra = {}) {
    return ask({ op, tab: this.uid, ...extra }, 30000).then((m) => {
      if (m.ok === false) this.say(m.error || 'That did not work');
      else { this.say(''); this.paintMeta(m); }
      return m;
    });
  }

  pickTab(id, asleep) {
    if (!this.uid || (id === this.activeTab && !asleep)) return;
    this.tabSig = '';
    this.tabAsk('tab_switch', { page: id });
  }

  closeTab(id) { if (this.uid) this.tabAsk('tab_close', { page: id }); }

  openTab() {
    if (!this.uid) return;
    this.tabAsk('tab_new').then((m) => { if (m.ok !== false && isDesk()) this.addr.focus(); });
  }

  paintMeta(m) {
    if (Array.isArray(m.tabs)) this.renderTabs(m.tabs, m.active);
    this.meta = { url: m.url || '', title: m.title || '', loading: !!m.loading, back: !!m.back, fwd: !!m.fwd };
    if (document.activeElement !== this.addr) this.addr.value = this.meta.url === 'about:blank' ? '' : this.meta.url;
    this.back.disabled = !this.meta.back;
    this.fwd.disabled = !this.meta.fwd;
    this.el.classList.toggle('loading', this.meta.loading);
    this.reloadB.title = this.reloadB.ariaLabel = this.meta.loading ? 'Stop' : 'Reload';
    if (m.w && m.h) this.vp = { w: m.w, h: m.h };
  }

  resized() {
    if (!this.attached || this.el.hidden) return;
    clearTimeout(this.rsT);
    this.rsT = setTimeout(() => {
      const r = this.screen.getBoundingClientRect();
      const w = Math.round(r.width), h2 = Math.round(r.height);
      if (w < 160 || h2 < 120) return;
      if (Math.abs(w - this.sentSize.w) < 4 && Math.abs(h2 - this.sentSize.h) < 4) return;
      this.sentSize = { w, h: h2 };
      this.op('resize', { w, h: h2 });
    }, 250);
  }

  frac(e) {
    const r = this.screen.getBoundingClientRect();
    return { fx: Math.min(1, Math.max(0, (e.clientX - r.left) / Math.max(1, r.width))), fy: Math.min(1, Math.max(0, (e.clientY - r.top) / Math.max(1, r.height))) };
  }

  mouse(kind, e, extra = {}) {
    if (!this.attached) return;
    this.op('mouse', { kind, ...this.frac(e), button: ['left', 'middle', 'right'][e.button] || 'left', cc: e.detail || 1, ...extra });
  }

  wire() {
    this.addr.addEventListener('keydown', (e) => {
      if (e.key !== 'Enter') return;
      e.preventDefault();
      const url = this.addr.value.trim();
      if (!url) return;
      this.addr.blur();
      this.go(url);
    });
    this.addr.addEventListener('focus', () => this.addr.select());
    this.addr.addEventListener('blur', () => { this.addr.value = this.meta.url === 'about:blank' ? '' : this.meta.url; });

    const s = this.screen;
    const pts = new Map();
    let last = 0;
    const mac = /Mac|iPhone|iPad/.test(navigator.userAgent);
    let lastCtx = 0;
    let touchAt = 0;
    const ctx = (x, y) => { lastCtx = performance.now(); showContextMenu(this, x, y); };
    s.addEventListener('contextmenu', (e) => {
      e.preventDefault();
      if (performance.now() - touchAt < 1500 && performance.now() - lastCtx < 900) return;
      ctx(e.clientX, e.clientY);
    });
    const press = (p) => { clearTimeout(p.timer); };
    s.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      s.focus({ preventScroll: true });
      try { s.setPointerCapture?.(e.pointerId); } catch {}
      if (e.pointerType === 'touch') {
        touchAt = performance.now();
        for (const q of pts.values()) press(q);
        const p = { x: e.clientX, y: e.clientY, sx: e.clientX, sy: e.clientY, moved: false, long: false };
        if (pts.size === 0) {
          p.timer = setTimeout(() => {
            if (p.moved || !pts.has(e.pointerId) || pts.size > 1 || performance.now() - lastCtx < 900) return;
            p.long = true;
            ctx(p.sx, p.sy);
          }, 500);
        }
        pts.set(e.pointerId, p);
        return;
      }
      if (e.button === 2 || (mac && e.button === 0 && e.ctrlKey)) return;
      this.mouse('move', e, { cc: 0, button: 'none' });
      this.mouse('down', e);
    });
    s.addEventListener('pointermove', (e) => {
      const p = pts.get(e.pointerId);
      if (p) {
        const dx = e.clientX - p.x, dy = e.clientY - p.y;
        if (!p.moved && Math.hypot(e.clientX - p.sx, e.clientY - p.sy) < 8) return;
        press(p);
        if (p.long) return;
        p.moved = true;
        p.x = e.clientX; p.y = e.clientY;
        const r = s.getBoundingClientRect();
        const k = (this.vp.w || r.width) / Math.max(1, r.width);
        this.op('mouse', { kind: 'wheel', ...this.frac(e), dx: -dx * k, dy: -dy * k });
        return;
      }
      const now = performance.now();
      if (!e.buttons && now - last < 45) return;
      last = now;
      this.mouse('move', e, { cc: 0, button: e.buttons ? 'left' : 'none' });
    });
    const up = (e) => {
      const p = pts.get(e.pointerId);
      if (p) {
        press(p);
        pts.delete(e.pointerId);
        if (!p.moved && !p.long && e.type === 'pointerup') { this.mouse('move', e, { cc: 0, button: 'none' }); this.mouse('down', e, { button: 'left' }); this.mouse('up', e, { button: 'left' }); }
        return;
      }
      if (e.type === 'pointerup' && !(e.button === 2 || (mac && e.button === 0 && e.ctrlKey))) { this.mouse('move', e, { cc: 0, button: 'left' }); this.mouse('up', e); }
    };
    s.addEventListener('pointerup', up);
    s.addEventListener('pointercancel', up);
    s.addEventListener('wheel', (e) => {
      e.preventDefault();
      const k = e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1;
      this.op('mouse', { kind: 'wheel', ...this.frac(e), dx: e.deltaX * k, dy: e.deltaY * k });
    }, { passive: false });

    const mods = (e) => (e.altKey ? 1 : 0) | (e.ctrlKey ? 2 : 0) | (e.metaKey ? 4 : 0) | (e.shiftKey ? 8 : 0);
    s.addEventListener('keydown', (e) => {
      if (!this.attached || e.isComposing || ['Shift', 'Control', 'Alt', 'Meta', 'CapsLock', 'Dead'].includes(e.key)) return;
      if ((e.ctrlKey || e.metaKey) && (e.key === 'v' || e.key === 'V')) return;
      e.preventDefault();
      e.stopPropagation();
      this.op('key', { key: e.key, code: e.code, mods: mods(e), text: e.key.length === 1 ? e.key : undefined });
    });
    s.addEventListener('paste', (e) => {
      const t = e.clipboardData?.getData('text/plain');
      if (t) { e.preventDefault(); this.op('text', { text: t.slice(0, 20000) }); }
    });

    const kb = this.kb;
    kb.addEventListener('beforeinput', (e) => {
      if (e.inputType === 'insertText' && e.data) { e.preventDefault(); this.op('text', { text: e.data }); }
      else if (e.inputType === 'insertFromPaste' || e.inputType === 'insertReplacementText') { e.preventDefault(); if (e.data) this.op('text', { text: e.data }); }
      else if (e.inputType === 'deleteContentBackward') { e.preventDefault(); this.op('key', { key: 'Backspace', code: 'Backspace', mods: 0 }); }
      else if (e.inputType === 'insertLineBreak' || e.inputType === 'insertParagraph') { e.preventDefault(); this.op('key', { key: 'Enter', code: 'Enter', mods: 0 }); }
    });
    kb.addEventListener('input', () => { if (!kb.value) return; const t = kb.value; kb.value = ''; this.op('text', { text: t }); });
    kb.addEventListener('keydown', (e) => {
      if (['Enter', 'Backspace', 'Tab', 'Escape', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'].includes(e.key) && !e.isComposing) {
        e.preventDefault();
        this.op('key', { key: e.key, code: e.code, mods: mods(e) });
      }
    });

    let d = null;
    const grip = this.grip;
    const phone = () => matchMedia('(max-width:759px),(max-height:559px)').matches;
    grip.addEventListener('pointerdown', (e) => {
      e.preventDefault();
      grip.setPointerCapture(e.pointerId);
      grip.classList.add('drag');
      const r = this.el.getBoundingClientRect(), host = this.el.parentElement.getBoundingClientRect();
      d = { x: e.clientX, y: e.clientY, w: r.width, h: r.height, hw: host.width, hh: host.height };
      document.documentElement.classList.add('side-drag');
    });
    grip.addEventListener('pointermove', (e) => {
      if (!d) return;
      if (phone()) this.setSize('h', d.h - (e.clientY - d.y), d.hh);
      else this.setSize('w', d.w - (e.clientX - d.x), d.hw);
    });
    const end = () => {
      if (!d) return;
      d = null;
      grip.classList.remove('drag');
      document.documentElement.classList.remove('side-drag');
      requestAnimationFrame(() => this.pane.layout());
    };
    grip.addEventListener('pointerup', end);
    grip.addEventListener('pointercancel', end);
    grip.addEventListener('dblclick', () => { document.documentElement.style.removeProperty('--ebvw'); document.documentElement.style.removeProperty('--ebvh'); store.set('ebvw', null); store.set('ebvh', null); this.pane.layout(); });
    grip.addEventListener('keydown', (e) => {
      if (e.key !== 'ArrowLeft' && e.key !== 'ArrowRight' && e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return;
      e.preventDefault();
      const r = this.el.getBoundingClientRect(), host = this.el.parentElement.getBoundingClientRect();
      const grow = e.key === 'ArrowLeft' || e.key === 'ArrowUp' ? 24 : -24;
      if (phone()) this.setSize('h', r.height + grow, host.height); else this.setSize('w', r.width + grow, host.width);
      this.pane.layout();
    });
    const saved = { w: store.get('ebvw', null), h: store.get('ebvh', null) };
    if (saved.w) document.documentElement.style.setProperty('--ebvw', saved.w);
    if (saved.h) document.documentElement.style.setProperty('--ebvh', saved.h);
  }

  setSize(axis, px, total) {
    const min = axis === 'w' ? 260 : 160;
    const v = Math.round(Math.min(Math.max(px, min), Math.max(min, total - (axis === 'w' ? 200 : 110))) / Math.max(1, total) * 1000) / 10 + '%';
    document.documentElement.style.setProperty(axis === 'w' ? '--ebvw' : '--ebvh', v);
    store.set(axis === 'w' ? 'ebvw' : 'ebvh', v);
  }
}
