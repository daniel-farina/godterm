import { toast, on } from './util.js';
import { send } from './conn.js';
import { tabById } from './state.js';

const MAX_BYTES = 20 << 20;
let seq = 0;
const waiting = new Map();
on('msg:file', (m) => { const w = waiting.get(m.id); if (w) { waiting.delete(m.id); w.res(m); } });
on('msg:error', (m) => { const w = waiting.get(m.id); if (w) { waiting.delete(m.id); w.rej(new Error(m.error)); } });
on('conn', (s) => { if (s !== 'live') for (const [k, w] of waiting) { waiting.delete(k); w.rej(new Error('connection lost')); } });

export const hasFiles = (e) => !!e.dataTransfer?.types?.includes('Files');

export function shellQuote(p) {
  return /^[\w@%+=:,./-]+$/.test(p) ? p : "'" + p.replace(/'/g, "'\\''") + "'";
}

const readData = (f) => new Promise((res, rej) => {
  const fr = new FileReader();
  fr.onload = () => res(fr.result);
  fr.onerror = () => rej(new Error('could not read it (a folder?)'));
  fr.readAsDataURL(f);
});

async function uploadFile(f) {
  if (f.size > MAX_BYTES) throw new Error('over 20 MB');
  const data = await readData(f);
  const id = 'file' + ++seq;
  return new Promise((res, rej) => {
    if (!send({ t: 'file', id, name: f.name || 'file', data })) return rej(new Error('not connected'));
    waiting.set(id, { res, rej });
    setTimeout(() => { if (waiting.delete(id)) rej(new Error('no reply')); }, 120000);
  });
}

export async function dropOnPane(files, pane) {
  files = [...files];
  if (!files.length) return;
  if (!pane?.id) return toast('Pick a tab in this pane first, then drop the files', 'warn');
  const t = tabById(pane.id);
  const imgs = files.filter((f) => /^image\//.test(f.type));
  const rest = files.filter((f) => !imgs.includes(f));
  if (imgs.length) await (await import('./images.js')).sendImages(imgs, pane).catch(() => {});
  if (!rest.length) return;
  const note = toast(rest.length > 1 ? `Uploading ${rest.length} files...` : `Uploading ${rest[0].name || 'the file'}...`, 'info');
  const paths = [];
  for (const f of rest) {
    try {
      const r = await uploadFile(f);
      paths.push(shellQuote(r.path));
    } catch (e) {
      toast(`${f.name || 'The file'} did not go through: ${e.message || e}`, 'error');
    }
  }
  note.remove();
  if (paths.length) {
    pane.paste(paths.join(' ') + ' ');
    toast(paths.length > 1 ? `${paths.length} file paths typed into ${t?.name || 'the tab'}` : `File path typed into ${t?.name || 'the tab'}`, 'ok');
  }
}

export function dropOnChat(files, add) {
  const imgs = files.filter((f) => /^image\//.test(f.type));
  if (files.length > imgs.length) toast('The assistant takes images only: drop other files onto a terminal pane', 'warn');
  imgs.forEach(add);
  return imgs.length;
}

export function initDrop() {
  addEventListener('dragover', (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    if (!e.target.closest?.('.pane, #chat')) e.dataTransfer.dropEffect = 'none';
  });
  addEventListener('drop', (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    if (!e.target.closest?.('.pane, #chat')) toast('Drop files onto a terminal pane to send them to that tab', 'info');
  });
}
