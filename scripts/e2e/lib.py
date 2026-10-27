"""Shared pieces of the GodTerm e2e suite: a private tmux server that runs
the real binary, screen waits, and a step report that records a capture
on failure and goes on.

Standard library only (runs on the system python3 and on CI runners).
"""

import datetime
import json
import os
import re
import shlex
import shutil
import subprocess
import time
import traceback

COLS, ROWS = 200, 50

PREFIX = "C-a"


class StepFailed(Exception):
    pass


class Tmux:
    """One tmux server of our own (`tmux -L <socket>`), never the user's."""

    def __init__(self, socket, log=None):
        if not shutil.which("tmux"):
            raise SystemExit("e2e: tmux is not installed")
        self.socket = socket
        self.log = log

    def tmux(self, *args, check=True, capture=True):
        cmd = ["tmux", "-L", self.socket, *args]
        r = subprocess.run(cmd, capture_output=capture, text=True)
        if check and r.returncode != 0:
            raise StepFailed(f"tmux {' '.join(args[:2])}: {r.stderr.strip()}")
        return r.stdout if capture else ""

    def start(self, name, argv, env, cwd, cols=COLS, rows=ROWS):
        """A new session running `argv` (a list) with exactly `env`."""
        self.kill(name)
        envs = [f"{k}={v}" for k, v in sorted(env.items())]
        cmd = " ".join(shlex.quote(a) for a in ["env", "-i", *envs, *argv])
        self.tmux("new-session", "-d", "-s", name, "-x", str(cols), "-y", str(rows), "-c", cwd, cmd)
        self.tmux("set-option", "-t", name, "remain-on-exit", "on")
        self.tmux("set-option", "-t", name, "history-limit", "5000")

    def kill(self, name):
        self.tmux("kill-session", "-t", name, check=False)

    def kill_server(self):
        self.tmux("kill-server", check=False)

    def alive(self, name):
        out = self.tmux("list-panes", "-t", name, "-F", "#{pane_dead} #{pane_dead_status}", check=False)
        return bool(out.strip()) and out.split()[0] == "0"

    def exit_status(self, name):
        out = self.tmux("list-panes", "-t", name, "-F", "#{pane_dead} #{pane_dead_status}", check=False).split()
        if len(out) == 2 and out[0] == "1":
            return int(out[1])
        return None

    def keys(self, name, *keys, pause=0.25):
        """tmux key names, e.g. keys(s, "C-a", "t") or keys(s, "Enter")."""
        for k in keys:
            self.tmux("send-keys", "-t", name, k)
            time.sleep(pause)

    def text(self, name, s, pause=0.3):
        """Literal text (no key name lookup)."""
        self.tmux("send-keys", "-t", name, "-l", s)
        time.sleep(pause)

    def cmd(self, name, key, pause=0.4):
        """The GodTerm prefix, then one key."""
        self.keys(name, PREFIX, pause=0.15)
        self.keys(name, key, pause=pause)

    def screen(self, name, ansi=False):
        args = ["capture-pane", "-p", "-t", name]
        if ansi:
            args.insert(2, "-e")
        return self.tmux(*args, check=False)

    def wait(self, name, pattern, timeout=15, gone=False):
        """Wait for a plain string (or "re:<regex>") on the screen; returns
        the screen. `gone`: wait for it to disappear instead."""
        rx = re.compile(pattern[3:]) if pattern.startswith("re:") else re.compile(re.escape(pattern))
        end = time.time() + timeout
        last = ""
        while time.time() < end:
            last = self.screen(name)
            hit = rx.search(last) is not None
            if hit != gone:
                return last
            time.sleep(0.3)
        what = "to disappear" if gone else "on screen"
        raise StepFailed(f"waited {timeout}s for {pattern!r} {what}")

    def wait_any(self, name, patterns, timeout=15):
        """The first of several regexes to show; returns (index, screen)."""
        rxs = [re.compile(p) for p in patterns]
        end = time.time() + timeout
        while time.time() < end:
            s = self.screen(name)
            for i, rx in enumerate(rxs):
                if rx.search(s):
                    return i, s
            time.sleep(0.3)
        raise StepFailed(f"waited {timeout}s for any of {patterns!r}")


class Report:
    """Steps with pass/fail/skip, a capture of the screen on failure (and on
    request), and a summary written as text and JSON."""

    def __init__(self, out_dir, title):
        self.out = out_dir
        self.title = title
        self.screens = os.path.join(out_dir, "screens")
        os.makedirs(self.screens, exist_ok=True)
        self.steps = []
        self.started = time.time()
        self.tmux = None
        self.session = None
        self.notes = []

    def bind(self, tmux, session):
        self.tmux, self.session = tmux, session

    def capture(self, label, session=None):
        session = session or self.session
        if not self.tmux or not session:
            return None
        safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", label)[:80]
        n = len([f for f in os.listdir(self.screens) if f.endswith(".txt")]) + 1
        path = os.path.join(self.screens, f"{n:03d}-{safe}.txt")
        with open(path, "w") as f:
            f.write(self.tmux.screen(session))
        with open(path[:-4] + ".ansi", "w") as f:
            f.write(self.tmux.screen(session, ansi=True))
        return path

    def step(self, name, fn, *args, **kw):
        """Run one step; a failure is recorded with a capture, never raised."""
        t0 = time.time()
        rec = {"name": name, "status": "pass", "secs": 0.0, "detail": "", "capture": None}
        try:
            r = fn(*args, **kw)
            if isinstance(r, tuple) and r and r[0] == "skip":
                rec["status"] = "skip"
                rec["detail"] = r[1]
            elif isinstance(r, str):
                rec["detail"] = r
        except StepFailed as e:
            rec["status"] = "fail"
            rec["detail"] = str(e)
        except Exception as e:
            rec["status"] = "fail"
            rec["detail"] = f"{type(e).__name__}: {e}\n{traceback.format_exc(limit=3)}"
        rec["secs"] = round(time.time() - t0, 1)
        if rec["status"] == "fail":
            rec["capture"] = self.capture(f"FAIL-{name}")
        mark = {"pass": "PASS", "fail": "FAIL", "skip": "SKIP"}[rec["status"]]
        line = f"  [{mark}] {name} ({rec['secs']}s)"
        if rec["detail"]:
            line += f": {rec['detail'].splitlines()[0][:160]}"
        print(line, flush=True)
        self.steps.append(rec)
        return rec["status"] == "pass"

    def note(self, s):
        print(f"  note: {s}", flush=True)
        self.notes.append(s)

    def counts(self):
        c = {"pass": 0, "fail": 0, "skip": 0}
        for s in self.steps:
            c[s["status"]] += 1
        return c

    def write(self):
        c = self.counts()
        lines = [
            f"# {self.title}",
            "",
            f"started {datetime.datetime.fromtimestamp(self.started):%Y-%m-%d %H:%M:%S}, "
            f"{round(time.time() - self.started)}s, "
            f"{c['pass']} passed, {c['fail']} failed, {c['skip']} skipped",
            "",
        ]
        for s in self.steps:
            lines.append(f"- {s['status'].upper():4} {s['name']} ({s['secs']}s)")
            if s["detail"]:
                for d in s["detail"].splitlines()[:6]:
                    lines.append(f"    {d}")
            if s["capture"]:
                lines.append(f"    capture: {s['capture']}")
        if self.notes:
            lines += ["", "## Notes", ""] + [f"- {n}" for n in self.notes]
        with open(os.path.join(self.out, "report.md"), "w") as f:
            f.write("\n".join(lines) + "\n")
        with open(os.path.join(self.out, "report.json"), "w") as f:
            json.dump({"title": self.title, "counts": c, "steps": self.steps, "notes": self.notes}, f, indent=2)
        return c


def base_env(home):
    """The environment every GodTerm under test gets: no sound, no mic, no
    windows opened on the desktop, no splash."""
    env = {
        "HOME": os.environ.get("HOME", ""),
        "USER": os.environ.get("USER", ""),
        "LOGNAME": os.environ.get("LOGNAME", os.environ.get("USER", "")),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "SHELL": os.environ.get("SHELL", "/bin/sh"),
        "LANG": os.environ.get("LANG", "en_US.UTF-8"),
        "TERM": "xterm-256color",
        "GODTERM_NO_AUDIO": "1",
        "GODTERM_NO_MIC": "1",
        "GODTERM_NO_OPEN": "1",
        "GODTERM_NO_SPLASH": "1",
    }
    if home:
        env["GODTERM_HOME"] = home
    return env


def panics_in(path):
    """Lines of a log that report a panic."""
    if not path or not os.path.exists(path):
        return []
    with open(path, errors="replace") as f:
        return [l.rstrip() for l in f if re.search(r"panicked at|\bpanic\b", l)]


def mcp_call(argv_prefix, env, tool, args=None, timeout=20):
    """One control API call through `godterm mcp` (stdio JSON-RPC) against
    the GodTerm whose GODTERM_HOME is in `env`. `argv_prefix` is the
    binary (possibly behind sandbox-exec). Returns the tool's JSON result,
    or {"ok": False, "error": ...}."""
    msgs = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "godterm-e2e", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call",
         "params": {"name": tool, "arguments": args or {}}},
    ]
    inp = "".join(json.dumps(m) + "\n" for m in msgs)
    try:
        r = subprocess.run([*argv_prefix, "mcp"], input=inp, capture_output=True,
                           text=True, env=env, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"ok": False, "error": "godterm mcp timed out"}
    for line in r.stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("id") == 2:
            if "error" in m:
                return {"ok": False, "error": m["error"].get("message")}
            text = m["result"]["content"][0]["text"]
            try:
                return json.loads(text)
            except ValueError:
                return {"ok": not m["result"].get("isError"), "text": text}
    return {"ok": False, "error": (r.stderr or "no reply").strip()[:300]}
