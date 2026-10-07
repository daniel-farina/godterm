"""Render every terminal shot make_edit.py planned (rec/renders.json) with
term-render/render.mjs, a few in parallel, into public/rec/<name>.mp4.

    python3 scripts/video/render_term.py [--project ~/godterm-video] [--jobs N] [--only a,b]"""

import argparse
import json
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor

ap = argparse.ArgumentParser()
ap.add_argument("--project", default=os.path.expanduser("~/godterm-video"))
ap.add_argument("--jobs", type=int, default=0, help="Chrome instances at once (default: from free CPU)")
ap.add_argument("--only", default="")
a = ap.parse_args()
P = a.project
REC = os.path.join(P, "rec")
OUT = os.path.join(P, "public", "rec")
os.makedirs(OUT, exist_ok=True)
renders = json.load(open(os.path.join(REC, "renders.json")))
if a.only:
    keep = set(a.only.split(","))
    renders = [r for r in renders if r["name"] in keep]
jobs = a.jobs or max(2, min(8, (os.cpu_count() or 8) // 2 - 1))


def run(r):
    cast = os.path.join(REC, r.get("cast", "godterm.cast"))
    dst = os.path.join(OUT, r["name"] + ".mp4")
    cmd = ["node", os.path.join(P, "term-render", "render.mjs"), "--cast", cast, "--out", dst,
           "--from", str(r["from"]), "--to", str(r["to"]), "--workers", "1"]
    if r.get("plan"):
        cmd += ["--plan", os.path.join(REC, r["plan"])]
    for k in ("vw", "vh", "width"):
        if k in r:
            cmd += ["--" + k, str(r[k])]
    p = subprocess.run(cmd, capture_output=True, text=True)
    ok = p.returncode == 0 and os.path.exists(dst)
    print(("ok   " if ok else "FAIL ") + r["name"], (p.stderr or "")[-400:] if not ok else "", flush=True)
    return ok


with ThreadPoolExecutor(jobs) as ex:
    res = list(ex.map(run, renders))
sys.exit(0 if all(res) else 1)
