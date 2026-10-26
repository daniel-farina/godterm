# Testing GodTerm end to end

Three levels, from cheapest to most real:

| Level | What | Command | Needs |
| --- | --- | --- | --- |
| Unit and PTY tests | `cargo test` (tests/tui.rs runs the binary in a PTY against a fake claude) | `cargo test --locked` | nothing |
| e2e, demo world | the real binary in tmux on `godterm --demo` (made up accounts, the demo agent, no network) | `scripts/e2e/run.py` | tmux, python3 |
| e2e, real accounts | the real binary under `sandbox-exec`, one cheap prompt per logged in harness | `scripts/e2e/run.py --suite accounts` | macOS, logged in CLIs |
| Web matrix | GodTerm Web in WebKit and Chromium (phone and desktop) on every harness, plus WebSocket stress and a soak | `scripts/e2e/web_matrix.py` | tmux, python3, node with Playwright |
| Web perf | GodTerm Web's load, input, streaming, reconnect and memory numbers against a demo or fake instance (docs/WEB_APP.md, Performance) | `scripts/e2e/web_perf.mjs` | node with Playwright (`PW_DIR`) |
| Clean machine | a fresh tart VM: install from the release artifacts, first run, setup, onboarding, Homebrew | `scripts/sandbox/vm/...` (below) | tart, the cirruslabs images |

Results go to `e2e-results/<suite>-<time>/` (gitignored): `report.md`,
`report.json`, and `screens/*.txt` (plain `tmux capture-pane`) with a
`.ansi` twin (colors kept) for every failed step and every screen a step
chose to keep. A failed step records its screen and the suite goes on.

## The e2e suite (scripts/e2e)

`scripts/e2e/run.py` starts its own tmux server (`tmux -L godterm-e2e-<pid>`,
never yours), runs the binary in a 200x50 session with exactly the
environment it builds (`env -i`), drives it with `send-keys` and reads it
with `capture-pane`. It also talks to the running GodTerm over its control
API (`godterm mcp`, stdio JSON-RPC) to read tab states and receipts.

```sh
cargo build --release
scripts/e2e/run.py                      # demo world, sandboxed (macOS)
scripts/e2e/run.py --suite accounts     # real accounts (macOS, see below)
scripts/e2e/run.py --suite accounts --only account1,codex
scripts/e2e/run.py --suite all
scripts/e2e/run.py --suite ci --bin target/debug/godterm   # GitHub runners: demo only, no sandbox-exec
E2E_NO_PROMPT=1 scripts/e2e/run.py --suite accounts       # launch and quit only, no model usage
```

`--keep` leaves the sandbox roots in /private/tmp for inspection; without
it they are deleted at the end. The exit status is 1 when any step failed.

### Demo world scenarios (`--suite demo`, `ci`)

Every run uses a fresh `GODTERM_DEMO_HOME` inside a sandbox root, with
`--demo-no-external` (no tmux session of its own) and `--demo-manual` (no
usage burn).

1. **Full world**: launch (16 tabs restored), the six pane grid, help
   (`Ctrl-a ?`), command palette (`Ctrl-a :`, filtering), Settings
   (`Ctrl-a ,`), layouts (`Ctrl-a L` five times: at least three distinct
   arrangements and back to the first), New tab (`Ctrl-a t`, Enter) and
   close it (`Ctrl-a w`, the tab count goes back), the approvals queue
   (`Ctrl-a y`, rows with Approve / Always / Deny / Jump), approving the
   selected row (`y`), the Browser dialog (`Ctrl-a i`, the demo Chrome
   profiles), update mode through the palette (on, then off), the voice
   onboarding replay from the palette, `get_state` through `godterm mcp`,
   quit (`Ctrl-a Q`, exit 0), and no panic in `godterm.log`.
2. **One account per harness** (`--demo-harnesses`): launch, six panes
   with four CLI-managed accounts, open and close a tab, the Browser
   dialog on a harness tab, `get_state`, quit, no panics.
3. **Onboarding at start** (`--demo --onboarding`): the welcome card comes
   first, Esc skips to the grid, quit.

### Real accounts (`--suite accounts`)

For each slot (by default every Claude account of the real config, the
`demo8` Grok login, Codex, Cursor, Antigravity and OpenCode when
installed), a separate sandboxed GodTerm with that one slot:

1. launch; the tab starts in a fresh project folder inside the sandbox
   root and reaches `ready` (`get_state`), stable for 3 s with the CLI's
   own prompt on screen. Start up questions are answered the way a user
   would without saving anything: the Codex update menu gets Skip, a
   folder trust prompt is answered through GodTerm's own approvals
   (`answer_prompt yes`). Cursor gets `--trust` for the launch. Codex
   asks to trust any folder outside an already trusted one and saves the
   answer in `~/.codex/config.toml` (write protected here, and it ignores
   a `-c` override for that prompt), so its project folder is
   `~/.cache/godterm-e2e/<root>/codex` (under the home folder, which is
   trusted) and is deleted with the root;
2. ONE prompt, "reply with the single word ok": Claude and Grok through
   the control API (`send_prompt`, so the receipt states are exercised),
   the other CLIs typed into the pane through tmux (text, then Enter on
   its own, which Codex needs);
3. wait for an `ok` line on the screen with the tab back at `ready`;
   Claude and Grok also `read_tab` the reply;
4. the activity states seen (`starting`, `working`, `ready`) and receipts:
   Claude's from its transcript (`received`, `started`, `done`; GodTerm
   learns a fresh tab's session id every 10 s, so they follow the reply by
   up to that), Grok's "delivered or not" (`unconfirmed`). OpenCode with no
   reachable model provider is a skip;
5. close the tab (`Ctrl-a w`), quit (`Ctrl-a Q`, exit 0);
6. the CLIs' settings files are byte for byte unchanged (a change made by
   your own sessions outside the sandbox during the run is reported as a
   skip, since the sandbox denies those writes), and no panic in the log.

`accounts.json` in the results lists the states, receipts, the number of
model prompts sent (one per slot at most) and the session files each run
left in the CLI's own store (ids only; nothing is deleted there). The
project folders go with the sandbox root. GodTerm's auto trust adds the
project folder to the Claude account's `.claude.json` project list, as
Claude Code itself does for every folder it runs in.

## Web matrix (scripts/e2e/web_matrix.py)

The web app driven like a user, on every agent CLI GodTerm hosts, in
both engines a phone and a desktop meet: WebKit and Chromium, at
430x932 (3x, touch, an iPhone's user agent) and 1440x900. Each instance
is its own GodTerm in a private tmux server, on port 7995 or the next
free one (never 7878 or 7880), and is stopped and deleted at the end.

```sh
# Playwright for Node beside the web app (or PW_DIR=<node_modules>):
ln -s <a dir with playwright>/node_modules web/dev/
scripts/e2e/web_matrix.py                          # demo world, both engines and sizes
scripts/e2e/web_matrix.py --instance demo,demofull,fake
scripts/e2e/web_matrix.py --instance demo,mock --groups layout,keyboard,shared,clip
scripts/e2e/web_matrix.py --instance unauth        # the real CLIs, signed out
scripts/e2e/web_matrix.py --instance real          # real accounts, host sandbox
scripts/e2e/web_matrix.py --soak 30                # + 30 minutes of soak
scripts/e2e/web_matrix.py --engines chromium --devices phone --groups typing,scroll
scripts/e2e/web_matrix.py --instance fake --serve  # start one, print its link, wait
```

`--web-dir web` serves the web app from disk (no rebuild for a JS edit).
Results: `e2e-results/web-matrix-<time>/report.md` (a table per instance:
harness by engine and size, then by check group, then every failure),
`results.jsonl` (one line per check) and screenshots. Exit status 1 when
a check failed.

Instances:

- `demo`: `godterm --demo --demo-harnesses` (sandboxed, made up
  accounts): one tab per harness, the demo agent drawn as each CLI, with
  each CLI's approval prompts and keys.
- `demofull`: `godterm --demo` (16 Claude and Grok tabs), for their
  approvals.
- `fake`: Codex and OpenCode slots running `scripts/e2e/fakecli.py`, a
  stand-in CLI on the main screen with 300 lines of history that logs
  every byte it reads and floods on demand (`flood 30`).
- `mock`: `web/dev/mock.py` alone (no GodTerm): fake tabs and speech
  clips on demand, for the `clip` group.
- `unauth`: the real CLIs with HOME in a temp root (signed out; the
  profile denies reads of every real agent dir). Only passive checks:
  a tab showing a login or setup screen gets no keys at all.
- `real`: the host sandbox (below) with the real logged in CLIs on a
  separate GodTerm: only keys that cannot start or change anything (no
  Esc, Shift+Tab or Enter), then one tiny turn per harness ("reply with
  the word ok") through the web app's Prompt box (`--no-turns`: none).
  Start up questions are answered as a user would without saving
  anything (Codex's update menu: Skip; folder trust from the web
  approvals).

Check groups (`--groups`): `tabs` (listed with the right state, opened,
snapshot drawn), `live` (output of a working tab), `typing` (plain text;
on a phone the keyboard paths of web/js/textinput.js: dictation partials,
autocorrect, a composition; Return as the harness's new line, Shift+Enter
on a desktop, paste bracketed when the app asked, the Prompt box's Enter
on its own frame), `keys` (Ctrl latch and lock, arrows in the app's
cursor mode, Tab, Esc, Shift+Tab, PgUp / PgDn to the app or the local
history, Send), `scroll` (a drag or the wheel: wheel notches to an app on
the alternate screen or with mouse reports, else this terminal's history;
no snap back while output streams; Jump to the end), `resize` (fit to
screen takes the size, zoom steps make one resize, take size and give it
back on a desktop), `reconnect` (a dropped socket resumes without a
snapshot; hidden then shown answers the ping), `image` (attached into the
tab: the saved path pasted, `Image: ` for Grok), `approval` (each
harness's first approval answered Yes from the approvals sheet; only the
choices its prompt has), `options` (Switch model: the tab's own models,
Fable for Claude, a note for the CLIs that pick their own; rename, pin,
move to another CLI refused, archive without a session id refused, close
of a pinned tab asks first), `chat` (a web turn, web_layout reaching this
browser, an image into the chat), `voice` (push to talk with Chromium's
fake microphone), `stress` (40 rapid tab switches, rotation, keyboard
open and close loops), `layout` (desktop: 820x1180 is the desktop
layout and 844x390 the phone one; 1000x700 with the tab panel and the
chat keeps the terminal 520 px wide, the side list folds into an overlay
the Tabs button opens and the scrim shuts; the status bar's "..." item
opens the dropped items as a menu above it; split 4 at 800x600 falls back
to 2 stacked and comes back with the room), `keyboard` (desktop: F6 and
Shift+F6 walk the areas with nothing sent to the tab, Shift+Esc leaves the
terminal for its pane header, ? opens the shortcuts sheet and Esc gives
the focus back, ? in the terminal is typed, a dialog is centred and closes
on Escape and the scrim), `keybar` (phone: the keys by use, the first
seven beside Send on a 390 px phone), `sheet` (phone: a message stays
under the sheet, a pull down or a flick closes it, a short pull settles
back), `retry` (the Sessions screen's Retry row from a failing tool reply
the test writes itself, an older GodTerm named as such, the Open on Mac
button's reply as a message where messages go on this device), `shared`
(a second page on the same tab takes the size: the first yields with
"Sized by ...", scrolling there leaves it alone, typing takes it back),
`clip` (mock only, Chromium: the speech clips `/dev/clip` sends are faded
at their edges and a full scale one is scaled under clipping), `turn`
(real only).

Without a browser, over the WebSocket: every tab subscribed and
streaming with the UI thread's round trip, a client that reads nothing,
two clients on one tab (the same bytes; a resize from one reaches the
other), 20 reconnects; on `fake`, the bytes the web sends arrive at the
CLI unchanged, and a 30 MB flood (the UI thread stays responsive, the
server's memory comes back, a snapshot after it ends where the CLI is).
`--soak N`: N minutes of a phone page switching tabs while they stream,
the server's RSS and CPU every 10 s and the page's JS heap and DOM size
(growth fails it).

Memory: one browser at a time; a run waits (then skips) while less than
6 GB is free.

## Host sandbox (scripts/sandbox/host)

`scripts/sandbox/host/sandbox.py` runs the real binary with the real
logged in agent CLIs, and nothing else of yours:

```sh
scripts/sandbox/host/sandbox.py list                       # the slots it can use here
scripts/sandbox/host/sandbox.py run --accounts account1    # GodTerm in your terminal, sandboxed
scripts/sandbox/host/sandbox.py profile --accounts codex   # print the profile
```

- **GodTerm's own state** lives in `<root>/home` (`GODTERM_HOME`), a new
  `/private/tmp/godterm-sandbox-*` root per run. The real `~/.godterm` is
  never read (the profile denies it) apart from the account dirs in use.
- **Accounts in place**: the slots point at the real account dirs
  (`~/.godterm/accounts/accountN` for Claude, exactly the `config_dir`
  string of the real config, since Claude Code's keychain item name is
  derived from it; `~/.godterm/accounts/demo8` for Grok; `~/.codex` for
  Codex), Cursor, Antigravity and OpenCode use their native stores.
  Nothing is copied: a copied OAuth refresh token rotates and breaks the
  real login. Nothing logs in or out, and no bypass flag is passed (every
  slot runs with `permission_mode = "default"`; Codex trusts the project
  folder for that launch only with `-c projects."<dir>".trust_level`).
- **The profile** (`sandbox-exec`, last matching rule wins): writes are
  denied everywhere except the sandbox root, `/dev`, the temp dirs, the
  account dirs in use and the CLIs' own state dirs (sessions, logs,
  caches, token refreshes); the CLIs' settings files (`~/.codex/config.toml`,
  `~/.cursor/cli-config.json`, `~/.gemini/.../settings.json`, the
  accounts' `settings.json`, the real `~/.godterm/config.toml`, ...) are
  write protected even inside those dirs; reads of `~/.ssh`,
  `/Volumes/Private`, `~/.gnupg`, `~/.aws`, `~/.config/gh`, the release
  signing keys, `~/Documents`, `~/Desktop`, `~/Downloads` and the rest of
  `~/.godterm` are denied. Network is allowed.
- **Quiet**: no sound, no microphone, nothing opened on the desktop, no
  splash, no update checks, no notifications, no supervisor, no assistant
  turns (`[assistant] mode = "off"`), the tab job tools off, and usage
  meters read from `tests/fixtures/usage.json` (no usage API calls).

The demo runs under a stricter profile (`demo_profile`): writes only in
its root, and no reads of any real agent or GodTerm data at all.

Refused writes show in the unified log; the accounts suite lists the ones
from the agent processes after each slot (`log show --predicate 'sender ==
"Sandbox"'`).

## Clean machine VM (scripts/sandbox/vm)

`mac-e2e.sh` and `linux-e2e.sh` test the release artifacts the way a new
user meets them: a fresh tart VM with no accounts, no agent CLIs and no
audio hardware.

```sh
scripts/sandbox/vm/mac-e2e.sh   [--artifacts DIR] [--version V] [--only 1,2,..6] [--brew tap|local|both|none] [--results DIR] [--base IMG] [--cpu N --memory MB] [--keep]
scripts/sandbox/vm/linux-e2e.sh [--artifacts DIR] [--version V] [--only 1..7] [--no-recommends] [--results DIR] [--base IMG] [--cpu N --memory MB] [--keep]
```

- Each run clones a cirruslabs base image (`ghcr.io/cirruslabs/macos-sonoma-base`
  or `ghcr.io/cirruslabs/ubuntu:24.04`, user admin/admin) into a throwaway
  `godterm-e2e-*` VM (never any other VM), checks the host's free memory
  first (`memory_pressure`, with an 8 GB margin; it refuses to boot under
  pressure), boots it headless (at most 4 CPUs / 8 GB for macOS, 2 / 4 GB
  for Linux), adds a per run ssh key (deleted at teardown) and copies in
  only the release artifacts (`dist/release`, `GODTERM_ARTIFACTS` or
  `--artifacts DIR` for a local build). Nothing from your home goes in.
- macOS scenarios: 1 install (the zip on PATH, `--version`; the DMG into
  /Applications with codesign, Gatekeeper, staple and quarantine checks),
  2 first launch on a fresh home, 3 the `godterm setup` wizard (an
  interrupted wizard writes no config), 4 `--onboarding` (with and without
  `GODTERM_NO_AUDIO`), 5 the demo smoke run, 6 Homebrew from the public tap
  and from the repo formula and cask on a `file://` copy.
- Linux: the .deb with apt, `--version`, help, doctor, first launch, the
  demo, both onboarding variants, `apt remove`, the AppImage (runs without
  FUSE) and the tarball.
- GodTerm runs in tmux inside the VM; every step is PASS, FAIL, WARN or SKIP
  and every screen is saved (`capture-pane`, plain and ANSI) under
  `e2e-results/vm-<ts>/{mac,linux}/` with `report.md`, `summary.txt`,
  `run.log` and `logs/`. The VM is stopped and deleted on exit (`--keep`
  leaves it running for debugging). One VM at a time; a full run takes
  about 3.5 minutes (macOS) and 2.5 minutes (Linux).

## CI

The `test` job in `.github/workflows/ci.yml` runs the demo subset on
macOS and Ubuntu runners after `cargo test`
(`scripts/e2e/run.py --suite ci --bin target/debug/godterm`) and uploads
`e2e-results/ci` when it fails.
