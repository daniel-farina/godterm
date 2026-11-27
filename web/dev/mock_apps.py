import time

KINDS = ["app", "game", "utility", "dashboard", "page", "tool", "library", "other"]
PREFILLS = []
RESTORED = []
HOME = "/home/sam"
T0 = time.time()


def reset():
    PREFILLS.clear()
    RESTORED.clear()


def _sess(sid, title, hours, harness="claude", source="Account 1", folder="~/work/harbor", gone=False):
    when = "%d h ago" % hours if hours < 24 else "%d days ago" % (hours // 24)
    return {"id": sid, "title": title, "when": when, "when_ms": int((T0 - hours * 3600) * 1000),
            "harness": harness, "source": source, "folder": folder, "cwd": folder.replace("~", HOME),
            "gone": gone and sid not in RESTORED}


def _app(name, kind, desc, path, project, tech, sessions, tags=(), run="", missing=False, project_missing=None):
    described = kind != "undescribed"
    return {"name": name, "kind": kind, "description": desc, "path": path.replace("~", HOME), "shown_path": path,
            "project": project.replace("~", HOME), "project_name": project.rstrip("/").rsplit("/", 1)[-1],
            "tech": tech, "run": run, "tags": list(tags), "described": described, "missing": missing,
            "project_missing": missing if project_missing is None else project_missing,
            "last": sessions[0]["when"] if sessions else None, "last_ms": sessions[0]["when_ms"] if sessions else None,
            "conversations": len(sessions), "sessions": sessions}


def listing():
    harbor = [_sess("5a1c0de0-0001", "Tide table from the public feed", 2),
              _sess("5a1c0de0-0002", "Menu bar layout and the dark icon", 30),
              _sess("5a1c0de0-0003", "First sketch of the harbor tools", 200, "grok", "Grok 1")]
    apps = [
        _app("Tide Clock", "utility", "A menu bar clock that shows the next high and low tide for a saved harbor.",
             "~/work/harbor/tide-clock", "~/work/harbor", "Swift, SwiftUI", harbor, ["tides", "menu bar", "macos"], "swift run"),
        _app("Buoy Map", "dashboard", "Live wave height and wind from nearby buoys on one map, with a seven day chart for each.",
             "~/work/harbor/buoy-map", "~/work/harbor", "TypeScript, Svelte, D3", harbor, ["weather", "map"], "npm run dev"),
        _app("Lantern Run", "game", "A small roguelike where you carry a lantern through a cave that changes every night.",
             "~/games/lantern-run", "~/games/lantern-run", "Rust, Bevy",
             [_sess("5a1c0de0-0010", "Cave generator: wider corridors", 5, folder="~/games/lantern-run"),
              _sess("5a1c0de0-0011", "Lantern oil and the fade shader", 49, folder="~/games/lantern-run")], ["roguelike", "pixel art"], "cargo run --release"),
        _app("Kiln Timer", "app", "Firing schedules for a pottery kiln, with a phone reminder at each ramp and hold.",
             "~/work/kiln-timer", "~/work/kiln-timer", "Kotlin, Compose",
             [_sess("5a1c0de0-0020", "Cone 6 glaze schedule presets", 80, folder="~/work/kiln-timer")], ["pottery", "android"]),
        _app("Orchard Ledger", "tool", "Command line ledger for harvest weights per tree, with a yearly summary as a table or CSV.",
             "~/work/orchard/ledger", "~/work/orchard", "Python, SQLite", [], ["cli", "garden"], "python3 -m ledger"),
        _app("Field Notes", "page", "One long page of birdwatching notes with a filter by season and place.",
             "~/sites/field-notes", "~/sites/field-notes", "HTML, CSS",
             [_sess("5a1c0de0-0030", "Season filter and print styles", 400, folder="~/sites/field-notes", gone=True),
              _sess("5a1c0de0-0031", "First draft of the notes page", 900, folder="~/sites/field-notes", gone=True)], ["birds"], missing=True),
        _app("Harbor Bell", "utility", "Rings a soft bell in the menu bar when the tide turns.",
             "~/work/harbor/bell", "~/work/harbor", "Swift", harbor, ["tides"], missing=True, project_missing=False),
        _app("Pebble Parse", "library", "A tiny parser for the pebble note format used by the other tools here.",
             "~/work/pebble-parse", "~/work/pebble-parse", "Rust",
             [_sess("5a1c0de0-0040", "Error spans and a fuzz target", 26, folder="~/work/pebble-parse")], ["parser"]),
        _app("Weekly Digest Mailer With A Rather Long Name To Check Wrapping", "other",
             "Collects the week's notes and sends one plain summary every Sunday evening.",
             "~/work/a/rather/deep/folder/that/keeps/going/weekly-digest-mailer", "~/work/a/rather/deep/folder/that/keeps/going/weekly-digest-mailer",
             "Go", [_sess("5a1c0de0-0050", "Template for the Sunday mail", 700, folder="~/work/weekly-digest-mailer")], ["mail"]),
        _app("moss-garden", "undescribed", "", "~/work/moss-garden", "~/work/moss-garden", "",
             [_sess("5a1c0de0-0060", "Watering plan for the north bed", 9, folder="~/work/moss-garden"),
              _sess("5a1c0de0-0061", "Sensor readings to a chart", 55, "grok", "Grok 1", "~/work/moss-garden")]),
        _app("paper-kite", "undescribed", "", "~/old/paper-kite", "~/old/paper-kite", "",
             [_sess("5a1c0de0-0070", "Kite sail outline as SVG", 900, folder="~/old/paper-kite", gone=True),
              _sess("5a1c0de0-0071", "Bridle knots, step by step", 1300, "grok", "Grok 1", "~/old/paper-kite", gone=True)], missing=True),
        _app("snow-globe", "undescribed", "", "~/old/snow-globe", "~/old/snow-globe", "",
             [_sess("5a1c0de0-0080", "Falling snow with a canvas", 2100, folder="~/old/snow-globe", gone=True)], missing=True),
        _app("reed-flute", "undescribed", "", "/mnt/elsewhere/reed-flute", "/mnt/elsewhere/reed-flute", "",
             [_sess("5a1c0de0-0090", "Finger chart for six holes", 3000, folder="/mnt/elsewhere/reed-flute", gone=True)], missing=True),
    ]
    return {"catalog": "~/.godterm/apps.json", "generated_at": "2026-01-10T09:30:00Z", "indexing": False,
            "workspace": "~/work", "kinds": KINDS, "apps": apps}


def handle(cl, m, find):
    base = {"t": "apps", "id": m.get("id"), "op": m.get("op")}
    if m.get("op") == "list":
        cl.send({**base, "ok": True, **listing()})
        return
    if m.get("op") == "restore":
        hit = next((x for a in listing()["apps"] for x in a["sessions"] if x["id"] == m.get("session")), None)
        if not hit:
            cl.send({**base, "ok": False, "error": "that conversation is not in the index"})
        elif not hit["gone"]:
            cl.send({**base, "ok": True, "folder": hit["folder"], "created": False})
        elif not hit["cwd"].startswith(HOME + "/"):
            cl.send({**base, "ok": False, "error": "%s does not exist, and new folders are only made inside the home folder" % hit["folder"]})
        elif m.get("confirm") is not True:
            cl.send({**base, "ok": False, "needs_confirmation": True, "folder": hit["folder"],
                     "error": "%s is gone: confirm to recreate it as an empty folder" % hit["folder"]})
        else:
            RESTORED.append(hit["id"])
            cl.send({**base, "ok": True, "folder": hit["folder"], "created": True})
        return
    if m.get("op") == "prefill":
        t = find(m.get("tab"))
        if not t:
            cl.send({**base, "ok": False, "error": "that tab is gone"})
            return
        if m.get("what") == "describe" and m.get("path"):
            text = "Describe what is built in %s for my personal app catalog (mock prompt, not sent)" % m["path"]
        elif m.get("what") == "rebuild" and m.get("path"):
            text = "The project that used to be in %s is gone from disk (mock prompt, not sent)" % m["path"]
        elif m.get("what") == "refresh":
            text = "Refresh my personal app catalog (mock prompt, not sent)"
        else:
            cl.send({**base, "ok": False, "error": "unknown prompt"})
            return
        PREFILLS.append({"tab": "t%d" % t.uid, "what": m.get("what"), "path": m.get("path"), "session": m.get("session"), "resumed": m.get("resumed")})
        t.out(text)
        cl.send({**base, "ok": True, "tab": "t%d" % t.uid})
        return
    cl.send({**base, "ok": False, "error": "unknown request"})
