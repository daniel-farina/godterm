#!/usr/bin/env python3
"""The web app matrix: GodTerm Web driven by Playwright (WebKit and
Chromium, a 430x932 phone at 3x with touch and a 1440x900 desktop) on
every agent CLI GodTerm hosts, plus WebSocket stress, a flood, a slow
client, two browsers and a soak with memory and CPU sampling.

    scripts/e2e/web_matrix.py                         # demo world, both engines, both sizes
    scripts/e2e/web_matrix.py --instance demo,fake    # + the stand-in CLI (floods, history)
    scripts/e2e/web_matrix.py --instance unauth       # the real CLIs, signed out, temp HOME
    scripts/e2e/web_matrix.py --instance real --real account1,demo8,codex,agy,opencode
    scripts/e2e/web_matrix.py --soak 30               # + 30 minutes of soak (demo)
    scripts/e2e/web_matrix.py --engines chromium --devices phone --groups typing,scroll

Instances (each its own GodTerm, private tmux server, port 7995 and up,
never 7878 / 7880):
  demo    `godterm --demo --demo-harnesses` in a sandboxed temp root: one
          tab per harness, each drawn as that CLI by the demo agent.
  demofull  `godterm --demo` (16 Claude and Grok tabs): tabs and approvals.
  fake    a temp GODTERM_HOME whose Codex and OpenCode slots run
          scripts/e2e/fakecli.py: main screen history, floods on demand.
  unauth  the real CLIs with HOME in a temp root (signed out, nothing of
          yours readable): screens, modes, scrolling, reconnect. Tabs that
          show a login screen get no keys at all.
  real    scripts/sandbox/host (the real logged in CLIs, sandbox-exec,
          nothing logs in or out, no bypass flags) on a separate GodTerm:
          only keys that cannot start anything, plus one tiny turn per
          harness ("reply with the word ok") unless --no-turns.

Needs Playwright for Node (web/dev/node_modules, or PW_DIR=<node_modules>)
with its WebKit and Chromium. Results: e2e-results/web-matrix-<time>/
(report.md with the summary tables, results.jsonl, screenshots).
Exit status 1 when a check failed.
"""

import argparse
import datetime
import json
import os
import re
import shutil
import signal
import socket
import statistics
import subprocess
import sys
import tempfile
import threading
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(REPO, "scripts", "sandbox", "host"))

import sandbox  # noqa: E402
from lib import Tmux  # noqa: E402
from wsclient import WS, login  # noqa: E402

LIVE_PORTS = {7878, 7880}
# A CLI's login or sign in screen (no keys go to it: a key could start a
# login). Its ready composer, by harness (as scripts/e2e/accounts.py).
LOGIN_SCREEN = re.compile(r"(?i)press any key to log ?in|select login method|sign in with (chatgpt|google)|log ?in with (google|chatgpt)|choose (a|your) (login|sign.?in)|please (log|sign) ?in|login required|enter your api key")
READY = {
    "claude": r"\? for shortcuts|❯",
    "grok": r"Type a message|❯",
    "codex": r"Ask Codex to do anything|› ",
    "cursor": r"Plan, search, build anything|Add a follow-up",
    "antigravity": r"\? for shortcuts|^> ",
    "opencode": r"Ask anything|ctrl\+p commands|╹▀",
}
ANSI = re.compile(r"\x1b\[[0-9;?<>=]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[()][0-9A-Za-z]|\x1b[=>78DEHM]")
HARNESSES = ["claude", "grok", "codex", "cursor", "antigravity", "opencode"]


def free_port(start):
    p = start
    while True:
        if p not in LIVE_PORTS:
            with socket.socket() as s:
                try:
                    s.bind(("127.0.0.1", p))
                    return p
                except OSError:
                    pass
        p += 1


def mem_free_gb():
    """Free plus inactive memory (what macOS can hand out without swap)."""
    try:
        out = subprocess.run(["vm_stat"], capture_output=True, text=True).stdout
        page = int(re.search(r"page size of (\d+)", out).group(1))
        n = sum(int(re.search(rf"{k}:\s+(\d+)", out).group(1)) for k in ("Pages free", "Pages inactive", "Pages speculative"))
        return n * page / 1e9
    except Exception:  # noqa: BLE001
        return 99.0


def screen_text(data):
    d = data.split("\x1b[?1049h")[-1]
    d = re.sub(r"\x1b\[(\d+);(\d+)H", "\n", d)
    return ANSI.sub("", d)


class Instance:
    """One GodTerm with the web app on, in our own tmux server."""

    def __init__(self, kind, args, tm, results):
        self.kind, self.a, self.tm, self.results = kind, args, tm, results
        self.session = f"gt-{kind}"
        self.port = free_port(args.port_base)
        self.roots = []
        self.pid = None
        self.extra_root_dirs = []

    # -- starting -------------------------------------------------------
    def start(self):
        getattr(self, "start_" + self.kind)()
        end = time.time() + 40
        while not os.path.exists(self.token_path) and time.time() < end:
            time.sleep(0.3)
        if not os.path.exists(self.token_path):
            raise SystemExit(f"{self.kind}: no web token after 40 s:\n{self.tm.screen(self.session)}")
        self.token = open(self.token_path).read().strip()
        end = time.time() + 20
        while time.time() < end:
            try:
                self.cookie = login("127.0.0.1", self.port, self.token)
                break
            except OSError:
                time.sleep(0.3)
        self.url = f"http://127.0.0.1:{self.port}/?t={self.token}"
        self.pid = self.find_pid()
        print(f"[{self.kind}] up on port {self.port} (pid {self.pid})", flush=True)

    def find_pid(self):
        out = self.tm.tmux("list-panes", "-t", self.session, "-F", "#{pane_pid}", check=False).strip()
        if not out:
            return None
        pid = int(out.split()[0])
        # The pane runs env / sandbox-exec, which exec godterm: same pid.
        return pid

    def start_demofull(self):
        self.start_demo(full=True)

    def start_demo(self, full=False):
        sb = sandbox.prepare_demo(bin_path=self.a.bin, sandboxed=sys.platform == "darwin")
        self.roots.append(sb["root"])
        env = dict(sb["env"])
        env["GODTERM_WEB_PORT"] = str(self.port)
        if self.a.web_dir:
            env["GODTERM_WEB_DIR"] = self.a.web_dir
        self.token_path = os.path.join(sb["app_home"], "web-token")
        self.app_home = sb["app_home"]
        world = [] if full else ["--demo-harnesses"]
        self.tm.start(self.session, sb["argv"] + ["--demo", *world, "--demo-no-external", "--demo-manual", "--web"], env, cwd=sb["root"])

    def _isolated(self, prefix):
        root = os.path.realpath(tempfile.mkdtemp(prefix=prefix, dir="/private/tmp"))
        self.roots.append(root)
        home, gt, proj, tmp = (os.path.join(root, d) for d in ("home", "gt", "projects", "tmp"))
        for d in (home, gt, proj, tmp):
            os.makedirs(d, exist_ok=True)
        return root, home, gt, proj, tmp

    def _env(self, home, gt, tmp, root):
        env = {
            "HOME": home, "USER": os.environ.get("USER", ""), "LOGNAME": os.environ.get("USER", ""),
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin",
            "SHELL": "/bin/zsh", "LANG": "en_US.UTF-8", "TERM": "xterm-256color", "TMPDIR": tmp + "/",
            "GODTERM_HOME": gt, "GODTERM_MAIN_DIR": os.path.join(root, "no-main-claude"),
            "GODTERM_MAIN_GROK_DIR": os.path.join(root, "no-main-grok"),
            "GODTERM_NO_AUDIO": "1", "GODTERM_NO_MIC": "1", "GODTERM_NO_OPEN": "1", "GODTERM_NO_SPLASH": "1",
            "GODTERM_WEB_PORT": str(self.port), "GODTERM_CONTROL_RW": "1",
        }
        fx = os.path.join(REPO, "tests", "fixtures", "usage.json")
        if os.path.exists(fx):
            env["GODTERM_USAGE_FIXTURE"] = fx
        if self.a.web_dir:
            env["GODTERM_WEB_DIR"] = self.a.web_dir
        return env

    def _base_cfg(self, proj, extra=""):
        return "\n".join([
            "notifications = false", "setup_dont_show = true", 'splash = "never"', "remember_window = false",
            'permission_mode = "default"', f'new_tab_base = "{proj}"', extra, "",
        ])

    def _tail_cfg(self):
        return "\n".join([
            "[voice]", "tts = false", "chime = false", 'wake_model = "off"', "",
            "[tabs]", "jobs_tools = false", "",
            "[assistant]", 'mode = "off"', "prewarm = false", "",
            "[update]", "enabled = false", "auto_download = false", "",
            "[supervisor]", "enabled = false", "hang_restart = false", "",
            "[web]", "enabled = true", f"port = {self.port}", "tailnet_ip = false", "tailnet_sign_in = false", "",
        ])

    def start_fake(self):
        root, home, gt, proj, tmp = self._isolated("godterm-webqa-fake-")
        bins = os.path.join(root, "bin")
        os.makedirs(bins)
        self.fake_log = os.path.join(root, "fakecli.log")
        for h in ("codex", "opencode"):
            p = os.path.join(bins, f"fake-{h}")
            with open(p, "w") as f:
                f.write(f"#!/bin/sh\nexport FAKECLI_LOG={self.fake_log} FAKECLI_AS={h}\nexec /usr/bin/python3 {os.path.join(HERE, 'fakecli.py')} \"$@\"\n")
            os.chmod(p, 0o755)
        accts = []
        for h in ("codex", "opencode"):
            d = os.path.join(proj, h)
            os.makedirs(d, exist_ok=True)
            accts += ["[[account]]", f'name = "fake-{h}"', f'label = "Fake {h}"', f'harness = "{h}"', f'cwd = "{d}"', 'permission_mode = "default"']
            if h == "codex":
                accts.append(f'config_dir = "{os.path.join(root, "codex-home")}"')
            accts.append("")
        cfg = self._base_cfg(proj, f'codex_bin = "{bins}/fake-codex"\nopencode_bin = "{bins}/fake-opencode"\n') + "\n".join(accts) + self._tail_cfg()
        with open(os.path.join(gt, "config.toml"), "w") as f:
            f.write(cfg)
        open(os.path.join(gt, "tour_done"), "w").write("1")
        self.token_path = os.path.join(gt, "web-token")
        self.app_home = gt
        env = self._env(home, gt, tmp, root)
        self.tm.start(self.session, [self.a.bin, "--web"], env, cwd=root)

    def start_unauth(self):
        root, home, gt, proj, tmp = self._isolated("godterm-webqa-unauth-")
        accts = []
        for h in HARNESSES:
            b = sandbox.which(h)
            if not b:
                continue
            d = os.path.join(proj, h)
            os.makedirs(d, exist_ok=True)
            accts += ["[[account]]", f'name = "{h}"', f'label = "{h.capitalize()}"', f'harness = "{h}"', f'cwd = "{d}"', 'permission_mode = "default"']
            if h in ("claude", "grok", "codex"):
                accts.append(f'config_dir = "{os.path.join(root, "accounts", h)}"')
            accts.append("")
        bins = "".join(f'{h}_bin = "{sandbox.which(h)}"\n' for h in HARNESSES if sandbox.which(h))
        cfg = self._base_cfg(proj, bins) + "\n".join(accts) + self._tail_cfg()
        with open(os.path.join(gt, "config.toml"), "w") as f:
            f.write(cfg)
        open(os.path.join(gt, "tour_done"), "w").write("1")
        self.token_path = os.path.join(gt, "web-token")
        self.app_home = gt
        env = self._env(home, gt, tmp, root)
        argv = [self.a.bin, "--web"]
        if sys.platform == "darwin":
            prof = os.path.join(root, "unauth.sb")
            no_read = sandbox.SECRET_DIRS + ["~/.godterm", "~/.claude", "~/.claude.json", "~/.grok", "~/.codex", "~/.cursor",
                                             "~/.gemini", "~/.local/share/opencode", "~/.config/opencode", "~/.godterm-demo",
                                             "~/.config/cursor", "~/Library/Application Support/Cursor"]
            rules = ["(version 1)", "(allow default)", '(deny file-write* (subpath "/"))', "(allow file-write*",
                     f"  (subpath {sandbox.sb_quote(root)})", '  (subpath "/dev")', '  (subpath "/private/var/folders")', ")",
                     "(deny file-read*", *[f"  (subpath {sandbox.sb_quote(sandbox.ex(p))})" for p in no_read], ")",
                     "(allow file-read*", f"  (subpath {sandbox.sb_quote(REPO)})", ")"]
            with open(prof, "w") as f:
                f.write("\n".join(rules) + "\n")
            argv = ["/usr/bin/sandbox-exec", "-f", prof] + argv
        self.tm.start(self.session, argv, env, cwd=root)

    def start_real(self):
        keys = self.a.real.split(",")
        extra = f"\n[web]\nenabled = true\nport = {self.port}\ntailnet_ip = false\ntailnet_sign_in = false\n"
        sb = sandbox.prepare(keys=keys, bin_path=self.a.bin, extra_cfg=extra)
        self.roots.append(sb["root"])
        self.extra_root_dirs += sb["outside"]
        env = dict(sb["env"])
        env["GODTERM_WEB_PORT"] = str(self.port)
        env["GODTERM_CONTROL_RW"] = "1"
        if self.a.web_dir:
            env["GODTERM_WEB_DIR"] = self.a.web_dir
        self.sb = sb
        self.token_path = os.path.join(sb["home"], "web-token")
        self.app_home = sb["home"]
        self.tm.start(self.session, sb["argv"] + ["--web"], env, cwd=sb["root"])

    # -- talking to it --------------------------------------------------
    def ws(self):
        return WS("127.0.0.1", self.port, self.cookie)

    def tool(self, w, name, args=None, extra=None, timeout=20):
        i = f"py{time.time_ns()}"
        w.send_json({"t": "tool", "name": name, "args": args or {}, "id": i, **(extra or {})})
        end = time.time() + timeout
        while time.time() < end:
            v = w.recv_json("result", timeout=end - time.time())
            if v.get("id") == i:
                return v["result"]
        raise TimeoutError(name)

    def snapshot(self, w, uid):
        w.send_json({"t": "subscribe", "tabs": [uid]})
        end = time.time() + 10
        while time.time() < end:
            v = w.recv_json("snapshot", timeout=end - time.time())
            if v["tab"] == uid:
                w.send_json({"t": "unsubscribe", "tabs": [uid]})
                return v
        raise TimeoutError("snapshot")

    def settle(self):
        """Wait for the tabs to run; answer start up questions the way a
        user would without saving anything (Codex update: Skip; folder
        trust: through the web approvals). Returns {uid: typing allowed}."""
        w = self.ws()
        h = w.recv_json("hello")
        harness = {a["account"]: a.get("harness") for a in h["state"]["accounts"]}
        end = time.time() + (40 if self.kind in ("real", "unauth") else 20)
        info = {}
        answered = set()
        while time.time() < end:
            try:
                w.recv_json("state", timeout=3)
            except (TimeoutError, OSError):
                pass
            tabs = w.last_state["tabs"]
            pending = False
            for t in tabs:
                hn = harness.get(t["account"])
                if t["state"] in ("starting", "not started"):
                    pending = True
                    continue
                if t["state"] == "waiting for approval" and self.kind in ("real", "unauth") and t["uid"] not in answered:
                    txt = screen_text(self.snapshot(w, t["uid"])["data"])
                    if "Update available" in txt and "Skip" in txt:
                        w.send_input(t["uid"], b"2")
                        answered.add(t["uid"])
                        self.note(f"{hn}: the update menu answered Skip (key 2)")
                    elif re.search(r"(?i)trust", txt):
                        w.send_json({"t": "answer", "id": f"tab:{t['uid']}", "choice": "yes"})
                        r = w.recv_json("answered", timeout=10)
                        answered.add(t["uid"])
                        self.note(f"{hn}: folder trust answered from the web: {r.get('result') or r.get('error')}")
                    pending = True
            if not pending:
                break
            time.sleep(1)
        for t in w.last_state["tabs"]:
            hn = harness.get(t["account"])
            txt = screen_text(self.snapshot(w, t["uid"])["data"])
            login_screen = bool(LOGIN_SCREEN.search(txt))
            composer = bool(re.search(READY.get(hn, "$^"), txt, re.M))
            ok = t.get("running", True) and composer and not login_screen and t["state"] not in ("starting", "not started", "ended")
            if self.kind == "unauth" and hn != "opencode":
                # Signed out: anything but OpenCode's composer is a login,
                # onboarding or setup screen; no keys.
                ok = False
            info[t["uid"]] = {"harness": hn, "typing": ok, "state": t["state"], "login": login_screen}
            if login_screen:
                self.note(f"{hn}: shows a login screen; no keys sent to it")
            elif not composer:
                self.note(f"{hn}: no composer on screen ({t['state']}); no keys sent to it")
        w.close()
        return info

    def note(self, s):
        print(f"[{self.kind}] note: {s}", flush=True)
        self.results.append({"instance": self.kind, "group": "note", "check": "note", "status": "note", "detail": s})

    def rss_cpu(self):
        if not self.pid:
            return None
        out = subprocess.run(["ps", "-o", "rss=,%cpu=,time=", "-p", str(self.pid)], capture_output=True, text=True).stdout.split()
        if len(out) < 2:
            return None
        return {"rss_mb": int(out[0]) / 1024, "cpu": float(out[1]), "time": out[2] if len(out) > 2 else ""}

    def stop(self):
        try:
            self.tm.keys(self.session, "C-a", "Q", pause=0.5)
            time.sleep(2)
        except Exception:  # noqa: BLE001
            pass
        self.tm.kill(self.session)
        if not self.a.keep:
            for r in self.roots + self.extra_root_dirs:
                shutil.rmtree(r, ignore_errors=True)


# ---------------------------------------------------------------------
# Browser runs

def run_browser(inst, engine, device, info, groups, out_dir, extra=None):
    if mem_free_gb() < 6:
        print(f"memory low ({mem_free_gb():.1f} GB free): waiting", flush=True)
        end = time.time() + 120
        while mem_free_gb() < 6 and time.time() < end:
            time.sleep(5)
        if mem_free_gb() < 6:
            return [{"engine": engine, "device": device, "group": "page", "check": "run", "status": "skip", "detail": "less than 6 GB free"}]
    res = os.path.join(out_dir, f"{inst.kind}-{engine}-{device}.jsonl")
    cfgp = res.replace(".jsonl", ".json")
    shots = os.path.join(out_dir, "shots")
    os.makedirs(shots, exist_ok=True)
    cfg = {"url": inst.url, "engine": engine, "device": device, "out": res, "groups": groups,
           "safe": inst.kind == "real", "tabs": {str(u): v for u, v in info.items()}, "shots": shots}
    if inst.kind == "demo":
        cfg["open_account"] = 1
    cfg.update(extra or {})
    with open(cfgp, "w") as f:
        json.dump(cfg, f)
    env = dict(os.environ)
    if inst.a.pw_dir:
        env["PW_DIR"] = inst.a.pw_dir
    print(f"[{inst.kind}] {engine} {device}", flush=True)
    t = cfg.get("soak_s", 0) + 1200
    try:
        subprocess.run(["node", os.path.join(HERE, "web_matrix.mjs"), cfgp], env=env, timeout=t)
    except subprocess.TimeoutExpired:
        pass
    rows = []
    if os.path.exists(res):
        for l in open(res):
            try:
                rows.append(json.loads(l))
            except ValueError:
                pass
    for r in rows:
        r["instance"] = inst.kind
    return rows


# ---------------------------------------------------------------------
# WebSocket level: streaming tabs, a flood, a slow client, two clients

def rec(results, inst, group, check, status, detail="", harness=""):
    r = {"instance": inst.kind, "engine": "ws", "device": "-", "group": group, "check": check, "harness": harness, "status": status, "detail": detail}
    results.append(r)
    mark = {"pass": "ok  ", "fail": "FAIL", "skip": "skip"}.get(status, status)
    print(f"{mark} [{inst.kind}] ws {group}/{check} {harness} {detail}", flush=True)


def ui_rtt(inst, w, n=10):
    ts = []
    for _ in range(n):
        t0 = time.time()
        inst.tool(w, "get_state")
        ts.append((time.time() - t0) * 1000)
    ts.sort()
    return statistics.median(ts), ts[-1]


def drain(w, secs, answer=False):
    """Read for `secs`: output bytes per tab (`answer`: say yes to tab
    approvals as they come, so demo tabs keep working)."""
    got = {}
    said = set()
    end = time.time() + secs
    while time.time() < end:
        w.sock.settimeout(0.2)
        try:
            op, d = w.recv()
        except (TimeoutError, OSError):
            continue
        if op == 2 and d[0] == 1:
            u = int.from_bytes(d[1:5], "big")
            got[u] = got.get(u, 0) + len(d) - 5
        elif op == 1:
            v = json.loads(d)
            if v.get("t") in ("hello", "state"):
                w.last_state = v["state"]
                for a in v["state"].get("approvals", []) if answer else []:
                    if a["kind"] == "tab" and "yes" in a["choices"] and a["id"] not in said:
                        said.add(a["id"])
                        w.send_json({"t": "answer", "id": a["id"], "choice": "yes"})
    return got


def ws_stress(inst, results, info):
    w = inst.ws()
    h = w.recv_json("hello")
    accts = [a["account"] for a in h["state"]["accounts"]]
    # More tabs, all streaming: two extra per account in the demo.
    if inst.kind == "demo":
        for a in accts[:3]:
            inst.tool(w, "open_tab", {"account": a, "prompt": "refactor the module and run the tests"}, {"user_says": "open a tab"})
        time.sleep(6)
    try:
        w.recv_json("state", timeout=3)
    except (TimeoutError, OSError):
        pass
    tabs = w.last_state["tabs"]
    uids = [t["uid"] for t in tabs]
    # Every idle demo tab gets a task, and approvals are answered while
    # it runs, so they all stream at once.
    if inst.kind == "demo":
        for t in tabs:
            if t["state"] == "ready":
                w.send_input(t["uid"], b"refactor the module and run the tests")
                time.sleep(0.2)
                w.send_input(t["uid"], b"\r")
        for a in w.last_state.get("approvals", []):
            if a["kind"] == "tab" and "yes" in a["choices"]:
                w.send_json({"t": "answer", "id": a["id"], "choice": "yes"})
        time.sleep(3)
    w.send_json({"t": "subscribe", "tabs": uids})
    got = drain(w, 8, answer=inst.kind == "demo")
    streaming = sum(1 for u in uids if got.get(u, 0) > 0)
    med, mx = ui_rtt(inst, w)
    rec(results, inst, "stress", f"{len(uids)} tabs subscribed", "pass" if streaming >= min(6, len(uids)) and med < 100 else "fail",
        f"{streaming} streaming, {sum(got.values()) / 8 / 1024:.0f} KB/s, UI thread get_state median {med:.1f} ms, max {mx:.1f} ms")
    # A client that reads nothing holds nothing up.
    lazy = inst.ws()
    lazy.send_json({"t": "subscribe", "tabs": uids})
    time.sleep(3)
    med2, mx2 = ui_rtt(inst, w)
    rec(results, inst, "stress", "slow client (reads nothing)", "pass" if med2 < 150 else "fail", f"get_state median {med2:.1f} ms, max {mx2:.1f} ms with it connected")
    lazy.sock.close()
    # Two clients on one tab: the same stream, and a resize from one
    # reaches the other.
    a, b = inst.ws(), inst.ws()
    a.recv_json("hello"), b.recv_json("hello")
    u = uids[0]
    for c in (a, b):
        c.send_json({"t": "subscribe", "tabs": [u]})
        c.recv_json("snapshot")
    ga, gb = {}, {}
    ta = threading.Thread(target=lambda: ga.update(drain(a, 5)))
    tb = threading.Thread(target=lambda: gb.update(drain(b, 5)))
    ta.start(), tb.start(), ta.join(), tb.join()
    na, nb = ga.get(u, 0), gb.get(u, 0)
    rec(results, inst, "stress", "two browsers, one tab", "pass" if abs(na - nb) <= max(4096, 0.1 * max(na, nb)) else "fail", f"{na} and {nb} bytes in 5 s")
    a.send_json({"t": "resize", "tab": u, "rows": 30, "cols": 70, "by": "qa"})
    seen = None
    end = time.time() + 5
    while time.time() < end and not seen:
        try:
            v = b.recv_json("resized", timeout=end - time.time())
            if v["tab"] == u:
                seen = v
        except TimeoutError:
            break
    rec(results, inst, "stress", "resize seen by the other browser", "pass" if seen and seen["cols"] == 70 and seen["rows"] == 30 else "fail", json.dumps(seen)[:120] if seen else "no resized frame")
    a.send_json({"t": "resize", "tab": u, "release": True})
    a.close(), b.close()
    # Reconnect storm: 20 sockets opened and closed fast.
    t0 = time.time()
    for _ in range(20):
        c = inst.ws()
        c.recv_json("hello")
        c.close()
    med3, _ = ui_rtt(inst, w, 5)
    rec(results, inst, "stress", "20 reconnects", "pass" if med3 < 150 else "fail", f"{time.time() - t0:.1f} s, then get_state {med3:.1f} ms")
    w.close()


def flood(inst, results):
    """30 MB through one tab of the stand-in CLI: a reading client keeps up
    (or is resynced with a snapshot), a client that reads nothing does not
    slow the UI thread, and the server's memory comes back down."""
    w = inst.ws()
    h = w.recv_json("hello")
    tabs = h["state"]["tabs"]
    if not tabs:
        return rec(results, inst, "flood", "30 MB", "skip", "no tab")
    u = tabs[0]["uid"]
    rss0 = inst.rss_cpu()
    w.send_json({"t": "subscribe", "tabs": [u]})
    w.recv_json("snapshot")
    lazy = inst.ws()
    lazy.recv_json("hello")
    lazy.send_json({"t": "subscribe", "tabs": [u]})
    side = inst.ws()
    side.recv_json("hello")
    w.send_input(u, b"flood 30")
    time.sleep(0.3)
    w.send_input(u, b"\r")
    t0 = time.time()
    got = 0
    snaps = 0
    rtts = []
    peak = 0
    done = False
    last_rtt = 0
    buf = b""
    while time.time() - t0 < 120 and not done:
        w.sock.settimeout(0.2)
        try:
            op, d = w.recv()
        except (TimeoutError, OSError):
            op, d = None, b""
        if op == 2 and d[0] == 1:
            got += len(d) - 5
            buf = (buf + d[5:])[-200:]
            if b"flood done" in buf:
                done = True
        elif op == 1:
            v = json.loads(d)
            if v.get("t") == "snapshot":
                snaps += 1
                if "flood done" in v.get("data", ""):
                    done = True
        if time.time() - last_rtt > 1:
            last_rtt = time.time()
            t1 = time.time()
            inst.tool(side, "get_state")
            rtts.append((time.time() - t1) * 1000)
            r = inst.rss_cpu()
            if r:
                peak = max(peak, r["rss_mb"])
    secs = time.time() - t0
    rec(results, inst, "flood", "30 MB through one tab", "pass" if done else "fail",
        f"{got / 1e6:.1f} MB streamed in {secs:.1f} s ({got / 1e6 / max(secs, 0.1):.1f} MB/s), {snaps} resync snapshots, done line seen {done}")
    rec(results, inst, "flood", "UI thread during the flood", "pass" if rtts and statistics.median(rtts) < 200 else "fail",
        f"get_state median {statistics.median(rtts) if rtts else 0:.0f} ms, max {max(rtts) if rtts else 0:.0f} ms ({len(rtts)} calls, a client that reads nothing connected)")
    lazy.sock.close()
    time.sleep(5)
    rss1 = inst.rss_cpu()
    if rss0 and rss1:
        rec(results, inst, "flood", "server memory", "pass" if rss1["rss_mb"] < rss0["rss_mb"] + 150 else "fail",
            f"RSS {rss0['rss_mb']:.0f} MB before, {peak:.0f} MB peak, {rss1['rss_mb']:.0f} MB after")
    # The browser's own copy after the flood: a fresh snapshot ends where the CLI is.
    v = inst.snapshot(side, u)
    txt = screen_text(v["data"])
    rec(results, inst, "flood", "snapshot after the flood", "pass" if "flood done" in txt else "fail", f"{len(v['data']) / 1024:.0f} KB, history {v['data'].split(chr(27) + '[?1049h')[0].count(chr(10))} lines")
    w.close(), side.close()


def bytes_reach_cli(inst, results):
    """The stand-in CLI logs what it reads: the web's bytes arrive intact
    (new line, Send, paste, Ctrl, arrows, wheel on the main screen)."""
    w = inst.ws()
    h = w.recv_json("hello")
    harness = {a["account"]: a.get("harness") for a in h["state"]["accounts"]}
    for t in h["state"]["tabs"]:
        hn = harness.get(t["account"])
        u = t["uid"]
        start = os.path.getsize(inst.fake_log) if os.path.exists(inst.fake_log) else 0
        nl = b"\n" if hn in ("codex", "opencode") else b"\x1b\r"
        seq = [b"one", nl, b"two", b"\x1b[200~pasted\x1b[201~", b"\x15", b"\x1b[A", b"\x1b[Z"]
        for s in seq:
            w.send_input(u, s)
            time.sleep(0.15)
        time.sleep(0.6)
        got = b""
        with open(inst.fake_log) as f:
            f.seek(start)
            for l in f:
                got += json.loads(l)["b"].encode("latin1")
        want = b"".join(seq)
        rec(results, inst, "bytes", "web input reaches the CLI unchanged", "pass" if got == want else "fail", f"{len(got)} of {len(want)} bytes" + ("" if got == want else f": {got[:80]!r}"), hn)
    w.close()


def soak(inst, results, args, out_dir, info):
    """The server under streaming load for N minutes with a browser
    attached: RSS and CPU every 10 s, the page's heap from the driver."""
    samples = []
    stop = threading.Event()

    def sample():
        while not stop.is_set():
            r = inst.rss_cpu()
            if r:
                samples.append({"t": time.time(), **r})
            stop.wait(10)

    th = threading.Thread(target=sample)
    th.start()
    rows = run_browser(inst, "chromium", "phone", info, ["none"], out_dir, {"soak_s": args.soak * 60})
    stop.set()
    th.join()
    results.extend(rows)
    with open(os.path.join(out_dir, f"soak-server-{inst.kind}.json"), "w") as f:
        json.dump(samples, f)
    if len(samples) >= 6:
        q = max(1, len(samples) // 4)
        a = statistics.mean(s["rss_mb"] for s in samples[:q])
        b = statistics.mean(s["rss_mb"] for s in samples[-q:])
        cpu = statistics.mean(s["cpu"] for s in samples)
        rec(results, inst, "soak", f"server over {args.soak} min", "pass" if b < a * 1.25 + 50 else "fail",
            f"RSS {a:.0f} -> {b:.0f} MB (first and last quarter), mean CPU {cpu:.1f}%, {len(samples)} samples")


# ---------------------------------------------------------------------
# Report

def report(results, out_dir, title):
    rows = [r for r in results if r.get("status") in ("pass", "fail", "skip")]
    with open(os.path.join(out_dir, "results.jsonl"), "w") as f:
        for r in results:
            f.write(json.dumps(r) + "\n")
    cols = sorted({(r.get("engine", ""), r.get("device", "")) for r in rows if r.get("engine") not in (None, "ws")})
    harn = [h for h in HARNESSES if any(r.get("harness") == h for r in rows)]
    lines = [f"# {title}", ""]
    for inst in sorted({r["instance"] for r in rows}):
        rs = [r for r in rows if r["instance"] == inst]
        lines += [f"## {inst}", "", "| harness | " + " | ".join(f"{e} {d}" for e, d in cols) + " | ws |", "|---|" + "---|" * (len(cols) + 1)]
        for h in harn + [""]:
            cells = []
            for c in cols + [("ws", "-")]:
                xs = [r for r in rs if (r.get("harness") or "") == h and (r.get("engine"), r.get("device")) == c]
                if not xs:
                    cells.append("")
                    continue
                p = sum(r["status"] == "pass" for r in xs)
                fl = sum(r["status"] == "fail" for r in xs)
                sk = sum(r["status"] == "skip" for r in xs)
                cells.append(f"{p}/{p + fl}" + (f" ({fl} FAIL)" if fl else "") + (f", {sk} skip" if sk else ""))
            if any(cells):
                lines.append(f"| {h or '(all)'} | " + " | ".join(cells) + " |")
        lines.append("")
        groups = sorted({r["group"] for r in rs})
        lines += ["| group | pass | fail | skip |", "|---|---|---|---|"]
        for g in groups:
            xs = [r for r in rs if r["group"] == g]
            lines.append(f"| {g} | {sum(r['status'] == 'pass' for r in xs)} | {sum(r['status'] == 'fail' for r in xs)} | {sum(r['status'] == 'skip' for r in xs)} |")
        lines.append("")
        fails = [r for r in rs if r["status"] == "fail"]
        if fails:
            lines += ["Failures:", ""]
            for r in fails:
                lines.append(f"- {r.get('engine')}/{r.get('device')} {r['group']}/{r['check']} {r.get('harness') or ''}: {r['detail'][:300]}")
            lines.append("")
    notes = [r["detail"] for r in results if r.get("status") == "note"]
    if notes:
        lines += ["## Notes", ""] + [f"- {n}" for n in notes] + [""]
    with open(os.path.join(out_dir, "report.md"), "w") as f:
        f.write("\n".join(lines))
    print("\n".join(lines))
    return sum(r["status"] == "fail" for r in rows)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--instance", default="demo", help="demo,demofull,fake,unauth,real")
    ap.add_argument("--engines", default="webkit,chromium")
    ap.add_argument("--devices", default="phone,desktop")
    ap.add_argument("--groups", default="tabs,live,typing,scroll,resize,reconnect,image,approval,options,chat,voice,stress")
    ap.add_argument("--real", default="account1,demo8,codex,cursor,agy,opencode", help="real instance: sandbox slot keys")
    ap.add_argument("--no-turns", action="store_true", help="real instance: no model turns at all")
    ap.add_argument("--soak", type=float, default=0, help="minutes of soak on the first instance")
    ap.add_argument("--no-ws", action="store_true", help="skip the WebSocket stress checks")
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "godterm"))
    ap.add_argument("--web-dir", help="serve web/ from disk (GODTERM_WEB_DIR)")
    ap.add_argument("--pw-dir", default=os.environ.get("PW_DIR") or (os.path.join(REPO, "web", "dev", "node_modules") if os.path.isdir(os.path.join(REPO, "web", "dev", "node_modules")) else None))
    ap.add_argument("--port-base", type=int, default=7995)
    ap.add_argument("--out")
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--serve", action="store_true", help="start the first instance, print its link and wait (debugging)")
    a = ap.parse_args()
    # A kill still stops the instances and writes the report.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    if a.port_base in LIVE_PORTS:
        raise SystemExit("never the live GodTerm's ports")
    if not a.pw_dir or not os.path.isdir(os.path.join(a.pw_dir, "playwright")):
        raise SystemExit("web_matrix: no Playwright for Node: ln -s <dir>/node_modules web/dev/ or set PW_DIR")
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    out = os.path.abspath(a.out or os.path.join(REPO, "e2e-results", f"web-matrix-{stamp}"))
    os.makedirs(out, exist_ok=True)
    tm = Tmux(f"godterm-webqa-{os.getpid()}")
    results = []
    groups = a.groups.split(",")
    try:
        for kind in a.instance.split(","):
            inst = Instance(kind, a, tm, results)
            try:
                inst.start()
                info = inst.settle()
                if a.serve:
                    print(f"[{kind}] {inst.url}\n[{kind}] tabs: {json.dumps(info)}\nCtrl-C to stop", flush=True)
                    try:
                        while True:
                            time.sleep(3600)
                    except KeyboardInterrupt:
                        return
                print(f"[{kind}] tabs: " + ", ".join(f"t{u} {i['harness']} {i['state']}{'' if i['typing'] else ' (no keys)'}" for u, i in info.items()), flush=True)
                g = groups if kind in ("demo", "fake") else [x for x in groups if x not in ("chat", "voice", "stress")]
                if kind == "demofull":
                    # The full demo world (16 Claude and Grok tabs): their
                    # approvals, which the one-tab-per-harness world rarely shows.
                    g = [x for x in groups if x in ("tabs", "approval")]
                if kind == "fake":
                    g = [x for x in g if x not in ("approval", "options", "chat", "voice")]
                for e in a.engines.split(","):
                    for d in a.devices.split(","):
                        results.extend(run_browser(inst, e, d, info, g, out))
                if not a.no_ws:
                    if kind in ("demo", "fake"):
                        ws_stress(inst, results, info)
                    if kind == "fake":
                        bytes_reach_cli(inst, results)
                        flood(inst, results)
                if kind == "real" and not a.no_turns:
                    # One tiny turn per CLI through the web app's Prompt box.
                    results.extend(run_browser(inst, a.engines.split(",")[-1], "phone", info, ["turn"], out, {"safe": False}))
                if a.soak and kind == a.instance.split(",")[0]:
                    soak(inst, results, a, out, info)
            finally:
                inst.stop()
    finally:
        tm.kill_server()
        fails = report(results, out, f"GodTerm web matrix {stamp}")
    print(f"\nreport: {out}/report.md")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
