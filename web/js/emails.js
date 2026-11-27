import { h, icon, store, emit, on } from './util.js';
import { S } from './state.js';

export const emailsOn = () => store.get('hideEmail', false) !== true;

export function setEmailsOn(v) {
  store.set('hideEmail', v ? null : true);
  emit('emails', v);
  emit('state', S);
}

export const emailOf = (a) => (emailsOn() && a && typeof a.email === 'string' ? a.email : '');

export function emailToggle(el, after, cls = '') {
  const b = el || h('button.ib.em-tog' + (cls ? '.' + cls : ''), { type: 'button' });
  const paint = () => {
    const shown = emailsOn();
    const label = shown ? 'Hide account emails' : 'Show account emails';
    b.innerHTML = icon(shown ? 'eye' : 'eyeoff');
    b.setAttribute('aria-label', label);
    b.setAttribute('title', label);
    b.setAttribute('aria-pressed', String(!shown));
    b.classList.toggle('em-off', !shown);
  };
  b.onclick = (e) => { e.stopPropagation(); setEmailsOn(!emailsOn()); if (after) after(); };
  paint();
  const off = on('emails', () => { if (el || b.isConnected) paint(); else off(); });
  return b;
}
