import { h, on, toast, isDesk, store, icon, stateWords, skeleton, failRow } from './util.js';
import { send, tool } from './conn.js';
import { tabById } from './state.js';
import { openSheet, closeSheet } from './sheet.js';

let seq = 0;
const waits = new Map();
on('msg:git', (m) => {
  const w = waits.get(m.id);
  if (w) { waits.delete(m.id); w(m); }
});

function ask(frame, ms = 180000) {
  return new Promise((res) => {
    const id = 'g' + (++seq) + '-' + Date.now();
    waits.set(id, res);
    if (!send({ t: 'git', id, ...frame })) { waits.delete(id); return res({ ok: false, error: 'not connected to GodTerm' }); }
    setTimeout(() => { if (waits.has(id)) { waits.delete(id); res({ ok: false, error: 'GodTerm did not answer in time' }); } }, ms);
  });
}

let styled = false;
function style() {
  if (styled) return;
  styled = true;
  document.head.append(h('style', {}, `
.gsh{display:flex;flex-direction:column;gap:10px;min-width:0}
.gsh .gwarn{color:var(--warn);font-size:13px;display:flex;gap:6px;flex-wrap:wrap}
.gsh .gwarn span{border:1px solid var(--warn);border-radius:8px;padding:2px 8px}
.gsh .gmono{font-family:ui-monospace,Menlo,monospace;font-size:12.5px}
.gsh .gup{color:var(--warn)}
.gsh .gbad{color:var(--err);font-size:13px}
.gsh .gok{color:var(--ok);font-size:13px}
.gsh textarea,.gsh input[type=text],.gsh select{width:100%;box-sizing:border-box;font:inherit;padding:8px 10px;border:1px solid var(--line);border-radius:10px;background:var(--bg);color:var(--text)}
.gsh label.gck{display:flex;gap:8px;align-items:center;font-size:13.5px}
.gsh .gplan div{font-size:13px;color:var(--dim);white-space:pre-wrap;overflow-wrap:anywhere}
.gsh .ghead{display:flex;flex-direction:column;gap:7px;min-width:0}
.gsh .gpath{color:var(--dim);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.gsh .gchips{display:flex;gap:6px;flex-wrap:wrap;align-items:center}
.gsh .gchip{display:inline-flex;align-items:center;gap:5px;max-width:100%;height:24px;padding:0 9px;border:1px solid var(--chip-line);border-radius:12px;background:var(--chip);color:var(--chip-text);font-size:12.5px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.gsh .gchip b{font-weight:600}
.gsh .gchip.dim{color:var(--chip-dim)}
.gsh .gchip.ok{border-color:color-mix(in srgb,var(--ok) 55%,var(--chip-line));color:var(--ok)}
.gsh .gchip.warn{border-color:color-mix(in srgb,var(--warn) 55%,var(--chip-line));color:var(--warn)}
.gsh .gacts{display:flex;flex-wrap:wrap;gap:6px}
.gsh .gab{min-height:32px;padding:0 12px;border:1px solid var(--line);border-radius:10px;background:var(--bg);color:var(--text);font-size:13px;white-space:nowrap}
.gsh .gab:hover:not(:disabled){border-color:var(--dim);background:var(--panel2)}
.gsh .gab:disabled{opacity:.45;cursor:default}
.gsh .gab.pri:disabled{opacity:1;background:none;border-color:var(--line);color:var(--dim);font-weight:500}
.gsh .gab.g2{color:var(--dim)}
.gsh .gab.g2:hover:not(:disabled){color:var(--text)}
.gsh .ggrp{display:flex;flex-direction:column;gap:8px;min-width:0}
.gsh .gcols{display:grid;grid-template-columns:minmax(0,1fr);gap:12px;align-items:start}
.gsh .gcol{display:flex;flex-direction:column;gap:8px;min-width:0}
.gsh .gfrow{display:flex;gap:8px;align-items:baseline;padding:5px 10px;font-size:12.5px;min-width:0}
.gsh .list>.gfrow+.gfrow{border-top:1px solid var(--line)}
.gsh .gfrow .gp{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;direction:rtl;text-align:left}
.gsh .gfrow .gn{flex:none;color:var(--dim);font-variant-numeric:tabular-nums}
.gsh .gfrow .gn .a{color:var(--ok)}.gsh .gfrow .gn .r{color:var(--err)}
.gsh .gmore{padding:5px 10px;font-size:12px;color:var(--dim);border-top:1px solid var(--line)}
.gsh .gclean{font-size:13px;color:var(--dim);padding:2px 4px}
.gsh .gnone{font-size:13.5px;padding:2px 4px;overflow-wrap:anywhere}
.gsh .gnone small{display:block;color:var(--dim);font-size:12.5px;margin-top:2px}
.gsh .gcm{display:flex;flex-direction:column;gap:2px;padding:7px 10px;min-width:0}
.gsh .list>.gcm+.gcm{border-top:1px solid var(--line)}
.gsh .gcm .s{font-size:13px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.gsh .gcm .m{font-size:11.5px;color:var(--dim);overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.gsh .gfoot{display:flex;gap:8px;justify-content:flex-end}
.gsh .gpick{display:flex;flex-direction:column;gap:8px;padding:10px 12px;border:1px solid color-mix(in srgb,var(--warn) 45%,var(--line));border-radius:10px;background:color-mix(in srgb,var(--warn) 6%,var(--bg))}
.gsh .gpick .q{font-size:13.5px}
.gsh .gpick .q small{display:block;color:var(--dim);font-size:12px;margin-top:2px}
.gsh .gpick .opts{display:flex;flex-wrap:wrap;gap:6px}
.gsh .gpick .gab{display:inline-flex;align-items:center;gap:6px}
.gsh .gpick .gab small{color:var(--dim);font-size:11.5px}
.gsh .gpick .gcode{font-family:ui-monospace,Menlo,monospace;font-size:22px;letter-spacing:2px;padding:4px 0;user-select:all}
.gsh .gpick .st{font-size:12.5px;color:var(--dim)}
.gsh .gpick .st.bad{color:var(--err)}.gsh .gpick .st.ok{color:var(--ok)}
.gsh .gpick code{font-family:ui-monospace,Menlo,monospace;font-size:12px;overflow-wrap:anywhere}
.gsh.gw .gcols{grid-template-columns:minmax(0,1fr) minmax(0,1fr);gap:16px}
.gsh.gw .gsub{width:100%;max-width:760px;display:flex;flex-direction:column;gap:10px}
.touch .gsh .gab{min-height:44px}
.touch .gsh .gai .gaibig{min-height:44px}
.touch .gsh details.gman>summary{min-height:44px}
.gsh.gp{gap:8px}
.gsh.gp .gab{min-height:30px;padding:0 10px;font-size:12.5px}
.touch .gsh.gp .gab{min-height:44px}
.gsh.gp .gacts.main{display:grid;grid-template-columns:repeat(3,minmax(0,1fr))}
.gsh.gp .gacts.main .gab{white-space:normal;line-height:1.2;padding-top:4px;padding-bottom:4px}
.gsh.gp .gcm{padding:6px 10px}
.gsh.gp .gchip{height:22px;font-size:12px}
.gsh.gp .gfoot .btn{flex:1}
.gsh .gai{display:flex;flex-direction:column;gap:8px;padding:10px 12px;border:1px solid var(--line);border-radius:12px;background:var(--panel2);min-width:0}
.gsh .gai .gait{display:flex;align-items:baseline;gap:8px;flex-wrap:wrap;font-size:13.5px;font-weight:600}
.gsh .gai .gait small{font-weight:400;color:var(--dim);font-size:12px}
.gsh .gai .gairow{display:flex;flex-wrap:wrap;gap:6px}
.gsh .gab.pri{background:var(--accent);border-color:var(--accent);color:var(--accent-ink);font-weight:600}
.gsh .gab.pri:hover:not(:disabled){background:var(--accent);border-color:var(--accent);filter:brightness(1.06)}
.gsh .gai .gaibig{min-height:38px;font-size:14px;padding:0 16px}
.gsh .gai .gaiask{display:flex;gap:6px;min-width:0}
.gsh .gai .gaiask input{flex:1;min-width:0}
.gsh .gai .gaist{font-size:12.5px;color:var(--dim);overflow-wrap:anywhere}
.gsh .gai .gaist.ok{color:var(--ok)}.gsh .gai .gaist.bad{color:var(--err)}.gsh .gai .gaist.warn{color:var(--warn)}
.gsh details.gman{border:1px solid var(--line);border-radius:12px;background:var(--bg);min-width:0}
.gsh details.gman>summary{cursor:pointer;list-style:none;display:flex;align-items:center;gap:8px;min-height:36px;padding:0 12px;font-size:13px;color:var(--dim);user-select:none}
.gsh details.gman>summary::-webkit-details-marker{display:none}
.gsh details.gman>summary::before{content:"▸";font-size:11px;transition:transform .15s}
.gsh details.gman[open]>summary::before{transform:rotate(90deg)}
.gsh details.gman>summary:hover{color:var(--text)}
.gsh details.gman>summary small{margin-left:auto;font-size:11.5px}
.gsh details.gman>.gmanb{display:flex;flex-direction:column;gap:8px;padding:0 12px 12px}
.gsh details.gman .mnote{font-size:12.5px;color:var(--dim)}
.gsh details.gman .list{border-radius:10px}
.gsh details.guntr>summary{cursor:pointer;list-style:none}
.gsh details.guntr>summary::-webkit-details-marker{display:none}
.gsh details.guntr>summary .gl::before{content:"▸ ";}
.gsh details.guntr[open]>summary .gl::before{content:"▾ ";}
.gsh details.guntr[open]>.list{margin-top:8px}
.gsh.gp .gai{padding:9px 10px}
.gsh.gp .gai .gaibig{width:100%}
.gsh.gp .gai .gairow.sec{display:grid;grid-template-columns:repeat(2,minmax(0,1fr))}
.gsh:not(.gw):not(.gp) .gai .gaibig{width:100%}
.gsh:not(.gw):not(.gp) .gai .gairow.sec{display:grid;grid-template-columns:repeat(2,minmax(0,1fr))}
.gsh .gai .gairow.sec .gab{overflow:hidden;text-overflow:ellipsis}
`));
}

const put = (el, ...kids) => el.replaceChildren(...kids.flat().filter((k) => k != null && k !== false));

const short = (s, n = 60) => (s && s.length > n ? '…' + s.slice(-(n - 1)) : s || '');

const modeOf = (body) => body.mode || 'phone';

export async function gitSheet(t, place) {
  style();
  const mode = place ? 'panel' : isDesk() ? 'wide' : 'phone';
  const body = h('div.gsh' + (mode === 'wide' ? '.gw' : mode === 'panel' ? '.gp' : ''), {}, skeleton(mode === 'wide' ? 4 : 3, mode === 'wide' ? '' : '.sm'));
  body.place = place || null;
  body.mode = mode;
  if (place) place.mount(body);
  else openSheet({ kind: 'git', tall: true, big: mode === 'wide' }, h('div.sh-title', {}, h('span', {}, 'Git · ' + (t.name || t.id)), h('button.ib', { 'aria-label': 'Close', title: 'Close', onclick: closeSheet, html: icon('x') })), body);
  const r = await ask({ op: 'view', tab: t.id });
  if (!r.ok) { put(body, failRow(r.error, () => gitSheet(t, place), 'The Git screen')); return; }
  render(t, body, r.view);
}

function warnRow(v) {
  if (!v.warnings?.length) return null;
  return h('div.gwarn', {}, v.warnings.map((w) => h('span', {}, '⚠ ' + w)));
}

function srcWord(r) {
  return { remote: 'from its remote', saved: 'saved binding', rule: 'rule ' + (r.rule || ''), none: 'undefined' }[r.source] || '';
}

const needsAccount = (r) => !!(r && !r.refused && r.owner && !r.account && r.ask);

function header(v, body) {
  const r = v.resolved || {};
  const slug = r.owner && r.repo ? `${r.owner}/${r.repo}` : '';
  const src = srcWord(r);
  const ask = r.ask && !needsAccount(r) && v.is_repo ? h('div.gbad', {}, r.ask) : null;
  const prop = v.proposal ? h('div.mnote', {}, `The assistant proposes ${v.proposal.owner}/${v.proposal.repo} as ${v.proposal.account}: Define to use it.`) : null;

  const none = v.is_repo ? null : h('div.gnone', {}, 'This folder is not a git repo yet.', h('small', {}, 'Make it one below; then define which GitHub repo it belongs to, so it is backed up.'));
  const warnings = (v.warnings || []).filter((w) => v.is_repo || !/not in git|not a git repo/i.test(w));
  if (modeOf(body) === 'phone') {
    return [
      h('div.mhead', {},
        h('div.mm.gmono', {}, short(v.top || v.dir, 70)),
        v.is_repo ? h('div.mm', {}, [v.branch, v.remote, v.state, v.fetched].filter(Boolean).join(' · ')) : null,
        v.is_repo ? h('div.mm', {}, slug ? `GitHub ${slug}${r.account ? ' as ' + r.account : ', account not set'} (${src})` : 'No GitHub repo defined') : null),
      none, ask, warnRow({ warnings }), prop,
    ];
  }
  const chip = (text, tone = '', title = '') => h('span.gchip' + (tone ? '.' + tone : ''), { title: title || text }, text);
  const chips = [];
  if (v.is_repo) {
    if (v.branch) chips.push(h('span.gchip', { title: 'Branch' }, '⎇ ', h('b', {}, v.branch)));
    if (v.remote) chips.push(chip(v.remote, v.remote === 'no remote' ? 'warn' : 'dim', 'Remote'));
    if (v.ahead) chips.push(chip('↑ ' + v.ahead + ' ahead', 'warn', 'Commits not pushed'));
    if (v.behind) chips.push(chip('↓ ' + v.behind + ' behind', 'warn', 'Commits to pull'));
    if (v.state && !/ahead|behind/.test(v.state)) chips.push(chip(v.state, 'dim', 'State'));
    if (v.fetched) chips.push(chip(v.fetched, 'dim', 'Last fetch'));
  }
  if (v.is_repo) {
    if (slug) chips.push(chip(`GitHub ${slug}` + (r.account ? ' as ' + r.account : ''), r.account ? 'ok' : 'warn', src ? 'Binding: ' + src : ''));
    else if (!ask) chips.push(chip('No GitHub repo defined', 'warn'));
  }
  for (const w of warnings) chips.push(chip('⚠ ' + w, 'warn'));
  return [
    h('div.ghead', {}, h('div.gpath.gmono', { title: v.top || v.dir }, short(v.top || v.dir, 120)), chips.length ? h('div.gchips', {}, chips) : null),
    none, ask, prop,
  ];
}

const MAIN_ACTS = new Set(['init', 'commit', 'push', 'pull', 'sync', 'fetch', 'branches']);

function manualEl(v, body, go) {
  const kids = actionsEl(v, body, go).filter(Boolean);
  if (!kids.length) return null;
  const n = (v.actions || []).length;
  const d = h('details.gman', { open: !v.is_repo || !!store.get('git.manual', false) },
    h('summary', {}, 'Manual git actions', h('small', {}, n + ' direct')),
    h('div.gmanb', {}, h('div.mnote', {}, 'These run git directly, without the agent. Each one shows what it will do and asks you to confirm.'), ...kids));
  d.addEventListener('toggle', () => { if (v.is_repo) store.set('git.manual', d.open); });
  return d;
}

function actionsEl(v, body, go) {
  const acts = v.actions || [];
  if (modeOf(body) === 'phone') {
    return [h('div.gl', {}, 'Actions'), h('div.list', {}, acts.map((a) => h('button.li' + (a.id === 'init' && !v.is_repo ? '.on' : ''), { type: 'button', disabled: !!a.disabled, title: a.disabled || a.hint, onclick: () => go(a.id) },
      h('span.ll', {}, a.label), h('span.lh', {}, a.disabled || ''))))];
  }
  const b = (a, sec) => h('button.gab' + (sec ? '.g2' : '') + (a.id === 'init' && !v.is_repo ? '.pri' : ''), { type: 'button', disabled: !!a.disabled, title: (a.disabled ? a.disabled + ' · ' : '') + (a.hint || ''), onclick: () => go(a.id) }, a.label);
  const main = acts.filter((a) => MAIN_ACTS.has(a.id));
  const rest = acts.filter((a) => !MAIN_ACTS.has(a.id));
  return [
    main.length ? h('div.gacts.main', {}, main.map((a) => b(a))) : null,
    rest.length ? h('div.gacts', {}, rest.map((a) => b(a, true))) : null,
  ];
}

const UNTRACKED_OPEN = 8;

function changesEl(v, body) {
  const ch = v.changes || {};
  const untracked = ch.untracked || [];
  const nch = (ch.staged_n || 0) + (ch.unstaged_n || 0) + untracked.length;
  if (modeOf(body) === 'phone') {
    const files = (ch.files || []).map((f) => h('div.lrow', {}, h('div.ct', {}, h('b.gmono', {}, (f.staged ? 'S ' : 'M ') + f.path), h('span.cs', {}, `+${f.added ?? 0} -${f.removed ?? 0}`))));
    return [
      nch ? h('div.gl', {}, `Changes: ${ch.staged_n} staged, ${ch.unstaged_n} unstaged, ${untracked.length} untracked (+${ch.added} -${ch.removed})`) : null,
      files.length ? h('div.list', {}, files) : null,
    ];
  }
  if (!v.is_repo) return [];
  if (!nch) return [h('div.gl', {}, 'Changes'), h('div.gclean', {}, 'Nothing to commit: the working tree is clean.')];
  const row = (path, nums) => h('div.gfrow', { title: path }, h('span.gp.gmono', {}, '‎' + path), nums);
  const nums = (f) => h('span.gn.gmono', {}, h('span.a', {}, '+' + (f.added ?? 0)), ' ', h('span.r', {}, '-' + (f.removed ?? 0)));
  const group = (label, rows, total) => {
    if (!total) return null;
    const more = total - rows.length;
    return h('div.ggrp', {}, h('div.gl', {}, `${label} (${total})`), h('div.list', {}, rows, more > 0 ? h('div.gmore', {}, `and ${more} more`) : null));
  };
  const files = ch.files || [];
  const staged = files.filter((f) => f.staged).map((f) => row(f.path, nums(f)));
  const unstaged = files.filter((f) => !f.staged).map((f) => row(f.path, nums(f)));
  const untr = untracked.slice(0, 40).map((p) => row(p, h('span.gn', {}, 'new')));

  const longU = untracked.length > UNTRACKED_OPEN;
  const untrEl = !untracked.length ? null : !longU ? group('Untracked', untr.slice(0, 12), untracked.length)
    : h('details.ggrp.guntr', {}, h('summary', {}, h('div.gl', {}, `Untracked (${untracked.length})`)),
      h('div.list', {}, untr, untracked.length > untr.length ? h('div.gmore', {}, `and ${untracked.length - untr.length} more`) : null));
  return [
    h('div.gl', {}, `Changes · +${ch.added ?? 0} -${ch.removed ?? 0}`),
    group('Staged', staged, ch.staged_n || 0),
    group('Unstaged', unstaged, ch.unstaged_n || 0),
    untrEl,
  ];
}

function commitsEl(v, body) {
  const mode = modeOf(body);
  const list = (v.commits || []).slice(0, mode === 'wide' ? 20 : 12);
  if (!list.length) return [];
  if (mode === 'phone') {
    return [h('div.gl', {}, 'Commits (↑ not pushed)'), h('div.list', {}, list.map((c) => h('div.lrow', {},
      h('div.ct', {}, h('b', { class: c.unpushed ? 'gup' : '' }, (c.unpushed ? '↑ ' : '') + c.subject), h('span.cs', {}, c.when)),
      h('div.cs.gmono', {}, c.hash + ' · ' + c.author))))];
  }
  return [h('div.gl', {}, 'Commits (↑ not pushed)'), h('div.list', {}, list.map((c) => h('div.gcm', { title: c.subject },
    h('div.s', { class: c.unpushed ? 's gup' : 's' }, (c.unpushed ? '↑ ' : '') + c.subject),
    h('div.m.gmono', {}, `${c.hash} · ${c.author} · ${c.when}`))))];
}

function footer(t, body) {
  const sm = modeOf(body) === 'phone' ? '' : '.sm';
  return h(modeOf(body) === 'phone' ? 'div.acts' : 'div.gfoot', {},
    h('button.btn' + sm, { title: 'Read git again', onclick: () => gitSheet(t, body.place) }, 'Refresh'));
}

function sub(body, ...kids) {
  if (modeOf(body) !== 'wide') return put(body, ...kids);
  const box = h('div.gsub');
  put(box, ...kids);
  put(body, box);
}

function render(t, body, v, note) {
  if (v.refused) { put(body, h('div.gbad', {}, 'Refused: ' + v.refused)); return; }
  const go = (id) => onAction(t, body, v, id);
  const r = v.resolved || {};
  const noteEl = note ? h(note.ok ? 'div.gok' : 'div.gbad', {}, note.text) : null;
  const diverged = note && /^Diverged/.test(note.text) ? h('button.btn' + (modeOf(body) === 'phone' ? '' : '.sm'), { onclick: () => go('push_as_branch') }, 'Push as a branch') : null;
  const picker = needsAccount(r) ? accountPicker(t, body, v) : null;
  const agent = agentEl(t, body, v);
  if (modeOf(body) === 'phone') {
    put(body, ...header(v, body), picker, noteEl, diverged, agent, manualEl(v, body, go), ...changesEl(v, body), ...commitsEl(v, body), footer(t, body));
    return;
  }
  const ch = changesEl(v, body);
  const cm = commitsEl(v, body);
  put(body,
    ...header(v, body), picker, noteEl, diverged,
    agent, manualEl(v, body, go),
    ch.length || cm.length ? h('div.gcols', {}, ch.length ? h('div.gcol', {}, ch) : null, cm.length ? h('div.gcol', {}, cm) : null) : null,
    footer(t, body));
}

function gitContext(v) {
  const r = v.resolved || {};
  const ch = v.changes || {};
  const nU = (ch.untracked || []).length;
  const sync = [v.state, v.ahead ? v.ahead + ' ahead' : '', v.behind ? v.behind + ' behind' : '', v.fetched].filter(Boolean);
  const uniq = [...new Set(sync)].join(', ');
  const slug = r.owner && r.repo ? `${r.owner}/${r.repo}` : '';
  const bind = slug ? `${slug} (${srcWord(r) || 'binding'})` : 'none defined';
  const acct = r.account ? r.account
    : r.owner ? `not set for ${r.owner}: do not push; ask me which gh account pushes for ${r.owner}`
      : 'none (no GitHub owner defined): do not push; ask me first';
  return [
    'Git context (from the GodTerm Git panel of this tab):',
    `- Repo: ${v.top || v.dir}`,
    `- Branch: ${v.branch || '(none)'}${uniq ? ' (' + uniq + ')' : ''}`,
    `- Remote: ${v.remote || '(none)'}`,
    `- GitHub binding: ${bind}`,
    `- Pushes as gh account: ${acct}`,
    `- Working tree: ${ch.staged_n || 0} staged, ${ch.unstaged_n || 0} unstaged, ${nU} untracked`,
  ].join('\n');
}

const GIT_RULES = [
  'Rules:',
  '- Never force push (no --force, no --force-with-lease).',
  '- Never rewrite published history (no amend, rebase, reset or squash of commits that are already on the remote).',
  '- On any merge or rebase conflict, stop and ask me; do not resolve it yourself.',
  '- Never commit secrets (.env files, keys, tokens, credentials) or junk (build output, caches, logs, OS files). Respect .gitignore.',
  '- Ask me before adding large files or untracked binary files.',
  '- Push only as the gh account above; if git would push as a different account, stop and tell me.',
].join('\n');

const COMMIT_WORK = 'Commit all meaningful changes with clear commit messages (separate commits for unrelated changes). Leave out secrets and junk; for untracked files, include only the ones that clearly belong in the repo and ask about the rest.';
const SYNC_WORK = 'pull from the remote (rebase my unpushed commits onto it, or merge if a rebase is not safe), then push';

const AGENT_ACTS = [
  { id: 'commit_sync', label: 'Commit and sync', main: true, hint: 'Commit all changes, pull, then push',
    need: (v) => (!v.is_repo ? 'not a git repo' : null),
    task: (v) => noRemote(v) ? COMMIT_WORK + ' There is no remote yet, so do not pull or push; tell me.' : `${COMMIT_WORK} Then ${SYNC_WORK}. Report what you committed, pulled and pushed, and the final git status.` },
  { id: 'commit', label: 'Commit only', hint: 'Commit all changes; no pull or push',
    need: (v) => (!v.is_repo ? 'not a git repo' : !dirtyN(v) ? 'nothing to commit' : null),
    task: () => `${COMMIT_WORK} Do not pull or push. Report the commits you made.` },
  { id: 'sync', label: 'Sync with remote', hint: 'Pull and push; no new commits',
    need: (v) => (!v.is_repo ? 'not a git repo' : noRemote(v) ? 'no remote yet' : null),
    task: () => `Bring this branch in sync with its remote without making new commits: fetch, ${SYNC_WORK} any unpushed commits. Leave uncommitted changes alone (if the pull needs a clean tree, stash them and restore them after). Report what you pulled and pushed, and the final git status.` },
  { id: 'review', label: 'Review changes', hint: 'Summary and suggested commits; changes nothing',
    need: (v) => (!v.is_repo ? 'not a git repo' : !dirtyN(v) ? 'nothing to review' : null),
    task: () => 'Review the uncommitted changes: summarize the diff, point out anything that looks wrong or should not be committed, and suggest how to split it into commits with messages. Change nothing: no staging, commits or file edits.' },
  { id: 'cleanup', label: 'Clean up untracked', hint: 'Suggest .gitignore rules; changes nothing until asked',
    need: (v) => (!v.is_repo ? 'not a git repo' : !(v.changes?.untracked || []).length ? 'no untracked files' : null),
    task: () => 'Look at the untracked files: suggest .gitignore additions for the junk, and say which files should be committed and which deleted. Change nothing until I say which suggestions to apply.' },
];

const noRemote = (v) => !v.remote || v.remote === 'no remote';
const dirtyN = (v) => { const c = v.changes || {}; return (c.staged_n || 0) + (c.unstaged_n || 0) + (c.untracked || []).length; };
const BUSY = new Set(['working', 'background', 'busy', 'approval', 'waiting', 'starting']);

export function gitPrompt(v, task) {
  return `[Git panel] ${task}\n\n${gitContext(v)}\n\n${GIT_RULES}`;
}

function receiptText(r) {
  const res = r.result || r;
  const d = res.prompt || (res.deliveries || [])[0] || {};
  const failed = (res.failed || [])[0];
  if (failed) return { tone: 'bad', text: 'Not sent: ' + (failed.reason || 'it failed') };
  const st = d.state || d.status || '';
  const queued = d.mode === 'queue' && /queued|received|sending|sent/.test(st) && st !== 'started';
  const word = { done: 'done', started: 'it started', received: 'the agent has it', sent: 'typed in', sending: 'sending', queued: 'queued', unconfirmed: 'sent (no transcript to confirm)', dropped: 'dropped: send it again', interrupted: 'interrupted: send it again' }[st] || st;
  const bad = /dropped|interrupted|failed/.test(st + (d.status || ''));
  const text = (res.say ? res.say.trim() : 'Sent to the agent.') + (word ? ` Receipt: ${word}` : '') + (d.mode_note ? ` (${d.mode_note})` : '') + (d.reason ? ` (${d.reason})` : '');
  return { tone: bad ? 'bad' : queued ? 'warn' : 'ok', text };
}

function agentEl(t, body, v) {
  if (v.refused) return null;

  if (AGENT_ACTS.every((a) => a.need(v))) return null;
  const live = () => tabById(t.id) || t;
  const st = h('div.gaist');
  const say = (text, tone = '') => { st.className = 'gaist' + (tone ? ' ' + tone : ''); st.textContent = text || ''; };
  const busyNote = () => {
    const s = (live().state || '').split(' ')[0];
    return BUSY.has(s) || live().busy ? `This tab's agent is ${stateWords(s)}: the request is queued and runs after its current work.` : '';
  };
  let sending = false;
  const btns = [];
  async function sendTask(label, task) {
    if (sending) return;
    sending = true;
    btns.forEach((b) => (b.disabled = true));
    const busy = busyNote();
    say((busy ? busy + ' ' : '') + `Sending "${label}" to the agent...`, busy ? 'warn' : '');

    let r = await tool('send_prompt', { tab: t.id, text: gitPrompt(v, task), mode: 'queue', expect_reply: false }, 30000);

    const conf = r.needs_confirmation ? r : r.result?.needs_confirmation ? r.result : null;
    if (conf?.token) r = await tool('send_prompt', { confirm_token: conf.token }, 30000, { user_says: 'yes' });
    sending = false;
    btns.forEach((b) => (b.disabled = !!b.dataset.off));
    if (r.ok === false || r.error) {
      const e = r.error || r.result?.error || 'It did not work';
      say('Not sent: ' + e, 'bad');
      toast('Git request not sent: ' + e, 'error');
      return;
    }
    const rc = receiptText(r);
    say(`${label}: ${rc.text}`, rc.tone);
    toast(`${label}: ${rc.text}`, rc.tone === 'bad' ? 'error' : rc.tone === 'warn' ? 'warn' : 'ok');
  }
  const mk = (a) => {
    const off = a.need(v);
    const b = h('button.gab' + (a.main ? '.pri.gaibig' : ''), { type: 'button', disabled: !!off, title: (off ? off + ' · ' : '') + a.hint, onclick: () => sendTask(a.label, a.task(v)) }, a.label);
    if (off) b.dataset.off = '1';
    btns.push(b);
    return b;
  };
  const main = AGENT_ACTS.filter((a) => a.main).map(mk);
  const rest = AGENT_ACTS.filter((a) => !a.main).map(mk);
  const q = h('input', { type: 'text', placeholder: 'Ask about git…', autocomplete: 'off', enterkeyhint: 'send' });
  const askIt = () => {
    const text = q.value.trim();
    if (!text) return q.focus();
    sendTask('Question', `${text}\n\n(Answer about this repo. Change nothing unless the request above clearly asks for a change.)`).then(() => { if (!st.classList.contains('bad')) q.value = ''; });
  };
  q.addEventListener('keydown', (e) => { if (e.key === 'Enter' && !e.isComposing) { e.preventDefault(); askIt(); } });
  const go = h('button.gab', { type: 'button', onclick: askIt }, 'Send');
  btns.push(go);
  const busy = busyNote();
  if (busy) say(busy, 'warn');
  return h('div.gai', {},
    h('div.gait', { title: GIT_RULES }, "Ask this tab's agent", h('small', {}, 'It works in its own session: never force pushes, never rewrites published history, stops and asks on conflicts.')),

    modeOf(body) === 'wide' ? h('div.gairow', {}, main, rest) : [h('div.gairow', {}, main), h('div.gairow.sec', {}, rest)],
    h('div.gaiask', {}, q, go),
    st);
}

function firstMessage(v) {
  const names = [...(v.changes?.files || []).map((f) => f.path), ...(v.changes?.untracked || [])].map((p) => p.split('/').pop());
  if (!names.length) return '';
  return names.length <= 3 ? 'Update ' + names.join(', ') : `Update ${names[0]}, ${names[1]} and ${names.length - 2} more`;
}

function onAction(t, body, v, id) {
  const back = () => render(t, body, v);
  const prep = (action, extra = {}) => prepare(t, body, v, { action, ...extra });
  if (id === 'commit') {
    const msg = h('textarea', { rows: 4 }, v.draft || firstMessage(v));
    const all = h('input', { type: 'checkbox', checked: true });
    sub(body, ...header(v, body),
      h('div.gl', {}, 'Commit'), v.draft ? h('div.mnote', {}, 'Drafted by the assistant; edit it before you commit.') : null, msg,
      h('label.gck', {}, all, 'Everything (new files too); off: staged only'),
      h('div.acts', {}, h('button.btn.pri', { onclick: () => prep('commit', { message: msg.value, all: all.checked }) }, 'Check'), h('button.btn', { onclick: back }, 'Back')));
    return msg.focus();
  }
  if (id === 'define' || id === 'create') {
    const r = v.resolved || {};
    const p = v.proposal || {};
    const sel = (items, cur) => h('select', {}, (items.length ? items : ['']).map((o) => h('option', { value: o, selected: o === cur }, o || '(none known)')));
    const owner = sel(v.owners || [], p.owner || r.owner);
    const repo = h('input', { type: 'text', value: p.repo || r.repo || '' });
    const acct = sel(v.accounts || [], p.account || r.account);
    const create = id === 'create';
    sub(body, ...header(v, body),
      h('div.gl', {}, create ? 'Create a private repo (never public)' : 'Which GitHub repo this folder belongs to'),
      h('div.mnote', {}, 'Owner'), owner, h('div.mnote', {}, 'Repo name'), repo, h('div.mnote', {}, 'gh account'), acct,
      h('div.acts', {}, h('button.btn.pri', { onclick: () => prep(create ? 'create' : 'bind', create ? { owner: owner.value, name: repo.value.trim(), account: acct.value, push: true } : { owner: owner.value, repo: repo.value.trim(), account: acct.value }) }, create ? 'Check' : 'Check and save'),
        h('button.btn', { onclick: back }, 'Back')));
    return;
  }
  if (id === 'branches') {
    const stash = h('input', { type: 'checkbox' });
    const name = h('input', { type: 'text', placeholder: 'new-branch-name' });
    const rows = (v.branches || []).map((b) => h('button.li', { type: 'button', disabled: b.current, onclick: () => prep('switch', { branch: b.name, stash: stash.checked }) },
      h('span.ll', {}, (b.current ? '● ' : '') + b.name), h('span.lh', {}, (b.upstream || 'no upstream') + ' · ' + b.when)));
    sub(body, ...header(v, body), h('div.gl', {}, 'Switch to'), h('div.list', {}, rows),
      h('label.gck', {}, stash, 'Stash uncommitted changes first'),
      h('div.gl', {}, 'New branch from here'), name,
      h('div.acts', {}, h('button.btn', { onclick: () => prep('create_branch', { name: name.value.trim() }) }, 'Check and create'), h('button.btn', { onclick: back }, 'Back')));
    return;
  }
  prep(id);
}

async function prepare(t, body, v, frame) {
  sub(body, ...header(v, body), h('div.mnote', {}, 'Checking...'));
  const r = await ask({ op: 'prepare', tab: t.id, ...frame });
  if (!r.ok) { render(t, body, v, { ok: false, text: r.error || 'It did not work' }); return; }
  const p = r.plan;
  const can = !(p.hard_blocks || []).length;
  const typed = p.needs_typed ? h('input', { type: 'text', placeholder: `type "${r.override}" to push past the findings`, autocomplete: 'off' }) : null;
  const yes = h('button.btn.pri', { disabled: !can, onclick: async () => {
    yes.disabled = true;
    (body.querySelector('.gsub') || body).append(h('div.mnote', {}, p.title + '...'));
    const x = await ask({ op: 'run', tab: t.id, plan_id: r.plan_id, typed: typed ? typed.value : undefined });
    if (x.ok) toast(x.said, 'ok'); else toast(x.error || 'It did not work', 'error');
    render(t, body, x.view || v, { ok: !!x.ok, text: x.ok ? x.said : x.error });
  } }, p.needs_typed ? 'Push' : 'Yes, do it');
  sub(body, ...header(v, body),
    h('div.gl', {}, p.title),
    h('div.gplan', {}, (p.lines || []).map((l) => h('div', {}, l))),
    ...(p.warnings || []).map((w) => h('div.gwarn', {}, h('span', {}, '⚠ ' + w))),
    ...(p.hard_blocks || []).map((b) => h('div.gbad', {}, '✕ ' + b)),
    ...(p.soft_blocks || []).slice(0, 20).map((b) => h('div.gbad', {}, '✕ ' + b)),
    !can ? h('div.mnote', {}, 'Fix these first; nothing was changed.') : null,
    p.needs_typed && can ? h('div.gbad', {}, 'Possible secrets: push only if you are sure.') : null,
    typed,
    h('div.acts', {}, can ? yes : null, h('button.btn', { onclick: () => render(t, body, v) }, can ? 'Cancel' : 'Back')));
}

const LOGIN_CMD = 'gh auth login --hostname github.com --git-protocol https --web';
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

async function copyText(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch {}
  const ta = h('textarea', { style: 'position:fixed;opacity:0' }, text);
  document.body.append(ta); ta.select();
  let ok = false;
  try { ok = document.execCommand('copy'); } catch {}
  ta.remove();
  return ok;
}

function accountPicker(t, body, v) {
  const owner = v.resolved.owner;
  const opts = h('div.opts');
  const st = h('div.st');
  const login = h('div.ggrp');
  const box = h('div.gpick', {},
    h('div.q', {}, 'Which gh account pushes for ', h('b', {}, owner), '?',
      h('small', {}, 'It is not in [git.owners] yet. Pick one to save it there (config.toml), or add a gh login.')),
    opts, st, login);

  let accts = (v.accounts || []).map((name) => ({ name, logged_in: true }));
  let fresh = '';
  let busy = false;
  const say = (text, tone = '') => { st.className = 'st' + (tone ? ' ' + tone : ''); st.textContent = text || ''; };
  const paint = () => put(opts,
    accts.map((a) => h('button.gab', {
      type: 'button', disabled: busy || !a.logged_in,
      title: a.logged_in ? `Save: ${owner} pushes as ${a.name}` : `${a.name} is logged out of gh; log in again with Add account`,
      onclick: () => choose(a.name),
    }, a.name, a.active ? h('small', {}, 'active') : null, a.name === fresh ? h('small', {}, 'new') : null, !a.logged_in ? h('small', {}, 'logged out') : null)),
    h('button.gab.g2', { type: 'button', disabled: busy, onclick: () => startLogin() }, '+ Add account...'));
  if (!accts.length) say('Asking gh which accounts are logged in...');
  paint();

  async function loadAccounts(tries = 6) {
    for (let i = 0; i < tries && box.isConnected !== false; i++) {
      const r = await ask({ op: 'cfg_view' }, 20000);
      if (r.ok && Array.isArray(r.view?.accounts)) {
        accts = r.view.accounts;
        if (!accts.length) say('No gh account is logged in on this Mac: add one.');
        else if (/^Asking/.test(st.textContent)) say('');
        paint();
        return;
      }
      await wait(1500);
    }
  }

  async function choose(name) {
    busy = true; paint();
    say(`Saving ${owner} -> ${name}...`);
    const fail = (e) => { busy = false; paint(); say(e || 'It did not work', 'bad'); };
    const cv = await ask({ op: 'cfg_view' }, 20000);
    if (!cv.ok) return fail(cv.error);
    const draft = JSON.parse(JSON.stringify(cv.view.draft || { owners: [], rules: [], binds: [] }));
    draft.owners = (draft.owners || []).filter((o) => String(o.owner).toLowerCase() !== owner.toLowerCase());
    draft.owners.push({ owner, account: name });
    draft.binds = [];
    const ck = await ask({ op: 'cfg_check', draft }, 20000);
    if (!ck.ok) return fail(ck.error);
    if (ck.check?.errors?.length) return fail(ck.check.errors.join('; '));
    if (ck.plan_id) {
      const sv = await ask({ op: 'cfg_save', plan_id: ck.plan_id }, 20000);
      if (!sv.ok) return fail(sv.error);
    }
    const warn = (ck.check?.warnings || []).filter((w) => w.includes(name));
    toast(`Saved: ${name} pushes for ${owner}` + (warn.length ? ' (' + warn[0] + ')' : ''), warn.length ? 'warn' : 'ok');
    gitSheet(t, body.place);
  }

  function paintLogin(l) {
    const sm = (label, fn, cls = '') => h('button.gab' + cls, { type: 'button', onclick: fn }, label);
    if (!l || !l.state || l.state === 'cancelled') return put(login);
    if (l.state === 'starting') return put(login, h('div.st', {}, 'Starting gh auth login...'), h('div.gacts', {}, sm('Cancel', cancel, '.g2')));
    if (l.state === 'code') {
      return put(login,
        h('div.st', {}, 'Copy this one-time code, open GitHub, paste it and sign in as the account to add:'),
        h('div.gcode', {}, l.code || '...'),
        h('div.gacts', {},
          sm('Copy code', async () => toast((await copyText(l.code || '')) ? 'Code copied' : 'Copy did not work; select the code', 'ok')),
          sm('Open ' + (l.url || 'GitHub').replace(/^https:\/\//, ''), () => window.open(l.url || 'https://github.com/login/device', '_blank', 'noopener')),
          sm('Cancel', cancel, '.g2')),
        h('div.st', {}, 'Waiting for GitHub...'));
    }
    if (l.state === 'done') return put(login, h('div.st.ok', {}, `Logged in as ${l.account || 'the new account'}. Pick it above to save it for ${owner}.`));
    put(login, h('div.st.bad', {}, 'The gh login did not finish: ' + (l.error || l.state)), h('div.gacts', {}, sm('Try again', () => startLogin())));
  }

  function noServerLogin() {
    put(login,
      h('div.st', {}, 'This GodTerm cannot run the gh login from here yet. Run this in a terminal tab, then press Refresh:'),
      h('code', {}, LOGIN_CMD),
      h('div.gacts', {}, h('button.gab', { type: 'button', onclick: async () => toast((await copyText(LOGIN_CMD)) ? 'Command copied' : 'Copy did not work', 'ok') }, 'Copy command')));
  }

  async function follow(l) {
    paintLogin(l);
    while (l && (l.state === 'starting' || l.state === 'code') && box.isConnected) {
      await wait(1500);
      if (!box.isConnected) return;
      const r = await ask({ op: 'cfg_gh_login_poll' }, 20000);
      if (!r.ok) { paintLogin({ state: 'failed', error: r.error }); return; }
      l = r.login;
      paintLogin(l);
    }
    if (l?.state === 'done') {
      fresh = l.account || '';
      await wait(2500); 
      await loadAccounts();
      if (fresh && !accts.some((a) => a.name === fresh)) accts.push({ name: fresh, logged_in: true });
      paint();
    }
  }

  async function startLogin() {
    say('');
    const r = await ask({ op: 'cfg_gh_login' }, 30000);
    if (!r.ok) return /unknown git op/.test(r.error || '') ? noServerLogin() : paintLogin({ state: 'failed', error: r.error });
    follow(r.login);
  }

  async function cancel() {
    const r = await ask({ op: 'cfg_gh_login_cancel' }, 20000);
    paintLogin(r.login || { state: 'cancelled' });
  }

  queueMicrotask(async () => {
    await loadAccounts();
    const r = await ask({ op: 'cfg_gh_login_poll' }, 20000);
    if (r.ok && (r.login?.state === 'starting' || r.login?.state === 'code')) follow(r.login);
  });
  return box;
}
