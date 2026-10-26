// The theme, set before the first paint (a plain script in <head>; the
// page's policy allows no inline script). Dark is the default; Light and
// Match system are the other choices, kept per device in localStorage as
// gt.theme (the store in util.js). It puts data-theme="dark|light" on
// <html> (app.css keys the light colors off it) and points the browser's
// theme color and iOS's status bar at the header's color. signin.js and
// offline.html carry the same few lines for their own pages.
(function () {
  var COLORS = { dark: '#23262b', light: '#ebe7de' };
  var sys = matchMedia('(prefers-color-scheme: light)');
  function pref() {
    try { var v = JSON.parse(localStorage.getItem('gt.theme')); return v === 'light' || v === 'system' ? v : 'dark'; } catch (e) { return 'dark'; }
  }
  function apply(p) {
    var t = p === 'system' ? (sys.matches ? 'light' : 'dark') : p, root = document.documentElement;
    root.setAttribute('data-theme', t);
    root.style.colorScheme = t;
    // Match system keeps one color per system theme (the browser picks);
    // a fixed choice gives both the same.
    var ms = document.querySelectorAll('meta[name=theme-color]');
    for (var i = 0; i < ms.length; i++) {
      var light = p === 'system' && ms[i].media ? /light/.test(ms[i].media) : t === 'light';
      ms[i].setAttribute('content', COLORS[light ? 'light' : 'dark']);
    }
    // iOS reads this when the installed app starts.
    var st = document.querySelector('meta[name=apple-mobile-web-app-status-bar-style]');
    if (st) st.setAttribute('content', t === 'dark' ? 'black-translucent' : 'default');
  }
  apply(pref());
  sys.addEventListener('change', function () { if (pref() === 'system') apply('system'); });
  addEventListener('storage', function (e) { if (e.key === 'gt.theme') apply(pref()); });
  window.gtTheme = {
    get: pref,
    set: function (p) { try { localStorage.setItem('gt.theme', JSON.stringify(p)); } catch (e) {} apply(p); },
  };
})();
