"""Record the GodTerm promo: run `godterm --demo` in a pseudo terminal and
perform every beat of the video (clicks, keys, voice commands through the
demo control tools), writing asciicasts with timed marks:

    python3 scripts/video/director.py [--exe PATH] [--out DIR]

Outputs in DIR (default ~/godterm-video/rec): godterm.cast (the main
240x60 terminal), external.cast (the "other terminal": the tmux session
running the external claude) and marks.json (beat times and the cells of
the UI elements each beat is about, for zooms and highlights).

Nothing here touches real accounts: the demo runs in ~/.godterm-demo."""

import argparse
import json
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from demo_pty import Demo  # noqa: E402

ap = argparse.ArgumentParser()
ap.add_argument("--exe", default=os.path.expanduser("~/claudego/target-demo/release/godterm"))
ap.add_argument("--out", default=os.path.expanduser("~/godterm-video/rec"))
ap.add_argument("--quick", action="store_true", help="shorter holds (testing)")
a = ap.parse_args()
os.makedirs(a.out, exist_ok=True)

HOLD = 0.4 if a.quick else 1.0


def hold(s):
    time.sleep(s * HOLD)


d = Demo(a.exe, cols=240, rows=60, cast=os.path.join(a.out, "godterm.cast"), args=["--demo-manual"])
regions = {}


def region(name, x, y, w, h):
    """A box of cells a beat is about (for zooms and highlights)."""
    regions[name] = {"col": x, "row": y, "cols": w, "rows": h}


def dump(tag):
    with open(os.path.join(a.out, f"screen-{tag}.txt"), "w") as f:
        f.write("\n".join(d.screen()))


def voice(text, partials=()):
    d.control("demo_voice", {"status": "listening"})
    d.mark("voice_start", text=text)
    for p in partials:
        time.sleep(0.45)
        d.control("demo_voice", {"partial": p})
    time.sleep(0.5)
    d.control("demo_voice", {"status": "transcribing"})
    time.sleep(0.25)
    d.control("demo_voice", {"heard": text})
    return d.mark("voice_heard", text=text)


def wait_text(t, timeout=20):
    p = d.wait_for(t, timeout=timeout)
    if not p:
        print(f"!! never saw {t!r}", file=sys.stderr)
        dump("missing-" + "".join(c for c in t if c.isalnum())[:20])
    return p


# The other terminal: the tmux session the demo started for the external
# claude, watched by a second client so it is recorded too.
time.sleep(2.0)
ext = Demo(None, cols=110, rows=32, cast=os.path.join(a.out, "external.cast"),
           argv=["/opt/homebrew/bin/tmux", "-L", "godterm-demo", "attach", "-t", "external"])
# where external.cast's time zero falls on godterm.cast's clock
d.mark("ext_t0", offset=round(ext.t0 - d.t0, 3))

# ---- 1. the grid comes up, everything working
time.sleep(7.0)
d.mark("grid")
L = d.screen()
p = d.find_in("5 hour", 20, 80, 20, 30, L)
if p:
    region("work_gauges", p[0] - 1, p[1] - 1, 58, 4)
hold(7)

# ---- 2. Work burns down to zero
d.point(p[0] + 20, p[1]) if p else None
d.control("demo_burn", {"account": "work", "from": 40, "to": 0, "secs": 9})
d.mark("burn_start")
time.sleep(9.8)
d.mark("burn_zero")
dump("zero")
hold(3)

# ---- 3. move the conversation: double click the tab, pick Research
t = d.wait_tab("payments-api", "ready", timeout=60)
L = d.screen()
p = d.find_in("payme", 0, 22, 2, 12, L)
region("work_tabs", 0, 1, 22, 10)
d.point(p[0] + 3, p[1])
d.mark("dbl_tab")
d.click(p[0] + 3, p[1], double=True)
time.sleep(1.2)
L = d.screen()
r = d.find_in("› Research", 0, 240, 0, 60, L)
dump("picker")
if r:
    top = d.find_in("move 'payments-api'", 0, 240, 0, 60, L)
    if top:
        region("picker", top[0] - 2, top[1], 104, 14)
    d.mark("picker")
    hold(2.2)
    d.point(r[0] + 6, r[1])
    hold(0.6)
    d.click(r[0] + 6, r[1], double=True)
else:
    d.key("\r")
d.mark("moved")
time.sleep(1.5)
L = d.screen()
q = d.find_in("payme", 160, 182, 2, 20, L)
if q:
    region("research_tab", q[0] - 3, q[1] - 1, 22, 3)
region("research_pane", 160, 1, 80, 29)
dump("moved")
hold(6)
d.mark("continues")
hold(3)

# ---- 4. voice: what is everyone working on (with the assistant panel)
d.prefix(".", wait=0.8)
d.mark("panel")
hold(1)
voice("hey god, what's everyone working on?", ["hey god", "hey god what's", "hey god what's everyone working"])
wait_text("sessions on", 15)
d.mark("v1_reply")
dump("v1")
hold(9)

# ---- 5. voice: open a tab on Research and build a pricing page
voice("hey god, open a tab on Research and build a pricing page",
      ["hey god", "hey god open a tab", "hey god open a tab on research", "hey god open a tab on research and build"])
wait_text("pricing page starts", 20)
d.mark("v2_reply")
hold(3.5)
d.prefix(".", wait=0.6)  # close the panel: the new tab shows
d.mark("v2_tab")
L = d.screen()
q = d.find_in("prici", 160, 182, 2, 20, L)
if q:
    region("pricing_tab", q[0] - 3, q[1] - 1, 22, 3)
dump("pricing")
hold(6)

# ---- 6. approvals: one queue for every account
L = d.screen()
b = d.find_in("Approvals", 0, 240, 0, 1, L)
d.point(b[0] + 4, 0)
hold(0.4)
d.click(b[0] + 4, 0)
time.sleep(1.0)
d.mark("approvals")
L = d.screen()
top = d.find_in("approvals:", 0, 240, 0, 60, L)
if top:
    region("approvals", top[0] - 2, top[1], 108, 16)
dump("approvals")
hold(2.5)
for i in range(2):
    L = d.screen()
    ap1 = d.find_in("Approve", 0, 240, 1, 60, L)
    if not ap1:
        break
    d.point(ap1[0] + 3, ap1[1])
    hold(0.5)
    d.click(ap1[0] + 3, ap1[1])
    d.mark("approved", n=i + 1)
    hold(2.0)
dump("approved")
hold(1.5)
d.key("\x1b", 0.8)

# ---- 7. the live map
voice("hey god, show the live map", ["hey god", "hey god show the", "hey god show the live"])
time.sleep(1.2)
d.mark("map")
hold(6)
L = d.screen()
n = d.find("payments-api", L)
if n:
    d.point(n[0] + 4, n[1])
    d.mark("map_hover")
hold(5)
dump("map")
d.key("\x1b", 1.0)

# ---- 8. take over the claude in the other terminal
d.mark("ext_show")
hold(4)
d.prefix("H", wait=1.5)
L = d.screen()
c = d.find("This Mac (main)", L)
d.point(c[0] + 5, c[1])
d.click(c[0] + 5, c[1])
time.sleep(1.2)
d.mark("sessions_main")
L = d.screen()
row = d.find("◉", L)
if row:
    region("ext_row", 0, row[1] - 1, 240, 3)
    d.point(row[0] + 30, row[1])
dump("main")
hold(2.5)
d.key("B", 1.2)
L = d.screen()
tk = d.find("Take over: stop it there", L)
dump("bring")
if tk:
    region("bring", tk[0] - 2, tk[1] - 2, 54, 7)
    d.mark("bring")
    hold(1.5)
    d.point(tk[0] + 8, tk[1])
    hold(0.5)
    d.click(tk[0] + 8, tk[1])
d.mark("takeover")
# until the session runs here
for _ in range(40):
    st = d.control("demo_state").get("result", {})
    if any((x.get("folder") or "").endswith("legacy-api") for x in st.get("tabs", [])):
        break
    time.sleep(0.5)
d.mark("taken")
hold(1.5)
d.key("\x1b", 1.0)
d.mark("taken_grid")
dump("taken")
hold(6)

# ---- 9. tab groups and pins, then search every session
L = d.screen()
g = d.find("▾ Billing", L)
if g:
    region("groups", 0, 1, 24, 12)
    d.point(g[0] + 4, g[1])
    d.mark("groups")
    hold(2.5)
    d.click(g[0] + 4, g[1])
    d.mark("group_fold")
    hold(2.0)
    d.click(g[0] + 4, g[1])
    hold(1.5)
d.prefix("H", wait=1.2)
L = d.screen()
c = d.find("All ", L)
if c:
    d.click(c[0] + 1, c[1])
    time.sleep(0.8)
d.key("/", 0.5)
d.mark("search")
d.type("pod mesh", cps=9)
time.sleep(1.0)
d.mark("search_done")
L = d.screen()
h = d.find("pod mesh", L, after_row=4)
if h:
    region("search_hit", 0, h[1] - 1, 240, 3)
dump("search")
hold(4)
d.key("\r", 0.5)
d.key("\x1b", 0.6)
d.key("\x1b", 0.8)

# ---- 10. the whole thing once more, for the outro
d.prefix("G", wait=0.5)
d.mark("outro_map")
hold(8)
d.mark("end")

json.dump({"marks": d.marks, "regions": regions, "cols": 240, "rows": 60},
          open(os.path.join(a.out, "marks.json"), "w"), indent=1)
ext.alive = False
time.sleep(0.3)
if ext.cast:
    ext.cast.close()
d.stop()
print("recorded", round(d.now(), 1), "s;", len(d.marks), "marks")
