"""Turn a recording (director.py's marks.json) into the cut: per scene
camera plans for the terminal renderer and the Remotion edit.json.

    python3 scripts/video/make_edit.py [--project ~/godterm-video]

Writes <project>/rec/plans/<scene>.json (camera keys in cast time, read by
term-render/render.mjs) and <project>/public/edit.json. Every overlay rect
is computed through the same camera, in final 3840x2160 pixels."""

import argparse
import json
import os
import re

ap = argparse.ArgumentParser()
ap.add_argument("--project", default=os.path.expanduser("~/godterm-video"))
a = ap.parse_args()
P = a.project
REC = os.path.join(P, "rec")
rec = json.load(open(os.path.join(REC, "marks.json")))
narr = {l["id"]: l for l in json.load(open(os.path.join(P, "public/audio/narration.json")))["lines"]}

# ---- the terminal page: 240x60 cells on 1920x1080 CSS px, output 3840 wide
VW, VH, OUTW, OUTH = 1920, 1080, 3840, 2160
CW, CH, OX, OY = 1915 / 240, 18.0, 2.5, 0.0
FULL = {"x": 0.0, "y": 0.0, "w": VW, "h": VH}

marks = {}
for m in rec["marks"]:
    marks.setdefault(m["name"], []).append(m)


def M(name, i=0):
    return marks[name][i]["t"]


def dur(lid, default):
    return narr[lid]["duration"] if lid in narr else default


def cells(col, row, cols, rows):
    return {"x": OX + col * CW, "y": OY + row * CH, "w": cols * CW, "h": rows * CH}


def cam_for(r, pad=1.25, min_w=360.0):
    """A 16:9 camera rect around region r (CSS px), padded, kept on the page."""
    w = max(r["w"] * pad, r["h"] * pad * VW / VH, min_w)
    w = min(w, VW)
    h = w * VH / VW
    x = r["x"] + r["w"] / 2 - w / 2
    y = r["y"] + r["h"] / 2 - h / 2
    x = max(0.0, min(x, VW - w))
    y = max(0.0, min(y, VH - h))
    return {"x": x, "y": y, "w": w, "h": h}


def to_out(r, cam):
    s = OUTW / cam["w"]
    return {"x": round((r["x"] - cam["x"]) * s), "y": round((r["y"] - cam["y"]) * s),
            "w": round(r["w"] * s), "h": round(r["h"] * s)}


def grow(r, px):
    return {"x": r["x"] - px, "y": r["y"] - px, "w": r["w"] + 2 * px, "h": r["h"] + 2 * px}


def screen(tag):
    p = os.path.join(REC, f"screen-{tag}.txt")
    return open(p).read().split("\n") if os.path.exists(p) else []


def find(lines, text, x0=0, x1=10 ** 6, y0=0, y1=10 ** 6):
    for y, l in enumerate(lines):
        if not (y0 <= y < y1):
            continue
        i = l.find(text)
        while i >= 0:
            if x0 <= i < x1:
                return (i, y)
            i = l.find(text, i + 1)
    return None


R = {k: cells(v["col"], v["row"], v["cols"], v["rows"]) for k, v in rec["regions"].items()}
R.setdefault("work_gauges", cells(23, 25, 58, 4))
R["work_pane"] = cells(0, 1, 80, 29)
R["work_footer"] = cells(0, 18, 80, 12)
R["top_gauges"] = cells(0, 24, 160, 5)
R["research_pane"] = cells(160, 1, 80, 29)
R["research_top"] = cells(160, 1, 80, 22)
R["panel_low"] = cells(120, 0, 120, 30)
R["voice_strip"] = cells(0, 58, 160, 1)
R["bottom_left2"] = cells(0, 29, 160, 29)
R["sessions_top"] = cells(0, 0, 140, 12)
R["menu"] = cells(0, 0, 110, 28)

# ---- building the cut
scenes, overlays, voice, renders = [], [], [], []
T = 0.0


def term_scene(name, a, b, rate=1.0, keys=(), focus=None, extra=None):
    """A stretch [a, b] of the recording (cast seconds) at `rate`, with
    camera `keys` [(cast t, rect, ease)], from output time T."""
    global T
    plan = {"camera": [dict(t=round(t, 3), ease=e, **{k: round(v, 2) for k, v in r.items() if k in "xyw"})
                       for (t, r, e) in keys]}
    os.makedirs(os.path.join(REC, "plans"), exist_ok=True)
    json.dump(plan, open(os.path.join(REC, "plans", f"{name}.json"), "w"), indent=1)
    renders.append({"name": name, "from": round(a, 3), "to": round(b, 3), "plan": f"plans/{name}.json"})
    d = (b - a) / rate
    sc = {"type": "term", "start": round(T, 3), "dur": round(d, 3), "src": f"rec/{name}.mp4", "srcStart": 0,
          "rate": rate, "drift": 0, "vignette": 0.25}
    if focus:
        sc["focus"] = focus
    if extra:
        sc.update(extra)
    scenes.append(sc)
    s0 = T

    def out(c):
        return s0 + (c - a) / rate

    def cam(c):
        cur = FULL
        for (t, r, e) in keys:
            if t <= c:
                cur = r
        return cur

    T += d
    return out, cam


def say(lid, at, default=3.0):
    voice.append({"id": lid, "file": f"audio/{lid}.wav", "start": round(at, 3), "role": narr.get(lid, {}).get("role", "narrator")})
    return at + dur(lid, default)


def ov(**kw):
    for k in ("start", "end", "t"):
        if k in kw:
            kw[k] = round(kw[k], 3)
    overlays.append(kw)


# 1. hook: Work's gauge drains (2x speed), zoomed on its footer
cam_hook = cam_for(R["work_gauges"], pad=1.5)
a0 = M("burn_start") + 0.2
out, _ = term_scene("s01_hook", a0, a0 + 8.8, rate=2.0, keys=[(a0 - 1, cam_hook, 0)],
                    focus=to_out(R["work_gauges"], cam_hook), extra={"transition": "cut"})
say("n01_hook", 0.35)
ov(type="kinetic", start=0.5, end=out(a0 + 8.8) - 0.2, text="Usage limit.\nMid-task.", accent=["limit."], style="hook", hideCaptions=False)
ov(type="box", start=out(a0 + 5.5), end=out(a0 + 8.8), rect=to_out(grow(R["work_gauges"], 4), cam_hook), tone="warn")

# 2. intro
scenes.append({"type": "intro", "start": round(T, 3), "dur": 4.3})
say("n02_meet", T + 0.5)
T += 4.3

# 3. the grid: every account side by side, then the gauges
a3 = M("grid") - 0.8
cam_g = cam_for(R["top_gauges"], pad=1.05)
out, cam = term_scene("s03_grid", a3, a3 + 9.6, keys=[(a3 - 1, FULL, 0), (a3 + 5.0, cam_g, 1.2)],
                      focus={"x": 0, "y": 0, "w": OUTW, "h": OUTH})
say("n03_grid", out(a3) + 0.25)
ov(type="spotlight", start=out(a3 + 6.4), end=out(a3 + 9.5), rect=to_out(grow(R["top_gauges"], 2), cam_g), dim=0.55)
ov(type="callout", start=out(a3 + 6.6), end=out(a3 + 9.5), rect=to_out(R["work_gauges"], cam_g), text="5-hour + weekly limits, live", side="top")

# 4. zero
z = M("burn_zero")
cam_z = cam_for(R["work_gauges"], pad=1.7)
out, _ = term_scene("s04_zero", z - 2.4, z + 1.4, keys=[(z - 3.4, cam_z, 0)], focus=to_out(R["work_gauges"], cam_z))
say("n04_zero", out(z - 2.4) + 0.3)
ov(type="box", start=out(z - 0.2), end=out(z + 1.4), rect=to_out(grow(R["work_gauges"], 4), cam_z), tone="warn", label="5-hour limit reached")

# 5. move it: double click the tab, pick the account with the most left
m0 = M("dbl_tab") - 2.6
cam_tabs = cam_for(cells(0, 1, 100, 29), pad=1.0)
pick = R.get("picker", cells(68, 23, 104, 14))
cam_p = cam_for(pick, pad=1.12)
cam_r = cam_for(R["research_top"], pad=1.08)
mv = M("moved")
out, cam = term_scene("s05_move", m0, mv + 5.4, keys=[(m0 - 1, cam_tabs, 0), (M("picker") - 0.15, cam_p, 0.55), (mv + 0.25, cam_r, 0.8)],
                      focus=to_out(pick, cam_p))
t5 = say("n05_move", out(m0) + 0.15)
say("n06_picker", t5 + 0.35)
ov(type="kinetic", start=out(m0) + 0.35, end=out(M("dbl_tab")) - 0.05, text="Move it.\nOne click.", accent=["One", "click."], style="hook", dim=0.25)
L = screen("picker")
rr = find(L, "› Research")
if rr:
    row = cells(rr[0] - 1, rr[1], 100, 1)
    ov(type="box", start=out(M("picker")) + 0.45, end=out(mv) - 0.15, rect=to_out(grow(row, 3), cam_p), tone="ok", label="most left")
rt = R.get("research_tab")
ov(type="callout", start=out(mv) + 1.2, end=out(mv + 5.4) - 0.1, rect=to_out(cells(181, 2, 58, 6), cam_r),
   text="Same conversation. Fresh limits.", side="bottom")

# 6. voice: what is everyone working on; then open a tab by voice
v0 = M("panel") - 1.6
vh0, vh1 = M("voice_heard", 0), M("voice_heard", 1)
rate6 = 0.82
cam_v = cam_for(R["panel_low"], pad=1.0)
cam_pr = cam_for(R["research_top"], pad=1.08)
v_end = M("v2_tab") + 4.4
out, cam = term_scene("s06_voice", v0, v_end, rate=rate6,
                      keys=[(v0 - 1, FULL, 0), (M("panel") + 0.3, cam_v, 0.7), (M("v2_tab") + 0.2, cam_pr, 0.8)],
                      focus=to_out(R["panel_low"], cam_v))
say("n07_ask", out(v0) + 0.3)
ov(type="keys", start=out(M("panel")) - 0.2, end=out(M("panel")) + 1.8, keys=["Ctrl", "A", "."], label="the assistant", pos="tl")
say("u01_working", out(vh0) - dur("u01_working", 1.8) - 0.05)
g1 = say("g01_working", out(M("v1_reply")) + 0.12, 8.9)

say("u02_open", out(vh1) - dur("u02_open", 3.5) - 0.05)
g2 = say("g02_open", out(M("v2_reply")) + 0.12, 4.2)
say("n08_drives", max(g2 + 0.35, out(M("v2_tab")) + 0.3))
L = screen("pricing")
pt = find(L, "prici", 160, 182, 1, 30)
if pt:
    ov(type="box", start=out(M("v2_tab")) + 0.9, end=out(v_end) - 0.1, rect=to_out(grow(cells(pt[0] - 3, pt[1], 21, 2), 3), cam_pr),
       tone="spectrum", label="new tab, by voice")

# 7. approvals: one queue, every account
p0 = M("approvals") - 2.0
ap_r = R.get("approvals", cells(65, 24, 108, 16))
cam_m = cam_for(cells(0, 0, 120, 20), pad=1.0)
cam_a = cam_for(ap_r, pad=1.1)
p1 = M("approved", 1) + 1.0
out, cam = term_scene("s07_approvals", p0, p1, keys=[(p0 - 1, cam_m, 0), (M("approvals") + 0.05, cam_a, 0.6)],
                      focus=to_out(ap_r, cam_a))
say("n09_approve", out(p0) + 0.3)
L = screen("approvals")
b = find(L, "Approvals", 0, 240, 0, 1)
if b:
    ov(type="box", start=out(p0) + 0.3, end=out(M("approvals")) - 0.1, rect=to_out(grow(cells(b[0] - 1, 0, 13, 1), 3), cam_m), tone="violet")
apc = rec["regions"].get("approvals", {"col": 65, "row": 24})
ov(type="callout", start=out(M("approvals")) + 0.6, end=out(M("approved", 0)) + 0.4, rect=to_out(cells(apc["col"] + 2, apc["row"], 40, 1), cam_a),
   text="Every account. One queue.", side="top")

# 8. the live map, by voice
lm0 = M("voice_start", 2) - 0.5
lm1 = M("map") + 7.4
cam_core = cam_for(cells(60, 8, 120, 46), pad=1.0)
out, cam = term_scene("s08_map", lm0, lm1, keys=[(lm0 - 1, FULL, 0), (M("map") + 1.5, cam_core, 6.0)],
                      focus={"x": 0, "y": 0, "w": OUTW, "h": OUTH})
say("u03_map", out(M("voice_heard", 2)) - dur("u03_map", 1.6) - 0.05, 1.6)
g3 = say("g03_map", out(M("map")) - 0.2, 1.2)
say("n10_map", g3 + 0.3)

# 9. take over the claude in the other terminal
ext_off = marks.get("ext_t0", [{"offset": 2.05}])[0].get("offset", 2.05)
e0 = M("ext_show") - 0.3
out, cam = term_scene("s09a_external", e0, e0 + 4.2, keys=[(e0 - 1, FULL, 0)], focus={"x": 0, "y": 0, "w": OUTW, "h": OUTH},
                      extra={"dim": 0.35})
say("n11_takeover", out(e0) + 0.3)
win = {"x": 520, "y": 300, "w": 1756 * 1.55, "h": 1152 * 1.55}
win = {k: round(v) for k, v in win.items()}
ext_from = max(0.0, e0 - ext_off - 1.0)
renders.append({"name": "external", "cast": "external.cast", "from": round(ext_from, 3), "to": round(ext_from + 7.0, 3),
                "vw": 878, "vh": 576, "width": 1756})
ov(type="window", start=out(e0) + 0.15, end=out(e0 + 4.2) + 0.25, rect=win, src="rec/external.mp4",
   srcStart=round(e0 - ext_off - ext_from, 3), title="Terminal · tmux · legacy-api", **{"from": "right"}, dim=0.45)
b0 = M("bring") - 1.6
cam_s = cam_for(cells(0, 0, 120, 30), pad=1.0)
out, cam = term_scene("s09b_bring", b0, M("takeover") + 0.7, keys=[(b0 - 1, cam_s, 0)], focus=to_out(cells(0, 0, 60, 8), cam_s))
ov(type="keys", start=out(M("bring")) - 1.3, end=out(M("bring")) + 0.6, keys=["B"], label="bring it here", pos="br")
br = R.get("bring")
if br:
    ov(type="box", start=out(M("bring")) + 0.1, end=out(M("takeover")) + 0.5, rect=to_out(grow(br, 2), cam_s), tone="violet")
tg = M("taken_grid")
L = screen("taken")
lt = find(L, "legacy-api", 160, 240, 0, 2)
cam_t = cam_for(R["research_top"], pad=1.08)
out, cam = term_scene("s09c_taken", tg - 0.2, tg + 4.4, keys=[(tg - 1.2, cam_t, 0)], focus=to_out(R["research_top"], cam_t))
ov(type="callout", start=out(tg) + 0.4, end=out(tg + 4.4) - 0.1, rect=to_out(cells(181, 2, 58, 6), cam_t), text="History and all.", side="bottom")

# 10. tab groups, pins, colors; then search every session
g0 = M("groups") - 0.8
cam_gr = cam_for(cells(0, 1, 64, 14), pad=1.0)
out, cam = term_scene("s10a_groups", g0, M("group_fold") + 2.2, keys=[(g0 - 1, cam_gr, 0)], focus=to_out(cells(0, 1, 24, 12), cam_gr))
say("n12_tabs", out(g0) + 0.25)
ov(type="callout", start=out(g0) + 0.5, end=out(M("group_fold")) + 2.0, rect=to_out(cells(1, 2, 20, 1), cam_gr), text="Groups · pins · colors", side="right")
s0 = M("search") - 0.6
cam_se = cam_for(cells(0, 0, 150, 14), pad=1.0)
out, cam = term_scene("s10b_search", s0, M("search_done") + 2.4, keys=[(s0 - 1, cam_se, 0)], focus=to_out(cells(0, 0, 100, 8), cam_se))
L = screen("search")
h = find(L, "pod mesh", 60, 240, 4, 30)
if h:
    ov(type="box", start=out(M("search_done")) + 0.1, end=out(M("search_done") + 2.4) - 0.1, rect=to_out(grow(cells(0, h[1], 150, 1), 3), cam_se),
       tone="spectrum", label="every past session")

# 11. Claude Code and Grok Build, together
n0 = M("taken_grid") + 5.0
cam_b = cam_for(cells(0, 29, 160, 29), pad=1.0)
out, cam = term_scene("s11_both", n0, n0 + 4.6, keys=[(n0 - 1, cam_b, 0)], focus=to_out(cells(0, 29, 160, 29), cam_b))
say("n13_both", out(n0) + 0.25)
ov(type="callout", start=out(n0) + 0.5, end=out(n0 + 4.6) - 0.1, rect=to_out(cells(2, 30, 30, 1), cam_b), text="Claude Code", side="bottom")
ov(type="callout", start=out(n0) + 0.9, end=out(n0 + 4.6) - 0.1, rect=to_out(cells(82, 30, 30, 1), cam_b), text="Grok Build", side="bottom", tone="spectrum")

# 12. outro
scenes.append({"type": "outro", "start": round(T, 3), "dur": 8.2})
t = say("n14_outro", T + 0.8)
say("n15_url", t + 0.5)
T += 8.2

edit = {
    "fps": 30, "width": OUTW, "height": OUTH, "durationInFrames": int(round(T * 30)),
    "footage": {"width": OUTW, "height": OUTH}, "transitionFrames": 9, "poster": 12.0,
    "music": {"file": "audio/placeholder-pad.wav", "gain": 0.55, "duckTo": 0.16, "attack": 0.25, "release": 0.6, "fadeIn": 1.0, "fadeOut": 3.0},
    "voice": sorted(voice, key=lambda v: v["start"]), "scenes": scenes, "overlays": overlays,
}
# keep whatever music render.sh set
old = os.path.join(P, "public/edit.json")
if os.path.exists(old):
    try:
        edit["music"]["file"] = json.load(open(old)).get("music", {}).get("file", edit["music"]["file"])
    except Exception:
        pass
json.dump(edit, open(old, "w"), indent=1)
json.dump(renders, open(os.path.join(REC, "renders.json"), "w"), indent=1)
print(f"{T:.1f} s, {len(scenes)} scenes, {len(overlays)} overlays, {len(voice)} voice lines, {len(renders)} renders")
# overlapping voice lines?
vs = sorted(voice, key=lambda v: v["start"])
for x, y in zip(vs, vs[1:]):
    end = x["start"] + dur(x["id"], 2.0)
    if end > y["start"] + 0.05:
        print(f"!! {x['id']} ends {end:.2f} after {y['id']} starts {y['start']:.2f}")
