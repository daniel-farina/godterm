"""Demo world scenarios: `godterm --demo` (made up accounts, the demo
agent instead of claude and grok, no network, no logins). Safe for CI."""

import os
import re
import time

from lib import StepFailed, panics_in, mcp_call

S = "demo"


def need(screen, *needles):
    for n in needles:
        if n not in screen:
            raise StepFailed(f"{n!r} not on screen")


def tab_count(screen, pane_label):
    """`tabs N` in the header row under a pane title such as "1 Work"."""
    lines = screen.splitlines()
    for i, l in enumerate(lines):
        if pane_label in l and i + 1 < len(lines):
            col = l.index(pane_label)
            seg = lines[i + 1][max(0, col - 6):col + 40]
            m = re.search(r"tabs (\d+)", seg)
            if m:
                return int(m.group(1))
    return None


def run(rep, tm, sb, harnesses=False):
    """The full demo scenario list. `sb` comes from sandbox.prepare_demo."""
    args = ["--demo", "--demo-no-external", "--demo-manual"]
    if harnesses:
        args.append("--demo-harnesses")
    env = dict(sb["env"])
    rep.bind(tm, S)

    def launch():
        tm.start(S, sb["argv"] + args, env, cwd=sb["root"])
        tm.wait(S, "◆ GodTerm", 30)
        if harnesses:
            tm.wait(S, "CLI-managed", 30)
        else:
            tm.wait(S, "re:Restored \\d+ tabs", 30)
        return None

    def grid():
        s = tm.screen(S)
        if harnesses:
            # One account per agent CLI.
            need(s, "1 Work", "2 Grok Lab", "3 Backend", "4 Frontend", "5 Mobile", "6 Local")
            if s.count("CLI-managed") < 4:
                raise StepFailed("fewer than four CLI-managed accounts in the footer")
        else:
            need(s, "1 Work", "2 Personal", "3 Research", "4 Client", "5 Grok Lab", "6 Grok Ops")
        return f"{len(re.findall(r'[╭┌]◂ \d+ ', s))} panes"

    def help_card():
        tm.cmd(S, "?")
        tm.wait(S, "godterm help", 5)
        s = tm.screen(S)
        need(s, "Ctrl-a t", "Take the tour")
        tm.keys(S, "Escape")
        tm.wait(S, "godterm help", 5, gone=True)

    def palette():
        tm.cmd(S, ":")
        tm.wait(S, "command palette", 5)
        tm.text(S, "zoom")
        tm.wait(S, "› zoom pane", 5)
        tm.keys(S, "Escape")
        tm.wait(S, "command palette", 5, gone=True)

    def settings():
        tm.cmd(S, ",")
        i, s = tm.wait_any(S, [r"[Ss]ettings.*─ x ─", r"Voice and Audio", r"Layout"], 6)
        cap = rep.capture("settings")
        tm.keys(S, "Escape")
        time.sleep(0.6)
        if "Voice and Audio" in tm.screen(S):
            tm.keys(S, "Escape")
        return f"capture {os.path.basename(cap)}"

    def geometry(s):
        """Where the pane titles are: (row, col) of each "◂ N"."""
        out = []
        for r, l in enumerate(s.splitlines()):
            for m in re.finditer(r"◂ \d+ ", l):
                out.append((r, m.start()))
        return tuple(out)

    def layouts():
        g0 = geometry(tm.screen(S))
        seen = {g0}
        for _ in range(5):
            tm.cmd(S, "L", pause=1.0)
            seen.add(geometry(tm.screen(S)))
        g1 = geometry(tm.screen(S))
        if len(seen) < 3:
            raise StepFailed(f"Ctrl-a L gave only {len(seen)} distinct layouts")
        if g1 != g0:
            raise StepFailed("five presses of Ctrl-a L did not come back to the first layout")
        return f"{len(seen)} distinct pane arrangements, back to the first"

    def open_close_tab():
        label = "1 Work"
        tm.cmd(S, "1")
        before = tab_count(tm.screen(S), label)
        tm.cmd(S, "t")
        tm.wait(S, "re:New tab for", 6)
        tm.keys(S, "Enter")
        tm.wait(S, "re:New tab for", 6, gone=True)
        end = time.time() + 10
        after = None
        while time.time() < end:
            after = tab_count(tm.screen(S), label)
            if before is not None and after == before + 1:
                break
            time.sleep(0.4)
        if before is None or after != before + 1:
            raise StepFailed(f"tab count {before} -> {after} after New tab")
        rep.capture("new-tab-open")
        tm.cmd(S, "w")
        time.sleep(1)
        s = tm.screen(S)
        if re.search(r"Close (this )?tab|close it\?", s):
            tm.keys(S, "Enter")
        end = time.time() + 10
        while time.time() < end:
            now = tab_count(tm.screen(S), label)
            if now == before:
                return f"tabs {before} -> {after} -> {now}"
            time.sleep(0.4)
        raise StepFailed(f"tab count {after} -> {tab_count(tm.screen(S), label)} after close (want {before})")

    def approvals():
        # The demo agents ask for approval on their own after a few seconds.
        tm.wait(S, "re:\\d+ need approval \\(C-a y\\)", 60)
        tm.cmd(S, "y")
        s = tm.wait(S, "Approve   Always   Deny", 6)
        n = len(re.findall("Approve   Always   Deny", s))
        rep.capture("approvals-queue")
        tm.keys(S, "Escape")
        tm.wait(S, "Approve   Always   Deny", 6, gone=True)
        return f"{n} waiting in the queue"

    def approve_one():
        # Approve the selected row in the queue (y; Enter would jump to the
        # tab) and see that row leave the queue.
        tm.cmd(S, "y")
        s = tm.wait(S, "re:approvals: \\d+ waiting", 6)
        m = re.search(r"approvals: (\d+) waiting", s)
        n0 = int(m.group(1))
        first = re.search(r"› (\S+ tab \d+ \([^)]*\))", s)
        if n0 == 0 or not first:
            tm.keys(S, "Escape")
            return ("skip", "nothing waiting right now")
        row = first.group(1)
        tm.keys(S, "y")
        try:
            end = time.time() + 8
            while time.time() < end:
                s = tm.screen(S)
                if row not in s or "approvals:" not in s:
                    return f"approved {row}; {n0} waiting before"
                time.sleep(0.3)
            raise StepFailed(f"{row} still waiting after y")
        finally:
            if "approvals:" in tm.screen(S):
                tm.keys(S, "Escape")

    def browser():
        tm.cmd(S, "1")
        tm.cmd(S, "i")
        try:
            s = tm.wait(S, "re:╭ browser for ", 6)
            cap = rep.capture("browser-dialog")
            if harnesses:
                # Antigravity has no per tab tools; the others list profiles.
                return f"capture {os.path.basename(cap)}"
            need(s, "Chrome profile for this tab", "Work (alex@example.com)", "QA")
            return f"capture {os.path.basename(cap)}"
        finally:
            tm.keys(S, "Escape")
            tm.wait(S, "re:╭ browser for ", 5, gone=True)

    def update_mode():
        # The typed path: the palette entry, on then off.
        tm.cmd(S, ":")
        tm.wait(S, "command palette", 5)
        tm.text(S, "update mode")
        tm.wait(S, "voice: update mode on / off", 5)
        tm.keys(S, "Enter")
        # The "Update mode on." flash is soon replaced by demo events: the
        # control API is the check.
        on = None
        end = time.time() + 8
        while time.time() < end and on is not True:
            on = mode_now()
            time.sleep(0.5)
        cap = rep.capture("update-mode-on")
        tm.cmd(S, ":")
        tm.wait(S, "command palette", 5)
        tm.text(S, "update mode")
        tm.wait(S, "voice: update mode on / off", 5)
        tm.keys(S, "Enter")
        time.sleep(1)
        off = mode_now()
        if on is not True or off is not False:
            raise StepFailed(f"update_mode after on: {on}, after off: {off}")
        return f"on, then off; the control API agrees (capture {os.path.basename(cap)})"

    def mode_now():
        # The flash is soon replaced by demo events: ask the app instead.
        r = mcp_call(sb["argv"], env_for_mcp(), "update_mode", {"now": True})
        return (r.get("result") or r).get("update_mode")

    def onboarding_replay():
        tm.cmd(S, ":")
        tm.wait(S, "command palette", 5)
        tm.text(S, "onboarding")
        tm.wait(S, "re:› .*onboarding", 5)
        tm.keys(S, "Enter")
        _, s = tm.wait_any(S, [r"Turn voice on", r"voice tour · step 1 of 5", r"Voice needs a few pieces"], 10)
        rep.capture("onboarding-replay")
        tm.keys(S, "Escape")
        time.sleep(1)
        s = tm.screen(S)
        if "voice tour" in s or "Turn voice on" in s:
            tm.text(S, "stop onboarding")
            tm.keys(S, "Enter")
        tm.wait(S, "re:Turn voice on|voice tour · step", 8, gone=True)

    def control_state():
        st = mcp_call(sb["argv"], env_for_mcp(), "get_state")
        tabs = st.get("tabs") or (st.get("result") or {}).get("tabs")
        if not tabs:
            raise StepFailed(f"get_state gave no tabs: {str(st)[:200]}")
        states = sorted({t.get("state", "?") for t in tabs})
        return f"{len(tabs)} tabs, states {states}"

    def env_for_mcp():
        e = dict(env)
        e["GODTERM_HOME"] = sb["app_home"]
        e["GODTERM_MAIN_DIR"] = os.path.join(sb["demo_home"], "main", "claude")
        e["GODTERM_MAIN_GROK_DIR"] = os.path.join(sb["demo_home"], "main", "grok")
        e["GODTERM_DEMO"] = "1"
        return e

    def quit_clean():
        for attempt in range(2):
            tm.cmd(S, "Q")
            end = time.time() + 12
            while time.time() < end:
                st = tm.exit_status(S)
                if st is not None:
                    if st != 0:
                        raise StepFailed(f"exit status {st}")
                    return "exit 0" + (" (after closing a leftover dialog)" if attempt else "")
                time.sleep(0.3)
            # A dialog left open by a failed step swallows the keys.
            rep.capture("quit-blocked")
            tm.keys(S, "Escape")
        tm.kill(S)
        raise StepFailed("still running after Ctrl-a Q twice")

    def no_panics():
        p = panics_in(sb["log"])
        if p:
            raise StepFailed(f"{len(p)} panic line(s): {p[0][:200]}")
        if not os.path.exists(sb["log"]):
            return ("skip", f"no log at {sb['log']}")
        return f"log {os.path.getsize(sb['log'])} bytes, no panic"

    pre = "harnesses: " if harnesses else ""
    if not rep.step(pre + "launch", launch):
        rep.step(pre + "quit cleanly", quit_clean)
        return
    rep.step(pre + "grid", grid)
    if harnesses:
        rep.step(pre + "open and close a tab", open_close_tab)
        rep.step(pre + "Browser dialog (harness tab)", browser)
        rep.step(pre + "control API get_state", control_state)
    else:
        rep.step("help", help_card)
        rep.step("command palette", palette)
        rep.step("settings", settings)
        rep.step("layouts", layouts)
        rep.step("open and close a tab", open_close_tab)
        rep.step("approvals queue", approvals)
        rep.step("approve from the queue", approve_one)
        rep.step("Browser dialog (demo profiles)", browser)
        rep.step("update mode (typed path)", update_mode)
        rep.step("onboarding replay", onboarding_replay)
        rep.step("control API get_state", control_state)
    rep.step(pre + "quit cleanly", quit_clean)
    rep.step(pre + "no panics in the log", no_panics)


def run_onboarding_start(rep, tm, sb):
    """`godterm --demo --onboarding`: the voice onboarding comes first."""
    env = dict(sb["env"])
    rep.bind(tm, S)

    def start():
        tm.start(S, sb["argv"] + ["--demo", "--demo-no-external", "--demo-manual", "--onboarding"], env, cwd=sb["root"])
        tm.wait(S, "Turn voice on", 30)
        rep.capture("onboarding-welcome")

    def skip():
        tm.keys(S, "Escape")
        tm.wait(S, "Turn voice on", 8, gone=True)
        tm.wait(S, "◆ GodTerm", 15)

    def quit_():
        tm.cmd(S, "Q")
        end = time.time() + 15
        while time.time() < end:
            st = tm.exit_status(S)
            if st is not None:
                return None if st == 0 else (_ for _ in ()).throw(StepFailed(f"exit {st}"))
            time.sleep(0.3)
        raise StepFailed("did not quit")

    if rep.step("onboarding at start: welcome", start):
        rep.step("onboarding at start: Esc skips to the grid", skip)
    rep.step("onboarding at start: quit", quit_)
