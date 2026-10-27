import { webkit, devices } from 'playwright';
import fs from 'fs';

const URL0 = process.argv[2];
const SHOT = process.argv[3] || '';
const browser = await webkit.launch();
const ctx = await browser.newContext({ ...devices['iPhone 15 Pro'] });
await ctx.addInitScript(() => {
  window.__sent = [];
  const dec = new TextDecoder();
  const orig = WebSocket.prototype.send;
  WebSocket.prototype.send = function (d) {
    if (d instanceof Uint8Array && d[0] === 1) window.__sent.push(dec.decode(d.subarray(5)));
    return orig.call(this, d);
  };
});
const page = await ctx.newPage();
page.on('pageerror', (e) => console.log('PAGE ERROR', e.message));
await page.goto(URL0);
await page.waitForSelector('.pane .xterm-helper-textarea');
await page.waitForFunction(() => document.querySelector('.pane-msg')?.hidden);
await page.waitForTimeout(800);

const box = await page.locator('.pane[data-i="0"] .pane-body').boundingBox();
await page.touchscreen.tap(box.x + box.width / 2, box.y + box.height / 2);
await page.waitForTimeout(300);

const run = (name, fn) => page.evaluate(async ([name, src]) => {
  const ta = document.querySelector('.pane[data-i="0"] .xterm-helper-textarea');
  const fire = (type, init = {}) => {
    let ev;
    if (type.startsWith('composition')) ev = new CompositionEvent(type, { bubbles: true, cancelable: true, data: init.data || '' });
    else if (type === 'beforeinput' || type === 'input') ev = new InputEvent(type, { bubbles: true, cancelable: type === 'beforeinput', inputType: init.inputType, data: init.data ?? null, isComposing: !!init.isComposing });
    else if (type.startsWith('key')) ev = new KeyboardEvent(type, { bubbles: true, cancelable: true, key: init.key, code: init.code || '', keyCode: init.keyCode || 0, which: init.keyCode || 0, charCode: type === 'keypress' ? (init.key || '').charCodeAt(0) : 0 });
    else if (type === 'paste') { const dt = new DataTransfer(); dt.setData('text/plain', init.text); ev = new ClipboardEvent('paste', { bubbles: true, cancelable: true, clipboardData: dt }); }
    ta.dispatchEvent(ev);
    return ev;
  };

  const replace = (s, e, text, inputType = 'insertText', isComposing = false) => {
    ta.setSelectionRange(s, e);
    const b = fire('beforeinput', { inputType, data: text, isComposing });
    if (!b.defaultPrevented) ta.setRangeText(text, s, e, 'end');
    fire('input', { inputType, data: text, isComposing });
  };

  const key = (k) => {
    const kc = k === ' ' ? 32 : k.toUpperCase().charCodeAt(0);
    const d = fire('keydown', { key: k, keyCode: kc });
    const p = fire('keypress', { key: k, keyCode: kc });
    if (!d.defaultPrevented && !p.defaultPrevented) {
      const n = ta.value.length;
      replace(ta.selectionStart ?? n, ta.selectionEnd ?? n, k);
    }
    fire('keyup', { key: k, keyCode: kc });
  };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  window.__sent.length = 0;
  await (0, eval)('(' + src + ')')({ ta, fire, replace, key, sleep });
  await sleep(150);
  return { name, sent: window.__sent.slice(), value: ta.value };
}, [name, fn.toString()]);

const apply = (chunks0) => {
  const chunks = chunks0.map((c) => c.replace(/\x1b\[20[01]~/g, ''));
  let line = [];
  for (const c of chunks) for (const ch of Array.from(c)) { if (ch === '\x7f') line.pop(); else line.push(ch); }
  return line.join('');
};
const show = (s) => JSON.stringify(s).replace(/\\u007f/g, '⌫');

const PARTIALS = ['the', 'the interaction', 'the interaction of', 'the interaction of micro', 'the interaction of the microphone', 'the interaction of the microphone was confusing', 'the interaction of the microphone was confusing.'];
const FINAL = 'the interaction of the microphone was confusing.';
const results = [];
const check = (r, want, maxBytes) => {
  const got = apply(r.sent);
  const raw = r.sent.join('');
  const ok = got === want && (maxBytes == null || Array.from(raw).length <= maxBytes);
  results.push({ name: r.name, ok, got, want, raw: show(raw), bytes: Array.from(raw).length, frames: r.sent.length });
  console.log(`${ok ? 'ok  ' : 'FAIL'} ${r.name}: ${Array.from(raw).length} chars in ${r.sent.length} frames -> ${show(got)}${ok ? '' : '  want ' + show(want) + '  raw ' + show(raw)}`);
};
const enter = () => run('enter', async ({ fire }) => { fire('keydown', { key: 'Escape', code: 'Escape', keyCode: 27 }); });

check(await run('dictation, insertText replacing the phrase', async ({ ta, replace, sleep }) => {
  const P = ['the', 'the interaction', 'the interaction of', 'the interaction of micro', 'the interaction of the microphone', 'the interaction of the microphone was confusing', 'the interaction of the microphone was confusing.'];
  const s = ta.value.length; let prev = '';
  for (const p of P) { replace(s, s + prev.length, p); prev = p; await sleep(30); }
}), FINAL, FINAL.length + 30);
await enter();

check(await run('dictation, insertReplacementText', async ({ ta, replace, sleep }) => {
  const P = ['the', 'the interaction', 'the interaction of', 'the interaction of micro', 'the interaction of the microphone', 'the interaction of the microphone was confusing'];
  const s = ta.value.length; let prev = '';
  for (const p of P) { replace(s, s + prev.length, p, 'insertReplacementText'); prev = p; await sleep(30); }
  replace(s, s + prev.length, prev + '.', 'insertFromDictation');
}), FINAL, FINAL.length + 30);
await enter();

check(await run('dictation, composition updates', async ({ ta, fire, replace, sleep }) => {
  const P = ['the', 'the interaction', 'the interaction of', 'the interaction of micro', 'the interaction of the microphone', 'the interaction of the microphone was confusing.'];
  const s = ta.value.length; let prev = '';
  fire('compositionstart', { data: '' });
  for (const p of P) { fire('compositionupdate', { data: p }); replace(s, s + prev.length, p, 'insertCompositionText', true); prev = p; await sleep(30); }
  fire('compositionend', { data: prev });
  fire('input', { inputType: 'insertFromComposition', data: prev });
}), FINAL, FINAL.length);
await enter();

check(await run('typing + autocorrect + predictive', async ({ ta, key, replace }) => {
  for (const k of 'hello teh') key(k);

  const i = ta.value.lastIndexOf('teh');
  replace(i, i + 3, 'the', 'insertReplacementText');
  key(' ');

  const n = ta.value.length;
  replace(n, n, 'microphone ', 'insertText');
}), 'hello the microphone ', 40);
await enter();

check(await run('paste', async ({ fire }) => { fire('paste', { text: 'pasted once' }); }), 'pasted once', 30);
await enter();

check(await run('typed + dictation + backspace', async ({ ta, key, replace, fire }) => {
  for (const k of 'ok ') key(k);
  const s = ta.value.length; let prev = '';
  for (const p of ['so', 'so far', 'so far so good']) { replace(s, s + prev.length, p); prev = p; }
  for (let j = 0; j < 2; j++) {
    const d = fire('keydown', { key: 'Backspace', code: 'Backspace', keyCode: 8 });
    if (!d.defaultPrevented) { const n = ta.value.length; replace(n - 1, n, '', 'deleteContentBackward'); }
  }
}), 'ok so far so go', 40);
await enter();

check(await run('backspace when empty', async ({ ta, fire, replace }) => {
  const d = fire('keydown', { key: 'Backspace', code: 'Backspace', keyCode: 8 });
  if (!d.defaultPrevented && ta.value) { const n = ta.value.length; replace(n - 1, n, '', 'deleteContentBackward'); }
}), '', 1);

check(await run('dictation left in the prompt', async ({ ta, replace, sleep }) => {
  const s = ta.value.length; let prev = '';
  for (const p of ['dictated', 'dictated once', 'dictated once into', 'dictated once into the tab']) { replace(s, s + prev.length, p); prev = p; await sleep(30); }
}), 'dictated once into the tab', 40);
await page.waitForTimeout(800);
if (SHOT) await page.screenshot({ path: SHOT });
fs.writeFileSync(process.env.OUT || '/dev/null', JSON.stringify(results, null, 1));
const bad = results.filter((r) => !r.ok).length;
console.log(bad ? `${bad} FAILED` : 'all passed');
await browser.close();
process.exit(bad ? 1 : 0);
