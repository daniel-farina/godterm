// Push notifications (docs/WEB_APP.md, "Push notifications"): Manage >
// This browser > Notifications subscribes this device with the push
// service and hands the subscription to GodTerm (`push` frames), which
// then notifies it when a tab waits for approval, finishes or ends with
// an error. Permission is asked only from a tap. While the page is
// visible it says so every 15 s, so GodTerm does not notify a device
// that is looking. A tapped notification opens `#tab=12`, `#approvals`
// or both; this module takes the hash (also from the service worker's
// message when the app is already open) and shows that tab or sheet.
import { h, on, emit, toast, store, li } from './util.js';
import { send, conn } from './conn.js';
import { device, standalone, dlog } from './debuglog.js';
import { S, tabById } from './state.js';

const KINDS = [['approval', 'Approvals'], ['finished', 'Finished turns'], ['error', 'Errors']];
const ios = () => /iP(hone|ad|od)/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
const st = { on: false, sub: '', kinds: store.get('push-kinds', KINDS.map((k) => k[0])), busy: false, key: null };
let box = null, seq = 0;

// Requests to GodTerm: `push` frames, answered with the same id.
const waiting = new Map();
on('msg:push', (m) => { const p = waiting.get(m.id); if (p) { waiting.delete(m.id); clearTimeout(p.t); p.resolve(m); } });
function ask(op, args = {}, ms = 15000) {
  const id = 'p' + ++seq;
  return new Promise((resolve) => {
    if (!send({ t: 'push', op, id, ...args })) return resolve({ ok: false, error: 'not connected' });
    const t = setTimeout(() => { waiting.delete(id); resolve({ ok: false, error: 'GodTerm did not answer' }); }, ms);
    waiting.set(id, { resolve, t });
  });
}

// Why notifications cannot work here, or '' when they can.
function why() {
  if (!window.isSecureContext) return 'Needs the https link (tailscale serve): a plain http page cannot get notifications';
  const has = 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;
  if (ios() && !standalone()) return 'On iPhone and iPad, add GodTerm to the Home Screen (Share > Add to Home Screen) and open it from there';
  if (!has) return 'This browser has no web push';
  if (Notification.permission === 'denied') return ios() ? 'Blocked: allow them in Settings > Notifications > GodTerm' : 'Blocked: allow notifications for this site in the browser settings';
  return '';
}

const b64 = (s) => { const t = s.replace(/-/g, '+').replace(/_/g, '/'); const r = atob(t + '='.repeat((4 - (t.length % 4)) % 4)); return Uint8Array.from(r, (c) => c.charCodeAt(0)); };
const b64u = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const ready = () => Promise.race([navigator.serviceWorker.ready, new Promise((_, no) => setTimeout(() => no(new Error('the service worker is not running')), 8000))]);
async function key() {
  if (st.key) return st.key;
  const r = await ask('key');
  if (!r.ok) throw new Error(r.error || 'no key');
  st.key = r.key;
  return r.key;
}

function took(r) {
  st.on = !!r.subscribed;
  st.sub = r.sub || '';
  if (Array.isArray(r.kinds)) { st.kinds = r.kinds; store.set('push-kinds', r.kinds); }
}

// On every connect: a subscription this device has goes to GodTerm again
// (it keeps the kinds chosen), so a new Mac side file or a rotated
// subscription heals itself.
async function sync() {
  if (why() || Notification.permission !== 'granted') { st.on = false; return paint(); }
  try {
    const reg = await ready();
    let s = await reg.pushManager.getSubscription();
    const k = await key().catch(() => null);
    // Made for another key (GodTerm's key file was replaced): renew.
    if (s && k && s.options?.applicationServerKey && b64u(s.options.applicationServerKey) !== k) {
      await s.unsubscribe().catch(() => {});
      s = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64(k) });
    }
    if (!s) { st.on = false; return paint(); }
    took(await ask('subscribe', { sub: s.toJSON(), device: device() }));
    visible();
  } catch (e) { dlog('push', 'sync: ' + e.message); }
  paint();
}

// The toggle, from a tap: the permission prompt comes first (iOS wants
// it straight from the gesture), then the subscription.
async function enable() {
  const no = why();
  if (no) { toast(no, 'warn'); return paint(); }
  st.busy = true; paint();
  try {
    let perm = Notification.permission;
    if (perm !== 'granted') perm = await Notification.requestPermission();
    dlog('push', 'permission ' + perm);
    if (perm !== 'granted') throw new Error(perm === 'denied' ? why() || 'Notifications are blocked' : 'Notifications were not allowed');
    const k = await key();
    const reg = await ready();
    let s = await reg.pushManager.getSubscription();
    if (!s) s = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64(k) });
    const r = await ask('subscribe', { sub: s.toJSON(), device: device(), kinds: st.kinds });
    if (!r.ok) throw new Error(r.error || 'GodTerm refused the subscription');
    took(r);
    visible();
    toast('Notifications on for this device', 'ok');
  } catch (e) {
    dlog('push', 'enable failed: ' + e.message);
    toast(e.message || 'Notifications could not be turned on', 'error');
  }
  st.busy = false; paint();
}

async function disable() {
  st.busy = true; paint();
  try {
    const s = await (await ready()).pushManager.getSubscription();
    if (s) { await ask('unsubscribe', { endpoint: s.endpoint }); await s.unsubscribe().catch(() => {}); }
    else if (st.sub) await ask('unsubscribe', { sub: st.sub });
    st.on = false; st.sub = '';
    toast('Notifications off for this device');
  } catch (e) { toast(e.message || 'It did not work', 'error'); }
  st.busy = false; paint();
}

async function setKind(k, onOff) {
  st.kinds = KINDS.map((x) => x[0]).filter((x) => (x === k ? onOff : st.kinds.includes(x)));
  store.set('push-kinds', st.kinds);
  if (!st.on) return;
  const s = await (await ready()).pushManager.getSubscription();
  if (s) took(await ask('subscribe', { sub: s.toJSON(), device: device(), kinds: st.kinds }));
  paint();
}

async function test() {
  if (!st.sub) return toast('Turn notifications on first', 'warn');
  toast('Sending a test notification');
  const r = await ask('test', { sub: st.sub }, 20000);
  if (r.ok) toast('Sent: it should arrive in a few seconds', 'ok');
  else { toast(r.error || 'The test did not go out', 'error'); if (/no longer knows|not subscribed/.test(r.error || '')) { st.on = false; paint(); } }
}

// "This page is looking": no push for this device meanwhile.
function visible() {
  if (st.sub && conn.open) send({ t: 'push', op: 'visible', on: !document.hidden, sub: st.sub });
}
document.addEventListener('visibilitychange', visible);
setInterval(() => { if (!document.hidden) visible(); }, 15000);
on('conn', (s) => { if (s === 'live') sync(); });

// Settings > This browser: the toggle, what it notifies about, a test.
function rows() {
  const no = why();
  const tog = h('input', { type: 'checkbox', checked: st.on, disabled: st.busy || (!!no && !st.on) });
  tog.onchange = () => (tog.checked ? enable() : disable());
  const out = [h('label.li', {}, tog, h('span.ll', {}, 'Notifications'), h('span.lh', {}, st.busy ? 'working…' : st.on ? 'on' : no ? 'unavailable' : 'off'))];
  if (no && !st.on) out.push(h('div.lrow', {}, h('div.cs', {}, no)));
  if (st.on) {
    for (const [k, label] of KINDS) {
      const c = h('input', { type: 'checkbox', checked: st.kinds.includes(k) });
      c.onchange = () => setKind(k, c.checked);
      out.push(h('label.li', {}, c, h('span.ll', {}, label)));
    }
    out.push(li('send', 'Send a test notification', { fn: test }));
  }
  return out;
}
function paint() { if (box?.isConnected) box.replaceChildren(...rows()); }
export function pushCard() {
  if (!why() && conn.open) key().catch(() => {});
  box = h('div.list', {}, rows());
  return box;
}

// A tapped notification: `#tab=12` shows the tab, `#approvals` the
// approvals sheet (once the state is here).
function route(hash) {
  const q = new URLSearchParams(String(hash || '').replace(/^#/, ''));
  if (!q.has('tab') && !q.has('approvals')) return;
  history.replaceState(null, '', location.pathname + location.search);
  const go = () => {
    const tab = parseInt(q.get('tab'), 10);
    if (tab && tabById('t' + tab)) emit('msg:ui', { action: 'show', tab });
    if (q.has('approvals')) emit('msg:ui', { action: 'view', view: 'approvals' });
    dlog('push', 'opened from a notification: ' + [...q.keys()].join(','));
  };
  if (S.tabs.length) go();
  else { const off = on('hello', () => { off(); setTimeout(go, 0); }); }
}
route(location.hash);
addEventListener('hashchange', () => route(location.hash));
navigator.serviceWorker?.addEventListener('message', (e) => {
  if (e.data?.gt === 'open') { try { route(new URL(e.data.url, location.href).hash); } catch {} }
});
