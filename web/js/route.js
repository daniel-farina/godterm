import { $, h, on, toast, icon, li, isDesk } from './util.js';
import { tool } from './conn.js';
import { tabById } from './state.js';
import { openMenu, openSheet, closeSheet, sheetKind } from './sheet.js';
import { panes } from './term.js';

const LIMITS = 'Sets proxy variables for the tab\'s agent. Tools that ignore them (ssh, raw sockets, some CLIs) still connect directly: this is not a kill switch.';
const WORDS = { ready: 'ready', starting: 'starting', login: 'login needed', missing: 'tailscaled missing', unreachable: 'unreachable', choose: 'choose an exit node', error: 'error', stopped: 'starts on use' };
const TONE = { ready: 'ok', starting: 'dim', stopped: 'dim', login: 'warn', missing: 'warn', unreachable: 'warn', choose: 'warn', error: 'warn' };

export const routeTone = (r) => (!r ? 'off' : r.state === 'on' || r.state === 'idle' ? 'on' : 'warn');
export const routeWords = (r) => {
  if (!r) return 'Direct connection';
  const why = { blocked: 'not started: ' + (r.detail || 'the route is not ready'), bypassed: 'running without the route', warn: r.detail || 'the route is not ready', idle: 'applies when the tab starts' }[r.state];
  return 'Routed via ' + r.name + (why ? ' (' + why + ')' : '');
};

let view = null;
const title = (t) => h('div.sh-title', {}, h('span', {}, t), h('button.ib', { 'aria-label': 'Close', title: 'Close', onclick: closeSheet, html: icon('x') }));
const pill = (state, text) => h('span.rt-st', { 'data-s': TONE[state] || 'dim' }, text || WORDS[state] || state);

async function load(system = false) {
  const r = await tool('tab_route', { op: 'view', system }, 15000);
  if (r.ok === false || r.error) return { error: r.error || 'Routes are not available' };
  view = r.result || {};
  return view;
}

async function call(args, quiet = false) {
  const r = await tool('tab_route', args, 30000);
  if (r.ok === false || r.error) { toast(r.error || 'It did not work', 'error'); return null; }
  if (r.result?.view) view = r.result.view;
  if (!quiet && r.result?.say && !r.result.needs_confirm) toast(r.result.say, /not started|Still not/.test(r.result.say) ? 'warn' : 'ok');
  return r.result || {};
}

function paintChip() {
  const b = $('#b-route');
  if (!b) return;
  const p = panes.focused();
  const t = p && tabById(p.id);
  const r = t?.route || null;
  const tone = routeTone(r);
  b.dataset.route = tone;
  b.querySelector('.rt-l').textContent = r ? 'via ' + r.name : 'Direct';
  const say = t ? 'Route: ' + routeWords(r) + '. Tap to change' : 'Route: pick a tab first';
  b.title = say;
  b.setAttribute('aria-label', say);
  const m = $('#b-tabmenu');
  if (m) {
    m.dataset.route = tone;
    m.title = r ? 'This tab\'s options. ' + routeWords(r) : 'This tab\'s options';
    m.setAttribute('aria-label', m.title);
  }
}

function present(at, kind, heading, ...kids) {
  if (at) openMenu(at, h('div.rt-menu', { 'data-kind': kind }, ...kids));
  else openSheet({ kind }, title(heading), h('div.rt-menu', { 'data-kind': kind }, ...kids));
}

async function choose(t, name, at, force = false) {
  const r = await call({ op: 'set', tab: t.id, route: name, force });
  if (!r) return;
  if (r.needs_confirm) {
    present(at, 'route-confirm', 'Change the route', h('div.appr-q', {}, r.say),
      h('div.acts.rt-acts', {}, h('button.btn', { type: 'button', onclick: closeSheet }, 'Cancel'),
        h('button.btn.pri', { type: 'button', 'data-go': '1', onclick: () => { closeSheet(); choose(t, name, at, true); } }, name ? 'Restart through ' + name : 'Restart direct')));
    return;
  }
  closeSheet();
}

export function routeMenu(id, at = null) {
  const t = tabById(id);
  if (!t) { toast('Pick a tab first', 'warn'); return; }
  if (!at && isDesk()) { const b = $('#b-route'); if (b?.offsetWidth) { const r = b.getBoundingClientRect(); at = { x: r.left, y: r.bottom + 4 }; } }
  const cur = t.route?.name || '';
  const paint = (v) => {
    const rows = [li(cur ? 'arrowr' : 'tick', 'Direct', { on: !cur, sub: 'No proxy', fn: () => choose(t, '', at) })];
    for (const r of v?.routes || []) {
      const row = li(cur === r.name ? 'tick' : 'route', r.name, { on: cur === r.name, sub: r.kind === 'tailscale' ? 'Tailscale exit node' + (r.exit_node ? ' ' + r.exit_node : '') : r.url || 'Custom proxy', fn: () => choose(t, r.name, at) });
      row.dataset.route = r.name;
      row.append(pill(r.state));
      rows.push(row);
    }
    rows[0].dataset.route = '';
    present(at, 'route', 'Route: ' + t.name,
      at ? h('div.gl', {}, 'Route for ' + t.name) : null,
      t.route && routeTone(t.route) === 'warn' ? h('div.rt-warn', {}, routeWords(t.route)) : null,
      h('div.list', {}, rows),
      v?.error ? h('div.mnote', {}, v.error) : !v ? h('div.mnote', {}, 'Loading the routes...') : !(v.routes || []).length ? h('div.mnote', {}, 'No routes yet. Add one under Manage routes.') : null,
      h('div.list', {}, li('gear', 'Manage routes', { chev: true, fn: () => routesSheet() })),
      h('div.mnote.rt-limit', {}, v?.limits || LIMITS));
  };
  paint(view);
  load().then((v) => { if ($('.rt-menu[data-kind=route]')) paint(v); });
}

export function routeBlockCard(t) {
  const r = t.route || {};
  const act = (op) => async (e) => {
    const b = e.currentTarget;
    b.disabled = true;
    await call({ op, tab: t.id });
    b.disabled = false;
  };
  return h('div.lost.rt-block', {}, h('div.lost-t', {}, 'Not started: route ' + (r.name || '') + ' is not ready'),
    h('div.lost-s', {}, (r.detail || 'The route is not ready') + '. The tab was not launched without its route.'),
    h('div.acts.rt-acts', {},
      h('button.btn.pri', { type: 'button', 'data-op': 'retry', onclick: act('retry') }, 'Retry'),
      h('button.btn', { type: 'button', 'data-op': 'route', onclick: (e) => { const b = e.currentTarget.getBoundingClientRect(); routeMenu(t.id, isDesk() ? { x: b.left, y: b.bottom + 4 } : null); } }, 'Change route'),
      h('button.btn', { type: 'button', 'data-op': 'launch_direct', title: 'Start this tab once with a direct connection. Its route stays set.', onclick: act('launch_direct') }, 'Launch without the route')));
}

let timer = 0;
export async function routesSheet(edit = null) {
  clearInterval(timer);
  let form = edit, binsOpen = null;
  const body = h('div.rt-body');
  let seen = '';
  const refresh = async () => {
    const a = document.activeElement;
    if (form != null || (a && a.tagName === 'INPUT' && body.contains(a))) return;
    await load(true);
    const now = JSON.stringify(view);
    if (sheetKind === 'routes' && form == null && now !== seen) { seen = now; paint(); }
  };
  const open = (f) => { form = f; paint(); };

  const card = (r) => {
    const ts = r.kind === 'tailscale';
    const test = r.test;
    const busy = test?.state === 'running';
    return h('div.rt-card', { 'data-route': r.name },
      h('div.rt-h', {}, h('b', {}, r.name), h('span.tagl', {}, ts ? 'Tailscale exit node' : 'Custom proxy'), pill(r.state), r.tabs ? h('span.rt-n', {}, r.tabs + (r.tabs === 1 ? ' tab' : ' tabs')) : null),
      h('div.rt-u', {}, ts ? (r.exit_node ? 'Exit node ' + r.exit_node + (r.allow_lan ? ' · LAN access allowed' : '') : 'No exit node chosen yet') : r.url),
      r.state !== 'ready' && r.state !== 'stopped' && !r.login_url && r.detail ? h('div.rt-d', {}, r.detail) : null,
      r.login_url ? h('div.rt-login', {}, 'This route is its own device on your tailnet and needs a one time login: ', h('a', { href: r.login_url, target: '_blank', rel: 'noopener noreferrer' }, 'open the login page'), '.') : null,
      test && !busy ? h('div.rt-t', { 'data-s': test.state === 'ok' ? 'ok' : 'warn' }, test.detail || (test.state === 'ok' ? 'It works' : 'It failed')) : null,
      h('div.acts.rt-acts', {},
        ts && !r.running && r.state !== 'missing' ? h('button.btn.sm', { type: 'button', 'data-op': 'connect', title: 'Start the managed Tailscale now to log in or choose an exit node. It stops after 10 minutes unless a tab uses it.', onclick: async () => { await call({ op: 'connect', name: r.name }); paint(); } }, 'Connect') : null,
        h('button.btn.sm', { type: 'button', 'data-op': 'test', disabled: busy, title: 'Connects through the route to ' + (view?.test_host || 'the agent API host') + ' and stops after the TLS handshake. Nothing else is sent.', onclick: async () => { await call({ op: 'test', name: r.name }, true); paint(); } }, busy ? 'Testing...' : 'Test'),
        h('button.btn.sm', { type: 'button', 'data-op': 'edit', onclick: () => open({ ...r, was: r.name }) }, 'Edit'),
        h('button.btn.sm.bad', { type: 'button', 'data-op': 'delete', disabled: !!r.tabs, title: r.tabs ? 'Set its tabs to Direct first' : 'Delete this route', onclick: async () => { await call({ op: 'delete', name: r.name }); paint(); } }, 'Delete')));
  };

  const formView = (f) => {
    const name = h('input', { type: 'text', value: f.name || '', placeholder: 'office', maxlength: 24, autocapitalize: 'off', autocorrect: 'off', spellcheck: 'false', 'aria-label': 'Route name' });
    const url = h('input', { type: 'text', value: '', placeholder: f.was && f.url ? 'unchanged: ' + f.url : 'http://proxy.example:3128', autocomplete: 'off', autocapitalize: 'off', autocorrect: 'off', spellcheck: 'false', 'aria-label': 'Proxy URL' });
    const node = h('input', { type: 'text', value: f.exit_node || '', placeholder: 'exit-node-1', list: 'rt-nodes', autocapitalize: 'off', autocorrect: 'off', spellcheck: 'false', 'aria-label': 'Exit node' });
    const known = [...new Set([...(f.nodes || []).map((n) => n.name), ...(view?.system_nodes || [])])];
    const dl = h('datalist', { id: 'rt-nodes' }, known.map((n) => h('option', { value: n })));
    const lan = h('input', { type: 'checkbox', checked: !!f.allow_lan });
    const note = h('div.mnote.rt-note');
    let kind = f.kind || 'custom';
    const custom = h('div.rt-f', {}, h('label.field', {}, 'Proxy URL (http://, https://, socks5:// or socks5h://)', url),
      h('div.mnote', {}, 'A user name and password in the URL are kept in GodTerm\'s config file on this machine and are not shown again. The routed tab\'s own agent can read them in its environment.'));
    const tsBox = h('div.rt-f', {}, h('label.field', {}, 'Exit node (device name or IP)', node), dl,
      known.length ? h('div.rt-nodes', {}, known.map((n) => h('button.chip', { type: 'button', onclick: () => { node.value = n; } }, n))) : h('div.mnote', {}, 'The list of exit nodes appears here once this route is logged in. You can type a name now.'),
      h('label.rt-chk', {}, lan, h('span', {}, 'Allow access to the local network while routed')),
      h('div.mnote', {}, 'GodTerm runs its own userspace tailscaled for this route. It is a separate device on your tailnet and never changes the Tailscale app\'s settings.'));
    const seg = h('div.chips.rt-kind', {}, [['custom', 'Custom proxy'], ['tailscale', 'Tailscale exit node']].map(([k, l]) => h('button.chip' + (kind === k ? '.on' : ''), { type: 'button', 'data-k': k, disabled: !!f.was && k !== kind, onclick: (e) => {
      kind = k;
      for (const c of seg.children) c.classList.toggle('on', c === e.currentTarget);
      custom.hidden = kind !== 'custom'; tsBox.hidden = kind !== 'tailscale';
    } }, l)));
    custom.hidden = kind !== 'custom'; tsBox.hidden = kind !== 'tailscale';
    const save = async () => {
      note.textContent = '';
      const args = { op: 'save', name: name.value.trim().toLowerCase(), kind, was: f.was || '' };
      if (kind === 'custom') args.url = url.value.trim();
      else { args.exit_node = node.value.trim(); args.allow_lan = lan.checked; }
      const r = await tool('tab_route', args, 30000);
      if (r.ok === false || r.error) { note.textContent = r.error || 'It did not work'; return; }
      view = r.result?.view || view;
      toast(r.result?.say || 'Saved', 'ok');
      open(null);
    };
    return [h('div.gl', {}, f.was ? 'Edit ' + f.was : 'New route'), h('label.field', {}, 'Name', name), seg, custom, tsBox, note,
      h('div.acts.rt-acts', {}, h('button.btn', { type: 'button', onclick: () => open(null) }, 'Cancel'), h('button.btn.pri', { type: 'button', 'data-op': 'save', onclick: save }, 'Save'))];
  };

  const bins = () => {
    const b = view?.bins || {};
    const d = h('input', { type: 'text', value: b.tailscaled || '', placeholder: 'found automatically', autocapitalize: 'off', spellcheck: 'false', 'aria-label': 'tailscaled path' });
    const c = h('input', { type: 'text', value: b.tailscale || '', placeholder: 'found automatically', autocapitalize: 'off', spellcheck: 'false', 'aria-label': 'tailscale path' });
    const box = h('details.rt-bins', { open: binsOpen ?? (!b.found && (view?.routes || []).some((r) => r.kind === 'tailscale')) },
      h('summary', { onclick: () => { binsOpen = !box.open; } }, 'Tailscale programs', b.found ? pill('ready', 'found') : pill('missing', 'not found')),
      !b.found ? h('div.rt-d', {}, b.problem || b.hint || '') : null,
      h('label.field', {}, 'tailscaled (full path)', d), h('label.field', {}, 'tailscale (full path)', c),
      h('div.acts.rt-acts', {}, h('button.btn.sm', { type: 'button', onclick: async () => { await call({ op: 'bins', tailscaled: d.value.trim(), tailscale: c.value.trim() }); paint(); } }, 'Save paths')));
    return box;
  };

  function paint() {
    if (form) { body.replaceChildren(...formView(form)); return; }
    const v = view || {};
    body.replaceChildren(...[
      h('div.mnote.rt-limit', {}, (v.limits || LIMITS) + ' A route is chosen per tab, from the Route chip or the tab\'s options.'),
      v.error ? h('div.mnote', {}, v.error) : null,
      ...(v.routes || []).map(card),
      !(v.routes || []).length && !v.error ? h('div.empty', {}, h('div', {}, 'No routes yet'), h('div.eh', {}, 'A route sends one tab\'s agent through a proxy: your own, or a Tailscale exit node.')) : null,
      h('div.acts.rt-acts', {}, h('button.btn.pri', { type: 'button', 'data-op': 'add', onclick: () => open({ kind: 'custom' }) }, 'Add a route')),
      bins()].filter(Boolean));
  }

  openSheet({ kind: 'routes', tall: true, onClose: () => clearInterval(timer) }, title('Routes'), body);
  paint();
  await load(true);
  if (sheetKind !== 'routes') return;
  paint();
  timer = setInterval(refresh, 1500);
}

on('state', paintChip);
on('focus', paintChip);
on('routemenu', (id, at) => routeMenu(id, at));
{
  const b = $('#b-route');
  if (b) b.onclick = () => { const p = panes.focused(); if (!p?.id) return toast('Pick a tab first', 'warn'); const r = b.getBoundingClientRect(); routeMenu(p.id, { x: r.left, y: r.bottom + 4 }); };
  paintChip();
}
