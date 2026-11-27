import copy

LIMITS = "Sets proxy variables for the tab's agent. Tools that ignore them (ssh, raw sockets, some CLIs) still connect directly: this is not a kill switch."
HINT = "tailscaled is not installed. Install the open source Tailscale (for example: brew install tailscale), or set its path under Manage routes."
NODES = [{"name": "exit-node-1", "ip": "192.0.2.9", "online": True, "active": True}, {"name": "exit-node-2", "ip": "192.0.2.10", "online": False, "active": False}]

SEED = [
    {"name": "office", "kind": "custom", "url": "http://***@proxy.example:3128", "has_secret": True, "state": "ready", "detail": "Ready"},
    {"name": "exit", "kind": "tailscale", "exit_node": "exit-node-1", "allow_lan": False, "state": "ready", "detail": "Ready", "running": True, "nodes": NODES},
    {"name": "travel", "kind": "tailscale", "exit_node": "", "allow_lan": False, "state": "login", "detail": "Login needed: open the login link once", "login_url": "https://login.example.test/a/abc123", "running": True, "nodes": []},
    {"name": "lab", "kind": "custom", "url": "socks5h://127.0.0.1:1080", "has_secret": False, "state": "unreachable", "detail": "The proxy does not accept connections"},
]
ROUTES = copy.deepcopy(SEED)
TABS = {}
BINS = {"tailscaled": "", "tailscale": "", "found": True, "problem": None, "hint": HINT}


def reset():
    ROUTES[:] = copy.deepcopy(SEED)
    TABS.clear()
    BINS.update({"tailscaled": "", "tailscale": "", "found": True, "problem": None})


def of(uid):
    return TABS.get(uid)


def route(name):
    return next((r for r in ROUTES if r["name"] == name), None)


def view():
    rows = []
    for r in ROUTES:
        v = dict(r)
        v["tabs"] = sum(1 for t in TABS.values() if t["name"] == r["name"])
        rows.append(v)
    return {"routes": rows, "limits": LIMITS, "test_host": "api.anthropic.com", "bins": dict(BINS), "system_nodes": ["exit-node-1", "exit-node-2", "exit-node-3"]}


def ok(say):
    return {"ok": True, "result": {"say": say, "view": view()}}


def place(t, r):
    if r["state"] == "ready":
        TABS[t.uid] = {"name": r["name"], "kind": r["kind"], "state": "on"}
        return "%s restarted through %s" % (t.name, r["name"])
    TABS[t.uid] = {"name": r["name"], "kind": r["kind"], "state": "blocked", "detail": r["detail"][0].lower() + r["detail"][1:]}
    return "%s is set to %s but not started: %s" % (t.name, r["name"], TABS[t.uid]["detail"])


def tool(name, a, find, dirty):
    if name != "tab_route":
        return None
    op = a.get("op") or "view"
    t = find(a.get("tab"))
    nm = (a.get("name") or "").strip()
    if op == "view":
        return {"ok": True, "result": view()}
    if op == "_reset":
        reset()
        dirty()
        return ok("Reset")
    if op == "_missing":
        on = bool(a.get("on"))
        BINS.update({"found": not on, "problem": HINT if on else None})
        for r in ROUTES:
            if r["kind"] == "tailscale":
                seed = next((s for s in SEED if s["name"] == r["name"]), None)
                if on:
                    r.update({"state": "missing", "detail": HINT, "running": False})
                    r.pop("login_url", None)
                elif seed:
                    r.clear()
                    r.update(copy.deepcopy(seed))
        return ok("Mock: tailscaled %s" % ("missing" if on else "found"))
    if op == "set":
        if not t:
            return {"ok": False, "error": "no such tab"}
        want = (a.get("route") or "").strip()
        cur = TABS.get(t.uid)
        if (cur["name"] if cur else "") == want and not (cur and cur["state"] == "blocked"):
            return ok("Nothing changed")
        if want and not route(want):
            return {"ok": False, "error": "no route called %s" % want}
        if t.state == "working" and not a.get("force"):
            return {"ok": True, "result": {"needs_confirm": True, "say": "%s is working. Changing its route restarts it now and resumes the same conversation; the work in progress is interrupted." % t.name}}
        if not want:
            TABS.pop(t.uid, None)
            say = "%s restarted with a direct connection" % t.name
        else:
            say = place(t, route(want))
        dirty()
        return ok(say)
    if op in ("retry", "launch_direct"):
        cur = t and TABS.get(t.uid)
        if not cur or cur["state"] != "blocked":
            return {"ok": False, "error": "that tab is not waiting for its route"}
        if op == "launch_direct":
            cur.update({"state": "bypassed", "detail": "running without the route: you chose to launch it directly"})
            dirty()
            return ok("%s started without its route" % t.name)
        say = place(t, route(cur["name"]))
        dirty()
        return ok(say if TABS[t.uid]["state"] == "on" else "Still not started: %s" % TABS[t.uid]["detail"])
    if op == "save":
        nm = nm.lower()
        was = (a.get("was") or "").strip()
        if not nm or len(nm) > 24 or nm == "direct" or any(not (c.islower() or c.isdigit() or c in "-_") for c in nm):
            return {"ok": False, "error": "a route name is 1 to 24 lowercase letters, digits, - or _ (and not \"direct\")"}
        old = route(was or nm)
        if not was and old:
            return {"ok": False, "error": "a route called %s already exists" % nm}
        kind = a.get("kind") or "custom"
        if kind == "custom":
            url = (a.get("url") or "").strip()
            if not url and not old:
                return {"ok": False, "error": "the proxy URL is required"}
            if url and url.split("://")[0] not in ("http", "https", "socks5", "socks5h"):
                return {"ok": False, "error": "only http://, https://, socks5:// and socks5h:// proxies"}
            masked = old["url"] if old and not url else url
            if url and "@" in url:
                masked = url.split("://")[0] + "://***@" + url.rsplit("@", 1)[1]
            new = {"name": nm, "kind": "custom", "url": masked, "has_secret": "***@" in masked, "state": "ready", "detail": "Ready"}
        else:
            node = (a.get("exit_node") or "").strip()
            new = {"name": nm, "kind": "tailscale", "exit_node": node, "allow_lan": bool(a.get("allow_lan")), "state": "stopped" if node else "choose", "detail": "Starts when a tab uses it" if node else "Choose an exit node", "nodes": []}
            if not BINS["found"]:
                new.update({"state": "missing", "detail": HINT})
        if old:
            old.clear()
            old.update(new)
            for v in TABS.values():
                if v["name"] == was:
                    v["name"] = nm
        else:
            ROUTES.append(new)
        dirty()
        return ok("Saved %s" % nm)
    if op == "delete":
        n = sum(1 for v in TABS.values() if v["name"] == nm)
        if n:
            return {"ok": False, "error": "%d tab%s use %s: set them to Direct first" % (n, "" if n == 1 else "s", nm)}
        if not route(nm):
            return {"ok": False, "error": "no route called %s" % nm}
        ROUTES[:] = [r for r in ROUTES if r["name"] != nm]
        return ok("Deleted %s" % nm)
    if op == "bins":
        BINS.update({"tailscaled": a.get("tailscaled") or "", "tailscale": a.get("tailscale") or ""})
        return ok("Saved the Tailscale program paths")
    if op == "connect":
        r = route(nm)
        if not r or r["kind"] != "tailscale":
            return {"ok": False, "error": "no tailscale route called %s" % nm}
        r.update({"running": True, "state": "ready" if r.get("exit_node") else "choose", "detail": "Ready" if r.get("exit_node") else "Choose an exit node", "nodes": NODES})
        return ok("Starting %s" % nm)
    if op == "test":
        r = route(nm)
        if not r:
            return {"ok": False, "error": "no route called %s" % nm}
        if r["state"] == "ready":
            r["test"] = {"state": "ok", "detail": "Reached api.anthropic.com through the route (TLS handshake only, 182 ms)"}
        else:
            r["test"] = {"state": "failed", "detail": "Not ready yet: %s" % r["detail"] if r["kind"] == "tailscale" else "could not connect to the proxy: connection refused"}
        return ok("Testing %s" % nm)
    return {"ok": False, "error": "unknown op %s" % op}
