"""Run `godterm --demo` in a pseudo terminal, record its output with
timestamps (asciicast v2), and drive it: keys, SGR mouse clicks, and the
demo control tools over the control socket. Used by director.py."""

import fcntl
import json
import os
import pty
import select
import signal
import socket
import struct
import termios
import threading
import time


class Demo:
    def __init__(self, exe, cols=240, rows=60, cast=None, env=None, args=(), argv=None):
        self.cols, self.rows = cols, rows
        self.t0 = None
        self.cast = open(cast, "w") if cast else None
        self.marks = []
        self.lock = threading.Lock()
        pid, fd = pty.fork()
        if pid == 0:
            e = dict(os.environ)
            for k in list(e):
                if k.startswith(("GODTERM_", "CLAUDEGO_", "CLAUDE_CODE", "GROK_")) or k in ("CLAUDECODE", "CLAUDE_CONFIG_DIR"):
                    del e[k]
            e.update({"TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "en_US.UTF-8", "TERM_PROGRAM": "xterm.js"})
            e.update(env or {})
            argv = argv or [exe, "--demo", *args]
            os.execve(argv[0], argv, e)
        self.pid, self.fd = pid, fd
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        self.t0 = time.time()
        if self.cast:
            self.cast.write(json.dumps({"version": 2, "width": cols, "height": rows, "timestamp": int(self.t0),
                                        "env": {"TERM": "xterm-256color"}}) + "\n")
        self.alive = True
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()

    def now(self):
        return time.time() - self.t0

    def _read(self):
        dec = __import__("codecs").getincrementaldecoder("utf-8")("replace")
        while self.alive:
            r, _, _ = select.select([self.fd], [], [], 0.2)
            if not r:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                break
            if not data:
                break
            text = dec.decode(data)
            # Answer the terminal queries crossterm makes (cursor position,
            # keyboard enhancement, colors) like a real terminal would.
            if "\x1b[6n" in text:
                self.send("\x1b[1;1R")
            if "\x1b[?u" in text:
                self.send("\x1b[?0u")
            if "\x1b[c" in text:
                self.send("\x1b[?62;22c")
            if text and self.cast:
                with self.lock:
                    self.cast.write(json.dumps([round(self.now(), 4), "o", text]) + "\n")
                    self.cast.flush()
        self.alive = False

    def send(self, s):
        if isinstance(s, str):
            s = s.encode()
        os.write(self.fd, s)

    def mark(self, name, **kw):
        m = {"t": round(self.now(), 3), "name": name, **kw}
        self.marks.append(m)
        if self.cast:
            with self.lock:
                self.cast.write(json.dumps([round(self.now(), 4), "m", json.dumps(m)]) + "\n")
        return m

    # keys
    def key(self, k, wait=0.0):
        self.send(k)
        if wait:
            time.sleep(wait)

    def prefix(self, k, wait=0.35):
        self.send("\x01")
        time.sleep(0.08)
        self.send(k)
        time.sleep(wait)

    def type(self, text, cps=14.0):
        for ch in text:
            self.send(ch)
            time.sleep(1.0 / cps)

    # mouse (cells are 0 based; SGR is 1 based)
    def move(self, x, y):
        self.send(f"\x1b[<35;{x+1};{y+1}M")

    def click(self, x, y, button=0, double=False, hold=0.06):
        self.move(x, y)
        time.sleep(0.05)
        for i in range(2 if double else 1):
            self.send(f"\x1b[<{button};{x+1};{y+1}M")
            time.sleep(hold)
            self.send(f"\x1b[<{button};{x+1};{y+1}m")
            if double and i == 0:
                time.sleep(0.09)
        self.mark("click", x=x, y=y, double=double)

    def wheel(self, x, y, up=True, n=1):
        for _ in range(n):
            self.send(f"\x1b[<{64 if up else 65};{x+1};{y+1}M")
            time.sleep(0.05)

    # control socket
    def control(self, tool, args=None, home=None):
        home = home or os.path.expanduser("~/.godterm-demo/app")
        info = json.load(open(os.path.join(home, "control.json")))
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(25)
        s.connect(info["socket"])
        s.sendall((json.dumps({"token": info["token"], "tool": tool, "args": args or {}}) + "\n").encode())
        buf = b""
        while not buf.endswith(b"\n"):
            c = s.recv(65536)
            if not c:
                break
            buf += c
        s.close()
        return json.loads(buf or b"{}")

    def screen(self):
        r = self.control("demo_screen")
        return r.get("result", {}).get("lines", [])

    def find(self, text, lines=None, nth=0, after_row=0):
        """(x, y) of the nth occurrence of text on screen (cells)."""
        lines = lines if lines is not None else self.screen()
        n = 0
        for y, l in enumerate(lines):
            if y < after_row:
                continue
            start = 0
            while True:
                i = l.find(text, start)
                if i < 0:
                    break
                if n == nth:
                    # character index -> cell column (wide chars are rare here)
                    return (i, y)
                n += 1
                start = i + 1
        return None

    def find_in(self, text, x0=0, x1=10**6, y0=0, y1=10**6, lines=None):
        """First (x, y) of text whose start lies in the box."""
        lines = lines if lines is not None else self.screen()
        for y, l in enumerate(lines):
            if not (y0 <= y < y1):
                continue
            start = 0
            while True:
                i = l.find(text, start)
                if i < 0:
                    break
                if x0 <= i < x1:
                    return (i, y)
                start = i + 1
        return None

    def tab(self, name):
        st = self.control("demo_state").get("result", {})
        for t in st.get("tabs", []):
            if name == t.get("name") or (t.get("folder") or "").endswith("/" + name):
                return t
        return None

    def wait_tab(self, name, state, timeout=90.0):
        end = time.time() + timeout
        while time.time() < end:
            t = self.tab(name)
            if t and state in (t.get("state") or ""):
                return t
            time.sleep(0.5)
        return None

    def point(self, x, y, glide=0.5):
        """Move the drawn pointer there (the app sees a mouse move too)."""
        self.mark("pointer", x=x, y=y, glide=glide)
        self.move(x, y)
        time.sleep(glide + 0.1)

    def wait_for(self, text, timeout=20.0, every=0.25):
        end = time.time() + timeout
        while time.time() < end:
            try:
                p = self.find(text)
            except Exception:
                p = None
            if p:
                return p
            time.sleep(every)
        return None

    def stop(self):
        try:
            self.prefix("Q", wait=1.5)
        except OSError:
            pass
        for _ in range(30):
            if not self.alive:
                break
            time.sleep(0.2)
        try:
            os.kill(self.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        time.sleep(0.5)
        self.alive = False
        if self.cast:
            self.cast.close()
