#!/usr/bin/env python3
"""GodTerm e2e suite: the real binary in a private tmux server, driven
with send-keys and read with capture-pane.

    scripts/e2e/run.py                    # demo world (no accounts), sandboxed on macOS
    scripts/e2e/run.py --suite accounts   # one cheap prompt per logged in harness (host sandbox)
    scripts/e2e/run.py --suite all
    scripts/e2e/run.py --suite ci         # demo only, no sandbox-exec (GitHub runners)

Options: --bin PATH (default target/release/godterm), --out DIR (default
e2e-results/<suite>-<time>), --only account1,codex (accounts suite),
--keep (leave the sandbox roots for inspection).

Exit status: 0 when no step failed. The report is <out>/report.md.
"""

import argparse
import datetime
import os
import shutil
import signal
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", ".."))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(REPO, "scripts", "sandbox", "host"))

import lib  # noqa: E402
import sandbox  # noqa: E402


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suite", default="demo", choices=["demo", "accounts", "all", "ci"])
    ap.add_argument("--bin", default=os.path.join(REPO, "target", "release", "godterm"))
    ap.add_argument("--out")
    ap.add_argument("--only", help="accounts suite: comma separated slot keys")
    ap.add_argument("--keep", action="store_true", help="keep sandbox roots")
    ap.add_argument("--no-sandbox", action="store_true", help="demo: run without sandbox-exec")
    a = ap.parse_args()
    # A kill still writes the report and stops the tmux server.
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))

    if not os.access(a.bin, os.X_OK):
        raise SystemExit(f"e2e: no binary at {a.bin} (cargo build --release)")
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    out = os.path.abspath(a.out or os.path.join(REPO, "e2e-results", f"{a.suite}-{stamp}"))
    os.makedirs(out, exist_ok=True)
    rep = lib.Report(out, f"GodTerm e2e ({a.suite}) {os.path.basename(a.bin)}")
    tm = lib.Tmux(f"godterm-e2e-{os.getpid()}")
    sandboxed = sys.platform == "darwin" and not a.no_sandbox and a.suite != "ci"
    roots = []
    print(f"e2e: suite {a.suite}, binary {a.bin}, results {out}", flush=True)
    try:
        if a.suite in ("demo", "all", "ci"):
            import demo
            print("demo world", flush=True)
            sb = sandbox.prepare_demo(bin_path=a.bin, sandboxed=sandboxed)
            roots.append(sb["root"])
            demo.run(rep, tm, sb)
            print("demo world, one account per harness", flush=True)
            sb2 = sandbox.prepare_demo(bin_path=a.bin, sandboxed=sandboxed)
            roots.append(sb2["root"])
            demo.run(rep, tm, sb2, harnesses=True)
            print("demo world, onboarding at start", flush=True)
            sb3 = sandbox.prepare_demo(bin_path=a.bin, sandboxed=sandboxed)
            roots.append(sb3["root"])
            demo.run_onboarding_start(rep, tm, sb3)
        if a.suite in ("accounts", "all"):
            if sys.platform != "darwin":
                raise SystemExit("e2e: the accounts suite needs macOS (sandbox-exec)")
            import accounts
            keys = a.only.split(",") if a.only else None
            accounts.run(rep, tm, a.bin, keys, roots, out)
    finally:
        tm.kill_server()
        c = rep.write()
        if not a.keep:
            for r in roots:
                shutil.rmtree(r, ignore_errors=True)
        else:
            rep.note("kept sandbox roots: " + ", ".join(roots))
            rep.write()
    print(f"e2e: {c['pass']} passed, {c['fail']} failed, {c['skip']} skipped; report {out}/report.md")
    sys.exit(1 if c["fail"] else 0)


if __name__ == "__main__":
    main()
