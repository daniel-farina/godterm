import { h, toast, icon } from './util.js';

const enc = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const dec = (s) => {
  const b = atob(s.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (s.length % 4)) % 4));
  return Uint8Array.from(b, (c) => c.charCodeAt(0)).buffer;
};

async function api(path, body) {
  const r = await fetch(path, body === undefined ? { credentials: 'same-origin' } : {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'same-origin',
    body: JSON.stringify(body),
  });
  const j = await r.json().catch(() => ({}));
  if (!r.ok || j.ok === false) throw new Error(j.error || (r.status === 401 ? 'Not signed in' : 'Request failed (' + r.status + ')'));
  return j;
}

let styled = false;
function style() {
  if (styled) return;
  styled = true;
  document.head.append(h('style', {}, `
.rm{display:flex;flex-direction:column;gap:12px;min-width:0}
.rm .rm-card{border:1px solid var(--line);border-radius:12px;background:var(--panel);padding:12px 14px;display:flex;flex-direction:column;gap:8px;min-width:0}
.rm .rm-card h3{margin:0;font-size:14px;font-weight:600;display:flex;gap:8px;align-items:center}
.rm .rm-card h3 svg{width:16px;height:16px;flex:none}
.rm .rm-card p{margin:0;font-size:13px;line-height:1.45;color:var(--dim)}
.rm .rm-card p b{color:var(--text);font-weight:600}
.rm .rm-url{font:13px ui-monospace,Menlo,monospace;overflow-wrap:anywhere;color:var(--text);padding:8px 10px;border:1px solid var(--line);border-radius:8px;background:var(--bg)}
.rm .rm-acts{display:flex;gap:8px;flex-wrap:wrap}
.rm .rm-pill{display:inline-block;font-size:11.5px;border:1px solid var(--line);border-radius:6px;padding:1px 7px;color:var(--dim);white-space:nowrap}
.rm .rm-pill.ok{color:var(--ok);border-color:color-mix(in srgb,var(--ok) 60%,var(--line))}
.rm .rm-pill.warn{color:var(--warn);border-color:var(--warn)}
.rm .rm-warn{border-left:3px solid var(--warn);padding-left:10px}
.rm .rm-item{display:flex;gap:10px;align-items:center;padding:8px 0;border-top:1px solid var(--line);min-width:0}
.rm .rm-item:first-of-type{border-top:0}
.rm .rm-item .rm-t{flex:1;min-width:0;display:flex;flex-direction:column;gap:2px}
.rm .rm-item .rm-t b{font-weight:500;font-size:14px;overflow-wrap:anywhere}
.rm .rm-item .rm-t small{font-size:12px;color:var(--dim)}
.rm .rm-item .rm-b{display:flex;gap:6px;flex:none;flex-wrap:wrap;justify-content:flex-end}
.rm .rm-code{font:600 17px ui-monospace,Menlo,monospace;letter-spacing:.04em;overflow-wrap:anywhere;text-align:center;padding:12px 8px;border:1px dashed var(--accent);border-radius:8px;background:var(--bg);user-select:all}
@media (max-width:480px){.rm .rm-item{flex-wrap:wrap}.rm .rm-item .rm-b{width:100%;justify-content:flex-start}}
`));
}

const btn = (label, fn, cls = '', attrs = {}) => h('button.btn.sm' + cls, { type: 'button', ...attrs, onclick: async (e) => {
  e.preventDefault();
  const b = e.currentTarget;
  b.disabled = true;
  try { await fn(); } catch (err) { toast(err.message || String(err), 'error'); } finally { b.disabled = false; }
} }, label);

const when = (t) => (t ? new Date(t * 1000).toLocaleString([], { dateStyle: 'medium', timeStyle: 'short' }) : 'never');
const ago = (t) => {
  const d = Math.max(0, Date.now() / 1000 - t);
  if (d < 90) return 'just now';
  if (d < 5400) return Math.round(d / 60) + ' min ago';
  if (d < 129600) return Math.round(d / 3600) + ' h ago';
  return Math.round(d / 86400) + ' days ago';
};

async function copy(text) {
  try { await navigator.clipboard.writeText(text); toast('Copied', 'info'); } catch { toast('Could not copy: select the text instead', 'warn'); }
}

export function remoteSection() {
  style();
  const root = h('div.rm');
  let recovery = '';
  let data = null;
  let remote = null;
  let failed = '';

  const load = async () => {
    try {
      data = await api('/auth/status');
      failed = '';
    } catch (e) { failed = e.message; }
    try { remote = (await api('/auth/remote')).remote; } catch { remote = null; }
    paint();
  };

  const act = async (fn) => { await fn(); await load(); };

  const addPasskey = async () => {
    const label = prompt('Name this passkey (for example "iPhone")', '');
    if (label === null) return;
    const b = await api('/auth/register/begin', {});
    const pk = b.publicKey;
    const cred = await navigator.credentials.create({ publicKey: {
      rp: pk.rp,
      user: { id: dec(pk.user.id), name: pk.user.name, displayName: pk.user.displayName },
      challenge: dec(pk.challenge),
      pubKeyCredParams: pk.pubKeyCredParams,
      timeout: pk.timeout,
      attestation: pk.attestation,
      authenticatorSelection: pk.authenticatorSelection,
      excludeCredentials: pk.excludeCredentials.map((c) => ({ type: c.type, id: dec(c.id) })),
    } });
    await api('/auth/register/finish', {
      cid: b.cid,
      label: label.trim(),
      clientDataJSON: enc(cred.response.clientDataJSON),
      attestationObject: enc(cred.response.attestationObject),
    });
    toast('Passkey saved', 'info');
  };

  const openRemote = async () => {
    const p = await api('/auth/pair', {});
    window.open(p.url, '_blank', 'noopener');
    toast('Opened the remote address; the link works once for ten minutes', 'info');
  };

  const copyLink = async () => {
    const p = await api('/auth/pair', {});
    await copy(p.url);
  };

  const remoteCard = () => {
    const c = h('div.rm-card', {}, h('h3', {}, h('span', { html: icon('globe') }), 'Remote address'));
    if (!remote) { c.append(h('p', {}, 'Could not ask Tailscale.')); return c; }
    if (!remote.installed) {
      c.append(h('p', {}, 'Tailscale is not installed on this Mac. Install it and sign in, then come back: GodTerm uses its HTTPS address and installs nothing by itself.'));
      return c;
    }
    if (!remote.running) {
      c.append(h('p', {}, 'Tailscale is installed but not running or not signed in.'));
      c.append(h('div.rm-acts', {}, btn('Check again', load)));
      return c;
    }
    if (!remote.url) {
      c.append(h('p', {}, h('b', {}, 'Private address. '), 'This runs "tailscale serve" for GodTerm: an HTTPS address that only your own devices on your tailnet can open.'));
      c.append(h('div.rm-acts', {}, btn('Create private address', () => act(() => api('/auth/remote', { action: 'serve' }).then(() => toast('Address created', 'info'))), '.pri')));
      return c;
    }
    c.append(h('div.rm-url', {}, remote.url));
    c.append(h('div', {}, h('span.rm-pill' + (remote.public ? '.warn' : '.ok'), {}, remote.public ? 'Public on the internet (Funnel)' : 'Private to your tailnet')));
    c.append(h('p', {}, remote.public
      ? 'Anyone on the internet can reach this address and see the sign-in page. Only a saved passkey gets in. Terminal tabs stay behind that sign-in.'
      : 'Only devices signed in to your tailnet can open it. With login on, they still need a passkey.'));
    const acts = h('div.rm-acts', {}, btn('Copy', () => copy(remote.url)));
    acts.append(btn('Open', openRemote));
    if (remote.public) acts.append(btn('Make private', () => act(() => api('/auth/remote', { action: 'private' }))));
    else {
      acts.append(btn('Make public (Funnel)', () => act(async () => {
        if (!confirm('Funnel makes this address reachable from the whole internet, not only your tailnet. Visitors see only the passkey sign-in page, but your Mac is then exposed to the public internet. Continue?')) return;
        await api('/auth/remote', { action: 'funnel' });
      }), '', { disabled: !data?.login.enabled, title: data?.login.enabled ? '' : 'Turn on login first' }));
      if (!data?.login.enabled) c.append(h('p', {}, 'Funnel needs login on first, so the public address is never open with only a link.'));
    }
    if (!data?.login.enabled) acts.append(btn('Remove address', () => act(() => api('/auth/remote', { action: 'off' }))));
    c.append(acts);
    return c;
  };

  const passkeyCard = () => {
    const lg = data.login;
    const here = !!remote?.url && location.origin === remote.url;
    const c = h('div.rm-card', {}, h('h3', {}, h('span', { html: icon('key') }), 'Passkeys'));
    if (!remote?.url && !lg.remote_url) {
      c.append(h('p', {}, 'Create the remote address first. A passkey belongs to one address.'));
      return c;
    }
    c.append(h('p', {}, 'A passkey is your Face ID, Touch ID or device code. Passkeys saved in iCloud Keychain are available on all your Apple devices; a security key works too.'));
    if (!data.passkeys.length) c.append(h('p', {}, 'No passkeys yet.'));
    for (const p of data.passkeys) {
      c.append(h('div.rm-item', {},
        h('div.rm-t', {}, h('b', {}, p.label), h('small', {}, `Added ${when(p.created)} · last used ${p.last_used ? ago(p.last_used) : 'never'}${p.current ? '' : ' · for an older address, unused'}`)),
        h('div.rm-b', {},
          btn('Rename', () => act(async () => {
            const n = prompt('Passkey name', p.label);
            if (n && n.trim()) await api('/auth/passkey/rename', { pid: p.pid, label: n.trim() });
          })),
          btn('Remove', () => act(async () => {
            if (confirm(`Remove the passkey "${p.label}"? It will no longer sign in. (Also delete it from your device's passwords if you like.)`)) await api('/auth/passkey/remove', { pid: p.pid });
          })))));
    }
    const acts = h('div.rm-acts');
    if (here || (lg.remote_url && location.origin === lg.remote_url)) acts.append(btn('Add a passkey', () => act(addPasskey), '.pri'));
    else {
      c.append(h('p', { class: 'rm-warn' }, 'A passkey can only be created from the remote address itself. Open it on the device you want to use; this signs that browser in once, for ten minutes of link validity.'));
      acts.append(btn('Open remote address to add a passkey', openRemote, '.pri'));
      acts.append(btn('Copy one-time link for another device', copyLink));
    }
    c.append(acts);
    return c;
  };

  const loginCard = () => {
    const lg = data.login;
    const c = h('div.rm-card', {}, h('h3', {}, h('span', { html: icon('shieldalert') }), 'Login ', h('span.rm-pill' + (lg.enabled ? '.ok' : ''), {}, lg.enabled ? 'On' : 'Off')));
    if (recovery) {
      c.append(h('p', {}, h('b', {}, 'Recovery code. '), 'Shown once. Keep it somewhere safe; it signs you in one time if all your passkeys are lost.'));
      c.append(h('div.rm-code', {}, recovery));
      c.append(h('div.rm-acts', {}, btn('Copy', () => copy(recovery)), btn('I saved it', () => { recovery = ''; paint(); }, '.pri')));
    }
    if (lg.enabled) {
      c.append(h('p', {}, 'Every request that arrives through the remote address needs a passkey sign-in. This Mac\'s own browser at 127.0.0.1 still uses the token link, and the token link no longer works remotely.'));
      c.append(h('div.rm-acts', {},
        btn('New recovery code', () => act(async () => { recovery = (await api('/auth/recovery/new', {})).recovery; })),
        btn('Turn off login', () => act(async () => {
          if (!confirm('Turn off login? Passkeys are kept, all signed-in browsers are signed out, and a public address is made private.')) return;
          await api('/auth/disable', {});
          toast('Login is off', 'info');
        }))));
    } else {
      c.append(h('p', {}, 'Off: the remote address works with the sign-in link or your tailnet identity, as before. Turn it on to require a passkey for every remote request.'));
      const ready = data.passkeys.some((p) => p.current);
      c.append(h('div.rm-acts', {}, btn('Turn on login', () => act(async () => {
        recovery = (await api('/auth/enable', {})).recovery;
        toast('Login is on', 'info');
      }), '.pri', { disabled: !ready || !lg.remote_url, title: ready ? '' : 'Add a passkey first' })));
      if (!ready) c.append(h('p', {}, 'Add a passkey below first.'));
    }
    return c;
  };

  const sessionCard = () => {
    const c = h('div.rm-card', {}, h('h3', {}, h('span', { html: icon('users') }), 'Signed-in browsers'));
    if (!data.sessions.length) c.append(h('p', {}, 'None. Browsers signed in with a passkey or a one-time link appear here for 30 days; using one renews it.'));
    for (const s of data.sessions) {
      c.append(h('div.rm-item', {},
        h('div.rm-t', {}, h('b', {}, s.label || 'Browser', s.current ? h('span.rm-pill.ok', { style: 'margin-left:8px' }, 'This browser') : null), h('small', {}, `Signed in ${when(s.created)} · seen ${ago(s.last_seen)} · expires ${when(s.expires)}`)),
        h('div.rm-b', {}, btn('Sign out', () => act(async () => {
          await api('/auth/session/revoke', { sid: s.sid });
          if (s.current) location.reload();
        })))));
    }
    if (data.sessions.length) c.append(h('div.rm-acts', {}, btn('Sign out everywhere', () => act(async () => {
      if (!confirm('Sign out every browser, including this one?')) return;
      await api('/auth/session/revoke_all', {});
      if (data.here.session) location.reload();
    }))));
    return c;
  };

  const paint = () => {
    if (failed || !data) {
      root.replaceChildren(h('div.rm-card', {}, h('p', {}, failed ? 'Remote access settings did not load: ' + failed : 'Loading'), btn('Try again', load)));
      return;
    }
    root.replaceChildren(
      h('div.mnote.st-note', {}, 'Use GodTerm from your phone or another computer through an HTTPS address, protected by a passkey. Only you, from this app, can change these settings; terminal tabs and the assistant cannot.'),
      remoteCard(), passkeyCard(), loginCard(), sessionCard());
  };

  paint();
  if ('IntersectionObserver' in window) {
    const io = new IntersectionObserver((es) => {
      if (es.some((e) => e.isIntersecting)) { io.disconnect(); load(); }
    });
    io.observe(root);
  } else load();
  return root;
}
