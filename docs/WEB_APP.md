# GodTerm Web

GodTerm's whole interface in a browser: every tab as a live terminal
(xterm.js), the assistant chat, approvals, voice, jobs and the tab
management. It runs inside the GodTerm process, so a browser sees the same
tabs, PTYs and assistant as the terminal UI, at the same moment.

## Goals

- Fast: a page load under 100 KB before xterm (xterm.js is vendored and
  cached), first screen of a tab painted in one round trip, keystroke
  echo as fast as a local terminal (no polling anywhere; one WebSocket).
- Mobile first: works one handed on a phone (drawer tab list, a key bar
  with Esc, Tab, Ctrl, arrows and Enter, big approval buttons, a composer
  for prompts), then scales up to a multi pane desktop layout.
- Opened locally (http://127.0.0.1:7878) or over Tailscale
  (`tailscale serve --https=7878 http://127.0.0.1:7878`, tailnet only;
  or http://<tailnet IPv4>:7880). HTTPS from Tailscale is what lets a
  phone use the mic. On the tailnet, the owner's own devices sign in with
  no token.
- Calm palette, no neon: the terminal UI's colors.

## Server

- `src/web/` in the GodTerm binary (as built: module 1 and the server
  half of modules 3 and 4). Plain std threads and `tungstenite` (sync); no
  async runtime. Off unless `[web] enabled = true` (config) or
  `godterm --web`. Binds `127.0.0.1`; the port is `[web] port`
  (default 7878), or `--web-port N` / `GODTERM_WEB_PORT` for one run.
  Remote access goes through `tailscale serve`, or the tailnet listener:
  with `[web] tailnet_ip = true` (default; needs `tailnet_sign_in`) it
  also binds this Mac's tailnet IPv4 (`Self.TailscaleIPs` from
  `tailscale status --json`) on `[web] ip_port` (default 7880), plain
  http. That port is tried again every 5 s while busy or while tailscale
  has no address, and the address is checked every 30 s (a new one is
  bound, the old one let go). A connection from outside 100.64.0.0/10 or
  fd7a:115c:a1e0::/48 is closed unanswered. Never a public bind.
  `[web] hosts = ["name"]` adds Host names besides loopback and `*.ts.net`.
  A busy port is tried again every 5 s (one flash and one log line say
  so) until it is free. Changing `web.enabled` or `web.port` in Settings
  (or config.toml) starts, stops or moves the server at once, no restart.
- Static files: everything under `web/` is embedded at build time by
  build.rs (subfolders included, dot files and `dev/` skipped), text files gzipped
  when that saves 10%. `index.html` (also served for `/`) is `no-cache`;
  any other file is `max-age=31536000, immutable` when requested with
  `?v=<build>` naming the build served now (the `build` from `hello`;
  an older build's `?v=` gets today's file with `no-cache`), else `no-cache` with an ETag
  (the build), so 304s keep reloads cheap. build.rs serves index.html and
  the modules with `?v=<build>` on every local src, href and `./x.js`
  import, so all of them are immutable; index.html preloads the first
  screen's modules (`modulepreload`, no import waterfall) and loads the
  vendored scripts with `defer`. sw.js gets the build too: it precaches
  every versioned file at install and serves them cache first, the page
  itself network first (the cache is its offline fallback). index.html gets a CSP:
  `default-src 'self'; connect-src 'self'; img-src 'self' data: blob:;
  media-src 'self' blob:; style-src 'self' 'unsafe-inline'; script-src
  'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'`
  (no inline scripts, no CDNs; workers and worklets from 'self').
- Hot reload (src/web/assets.rs, src/web/pack.rs): the files can come
  from a folder on disk instead, so a front-end change needs no rebuild
  and no restart. Which folder: `[web] assets_dir = "/path/to/web"` (`~`
  allowed); when that is empty, the repo's `web/` if this binary runs
  from inside the checkout it was built in (`target/release/godterm` in
  that repo: `CARGO_MANIFEST_DIR` from the build, the running exe under
  it, and `web/index.html` there), unless `[web] embedded_assets = true`.
  A copied or installed binary (a release, GodTerm.app's own copy) never
  picks a folder by itself. Changing either key restarts the server. A
  watcher thread (`web-assets`) checks the folder every second (paths,
  sizes, mtimes: no reads); a change is packed once the folder looked the
  same for 2 checks after it (2 to 3 s; a multi-file edit or a checkout
  under way waits), and checks do not count while git works in the
  checkout the folder is in (`index.lock`, `MERGE_HEAD`,
  `CHERRY_PICK_HEAD`, `REVERT_HEAD`, `rebase-merge`, `rebase-apply` in its
  git dir, worktrees included). The new files are swapped in whole (an
  `Arc` behind a `RwLock`; a request in flight keeps the table it took)
  with a new build id, and every connected browser gets a `build` frame
  at once. build.rs and the watcher share src/web/pack.rs (walk, build
  id, `?v=` versioning, gzip), so the same files give byte-identical
  output and the same build id as the embedded copy (a test checks it).
  Robustness: a file that cannot be read, a folder over 64 MB or one with
  no `index.html` (a checkout half way, a wrong path) keeps the files
  served now and is logged once; a folder missing at start serves the
  embedded files until it loads. Security: files are looked up by exact
  path in the in-memory table (no file system path is ever built from a
  request, so `..` reaches nothing); the walk skips dot entries, `dev/`
  and any entry whose real path (symlinks resolved) is outside the
  folder; auth and the public list are as for the embedded files.
  Anyone who can write the folder controls the app's JavaScript, so it
  should be yours (the repo). Caveat: what is in the folder goes live,
  including a branch you check out whose front end needs a newer server.
  `GODTERM_WEB_DIR` (below) is still the raw development mode (files as
  they are on every request, no versioning) and wins over both.
- Auth: a random token (64 hex) in `<app home>/web-token`
  (`~/.godterm/web-token`, `~/.godterm-demo/app/web-token` in demo mode,
  kept when the demo world is rebuilt;
  0600, made on first start). `godterm web-url` prints the local and
  Tailscale links (`https://<Self.DNSName>:<port>/?t=<token>`). Any GET
  with `?t=<token>` answers `303` to the same path and query without `t`,
  setting `godterm_web=<sha256 of the token>; Path=/; HttpOnly;
  SameSite=Strict; Max-Age=31536000` (plus `Secure` over https). Every
  other HTTP request and the WebSocket need that cookie (`401` without).
  Wrong tokens: `403`; after 10 in a minute every login is refused with
  `429` until the minute passes. A Host that is not loopback, `*.ts.net`
  or in `[web] hosts` gets `421` (DNS rebinding). The WebSocket also needs
  `Origin` equal to the Host (or `X-Forwarded-Host`), port included.
  Comparisons are constant time. What installing the app needs is
  public (no cookie): `manifest.webmanifest`, `sw.js`, `icon*.png`,
  `icon.svg`, `shots/*.png`, `signin.html`, `signin.js` and
  `offline.html`; none holds anything private, and a browser's manifest
  fetch carries no cookie. The page itself without a cookie (and no
  tailnet sign in) answers `401` with `signin.html`: paste the link or the
  token (a home screen app starts with no cookie and no address bar).
- Tailnet sign in (`[web] tailnet_sign_in = true`, the default;
  src/web/tailnet.rs): a request with no cookie is signed in with no token
  when its tailnet login is allowed: the owner of this Mac's tailnet node
  (`Self.UserID` -> `User[id].LoginName` in `tailscale status --json`; a
  tagged node has none) or one in `[web] tailnet_users = ["a@b.com"]`
  (any case). The answer is the page itself plus the same `Set-Cookie` a
  token login gets, so later requests and the WebSocket go by the cookie.
  Who the login is comes from one of two places:
  - Through `tailscale serve` on 127.0.0.1: the `Tailscale-User-Login`
    header serve adds for requests from tailnet users (none for tagged
    nodes; serve drops a copy the client sent, checked with tailscale
    1.98). Trusted only when the connection is loopback, the Host is
    `*.ts.net`, there is exactly one such header, and lsof shows no
    process of this user on the client end except a root owned Tailscale
    binary (by the kernel's path, `proc_pidpath`): tailscaled runs as root
    and is invisible to lsof, while a local program sending the header
    itself is seen and refused (logged). Any other Host ignores the
    header. The device must also be another one: when serve's
    `X-Forwarded-For` is this Mac's own tailnet address (any local
    program, a tab's agent too, can open the ts.net link), the token is
    needed.
  - On the tailnet listener: `tailscale whois --json <peer ip>` (kept 3
    minutes, a failed answer 10 s). A tagged node is refused. This Mac's
    own tailnet addresses are never signed in this way (a local process
    could connect to them): they need the token. Identity headers mean
    nothing on this listener.
  A tailnet user that is not allowed (a device shared in from another
  account, a tagged node) gets `403` with a page naming the login and how
  to allow it, never the app; the token link still works for them if
  they have it. No identity at all: the `401` token hint.
  `godterm web-url` prints the plain https and IP links (and who they
  sign in) plus the token link for local use. The token path is
  unchanged for 127.0.0.1 and everything else. Who can reach the app now:
  any device logged into the owner's tailnet account (phones, other Macs,
  containers or VMs running tailscale as that user), plus
  `tailnet_users`; set `tailnet_sign_in = false` for the token only. A login or WebSocket from a process
  GodTerm itself started (a tab's agent that read the token file: same
  user) is refused with `403` (the client's pid is looked up with lsof on
  loopback connections and connections from this Mac's own tailnet
  address, about 100 ms per login or socket open; a process
  that detached from GodTerm is not caught). A browser with the cookie is the user: it
  may do what the keyboard may do, and confirmations it answers count as
  the user's.

### Trying it

    # A demo instance on its own HOME, web app on port 7901:
    HOME=$(mktemp -d) GODTERM_WEB_PORT=7901 target/release/godterm --demo --demo-no-external --web
    # (another terminal, same HOME) the sign in link:
    HOME=<that dir> target/release/godterm --demo web-url   # or read ~/.godterm-demo/app/web-token
    # (same HOME) what the browser logged:
    HOME=<that dir> target/release/godterm --demo web-log --follow
    # Development: serve web/ from disk (a reload shows an edit):
    GODTERM_WEB_DIR=$PWD/web target/release/godterm --web
    # Hot reload end to end (a demo instance, a temp copy of web/ as
    # [web] assets_dir, edits pushed as build frames):
    scripts/e2e/web_hot.py
    # The e2e (a private tmux server, temp HOME, a free port):
    scripts/e2e/web.py

## Protocol

One WebSocket at `/ws` (protocol version 1). Text frames are JSON
`{"t": "<type>", ...}`; binary frames carry terminal bytes (and audio,
the mic and browser view pictures): 1 byte kind, 4 bytes tab uid (big
endian), then the payload. Tabs are named by their numeric `uid` (the
`t12` ids of get_state are accepted where a tab is named in JSON too).

### Settings > GitHub accounts and repos (`git` frames, `cfg_*`)

Browser to server: `{t: "git", id, op}` with `op` one of `cfg_view`
(owners, rules, placeholders, gh accounts, known projects, bindings,
preview), `cfg_check` (`draft`, optional `test` folder: errors,
warnings, preview, diff and a `plan_id` when there is something to save)
and `cfg_save` (`plan_id`: writes config.toml and the bindings, then
answers with the new view). See docs/SYNC.md, "Setting accounts and
rules in the app".

`cfg_gh_login` (src/gh_login.rs) starts the user's gh login of one more
account, or answers the one under way: GodTerm runs exactly
`gh auth login --hostname github.com --git-protocol https --web` with no
terminal (prompts off, no GH_TOKEN, no browser opened), reads gh's
one-time code and URL from its output, and answers `login` `{state,
code, url, account, error}`, state being `starting`, `code`, `done`,
`failed` or `cancelled`. `cfg_gh_login_poll` answers the same (state ""
when none ran; once `done`, gh is asked for its accounts again) and
`cfg_gh_login_cancel` stops it. One at a time, given up after 15 minutes;
only these args pass `gh_args_ok`, never `gh auth switch` or logout. gh
makes the account it just logged in its active one; GodTerm pushes with
each owner's own account, so that changes nothing here.

### The Git sheet (`git` frames)

Browser to server: `{t: "git", id, tab, op}` with `op` one of
- `view`: the tab's Git section (`view`: branch, remote, state, warnings,
  resolved binding, commits, changes, branches, actions);
- `prepare`: `action` (`commit` with `message` and `all`, `push`, `pull`,
  `sync`, `fetch`, `push_as_branch`, `switch` with `branch` and `stash`,
  `create_branch` with `name`, `bind` with `owner`, `repo`, `account`,
  `create` with `owner`, `name`, `account`, `link`, `ignores`, `untrack`).
  Runs the checks, changes nothing, answers `plan` (title, lines,
  warnings, hard_blocks, soft_blocks, needs_typed) and a one time
  `plan_id` (10 minutes);
- `run`: `plan_id` and, when the plan needs it, `typed` ("push anyway").
  Answers `said` or `error` and a fresh `view`.

Server to browser: `{t: "git", id, tab, ok, ...}` as above. The sheet's
confirm button is the user's yes; no control tool sends these frames.

Layout (web/js/git.js): on a desktop the sheet is a large dialog in the
middle of the window (min(1100px, 92vw) by 85vh): the folder and status
chips on top (branch, remote, ahead and behind, last fetch, the GitHub
binding, warnings), the actions as a compact row of buttons (git ones,
then the repo ones), and the changes (staged, unstaged and untracked
lists) beside the commits. In the docked tab panel it is one compact
column (the git actions in a three column grid). Phones keep the bottom
sheet with the actions as full width rows. Its sub screens (commit,
define, branches, a plan to confirm) stay at most 760 px wide.

When no gh account is set for the repo's owner ("Which gh account
pushes for <owner>? It is not in [git.owners]"), the screen asks with
buttons instead: the gh accounts logged in on this Mac (from `cfg_view`,
the active one marked, logged out ones disabled), each saving
owner -> account in [git.owners] through `cfg_check` then `cfg_save` (the
click is the yes; the other owners, rules and bindings are kept), then
the screen reads git again. "Add account..." runs a gh login for a new
account (`cfg_gh_login`, below): it shows gh's one-time code with Copy
code and Open github.com/login/device, polls until gh finishes, then
lists the new account to pick (it is not picked for you). A GodTerm
without the login op answers "unknown git op"; the screen then shows
the gh command to run in a terminal tab instead.

### The live browser view (`bview` frames, binary kind 5)

A tab's Browser section shows the page its agent drives, live, when the
tab's bound browser has an active BrowserSkill session (src/browser_view.rs,
web/js/bview.js); Expand opens it big (a dialog on a desktop, the whole
screen on a phone). Without a session it says "No active session: the
agent hasn't opened the browser", with a Start a session button, and
the section's usual cards stay below either way.

Browser to server: `{t: "bview", id?, tab, op}` with `op` one of
- `open`: attach this browser's view of the tab. GodTerm finds the tab's
  session itself (the page never names one): sessions on the tab's own
  browser only (the bsk instance matched to its Chrome profile, as the
  Browser dialog matches it); of those, one the user started from this
  view, else the one the agent's transcript names (`--session <id>` in a
  bsk call, or the `session_id` a `session start` printed), else the only
  one there, unless another tab is bound to the same profile. It never
  starts one. Answers `{ok, state, say, can_start, session}`: `state` is
  `live` (pictures follow; `session` is the id's first 8 characters),
  `none`, `unsure` (several sessions, none known to be this tab's),
  `not_connected`, `no_browser`, `no_bsk` or `error`;
- `keep`: still watching (every 20 s; a view silent for 90 s is dropped);
- `close`: stop (the page sends it when the section or dialog closes or
  the page goes to the background; a socket that closes stops too);
- `act` with `do`: `click` `{x, y}` (fractions of the picture: the element
  there gets a real `bsk click` when its centre is that element, else the
  click is dispatched at the point in the page), `wheel` `{dy, dx}`
  (fractions of the picture's height and width, turned into page pixels),
  `back`, `forward`, `reload`, `navigate` `{url}` (http and https only; a
  bare host gets https), `type` `{text}` (inserted into the focused field)
  and `key` `{key}` (`bsk press`, names like `Enter` or `Ctrl+A`). Only on
  the session this browser's view is attached to; answers `{ok, say |
  error}`;
- `start`: the user pressed Start: `bsk session start --browser <the tab's
  instance>`, only when exactly one connected browser is the tab's, then
  attaches as `open` does.

Server to browser: the answers above as `{t: "bview", id, tab, op, ...}`,
and while a view is attached:
- binary kind 5, tab uid, a JPEG (or the PNG when that is smaller): the
  viewport (`bsk screenshot --session <id> --json --out <tmp>`, made a
  JPEG with `sips`, scaled to 1440 px wide at most). One capture loop per
  session, shared by every browser watching it, only while one does;
  never two captures at once, the next one after twice the time the last
  took (0.65 to 3 s: about 1.5 a second), 0.25 s after an action. A
  browser that has not taken the last picture yet gets none (no backlog);
- `{op: "meta", url, title, tabs}`: the Agent Window's active tab (`bsk tab
  list --scope agent`, every 2 s, sent when it changes);
- `{op: "error", error}`: a capture failed (the first, then every tenth);
- `{op: "ended"}`: the session is gone (the agent stopped it): the loop
  stops.

An older GodTerm answers `error` "unknown frame type": the section then
says the live view needs a restart. No control tool sends these frames.

### The Accounts screen (`accounts` frames)

Opened from Manage > Accounts and logins, Settings > Accounts and usage,
or the assistant's `web_view accounts` (src/app_accounts.rs,
web/js/accounts.js). One card per account (Claude, Grok, Codex, Cursor,
agy, OpenCode): label, agent, color, login (logged in with the email,
plan and organization the CLI cached, unless privacy is on; logged out;
expired; or "login in its CLI" for the CLIs that keep their own),
usage bars, running tabs, and whether it runs the assistant. Actions:
Log in / Re-login, Log out, Use for the assistant, Rename, Color, Remove,
Add account (name, agent, color, folder), and for Claude accounts the
long-lived token.

Browser to server: `{t: "accounts", id, op, ...}` with `op` one of
- `watch` `{on}`: this browser has the screen open (or not); while on,
  GodTerm pushes `{t: "accounts", op: "view", view}` whenever the view
  changes (a login moves on), checked every 250 ms, never polled;
- `view`; `login` `{account}` (1 based); `code` `{account, code}` (typed
  into the login's tab, then Enter; never logged); `retry` (Enter in the
  login tab, for "Press Enter to retry"); `cancel` (stops the login tab);
  `open_mac` (opens the sign in page on the Mac: a CLI whose page
  redirects to localhost, like Codex, must be signed in on the Mac);
- `add` `{label, harness, color, folder}` (starts its login like the
  terminal UI's Add account); `rename` `{account, label}`; `color`
  `{account, color}` (a swatch name); `assistant` `{account}`;
- `token_create` `{account}`: `claude setup-token` in a new secret tab
  of that account (CLAUDE_CONFIG_DIR = its folder); `token_use`
  `{account, on}`: the assistant on that account runs with the token;
  `token_sync` `{account, on}` marks it to sync; `token_sync_all`
  `{on}` turns token sync on for this Mac (on asks first with `confirm`;
  docs/SYNC.md 2.8). The view's `token_sync` is `{on, sync_enabled, repo,
  warning}` and each token has `sync` and `from` (the machine it came
  from);
- `logout`, `remove` `{delete_data?}`, `token_copy`, `token_delete`:
  these answer `confirm` `{id, question, yes}` and do nothing; the
  sheet's yes sends `{op: "confirm", confirm_id}` (`answer: "no"` drops
  it). A confirm id works once, for 5 minutes. A remove with
  `delete_data` asks about the folder in a second confirm after the
  account is removed; only folders under the app home's `accounts/` can
  go, and they go to the Trash.

Server to browser: `{t: "accounts", id, ok, said?, error?, confirm?,
tab?, view}`. `view`: `{accounts: [{account, name, label, harness,
harness_label, color, color_name, state, email, plan, org, folder,
config_dir, tabs, assistant, assistant_next, buckets, usage_error,
integrated, installed, can_token, token: {created, last4, use} | null,
flow, removed}], agents: [{id, name, sub, installed, how}], swatches:
[{name, hex}], privacy}`. `flow` is a login or token in progress (or
finished in the last 3 minutes): `{kind: login | token, stage: starting |
sign_in | code | checking | done | failed, note, tab, url, open_on_mac,
user_code, wants_code, error, retry, seconds}`.

The login from a phone: Log in starts the CLI's own login in a tab of
that account (a new tab when its shown tab is in use), as the terminal UI
does. GodTerm reads the tab's screen (src/login_screen.rs: the URL with
its wrapped lines joined, a device code, "Paste code here", success and
error lines, onboarding screens it answers with Enter) and the sheet
shows a big Open sign-in page link (opened on the phone), the device
code to check (Grok), and a Paste code box when the CLI asks for one.
When the account shows logged in (or the CLI says so) the flow is done.
Show the tab opens the tab itself (for CLIs whose login is not a URL).

Long-lived tokens: see SECURITY.md. The token is caught from the tab's
output before any screen sees it and is stored only in the keychain
(service `godterm-token`); no frame ever holds it, only `last4`.

### Server to browser

- `hello` (once, first): `{proto: 1, version, build, client, theme,
  state, messages, live, update}`.
  - `build`: a 12 hex hash of the web files served (embedded or the
    assets folder). The page knows its own build from its `?v=` (else
    the first hello's); a `hello` or `build` frame naming another one
    shows "Update available", asks the service worker to update (sw.js
    carries the build, so a new one installs and waits) and Reload
    hands over to it.
- `build` `{build}`: the served files changed (hot reload, above): sent
  to every browser at once.
  - `theme`: the terminal UI's colors (`bg, fg, dim, faint, bar,
    selection, select, sage, sand, slate, clay, mauve, stone, mist, rust,
    active, attention, red`) as `#rrggbb`.
  - `state`: as in `state` below. `messages`: the assistant chat so far
    (each as an `assistant` frame without `t`). `live`: the reply being
    streamed, or null. `update`: as the `update` frame.
- `state` `{state}`: the whole state again whenever it changed, at most
  every 100 ms (pushed from the UI loop, nothing polls). `state` holds:
  - `tabs`: get_state's tab objects (`id` "t12", `name`, `account`,
    `account_label`, `folder`, `state` (working, ready, waiting for
    approval, background, starting, ended, not started...), `busy`,
    `label`, `group`, `pinned`, `color`, `model`, `effort`, `loops`,
    `queued_prompts`, `waiting_for`, `focused`...) plus `uid`, `rows`,
    `cols` (the PTY size), `pane` (1 based), `active` (the pane's shown
    tab), `running`, `session_id` (its conversation, when known),
    `size_taken_by` (a client id, when a browser took its size).
    `git_facts` `{branch, dirty, unpushed, behind, error}`: its repo in
    numbers for the status bar (uncommitted files, commits no remote
    has, commits to pull, a git error), from the project index cache
    (never runs git; absent outside a repo or before the first pass).
    `git` (the line) and `git_warn` ("Not backed up: no GitHub repo")
    come from get_state.
  - `accounts`, `loops`, `jobs`, `archived`, `closed`, `modes`,
    `remote_control`, `listening_paused`: as in get_state.
  - `focused`: the uid of the terminal UI's focused tab.
  - `approvals`: what waits for a yes, the authority for an approvals
    list: `{id, kind, tab, question, choices, name?}` where kind is
    `confirm` (the assistant asks; id `confirm:<token>`, choices yes/no),
    `switch` (moving the assistant to another account; id `switch`) or
    `tab` (a tab at a permission prompt; id `tab:<uid>`, choices: those
    of yes, always and no its prompt has; none when GodTerm cannot read
    it, and the app offers Show to answer it in the tab).
  - each account also has `models`: its tabs' model bar `[{arg, label,
    desc}]` (empty for a CLI that picks its own model).
  - `assistant` `{on, busy, provider, account}`; `voice` `{mode (off,
    push, wake, open), muted, heard, action, web, speaking, awake,
    wake_word, barge_in}` (`web`: the client id of the browser that is the
    voice now, or null; `speaking`: a reply is being spoken; `awake`: the
    wake word's follow up window is open).
  - `mode_flags.direct` (the tab talked to directly, or null) and
    `mode_flags.brainstorm`.
- binary kind 1 `output`: raw PTY bytes of a subscribed tab, as the parser
  got them. Terminal queries GodTerm answers itself (cursor position,
  device attributes, XTVERSION, DECRQM, kitty `?u`, OSC 10/11/12/4 color
  queries) are taken out, so xterm.js never answers them a second time.
- `snapshot` `{tab, rows, cols, data}`: on subscribe, and again whenever
  the stream alone cannot carry a change (a new process, a rebuilt
  parser, a full screen app that ended, or output dropped because this
  browser fell behind).
  Reset the terminal to `rows` x `cols` and write `data`: up to 3000
  history lines as plain text, then the screen with colors, input modes
  (bracketed paste, application cursor, mouse modes) and the cursor. A
  full screen app's screen (Claude Code's fullscreen view, less, vim)
  comes after `ESC [ ? 1049 h`, on the browser's alternate screen, with
  no history.
  Output after it continues exactly where it ends (no gap, no repeat).
  `sid` names the tab's output stream and `pos` where in it the snapshot
  ends: a browser adds the length of every output frame for the tab to
  `pos` to know where its copy ends (for a resume).
- `resized` `{tab, rows, cols, sid, pos}`: the tab's PTY was resized (by
  any browser or the terminal UI), in order with its output: resize the
  terminal and go on writing output (no snapshot; the app redraws). The
  stream id changes, the position does not.
- `wheel` `{tab, taken: false}`: a `wheel` the tab's app does not take
  (the main screen without mouse reports).
- `resume` `{tab, sid, pos, rows, cols}`: the answer to a `subscribe` with
  `resume` that fits: the output after `pos` follows (only what the
  browser missed), with no snapshot. The browser keeps its terminal as it
  is. When its copy does not end at `pos` after all, it unsubscribes and
  subscribes again without `resume` for a snapshot.
- `assistant` `{id, role, text, done}`: one chat message; role is `user`,
  `assistant`, `tool`, `note` or `preamble` (text written before a tool
  call). Finished messages have numeric ids, in order, and `done: true`.
  The reply being streamed comes as `id: "live"`, `done: false`, its full
  text so far each time; it ends with an empty `live` (`done: true`) and
  arrives as a numbered `assistant` message.
- `assistant_reset` `{messages}`: the chat changed in a way that is not an
  append (a new conversation, a removed note): replace it all.
- `approval` (an approvals entry with `t`): once when it first appears,
  for a notification; it is gone when it is no longer in `state`.
- `answered` `{id, ok, result | error}`: the reply to an `answer`.
- `update` `{available, version, current, state, notes, page,
  update_with}`: GodTerm's own update news (update_info), sent on change:
  `available` true means "GodTerm <version> available"; `state` is the
  checker's phase ("available", "downloading 40%", "downloaded and
  verified, ready to restart", "up to date"...). To install, call the
  `restart_to_update` tool (it asks first; see `tool` below).
- `toast` `{text, level}`: a status bar message from the terminal UI.
- `result` `{id, result}`: the reply to a `tool` call: the tool's own JSON
  (`{"ok": true, "result": ...}` or `{"ok": false, "error": ...}`, or
  `{"ok": false, "needs_confirmation": true, "token", "question"}`).
- `error` `{error, id?, tab?}`: a bad frame, an unknown tab, input for a
  tab that is not running.
- `pong` `{id}`: the reply to `ping`.
- binary kind 2 `audio` (tab uid 0): the assistant's speech for the
  browser that has voice on, one WAV clip (PCM16 mono, the engine's rate)
  per frame, to be played back to back. Speech goes as a few clips per
  reply (the first after a third of a second of audio, then about a
  second each, the rest of a sentence at its end), never empty ones; the
  wake chime is a clip too. Nothing goes to the Mac's speakers meanwhile.
- `voice_level` `{db, ms}`: while the browser sends a recording (kind 4),
  the level of what GodTerm decoded, a few times a second, and the
  milliseconds decoded so far: the page's proof that sound arrives.
- `voice` `{set, ok, value | error}`: the answer to a `voice {set}`.
- `audio_stop` `{}`: the reply was cut off (a barge-in, a new reply):
  drop the clips queued.
- `ui` `{action, ...}`: the assistant changes this browser's view (its
  web_* tools, below): `layout` `{panes: 1|2|4, tabs: [uid...]}` (a
  phone keeps one pane and shows the first), `show` `{tab, pane?}`,
  `view` `{view: terminal|chat|sessions|manage|approvals|settings|accounts}`,
  `font` `{size}` or `{fit: true}`. The pane assignment it sets is kept
  per device (localStorage), as one picked by hand is.

### Browser to server

- `subscribe` `{tabs: [uid...], resume?: {"<uid>": {sid, pos}}}` /
  `unsubscribe` `{tabs: [uid...]}`. Subscribing again re-sends a
  snapshot. `resume` (after a lost connection: a phone back from another
  app) says where the browser's copy of a tab ends: when the stream is
  unbroken (no resize, new process or rebuilt screen since) and the tab
  still holds the bytes after `pos` (the last 256 KB of output, kept for
  15 minutes after the last browser left), GodTerm answers `resume` and
  sends only those bytes; otherwise a snapshot as usual.
- binary kind 1 `input`: bytes typed into a tab, written straight to its
  PTY (not through the UI thread). `input` `{tab, data}` as JSON works
  too (slower path, for text).
- `resize` `{tab, rows, cols, by?}`: this browser takes the tab's PTY size (the
  "take size" toggle, or a phone fitting the tab to its screen); the
  terminal UI's layout no longer resizes it, and its pane header says
  "sized by <by>" (a short device name). Typing into that tab on the Mac
  gives the size back to its pane. A browser that leaves gives its sizes
  back 20 s later, unless a browser takes them again before (a phone back
  from another app reconnects and does), so the tab is not resized twice
  and the phone resumes it without a snapshot.
  `{tab, release: true}` gives it back (so does disconnecting). Default:
  never sent; fit the tab's `rows` x `cols` and scale the font.
- `wheel` `{tab, up, n, col, row}`: `n` (1 to 64) wheel notches over
  screen cell (col, row), 0 based, sent to the tab's app as the terminal
  UI's wheel is: mouse reports in the encoding the app turned on (SGR
  1006, else the classic X10 bytes, UTF-8 1005), or arrow keys on the
  alternate screen when it reports no mouse. urxvt (1015) is not
  tracked by the parser and gets the classic encoding.
- `say` `{text, images?}`: a user turn to the assistant, exactly as typed
  in its panel. `images`: up to 6 pasted images as data URLs (PNG, JPEG,
  GIF or WebP, 20 MB each), attached as the panel attaches them: saved
  under the assistant's images, named in the text ("[image 1: 1280x720]"),
  sent to its model, and referable for tabs.
- `answer` `{id, choice}`: answer an approval from `state.approvals`.
  `tab:<uid>` presses the prompt's own option (yes, always or no) in that
  tab; `confirm:<token>` and `switch` say yes or no to the assistant, as
  its Yes and No buttons do.
- `tool` `{name, args, id, user_says?}`: any control tool, on the UI
  thread through the same path as the control socket, as client `web`
  (not the assistant: never counted against its turn, kept out of its
  chat). Replies come as `result` with the same `id`. A tool that needs a
  yes replies `needs_confirmation` with a `token` and a `question`: show
  the question, and on the user's yes call the same tool with only
  `{"confirm_token": token}` and `user_says: "yes"` (that is the user's
  turn saying yes; a no is `user_says: "no"` with the token too).
- `ping` `{id}`: answered with `pong` by the connection's reader (for
  latency checks). The page sends one every 20 s, and GodTerm pings every
  25 s whatever else it sends; a reader silent for 90 s is dropped.
- `image` `{id, data}`: an image (data URL, PNG, JPEG, GIF or WebP, 20 MB
  at most) for a tab: saved as the terminal UI saves a pasted image;
  answered `image` `{id, path, w, h}` (or `error` with the id). The page
  then pastes the path into the tab (`Image: <path>` for Grok), as an
  agent CLI takes an image file.
- `view` `{device: phone|desktop, panes, tabs: [uid|null per pane],
  focused, open}`: what this browser shows (sent on change). A turn typed
  in this browser (`say`) carries it to the assistant: its <state> gets a
  `web:` line naming the browser, its device and panes, and saying that
  layout words in this turn mean this browser's view (web_* tools), not
  the terminal UI's, unless the user names the terminal, the Mac or the
  GodTerm window.
- `voice` `{on: true, mode: "push" | "open" | "wake"}`: this browser becomes
  GodTerm's voice: its mic is the voice engine's input (the same engine,
  recognizer and turn handling as the Mac's mic) and the speech goes to
  it. The Mac's own voice mode is kept aside. `{on: false}` (or the
  browser leaving) gives the mic and speakers back to the Mac, in that
  mode. A second browser that turns voice on takes it over. Every mode
  works as from the Mac's mic: wake word (with the follow up window and
  the chime), open mic (with its auto sleep), barge-in (talking over a
  reply stops it; GodTerm decides on the browser's audio, which the
  browser already echo cancels), instant commands, direct mode and
  brainstorm.
- `voice` `{direct: uid | false}`: talk to that tab directly (as Ctrl-a K
  does), or back to the assistant. `{brainstorm: true | false}`: brainstorm
  mode on or off. `{hush: true}`: stop the reply being spoken (as talking
  over it does). `{away: true | false}`: the page went to the background
  or came back (logged; iOS stops the mic meanwhile).
  `{muted: true | false}`: the voice browser muted its mic (it then
  sends only silence and keeps its mode). Logged and in the state
  (`voice.web_muted`); while muted, open mic does not count the silence
  toward its auto sleep, and the count starts again at the unmute.
  GodTerm's own mute (`voice.muted`) is not touched. Another browser's
  word is ignored; a new voice session starts unmuted.
- `voice` `{set: {key, value}}`: change one of the voice settings the web
  app's voice sheet shows (talk back, engine and voice, speed, volume,
  wake words and sensitivity, follow up window, open mic sleep, barge-in,
  direct replies, instant commands, end of speech, recognizer, language,
  speaker lock): saved at once, as in the Settings screen. Other keys are
  refused.
- `voice` `{rec: type}`, binary kind 4 and `{rec_end: true}`: the mic as a
  recording (the web app's last fallback, below): `rec` starts one of
  that MediaRecorder type (audio/mp4 on iOS, audio/webm;codecs=opus
  elsewhere), kind 4 frames carry its pieces, and GodTerm decodes them
  with ffmpeg as they come into the same 16 kHz PCM as kind 3. `rec_end`
  decodes what is left before the frames after it are handled (a release
  of push to talk). A recording that cannot be read as a stream is
  decoded as a whole at its end.
- `voice` `{talk: true | false}`: press and release push to talk (mode
  push). A press from a browser that is not the voice (yet, or any more)
  makes it the voice in push to talk first. A release ends the capture
  whatever the engine reports, once the audio that came before it is
  read. The web app sends a quarter second more audio after a release,
  keeps the mic open between presses while voice is on (iOS asks for the
  mic again on every getUserMedia in an installed app), and turns voice
  off 90 s after the last press or speech.
- `voice` `{web_tts: {engine, voice} | null}`: the voice replies to this
  browser are spoken in (Voice settings > Voice in this browser, kept on
  the device as `gt.voice.webtts` and sent on every connect). Engines:
  `kokoro`, `say`, `grok`, and `chatterbox` / `ming` on Apple Silicon;
  `null` (or engine `mac`) is the Mac's own setting. A browser that never
  sent one hears Kokoro `af_heart`. It applies while that browser is the
  voice, from the next reply; the Mac's settings are not changed. Answered
  `{web_tts, ok}` or `{web_tts, ok: false, error}`. `{web_tts_list: true}`
  is answered with `{web_tts_list: [{engine, label, voices: [{id, label}],
  available, note}], default, mac, choice}` (an engine that cannot run
  here has `available: false` and why; speech then falls back through the
  Mac's chain). A browser switching mode (push to open) stays the voice:
  `moved` goes only to a browser another one took the voice from.
- `voice` `{debug: true | false}` and `{log: "..."}`: voice debugging.
  `log` lines go to GodTerm's log as `web: client N voice: ...`; `debug`
  turns on GodTerm's once a second lines (`voice debug: client N mic:
  ...` for the audio that came, `voice debug: engine read ...` for the
  endpointer). GODTERM_VOICE_DEBUG=1 turns those on from the start.
- `log` `{device, lines: [{at, cat, text}]}`: the page's debug log (see
  Debug log below): `at` ms since the epoch, `cat` a short category,
  `device` a label such as "iPhone Safari standalone 402x874@3". Sent in
  batches (every second or 50 lines); no reply.
- `push` `{op, id, ...}`: push notifications: the VAPID key, subscribe,
  unsubscribe, a test, and whether this page is visible (see Push
  notifications below). Answered with `push` `{id, ok, ...}`.
- binary kind 3 `mic` (tab uid 0): 16 kHz mono PCM16 little endian from
  the browser's mic, while voice is on. Audio that comes while the engine
  is not listening yet (its model loads on the first press) waits, and a
  capture that opens keeps what came in the last second; older audio is
  dropped. Each release is logged with what came (`web: client N released
  push to talk: 260 frames, 5200 ms of audio, level -30 dBFS ...`, with a
  note when nothing or only zeros came).

GodTerm answers `voice` frames that change something with `voice`
`{ok: true, on, mode}`, `{ok: true, on: false, moved: true}` (another
browser took it) or `{ok: false, code, error}` (refused, e.g. `muted`).
The state's `voice` also has `status` (off, starting, idle, listening,
transcribing) and `error` (why the engine stopped, or null).

The mic has one control: the mic in the header. A tap opens the mic
menu: Off, Push to talk, Open mic, Wake word ("Hey God", or the wake word
set), with the current one checked, then Direct to this tab (a switch:
`voice {direct}`) and Voice settings. The pick is kept on the device
(`gt.voice.pick`, Off by default). The header's mic shows it at a glance:
slashed and dim for Off, a PTT badge for push to talk, OPEN in green for
open mic, WAKE in blue for the wake word.

- Push to talk: a big Talk button stays on screen at the bottom right,
  above the key bar (the chat's own mic does the same in the chat). Hold
  it while you talk: it fills, says "Release to send" and shows the live
  level (a bar and dB). Nothing else listens.
- Open mic and Wake word: a strip under the header says so for as long
  as the mode is on ("Open mic · listening", "Wake word · say "Hey
  God"", hearing you, heard you with what was heard, thinking, speaking
  with Stop) with the live level as a bar and in dB (dBFS, e.g. -42 dB).
  A tap on it opens the mic menu (another mode, or Off). After a reload
  it offers Start (iOS needs a tap for the mic and the speakers).
- Mute mic: the slashed mic button on the strip, Mute mic in the mic
  menu (a switch) and in the voice sheet, or M on a keyboard (when the
  focus is not in a text field, the terminal or the chat; not while
  voice is Off). The mode stays; the capture track is disabled (so the
  browser's recording indicator shows it) and only silence goes to
  GodTerm, at the usual rate, so its engine keeps its timing and hears
  nothing; the silence check is skipped. The browser also sends
  `voice {muted: true | false}` (and `muted: true` again after a
  reconnect). The strip says "Open mic · muted" (in clay, no glow;
  "muted · speaking" with Stop while a reply plays) and the header's mic
  is slashed and says Muted. Replies keep playing. Push to talk while
  muted opens no mic and says how to unmute. Unmute listens again at
  once. Kept per browser (`gt.voice.micmute`); after a reload the strip
  still offers Start, muted.
- The agent's voice here: Agent voice in the mic menu and in the voice
  sheet ("Voice in this browser") is a speaker button (a tap mutes) and
  a volume slider (0 to 100 %, live). Every clip goes through one gain
  node, so a change applies at once, also mid reply. The strip has a
  small speaker button: a tap mutes or unmutes, and it shows the volume
  when under 100 % (or a crossed speaker when muted). Replies still come
  and show as text while muted here; the Mac's own volume and speaker
  mute (`voice.tts_volume`, `voice.speaker_muted`) are not changed. Kept
  per browser (`gt.voice.spkvol`, `gt.voice.spkmute`).
- The level shows whenever the mic is on. The page never says listening
  while no audio comes: before the first samples it says starting, and
  after 2 s with none (no samples, or no level from GodTerm for a
  recording) it says "no audio" in red.

A floating bar under the header says the rest: push to talk's listening,
sending and thinking, speaking, a reply that waits for a tap (iOS locks
audio output without a recent touch), and failures, which stay on screen
with what went wrong (no HTTPS, permission denied, no mic, the mic in
use, no audio or only silence from the mic, GodTerm refusing, the engine
stopping) until dismissed or voice works again.

The mic is read by the first of three methods that gives sound, chosen
automatically, logged and remembered on the device: an AudioWorklet on an
AudioContext made after the mic opened (on an iPhone home screen app one
made before can run without ever getting the mic's samples), then a
ScriptProcessorNode on a fresh context, then a MediaRecorder recording
GodTerm decodes. Each gets 0.7 s to give samples (2 s when they are only
zeros); the processing node goes to the destination directly, never
through a gain of 0 that Safari may optimise away. While no method is
known on the device, a recorder runs beside the first tries, so nothing
said meanwhile is lost when it takes over. A kept mic that gives nothing
on a press is built again after 0.6 s and opened again after 1.4 s.
`?micmethod=worklet|script|recorder|auto` forces one (remembered);
`?micbreak=worklet,script` makes those give nothing, to test the
fallbacks.

iOS stops the mic when the app goes to the background or the screen
locks: the strip says the mic is paused, and it comes back on return (or
asks for a tap on Resume). The voice sheet (Voice settings in the mic
menu, or the sliders button in the assistant panel) has the mode, direct
mode for the tab shown, brainstorm, the mic method with a live mic test
(method, frames a second, level) and the voice settings.

`?voicedebug=1` (remembered on the device, `?voicedebug=0`
turns it off) logs the browser's mic once a second to the console and to
GodTerm's log and turns GodTerm's debug lines on; `&voicetest=5` then
holds push to talk for 5 s after connecting, with no touch (for the iOS
Simulator).

### Installing (PWA)

The manifest has `id` and `start_url` `/`, standalone display, icons
(any and maskable), screenshots for Chrome's install sheet and a Web
Share Target: Share > GodTerm posts photos to `/share`, which the service
worker keeps in a cache and the page asks where to send (the tab shown,
the assistant, another tab). Chrome's install prompt is offered once and
under Settings > This browser > Install as an app; Safari gets a one time "Share > Add to
Home Screen" tip. The service worker serves the offline page when the
Mac cannot be reached.

### The top bar and Settings

Two rows, the same on a phone and a desktop (where labels sit next to
the icons):

- Row 1 says where you are: the drawer button, the tab shown with its
  state dot and a line of account, model and state (a tap opens the
  tab's options), the connection dot, the gear (Settings) and the
  approvals shield with its count. A swipe on it goes to the next or
  previous tab.
- Row 2 is the toolbar: the mic mode (opens the mic menu; says Off, PTT,
  Open or Wake), the assistant, Find (the tab list's filter), Sessions,
  New tab, Layout (desktop), Manage (usage, jobs, loops, closed and
  archived tabs) and, at the right end, the tab's own options. On a
  phone it scrolls sideways if it must; at 360 px it fits. While the
  phone keyboard is up only row 1 shows, and a phone on its side puts
  the two rows side by side.
- The open mic / wake word strip sits under row 2 while one is on.

### On a desktop: the tab panel and the status bar

- The Tab button (and a click on the title) toggles a docked panel at
  the right of the terminals with the shown tab's options (rename, pin,
  move, duplicate, copy session, model and effort, usage, reopen, color,
  group, loops and jobs, Git, Browser, talk directly, copy, keys,
  archive, close). Open or shut is kept per browser (`gt.tabpanel`). It
  follows the tab shown (another pane or tab picked), and its sections
  open inside it as a drill-down with a back arrow, never as a popup
  over the terminal; the live ones (the options, usage, model, loops,
  color, group, accounts to move to) paint again when what they show
  changes, not while typing or clicking in them. The terminals refit
  once when it opens or closes (no width animation). A right click or
  the row menu in the tab list still opens a popover for that row's tab.
- A slim status bar under the terminals shows the shown tab as the
  terminal UI's model bar and pane header do: the account with a usage
  bar and its percent left, the model and effort (and a queued switch),
  the browser (ready in green, a step missing in amber, none dim), its
  loops and jobs, git (branch, files changed, unpushed and behind, an
  amber warning when it is not backed up or not in git) and, at the
  right, its state. Each item opens its section: in the panel when it
  is open, else a popover above the item. It updates from the `state`
  frames.
- Effort (Claude tabs): low, medium, high, xhigh or max, sent to the tab
  as `/effort <level>` (send_prompt, so after its turn when busy).
  Other CLIs show theirs, set inside the CLI.
- Phones keep the bottom sheet for the tab's options and the key bar;
  neither the panel nor the status bar shows there.
- A tab's accent color (Tab options > Accent color: `set_tab_color`,
  its own color over its group's; the state's `color`) is the one the
  terminal UI draws as the bar before the tab in the tab bar and the
  tab list. The web app shows it as the row's left stripe in the tab
  list (the shown row keeps its background), a 3 px stripe at the left
  of the pane header, a short mark before the title's state dot (on a
  phone too), and a swatch with its name first in the status bar (a
  click opens the color section). The ten muted swatches (sage, sand,
  slate, clay, mauve, stone, moss, teal, rose, dusk) are CSS tokens
  (`--sw-*`, darker in the light theme); unknown names show nothing.

The gear opens Settings (a sheet on a phone, a wide panel with the
sections in a column on a desktop), every setting in one place with a
search box: This browser (theme, text size, fit text, Return key, the
voice replies here are spoken in, debug log, notifications, install),
Voice (mode, direct, the mic method and test, then GodTerm's voice
settings), Terminal (renderer, scrollback, take size, GodTerm's layout
and memory settings), Assistant, Accounts and usage, Jobs and loops, Web
app (port, tailnet sign in), General and About (version, build, update).
This browser's settings stay on the device; GodTerm's are saved with
`set_setting`, the tap being the user's own request (`user_says`), with
the same second yes for risky ones as everywhere.

### The tab list

The side list on a desktop, the drawer on a phone (web/js/tabs.js):

- The filter box (fuzzy over name, folder, account, id, group, model and
  label), then Sort (Tab order, Name, State, Folder) and Group: Account
  (the default: a header per account with its usage bar and tab count),
  Agent (Claude, Grok, Codex, OpenCode...), Folder (sorted by path; rows
  then leave the folder out), Status (Needs you, Working, Ready, Other)
  or None (one flat list in the sort's order). Pinned tabs come first in
  each group. Both are kept per browser (`gt.tsort`, `gt.tgroup`). The
  header swipe and the next / previous tab follow the same order.
- The status chips (All, Working, Needs you, Ready, Pinned) are a strip
  that scrolls sideways: a finger swipes it with momentum (the drawer's
  close swipe ignores touches that start on it), and on a desktop a
  mouse drag or the wheel scrolls it; a drag is never taken as a chip
  click. No scrollbar; an edge fades while there is more that way.
- Desktop: the list's right edge is a handle. Drag it (200 px to half
  the window), double click for the default 280 px, or focus it and use
  the arrow keys. The width is kept per browser (`gt.sidew`). The
  terminals refit once, when the drag ends, not on every pixel. The
  phone drawer has no handle: it stays `min(88vw, 360px)`.
- While the filter box has text, the list has three sections, each with
  its count (web/js/find.js): **Open tabs** (the local filter above, at
  once), then **Archived** (`list_archived {text}`; a row says Archived
  and a tap runs `restore_archived` and shows the restored tab), then
  **Other sessions** (the `sessions` tool over the server's session
  index, never a disk scan: every Claude account's config dir and the
  main `~/.claude`, Grok, Codex slots' `CODEX_HOME` and the shared
  Cursor store; main sessions only, never subagent or sidechain
  transcripts). A session row shows its title, harness, account, last
  activity and folder; a tap resumes it in a new tab like the Sessions
  screen (`open_session`, or `take_over_session` for one running
  elsewhere). Sessions already open in a tab or archived are left out
  of it. The two server queries go out 150 ms after the last key (a
  stale reply is dropped); Archived shows 5 and Other sessions 8, and
  Show more adds 20. Antigravity and OpenCode keep their sessions in
  their own databases and are not indexed.

### On a phone

- The page never scrolls: the body is fixed, a scroll iOS makes to show a
  focused field is undone at once, and the app is sized to the visible
  viewport (followed every frame while the keyboard slides), so the key
  bar sits on the keyboard. The terminal's hidden input sits under the
  header, not at the cursor, so focusing it gives iOS nothing to scroll.
- A swipe left or right on the header goes to the next or previous tab
  (the drawer's order). The last three tabs' terminals are kept hidden
  (without their GL context): going back to one shows it at once and
  resumes only the output it missed.
- A new approval's banner floats above the key bar with No and Yes; it
  stays until answered (here or elsewhere) or dismissed. Other messages
  float there too, so the terminal does not refit for them.
- In a tab, the phone keyboard's Return makes a new line in the prompt
  (Settings > This browser > Return key: New line, the default on phones,
  or Send): ESC CR for Claude Code and agents without a known new line
  (what the terminal UI sends for Shift+Enter), Ctrl+J (LF) for Codex
  and OpenCode. Send, fixed at the right end of the key bar outside the
  strip that scrolls, sends Enter (CR) and keeps the keyboard up. On a
  desktop Shift+Enter sends that new line too (xterm alone sends CR).
  The Prompt box's "Press Enter after it" pastes the text and sends the
  Enter as its own write once the tab shows it (at most 1.5 s later).
- The phone keyboard's text goes to a tab as what changed in xterm's
  hidden textarea (web/js/textinput.js), never as what each input event
  carries: iOS dictation replaces the phrase so far with every partial
  result ("the", then "the interaction", ...), and autocorrect replaces
  the last word, so xterm alone sent every partial result in full. The
  page keeps what it sent since the last key that is not text (Enter,
  an arrow, Tab, Esc, a key bar key, Send) and sends one DEL (0x7f) per
  character after the common start, then the new end. A composition
  (marked text: an IME, some dictation) goes once it ends, or when a
  key or Send comes first. Paste goes once, bracketed when the app asks.
  Backspace in an empty textarea sends DEL. `web/dev/dictation.mjs`
  replays the iOS event sequences in WebKit and checks every character
  reaches the tab once.
- The composers' return key makes a new line (Send sends); drafts are
  kept on the device until sent (`gt.draft.say`, `gt.draft.p.<tab>`).
- Below 560 px of height (a phone on its side) the phone layout stays.
- Fit text to this screen (Settings > This browser, `gt.fitscreen`; on by
  default on phones): while the phone shows a tab and was touched or
  typed on in the last 5 minutes, it takes the tab's size (`resize` with
  `by: "iPhone"`) at its own font, 14 px by default (about 50 columns on
  a 430 px iPhone). A-, A+ and a pinch change that font for the device
  (`gt.fontpref`) and the tab rewraps to it; Reset goes back to 14 px.
  The font is never scaled down then. The keyboard does not change the
  rows (the pane pans to the cursor), nor do sizes measured within 0.9 s
  of it coming or going (iOS drops the focus before the viewport grows
  back). Zoom steps resize the tab once, 600 ms after the last one. No
  resize goes out while the tab is scrolled back (its history here, or
  the app's own after wheel notches); it goes when back at the end (Jump
  to the end, typing). The size goes back when the phone
  shows another tab, is idle for 5 minutes, hidden for a minute, or
  leaves, and when the Mac's user types into that tab (the terminal UI's
  pane header says "sized by iPhone" meanwhile); the phone then keeps its
  font and pans until it is used there again. Off, the tab keeps its
  size and the font is scaled down to fit it, as on a desktop.
- Scrolling with one finger: an app that scrolls itself (alternate
  screen, or mouse reports with no history here: Claude Code's
  fullscreen view, less --mouse, vim with mouse=a) gets `wheel` notches,
  one per row of drag at the cell under the finger, sent once a frame,
  with momentum after a flick; PgUp / PgDn on the key bar go to it as
  PageUp / PageDown (Claude Code pages its transcript with them). A
  screen with history scrolls this terminal's own copy, and new output
  never pulls it down (Jump to the end does). While an app is scrolled
  up, Jump to the end sends the notches back down; typing ends it too.
- The browser's terminal ignores "erase saved lines" (CSI 3 J), as
  GodTerm's own screen does: Claude Code sends it with its redraws, and it
  wiped the history the snapshot brought, so a phone could not scroll
  back. 2 J still clears the screen. The debug log notes each snapshot's
  history size, scrolls (local history: buffer and position; wheel to
  the app: mouse mode, encoding and notches up and down), resizes held
  while scrolled back, and kept clears.

### Push notifications

Web Push (src/web/push.rs, js/push.js, sw.js): a device that turned it
on is notified when a tab starts waiting for approval, when a turn ends
(the same "finished" the tab list shows: working for 1.5 s or more, then
ready, background work done), when a tab ends with an error (a nonzero
exit), and when the assistant asks for a yes. The notification says only
the tab's name and what happened ("api-server: waits for approval",
"docs: finished", "docs: ended with an error", "Assistant: asks for a
yes"), titled GodTerm, never the question or any screen text.

- Turning it on: Settings > This browser > Notifications. The permission
  prompt comes only from that tap; then `pushManager.subscribe` with
  `userVisibleOnly` and GodTerm's VAPID key, and the subscription goes to
  GodTerm. Under it: Approvals, Finished turns and Errors (what this
  device wants, all on at first) and Send a test notification. When it
  cannot work the row says why: a page that is not https (the tailnet IP
  link is plain http: use the `tailscale serve` link), an iPhone or iPad
  page that is not installed (iOS gives push only to Home Screen apps,
  iOS 16.4 or later), a browser without push, or permission denied
  (Settings > Notifications > GodTerm on iOS).
- When: at most one push per tab per kind every 30 s. A device whose
  page is visible now is skipped: the page sends `visible` on every
  visibility change and every 15 s while visible, and it counts for 40 s
  (iOS can freeze a page without a word). What already waited when
  GodTerm started is not pushed.
- Sending: a thread of its own (`web-push`) reads the files, encrypts and
  posts; the UI thread only compares tab states. Each push is aes128gcm
  (RFC 8291: an ephemeral P-256 key and a random salt per message, one
  record of 4096) with a VAPID header (RFC 8292: `Authorization: vapid
  t=<ES256 JWT>, k=<public key>`, the JWT's `aud` the endpoint's origin,
  `exp` one hour ahead, `sub` `https://godterm.com`), `TTL: 3600` and
  `Urgency: high` for approvals (`normal` otherwise). Timeouts: 5 s to
  connect, 10 s in all. A 404 or 410 means the push service dropped the
  subscription: GodTerm forgets it. Every push is logged (`web push:
  sent finished to <device> (201)`), never the endpoint.
- Files, in the app home (`~/.godterm`, `~/.godterm-demo/app` in demo
  mode), both 0600: `web-push-vapid.pk8` (the VAPID key, PKCS#8, made on
  first use; a new one means every device has to turn notifications on
  again, which the page does by itself on its next connect) and
  `web-push-subs.json` (`{subs: [{endpoint, p256dh, auth, device,
  created, kinds}]}`, one per device, at most 20).
- The payload: `{title, body, tag, url, kind, tab}`. The service worker
  shows it with the app icon and `tag` (`approval-12`, `finished-12`,
  `approval-assistant`), so a newer one for the same tab and kind
  replaces the last. A tap focuses the open app and posts it the `url`,
  or opens the app at it: `./#tab=12` shows that tab, `#approvals` opens
  the approvals sheet (`./#tab=12&approvals` for a tab's approval). The
  page takes the hash once the state is there and clears it.

Frames: `push` `{op, id, ...}` both ways; GodTerm answers with `push`
`{id, ok, ...}` (or `{id, ok: false, error}`).

- `{op: "key"}`: `{key}`, the VAPID public key (65 bytes uncompressed,
  base64url), the `applicationServerKey`.
- `{op: "subscribe", sub, device, kinds?}`: store this device. `sub` is
  the PushSubscription's JSON (`{endpoint, keys: {p256dh, auth}}`; the
  endpoint must be https, p256dh 65 bytes, auth 16); `device` a label
  such as "iPhone Safari standalone 402x874@3"; `kinds` any of
  `approval`, `finished`, `error`. A known endpoint keeps its kinds when
  `kinds` is left out (the page sends it again on every connect).
  Answer: `{subscribed: true, sub, kinds}`, `sub` a 16 hex id for the
  device (the endpoint itself stays on the Mac).
- `{op: "status", endpoint}`: `{subscribed, sub?, kinds?}`.
- `{op: "unsubscribe", endpoint | sub}`: `{removed}`.
- `{op: "test", sub}`: a test notification to that device, answered once
  the push service did (`{ok: true, status: 201}`) or failed (`error`).
- `{op: "visible", on, sub}`: this page is visible (or not); no answer.

### Theme

Settings > This browser > Theme: Dark (the default), Light or Match system,
kept per device in localStorage (`gt.theme`). One theme covers the whole
app; the terminal stays dark in both, and in Light it sits in a light
frame. `js/theme.js` is a plain script in `<head>` (the page's policy
allows no inline script) that sets `data-theme` on `<html>` before the
first paint, points the `theme-color` metas and iOS's status bar style
at the header's color, and follows the system live under Match system.
The sign in page (through `signin.js`) and the offline page (inline: it
is served without the policy) read the same choice.

### Debug log

To see what happens in a browser (a phone above all), the page reports
to GodTerm, which writes `<app home>/web-client.log`
(`~/.godterm/web-client.log`, `~/.godterm-demo/app/web-client.log` in
demo mode): made 0600, rolled over to `web-client.log.1` at 4 MB (one
old file kept). One line per event: local time (the browser's clock),
client id, device, category, text:

    2026-10-25 14:03:22.123 c3 [iPhone Safari standalone 402x874@3] conn: down code 1006

Read it on the Mac with `godterm web-log [--follow] [--client N]
[--lines N]` (`--demo` for the demo's; `--follow` keeps printing new
lines, across a rollover).

- What `js/debuglog.js` sends: console log, warn and error (still shown
  in the console), uncaught errors and rejections, the socket (open, close
  codes, retries, ping round trips, `hello`), visibility, online/offline,
  viewport and visual viewport sizes and the on-screen keyboard (settled
  for 400 ms), theme changes, and every 2 s a summary of taps and swipes
  by area and keys: special keys by name (Esc, Tab, arrows, Ctrl+C, key
  bar buttons by label), typed text only as "typed N chars". Never text,
  clipboard contents, audio, tokens or cookies. It starts with the user
  agent, screen size, dpr and display mode. Other modules call
  `dlog(cat, text)` (`import { dlog } from './debuglog.js'`).
- While the socket is down lines wait, up to 500 lines or 64 KB (the
  oldest go first, with a "N lines dropped" note).
- On by default; Settings > This browser > Debug log turns it off on that
  device (localStorage `gt.debuglog`). Send debug report there logs a
  `report` block: build, connection, viewport, theme, panes with their
  tabs, PTY and xterm sizes, font and cell metrics, renderer, the
  subscribed tabs and the last errors.
- The server (src/web/clientlog.rs) scrubs every line again: control
  characters go, text is capped at 2000 characters, the web token, any
  32+ hex run, long base64 like runs, `t=` query values and Cookie,
  Authorization, `godterm_web` and Bearer values become `[redacted]`
  style marks, and categories `typed`, `input`, `paste`, `clipboard`,
  `audio` and `mic` keep only their length. Each client gets a burst of
  1000 lines (256 KB), refilled at 20 lines (16 KB) a second; more is
  dropped and counted in a note. A thread of its own writes the file;
  the socket's reader never waits on the disk.

### Tools the web app uses

All through `tool`. The tab tools take `tab` as an id ("t12"), a list, or
a word ("all", "waiting"...).

- Sessions: `sessions` `{text?, harness? (claude|grok|all), source?
  (account number or name, "main", "all"), since?, until? ("today",
  "3 days", 2026-10-01), project?, state? (active|recent|all), sort?
  (modified|created|messages|tokens|context|project|source|title|oldest),
  reverse?, limit?, offset?, grouped?, include_subagents?,
  include_headless?}`. From the web app `limit` goes up to 2000 (the
  assistant's 50 cap does not apply), `offset` pages, and `grouped: false`
  keeps the requested sort (default: Claude first, then Grok). Reply:
  `{sessions: [{id, title, project, source, harness, last_activity,
  first_prompt, messages, tokens, largest_context, cost_usd, live,
  open_in_tab?, subagents?, external?, saved?}], matched, shown, offset,
  headless_hidden, indexing, did_you_mean?}`; `indexing: true` means the
  first scan still runs (call again shortly). Scripted runs and sessions
  in temp folders are hidden unless `include_headless`.
- Session details: `session_detail` `{id}`.
- Resume a session: `open_session` `{id, account?, name?}` (a main
  ~/.claude or ~/.grok session is copied into `account` first);
  `take_over_session` `{id}` for one open in another terminal.
- Tabs: `open_tab` `{account?, name?, dir?, prompt?}`, `close_tabs`
  `{tab}` (asks first for several or busy tabs), `rename_tab` `{tab,
  name}`, `move_tab`, `pin_tab`/`unpin_tab`, `set_tab_color`,
  `group_tabs`, `archive_tabs`, `restore_archived`, `reopen_closed`,
  `send_prompt` `{tab, text}`, `answer_prompt` `{tab, choice}`.
- Jobs and loops: `list_jobs`, `create_job`, `update_job`, `delete_job`,
  `run_job_now`, `stop_loops`.
- Update: `update_info`, `restart_to_update` (asks first).
- Settings: `list_settings`, `get_setting`, `set_setting` `{key, value}`.
- The assistant's tools for the web app: `web_layout` `{panes, tabs,
  client?}`, `web_show` `{tab, pane?, client?}`, `web_view` `{view,
  client?}`, `web_font` `{size | fit, client?}`. Each sends a `ui` frame
  to the browser the request was typed in (or `client`: an id, or
  "all"); with no web turn and one browser open, to that one.
- Everything else in control.rs's TOOLS works the same way.

## Modules (build order)

1. Core server: config, token, static files, WebSocket, state stream,
   tab subscribe with snapshot plus live output, input, resize.
2. Web app shell: tab list (accounts, status dots, search), terminal view
   with xterm.js (WebGL renderer, fit, unicode11, web links), mobile key
   bar, desktop split panes, PWA manifest.
3. Assistant: chat panel (markdown), composer, approvals, toasts.
4. Management: new tab, close, rename, move, archive, sessions search,
   jobs and loops, usage meters, settings that are safe to change.
5. Voice: push to talk and open mic from the browser into the
   recognizer, assistant speech played in the browser, voice modes.
