"""The mock's `git` frames: the tab Git sheet (view, prepare, run), the
GitHub settings (cfg_view, cfg_check, cfg_save) and the gh login
(cfg_gh_login, cfg_gh_login_poll, cfg_gh_login_cancel). Fake data only:
nothing here runs git or gh."""
import os
import threading
import time

LOCK = threading.RLock()
GH = [{"name": "dev-user", "logged_in": True, "active": True}, {"name": "dev-work", "logged_in": True, "active": False}]
OWNERS = {}
PLANS = {}
LOGIN = {"state": None}


def gh_names():
    return [a["name"] for a in GH if a["logged_in"]]


def files(n):
    names = ["web/js/git.js", "web/app.css", "src/git_panel.rs", "docs/WEB_APP.md", "README.md", "Cargo.toml", "src/main.rs"]
    return [{"path": names[i % len(names)], "added": 3 + i * 7, "removed": i * 2, "staged": i < 2} for i in range(n)]


def commits(n, unpushed):
    subj = ["Git sheet: wide layout on a desktop", "Mock: git frames", "Fix the status bar width", "Docs: the Git sheet",
            "Session index: Codex sessions too", "Tab panel: resizable from its left edge", "Settings: search box",
            "Voice: push to talk on a phone", "Archive: restore a tab", "First commit"]
    return [{"hash": "%07x" % (0xa1b2c3d + i * 7919), "subject": subj[i % len(subj)], "when": "%d hours ago" % (i * 3 + 1),
             "author": "Alice Example", "unpushed": i < unpushed} for i in range(n)]


def view(tab):
    k = tab.uid % 3
    name = tab.name.replace(" ", "-")
    v = {"dir": tab.folder, "is_repo": k != 2, "top": tab.folder if k != 2 else None, "branch": "", "remote": "", "state": "", "fetched": "",
         "ahead": 0, "behind": 0, "warnings": [], "commits": [], "changes": {"files": [], "untracked": [], "staged_n": 0, "unstaged_n": 0, "added": 0, "removed": 0},
         "branches": [], "missing_ignores": [], "secret_files": [], "draft": None, "proposal": None}
    r = {"dir": tab.folder, "source": "none", "on_github": False, "state_exclude": False}
    if k == 0:
        owner = "acme-org"
        v.update(branch="main", remote="origin github.com/%s/%s" % (owner, name), state="2 ahead", fetched="fetched 4 min ago", ahead=2)
        ch = files(5)
        v["changes"] = {"files": ch, "untracked": ["notes/todo.md", "web/dev/mock_git.py", ".env.local"] + ["build/out-%d.log" % i for i in range(9)], "staged_n": 2, "unstaged_n": 3,
                        "added": sum(f["added"] for f in ch), "removed": sum(f["removed"] for f in ch)}
        v["commits"] = commits(12, 2)
        v["branches"] = [{"name": "main", "upstream": "origin/main", "current": True, "when": "2 hours ago"},
                         {"name": "web-gitui", "upstream": None, "current": False, "when": "1 day ago"}]
        v["missing_ignores"] = [".env*"]
        v["secret_files"] = []
        v["warnings"] = ["2 unpushed", "uncommitted changes"]
        r.update(source="remote", on_github=True, owner=owner, repo=name)
        acc = OWNERS.get(owner)
        if acc:
            r["account"] = acc
        else:
            r["ask"] = "Which gh account pushes for %s? It is not in [git.owners]." % owner
    elif k == 1:
        v.update(branch="feature/x", remote="no remote", state="clean", fetched="never fetched", behind=0)
        v["commits"] = commits(4, 0)
        v["branches"] = [{"name": "feature/x", "upstream": None, "current": True, "when": "3 days ago"}]
        v["warnings"] = ["no GitHub repo"]
        r["ask"] = "No repo defined. Define which GitHub owner it belongs to."
    else:
        v["warnings"] = ["not in git"]
        r["ask"] = "No repo defined. Define which GitHub owner it belongs to."
    v["resolved"] = r
    v["owners"] = sorted(set(list(OWNERS) + ["acme-org"] + gh_names()))
    v["accounts"] = sorted(set(gh_names() + list(OWNERS.values())))
    v["actions"] = actions(v)
    return v


def actions(v):
    a = []
    row = lambda i, l, d=None, hint="": {"id": i, "label": l, "disabled": d, "hint": hint}
    if not v["is_repo"]:
        a.append(row("init", "Make it a git repo", None, "git init -b main here"))
    else:
        ch = v["changes"]
        dirty = ch["staged_n"] + ch["unstaged_n"] + len(ch["untracked"]) > 0
        nr = "no remote yet" if v["remote"] == "no remote" else None
        a += [row("commit", "Commit...", None if dirty else "nothing to commit", "Write or edit the message, then confirm"),
              row("push", "Push", nr, "Checks for secrets and large files first"),
              row("pull", "Pull", nr, "Fast forward only; never merges or rebases"),
              row("sync", "Sync", nr, "Fetch, fast forward if behind, push if ahead"),
              row("fetch", "Fetch", nr, "Remote refs only"),
              row("branches", "Branches...", None, "Switch or create a branch")]
    r = v["resolved"]
    a.append(row("define", "Define repo..." if r["source"] == "none" else "Change binding...", None, "Which GitHub owner, repo name and gh account"))
    if not r["on_github"]:
        a.append(row("create", "Create private repo...", "define the owner, name and account first", "gh repo create --private"))
    if v["missing_ignores"]:
        a.append(row("ignores", "Add .gitignore rules...", None, "Secrets and build output stay out of git"))
    if v["is_repo"] and v["resolved"].get("on_github"):
        a.append(row("untrack", "Untrack secret files...", None, "git rm --cached; the files stay on disk"))
    return a


def cfg_view():
    return {"draft": {"owners": [{"owner": o, "account": a} for o, a in OWNERS.items()], "rules": [], "binds": []},
            "empty": not OWNERS, "placeholders": {"owners": [{"owner": "your-org", "account": "your-gh-account"}], "rules": [], "binds": []},
            "placeholder_owners": ["your-org"], "accounts": list(GH), "projects": [], "bindings": [], "preview": [],
            "check": {"errors": [], "warnings": []}, "config": "~/.godterm/config.toml"}


def frame(m, find):
    op = m.get("op") or "view"
    with LOCK:
        if op == "cfg_view":
            return {"ok": True, "view": cfg_view()}
        if op == "cfg_check":
            d = m.get("draft") or {}
            errs = [("%s is in the owners table twice" % r["owner"]) for i, r in enumerate(d.get("owners", []))
                    if any(x["owner"].lower() == r["owner"].lower() for x in d["owners"][:i])]
            old = dict(OWNERS)
            new = {r["owner"]: r["account"] for r in d.get("owners", [])}
            diff = ["+ owner %s -> %s" % (o, a) for o, a in new.items() if old.get(o) != a]
            pid = None
            if not errs and diff:
                pid = "p%d" % int(time.time() * 1000)
                PLANS[pid] = ("cfg", new)
            return {"ok": True, "check": {"errors": errs, "warnings": []}, "diff": diff, "plan_id": pid, "preview": []}
        if op == "cfg_save":
            p = PLANS.pop(m.get("plan_id"), None)
            if not p or p[0] != "cfg":
                return {"ok": False, "error": "that confirmation expired; press Save again"}
            OWNERS.clear()
            OWNERS.update(p[1])
            return {"ok": True, "said": "Saved: %d owners" % len(OWNERS), "view": cfg_view()}
        if op.startswith("cfg_gh_login") and os.environ.get("MOCK_GIT_NO_LOGIN"):
            return {"ok": False, "error": "unknown git op %r" % op}
        if op == "cfg_gh_login":
            if LOGIN["state"] not in ("starting", "code"):
                LOGIN.clear()
                LOGIN.update(state="starting", started=time.time())
            return {"ok": True, "login": login()}
        if op == "cfg_gh_login_poll":
            return {"ok": True, "login": login()}
        if op == "cfg_gh_login_cancel":
            if LOGIN["state"] in ("starting", "code"):
                LOGIN.update(state="cancelled")
            return {"ok": True, "login": login()}
        tab = find(m.get("tab"))
        if not tab:
            return {"ok": False, "error": "no such tab"}
        if op == "view":
            return {"ok": True, "view": view(tab)}
        if op == "prepare":
            act = m.get("action")
            pid = "p%d" % int(time.time() * 1000)
            PLANS[pid] = ("git", act)
            return {"ok": True, "plan_id": pid, "override": "push anyway",
                    "plan": {"title": act.capitalize(), "lines": ["(mock) would %s in %s" % (act, tab.folder)], "warnings": [], "hard_blocks": [], "soft_blocks": [], "needs_typed": False}}
        if op == "run":
            p = PLANS.pop(m.get("plan_id"), None)
            if not p:
                return {"ok": False, "error": "that confirmation expired; press the button again"}
            return {"ok": True, "said": "(mock) %s done" % p[1], "view": view(tab)}
    return {"ok": False, "error": "unknown git op %r" % op}


def login():
    """The fake `gh auth login --web`: a code after 1 s, done after 8 s."""
    s = LOGIN.get("state")
    if s in ("starting", "code"):
        age = time.time() - LOGIN["started"]
        if age > 8:
            name = "carol-dev"
            if not any(a["name"] == name for a in GH):
                GH.append({"name": name, "logged_in": True, "active": False})
            LOGIN.update(state="done", account=name)
        elif age > 1:
            LOGIN.update(state="code", code="ABCD-1234", url="https://github.com/login/device")
    return {k: v for k, v in LOGIN.items() if k != "started"}
