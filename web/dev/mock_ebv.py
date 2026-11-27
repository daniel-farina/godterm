"""Mock of the embedded browser frames (t: "ebv") for web/dev/mock.py.

Pages are pictures of web/dev/ebv_demo.html rendered by one headless Chrome
at a time (its own temp profile), so the pane can be looked at without the
Rust side. Needs a Chrome or Chromium; without one the frames are skipped.
"""
import os
import shutil
import subprocess
import tempfile
import threading
import time
import urllib.parse

HERE = os.path.dirname(os.path.abspath(__file__))
DEMO = os.path.join(HERE, "ebv_demo.html")
CHROMES = [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
]
RENDER = threading.Lock()
OPEN = set()
CLIENTS = set()
SESSIONS = {}


def chrome():
    for p in CHROMES:
        if os.path.isfile(p):
            return p
    for n in ("google-chrome", "chromium", "chromium-browser"):
        p = shutil.which(n)
        if p:
            return p
    return None


def render(url, w, h, click="", typed=""):
    exe = chrome()
    if not exe:
        return None
    q = urllib.parse.urlencode({"url": url, "click": click, "typed": typed})
    with RENDER:
        d = tempfile.mkdtemp(prefix="mock-ebv-")
        out = os.path.join(d, "p.png")
        try:
            subprocess.run([
                exe, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--no-first-run",
                "--user-data-dir=" + os.path.join(d, "profile"), "--screenshot=" + out,
                "--window-size=%d,%d" % (w, h), "--force-device-scale-factor=1",
                "file://" + DEMO + "?" + q,
            ], timeout=30, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            with open(out, "rb") as f:
                return f.read()
        except Exception:
            return None
        finally:
            shutil.rmtree(d, ignore_errors=True)


def title_of(url):
    host = urllib.parse.urlparse(url).netloc or url
    return "Local dev server" if host.startswith(("localhost", "127.")) else host


def normalize(url):
    u = url.strip()
    if not u:
        return None, "no address"
    low = u.lower()
    for bad in ("file:", "chrome:", "javascript:", "data:", "view-source:", "about:"):
        if low.startswith(bad) and low != "about:blank":
            return None, "%s pages are not opened in the embedded browser" % bad.rstrip(":")
    if "://" not in u:
        u = ("http://" if u.startswith(("localhost", "127.")) else "https://") + u
    return u, None


MAX_TABS = 6


def new_tab(s, url="about:blank"):
    t = {"id": s["next"], "hist": [url], "i": 0, "click": "", "typed": ""}
    s["next"] += 1
    s["tabs"].append(t)
    s["active"] = t["id"]
    return t


def session(uid):
    s = SESSIONS.get(uid)
    if s is None:
        s = {"tabs": [], "active": 0, "next": 1, "w": 900, "h": 600}
        new_tab(s, "http://localhost:3000/")
        SESSIONS[uid] = s
    return s


def cur(s):
    return next(t for t in s["tabs"] if t["id"] == s["active"])


def label(t):
    url = t["hist"][t["i"]]
    return "" if url == "about:blank" else title_of(url)


def tab_row(x):
    row = {"id": x["id"], "url": x["hist"][x["i"]], "title": label(x)}
    if x.get("asleep"):
        row["asleep"] = True
    return row


def agent_sleep(uid, page):
    s = session(uid)
    for t in s["tabs"]:
        if t["id"] == page:
            t["asleep"] = True
            t["hist"] = [t["hist"][t["i"]]]
            t["i"] = 0
            announce(uid)
            return True
    return False


def meta(uid, s, op="meta"):
    t = cur(s)
    url = t["hist"][t["i"]]
    return {"t": "ebv", "tab": uid, "op": op, "ok": True, "url": url, "title": label(t), "loading": False,
            "back": t["i"] > 0, "fwd": t["i"] < len(t["hist"]) - 1, "w": s["w"], "h": s["h"],
            "tabs": [tab_row(x) for x in s["tabs"]],
            "active": s["active"]}


def announce(uid):
    s = SESSIONS.get(uid)
    if s is None:
        return
    for cl in list(CLIENTS):
        cl.send(meta(uid, s))
        threading.Thread(target=push, args=(cl, uid, s), daemon=True).start()


def agent_tab(uid, url="", switch=0):
    s = session(uid)
    if switch:
        if not any(t["id"] == switch for t in s["tabs"]):
            return False
        s["active"] = switch
    elif len(s["tabs"]) < MAX_TABS:
        u, err = normalize(url) if url else ("about:blank", None)
        if err:
            return False
        new_tab(s, u)
    else:
        return False
    announce(uid)
    return True


def push(cl, uid, s):
    t = cur(s)
    png = render(t["hist"][t["i"]], s["w"], s["h"], t["click"], t["typed"])
    if png:
        cl.bin(7, uid, png)


def handle(cl, m, find):
    uid = int(str(m.get("tab", 0)).lstrip("t"))
    op = m.get("op")
    s = session(uid)
    rid = m.get("id")
    if op == "open":
        cur(s)["asleep"] = False
        if m.get("w") and m.get("h"):
            s["w"], s["h"] = int(m["w"]), int(m["h"])
        OPEN.add(uid)
        cl.send({**meta(uid, s, "open"), "id": rid})
        push(cl, uid, s)
    elif op == "go":
        u, err = normalize(m.get("url", ""))
        if err:
            cl.send({"t": "ebv", "id": rid, "tab": uid, "op": "go", "ok": False, "error": err})
            return
        t = cur(s)
        t["hist"] = t["hist"][: t["i"] + 1] + [u]
        t["i"] += 1
        t["click"] = ""
        t["typed"] = ""
        cl.send({**meta(uid, s, "go"), "id": rid})
        cl.send(meta(uid, s))
        push(cl, uid, s)
    elif op in ("back", "forward", "reload"):
        t = cur(s)
        if op == "back" and t["i"] > 0:
            t["i"] -= 1
        if op == "forward" and t["i"] < len(t["hist"]) - 1:
            t["i"] += 1
        cl.send(meta(uid, s))
        push(cl, uid, s)
    elif op == "resize":
        s["w"], s["h"] = int(m["w"]), int(m["h"])
        push(cl, uid, s)
    elif op == "mouse" and m.get("kind") == "up":
        cur(s)["click"] = "%.3f,%.3f" % (m.get("fx", 0), m.get("fy", 0))
        push(cl, uid, s)
    elif op in ("key", "text"):
        t = cur(s)
        if op == "text":
            t["typed"] += m.get("text", "")
        elif m.get("key") == "Backspace":
            t["typed"] = t["typed"][:-1]
        elif len(m.get("key", "")) == 1:
            t["typed"] += m["key"]
        push(cl, uid, s)
    elif op in ("tab_new", "tab_switch", "tab_close"):
        page = m.get("page")
        err = None
        if op == "tab_new":
            if len(s["tabs"]) >= MAX_TABS:
                err = "this browser already has %d tabs: close one first" % MAX_TABS
            else:
                u = None
                if m.get("url"):
                    u, err = normalize(m["url"])
                if not err:
                    new_tab(s, u or "about:blank")
        elif not any(t["id"] == page for t in s["tabs"]):
            err = "there is no tab with that id"
        elif op == "tab_switch":
            s["active"] = page
            cur(s)["asleep"] = False
        else:
            i = next(k for k, t in enumerate(s["tabs"]) if t["id"] == page)
            was = s["active"] == page
            s["tabs"].pop(i)
            if not s["tabs"]:
                new_tab(s)
            elif was:
                s["active"] = s["tabs"][max(0, i - 1)]["id"]
        if err:
            cl.send({"t": "ebv", "id": rid, "tab": uid, "op": op, "ok": False, "error": err})
            return
        cl.send({**meta(uid, s, op), "id": rid})
        push(cl, uid, s)
    elif op == "hit":
        if HIT["delay"]:
            time.sleep(HIT["delay"])
        cl.send(hit_reply(uid, s, rid))
    elif op == "edit":
        EDITS.append({k: m.get(k) for k in ("what", "text")})
        cl.send({"t": "ebv", "id": rid, "tab": uid, "op": "edit", "ok": m.get("what") in ("selectall", "delete", "paste", "focus")})
    elif op == "source":
        cl.send({"t": "ebv", "id": rid, "tab": uid, "op": "source", "ok": True, "html": SOURCE, "truncated": False})
    elif op == "kill":
        OPEN.discard(uid)
        SESSIONS.pop(uid, None)
        cl.send({"t": "ebv", "tab": uid, "op": "ended"})
    elif op == "keep":
        cl.send({"t": "ebv", "id": rid, "tab": uid, "op": "keep", "ok": uid in OPEN})


HITS = {
    "page": {},
    "link": {"link": {"href": "https://example.com/docs", "text": "Read the docs", "open": True}},
    "mail": {"link": {"href": "mailto:someone@example.com", "text": "Mail", "open": False,
                      "why": "mailto: links are shown here but not opened (only http and https open)"}},
    "image": {"image": {"src": "https://example.com/logo.png", "alt": "logo", "open": True}},
    "linkimage": {"link": {"href": "https://example.com/docs", "text": "Logo", "open": True},
                  "image": {"src": "https://example.com/logo.png", "alt": "logo", "open": True}},
    "selection": {"selection": "calm muted palettes for terminals"},
    "field": {"editable": True},
    "fieldsel": {"editable": True, "selection": "typed words"},
    "readonly": {"editable": True, "readonly": True},
}
HIT = {"kind": "page", "delay": 0.0}
EDITS = []
SOURCE = "<!DOCTYPE html>\n<html>\n<head><title>Local dev server</title></head>\n<body><h1>Hello</h1></body>\n</html>"


def set_hit(kind):
    delay = 0.0
    if kind.startswith("slow-"):
        kind, delay = kind[5:], 0.4
    HIT["kind"] = kind if kind in HITS else "page"
    HIT["delay"] = delay


def hit_reply(uid, s, rid):
    base = {"link": None, "image": None, "selection": "", "editable": False, "readonly": False,
            "password": False, "frame": False}
    t = cur(s)
    return {"t": "ebv", "id": rid, "tab": uid, "op": "hit", "ok": True, **base, **HITS[HIT["kind"]],
            "url": t["hist"][t["i"]], "title": label(t)}


PROFILES = [
    {"dir": "Default", "name": "Personal", "account": "person@example.com"},
    {"dir": "Profile 1", "name": "Work", "account": ""},
]
OPENED = []


def open_ext(cl, m):
    rid = m.get("id")
    base = {"t": "oext", "id": rid, "op": m.get("op")}
    if m.get("op") == "list":
        cl.send({**base, "ok": True, "chrome": True, "safari": True, "profiles": PROFILES})
        return
    u, err = normalize(m.get("url", ""))
    if err or not u.lower().startswith(("http://", "https://")):
        cl.send({**base, "ok": False, "error": "only http and https pages can be opened in another browser"})
        return
    if m.get("browser") == "chrome":
        names = {p["dir"]: p["name"] for p in PROFILES}
        who = names.get(m.get("profile"))
        if m.get("profile") and not who:
            cl.send({**base, "ok": False, "error": "that Chrome profile was not found"})
            return
        msg = "Opened in Chrome, profile " + who if who else "Opened in Chrome"
    else:
        msg = "Opened in Safari"
    OPENED.append({"browser": m.get("browser"), "profile": m.get("profile"), "url": u})
    cl.send({**base, "ok": True, "message": msg})
