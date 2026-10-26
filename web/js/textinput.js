// Phone keyboards and xterm's hidden textarea.
//
// iOS dictation, autocorrect, predictive text and IMEs edit the textarea
// in place: a dictation's partial results replace the phrase so far
// ("the", then "the interaction", then "the interaction of" ...), an
// autocorrect replaces the last word. xterm sends what each input event
// carries (or the whole value when its length stays the same), so a
// dictated sentence reached the tab as every partial result in full, one
// after the other: thousands of repeated characters in a prompt.
//
// On touch devices the textarea is owned here instead: its value is the
// text typed since the last key that is not text (Enter, an arrow, a key
// bar key), and every change to it goes to the tab as the difference from
// what was sent: one DEL (0x7f) per character after the common start, then
// the new end. A composition (marked text: an IME, some dictation) goes
// once it ends. Paste goes as one bracketed paste. xterm never sees these
// events; keys that are not text (Enter, arrows, Tab, Esc, Ctrl and Cmd
// combinations) still go through xterm as before.

const cps = (s) => Array.from(s);

// From `a` to `b`: how many characters to delete from the end of `a` and
// what to type after that (code points, so an emoji is one character).
export function textDiff(a, b) {
  if (a === b) return { del: 0, ins: '' };
  const x = cps(a), y = cps(b);
  let p = 0;
  while (p < x.length && p < y.length && x[p] === y[p]) p++;
  return { del: x.length - p, ins: y.slice(p).join('') };
}

// Own the textarea of terminal `t` (opened in `root`). `out(data, how)`
// sends to the tab; `how` is 'text' (typed or dictated), 'del' (the DELs
// before it) or 'paste'. Returns {commit, reset, base}.
export function ownTextarea(t, root, out) {
  const ta = t.textarea;
  if (!ta) return null;
  // What the tab has from this textarea (sent, since the last reset).
  let base = '';
  let composing = false;
  const state = { get base() { return base; }, get composing() { return composing; } };

  // Send the textarea's change since the last sync.
  const sync = (how = 'text') => {
    if (composing) return;
    const v = ta.value;
    if (v === base) return;
    const { del, ins } = textDiff(base, v);
    base = v;
    if (del) out('\x7f'.repeat(del), 'del');
    if (ins) out(ins, how);
  };
  // Start again from an empty textarea (after a key that is not text: the
  // tab's line moved on, so the old text is not there to diff against).
  const reset = () => {
    composing = false;
    base = '';
    if (ta.value) ta.value = '';
  };
  // Before a key that is not text: what a composition still holds goes
  // first, then the textarea starts again.
  const commit = () => {
    if (composing) { composing = false; sync(); }
    reset();
  };
  // A change nobody sent (xterm clears the value on blur and on Enter):
  // taken as the new start, nothing sent for it.
  const adopt = () => { if (!composing && ta.value !== base) base = ta.value; };
  const mine = (e) => e.target === ta;
  const opt = { capture: true };

  root.addEventListener('beforeinput', (e) => {
    if (!mine(e)) return;
    e.stopPropagation();
    adopt();
    // A line break the keyboard inserts (dictation's "new line", a
    // Return xterm did not take): Enter, as xterm sends it.
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
    // Safari updates the value after compositionend in some cases: the
    // input event that follows syncs again (nothing twice: base moved).
    sync();
  }, opt);
  // Text paste: once, bracketed when the app asked for it. (Images are
  // taken earlier by the pane.)
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
    // An IME or dictation at work: the textarea, not xterm (xterm would
    // diff the textarea itself and send it again).
    if (composing || e.isComposing || e.keyCode === 229) { e.stopPropagation(); return; }
    if (e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === 'Backspace') {
      e.stopPropagation();
      // Typed text here: the textarea deletes it and the change goes out.
      // An empty textarea: DEL to the tab, as xterm sends it.
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
    // A printable key: inserted by the browser, sent by the input event.
    if (cps(e.key).length === 1) {
      e.stopPropagation();
      const n = ta.value.length;
      if (ta.selectionStart !== n || ta.selectionEnd !== n) ta.setSelectionRange(n, n);
    }
  }, opt);
  // xterm sends printable keys on keypress: not here (the input event does).
  root.addEventListener('keypress', (e) => {
    if (!mine(e) || e.ctrlKey || e.metaKey || e.altKey) return;
    if (e.key === 'Enter') return;
    e.stopPropagation();
  }, opt);
  return Object.assign(state, { commit, reset, sync });
}
