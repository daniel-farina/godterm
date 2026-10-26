"""Real account scenarios (host sandbox, layer b): for each logged in
harness, one GodTerm in the sandbox with that one slot, one tab in a temp
project folder, ONE cheap prompt, its reply, GodTerm's activity and
receipt states, then close and quit.

Claude and Grok get the prompt through the control API (send_prompt), so
the receipt states are checked; Codex, Cursor, Antigravity and OpenCode
get it typed through tmux, as a user would.
"""

import hashlib
import json
import os
import re
import subprocess
import time

import sandbox
from lib import StepFailed, mcp_call, panics_in

PROMPT = "reply with the single word ok"
# E2E_NO_PROMPT=1: launch, ready, close, quit; no model usage.
NO_PROMPT = os.environ.get("E2E_NO_PROMPT") == "1"
S = "acct"

# The CLI's own ready screen (its prompt box), by harness.
READY = {
    "claude": r"\? for shortcuts|❯\s",
    "grok": r"Type a message|│ ❯",
    "codex": r"Ask Codex to do anything|› ",
    "cursor": r"Plan, search, build anything|Add a follow-up",
    "antigravity": r"\? for shortcuts",
    "opencode": r"Ask anything|ctrl\+p commands",
}

# Where each CLI keeps sessions (to list what a run left behind).
STORES = {
    "claude": lambda s: [os.path.join(s["config_dir"], "projects")],
    "grok": lambda s: [s["config_dir"]],
    "codex": lambda s: [os.path.expanduser("~/.codex/sessions")],
    "cursor": lambda s: [os.path.expanduser("~/.cursor/chats"), os.path.expanduser("~/.cursor/projects")],
    "antigravity": lambda s: [os.path.expanduser("~/.gemini/antigravity-cli")],
    "opencode": lambda s: [os.path.expanduser("~/.local/share/opencode")],
}

# Receipts that show the prompt reached the agent: Claude's come from its
# transcript; Grok's are "delivered or not" (unconfirmed: typed in and
# taken, no transcript to check), see docs/CLI_HARNESSES.md.
TAKEN = {"claude": {"received", "started", "done"},
         "grok": {"unconfirmed", "received", "started", "done"}}

UUID = re.compile(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|ses_[A-Za-z0-9]+")


def digest(path):
    try:
        with open(path, "rb") as f:
            return hashlib.sha256(f.read()).hexdigest()
    except OSError:
        return None


CURSOR_CFG = os.path.expanduser("~/.cursor/cli-config.json")
CURSOR_KEYS = ["permissions", "approvalMode", "sandbox", "model", "selectedModel", "editor",
               "display", "notifications", "attribution", "network", "maxMode"]


def cursor_settings():
    try:
        with open(CURSOR_CFG) as f:
            d = json.load(f)
    except (OSError, ValueError):
        return None
    return hashlib.sha256(json.dumps({k: d.get(k) for k in CURSOR_KEYS}, sort_keys=True).encode()).hexdigest()


def protected_files(slot):
    files = [os.path.realpath(os.path.expanduser(p)) for p in sandbox.PROTECTED]
    if slot.get("config_dir") and slot["harness"] == "claude":
        files.append(os.path.join(slot["config_dir"], "settings.json"))
    if slot.get("config_dir") and slot["harness"] in ("codex", "grok"):
        files.append(os.path.join(slot["config_dir"], "config.toml"))
    return files


def new_files(dirs, since, needle):
    """Files under `dirs` changed after `since` whose path or (small)
    contents mention `needle`; no contents are kept or printed."""
    hits = []
    for d in dirs:
        if not os.path.isdir(d):
            continue
        for root, _dirs, files in os.walk(d):
            for f in files:
                p = os.path.join(root, f)
                try:
                    st = os.stat(p)
                except OSError:
                    continue
                if st.st_mtime < since:
                    continue
                # Claude's project folder names: every non alphanumeric is "-".
                hit = needle in p or re.sub(r"[^A-Za-z0-9]", "-", needle) in p
                if not hit and st.st_size < 4_000_000:
                    try:
                        with open(p, "rb") as fh:
                            hit = needle.encode() in fh.read()
                    except OSError:
                        pass
                if hit:
                    hits.append(p)
    return hits


def sandbox_denials(since, root):
    """Paths the sandbox refused writes to since `since` (macOS unified log)."""
    mins = max(1, int((time.time() - since) / 60) + 1)
    try:
        r = subprocess.run(
            ["log", "show", "--last", f"{mins}m", "--style", "compact", "--predicate",
             'sender == "Sandbox" AND eventMessage CONTAINS "deny"'],
            capture_output=True, text=True, timeout=90)
    except (OSError, subprocess.TimeoutExpired):
        return []
    ours = {"godterm", "claude", "grok", "codex", "cursor-agent", "agent", "agy", "opencode",
            "node", "bun", "sh", "zsh", "bash", "git", "rg", "security"}
    out = set()
    for l in r.stdout.splitlines():
        # "Sandbox: claude(123) deny(1) file-write-create /path"
        m = re.search(r"Sandbox: (\S+?)\(\d+\) deny\(\d+\) (file-write\S*) (/.*)$", l)
        if m and m.group(1) in ours and not m.group(3).startswith(root):
            out.add(f"{m.group(1)}: {m.group(2)} {m.group(3).strip()}")
    return sorted(out)


def run(rep, tm, bin_path, keys, roots, out):
    cand = sandbox.candidates()
    # Default: every Claude account, the demo8 Grok login, and the others.
    default = [k for k, s in cand.items() if s["harness"] == "claude"]
    default += [k for k in ("demo8",) if k in cand]
    default += [k for k in ("codex", "cursor", "agy", "opencode") if k in cand]
    keys = keys or default
    summary = {"model_prompts": 0, "slots": {}, "leftover_sessions": {}}
    for key in keys:
        if key not in cand:
            rep.step(f"{key}: available", lambda: ("skip", "not installed or not logged in here"))
            continue
        slot = cand[key]
        one(rep, tm, bin_path, key, slot, roots, summary)
    with open(os.path.join(out, "accounts.json"), "w") as f:
        json.dump(summary, f, indent=2)
    rep.note(f"model prompts sent: {summary['model_prompts']} (one per harness at most)")
    for k, ids in summary["leftover_sessions"].items():
        rep.note(f"{k}: session files left by this run: {', '.join(ids) if ids else 'none found'}")


def one(rep, tm, bin_path, key, slot, roots, summary):
    h = slot["harness"]
    sb = sandbox.prepare(keys=None, bin_path=bin_path, slots={key: slot})
    roots.append(sb["root"])
    roots.extend(sb["outside"])
    env = dict(sb["env"])
    env["GODTERM_CONTROL_RW"] = "1"
    proj = sb["project"][key]
    started = time.time()
    before = {p: digest(p) for p in protected_files(slot)}
    cursor_before = cursor_settings()
    via_control = h in ("claude", "grok")
    st = {"tab": None, "states": [], "receipts": [], "reply": None, "prompted": False}
    summary["slots"][key] = st
    rep.bind(tm, S)

    def call(tool, args=None):
        return mcp_call(sb["argv"], env, tool, args)

    def tab_state():
        s = call("get_state")
        s = s.get("result", s) if isinstance(s.get("result"), dict) else s
        tabs = s.get("tabs") or []
        return s, (tabs[0] if tabs else None)

    answered = []

    def first_run_question(t):
        """A start up question: the Codex update menu (Skip, not saved) or
        a folder trust prompt (answered through GodTerm's own approvals,
        answer_prompt yes). Returns False when it is something else."""
        screen = tm.screen(S)
        if len(answered) >= 3:
            return False
        if "Update available" in screen and "2. Skip" in screen:
            rep.capture(f"{key}-update-menu")
            tm.keys(S, "2")
            time.sleep(0.5)
            if "Update available" in tm.screen(S):
                tm.keys(S, "Enter")
            answered.append("update menu: Skip")
            return True
        if re.search(r"(?i)trust", screen):
            rep.capture(f"{key}-trust-prompt")
            r = call("answer_prompt", {"tab": t["id"], "choice": "yes"})
            r = r.get("result", r) if isinstance(r.get("result"), dict) else r
            answered.append(f"trust: answer_prompt yes -> {json.dumps(r)[:120]}")
            return True
        return False

    def launch():
        tm.start(S, sb["argv"], env, cwd=proj)
        tm.wait(S, "◆ GodTerm", 30)
        end = time.time() + 90
        while time.time() < end:
            _, t = tab_state()
            if t:
                st["tab"] = t["id"]
                if not st["states"] or st["states"][-1] != t["state"]:
                    st["states"].append(t["state"])
                if t["state"] == "ready":
                    # Stable: still ready a few seconds later, with the CLI's
                    # own prompt on screen (a start up menu can follow).
                    time.sleep(3)
                    _, t2 = tab_state()
                    if not t2 or t2["state"] != "ready" or not re.search(READY[h], tm.screen(S)):
                        continue
                    extra = f" after {'; '.join(answered)}" if answered else ""
                    return f"tab {t['id']} ready in {t['folder']}{extra}"
                if t["state"] == "waiting for approval" and first_run_question(t):
                    time.sleep(2)
                    continue
                if t["state"] == "waiting for approval":
                    rep.capture(f"{key}-start-prompt")
                    raise StepFailed(f"the CLI asks something at start: {t.get('waiting_for', '?')}")
                if t["state"] == "ended":
                    raise StepFailed("the CLI exited at start")
            time.sleep(1.5)
        raise StepFailed(f"not ready after 90s (states {st['states']})")

    def prompt():
        if via_control:
            r = call("send_prompt", {"tab": st["tab"], "text": PROMPT, "mode": "queue", "expect_reply": False})
            r = r.get("result", r) if isinstance(r.get("result"), dict) else r
            st["prompted"] = True
            summary["model_prompts"] += 1
            res = (r.get("results") or r.get("tabs") or [r])
            st["send_result"] = json.dumps(res)[:300]
            st["receipts"].append("send_prompt")
            if r.get("ok") is False:
                raise StepFailed(f"send_prompt: {r.get('error') or str(r)[:200]}")
            return f"send_prompt: {json.dumps(res)[:160]}"
        tm.text(S, PROMPT)
        tm.wait(S, PROMPT, 10)
        # Codex reads text and Enter in one write as a paste: Enter apart.
        time.sleep(0.5)
        tm.keys(S, "Enter")
        st["prompted"] = True
        summary["model_prompts"] += 1
        return "typed in the pane"

    def reply():
        end = time.time() + 150
        saw_working = False
        # "⏺ ok", "ok.", or Grok's "ok      9:12 PM".
        rx = re.compile(r"(?mi)^[\s│|⏺●•>*·⎿]*\**ok\**\.?(\s+\d{1,2}:\d{2}\s*[AP]M)?\s*[│|]*\s*$")
        while time.time() < end:
            s, t = tab_state()
            if t:
                if t["state"] != (st["states"][-1] if st["states"] else None):
                    st["states"].append(t["state"])
                saw_working |= t["state"] == "working"
            for d in s.get("deliveries") or []:
                r = d.get("state") or d.get("status")
                if r and (not st["receipts"] or st["receipts"][-1] != r):
                    st["receipts"].append(r)
            screen = tm.screen(S)
            body = screen.split(PROMPT, 1)[-1] if PROMPT in screen else ""
            if rx.search(body) and t and t["state"] == "ready" and not st["reply"]:
                st["reply"] = "ok"
                st["reply_secs"] = round(time.time() - (end - 150), 1)
                if not via_control:
                    break
                # GodTerm learns a fresh tab's session id from its transcript
                # every 10 s; the receipt follows that.
                end = min(end, time.time() + 30)
            if st["reply"] and any(r in TAKEN.get(h, ()) for r in st["receipts"]):
                break
            if t and t["state"] == "waiting for approval":
                raise StepFailed(f"asks for approval: {t.get('waiting_for', '?')}")
            if h == "opencode" and re.search(r"Error: ECONNREFUSED|Connect provider|No provider", body) \
                    and t and t["state"] == "ready":
                return ("skip", "OpenCode has no reachable model provider here (its configured one refused the connection)")
            time.sleep(1.5)
        if st["reply"]:
            return f"ok after {st['reply_secs']}s; states {' -> '.join(st['states'])}; receipts {st['receipts'][1:] if via_control else 'n/a'}"
        raise StepFailed(f"no 'ok' reply in 150s (states {st['states']}, receipts {st['receipts'][-3:]})")

    def read_reply():
        r = call("read_tab", {"tab": st["tab"], "what": "reply"})
        r = r.get("result", r) if isinstance(r.get("result"), dict) else r
        text = (r.get("reply") or r.get("text") or "").strip()
        if not text:
            return ("skip", f"read_tab gave no reply text ({str(r)[:120]})")
        if not re.search(r"(?i)\bok\b", text):
            raise StepFailed(f"read_tab reply is {text[:80]!r}")
        return f"read_tab: {text[:40]!r}"

    def states_ok():
        if "working" not in st["states"] and not via_control:
            # A fast reply can finish between two looks; the screen showed it.
            return ("skip", f"never saw 'working' between polls: {st['states']}")
        if st["states"][-1] != "ready":
            raise StepFailed(f"ends in {st['states'][-1]}")
        if via_control:
            done = [r for r in st["receipts"] if r in TAKEN[h]]
            if not done:
                raise StepFailed(f"no receipt past 'sent' 30 s after the reply: {st['receipts']}")
        return f"states {st['states']}, receipts {st['receipts'][-4:]}"

    def close_and_quit():
        tm.cmd(S, "w")
        time.sleep(1.5)
        if re.search(r"Close (this )?tab|close it\?|Stop and close", tm.screen(S)):
            tm.keys(S, "Enter")
        end = time.time() + 15
        while time.time() < end:
            s, t = tab_state()
            if not t or t["id"] != st["tab"]:
                break
            time.sleep(1)
        else:
            raise StepFailed("tab still open after Ctrl-a w")
        tm.cmd(S, "Q")
        end = time.time() + 20
        while time.time() < end:
            code = tm.exit_status(S)
            if code is not None:
                if code != 0:
                    raise StepFailed(f"exit status {code}")
                return "tab closed, exit 0"
            time.sleep(0.5)
        tm.kill(S)
        raise StepFailed("GodTerm still running 20s after Ctrl-a Q")

    def unchanged():
        changed = [p for p, d in before.items() if digest(p) != d]
        if cursor_settings() != cursor_before:
            changed.append(CURSOR_CFG + " (settings keys)")
        tried = [d for d in sandbox_denials(started, sb["root"]) if any(d.endswith(" " + p) for p in before)]
        if tried:
            rep.note(f"{key}: the sandbox refused writes to settings files: {tried}")
        if changed:
            # Writes there are denied in the sandbox: the owner's own
            # sessions (outside) wrote them during the run.
            return ("skip", f"changed during the run by a process outside the sandbox: {changed}")
        return f"{len(before)} settings files unchanged" + (f"; {len(tried)} write(s) refused" if tried else "")

    def no_panics():
        log = os.path.join(sb["home"], "godterm.log")
        p = panics_in(log)
        if p:
            raise StepFailed(f"panic: {p[0][:200]}")

    ok = rep.step(f"{key} ({h}): launch in the sandbox, tab ready", launch)
    if ok and NO_PROMPT:
        rep.capture(f"{key}-ready")
        ok = False
    if ok:
        ok = rep.step(f"{key}: one prompt", prompt)
    if ok:
        ok = rep.step(f"{key}: reply 'ok'", reply)
        rep.capture(f"{key}-after-reply")
    if ok and h == "claude":
        rep.step(f"{key}: read_tab reply", read_reply)
    if ok:
        rep.step(f"{key}: activity and receipt states", states_ok)
    rep.step(f"{key}: close the tab and quit", close_and_quit)
    rep.step(f"{key}: settings files unchanged", unchanged)
    rep.step(f"{key}: no panics", no_panics)
    left = new_files(STORES[h](slot), started - 1, proj)
    ids = sorted({m.group(0) for p in left for m in [UUID.search(os.path.basename(p)) or UUID.search(p)] if m})
    summary["leftover_sessions"][key] = ids or [os.path.relpath(p, os.path.expanduser("~")) for p in left][:5]
    den = sandbox_denials(started, sb["root"])
    if den:
        rep.note(f"{key}: sandbox refused {len(den)} write(s): " + "; ".join(den[:8]))
