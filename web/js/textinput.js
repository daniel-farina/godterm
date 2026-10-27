const cps = (s) => Array.from(s);

export function textDiff(a, b) {
  if (a === b) return { del: 0, ins: '' };
  const x = cps(a), y = cps(b);
  let p = 0;
  while (p < x.length && p < y.length && x[p] === y[p]) p++;
  return { del: x.length - p, ins: y.slice(p).join('') };
}

export function ownTextarea(t, root, out) {
  const ta = t.textarea;
  if (!ta) return null;

  let base = '';
  let composing = false;
  const state = { get base() { return base; }, get composing() { return composing; } };

  const sync = (how = 'text') => {
    if (composing) return;
    const v = ta.value;
    if (v === base) return;
    const { del, ins } = textDiff(base, v);
    base = v;
    if (del) out('\x7f'.repeat(del), 'del');
    if (ins) out(ins, how);
  };

  const reset = () => {
    composing = false;
    base = '';
    if (ta.value) ta.value = '';
  };

  const commit = () => {
    if (composing) { composing = false; sync(); }
    reset();
  };

  const adopt = () => { if (!composing && ta.value !== base) base = ta.value; };
  const mine = (e) => e.target === ta;
  const opt = { capture: true };

  root.addEventListener('beforeinput', (e) => {
    if (!mine(e)) return;
    e.stopPropagation();
    adopt();

    if ((e.inputType === 'insertLineBreak' || e.inputType === 'insertParagraph') && !composing) {
      e.preventDefault();
      out('\r', 'enter');
    }
  }, opt);
  root.addEventListener('input', (e) => {
    if (!mine(e)) return;
    e.stopPropagation();
    if (composing || e.isComposing) return;
    sync(e.inputType === 'insertFromPaste' || e.inputType === 'insertFromDrop' ? 'paste' : 'text');
  }, opt);
  root.addEventListener('compositionstart', (e) => {
    if (!mine(e)) return;
    e.stopPropagation();
    adopt();
    composing = true;
  }, opt);
  root.addEventListener('compositionupdate', (e) => { if (mine(e)) e.stopPropagation(); }, opt);
  root.addEventListener('compositionend', (e) => {
    if (!mine(e)) return;
    e.stopPropagation();
    composing = false;

    sync();
  }, opt);

  root.addEventListener('paste', (e) => {
    if (!mine(e)) return;
    const text = e.clipboardData?.getData('text/plain');
    if (text == null) return;
    e.preventDefault();
    e.stopPropagation();
    commit();
    if (text) out(text, 'paste');
  }, opt);
  root.addEventListener('keydown', (e) => {
    if (!mine(e)) return;

    if (composing || e.isComposing || e.keyCode === 229) { e.stopPropagation(); return; }
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === 'Backspace') {
      e.stopPropagation();

      if (ta.value && base === ta.value) {
        const n = ta.value.length;
        if (ta.selectionStart !== n || ta.selectionEnd !== n) ta.setSelectionRange(n, n);
        return;
      }
      e.preventDefault();
      reset();
      out('\x7f', 'key');
      return;
    }

    if (cps(e.key).length === 1) {
      e.stopPropagation();
      const n = ta.value.length;
      if (ta.selectionStart !== n || ta.selectionEnd !== n) ta.setSelectionRange(n, n);
    }
  }, opt);

  root.addEventListener('keypress', (e) => {
    if (!mine(e) || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === 'Enter') return;
    e.stopPropagation();
  }, opt);
  return Object.assign(state, { commit, reset, sync });
}
