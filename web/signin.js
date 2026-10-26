// The theme first (this runs in <head>, before the page paints): the
// choice kept on this device by the app (js/theme.js), Dark by default.
(function () {
  var p = 'dark';
  try { var v = JSON.parse(localStorage.getItem('gt.theme')); if (v === 'light' || v === 'system') p = v; } catch (e) {}
  var t = p === 'system' ? (matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark') : p;
  document.documentElement.setAttribute('data-theme', t);
  var m = document.querySelector('meta[name=theme-color]');
  if (m) m.setAttribute('content', t === 'light' ? '#f4f1ea' : '#1b1d21');
  var st = document.querySelector('meta[name=apple-mobile-web-app-status-bar-style]');
  if (st) st.setAttribute('content', t === 'light' ? 'default' : 'black-translucent');
})();

// A pasted sign in link works as well as the bare token.
addEventListener('DOMContentLoaded', () => {
  document.getElementById('f').addEventListener('submit', () => {
    const i = document.getElementById('t');
    const m = i.value.match(/[?&]t=([0-9a-f]{16,})/i);
    if (m) i.value = m[1];
    i.value = i.value.trim();
  });
});
