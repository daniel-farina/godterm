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

    var ms = document.querySelectorAll('meta[name=theme-color]');
    for (var i = 0; i < ms.length; i++) {
      var light = p === 'system' && ms[i].media ? /light/.test(ms[i].media) : t === 'light';
      ms[i].setAttribute('content', COLORS[light ? 'light' : 'dark']);
    }

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
