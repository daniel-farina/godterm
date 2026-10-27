#!/usr/bin/env python3
"""The web app served from a folder and reloaded when it changes, end to
end: `godterm --demo --web` in a private tmux server with its own HOME,
then `[web] assets_dir` set to a temp copy of web/ in its config.toml.

    scripts/e2e/web_hot.py [--bin target/release/godterm] [--port N] [--keep]

Checks the copy is served, an edit to a module and to index.html reaches
a connected client as a `build` frame within a few seconds, the new files
are served under the new build, a folder that breaks (index.html gone)
keeps the last good files, and a path outside the folder is refused.
Exit status 0 when every step passed.
"""

import argparse
import http.client
import os
import re
import shutil
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)

from lib import Tmux  # noqa: E402
from wsclient import WS, login  # noqa: E402

SESSION = "webhot"


def get(port, path, cookie, timeout=5):
    c = http.client.HTTPConnection("127.0.0.1", port, timeout=timeout)
    c.request("GET", path, headers={"Cookie": cookie})
    r = c.getresponse()
    return r.status, r.read(), dict((k.lower(), v) for k, v in r.getheaders())


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
    assert a.port not in (7878, 7880), "never the live GodTerm's ports"

    home = tempfile.mkdtemp(prefix="godterm-e2e-webhot-")
    web = os.path.join(home, "web-copy")
    shutil.copytree(os.path.join(REPO, "web"), web)
    tm = Tmux(f"godterm-e2e-webhot-{os.getpid()}")
    env = {
        "HOME": home,
        "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin:/usr/local/bin",
        "TERM": "xterm-256color",
        "LANG": "en_US.UTF-8",
        "GODTERM_WEB_PORT": str(a.port),
    }
    argv = [a.bin, "--demo", "--demo-no-external", "--demo-manual", "--web"]
    failed = []
    st = {}

    def step(name, fn):
        t0 = time.time()
        try:
            r = fn()
            print(f"ok   {name} ({time.time() - t0:.1f} s){': ' + str(r) if r else ''}")
        except Exception as e:  # noqa: BLE001
            failed.append(name)
            print(f"FAIL {name}: {e!r}")

    def hello():
        ws = WS("127.0.0.1", a.port, st["cookie"])
        return ws, ws.recv_json("hello")

    def served_build():
        s, body, _ = get(a.port, "/", st["cookie"])
        assert s == 200, s
        m = re.search(rb"main\.js\?v=([0-9a-f]+)", body)
        assert m, "index.html names no versioned main.js"
        return m.group(1).decode(), body

    try:
        tm.start(SESSION, argv, env, cwd=home)
        tm.wait(SESSION, "◆ GodTerm", 30)
        app = os.path.join(home, ".godterm-demo", "app")
        tok_path = os.path.join(app, "web-token")
        end = time.time() + 20
        while not os.path.exists(tok_path) and time.time() < end:
            time.sleep(0.2)
        token = open(tok_path).read().strip()
        st["cookie"] = login("127.0.0.1", a.port, token)

        def built_in():
            ws, h = hello()
            ws.close()
            st["first"] = h["build"]
            return f"build {h['build']}"

        step("serving", built_in)

        def switch_to_copy():
            with open(os.path.join(web, "js", "util.js"), "a") as f:
                f.write("\n// e2e copy\n")
            cfg = os.path.join(app, "config.toml")
            text = open(cfg).read()
            line = f"assets_dir = {web!r}\n".replace("'", '"')
            if "[web]" in text:
                text = text.replace("[web]\n", "[web]\n" + line, 1)
            else:
                text += "\n[web]\n" + line
            open(cfg, "w").write(text)
            end = time.time() + 20
            while time.time() < end:
                try:
                    st["cookie"] = login("127.0.0.1", a.port, token)
                    b, _ = served_build()
                    if b != st["first"]:
                        _, body, _ = get(a.port, f"/js/util.js?v={b}", st["cookie"])
                        assert b"// e2e copy" in body
                        st["build"] = b
                        return f"build {b} from {web}"
                except (OSError, RuntimeError, AssertionError):
                    pass
                time.sleep(0.3)
            raise AssertionError("the copy was never served")

        step("assets_dir from config.toml", switch_to_copy)

        def edit_module():
            ws, h = hello()
            assert h["build"] == st["build"], (h["build"], st["build"])
            t0 = time.time()
            with open(os.path.join(web, "js", "util.js"), "a") as f:
                f.write("export const E2E_HOT = 1;\n")
            m = ws.recv_json("build", timeout=10)
            dt = time.time() - t0
            ws.close()
            assert m["build"] != st["build"]
            b, _ = served_build()
            assert b == m["build"], (b, m["build"])
            s, body, hd = get(a.port, f"/js/util.js?v={b}", st["cookie"])
            assert s == 200 and b"E2E_HOT" in body, s
            assert "immutable" in hd.get("cache-control", ""), hd
            s, _, hd = get(a.port, f"/js/util.js?v={st['build']}", st["cookie"])
            assert s == 200 and hd.get("cache-control") == "no-cache", hd
            s, body, _ = get(a.port, "/sw.js", st["cookie"])
            assert f"'{b}'".encode() in body, "sw.js has the old build"
            st["build"] = b
            return f"build frame after {dt:.1f} s, new build {b}"

        step("module edit pushes a build", edit_module)

        def edit_page():
            ws, _ = hello()
            p = os.path.join(web, "index.html")
            html = open(p).read()
            open(p, "w").write(html.replace("</body>", "<!-- e2e page --></body>"))
            m = ws.recv_json("build", timeout=10)
            ws.close()
            b, body = served_build()
            assert b == m["build"] and b"e2e page" in body
            st["build"] = b
            return f"build {b}"

        step("index.html edit pushes a build", edit_page)

        def broken_keeps_last():
            p = os.path.join(web, "index.html")
            os.rename(p, p + ".off")
            time.sleep(4.5)
            b, _ = served_build()
            assert b == st["build"], "a folder without index.html replaced the app"
            os.rename(p + ".off", p)
            return "kept"

        step("a broken folder keeps the last files", broken_keeps_last)

        def outside():
            secret = os.path.join(home, "secret.txt")
            open(secret, "w").write("secret")
            os.symlink(secret, os.path.join(web, "leak.txt"))
            time.sleep(4.5)
            for path in ("/leak.txt", "/../secret.txt", "/%2e%2e/secret.txt", "/js/../../secret.txt"):
                s, body, _ = get(a.port, path, st["cookie"])
                assert b"secret" not in body and s != 200, (path, s)
            return "refused"

        step("nothing outside the folder", outside)
    finally:
        tm.keys(SESSION, "C-a", "q", pause=0.3)
        time.sleep(1)
        tm.kill_server()
        if not a.keep:
            shutil.rmtree(home, ignore_errors=True)
    print("PASS" if not failed else f"FAILED: {', '.join(failed)}")
    return 0 if not failed else 1


if __name__ == "__main__":
    sys.exit(main())
