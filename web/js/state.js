// The mirrored get_state: `hello` gives it whole, `state` frames patch it
// (top level keys replace; a `tabs_changed` list patches tabs by id and
// `tabs_removed` drops ids). Listeners get one coalesced update per frame.
import { on, emit } from './util.js';

export const S = { tabs: [], accounts: [], jobs: { jobs: [] }, loops: [], pending_confirmations: [], modes: '', build: '', version: '' };
let queued = false;

function merge(m) {
  const src = m.state && typeof m.state === 'object' ? { ...m, ...m.state } : m;
  for (const [k, v] of Object.entries(src)) {
    if (k === 't' || k === 'state' || k === 'tabs_changed' || k === 'tabs_removed') continue;
    S[k] = v;
  }
  if (Array.isArray(src.tabs_changed)) {
    for (const t of src.tabs_changed) {
      const i = S.tabs.findIndex((x) => x.id === t.id);
      if (i >= 0) S.tabs[i] = { ...S.tabs[i], ...t };
      else S.tabs.push(t);
    }
  }
  if (Array.isArray(src.tabs_removed)) S.tabs = S.tabs.filter((t) => !src.tabs_removed.includes(t.id));
  if (!queued) {
    queued = true;
    requestAnimationFrame(() => { queued = false; emit('state', S); });
  }
}

on('msg:hello', (m) => { merge(m); emit('hello', m); });
on('msg:state', merge);

export const tabById = (id) => S.tabs.find((t) => t.id === id);
export const acctOf = (n) => S.accounts.find((a) => a.account === n);
