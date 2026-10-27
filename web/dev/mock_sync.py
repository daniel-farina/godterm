"""The mock's GodTerm Sync tools (Settings > Sync): sync_status,
sync_plan (also a join check with repo), sync_init and sync_clone
(asked first, then a fake background setup of a few seconds),
sync_pause, sync_now, take_over and release_session. Fake data only:
nothing here runs git or gh.

Dev endpoints (mock.py): /dev/sync-on and /dev/sync-off switch the
state without the flow."""
import threading
import time

import mock_git

LOCK = threading.RLock()
ST = {"enabled": False, "paused": False, "owner": "", "account": "", "op": None, "last_ms": 0, "fail": False}
TABS = {"here": None, "away": None}


def now_ms():
    return int(time.time() * 1000)


def set_on(on):
    with LOCK:
        ST["enabled"] = on
        ST["paused"] = False
        ST["owner"] = ST["owner"] or mock_git.gh_names()[0]
        ST["account"] = ST["account"] or ST["owner"]
        ST["last_ms"] = now_ms() - 95_000 if on else 0
        if not on:
            ST["op"] = None


PLAN = {"config": True, "stores": {"archive": 12, "jobs": 4, "learned": 9, "projects": 31}, "files": 214, "file_bytes": 38 << 20,
        "env_projects": 6, "env_files": 9, "env_not_ignored": ["~/code/site/.env.local"],
        "env_paths": ["~/code/api/.env", "~/code/site/.env.local"], "sessions": 412, "hot": 23,
        "session_bytes": 3_400 << 20, "new_bytes": 3_400 << 20, "left_out": ["4f2a..: mentions /Volumes/Private", "tmp: temp folder"],
        "vault_mentions": 1, "batches": 14,
        "projects_not_backed_up": ["~/code/notes: not backed up: no GitHub repo", "~/code/site: 3 unpushed"]}


def plan_text():
    return ("Sync plan (dry run, nothing written):\n  config: portable keys of config.toml\n  archive: 12 entries\n  jobs: 4 entries\n"
            "  files (account settings, skills, memory, assistant history): 214 (38.0 MB)\n  .env: 9 files in 6 projects\n"
            "    warning: ~/code/site/.env.local is not ignored by its project's git\n"
            "  conversations: 412 (23 hot), 3.3 GB in all, 3.3 GB to upload (14 syncs)\n")


def preflight(owner, account, name="godterm-state"):
    gh = mock_git.gh_names()
    full = "%s/%s" % (owner, name)
    pf = {"owner": owner, "account": account, "state_repo": full, "convos": "%s/godterm-convos-YYYY-MM" % owner,
          "remote": "github", "github": True, "gh_accounts": gh, "logged_in": account in gh,
          "exists": None, "visibility": None, "can_init": True, "can_clone": True, "warnings": []}
    if not owner:
        pf.update(can_init=False, can_clone=False, warnings=["no GitHub owner yet: log in with gh (gh auth login --hostname github.com) or give the owner"])
        return pf
    if account not in gh:
        pf.update(can_init=False, can_clone=False, warnings=["gh has no github.com login for %s: run gh auth login --hostname github.com (GodTerm never switches gh accounts)" % account])
        return pf
    exists = name == "public-state" or (name == "godterm-state" and owner == "dev-work")
    pf["exists"] = exists
    if exists:
        pf["visibility"] = "PUBLIC" if name == "public-state" else "PRIVATE"
        pf["can_init"] = False
        pf["warnings"].append("%s is there already: join it (clone) instead of enabling a new one" % full)
        if pf["visibility"] != "PRIVATE":
            pf["can_clone"] = False
            pf["warnings"].append("%s is PUBLIC, not private: GodTerm never syncs into it (make it private on GitHub first)" % full)
    else:
        pf["can_clone"] = False
    return pf


def plan(a):
    gh = mock_git.gh_names()
    if a.get("repo"):
        o, _, n = a["repo"].partition("/")
        pf = preflight(o, a.get("account") or o, n)
        pf["warnings"] = [w for w in pf["warnings"] if "is there already" not in w]
        say = ("%s is there and can be joined with the gh account %s." % (a["repo"], pf["account"]) if pf["exists"] and pf["can_clone"]
               else "%s does not exist (or the gh account %s cannot see it)." % (a["repo"], pf["account"]) if pf["exists"] is False
               else "%s cannot be joined: %s" % (a["repo"], "; ".join(pf["warnings"])))
        return {"plan": None, "text": None, "preflight": pf, "warnings": pf["warnings"], "enabled": ST["enabled"], "say": say}
    owner = a.get("owner") or ST["owner"] or (gh[0] if gh else "")
    account = a.get("account") or (a.get("owner") if a.get("owner") else None) or ST["account"] or owner
    pf = preflight(owner, account)
    if ST["enabled"]:
        pf["warnings"] = [w for w in pf["warnings"] if "is there already" not in w]
        pf["can_init"] = pf["can_clone"] = False
    ws = pf["warnings"] + ["9 .env files (secrets) in 6 projects go into the private state repo as they are",
                           "~/code/site/.env.local is not ignored by its project's git", "1 conversation mentions /Volumes/Private and is left out"]
    say = ("Dry run, nothing written: 412 conversations (3.3 GB, 3.3 GB to upload in 14 syncs), 9 .env files, config and 4 stores "
           "would go to the private repo %s with the gh account %s; %d warnings." % (pf["state_repo"], account, len(ws)))
    return {"plan": PLAN, "text": plan_text(), "preflight": pf, "warnings": ws, "enabled": ST["enabled"], "say": say}


def finish(kind, target, secs=4):
    def run():
        time.sleep(secs)
        with LOCK:
            op = ST["op"]
            if not op:
                return
            op["finished_ms"] = now_ms()
            op["ok"] = True
            if kind == "init":
                op["summary"] = {"say": "Sync is on: the first backup is pushed.", "lines": []}
            else:
                op["summary"] = {"say": "Joined %s: 23 conversations placed here, 5 projects; log in to each account again." % target,
                                 "logins": ["Work: log in from GodTerm (its tab runs the claude login; logins are never copied)"],
                                 "missing_paths": ["@studio"], "placed": 23}
            set_on(True)
    threading.Thread(target=run, daemon=True).start()


def status(tabs):
    if not ST["enabled"]:
        return {"enabled": False, "paused": False, "lines": ["Sync is off on this machine."], "state_repo": "/godterm-state",
                "owner": ST["owner"], "account": ST["account"], "remote": "github", "machine": {"label": "This Mac", "id": "a1b2c3d4"},
                "leases": {"fetched": False}, "op": ST["op"], "warnings": [], "repos": [], "token_sync": {"on": False},
                "never": "Project repos are never committed to, pushed or created by the sync.", "say": "Sync is off on this machine."}
    o = ST["owner"]
    here = tabs[0] if tabs else None
    away = tabs[1] if len(tabs) > 1 else None
    last = {"ok": not ST["fail"], "at_ms": ST["last_ms"], "last_ok_ms": ST["last_ms"] - (3_600_000 if ST["fail"] else 0), "pushed": True,
            "waiting": 2 if ST["fail"] else 0, "error": "fetch failed: Could not resolve host: github.com" if ST["fail"] else None,
            "sessions": 412, "sessions_pushed": 3, "bytes_pushed": 2 << 20, "remote_only": 7, "more": False}
    nxt = {"paused": True, "words": "paused"} if ST["paused"] else {"words": "in 1 min", "note": "at the latest; a few seconds after a change"}
    return {
        "enabled": True, "paused": ST["paused"], "lines": [], "state_repo": "%s/godterm-state" % o, "owner": o, "account": ST["account"],
        "remote": "github", "machine": {"label": "This Mac", "id": "a1b2c3d4"}, "worker": {"running": False, "reason": "change"},
        "last": last, "next": nxt,
        "repos": [{"name": "%s/godterm-state" % o, "role": "state", "visibility": "PRIVATE"},
                  {"name": "%s/godterm-convos-2026-10" % o, "role": "conversations", "size": "1.2 GB", "bytes": 1_200 << 20, "sealed": False, "visibility": "PRIVATE"},
                  {"name": "%s/godterm-convos-2026-09" % o, "role": "conversations", "size": "2.1 GB", "bytes": 2_100 << 20, "sealed": True, "visibility": "CHECKING"}],
        "machines": [],
        "leases": {"fetched": True, "online": True,
                   "owned_here": ([{"session": "5e1f9a20-aaaa", "name": here.name, "tab": "t%d" % here.uid}] if here else []) + [{"session": "77aa0c13-bbbb", "name": "release notes"}],
                   "owned_elsewhere": ([{"session": "c0ffee11-cccc", "name": away.name, "owner": "Studio", "active": "active 3 min ago", "live": True, "tab": "t%d" % away.uid}] if away else []),
                   "requests": [], "machines": [{"id": "a1b2c3d4", "label": "This Mac", "this": True, "active": "active just now", "live": True},
                                                {"id": "9f8e7d6c", "label": "Studio", "this": False, "active": "active 3 min ago", "live": True},
                                                {"id": "11223344", "label": "Old laptop", "this": False, "active": "active 12 d ago", "live": False}],
                   "paused_jobs": [{"id": "j3", "name": "nightly report", "why": "paused here: Studio owns its conversation (take it over to run it here)"}]},
        "projects_not_backed_up": [{"path": "~/code/notes", "warnings": ["not backed up: no GitHub repo"]}, {"path": "~/code/site", "warnings": ["3 unpushed"]}],
        "token_sync": {"on": False}, "op": ST["op"],
        "warnings": (["last sync failed: fetch failed: Could not resolve host: github.com"] if ST["fail"] else []) + ["~/code/site/.env.local is not ignored by its project's git"],
        "never": "Project repos are never committed to, pushed or created by the sync.",
        "say": "State repo %s/godterm-state: synced 1 min ago" % o,
    }


def tool(name, a, needs, tabs):
    """None when `name` is not a sync tool."""
    with LOCK:
        if name == "sync_status":
            return {"ok": True, "result": status(tabs)}
        if name == "sync_plan":
            time.sleep(0.8)
            return {"ok": True, "result": plan(a)}
        if name == "sync_init":
            if ST["enabled"]:
                return {"ok": False, "error": "sync is already on here"}
            v = plan(a)
            pf = v["preflight"]
            if not pf["can_init"]:
                return {"ok": False, "error": "cannot enable sync: " + "; ".join(pf["warnings"])}
            if not a.get("_confirmed"):
                return needs(name, a, "Create the PRIVATE GitHub repo %s with the gh account %s, push this machine's first backup (412 conversations (3.3 GB), 9 .env files, config and accounts (no logins)) and from now on private monthly repos %s? They hold your config, .env files and conversations as is; project repos are never touched."
                             % (pf["state_repo"], pf["account"], pf["convos"]))
            ST["owner"], ST["account"] = pf["owner"], pf["account"]
            ST["op"] = {"kind": "init", "target": pf["state_repo"], "started_ms": now_ms(), "finished_ms": None, "ok": None, "error": None, "summary": None}
            finish("init", pf["state_repo"])
            return {"ok": True, "result": {"started": True, "say": "Creating the private repo %s and pushing the first backup in the background." % pf["state_repo"]}}
        if name == "sync_clone":
            repo = a.get("repo") or ""
            if "/" not in repo:
                return {"ok": False, "error": "repo: the state repo as OWNER/NAME (godterm-state, usually)"}
            if not a.get("_confirmed"):
                return needs(name, a, "Join %s on this machine%s with the gh account %s? GodTerm copies its config, accounts (you log in again: logins are never copied), jobs (paused until you turn them on), archive, .env files and the last 7 days of conversations here, and clones the projects it lists into their folders (only cloned or fetched, never committed to or pushed). Nothing on GitHub changes."
                             % (repo, (' as "%s"' % a["label"]) if a.get("label") else "", a.get("account") or repo.split("/")[0]))
            ST["owner"] = repo.split("/")[0]
            ST["account"] = a.get("account") or ST["owner"]
            ST["op"] = {"kind": "clone", "target": repo, "started_ms": now_ms(), "finished_ms": None, "ok": None, "error": None, "summary": None}
            finish("clone", repo)
            return {"ok": True, "result": {"started": True, "say": "Joining %s in the background." % repo}}
        if name == "sync_pause":
            if not ST["enabled"]:
                return {"ok": False, "error": "sync is off on this machine: nothing to pause"}
            ST["paused"] = a.get("pause", True)
            return {"ok": True, "result": {"paused": ST["paused"], "say": "Paused GodTerm Sync on this machine." if ST["paused"] else "Resumed GodTerm Sync on this machine."}}
        if name == "sync_now":
            if not ST["enabled"]:
                return {"ok": False, "error": "sync is off on this machine"}
            ST["last_ms"] = now_ms()
            ST["fail"] = False
            return {"ok": True, "result": {"say": "Syncing GodTerm's state now."}}
        if name in ("take_over", "release_session"):
            if not a.get("_confirmed"):
                q = "Take over this conversation from Studio now? It stops writing it there, and anything it never pushed becomes a fork." if name == "take_over" \
                    else "Hand this conversation over now? Its tab here stops once the current turn ends, then another machine can take it."
                return needs(name, a, q)
            return {"ok": True, "result": {"say": "Done (mock %s)." % name}}
    return None
