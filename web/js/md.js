import { esc } from './util.js';

const trimPunct = (u) => { const t = u.match(/[.,;:!?)]+$/); return t ? [u.slice(0, -t[0].length), t[0]] : [u, '']; };

function inline(s) {
  const codes = [];
  s = s.replace(/`([^`\n]+)`/g, (_, c) => (codes.push(c), `\u0000${codes.length - 1}\u0000`));
  s = esc(s)
    .replace(/!\[([^\]]*)\]\((https?:\/\/[^\s)]+|data:image\/(?:png|jpeg|gif|webp);base64,[A-Za-z0-9+/=]+)\)/g, '<img src="$2" alt="$1" loading="lazy">')
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>')
    .replace(/(^|[\s(])(https?:\/\/[^\s<)]+)/g, (_, pre, u) => { const [href, tail] = trimPunct(u); return `${pre}<a href="${href}" target="_blank" rel="noopener noreferrer">${href}</a>${tail}`; })
    .replace(/\*\*([^*]+)\*\*/g, '<b>$1</b>')
    .replace(/(^|[^*\w])\*([^*\n]+)\*(?!\w)/g, '$1<i>$2</i>')
    .replace(/(^|[^_\w])_([^_\n]+)_(?!\w)/g, '$1<i>$2</i>')
    .replace(/~~([^~\n]+)~~/g, '<s>$1</s>');
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${esc(codes[i])}</code>`);
}

function fenceHtml(lang, body) {
  const l = (lang || '').toLowerCase().slice(0, 20);
  return `<div class="cb"><div class="cb-h"><span class="cb-l">${esc(l)}</span><button type="button" class="cb-copy" aria-label="Copy the code">Copy</button></div><pre><code${l ? ` class="lang-${esc(l)}"` : ''}>${esc(body)}</code></pre></div>`;
}

function listHtml(items) {
  let out = '';
  const stack = [];
  const close = () => { const s = stack.pop(); out += `</li></${s.t}>`; };
  for (const it of items) {
    while (stack.length && it.indent < stack[stack.length - 1].indent) close();
    const top = stack[stack.length - 1];
    if (top && it.indent === top.indent && it.t !== top.t) close();
    const now = stack[stack.length - 1];
    if (!now || it.indent > now.indent) { out += `<${it.t}>`; stack.push({ t: it.t, indent: it.indent }); }
    else out += '</li>';
    const task = it.text.match(/^\[([ xX])\]\s+(.*)/);
    out += task ? `<li class="task"><input type="checkbox" disabled${task[1] === ' ' ? '' : ' checked'}> ${inline(task[2])}` : `<li>${inline(it.text)}`;
  }
  while (stack.length) close();
  return out;
}

const cells = (l) => { let s = l.trim(); if (s.startsWith('|')) s = s.slice(1); if (s.endsWith('|')) s = s.slice(0, -1); return s.split(/(?<!\\)\|/).map((c) => c.trim().replace(/\\\|/g, '|')); };
const isRule = (l) => /^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$/.test(l) && l.includes('-');
const aligns = (l) => cells(l).map((c) => (c.startsWith(':') && c.endsWith(':') ? 'center' : c.endsWith(':') ? 'right' : ''));

function tableHtml(head, rule, rows) {
  const al = aligns(rule);
  const td = (tag, cs) => cs.map((c, i) => `<${tag}${al[i] ? ` style="text-align:${al[i]}"` : ''}>${inline(c)}</${tag}>`).join('');
  return `<div class="tbl"><table><thead><tr>${td('th', cells(head))}</tr></thead><tbody>${rows.map((r) => `<tr>${td('td', cells(r))}</tr>`).join('')}</tbody></table></div>`;
}

export function md(src) {
  const lines = String(src ?? '').replace(/\r/g, '').split('\n');
  let out = '', para = [], list = null, i = 0;
  const flushP = () => { if (para.length) { out += `<p>${para.map(inline).join('<br>')}</p>`; para = []; } };
  const flushL = () => { if (list) { out += listHtml(list); list = null; } };
  while (i < lines.length) {
    const l = lines[i];
    const fence = l.match(/^\s*(```+|~~~+)\s*(\w*)/);
    if (fence) {
      flushP(); flushL();
      const body = [];
      const end = new RegExp('^\\s*' + fence[1][0] + '{' + fence[1].length + ',}\\s*$');
      i++;
      while (i < lines.length && !end.test(lines[i])) body.push(lines[i++]);
      i++;
      out += fenceHtml(fence[2], body.join('\n'));
      continue;
    }
    let m;
    if (l.includes('|') && i + 1 < lines.length && isRule(lines[i + 1]) && cells(l).length > 1) {
      flushP(); flushL();
      const head = l, rule = lines[i + 1], rows = [];
      i += 2;
      while (i < lines.length && lines[i].includes('|') && lines[i].trim()) rows.push(lines[i++]);
      out += tableHtml(head, rule, rows);
      continue;
    }
    if ((m = l.match(/^(#{1,6})\s+(.*)/))) { flushP(); flushL(); const n = Math.min(m[1].length + 1, 4); out += `<h${n}>${inline(m[2].replace(/\s+#+\s*$/, ''))}</h${n}>`; }
    else if (/^\s*([-*_])(\s*\1){2,}\s*$/.test(l)) { flushP(); flushL(); out += '<hr>'; }
    else if ((m = l.match(/^(\s*)([-*+]|\d+[.)])\s+(.*)/))) {
      flushP();
      list ||= [];
      list.push({ indent: m[1].replace(/\t/g, '  ').length, t: /\d/.test(m[2]) ? 'ol' : 'ul', text: m[3] });
    }
    else if (list && /^\s{2,}\S/.test(l)) list[list.length - 1].text += ' ' + l.trim();
    else if (/^>/.test(l)) {
      flushP(); flushL();
      const q = [];
      while (i < lines.length && /^>/.test(lines[i])) q.push(lines[i++].replace(/^>\s?/, ''));
      out += `<blockquote>${md(q.join('\n'))}</blockquote>`;
      continue;
    }
    else if (!l.trim()) { flushP(); flushL(); }
    else { flushL(); para.push(l); }
    i++;
  }
  flushP(); flushL();
  return out;
}
