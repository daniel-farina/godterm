use super::*;

const MAX_SELECTION: usize = 5000;
const MAX_LABEL: usize = 200;
const MAX_SOURCE: usize = 120_000;

const RESOLVE_JS: &str = r#"
const X = __X__, Y = __Y__;
const resolve = () => {
  let root = document, lx = X, ly = Y, win = window, cross = false;
  let el = root.elementFromPoint(lx, ly);
  for (let i = 0; i < 16 && el; i++) {
    const sr = el.shadowRoot;
    if (sr) {
      const inner = sr.elementFromPoint(lx, ly);
      if (inner && inner !== el) { el = inner; root = sr; continue; }
    }
    if (el.tagName === 'IFRAME' || el.tagName === 'FRAME') {
      let d = null;
      try { d = el.contentDocument; } catch (e) {}
      if (!d) { cross = true; break; }
      const r = el.getBoundingClientRect();
      lx -= r.left + el.clientLeft;
      ly -= r.top + el.clientTop;
      const inner = d.elementFromPoint(lx, ly);
      if (!inner) break;
      win = el.contentWindow;
      el = inner;
      root = d;
      continue;
    }
    break;
  }
  return { el, root, win, lx, ly, cross };
};
const isEditable = (n) => {
  if (!n || !n.tagName) return false;
  const t = n.tagName;
  if (t === 'TEXTAREA') return true;
  if (t === 'INPUT') return !['button', 'submit', 'reset', 'checkbox', 'radio', 'file', 'image', 'color', 'range', 'hidden'].includes((n.type || '').toLowerCase());
  return !!n.isContentEditable;
};
"#;

const HIT_JS: &str = r#"(() => {
__RESOLVE__
const clip = (s, n) => { s = String(s == null ? '' : s).replace(/\s+/g, ' ').trim(); return s.length > n ? s.slice(0, n) : s; };
const { el, win, lx, ly, cross } = resolve();
if (!el) return { empty: true };
const up = (n) => n.parentElement || (n.getRootNode && n.getRootNode().host) || null;
let a = null, img = null;
let n = el;
for (let i = 0; n && i < 80; i++, n = up(n)) {
  const t = n.tagName ? n.tagName.toLowerCase() : '';
  if (!img && t === 'img') img = n;
  if (!a && (t === 'a' || t === 'area') && (n.hasAttribute('href') || n.hasAttribute('xlink:href'))) a = n;
}
const absolute = (n, attr) => {
  try {
    if (typeof n[attr] === 'string' && n[attr]) return n[attr];
    return new URL(n.getAttribute(attr) || n.getAttribute('xlink:href') || '', n.baseURI).href;
  } catch (e) { return ''; }
};
const out = { empty: false, link: null, image: null, selection: '', editable: false, readonly: false, password: false, cross };
if (a) {
  const href = absolute(a, 'href');
  if (href) out.link = { href, text: clip(a.innerText || a.textContent || a.getAttribute('aria-label') || a.title || (img && img.alt) || '', 200) };
}
if (img) {
  const src = img.currentSrc || img.src || '';
  if (src) out.image = { src, alt: clip(img.alt || img.title || '', 200) };
}
const t = el.tagName;
out.editable = isEditable(el);
if (out.editable) {
  out.readonly = !!(el.readOnly || el.disabled || el.getAttribute('aria-readonly') === 'true');
  out.password = (el.type || '').toLowerCase() === 'password';
}
if (out.editable && (t === 'INPUT' || t === 'TEXTAREA')) {
  if (!out.password && typeof el.selectionStart === 'number' && el.selectionEnd > el.selectionStart) out.selection = String(el.value).substring(el.selectionStart, el.selectionEnd).slice(0, 5000);
} else {
  try {
    const sel = win.getSelection();
    const text = sel ? sel.toString() : '';
    if (text && sel.rangeCount) {
      const rects = sel.getRangeAt(0).getClientRects();
      for (const r of rects) {
        if (lx >= r.left - 2 && lx <= r.right + 2 && ly >= r.top - 2 && ly <= r.bottom + 2) { out.selection = text.slice(0, 5000); break; }
      }
    }
  } catch (e) {}
}
return out;
})()"#;

const EDIT_JS: &str = r#"(() => {
__RESOLVE__
const { el } = resolve();
if (!el || !isEditable(el)) return false;
const doc = el.ownerDocument || document;
el.focus();
const what = __WHAT__;
if (what === 'selectall') {
  if (typeof el.select === 'function') el.select(); else doc.execCommand('selectAll');
} else if (what === 'delete') {
  doc.execCommand('delete');
}
return true;
})()"#;

fn page_js(template: &str, x: f64, y: f64) -> String {
    template
        .replace("__RESOLVE__", RESOLVE_JS)
        .replace("__X__", &format!("{}", x.round() as i64))
        .replace("__Y__", &format!("{}", y.round() as i64))
}

pub fn open_policy(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    match split_scheme(s) {
        Some((sch, _)) if sch.eq_ignore_ascii_case("http") || sch.eq_ignore_ascii_case("https") => {
            check_url(s)
        }
        Some((sch, _)) => {
            let shown: String = sch.chars().take(20).collect::<String>().to_lowercase();
            Err(format!(
                "{shown}: links are shown here but not opened (only http and https open)"
            ))
        }
        None => Err("that link is not an http or https address".into()),
    }
}

fn clean(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(max)
        .collect()
}

fn target(raw: &Value, key: &str, label_key: &str) -> Value {
    let Some(u) = raw[key].as_str().filter(|u| !u.trim().is_empty()) else {
        return Value::Null;
    };
    let label = clean(raw[label_key].as_str().unwrap_or(""), MAX_LABEL);
    if u.len() > MAX_URL * 4 || u.starts_with("data:") || u.starts_with("blob:") {
        let why = if u.len() > MAX_URL * 4 {
            "that address is too long".to_string()
        } else {
            let sch = u.split(':').next().unwrap_or("data");
            format!("{sch}: addresses are shown here but not opened (only http and https open)")
        };
        return json!({key: "", label_key: label, "open": false, "why": why, "kind": u.split(':').next().unwrap_or("")});
    }
    match open_policy(u) {
        Ok(url) => json!({key: url, label_key: label, "open": true}),
        Err(why) => json!({key: u.trim(), label_key: label, "open": false, "why": why}),
    }
}

pub fn parse_hit(raw: &Value) -> Value {
    if raw.is_null() || raw["empty"] == json!(true) {
        return json!({"link": null, "image": null, "selection": "", "editable": false, "readonly": false, "password": false, "frame": false});
    }
    let editable = raw["editable"].as_bool().unwrap_or(false);
    let password = editable && raw["password"].as_bool().unwrap_or(false);
    let selection = if password {
        String::new()
    } else {
        clean(raw["selection"].as_str().unwrap_or(""), MAX_SELECTION)
    };
    json!({
        "link": target(&raw["link"], "href", "text"),
        "image": target(&raw["image"], "src", "alt"),
        "selection": selection,
        "editable": editable,
        "readonly": editable && raw["readonly"].as_bool().unwrap_or(false),
        "password": password,
        "frame": raw["cross"].as_bool().unwrap_or(false),
    })
}

pub fn search_url(text: &str) -> Option<String> {
    let t: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(300).collect();
    if t.is_empty() {
        return None;
    }
    let mut q = String::new();
    for b in t.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                q.push(b as char)
            }
            b' ' => q.push('+'),
            _ => q.push_str(&format!("%{b:02X}")),
        }
    }
    Some(format!("https://duckduckgo.com/?q={q}"))
}

impl Session {
    pub fn hit_test(&self, x: f64, y: f64) -> Result<Value, String> {
        let raw = self.eval(&page_js(HIT_JS, x, y))?;
        let mut v = parse_hit(&raw);
        let g = lk(&self.st);
        let (url, title) = g.tabs.cur().map_or((String::new(), String::new()), |p| {
            (p.url.clone(), p.title.clone())
        });
        v["url"] = json!(url);
        v["title"] = json!(title);
        Ok(v)
    }

    pub fn edit_at(&self, x: f64, y: f64, what: &str, text: Option<&str>) -> Result<(), String> {
        let w = match what {
            "selectall" | "delete" | "focus" | "paste" => what,
            _ => return Err("unknown edit".into()),
        };
        let js = page_js(EDIT_JS, x, y).replace("__WHAT__", &js_str(w));
        if self.eval(&js)? != json!(true) {
            return Err("that is not an editable field any more".into());
        }
        if w == "paste" {
            if let Some(t) = text.filter(|t| !t.is_empty() && t.chars().count() <= 20000) {
                self.insert_text(t)?;
            }
        }
        Ok(())
    }

    pub fn page_source(&self) -> Result<Value, String> {
        let v = self.eval(
            "(()=>{const d=document.doctype?'<!DOCTYPE '+document.doctype.name+'>\\n':'';return d+document.documentElement.outerHTML})()",
        )?;
        let t = v.as_str().ok_or("this page has no source")?;
        let (t, cut) = trunc(t, MAX_SOURCE);
        Ok(json!({"html": t, "truncated": cut}))
    }
}
