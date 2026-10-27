import { h, on, toast } from './util.js';
import { send } from './conn.js';
import { S, tabById } from './state.js';

const OK_TYPES = ['image/png', 'image/jpeg', 'image/gif', 'image/webp'];
const MAX_SIDE = 2000;

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
