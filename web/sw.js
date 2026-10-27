const BUILD = '__BUILD__';
const CACHE = 'gt-' + BUILD;
const FILES = ['vendor/xterm.js', 'vendor/xterm.css', 'vendor/addon-fit.js', 'vendor/addon-webgl.js', 'vendor/addon-unicode11.js', 'vendor/addon-web-links.js',
  'app.css', 'js/main.js', 'js/debuglog.js', 'js/util.js', 'js/conn.js', 'js/state.js', 'js/term.js', 'js/textinput.js', 'js/tabs.js', 'js/keybar.js', 'js/chat.js', 'js/md.js', 'js/sheet.js',
  'js/voice.js', 'js/images.js', 'js/drop.js','js/install.js', 'js/manage.js', 'js/settings.js', 'js/sessions.js', 'js/git.js', 'js/bview.js', 'js/accounts.js', 'js/mic-worklet.js', 'js/mic.js', 'js/theme.js', 'js/push.js', 'offline.html', 'icon-192.png'].map((f) => f + '?v=' + BUILD);
const OFFLINE = 'offline.html?v=' + BUILD;
FILES.push('js/gitcfg.js?v=' + BUILD);
FILES.push('js/statusbar.js?v=' + BUILD);
FILES.push('js/find.js?v=' + BUILD);
FILES.push('js/syncui.js?v=' + BUILD);
FILES.push('js/keys.js?v=' + BUILD);
FILES.push('js/debughooks.js?v=' + BUILD);

self.addEventListener('install', (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => Promise.all(FILES.map((f) => c.add(f).catch(() => {})))));
});

self.addEventListener('activate', (e) => {
  e.waitUntil(caches.keys().then((ks) => Promise.all(ks.filter((k) => k !== CACHE && k !== 'gt-page' && k !== 'gt-share').map((k) => caches.delete(k)))).then(() => self.clients.claim()));
});

self.addEventListener('message', (e) => { if (e.data === 'skip') self.skipWaiting(); });

async function share(req) {
  try {
    const fd = await req.formData();
    const c = await caches.open('gt-share');
    for (const k of await c.keys()) await c.delete(k);
    let n = 0;
    for (const f of fd.getAll('images')) {
      if (!(f instanceof File) || !/^image\//.test(f.type)) continue;
      await c.put(`share/${n++}`, new Response(f, { headers: { 'Content-Type': f.type, 'X-Name': encodeURIComponent(f.name || 'image') } }));
    }
    const text = [fd.get('title'), fd.get('text')].filter(Boolean).join('\n');
    if (text) await c.put('share/text', new Response(text));
    return Response.redirect('./?share=' + n, 303);
  } catch {
    return Response.redirect('./', 303);
  }
}

self.addEventListener('fetch', (e) => {
  const req = e.request;
  const url = new URL(req.url);
  if (url.origin !== location.origin || url.pathname.endsWith('/ws')) return;
  if (req.method === 'POST' && url.pathname.endsWith('/share')) { e.respondWith(share(req)); return; }
  if (req.method !== 'GET') return;
  if (url.searchParams.has('v')) {
    e.respondWith(caches.open(CACHE).then((c) => c.match(req).then((hit) => hit || fetch(req).then((r) => { if (r.ok) c.put(req, r.clone()); return r; }))));
    return;
  }
  e.respondWith(
    fetch(req, { cache: 'no-cache' }).then((r) => {
      if (r.ok && !url.searchParams.has('t') && req.mode === 'navigate') { const copy = r.clone(); caches.open('gt-page').then((c) => c.put(url.pathname, copy)); }
      return r;
    }).catch(async () => {
      const page = await caches.open('gt-page').then((c) => c.match(url.pathname));
      if (page) return page;
      if (req.mode === 'navigate') return (await caches.match(OFFLINE)) || new Response('GodTerm is not reachable', { status: 503 });
      const kept = await caches.match(new URL(url.pathname.split('/').pop() + '?v=' + BUILD, self.registration.scope));
      if (kept) return kept;
      return Response.error();
    }),
  );
});

self.addEventListener('push', (e) => {
  let d = {};
  try { d = e.data ? e.data.json() : {}; } catch { d = { body: e.data ? e.data.text() : '' }; }
  const icon = new URL('icon-192.png', self.registration.scope).href;
  e.waitUntil(self.registration.showNotification(d.title || 'GodTerm', {
    body: d.body || '', tag: d.tag || 'godterm', icon, badge: icon, data: { url: d.url || './' },
  }));
});

self.addEventListener('notificationclick', (e) => {
  e.notification.close();
  const url = new URL(e.notification.data?.url || './', self.registration.scope).href;
  e.waitUntil(self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then(async (cs) => {
    for (const c of cs) {
      if (new URL(c.url).origin !== location.origin) continue;
      c.postMessage({ gt: 'open', url });
      try { return await c.focus(); } catch {  }
    }
    return self.clients.openWindow(url);
  }));
});
