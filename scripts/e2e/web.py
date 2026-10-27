#!/usr/bin/env python3
"""The web app end to end: `godterm --demo --web` in a private tmux server
with its own HOME, driven over the WebSocket like a browser.

    scripts/e2e/web.py [--bin target/release/godterm] [--port N] [--keep]

Checks sign in, hello, state pushes, a tab's snapshot and live output,
typing into a tab, a tool call through the UI thread, sessions, an
approval, and a turn to the assistant; prints keystroke and UI thread
round trips. Exit status 0 when every step passed.
"""

import argparse
import http.client
import json
import os
import shutil
import statistics
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)

from lib import Tmux  # noqa: E402
from wsclient import WS, login  # noqa: E402

SESSION = "web"


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "godterm"))
    ap.add_argument("--port", type=int, default=0, help="default: a free one")
    ap.add_argument("--keep", action="store_true", help="keep the temp HOME")
    a = ap.parse_args()
    if not a.port:
        import socket

        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            a.port = s.getsockname()[1]

    home = tempfile.mkdtemp(prefix="godterm-e2e-web-")
    tm = Tmux(f"godterm-e2e-web-{os.getpid()}")
    env = {
        "HOME": home,
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin",
        "TERM": "xterm-256color",
        "LANG": "en_US.UTF-8",
        "GODTERM_WEB_PORT": str(a.port),
    }
    argv = [a.bin, "--demo", "--demo-no-external", "--demo-manual", "--web"]
    failed = []

    def step(name, fn):
        t0 = time.time()
        try:
            r = fn()
            print(f"ok   {name} ({time.time() - t0:.1f} s){': ' + str(r) if r else ''}")
        except Exception as e:  # noqa: BLE001
            failed.append(name)
            print(f"FAIL {name}: {e!r}")

    try:
        tm.start(SESSION, argv, env, cwd=home)
        tm.wait(SESSION, "◆ GodTerm", 30)
        tok_path = os.path.join(home, ".godterm-demo", "app", "web-token")
        end = time.time() + 20
        while not os.path.exists(tok_path) and time.time() < end:
            time.sleep(0.2)
        token = open(tok_path).read().strip()
        mode = os.stat(tok_path).st_mode & 0o777
        st = {}

        def http_checks():
            c = http.client.HTTPConnection("127.0.0.1", a.port, timeout=5)
            c.request("GET", "/")
            r = c.getresponse()
            r.read()
            assert r.status == 401, r.status
            c = http.client.HTTPConnection("127.0.0.1", a.port, timeout=5)
            c.request("GET", "/?t=" + "0" * 64)
            r = c.getresponse()
            r.read()
            assert r.status == 403, r.status
            assert mode == 0o600, oct(mode)
            st["cookie"] = login("127.0.0.1", a.port, token)
            c = http.client.HTTPConnection("127.0.0.1", a.port, timeout=5)
            c.request("GET", "/", headers={"Cookie": st["cookie"], "Accept-Encoding": "gzip"})
            r = c.getresponse()
            body = r.read()
            assert r.status == 200 and body, r.status
            return f"index.html {len(body)} bytes ({r.getheader('Content-Encoding') or 'plain'})"

        step("sign in", http_checks)

        def bad_origin():
            try:
                WS("127.0.0.1", a.port, st["cookie"], origin="https://evil.example")
            except ConnectionError as e:
                assert "403" in str(e), e
                return "refused"
            raise AssertionError("a foreign origin got in")

        step("foreign origin refused", bad_origin)

        def hello():
            st["ws"] = WS("127.0.0.1", a.port, st["cookie"])
            h = st["ws"].recv_json("hello")
            for k in ("proto", "version", "build", "state", "theme", "messages", "update"):
                assert k in h, k
            tabs = h["state"]["tabs"]
            assert tabs and all("uid" in t for t in tabs)
            st["hello"] = h
            return f"{len(tabs)} tabs, build {h['build']}"

        step("hello", hello)

        def state_push():
            ws = st["ws"]
            v = ws.recv_json("state", timeout=10)
            assert v["state"]["tabs"]
            return "pushed"

        step("state pushes", state_push)

        def snapshot():
            ws = st["ws"]
            tabs = st["hello"]["state"]["tabs"]
            uids = [t["uid"] for t in tabs]
            ws.send_json({"t": "subscribe", "tabs": uids})
            got = set()
            end = time.time() + 10
            while len(got) < len(uids) and time.time() < end:
                v = ws.recv_json("snapshot")
                got.add(v["tab"])
                assert v["rows"] > 0 and v["cols"] > 0
            assert got == set(uids), (got, uids)
            n = 0
            end = time.time() + 3
            while time.time() < end:
                ws.sock.settimeout(0.5)
                try:
                    op, _ = ws.recv()
                except (TimeoutError, OSError):
                    continue
                n += op == 2
            assert n > 0, "no live output from working tabs"
            return f"{len(uids)} snapshots, {n} output frames in 3 s"

        step("subscribe: snapshots then live output", snapshot)

        def typing():
            ws = st["ws"]
            end = time.time() + 30
            tabs = []
            while not tabs and time.time() < end:
                ws.recv_json("state", timeout=10)
                tabs = [t for t in ws.last_state["tabs"] if t.get("state") == "ready"]
            assert tabs, "no ready tab to type into"
            uid = tabs[0]["uid"]
            times = []
            for ch in "webtest":
                t0 = time.time()
                ws.send_input(uid, ch.encode())
                end = t0 + 3
                seen = b""
                while time.time() < end:
                    ws.sock.settimeout(0.5)
                    try:
                        op, d = ws.recv()
                    except (TimeoutError, OSError):
                        continue
                    if op == 2 and int.from_bytes(d[1:5], "big") == uid:
                        seen += d[5:]
                        if ch.encode() in seen:
                            break
                times.append((time.time() - t0) * 1000)
            ws.send_input(uid, b"\x15")
            return f"echo median {statistics.median(times):.1f} ms, max {max(times):.1f} ms (demo agent redraws)"

        step("typing into a tab", typing)

        def ui_thread():
            ws = st["ws"]
            times = []
            for i in range(30):
                t0 = time.time()
                ws.send_json({"t": "tool", "name": "get_state", "args": {}, "id": i})
                while True:
                    v = ws.recv_json("result")
                    if v["id"] == i:
                        break
                times.append((time.time() - t0) * 1000)
                assert v["result"]["ok"], v
            times.sort()
            return f"get_state through the UI thread: median {statistics.median(times):.1f} ms, p90 {times[26]:.1f} ms"

        step("tool call (UI thread round trip, all tabs streaming)", ui_thread)

        def sessions():
            ws = st["ws"]
            for _ in range(20):
                ws.send_json({"t": "tool", "name": "sessions", "args": {"limit": 500, "sort": "modified", "grouped": False, "include_headless": True}, "id": "s"})
                v = ws.recv_json("result", timeout=20)
                r = v["result"]["result"]
                if not r.get("indexing"):
                    break
                time.sleep(0.5)
            assert r["shown"] > 0, r
            return f"{r['shown']} of {r['matched']} sessions"

        step("sessions", sessions)

        def approval():
            ws = st["ws"]
            ap_ = None
            end = time.time() + 30
            while time.time() < end and ap_ is None:
                v = ws.recv_json("state", timeout=10)
                tabs = [x for x in v["state"]["approvals"] if x["kind"] == "tab"]
                ap_ = tabs[0] if tabs else None
            assert ap_, "no tab waiting for approval"
            ws.send_json({"t": "answer", "id": ap_["id"], "choice": "yes"})
            r = ws.recv_json("answered")
            assert r["ok"], r
            return r["result"]

        step("approval answered from the browser", approval)

        def say():
            ws = st["ws"]
            ws.send_json({"t": "say", "text": "which tabs are working?"})
            roles = []
            end = time.time() + 40
            while time.time() < end:
                v = ws.recv_json("assistant", timeout=end - time.time())
                roles.append(v["role"])
                if v["role"] == "assistant" and v["done"] and v["id"] != "live":
                    return f"reply: {v['text'][:60]!r}"
            raise AssertionError(f"no reply ({roles})")

        step("a turn to the assistant", say)

        def stall():
            lazy = WS("127.0.0.1", a.port, st["cookie"])
            lazy.send_json({"t": "subscribe", "tabs": [t["uid"] for t in st["hello"]["state"]["tabs"]]})
            time.sleep(3)
            ws = st["ws"]
            t0 = time.time()
            ws.send_json({"t": "tool", "name": "get_state", "args": {}, "id": "lazy"})
            while ws.recv_json("result")["id"] != "lazy":
                pass
            ms = (time.time() - t0) * 1000
            lazy.close()
            assert ms < 200, ms
            return f"{ms:.1f} ms with a client that reads nothing"

        step("a stuck browser holds nothing up", stall)
        assert tm.alive(SESSION), "godterm exited"
    finally:
        try:
            st.get("ws") and st["ws"].close()
        except Exception:  # noqa: BLE001
            pass
        tm.keys(SESSION, "C-a", "q", pause=0.3)
        time.sleep(1)
        tm.kill_server()
        if not a.keep:
            shutil.rmtree(home, ignore_errors=True)
    print("PASS" if not failed else f"FAILED: {', '.join(failed)}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
