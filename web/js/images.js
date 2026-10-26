// Images from the phone: picked (photo library or camera), pasted or
// shared. Big photos and formats a model may not read (HEIC) are redrawn
// as a JPEG of at most 2000 px first, so uploads stay quick over the
// tailnet. Into a tab: GodTerm saves the file (as the terminal UI saves a
// pasted image) and its path is pasted into the tab, which is how an
// agent CLI takes an image ("[Image #1]" in Claude Code).
import { h, on, toast } from './util.js';
import { send } from './conn.js';
import { S, tabById } from './state.js';

const OK_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
const MAX_SIDE = 2000;

// A data URL the server accepts (PNG, JPEG, GIF or WebP, small enough).
export async function prepare(file) {
  if (!file || !/^image\//.test(file.type || 'image/')) throw new Error('not an image');
  let blob = file;
  if (!OK_TYPES.includes(file.type) || file.size > 1.5e6) {
    const bm = await createImageBitmap(file);
    const k = Math.min(1, MAX_SIDE / Math.max(bm.width, bm.height));
    const c = document.createElement('canvas');
    c.width = Math.round(bm.width * k); c.height = Math.round(bm.height * k);
    c.getContext('2d').drawImage(bm, 0, 0, c.width, c.height);
    bm.close?.();
    blob = await new Promise((r) => c.toBlob(r, 'image/jpeg', 0.85));
    if (!blob) throw new Error('could not convert');
  }
  return new Promise((res, rej) => { const fr = new FileReader(); fr.onload = () => res(fr.result); fr.onerror = rej; fr.readAsDataURL(blob); });
}

let seq = 0;
const waiting = new Map();
on('msg:image', (m) => { const w = waiting.get(m.id); if (w) { waiting.delete(m.id); w.res(m); } });
on('msg:error', (m) => { const w = waiting.get(m.id); if (w) { waiting.delete(m.id); w.rej(new Error(m.error)); } });
on('conn', (s) => { if (s !== 'live') for (const [k, w] of waiting) { waiting.delete(k); w.rej(new Error('connection lost')); } });

function upload(data) {
  const id = 'img' + ++seq;
  return new Promise((res, rej) => {
    if (!send({ t: 'image', id, data })) return rej(new Error('not connected'));
    waiting.set(id, { res, rej });
    setTimeout(() => { if (waiting.delete(id)) rej(new Error('no reply')); }, 60000);
  });
}

// Send images into a tab pane: each saved on the Mac, then its path
// pasted (Grok takes "Image: <path>").
export async function sendImages(files, pane) {
  files = [...files].filter((f) => /^image\//.test(f.type || 'image/'));
  if (!files.length || !pane?.id) return;
  const t = tabById(pane.id);
  const grok = (S.accounts.find((a) => a.account === t?.account)?.harness || '') === 'grok';
  const note = toast(files.length > 1 ? `Sending ${files.length} images...` : 'Sending the image...', 'info');
  let n = 0;
  for (const f of files) {
    try {
      const r = await upload(await prepare(f));
      pane.paste((grok ? 'Image: ' : '') + r.path + ' ');
      n++;
    } catch (e) {
      toast('The image did not go through: ' + (e.message || e), 'error');
    }
  }
  note.remove();
  if (n) toast(n > 1 ? `${n} images added to ${t?.name || 'the tab'}` : `Image added to ${t?.name || 'the tab'}`, 'ok');
}

// The photo library or the camera (no `capture`, so iOS and Android offer
// both), several at once. `then` gets the files instead of sending them.
export function pickImages(pane, then) {
  const inp = h('input', { type: 'file', accept: 'image/*', multiple: true, style: 'position:fixed;left:-100px;top:0;width:1px;height:1px;opacity:0' });
  inp.addEventListener('change', () => { const f = [...inp.files]; if (then) { if (f.length) then(f); } else sendImages(f, pane); inp.remove(); });
  document.body.append(inp);
  inp.click();
  setTimeout(() => inp.isConnected && !inp.files?.length && inp.remove(), 120000);
}
