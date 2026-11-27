import { emit } from './util.js';
import { send } from './conn.js';
import { tabById } from './state.js';

export const asks = new Map();

const tid = (x) => (typeof x === 'number' ? 't' + x : String(x));
export const isGod = (a) => a.kind !== 'tab';
export const askTabs = (a) => [a.tab, ...(Array.isArray(a.tabs) ? a.tabs : [])].filter((x) => x != null).map(tid).filter((id) => tabById(id));
export const asksOf = (id) => (id ? [...asks.values()].filter((a) => isGod(a) && askTabs(a).includes(id)) : []);
export const askWhat = (a) => a.what || (a.tool ? String(a.tool).replace(/_/g, ' ') : a.kind === 'switch' ? 'switch account' : 'a question');
export const askWho = (a) => (a.kind === 'tabjob' ? 'Asked by the agent in this tab' : 'Asked by the assistant');

const mins = (s) => (s < 90 ? 'a minute' : Math.round(s / 60) + ' min');
export function askAge(a) {
  const now = Date.now() / 1000, bits = [];
  if (a.asked_at) bits.push(now - a.asked_at < 45 ? 'just now' : mins(now - a.asked_at) + ' ago');
  if (a.expires_at && a.expires_at > now) bits.push('expires in ' + mins(a.expires_at - now));
  return bits.join(', ');
}

export function waitWords(t) {
  if (!t) return '';
  const a = asksOf(t.id)[0];
  if (a) return 'needs your OK: ' + askWhat(a);
  if (t.label === 'confirm') return 'needs your OK: open Approvals';
  if (t.state === 'approval' || t.label === 'blocked') return 'prompt in the terminal';
  return '';
}
export const needsOk = (t) => !!t && (asksOf(t.id).length > 0 || t.label === 'confirm');

const sent = new Map();
export const answered = (id) => Date.now() - (sent.get(id) || 0) < 4000;
export const unanswer = (id) => sent.delete(id);

export function answerAsk(a, choice) {
  send({ t: 'answer', id: a.id, choice });
  sent.set(a.id, Date.now());
  asks.delete(a.id);
  emit('asks');
}
