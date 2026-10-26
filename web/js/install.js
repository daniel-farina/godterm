// Installing the app: Chrome's install prompt (kept for an Install button
// in Manage and a one time hint), Safari's "Share > Add to Home Screen"
// hint (once, only in Safari, not when already installed), and photos
// shared to the installed app (Share > GodTerm).
import { h, on, toast, store, li, group, icon } from './util.js';
import { S } from './state.js';
import { openSheet, closeSheet } from './sheet.js';
import { panes } from './term.js';
import { attachFiles } from './chat.js';

let prompt = null;
export const standalone = () => matchMedia('(display-mode: standalone)').matches || navigator.standalone === true;
export const ios = () => /iP(hone|ad|od)/.test(navigator.userAgent) || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
export const safari = () => /Safari\//.test(navigator.userAgent) && !/CriOS|FxiOS|EdgiOS|Chrome\//.test(navigator.userAgent);

export const canInstall = () => !!prompt;
export async function install() {
  if (!prompt) return iosSheet();
  const p = prompt;
  prompt = null;
  p.prompt();
  const r = await p.userChoice.catch(() => null);
  if (r?.outcome === 'accepted') toast('GodTerm is on your home screen', 'ok');
}

function iosSheet() {
  openSheet({ kind: 'install' }, h('div.sh-title', {}, h('span', {}, 'Install GodTerm')),
    h('div.mnote', {}, ios() ? 'In Safari, tap Share, then Add to Home Screen. GodTerm then opens full screen, like an app.' : 'Use your browser\'s menu: Install app, or Add to Home Screen.'),
    h('div.acts', {}, h('button.btn.pri', { onclick: closeSheet }, 'OK')));
}

export function initInstall() {
  addEventListener('beforeinstallprompt', (e) => {
    e.preventDefault();
    prompt = e;
    if (!store.get('install-hint', false) && !standalone()) {
      store.set('install-hint', true);
      toast('Install GodTerm as an app on this device?', 'info', { label: 'Install', fn: install });
    }
  });
  addEventListener('appinstalled', () => { prompt = null; });
  if (ios() && safari() && !standalone() && !store.get('install-hint', false)) {
    store.set('install-hint', true);
    // After a minute of use, for 10 s (it has an x and a tap dismisses it).
    setTimeout(() => toast('Tip: Share, then Add to Home Screen, makes GodTerm an app', 'info', { label: 'How', fn: iosSheet }, 10000), 60000);
  }
  shared();
}

// Photos shared to the app: the service worker kept them; ask where.
async function shared() {
  const n = new URLSearchParams(location.search).get('share');
  if (n == null) return;
  history.replaceState(null, '', location.pathname);
  let files = [], text = '';
  try {
    const c = await caches.open('gt-share');
    for (const k of await c.keys()) {
      const r = await c.match(k);
      if (k.url.endsWith('/share/text')) text = await r.text();
      else files.push(new File([await r.blob()], decodeURIComponent(r.headers.get('X-Name') || 'image'), { type: r.headers.get('Content-Type') || 'image/jpeg' }));
      await c.delete(k);
    }
  } catch {}
  if (!files.length && !text) return;
  // Wait for the tab list.
  if (!S.tabs.length) await new Promise((r) => { const off = on('state', () => { if (S.tabs.length) { off(); r(); } }); setTimeout(r, 8000); });
  const to = async (id) => {
    closeSheet();
    if (!id) { if (text) { const say = document.getElementById('say'); say.value = text; } return attachFiles(files); }
    panes.show(id);
    const p = panes.focused();
    if (files.length) await (await import('./images.js')).sendImages(files, p);
    if (text) p.paste(text);
  };
  const cur = panes.focused()?.id;
  const tabs = S.tabs.filter((t) => t.id !== cur).slice(0, 30);
  const what = files.length ? (files.length > 1 ? files.length + ' images' : 'The image') : 'The text';
  openSheet({ kind: 'share', tall: tabs.length > 6 }, h('div.sh-title', {}, h('span', {}, 'Send ' + what.toLowerCase() + ' to'), h('button.ib', { 'aria-label': 'Close', onclick: closeSheet, html: icon('x') })),
    files.length ? h('div.att-in', {}, files.slice(0, 6).map((f) => h('img', { src: URL.createObjectURL(f), alt: f.name }))) : h('div.mnote', {}, text.slice(0, 200)),
    ...group('', [cur ? li('move', S.tabs.find((t) => t.id === cur)?.name || 'This tab', { fn: () => to(cur), hint: 'shown now' }) : null, li('chat', 'The assistant', { fn: () => to('') })]),
    ...group('Other tabs', tabs.map((t) => li('move', t.name, { fn: () => to(t.id), hint: t.account_label || '' }))));
}
