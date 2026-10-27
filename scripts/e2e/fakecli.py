#!/usr/bin/env python3
"""A stand-in agent CLI for the web matrix (scripts/e2e/web_matrix.py):
GodTerm runs it as a harness binary (`codex_bin = ".../fakecli.py"`) to
get screens and floods no real CLI makes on demand. Standard library only.

It starts on the main screen with 300 lines of history and a prompt
(`› ` with FAKECLI_AS=codex, `Ask anything> ` with opencode, else
`fake> `), turns on bracketed paste, echoes what is typed and logs every
byte it reads (as JSON lines) to $FAKECLI_LOG when set. Commands, typed
then Enter (CR):

    flood N     N MB of numbered lines, as fast as the PTY takes them
    stream S    one line every 100 ms for S seconds
    alt         toggle the alternate screen (redraws the prompt there)
    mouse       toggle mouse reports (1002 + SGR 1006)
    lines N     N numbered lines
    quit        exit 0

ESC CR and LF (Ctrl+J) add a new line to the prompt without submitting,
as the real CLIs do.
"""

import json
import os
import select
import sys
import termios
import time
import tty

LOG = os.environ.get("FAKECLI_LOG")
out = sys.stdout.buffer
fd = sys.stdin.fileno()


def w(s):
    out.write(s.encode() if isinstance(s, str) else s)
    out.flush()


def log(b):
    if not LOG:
        return
    with open(LOG, "a") as f:
        f.write(json.dumps({"t": time.time(), "b": b.decode("latin1")}) + "\n")


class St:
    alt = False
    mouse = False
    line = ""


NAME = os.environ.get("FAKECLI_AS", "")
PROMPT = {"codex": "› ", "opencode": "Ask anything> "}.get(NAME, "fake> ")


def prompt():
    shown = St.line.replace("\n", "⏎")
    w(f"\r\x1b[2K{PROMPT}{shown}")


def run(cmd):
    parts = cmd.split()
    if not parts:
        return
    c, arg = parts[0], (parts[1] if len(parts) > 1 else "")
    if c == "flood":
        mb = float(arg or 1)
        n = 0
        target = int(mb * 1024 * 1024)
        line_no = 0
        while n < target:
            chunk = "".join(f"flood line {line_no + i:08d} the quick brown fox jumps over the lazy dog\r\n" for i in range(200))
            line_no += 200
            w(chunk)
            n += len(chunk)
        w(f"flood done {n} bytes\r\n")
    elif c == "stream":
        end = time.time() + float(arg or 10)
        i = 0
        while time.time() < end:
            w(f"\r\x1b[2Kstream line {i}\r\n")
            prompt()
            i += 1
            r, _, _ = select.select([fd], [], [], 0.1)
            if r:
                b = os.read(fd, 4096)
                log(b)
                if b"\x03" in b:
                    break
    elif c == "lines":
        for i in range(int(arg or 10)):
            w(f"numbered line {i}\r\n")
    elif c == "alt":
        St.alt = not St.alt
        w("\x1b[?1049h\x1b[2J\x1b[H" if St.alt else "\x1b[?1049l")
        w("alternate screen on\r\n" if St.alt else "alternate screen off\r\n")
    elif c == "mouse":
        St.mouse = not St.mouse
        w("\x1b[?1002h\x1b[?1006h" if St.mouse else "\x1b[?1002l\x1b[?1006l")
        w(f"mouse {'on' if St.mouse else 'off'}\r\n")
    elif c == "quit":
        raise SystemExit(0)
    else:
        w(f"you said: {cmd}\r\n")


def main():
    old = termios.tcgetattr(fd)
    tty.setraw(fd)
    try:
        name = NAME or "fakecli"
        w("\x1b[?2004h")
        for i in range(300):
            w(f"history line {i:04d} of the fake agent ({name})\r\n")
        w("? for shortcuts\r\n")
        prompt()
        paste = False
        while True:
            b = os.read(fd, 65536)
            if not b:
                break
            log(b)
            i = 0
            while i < len(b):
                if b.startswith(b"\x1b[200~", i):
                    paste = True
                    i += 6
                    continue
                if b.startswith(b"\x1b[201~", i):
                    paste = False
                    i += 6
                    continue
                ch = b[i:i + 1]
                if not paste and b.startswith(b"\x1b\r", i):
                    St.line += "\n"
                    i += 2
                    prompt()
                    continue
                elif not paste and ch == b"\r":
                    cmd, St.line = St.line, ""
                    w("\r\n")
                    run(cmd.replace("\n", " "))
                elif not paste and ch == b"\n":
                    St.line += "\n"
                    i += 1
                    prompt()
                    continue
                elif ch == b"\x7f":
                    St.line = St.line[:-1]
                elif ch == b"\x15":
                    St.line = ""
                elif ch == b"\x03":
                    St.line = ""
                elif ch == b"\x1b":
                    j = i + 1
                    if j < len(b) and b[j:j + 1] in (b"[", b"O"):
                        j += 1
                        while j < len(b) and not (0x40 <= b[j] <= 0x7E):
                            j += 1
                        j += 1
                    i = j
                    continue
                elif ch[0] >= 0x20 or paste:
                    j = i + 1
                    while j < len(b) and 0x80 <= b[j] < 0xC0:
                        j += 1
                    St.line += b[i:j].decode("utf-8", "replace")
                    i = j
                    prompt()
                    continue
                i += 1
            prompt()
    finally:
        termios.tcsetattr(fd, termios.TCSADRAIN, old)


if __name__ == "__main__":
    main()
