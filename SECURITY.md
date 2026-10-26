# Security policy

## Reporting a vulnerability

Please report security problems privately through GitHub's private
vulnerability reporting on `daniel-farina/godterm` (Security > Report a
vulnerability), not in a public issue. You can expect an answer within 3
working days, and a fix or a plan within 14 days for anything rated high.

## Supported versions

Only the latest release gets security fixes before 1.0.

## Threat model

GodTerm runs on your own machine, as you. What it defends against, and
what it does not:

- **Other local users** (another account on the same Mac or Linux box) are
  untrusted. `~/.godterm` is 0700 and its files 0600, the control socket
  is 0600, nothing listens on the network except the local speech server
  on 127.0.0.1 (and GodTerm only adopts it once the listener on that port
  is its own child process) and the web app when it is on (see below), and
  secrets never go on a command line, where `ps` would show them to
  everyone.
- **Text GodTerm did not write is data, not instructions.** Tab screens
  and replies, transcripts, files, web pages, CLI output and release notes
  may carry prompt injection. The assistant's prompt says so in a locked
  section, and the code enforces the important parts:
  - anything that changes much (closing or archiving several tabs,
    installs, logins, settings, jobs, prompt edits, an update restart)
    needs a confirmation the brain cannot give itself: the question is
    asked in one turn and runs only on the user's own clear yes in a later
    turn (`control::planned`, `restart_confirmed`);
  - a turn GodTerm starts itself to read out a tab's answer may look and
    speak, never change anything (any other tool is refused);
  - learned rules and prompt edits that come from tab or file text are
    refused.
- **Agents inside tabs** are only partly trusted. They run as you, with the
  permissions you gave their CLI, so they can read your files, including
  everything under `~/.godterm`. GodTerm limits what they can do *through
  GodTerm*:
  - each tab's own `godterm mcp` has only its tab's job and browser tools,
    and every job change is approved by you in a dialog;
  - the assistant's read-write control token works only from the
    assistant's own process tree (the socket peer's pid is checked on macOS
    and Linux), so a tab agent that reads it from
    `~/.godterm/assistant/mcp.json` cannot drive other tabs with it;
  - every other client gets the read-only token from `control.json`
    (unless you start GodTerm with `--control-rw`).
  Being the same user, a tab agent can still read other tabs' transcripts
  in the CLIs' own folders, and could see another tab's job token in
  `~/.godterm/tab-mcp`; that only lets it ask for jobs on that tab, which
  you approve or refuse. Run untrusted code in a sandbox or another user.
- **The release channel.** Updates are trusted only through the minisign
  signature on `SHA256SUMS` (public key built into the binary) and, on
  macOS, the Developer ID and Team ID check. The signed comment names the
  version, so an old signed build cannot be served under a newer tag; a
  release without a valid signature for its own version is refused.
- **Out of scope:** root, malware already running as you, a compromised
  Claude Code / Grok / Codex / Cursor / OpenCode CLI, and the services
  those CLIs talk to.

## What GodTerm handles

- **Claude account logins.** GodTerm never stores OAuth tokens itself. Each
  account is a Claude Code config folder; claude keeps its login in the
  macOS login keychain (service `Claude Code-credentials-<hash>`) or, on
  Linux, in `<folder>/.credentials.json`. GodTerm reads them only to show
  plan usage, and writes keychain items only in `godterm migrate-accounts`
  (through `security -i` on stdin, never on a command line). It never
  touches the default `~/.claude` login. The xAI key lives in the keychain
  the same way.
- **Long-lived account tokens** (`claude setup-token`, from the web
  app's Accounts screen or the palette). The token is taken out of the
  setup-token tab's output before the terminal parser sees it (the tab is
  320 columns wide so it never wraps; every token character becomes a
  dot), so neither the terminal UI, a web snapshot nor the tab's live
  output to a browser ever carries it. It is stored only in the macOS
  login keychain (generic password, service `godterm-token`, account =
  the GodTerm account's name, written through `security -i` on stdin),
  never in config, state, logs or a WebSocket frame, and not in the sync
  unless the user turns token sync on (off by default, a confirm, then
  per token): then a marked token goes to the state repo only when GitHub
  says that repo is private right before the push (docs/SYNC.md 2.8), and
  another machine with token sync on puts it in its own keychain.
  Anyone who can read the state repo can then use such a token.
  (`account-tokens.json` holds those choices and a short hash per token,
  never a token, and is on the sync's never list.) Browsers get that one exists, its date and its
  last four characters. It is read back only to start that account's
  assistant (CLAUDE_CODE_OAUTH_TOKEN for that one process, when the user
  turned "Use it for the assistant" on) or to copy it to the Mac's
  clipboard after a confirm. Deleting it removes the keychain item only;
  the token stays valid until revoked at claude.ai or a year passes. The
  log masks any `sk-ant-` run, whoever writes it. Same user processes can
  read the item with `security` or see the variable of that process (see
  Known limits). The assistant has no tool that reads, copies or moves a
  token.
- **Logins from the web app.** Log in runs the CLI's own login in a tab of
  that account; the sign in URL is shown to the browser and a code the
  CLI asks for is typed into that tab from the browser (never logged).
  Log out, remove, copy and delete a token need a confirm the browser
  answers with a one time id (5 minutes); removing an account never
  deletes its folder unless the user also ticks that and says yes a
  second time (it goes to the Trash, only for folders under
  `~/.godterm/accounts`). These go in `accounts` frames, not control
  tools, so the assistant cannot send them; it may still start a login
  (relogin_account, with its own confirmation) and read login state.
- **The control socket.** `~/.godterm/control.sock` (mode 0600; with a very
  long home path, a socket in your own temp folder) takes one JSON request
  per line. Two random tokens are made per launch: read-write for the
  assistant only (bound to its process tree), read-only for everything
  else, written to `control.json` (0600). `user_says` (a stand-in for the
  user's own confirmation) is refused unless GodTerm runs in test control
  mode.
- **The web app** (off unless `[web] enabled`; docs/WEB_APP.md). It
  listens on 127.0.0.1 and, by default, on this Mac's tailnet IPv4 (plain
  http; peers outside Tailscale's ranges are dropped). A browser is the
  user: it can type into every tab and answer confirmations. It signs in
  with the token in `~/.godterm/web-token` (0600) or, with
  `[web] tailnet_sign_in` (default on), with no token when its tailnet
  login is the owner of this Mac's tailnet node or in
  `[web] tailnet_users`: from `tailscale serve`'s identity header (trusted
  only on loopback, a `*.ts.net` Host, and a client end no process of this
  user holds) or from `tailscale whois` on the tailnet address. So **any
  device logged into your tailnet account can open it with no token**;
  anyone who can log into that account, or a device of yours that is
  compromised, is you to GodTerm. Other tailnet users (devices shared in
  from another account) and tagged devices are refused. Host and Origin
  checks stop DNS rebinding and cross site WebSockets, and a login or
  socket from a process GodTerm started is refused.
- **Files.** `~/.godterm` and its files are owner only; config, state,
  jobs, archive and indexes are written through a private temp file and a
  rename. State and the assistant's history hold folders, session ids and
  prompt text (private, but no keys or tokens).
- **Logs.** `~/.godterm/godterm.log` (0600, rotated at 2 MB) records
  actions and the prompts sent, not secrets: tokens and OAuth credentials
  must never be logged, and opened links are logged without their query
  string. Privacy mode hides account emails on screen.
- **Voice.** Local by default: audio is processed on the machine
  (whisper.cpp, Apple on device recognition, Kokoro). Grok voice is opt in:
  Grok recognition sends microphone audio to xAI and Grok talk back sends
  the reply text to xAI. Debug utterances, when enabled, stay in
  `~/.godterm/voice/debug` (newest 20).
- **Opening things.** `open_path` opens only http(s) links and documents or
  folders inside your tabs' folders and known projects, never programs, app
  bundles or files marked executable, and never the private folders
  (`~/.ssh`, `~/.godterm`, the CLIs' login folders and the like). "Open in
  Finder" reveals an app bundle instead of launching it.
- **Sync (opt in, docs/SYNC.md).** `godterm sync init` backs this
  machine's state up to a private GitHub repo it creates only after a
  typed yes; whoever controls that GitHub account can read the synced
  config, .env files and conversations (stored as is). Collection is an
  allowlist: OAuth logins, `.credentials.json`, `.claude.json`, keychain
  items, control and web tokens, shell snapshots, session-env and caches
  are never read, and nothing in `/Volumes/Private` (also through
  symlinks, mounted or not). Pulled config cannot set `*_bin`, `[web]`,
  `[supervisor]`, `[update]`, `[sync]`, `pass_env` or `args` with shell
  syntax; pulled jobs arrive paused. gh tokens are read per call by a git
  credential helper (never on a command line, logged or stored; `gh auth
  switch` is never run). No sync push is ever forced, and the sync only
  writes its own repos: it never commits to, pushes or creates a project
  repo (projects without a GitHub backup are only listed as warnings).
  Conversation ownership is a lease on the state repo's `leases` branch
  (compare and swap by a plain push, never forced): whoever can push
  there can take a conversation over, and a machine that loses one
  stops writing it and keeps its unsent lines as a fork, so nothing is
  overwritten. Taking a live conversation over needs the user's own
  press and yes (the assistant's take_over asks, and the web buttons'
  sync_lease refuses the assistant).
- **Release integrity.** macOS builds are signed with a Developer ID and
  notarized by Apple. Every release ships SHA256SUMS and its minisign
  signature (see Threat model).

### Project repos (Git section)

- Nothing changes a project repo without the user: commit, push, pull,
  branch switch or create, binding and repo creation run only from the
  Git section's button plus its confirmation (TUI key or the web sheet's
  `git` frame, which is not a control tool). The assistant's git tools only
  read or prepare. `[git] fetch_every_min` (off by default) is the only
  background git, and it only fetches.
- No force push anywhere: every git argument list passes
  `git_ops::forbidden` (a closed set of subcommands; no `--force*`, `+` or
  `:` refspecs, `--mirror`, deletes, `--amend`, rebase or reset), checked
  by a property test over random names.
- Secrets: every push is scanned first (gitleaks when installed, else
  built-in patterns over the outgoing commits); findings block it and only
  the human can pass them by typing "push anyway". Values are shown
  redacted. Tracked .env and key files block too; a staged symlink into
  the vault or a private folder, a file over 95 MB or a push over 1.5 GB
  cannot be passed.
- Credentials: an https GitHub remote gets `godterm git-credential
  --account <acct>` as its only helper for that call; the token comes from
  `gh auth token --user <acct>` and goes to git's stdin only (never argv,
  log or disk). gh calls get it as GH_TOKEN. `gh auth switch`, login,
  logout, public or internal repos and deletes are refused. Repos are
  created `--private` and checked PRIVATE afterwards.
- `/Volumes/Private` is a private folder: never read, bound, scanned or
  followed into (symlinks are resolved one step at a time and refused at
  the vault without looking inside it).
- Known limit: a tab's own agent with shell access could still run git
  itself; the Git section's rules cover GodTerm, not the agents' shells.

## Known limits

- Same-user processes are not isolated from each other by the OS; the
  token binding above raises the bar for tab agents but is not a sandbox.
- The web app trusts your Tailscale account: tailnet sign in is as strong
  as that account's login and the devices on it. Turn it off
  (`[web] tailnet_sign_in = false`) to need the token everywhere.
  Another local user could send serve's identity header to 127.0.0.1
  themselves (lsof cannot see their processes); only a request on a
  `*.ts.net` Host with an allowed login gets in that way, so on a shared
  machine turn tailnet sign in off.
- A long-lived token in the keychain is readable by any program running
  as you that runs `security find-generic-password -s godterm-token -w`
  (the item trusts the `security` tool that wrote it), and the assistant's
  process carries it in its environment while it uses it. Delete and
  revoke tokens you no longer need.
- The local speech server has no authentication of its own; another local
  user who finds its port can send it audio to transcribe (it never sees
  yours: GodTerm checks the listener is its own child).
- The CLIs GodTerm hosts get the permissions you give them (bypass mode
  included); GodTerm cannot make a CLI safer than its own settings.
