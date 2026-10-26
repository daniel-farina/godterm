#!/usr/bin/env python3
"""Host sandbox for GodTerm (layer b): the real binary, the real logged in
agent CLIs, everything else locked down.

- GodTerm's own state goes to a fresh home (GODTERM_HOME) under a sandbox
  root, never ~/.godterm.
- Its account slots point IN PLACE at the real account dirs (Claude:
  ~/.godterm/accounts/accountN, Grok: ~/.godterm/accounts/<name>, Codex:
  ~/.codex). Nothing is copied: copied OAuth refresh tokens can rotate and
  break the real logins. Cursor, Antigravity and OpenCode use their
  native stores anyway.
- macOS sandbox-exec profile: writes are denied everywhere except the
  sandbox root, the account dirs in use, the agent CLIs' own state dirs,
  /dev and the temp dirs; the CLIs' settings files (and the real
  ~/.godterm/config.toml) are write protected even inside those; reads of
  ~/.ssh, /Volumes/Private, cloud and signing secrets and the rest of
  ~/.godterm are denied. Network is allowed.

Usage (interactive, in your own terminal):

    scripts/sandbox/host/sandbox.py run --accounts account1,codex -- [godterm args]
    scripts/sandbox/host/sandbox.py list          # what would be used
    scripts/sandbox/host/sandbox.py profile ...   # print the profile

The e2e suite imports `prepare()`.
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import tomllib

H = os.path.expanduser("~")
REAL_HOME = os.path.join(H, ".godterm")
REPO = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", ".."))

# Never readable from inside the sandbox.
SECRET_DIRS = [
    "~/.ssh", "/Volumes/Private", "~/.gnupg", "~/.aws", "~/.azure", "~/.kube",
    "~/.docker", "~/.config/gh", "~/.config/godterm", "~/.config/godterm-release",
    "~/.appstoreconnect", "~/.password-store", "~/.netrc", "~/.npmrc", "~/.pypirc",
    "~/Library/Mobile Documents", "~/Documents", "~/Desktop", "~/Downloads",
]

# State dirs the agent CLIs write to (sessions, logs, caches, token
# refreshes). Writable; their settings files below are not.
CLI_STATE = {
    "claude": ["~/.local/share/claude", "~/.local/state/claude", "~/.cache/claude",
               "~/.cache/claude-cli-nodejs", "~/Library/Caches/claude-cli-nodejs"],
    "grok": ["~/.cache/grok", "~/.local/share/grok", "~/.local/state/grok"],
    "codex": ["~/.codex"],
    "cursor": ["~/.cursor", "~/.config/cursor", "~/Library/Application Support/Cursor",
               "~/Library/Caches/cursor-agent", "~/.local/share/cursor-agent"],
    "antigravity": ["~/.gemini", "~/Library/Application Support/Antigravity",
                    "~/.cache/antigravity"],
    "opencode": ["~/.local/share/opencode", "~/.local/state/opencode", "~/.cache/opencode",
                 "~/.config/opencode"],
}

# Settings files that must not change, even where their dir is writable.
# (Cursor's ~/.cursor/cli-config.json is not here: it mixes settings with
# its auth info and caches, which it rewrites; the suite compares its
# settings keys instead.)
PROTECTED = [
    "~/.godterm/config.toml",
    "~/.codex/config.toml",
    "~/.cursor/mcp.json",
    "~/.gemini/settings.json",
    "~/.gemini/antigravity-cli/settings.json",
    "~/.gemini/antigravity-cli/mcp_config.json",
    "~/.config/opencode/opencode.json",
    "~/.config/opencode/config.json",
    "~/.claude/settings.json",
    "~/.claude.json",
]

BINS = {
    "claude": ["claude"],
    "grok": ["grok"],
    "codex": ["codex"],
    "cursor": ["cursor-agent", "agent"],
    "antigravity": ["agy"],
    "opencode": ["opencode"],
}


def ex(p):
    return os.path.expanduser(p)


def real_accounts():
    """(name, harness, config_dir) of the real config's accounts, read only."""
    path = os.path.join(REAL_HOME, "config.toml")
    out = []
    try:
        with open(path, "rb") as f:
            cfg = tomllib.load(f)
    except (OSError, tomllib.TOMLDecodeError):
        return out
    for a in cfg.get("account", []):
        name = a.get("name")
        if not name:
            continue
        h = a.get("harness", "claude")
        d = a.get("config_dir") or os.path.join(REAL_HOME, "accounts", name)
        out.append((name, h, ex(d)))
    return out


def which(h):
    for b in BINS[h]:
        p = shutil.which(b)
        if p:
            return p
    return None


def candidates():
    """Every slot the sandbox could use: {key: {harness, config_dir, why}}.
    Claude and Grok slots come from the real config (dirs must exist and
    look logged in); the other CLIs when installed."""
    c = {}
    for name, h, d in real_accounts():
        if h not in ("claude", "grok") or not os.path.isdir(d) or not which(h):
            continue
        if h == "claude" and not os.path.exists(os.path.join(d, ".claude.json")):
            continue
        c[name] = {"harness": h, "config_dir": d, "label": name}
    if which("codex") and os.path.exists(ex("~/.codex/auth.json")):
        c["codex"] = {"harness": "codex", "config_dir": ex("~/.codex"), "label": "Codex"}
    if which("cursor") and os.path.isdir(ex("~/.cursor")):
        c["cursor"] = {"harness": "cursor", "config_dir": None, "label": "Cursor"}
    if which("antigravity") and os.path.isdir(ex("~/.gemini/antigravity-cli")):
        c["agy"] = {"harness": "antigravity", "config_dir": None, "label": "Antigravity"}
    if which("opencode"):
        c["opencode"] = {"harness": "opencode", "config_dir": None, "label": "OpenCode"}
    return c


def toml_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def write_config(home, slots, projects, extra=""):
    """A GodTerm config for the sandbox: the chosen slots, each starting in
    its own project folder; nothing that bypasses permissions, no sound,
    no update checks, no notifications."""
    lines = [
        "notifications = false",
        "setup_dont_show = true",
        'splash = "never"',
        "remember_window = false",
        'permission_mode = "default"',
        f"new_tab_base = {toml_str(projects)}",
        "",
    ]
    for key, s in slots.items():
        proj = s.get("project") or os.path.join(projects, key)
        os.makedirs(proj, exist_ok=True)
        lines += [
            "[[account]]",
            f"name = {toml_str(key)}",
            f"label = {toml_str(s['label'])}",
            f'harness = "{s["harness"]}"',
            f"cwd = {toml_str(proj)}",
            'permission_mode = "default"',
        ]
        if s.get("config_dir"):
            lines.append(f"config_dir = {toml_str(s['config_dir'])}")
        args = list(s.get("args", []))
        if s["harness"] == "codex":
            # Trust the test folder for this launch only (-c is not saved).
            args += ["-c", f'projects."{proj}".trust_level="trusted"']
        if s["harness"] == "cursor":
            # Trust the test folder without the prompt.
            args += ["--trust"]
        if args:
            lines.append("args = [" + ", ".join(toml_str(a) for a in args) + "]")
        lines.append("")
    lines += [
        "[voice]", "tts = false", "chime = false", 'wake_model = "off"', "",
        "[tabs]", "jobs_tools = false", "",
        # No assistant turns (they would spend model quota).
        "[assistant]", 'mode = "off"', "prewarm = false", "",
        "[update]", "enabled = false", "auto_download = false", "",
        "[supervisor]", "enabled = false", "hang_restart = false", "",
    ]
    with open(os.path.join(home, "config.toml"), "w") as f:
        f.write("\n".join(lines) + extra)
    # The first launch tour has its own e2e step (demo).
    with open(os.path.join(home, "tour_done"), "w") as f:
        f.write("1")


def sb_quote(p):
    return '"' + p.replace("\\", "\\\\").replace('"', '\\"') + '"'


def profile(root, slots, extra_write=()):
    """The sandbox-exec profile (SBPL; the last matching rule wins)."""
    write = [root, "/dev", "/private/var/folders", ex(os.environ.get("TMPDIR", "/private/var/folders"))]
    write += list(extra_write)
    harnesses = {s["harness"] for s in slots.values()}
    for h in harnesses:
        write += [ex(p) for p in CLI_STATE.get(h, [])]
    for s in slots.values():
        if s.get("config_dir"):
            write.append(s["config_dir"])
        if s.get("project"):
            write.append(s["project"])
    # Reads: the account dirs in use stay readable inside the denied
    # ~/.godterm; nothing else of the real GodTerm home.
    read_ok = [s["config_dir"] for s in slots.values() if s.get("config_dir", "") and
               s["config_dir"].startswith(REAL_HOME + os.sep)]
    # The repo and the binary must stay readable even if they live under a
    # denied folder (e.g. ~/Documents).
    read_ok.append(REPO)
    rules = [
        "(version 1)",
        "(allow default)",
        ";; Writes: nowhere but these.",
        '(deny file-write* (subpath "/"))',
        "(allow file-write*",
        *[f"  (subpath {sb_quote(os.path.realpath(p))})" for p in sorted(set(write))],
        ")",
        ";; Settings files stay as they are.",
        "(deny file-write*",
        *[f"  (literal {sb_quote(os.path.realpath(ex(p)))})" for p in PROTECTED],
        *[f"  (literal {sb_quote(os.path.join(s['config_dir'], 'settings.json'))})"
          for s in slots.values() if s.get("config_dir") and s["harness"] == "claude"],
        *[f"  (literal {sb_quote(os.path.join(s['config_dir'], 'config.toml'))})"
          for s in slots.values() if s.get("config_dir") and s["harness"] in ("codex", "grok")],
        ")",
        ";; Secrets and the real GodTerm home: not even readable.",
        "(deny file-read*",
        *[f"  (subpath {sb_quote(ex(p))})" for p in SECRET_DIRS],
        f"  (subpath {sb_quote(REAL_HOME)})",
        ")",
        "(allow file-read*",
        *[f"  (subpath {sb_quote(p)})" for p in read_ok],
        ")",
        ";; Real GodTerm's own files inside ~/.godterm/accounts are not ours.",
        '(allow network*)',
    ]
    return "\n".join(rules) + "\n"


def prepare(root=None, keys=None, bin_path=None, extra_cfg="", slots=None):
    """Create a sandbox root with home/, projects/, tmp/ and the profile.
    Returns a dict: root, home, projects, profile, argv (sandbox-exec ...
    godterm), env (for the binary), slots."""
    root = os.path.realpath(root or tempfile.mkdtemp(prefix="godterm-sandbox-", dir="/private/tmp"))
    home = os.path.join(root, "home")
    projects = os.path.join(root, "projects")
    tmp = os.path.join(root, "tmp")
    for d in (home, projects, tmp):
        os.makedirs(d, exist_ok=True)
    os.chmod(home, 0o700)
    if slots is None:
        cand = candidates()
        keys = keys if keys is not None else list(cand)
        missing = [k for k in keys if k not in cand]
        if missing:
            raise SystemExit(f"sandbox: not available here: {', '.join(missing)} (have: {', '.join(cand)})")
        slots = {k: cand[k] for k in keys}
    slots = {k: dict(s) for k, s in slots.items()}
    outside = []
    for k, s in slots.items():
        if s["harness"] == "codex" and not s.get("project"):
            # Codex asks to trust any folder not under a trusted one and
            # saves the answer in ~/.codex/config.toml (write protected
            # here), ignoring a -c override for that prompt. A folder in
            # ~/.cache sits under the home folder, which this user trusts;
            # it is deleted with the sandbox root.
            s["project"] = os.path.join(H, ".cache", "godterm-e2e", os.path.basename(root), k)
            outside.append(os.path.dirname(s["project"]))
    write_config(home, slots, projects, extra_cfg)
    prof = os.path.join(root, "godterm.sb")
    with open(prof, "w") as f:
        f.write(profile(root, slots))
    bin_path = os.path.realpath(bin_path or os.path.join(REPO, "target", "release", "godterm"))
    env = {
        "HOME": H,
        "USER": os.environ.get("USER", ""),
        "LOGNAME": os.environ.get("USER", ""),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "SHELL": os.environ.get("SHELL", "/bin/zsh"),
        "LANG": os.environ.get("LANG", "en_US.UTF-8"),
        "TERM": "xterm-256color",
        "TMPDIR": tmp + "/",
        "GODTERM_HOME": home,
        # The main installs: an empty dir (GodTerm never adopts ~/.claude).
        "GODTERM_MAIN_DIR": os.path.join(root, "no-main-claude"),
        "GODTERM_MAIN_GROK_DIR": os.path.join(root, "no-main-grok"),
        "GODTERM_NO_AUDIO": "1",
        "GODTERM_NO_MIC": "1",
        "GODTERM_NO_OPEN": "1",
        "GODTERM_NO_SPLASH": "1",
    }
    # Usage meters: a fixture, so the sandbox makes no usage API calls.
    fx = os.path.join(REPO, "tests", "fixtures", "usage.json")
    if os.path.exists(fx):
        env["GODTERM_USAGE_FIXTURE"] = fx
    return {
        "root": root, "home": home, "projects": projects, "profile": prof,
        "argv": ["/usr/bin/sandbox-exec", "-f", prof, bin_path],
        "bin": bin_path, "env": env, "slots": slots,
        # Project folders outside the root (delete them with it).
        "outside": outside,
        "project": {k: s.get("project") or os.path.join(projects, k) for k, s in slots.items()},
    }


def demo_profile(root):
    """The demo world needs nothing of the user's: writes only in the
    sandbox root, and no reads of any real agent or GodTerm data."""
    no_read = SECRET_DIRS + ["~/.godterm", "~/.claude", "~/.claude.json", "~/.grok",
                             "~/.codex", "~/.cursor", "~/.gemini", "~/.local/share/opencode",
                             "~/.config/opencode", "~/.godterm-demo"]
    rules = [
        "(version 1)",
        "(allow default)",
        '(deny file-write* (subpath "/"))',
        "(allow file-write*",
        f"  (subpath {sb_quote(root)})",
        '  (subpath "/dev")',
        '  (subpath "/private/var/folders")',
        ")",
        "(deny file-read*",
        *[f"  (subpath {sb_quote(ex(p))})" for p in no_read],
        ")",
        "(allow file-read*",
        f"  (subpath {sb_quote(REPO)})",
        ")",
    ]
    return "\n".join(rules) + "\n"


def prepare_demo(root=None, bin_path=None, sandboxed=True):
    """A sandbox root for `godterm --demo` (GODTERM_DEMO_HOME inside it).
    `sandboxed=False` (Linux CI, no sandbox-exec): the same env, no profile."""
    root = os.path.realpath(root or tempfile.mkdtemp(prefix="godterm-demo-sandbox-", dir="/private/tmp" if os.path.isdir("/private/tmp") else None))
    tmp = os.path.join(root, "tmp")
    os.makedirs(tmp, exist_ok=True)
    bin_path = os.path.realpath(bin_path or os.path.join(REPO, "target", "release", "godterm"))
    env = {
        "HOME": H,
        "USER": os.environ.get("USER", ""),
        "LOGNAME": os.environ.get("USER", ""),
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "SHELL": os.environ.get("SHELL", "/bin/sh"),
        "LANG": os.environ.get("LANG", "en_US.UTF-8"),
        "TERM": "xterm-256color",
        "TMPDIR": tmp + "/",
        "GODTERM_DEMO_HOME": os.path.join(root, "demo"),
        "GODTERM_NO_AUDIO": "1",
        "GODTERM_NO_MIC": "1",
        "GODTERM_NO_OPEN": "1",
        "GODTERM_NO_SPLASH": "1",
    }
    argv = [bin_path]
    prof = None
    if sandboxed:
        prof = os.path.join(root, "demo.sb")
        with open(prof, "w") as f:
            f.write(demo_profile(root))
        argv = ["/usr/bin/sandbox-exec", "-f", prof, bin_path]
    return {"root": root, "demo_home": env["GODTERM_DEMO_HOME"], "profile": prof,
            "argv": argv, "bin": bin_path, "env": env,
            "log": os.path.join(env["GODTERM_DEMO_HOME"], "app", "godterm.log"),
            "app_home": os.path.join(env["GODTERM_DEMO_HOME"], "app")}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("cmd", choices=["run", "list", "profile"])
    ap.add_argument("--accounts", help="comma separated slot keys (default: all available)")
    ap.add_argument("--root", help="sandbox root (default: a new /private/tmp/godterm-sandbox-*)")
    ap.add_argument("--bin", help="godterm binary (default: target/release/godterm)")
    argv = sys.argv[1:]
    rest = argv[argv.index("--") + 1:] if "--" in argv else []
    a = ap.parse_args(argv[:argv.index("--")] if "--" in argv else argv)
    if sys.platform != "darwin":
        raise SystemExit("sandbox: macOS only (sandbox-exec)")
    if a.cmd == "list":
        for k, s in candidates().items():
            print(f"{k:12} {s['harness']:12} {s['config_dir'] or '(native store)'}")
        return
    keys = a.accounts.split(",") if a.accounts else None
    sb = prepare(a.root, keys, a.bin)
    if a.cmd == "profile":
        print(open(sb["profile"]).read())
        return
    print(f"sandbox root {sb['root']} (delete it when done)", file=sys.stderr)
    os.execve(sb["argv"][0], sb["argv"] + rest, sb["env"])


if __name__ == "__main__":
    main()
