#!/usr/bin/env python3
"""Mock GodTerm web server for developing the browser app without the real
backend. Python stdlib only, with a hand-rolled WebSocket. It serves web/
and speaks the protocol in docs/WEB_APP.md with fake tabs running a fake
shell, a fake assistant, approvals, jobs, loops and ~200 fake sessions.

    python3 web/dev/mock.py [--port 7890]

Dev endpoints: /dev/bump (new web build -> "Update available"),
/dev/update (GodTerm app update news), /dev/approval (a new approval).
"""
import argparse
import base64
import hashlib
import http.server
import io
import json
import math
import os
import random
import socket
import struct
import threading
import time
import urllib.parse
import wave

import mock_git

WEB = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
LOCK = threading.RLock()
CLIENTS = set()
BUILD = [hashlib.sha1(str(time.time()).encode()).hexdigest()[:12]]
RNG = random.Random(7)

ACCOUNTS = [
    {"account": 1, "label": "Alpha", "harness": "claude", "logged_in": True, "available": True},
    {"account": 2, "label": "Bravo", "harness": "claude", "logged_in": True, "available": True},
    {"account": 3, "label": "Grok", "harness": "grok", "logged_in": True, "available": True},
    {"account": 4, "label": "Delta", "harness": "claude", "logged_in": False, "available": False},
]
USAGE = {1: (72, 55), 2: (18, 40), 3: (90, 88), 4: (None, None)}
for a in ACCOUNTS:
    five, week = USAGE[a["account"]]
    a["five_hour_left_pct"] = five
    a["week_left_pct"] = week
    a["effective_left_pct"] = None if five is None else min(five, week)
    a["buckets"] = None if five is None else [
        {"bucket": "5 hour", "left_pct": five, "reset_passed": False, "resets_at": "Sun Oct 25 16:40"},
        {"bucket": "weekly", "left_pct": week, "reset_passed": False, "resets_at": "Fri Oct 30 09:00"},
    ]
    if a["account"] == 4:
        a["usage_error"] = "not_logged_in"

SGR = {"g": "\x1b[38;5;108m", "y": "\x1b[38;5;179m", "b": "\x1b[38;5;110m", "d": "\x1b[38;5;245m",
       "r": "\x1b[38;5;174m", "m": "\x1b[38;5;139m", "B": "\x1b[1m", "0": "\x1b[0m"}


def c(s):
    for k, v in SGR.items():
        s = s.replace("{" + k + "}", v)
    return s


class Tab:
    def __init__(self, uid, account, name, folder, cols=100, rows=30, state="ready", pinned=False):
        self.uid, self.account, self.name, self.folder = uid, account, name, folder
        self.cols, self.rows, self.state, self.pinned = cols, rows, state, pinned
        self.own = (cols, rows)
        self.hist = bytearray()
        self.line = ""
        self.work_left = 0
        self.model = "opus" if acct(account)["harness"] == "claude" else "grok-4.7"
        self.group = None
        self.color = None
        self.out(c("{y}{B}╭" + "─" * 40 + "╮{0}\r\n"))
        self.out(c("{y}│{0} {B}%-38s{0} {y}│{0}\r\n") % ("GodTerm mock: " + name)[:38])
        self.out(c("{y}│{0} {d}%-38s{0} {y}│{0}\r\n") % folder[:38])
        self.out(c("{y}╰" + "─" * 40 + "╯{0}\r\n\r\n"))
        self.out(c("{d}Type {0}help{d} for the fake commands.{0}\r\n"))
        self.prompt()

    def json(self, pos, n):
        a = acct(self.account)
        v = {"id": "t%d" % self.uid, "uid": self.uid, "cols": self.cols, "rows": self.rows, "running": True, "account": self.account, "account_label": a["label"], "position": "tab %d of %d" % (pos, n),
             "name": self.name, "folder": self.folder, "state": self.state, "model": self.model}
        if self.pinned:
            v["pinned"] = True
        if self.state == "working":
            v["busy"] = True
        if self.state == "approval":
            v["waiting_for"] = "Bash(rm -rf build/)"
        if self.group:
            v["group"] = self.group
        if self.color:
            v["color"] = self.color
        loops = sum(1 for l in LOOPS if l["tab"] == "t%d" % self.uid)
        if loops:
            v["loops"] = loops
        if a["harness"] == "claude":
            v["effort"] = "high" if self.uid % 2 else "medium"
        # The status bar's git and browser, varied across the tabs.
        k = self.uid % 3
        if k == 0:
            v["git"] = "main, github acme/%s, 2 unpushed, 3 modified" % self.name
            v["git_facts"] = {"branch": "main", "dirty": 3, "unpushed": 2, "behind": 0, "error": False}
            v["browser"] = {"profile": "Work", "profile_dir": "Profile 1", "state": "ready"}
        elif k == 1:
            v["git"] = "feature/x, no remote, clean"
            v["git_facts"] = {"branch": "feature/x", "dirty": 0, "unpushed": 0, "behind": 1, "error": False}
            v["git_warn"] = "Not backed up: no GitHub repo"
        else:
            v["git"] = "not a git repo"
            v["git_warn"] = "Not backed up: not in git"
        return v

    def out(self, s):
        b = s.encode() if isinstance(s, str) else s
        self.hist += b
        if len(self.hist) > 200_000:
            del self.hist[:100_000]
        broadcast_bin(1, self.uid, b)

    def prompt(self):
        self.out(c("{g}%s{0} {d}${0} " % self.folder))

    def snapshot(self):
        return "\x1b[0m\x1b[H\x1b[2J" + self.hist.decode("utf-8", "replace")

    def key(self, data):
        s = data.decode("utf-8", "replace")
        i = 0
        while i < len(s):
            ch = s[i]
            if ch == "\x1b":
                j = i + 1
                if j < len(s) and s[j] in "[O":
                    j += 1
                    while j < len(s) and not s[j].isalpha() and s[j] != "~":
                        j += 1
                    seq = s[i:j + 1]
                    i = j + 1
                    if seq in ("\x1b[A", "\x1bOA"):
                        self.out(c("{d}[up]{0}"))
                    elif seq in ("\x1b[B", "\x1bOB"):
                        self.out(c("{d}[down]{0}"))
                    elif seq in ("\x1b[5~", "\x1b[6~"):
                        self.out(c("{d}[%s]{0}" % ("pgup" if "5" in seq else "pgdn")))
                    continue
                self.out(c("{d}[esc]{0}"))
                if self.state == "working":
                    self.work_left = 0
                    self.set_state("ready")
                    self.out(c("\r\n{r}Interrupted{0}\r\n"))
                    self.prompt()
                i += 1
                continue
            i += 1
            if ch in "\r\n":
                self.out("\r\n")
                self.run(self.line.strip())
                self.line = ""
            elif ch in "\x7f\b":
                if self.line:
                    self.line = self.line[:-1]
                    self.out("\b \b")
            elif ch == "\x03":
                self.out("^C\r\n")
                self.line = ""
                self.work_left = 0
                self.set_state("ready")
                self.prompt()
            elif ch == "\t":
                self.out("    ")
                self.line += "    "
            elif ch >= " ":
                self.line += ch
                self.out(ch)
            else:
                self.out(c("{d}^%s{0}" % chr(ord(ch) + 64)))

    def run(self, cmd):
        w = cmd.split(" ", 1)[0]
        if not cmd:
            pass
        elif w == "help":
            self.out("commands: help ls colors lorem work approve size clear wide\r\n")
        elif w == "ls":
            for n in ["src/", "docs/", "web/", "Cargo.toml", "README.md", "notes.txt"]:
                col = "{b}" if n.endswith("/") else "{0}"
                self.out(c(col + n + "{0}  "))
            self.out("\r\n")
        elif w == "colors":
            for i in range(16):
                self.out("\x1b[48;5;%dm  %2d  \x1b[0m" % (i, i) + ("\r\n" if i % 8 == 7 else ""))
            for i in range(16, 232, 6):
                self.out("\x1b[48;5;%dm \x1b[0m" % i)
            self.out("\r\n")
        elif w == "lorem":
            for n in range(60):
                self.out(c("{d}%3d{0} Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod.\r\n" % n))
        elif w == "wide":
            self.out(("=" * (self.cols - 1)) + "|\r\n")
            self.out("".join(str(i % 10) for i in range(self.cols)) + "\r\n")
        elif w == "size":
            self.out("%dx%d\r\n" % (self.cols, self.rows))
        elif w == "clear":
            self.out("\x1b[H\x1b[2J")
            self.hist = bytearray(b"\x1b[H\x1b[2J")
        elif w == "work":
            self.work_left = 8
            self.set_state("working")
            return
        elif w == "approve":
            ask_approval(self)
        else:
            self.out(c("{d}you typed:{0} %s\r\n" % cmd))
        self.prompt()

    def set_state(self, st):
        self.state = st
        dirty()

    def tick(self):
        if self.state == "working" and self.work_left > 0:
            self.work_left -= 1
            spin = "⠋⠙⠹⠸⠼⠴⠦⠧"[self.work_left % 8]
            self.out(c("{y}%s{0} Reading files {d}(%d left){0}\r\n" % (spin, self.work_left)))
            if self.work_left == 0:
                self.out(c("{g}✔ Done.{0}\r\n"))
                self.set_state("ready")
                self.prompt()


def acct(n):
    return next(a for a in ACCOUNTS if a["account"] == n)


TABS = []
NEXT_UID = [3760]


def new_tab(account, name, folder, **kw):
    NEXT_UID[0] += 1
    t = Tab(NEXT_UID[0], account, name, folder, **kw)
    TABS.append(t)
    return t


def seed():
    new_tab(1, "web app", "~/godterm", cols=120, rows=36, pinned=True)
    new_tab(1, "trading room", "~/trading", cols=100, rows=30, state="working").work_left = 30
    new_tab(1, "docs pass", "~/godterm/docs", cols=80, rows=24)
    new_tab(2, "api refactor", "~/varium/api", cols=132, rows=40, state="approval")
    new_tab(2, "landing page", "~/sites/landing", cols=90, rows=28)
    new_tab(3, "grok scratch", "~/scratch", cols=100, rows=30)
    new_tab(3, "data pipeline", "~/etl", cols=110, rows=32, state="background")
    TABS[1].tick()

JOBS = [
    {"id": "j1", "name": "morning brief", "schedule": "0 8 * * 1-5", "status": "active", "tab": "docs pass", "next": "in 14 h", "last": "ok"},
    {"id": "j2", "name": "deps check", "schedule": "every 6 hours", "status": "paused", "tab": "web app", "next": None, "last": "failed"},
]
LOOPS = [{"job": "L7", "tab": "t3762", "cadence": "every 10m", "prompt": "Check the order book and report anything unusual.", "next": "in 4m"}]
CLOSED = [{"index": 0, "tab": "old experiment", "account": 1, "cwd": "~/exp", "loops": 0}]
APPROVALS = {}
MODES = {"memory_saver": False, "privacy": False, "voice": "push"}
CHAT = []
STATE_DIRTY = [False]


def modes_line():
    return "Modes: memory saver %s, privacy %s, voice %s" % ("on" if MODES["memory_saver"] else "off", "on" if MODES["privacy"] else "off", MODES["voice"])


def state():
    n = len(TABS)
    for a in ACCOUNTS:
        a["tabs"] = sum(1 for t in TABS if t.account == a["account"])
    return {
        "accounts": ACCOUNTS, "tabs": [t.json(i + 1, n) for i, t in enumerate(TABS)],
        "jobs": {"count": len(JOBS), "active": sum(j["status"] == "active" for j in JOBS), "next": "morning brief in 14 h", "jobs": JOBS},
        "loops": LOOPS, "closed": CLOSED, "modes": modes_line(), "pending_confirmations": [],
        "voice": {"mode": MODES["voice"], "sample_rate": 24000},
        "approvals": [public(a) for a in APPROVALS.values()],
        "assistant": {"on": True, "busy": False, "provider": "claude"},
        "archived": archived_state(),
    }


def dirty():
    STATE_DIRTY[0] = True


# ---- WebSocket -----------------------------------------------------------

class Client:
    def __init__(self, sock):
        self.sock, self.wlock, self.subs = sock, threading.Lock(), set()
        self.audio_bytes = 0
        self.audio_nonzero = 0
        self.alive = True

    def frame(self, op, payload):
        n = len(payload)
        if n < 126:
            head = struct.pack("!BB", 0x80 | op, n)
        elif n < 65536:
            head = struct.pack("!BBH", 0x80 | op, 126, n)
        else:
            head = struct.pack("!BBQ", 0x80 | op, 127, n)
        try:
            with self.wlock:
                self.sock.sendall(head + payload)
        except OSError:
            self.alive = False

    def send(self, obj):
        self.frame(1, json.dumps(obj).encode())

    def bin(self, kind, uid, payload):
        self.frame(2, struct.pack("!BI", kind, uid) + payload)


def recv_exact(sock, n):
    buf = b""
    while len(buf) < n:
        chunk = sock.recv(n - len(buf))
        if not chunk:
            raise ConnectionError
        buf += chunk
    return buf


def read_message(sock):
    data, first_op = b"", None
    while True:
        b1, b2 = recv_exact(sock, 2)
        fin, op = b1 & 0x80, b1 & 0x0F
        n = b2 & 0x7F
        if n == 126:
            n = struct.unpack("!H", recv_exact(sock, 2))[0]
        elif n == 127:
            n = struct.unpack("!Q", recv_exact(sock, 8))[0]
        mask = recv_exact(sock, 4) if b2 & 0x80 else b"\0\0\0\0"
        p = bytearray(recv_exact(sock, n))
        for i in range(n):
            p[i] ^= mask[i % 4]
        if op >= 8:
            return op, bytes(p)
        if first_op is None:
            first_op = op
        data += p
        if fin:
            return first_op, data


def broadcast_bin(kind, uid, b):
    for cl in list(CLIENTS):
        if uid in cl.subs:
            cl.bin(kind, uid, b)


def broadcast(obj):
    for cl in list(CLIENTS):
        cl.send(obj)


def find(tab):
    if isinstance(tab, list):
        tab = tab[0] if tab else None
    if tab is None:
        return None
    u = int(str(tab).lstrip("t")) if str(tab).lstrip("t").isdigit() else None
    return next((t for t in TABS if t.uid == u or t.name == tab), None)


def ask_approval(tab):
    aid = "tab:%d" % tab.uid
    q = "Bash(rm -rf build/ && cargo build --release)"
    APPROVALS[aid] = {"id": aid, "kind": "tab", "tab": tab.uid, "name": tab.name, "question": q, "choices": ["yes", "always", "no"], "tab_ref": tab}
    tab.set_state("approval")
    broadcast({"t": "approval", **public(APPROVALS[aid])})


def public(a):
    return {k: v for k, v in a.items() if k != "tab_ref"}


def stream_reply(cl, text):
    rid = "m%d" % int(time.time() * 1000)
    words = ("Here is what I see across your tabs:\n\n"
             "- **trading room** is working (reading files)\n"
             "- **api refactor** waits for an approval: `rm -rf build/`\n"
             "- Bravo has **18%%** of its 5 hour window left\n\n"
             "You said: _%s_\n\n```sh\ngodterm web-url\n```\nAnything else?" % text).split(" ")
    acc = ""
    for i, w in enumerate(words):
        acc += ("" if i == 0 else " ") + w
        broadcast({"t": "assistant", "id": "live", "role": "assistant", "text": acc, "done": False})
        time.sleep(0.03)
    # Like GodTerm: the live reply ends empty and the final text is a log entry.
    broadcast({"t": "assistant", "id": "live", "role": "assistant", "text": "", "done": True})
    broadcast({"t": "assistant", "id": rid, "role": "assistant", "text": acc, "done": True})
    CHAT.append({"id": rid, "role": "assistant", "text": acc})


def beep():
    buf = io.BytesIO()
    with wave.open(buf, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(24000)
        w.writeframes(b"".join(struct.pack("<h", int(6000 * math.sin(2 * math.pi * 520 * i / 24000) * min(1, (7200 - i) / 800))) for i in range(7200)))
    return buf.getvalue()


# ---- sessions -----------------------------------------------------------

def make_sessions():
    r = random.Random(42)
    words = ["refactor", "api", "landing", "trading", "bot", "docs", "voice", "tests", "deploy", "varium", "pipeline", "auth", "web", "ui", "grid", "icons", "cache", "billing"]
    projects = ["~/godterm", "~/varium/api", "~/sites/landing", "~/trading", "~/etl", "~/scratch", "~", "~/mobile/app", "~/fotmesh"]
    out = []
    now = time.time()
    for i in range(200):
        h = "grok" if r.random() < 0.2 else "claude"
        src = 3 if h == "grok" and r.random() < 0.7 else ("main" if r.random() < 0.15 else r.choice([1, 2, 4]))
        age = r.random() ** 2 * 60 * 86400
        title = " ".join(r.sample(words, r.randint(2, 4))).title()
        msgs = r.randint(2, 900)
        out.append({"id": "%08x-%04x" % (r.getrandbits(32), i), "harness": h, "source": src, "project": r.choice(projects), "title": title,
                    "first_prompt": "Please " + " ".join(r.sample(words, 5)), "modified": now - age, "created": now - age - r.random() * 5 * 86400,
                    "messages": msgs, "tokens": {"new_input": msgs * r.randint(200, 2000), "output": msgs * r.randint(50, 400), "largest_context": r.randint(10, 190) * 1000},
                    "subagents": r.choice([None, None, None, 1, 3]), "live": age < 600})
    # Fixtures for the tab filter's "Other sessions" (one per harness).
    for i, (h, src, proj, title) in enumerate([
            ("claude", 2, "~/work/acme-api", "Acme API login refactor"), ("grok", 3, "~/work/acme-web", "Acme web landing page"),
            ("codex", "codex 1", "~/work/acme-api", "Fix the acme billing export"), ("cursor", "~/.cursor", "~/work/acme-web", "Add a dark theme to acme web"),
            ("claude", "main", "~/work/acme-infra", "Acme infra terraform plan")]):
        age = (i + 1) * 5 * 3600
        out.append({"id": "acme%04d-0000-4000-8000-%012d" % (i, i), "harness": h, "source": src, "project": proj, "title": title,
                    "first_prompt": "Please look at " + title.lower(), "modified": now - age, "created": now - age - 3600, "messages": 20 + i,
                    "tokens": {"new_input": 9000, "output": 3000, "largest_context": 40000}, "subagents": None, "live": False})
    return out


SESSIONS = make_sessions()

# Archived tabs, as the server's archived_json.
ARCHIVED = [{"id": "arch%02d" % i, "name": n, "account": acc, "account_label": "Account %d" % acc, "provider": "claude", "folder": f,
             "session": None, "model": None, "pinned": False, "group": None, "loops": 0, "idle_hours_when_archived": 30,
             "last_active": "%d days ago" % (i + 2), "archived": "%d days ago" % (i + 1), "by": "user"}
            for i, (n, acc, f) in enumerate([("acme dashboard", 1, "~/work/acme-dash"), ("acme mobile", 2, "~/work/acme-mobile"),
                                             ("old trading bot", 1, "~/trading"), ("acme docs", 4, "~/work/acme-docs"),
                                             ("acme search", 1, "~/work/acme-search"), ("acme ops", 2, "~/work/acme-ops"),
                                             ("acme mail", 1, "~/work/acme-mail")])]


def archived_state():
    return {"count": len(ARCHIVED), "recent": ARCHIVED[:8]}


def rel(t):
    s = int(time.time() - t)
    return "%d s ago" % s if s < 60 else "%d min ago" % (s // 60) if s < 3600 else "%d h ago" % (s // 3600) if s < 86400 else "%d days ago" % (s // 86400)


def sessions_tool(a):
    rows = SESSIONS
    text = (a.get("text") or "").lower().split()
    if text:
        rows = [s for s in rows if all(w in (s["title"] + " " + s["first_prompt"] + " " + s["project"]).lower() for w in text)]
    if a.get("harness") not in (None, "", "all"):
        rows = [s for s in rows if s["harness"] == a["harness"]]
    if a.get("source") not in (None, "", "all"):
        rows = [s for s in rows if str(s["source"]) == str(a["source"])]
    if a.get("project"):
        rows = [s for s in rows if a["project"].lower() in s["project"].lower()]
    if a.get("state") == "active":
        rows = [s for s in rows if s["live"]]
    elif a.get("state") == "recent":
        rows = [s for s in rows if time.time() - s["modified"] < 3 * 86400]
    since = {"today": 1, "yesterday": 2, "3 days": 3, "last week": 7, "30 days": 30}.get(a.get("since") or "")
    if since:
        rows = [s for s in rows if time.time() - s["modified"] < since * 86400]
    key = a.get("sort") or "modified"
    keys = {"modified": lambda s: -s["modified"], "created": lambda s: -s["created"], "messages": lambda s: -s["messages"],
            "tokens": lambda s: -(s["tokens"]["new_input"] + s["tokens"]["output"]), "context": lambda s: -s["tokens"]["largest_context"],
            "project": lambda s: s["project"], "source": lambda s: str(s["source"]), "title": lambda s: s["title"], "oldest": lambda s: s["modified"]}
    rows = sorted(rows, key=keys.get(key, keys["modified"]), reverse=bool(a.get("reverse")))
    lim = int(a.get("limit") or 30)
    shown = [{**{k: v for k, v in s.items() if k not in ("modified", "created")}, "last_activity": rel(s["modified"])} for s in rows[:lim]]
    return {"sessions": shown, "matched": len(rows), "shown": len(shown), "say": "%d sessions." % len(rows)}


# ---- tools ---------------------------------------------------------------

CONFIRM = {}


def needs(name, args, question):
    tok = "c%d" % RNG.randrange(10**6)
    CONFIRM[tok] = (name, args)
    return {"ok": False, "needs_confirmation": True, "token": tok, "question": question}


def tool(name, a):
    if a.get("confirm_token"):
        name2, a = CONFIRM.pop(a["confirm_token"], (None, None))
        if name2 is None:
            return {"ok": False, "error": "that confirmation expired"}
        a = {**a, "_confirmed": True}
    t = find(a.get("tab"))
    if name == "sessions":
        return {"ok": True, "result": sessions_tool(a)}
    if name == "list_archived":
        words = (a.get("text") or a.get("name") or "").lower().split()
        rows = [r for r in ARCHIVED if all(w in (r["name"] + " " + r["folder"] + " " + r["account_label"]).lower() for w in words)]
        lim = int(a.get("limit") or 30)
        return {"ok": True, "result": {"archived": rows[:lim], "total": len(rows), "say": "%d archived." % len(rows)}}
    if name == "restore_archived":
        r = next((r for r in ARCHIVED if r["id"] == a.get("id")), None)
        if not r:
            return {"ok": False, "error": "%s is not in the archive" % a.get("id")}
        ARCHIVED.remove(r)
        nt = new_tab(r["account"], r["name"], r["folder"], cols=100, rows=30)
        dirty()
        return {"ok": True, "result": {"restored": 1, "tabs": [{"id": r["id"], "name": r["name"], "tab": "t%d" % nt.uid}], "say": "Restored 1 conversation."}}
    if name in ("open_tab", "open_session", "take_over_session", "reopen_tab", "reopen_closed"):
        acc = int(a.get("account") or 1)
        nm = a.get("name") or (a.get("dir") or "").rsplit("/", 1)[-1] or ("session " + str(a.get("id", ""))[:6]) or "new tab"
        nt = new_tab(acc, nm, a.get("dir") or "~/" + nm.replace(" ", "-"), cols=100, rows=30)
        dirty()
        return {"ok": True, "result": {"tab": "t%d" % nt.uid, "say": "Opened %s on %s." % (nm, acct(acc)["label"])}}
    if name == "close_tabs" and t:
        if (t.pinned or t.state == "working") and not a.get("_confirmed"):
            return needs(name, a, "Close %s? It is %s." % (t.name, "pinned" if t.pinned else "working"))
        TABS.remove(t)
        CLOSED.insert(0, {"index": 0, "tab": t.name, "account": t.account, "cwd": t.folder, "loops": 0})
        for i, x in enumerate(CLOSED):
            x["index"] = i
        dirty()
        return {"ok": True, "result": {"say": "Closed %s." % t.name}}
    if name == "archive_tabs" and t:
        if not a.get("_confirmed"):
            return needs(name, a, "Archive %s? Its session stops and it moves to Archived." % t.name)
        TABS.remove(t)
        ARCHIVED.insert(0, {"id": "arch%d" % t.uid, "name": t.name, "account": t.account, "account_label": acct(t.account)["label"], "provider": "claude",
                            "folder": getattr(t, "folder", ""), "session": None, "last_active": "just now", "archived": "just now", "by": "user"})
        dirty()
        return {"ok": True, "result": {"say": "Archived 1 tab.", "archived": 1}}
    if name == "rename_tab" and t:
        t.name = a.get("name") or t.name
        dirty()
        return {"ok": True, "result": {"say": "Renamed to %s." % t.name}}
    if name == "move_tab" and t:
        to = int(a.get("to") or 1)
        if a.get("copy"):
            nt = new_tab(to, t.name, t.folder, cols=t.cols, rows=t.rows)
            dirty()
            return {"ok": True, "result": {"say": "Duplicated to %s." % acct(to)["label"], "tabs": ["t%d" % nt.uid]}}
        t.account = to
        dirty()
        return {"ok": True, "result": {"say": "Moved %s to %s." % (t.name, acct(to)["label"])}}
    if name == "copy_session":
        return {"ok": True, "result": {"say": "Copied the session to %s." % acct(int(a.get("account") or 1))["label"]}}
    if name in ("pin_tab", "unpin_tab") and t:
        t.pinned = name == "pin_tab"
        dirty()
        return {"ok": True, "result": {"say": ("Pinned " if t.pinned else "Unpinned ") + t.name}}
    if name == "set_tab_color" and t:
        t.color = None if a.get("color") == "default" else a.get("color")
        dirty()
        return {"ok": True, "result": {"say": "Color set."}}
    if name == "group_tabs" and t:
        t.group = a.get("group")
        dirty()
        return {"ok": True, "result": {"say": "Grouped as %s." % t.group}}
    if name == "ungroup":
        for x in TABS:
            if x.group == a.get("group"):
                x.group = None
        dirty()
        return {"ok": True, "result": {"say": "Ungrouped."}}
    if name == "set_tab_model" and t:
        t.model = a.get("model")
        dirty()
        return {"ok": True, "result": {"say": "%s now uses %s." % (t.name, t.model), "status": "applied"}}
    if name == "press_key" and t:
        if t.state == "working" and a.get("key") in ("escape", "ctrl_c") and not a.get("_confirmed"):
            return needs(name, a, "Interrupt %s? It is working." % t.name)
        t.key(b"\x1b" if a.get("key") == "escape" else b"\x03" if a.get("key") == "ctrl_c" else b"\r")
        return {"ok": True, "result": {"say": "Pressed %s in %s." % (a.get("key"), t.name)}}
    if name in ("talk_to_tab", "open_path", "show_tab_loops"):
        return {"ok": True, "result": {"say": "Done (mock %s)." % name}}
    if name == "browser_status":
        return {"ok": True, "result": {"say": "BrowserSkill 0.9.2 installed; daemon running; 1 browser connected (Work Chrome)."}}
    if name == "set_mode":
        for k in ("memory_saver", "privacy", "voice"):
            if k in a:
                MODES[k] = a[k]
        dirty()
        return {"ok": True, "result": {"say": modes_line()}}
    if name == "run_job_now":
        return {"ok": True, "result": {"say": "Started %s." % a.get("job")}}
    if name == "update_job":
        for j in JOBS:
            if j["id"] == a.get("job"):
                j["status"] = "paused" if a.get("action") == "pause" else "active"
        dirty()
        return {"ok": True, "result": {"say": "Job %s." % ("paused" if a.get("action") == "pause" else "resumed")}}
    if name == "delete_job":
        if not a.get("_confirmed"):
            return needs(name, a, "Delete the job %s?" % a.get("job"))
        JOBS[:] = [j for j in JOBS if j["id"] != a.get("job")]
        dirty()
        return {"ok": True, "result": {"say": "Deleted."}}
    if name == "create_job":
        JOBS.append({"id": "j%d" % (len(JOBS) + 9), "name": a.get("name"), "schedule": a.get("cron") or a.get("every") or a.get("at"),
                     "status": "active", "tab": t.name if t else None, "next": "soon", "last": None})
        dirty()
        return {"ok": True, "result": {"say": "Created the job %s." % a.get("name")}}
    if name in ("stop_loops", "adopt_loop"):
        ids = a.get("jobs") or [a.get("job")]
        LOOPS[:] = [l for l in LOOPS if l["job"] not in ids]
        if name == "adopt_loop":
            JOBS.append({"id": "j%d" % (len(JOBS) + 20), "name": "adopted loop", "schedule": "every 10m", "status": "active", "tab": "trading room", "next": "in 10m", "last": None})
        dirty()
        return {"ok": True, "result": {"say": "Stopped." if name == "stop_loops" else "Adopted as a GodTerm job."}}
    return {"ok": False, "error": "mock: %s is not mocked (or no such tab)" % name}


def handle_text(cl, m):
    t = m.get("t")
    if t == "subscribe":
        for u in m.get("tabs", []):
            u = int(str(u).lstrip("t"))
            tab = next((x for x in TABS if x.uid == u), None)
            if tab:
                with LOCK:
                    cl.send({"t": "snapshot", "tab": u, "cols": tab.cols, "rows": tab.rows, "data": tab.snapshot()})
                    cl.subs.add(u)
    elif t == "unsubscribe":
        for u in m.get("tabs", []):
            cl.subs.discard(int(str(u).lstrip("t")))
    elif t == "resize":
        tab = find(m.get("tab"))
        if tab and m.get("release"):
            tab.cols, tab.rows = tab.own
            broadcast({"t": "resized", "tab": tab.uid, "cols": tab.cols, "rows": tab.rows})
        elif tab:
            tab.cols, tab.rows = int(m["cols"]), int(m["rows"])
            broadcast({"t": "resized", "tab": tab.uid, "cols": tab.cols, "rows": tab.rows})
    elif t == "say":
        uid_ = "u%d" % int(time.time() * 1000)
        broadcast({"t": "assistant", "id": uid_, "role": "user", "text": m.get("text", ""), "done": True})
        CHAT.append({"id": uid_, "role": "user", "text": m.get("text", "")})
        threading.Thread(target=stream_reply, args=(cl, m.get("text", "")), daemon=True).start()
    elif t == "answer":
        ap = APPROVALS.pop(m.get("id"), None)
        cl.send({"t": "answered", "id": m.get("id"), "ok": bool(ap), **({"result": "answered"} if ap else {"error": "that question is no longer open"})})
        if ap:
            ap["tab_ref"].set_state("ready")
            ap["tab_ref"].out("\r\n\x1b[38;5;108mAnswered: %s\x1b[0m\r\n" % m.get("choice"))
            dirty()
            broadcast({"t": "toast", "text": "Answered %s for %s" % (m.get("choice"), ap["tab_ref"].name), "level": "ok"})
    elif t == "tool":
        with LOCK:
            r = tool(m.get("name"), m.get("args") or {})
        time.sleep(0.15)
        cl.send({"t": "result", "id": m.get("id"), "result": r})
    elif t == "ping":
        cl.send({"t": "pong", "id": m.get("id")})
    elif t == "git":
        time.sleep(0.1)
        r = mock_git.frame(m, find)
        cl.send({"t": "git", "id": m.get("id"), **r})
    elif t == "voice":
        if "muted" in m:
            # The browser's mic mute: it sends only silence meanwhile.
            print("voice: client %d mic %s (nonzero audio so far: %d bytes)" % (id(cl) % 1000, "muted" if m["muted"] else "unmuted", cl.audio_nonzero), flush=True)
            cl.audio_nonzero = 0
        elif m.get("on"):
            cl.audio_bytes = 0
        elif m.get("on") is False or m.get("talk") is False:
            secs = cl.audio_bytes / 32000
            broadcast({"t": "assistant", "id": "v%d" % int(time.time() * 1000), "role": "assistant", "text": "(mock) I heard %.1f s of audio." % secs, "done": True})
            cl.bin(2, 0, beep())


def ws_session(sock):
    cl = Client(sock)
    with LOCK:
        CLIENTS.add(cl)
        hello = {"t": "hello", "proto": 1, "version": "mock-0.1", "build": BUILD[0], "client": id(cl) % 1000,
                 "theme": {"bg": "#1c1e21", "fg": "#d8d4cc", "selection": "#3a3e45"}, "state": state(),
                 "messages": CHAT[-30:], "live": None, "update": {"t": "update", "available": False}}
    cl.send(hello)
    try:
        while cl.alive:
            op, data = read_message(sock)
            if op == 8:
                break
            if op == 9:
                cl.frame(10, data)
            elif op == 1:
                handle_text(cl, json.loads(data))
            elif op == 2 and len(data) >= 5:
                kind, uid = struct.unpack("!BI", data[:5])
                if kind == 1:
                    tab = next((x for x in TABS if x.uid == uid), None)
                    if tab:
                        with LOCK:
                            tab.key(data[5:])
                elif kind == 3:
                    cl.audio_bytes += len(data) - 5
                    cl.audio_nonzero += sum(1 for b in data[5:] if b)
    except (ConnectionError, OSError, ValueError):
        pass
    finally:
        with LOCK:
            CLIENTS.discard(cl)
        try:
            sock.close()
        except OSError:
            pass


TYPES = {".html": "text/html; charset=utf-8", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png",
         ".webmanifest": "application/manifest+json", ".json": "application/json", ".txt": "text/plain"}


class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def reply(self, code, body, ctype="text/plain"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        u = urllib.parse.urlparse(self.path)
        if u.path == "/ws" and self.headers.get("Upgrade", "").lower() == "websocket":
            key = self.headers["Sec-WebSocket-Key"]
            acc = base64.b64encode(hashlib.sha1((key + GUID).encode()).digest()).decode()
            self.wfile.write(("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: %s\r\n\r\n" % acc).encode())
            self.wfile.flush()
            self.close_connection = True
            ws_session(self.connection)
            return
        if u.path == "/dev/bump":
            BUILD[0] = hashlib.sha1(str(time.time()).encode()).hexdigest()[:12]
            broadcast({"t": "build", "build": BUILD[0]})
            return self.reply(200, ("build " + BUILD[0]).encode())
        if u.path == "/dev/update":
            broadcast({"t": "update", "available": True, "version": "0.42.0", "current": "0.41.3", "state": "downloaded", "notes": "Faster startup and a new sessions view.", "page": None, "update_with": "Update and restart"})
            return self.reply(200, b"ok")
        if u.path == "/dev/approval":
            with LOCK:
                ask_approval(RNG.choice(TABS))
            return self.reply(200, b"ok")
        path = u.path.lstrip("/") or "index.html"
        full = os.path.realpath(os.path.join(WEB, path))
        if not full.startswith(WEB + os.sep) or "/dev/" in full[len(WEB):] or not os.path.isfile(full):
            return self.reply(404, b"not found")
        with open(full, "rb") as f:
            self.reply(200, f.read(), TYPES.get(os.path.splitext(full)[1], "application/octet-stream"))


def ticker():
    n = 0
    while True:
        time.sleep(0.1)
        n += 1
        with LOCK:
            if n % 10 == 0:
                for t in TABS:
                    t.tick()
                if TABS and TABS[1:2] and TABS[1].state == "ready" and n % 300 == 0:
                    TABS[1].run("work")
            if n == 40 and not APPROVALS and len(TABS) > 3:
                ask_approval(TABS[3])
            if STATE_DIRTY[0]:
                STATE_DIRTY[0] = False
                broadcast({"t": "state", "state": state()})


class Server(http.server.ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=7890)
    ap.add_argument("--bump-every", type=int, default=0, help="seconds between fake web build bumps (0: never)")
    args = ap.parse_args()
    if args.port in (7878, 7879):
        raise SystemExit("ports 7878 and 7879 belong to the real GodTerm")
    seed()
    threading.Thread(target=ticker, daemon=True).start()
    if args.bump_every:
        def bumper():
            while True:
                time.sleep(args.bump_every)
                BUILD[0] = hashlib.sha1(str(time.time()).encode()).hexdigest()[:12]
                broadcast({"t": "build", "build": BUILD[0]})
        threading.Thread(target=bumper, daemon=True).start()
    print("mock GodTerm web on http://127.0.0.1:%d" % args.port, flush=True)
    Server(("127.0.0.1", args.port), H).serve_forever()
