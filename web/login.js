(() => {
  const $ = (s) => document.querySelector(s);
  const msg = (t, bad) => { const m = $('#msg'); m.textContent = t || ''; m.className = bad ? 'bad' : ''; };
  const enc = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  const dec = (s) => {
    const b = atob(s.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (s.length % 4)) % 4));
    return Uint8Array.from(b, (c) => c.charCodeAt(0)).buffer;
  };
  const post = (path, body) => fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    credentials: 'same-origin',
    body: JSON.stringify(body || {}),
  }).then(async (r) => ({ ok: r.ok, status: r.status, body: await r.json().catch(() => ({})) }));

  let ctl = null;
  let busy = false;

  const done = () => { location.replace('/'); };

  async function ask(conditional) {
    if (ctl) ctl.abort();
    ctl = new AbortController();
    const b = await post('/auth/login/begin');
    if (!b.ok) throw new Error(b.body.error || 'Login is not available');
    const pk = b.body.publicKey;
    const cred = await navigator.credentials.get({
      publicKey: {
        challenge: dec(pk.challenge),
        rpId: pk.rpId,
        timeout: pk.timeout,
        userVerification: pk.userVerification,
        allowCredentials: [],
      },
      mediation: conditional ? 'conditional' : undefined,
      signal: ctl.signal,
    });
    if (!cred) throw new Error('No passkey was chosen');
    const r = cred.response;
    const f = await post('/auth/login/finish', {
      cid: b.body.cid,
      id: enc(cred.rawId),
      clientDataJSON: enc(r.clientDataJSON),
      authenticatorData: enc(r.authenticatorData),
      signature: enc(r.signature),
    });
    if (!f.ok) throw new Error(f.status === 429 ? 'Too many tries. Wait a minute.' : 'That passkey was not accepted.');
    done();
  }

  async function manual() {
    if (busy) return;
    busy = true;
    $('#go').disabled = true;
    msg('Waiting for your passkey');
    try {
      await ask(false);
    } catch (e) {
      if (e && e.name === 'NotAllowedError') msg('Cancelled. Tap the button to try again.');
      else if (e && e.name === 'AbortError') return;
      else msg(e.message || 'Sign in failed', true);
    } finally {
      busy = false;
      $('#go').disabled = false;
      auto();
    }
  }

  async function auto() {
    try {
      if (!window.PublicKeyCredential || !PublicKeyCredential.isConditionalMediationAvailable) return;
      if (!(await PublicKeyCredential.isConditionalMediationAvailable())) return;
      await ask(true);
    } catch (e) {
      if (e && e.name !== 'AbortError' && e.name !== 'NotAllowedError') msg(e.message || '', true);
    }
  }

  $('#go').addEventListener('click', manual);

  $('#rf').addEventListener('submit', async (e) => {
    e.preventDefault();
    const code = $('#code').value.trim();
    if (!code) return;
    msg('Checking the code');
    const r = await post('/auth/recovery/use', { code });
    if (r.ok) done();
    else msg(r.status === 429 ? 'Too many tries. Wait a minute.' : 'That code was not accepted.', true);
  });

  (async () => {
    if (!window.PublicKeyCredential || !window.isSecureContext) {
      $('#go').disabled = true;
      msg('Passkeys need a secure (https) address and a current browser.', true);
      return;
    }
    const s = await fetch('/auth/state', { credentials: 'same-origin' }).then((r) => r.json()).catch(() => null);
    if (s && !s.enabled) {
      msg('Login is off for this address.');
      $('#go').disabled = true;
      return;
    }
    if (s && s.origin && s.origin !== location.origin) {
      msg('Open ' + s.origin + ' to sign in with your passkey.', true);
      $('#go').disabled = true;
      return;
    }
    auto();
  })();
})();
