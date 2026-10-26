// A small, safe markdown renderer: fences, headings, lists, quotes,
// paragraphs, inline code, bold, italic and links. Input is escaped first.
import { esc } from './util.js';

function inline(s) {
  const codes = [];
  s = s.replace(/`([^`\n]+)`/g, (_, c) => (codes.push(c), `\u0000${codes.length - 1}\u0000`));
  s = esc(s)
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener">$1</a>')
    .replace(/(^|[\s(])(https?:\/\/[^\s<)]+)/g, '$1<a href="$2" target="_blank" rel="noopener">$2</a>')
    .replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>')
    .replace(/(^|[^*\w])\*([^*\n]+)\*(?!\w)/g, '$1<i>$2</i>')
    .replace(/(^|[^_\w])_([^_\n]+)_(?!\w)/g, '$1<i>$2</i>');
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${esc(codes[i])}</code>`);
}

export function md(src) {
  const lines = String(src ?? '').replace(/\r/g, '').split('\n');
  let out = '', para = [], list = null, i = 0;
  const flushP = () => { if (para.length) { out += `<p>${para.map(inline).join('<br>')}</p>`; para = []; } };
  const flushL = () => { if (list) { out += `<${list.t}>${list.items.map((x) => `<li>${inline(x)}</li>`).join('')}</${list.t}>`; list = null; } };
  while (i < lines.length) {
    const l = lines[i];
    const fence = l.match(/^\s*```(\w*)/);
    if (fence) {
      flushP(); flushL();
      const body = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) body.push(lines[i++]);
      i++;
      out += `<pre><code>${esc(body.join('\n'))}</code></pre>`;
      continue;
    }
    let m;
    if ((m = l.match(/^(#{1,4})\s+(.*)/))) { flushP(); flushL(); out += `<h3>${inline(m[2])}</h3>`; }
    else if ((m = l.match(/^\s*[-*+]\s+(.*)/))) { flushP(); if (list?.t !== 'ul') { flushL(); list = { t: 'ul', items: [] }; } list.items.push(m[1]); }
    else if ((m = l.match(/^\s*\d+[.)]\s+(.*)/))) { flushP(); if (list?.t !== 'ol') { flushL(); list = { t: 'ol', items: [] }; } list.items.push(m[1]); }
    else if ((m = l.match(/^>\s?(.*)/))) { flushP(); flushL(); out += `<blockquote>${inline(m[1])}</blockquote>`; }
    else if (!l.trim()) { flushP(); flushL(); }
    else { flushL(); para.push(l); }
    i++;
  }
  flushP(); flushL();
  return out;
}
