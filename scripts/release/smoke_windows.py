#!/usr/bin/env python3
"""Smoke test GodTerm on the Windows build machine, driven from a Mac.

Installs the setup.exe silently (per user), runs the installed godterm.exe in
a real console (Windows OpenSSH gives `ssh -tt` sessions a ConPTY, the same
pseudo console Windows Terminal uses), against a fake claude (a .cmd that
echoes), with a throwaway GODTERM_HOME. It checks that the TUI renders both
accounts' panes, opens a tab through the New Tab dialog, quits cleanly with
exit code 0, then uninstalls.

    venv/bin/python scripts/release/smoke_windows.py dist/GodTerm-0.2.0-windows-x64-setup.exe

Needs `pyte` (pip install pyte) to read the screen. Env: WIN_HOST, WIN_USER,
WIN_SSH_KEY as in windows-remote.sh.
"""
import base64
import re
import os
import pty
import select
import subprocess
import sys
import time

import pyte

def _release_env() -> dict:
    """KEY=value lines from ~/.config/godterm/release.env, if present."""
    p = os.path.expanduser(os.environ.get("RELEASE_ENV", "~/.config/godterm/release.env"))
    out = {}
    if os.path.isfile(p):
        for line in open(p):
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                out[k.strip()] = os.path.expandvars(v.strip().strip('"'))
    return out


_ENV = {**_release_env(), **os.environ}
HOST = _ENV.get("WIN_HOST") or sys.exit("set WIN_HOST (the Windows test machine)")
USER = _ENV.get("WIN_USER") or sys.exit("set WIN_USER")
KEY = os.path.expanduser(_ENV.get("WIN_SSH_KEY", "~/.ssh/id_ed25519"))
SSH = ["ssh", "-p", "22", "-F", "/dev/null", "-i", KEY, "-o", "BatchMode=yes",
       "-o", "ConnectTimeout=20", "-o", "StrictHostKeyChecking=accept-new", f"{USER}@{HOST}"]
SCP = ["scp", "-q", "-P", "22", "-F", "/dev/null", "-i", KEY, "-o", "BatchMode=yes"]
HOME = rf"C:\Users\{USER}\godterm-smoke"
COLS, ROWS = 140, 40


def ps(script: str, check: bool = True) -> str:
    b64 = base64.b64encode(script.encode("utf-16-le")).decode()
    r = subprocess.run(SSH + [f"powershell -NoProfile -NonInteractive -OutputFormat Text -EncodedCommand {b64}"],
                       capture_output=True, text=True)
    out = "\n".join(l for l in r.stdout.splitlines() if "CLIXML" not in l and "<Objs" not in l)
    if check and r.returncode != 0:
        sys.exit(f"remote PowerShell failed ({r.returncode}):\n{out}\n{r.stderr}")
    return out


def main() -> int:
    setup = sys.argv[1] if len(sys.argv) > 1 else None
    if not setup or not os.path.isfile(setup):
        sys.exit("usage: smoke_windows.py <GodTerm-...-setup.exe>")
    name = os.path.basename(setup)
    print(f"==> copy {name}")
    subprocess.run(SCP + [setup, f"{USER}@{HOST}:C:/Users/{USER}/{name}"], check=True)

    print("==> silent per user install")
    exe = ps(rf"""
$ProgressPreference = 'SilentlyContinue'
$p = Start-Process -Wait -PassThru "$env:USERPROFILE\{name}" -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART','/CURRENTUSER','/TASKS=addtopath'
"exit $($p.ExitCode)"
$exe = Join-Path $env:LOCALAPPDATA 'Programs\GodTerm\godterm.exe'
if (Test-Path $exe) {{ "EXE=$exe"; & $exe --version }} else {{ 'EXE=missing' }}
""")
    print(exe)
    exe_path = next((l[4:] for l in exe.splitlines() if l.startswith("EXE=")), "missing")
    if exe_path == "missing":
        sys.exit("installer did not put godterm.exe in %LOCALAPPDATA%\\Programs\\GodTerm")

    print("==> throwaway home with a fake claude")
    ps(rf"""
$h = '{HOME}'
Remove-Item -Recurse -Force $h -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path "$h\work", "$h\tabs", "$h\accounts\one", "$h\accounts\two" | Out-Null
Set-Content -Encoding ASCII "$h\fake-claude.cmd" "@echo off`r`necho fake claude in %CD% args:%*`r`necho ? for shortcuts`r`n:loop`r`nset /p line=`r`necho got:%line%`r`ngoto loop"
$creds = '{{"claudeAiOauth":{{"accessToken":"fake","refreshToken":"fake","expiresAt":4102444800000,"subscriptionType":"max"}}}}'
Set-Content -Encoding ASCII "$h\accounts\one\.credentials.json" $creds
Set-Content -Encoding ASCII "$h\accounts\two\.credentials.json" $creds
$cfg = @"
claude_bin = '$h\fake-claude.cmd'
notifications = false
setup_dont_show = true
new_tab_base = '$h\tabs'
[[account]]
name = "one"
label = "Alpha"
cwd = '$h\work'
[[account]]
name = "two"
label = "Bravo"
cwd = '$h\work'
[voice]
tts = false
chime = false
wake_model = "off"
"@
Set-Content -Encoding UTF8 "$h\config.toml" $cfg
Set-Content -Encoding ASCII "$h\tour_done" "1"
""")

    print("==> run the TUI in a console (ConPTY over ssh -tt)")
    # our own test runs must never show up in godterm.com's install counts
    launch = (f"$env:GODTERM_HOME='{HOME}'; $env:GODTERM_NO_TELEMETRY='1'; $env:DO_NOT_TRACK='1'; "
              f"$env:GODTERM_NO_AUDIO='1'; $env:GODTERM_NO_MIC='1'; "
              f"$env:GODTERM_NO_OPEN='1'; & '{exe_path}'; \"GODTERM_EXIT=$LASTEXITCODE\"")
    b64 = base64.b64encode(launch.encode("utf-16-le")).decode()
    pid, fd = pty.fork()
    if pid == 0:
        os.environ["TERM"] = "xterm-256color"
        os.execvp("ssh", SSH[:1] + ["-tt"] + SSH[1:] + [f"powershell -NoProfile -EncodedCommand {b64}"])
    import fcntl, struct, termios
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    screen = pyte.Screen(COLS, ROWS)
    stream = pyte.ByteStream(screen)
    raw = bytearray()

    def pump(seconds: float) -> None:
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.1)
            if r:
                try:
                    chunk = os.read(fd, 65536)
                except OSError:
                    return
                if not chunk:
                    return
                raw.extend(chunk)
                stream.feed(chunk)
                # Answer cursor position queries like a real terminal.
                if b"\x1b[6n" in chunk:
                    os.write(fd, f"\x1b[{screen.cursor.y + 1};{screen.cursor.x + 1}R".encode())

    def text() -> str:
        return "\n".join(screen.display)

    def wait_for(needle: str, seconds: float) -> bool:
        end = time.time() + seconds
        while time.time() < end:
            pump(0.3)
            if needle in text():
                return True
        return False

    def send(b: bytes) -> None:
        os.write(fd, b)

    checks = []

    def check(what: str, ok: bool) -> None:
        checks.append((what, ok))
        print(("  ok   " if ok else "  FAIL ") + what)

    check("TUI renders account Alpha", wait_for("Alpha", 30))
    check("TUI renders account Bravo", wait_for("Bravo", 5))
    check("fake claude runs in a pane", wait_for("fake claude in", 20))
    print("---- screen after start ----\n" + text().rstrip() + "\n----")
    send(b"\x01t")  # Ctrl-a t: new tab
    check("New Tab dialog opens", wait_for("New tab for Alpha", 10))
    send(b"\r")
    check("second tab opens", wait_for("[2/2]", 15))
    print("---- screen with two tabs ----\n" + text().rstrip() + "\n----")
    send(b"\x01Q")  # Ctrl-a Q: quit
    exited = False
    end = time.time() + 20
    while time.time() < end:
        pump(0.3)
        if b"GODTERM_EXIT=" in raw:
            exited = True
            break
    code = re.match(rb"\d+", raw.split(b"GODTERM_EXIT=")[-1]).group().decode() if exited and re.match(rb"\d+", raw.split(b"GODTERM_EXIT=")[-1]) else "?"
    check(f"quits cleanly (exit {code})", exited and code.startswith("0"))
    try:
        os.kill(pid, 9)
    except OSError:
        pass

    print("==> uninstall and clean up")
    print(ps(rf"""
$u = Join-Path $env:LOCALAPPDATA 'Programs\GodTerm\unins000.exe'
if (Test-Path $u) {{ $p = Start-Process -Wait -PassThru $u -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART'; "uninstall exit $($p.ExitCode)" }}
"left behind: $(Test-Path (Join-Path $env:LOCALAPPDATA 'Programs\GodTerm\godterm.exe'))"
Remove-Item -Recurse -Force '{HOME}', "$env:USERPROFILE\{name}" -ErrorAction SilentlyContinue
""", check=False))
    failed = [w for w, ok in checks if not ok]
    print(f"==> {len(checks) - len(failed)}/{len(checks)} checks passed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
