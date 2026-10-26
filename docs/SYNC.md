# GodTerm Sync: project repos, machine state, conversation ownership

Status: approved design (2026-10-25). Phases 1-3 below. Phases 2 and 3 are built (see "Phase 2 as built" and "Phase 3 as built" at the end).

**User rule (binding for all of Sync):** GodTerm, the assistant and every background worker NEVER commit to, push or create a project repo on their own. Project repo actions happen only from an explicit user action in the UI (the TUI or web Git section, a button plus its confirm dialog), never from an assistant tool alone and never automatically. Every project is part of the machine state (bindings, .env, conversations) whether or not it has a repo; a project with no repo, no GitHub remote, a remote that is only a local folder, unpushed commits or uncommitted work shows a clear warning ("not backed up: no GitHub repo", "12 unpushed commits", "uncommitted changes") on the Sync screen and in project lists instead of being skipped. GodTerm's own state and conversation repos are synced automatically once the user ran `sync init` or `sync clone`.
Status: approved design (2026-10-25). Phases 1-3 below.

Facts that shaped it (measured on the first machine it was built for):
- Transcripts are about 59 GB of JSONL (4 accounts x 5-19 GB); about 48 GB is `subagents/` folders; 48 files are over 100 MB (GitHub rejects those); about 57 GB touched in the last 30 days; image heavy (160 MB gzips to 105 MB). So: monthly conversation repos, append-only chunks, a sync window.
- Account dirs hold unsafe files next to transcripts: `.claude.json`, `shell-snapshots/` (exported env vars), `session-env/`, `sessions/`. Collection uses an ALLOWLIST, never a skip list.
- `/Volumes/Private` is missing from `guard::private_dirs()` in `src/guard.rs`. Phase 1 adds it.
- `gitleaks` is used when installed; `git-lfs` and `age` are not needed. Crates available: regex, glob, walkdir, uuid, toml_edit, sha2, ring.
- gh uses https and its accounts sit in the keyring: read one token per account with `gh auth token --hostname github.com --user <acct>` through GodTerm's own credential helper. NEVER run `gh auth switch`.
- A machine may have several gh accounts (a personal one, a work one with its own org, ...); each owner is pushed by its own account.

## Phase 1 as built (read this first)

The user's rule overrides parts of the design below: **GodTerm never commits,
pushes, pulls, switches branches, binds a folder or creates a repo on its
own.** Every one of those runs only when the user presses the button in a
tab's Git section (terminal UI: tab options > Git...; web: tab menu > Git)
and confirms the plan the section checked.

- **Not offered:** `auto_sync` and `wip_backup` (sections 1.1 and 1.7) are
  not built and not in the bindings file. Nothing touches a project repo in
  the background except a read only `git fetch` of the open tabs' repos,
  and only when the user sets `[git] fetch_every_min` (0, the default, is
  off).
- **Assistant tools** only read (`git_status`, `git_log`, `git_diff`,
  `git_branches`, `git_bindings`) or prepare something for the user
  (`git_draft_commit` puts a message in the commit box,
  `git_propose_binding` shows a proposed owner/repo/account). There is no
  `git_commit`, `git_push`, `git_pull`, `git_sync`, `git_branch`,
  `git_bind` or `git_create_repo` tool, with or without a confirm token
  (tested in src/gitpanel_tests.rs).
- **Warnings:** a project with no GitHub repo (no remote, only a local
  folder remote, or not in git), unpushed commits, commits behind, or
  uncommitted changes shows ⚠ in the tab list (TUI and web) and the words
  in its Git section: "Not backed up: no GitHub repo", "12 unpushed",
  "uncommitted changes". `get_state` carries the same as `git_warn`.
- **The web sheet** talks through `git` WebSocket frames (`op`: `view`,
  `prepare`, `run` with the `plan_id` the prepare returned, plus `typed`
  for "push anyway"). It is not a control tool, so the assistant cannot
  send it.
- **Diverged:** a push or pull that finds both sides ahead is reported;
  "Push as a branch" (the user's choice) pushes the local commits to
  `godterm/<machine>/<branch>-<yyyymmdd>` (`-2`, `-3`... when taken).
  Rebase is not offered in Phase 1.
- **No defaults with anyone's names are shipped.** Owners and rules come
  from your config. For example:

```toml
[git]
undefined = "ask"
repo_name = "folder"

[git.owners]
"alice"      = "alice"
"alice-labs" = "alice"
"acme-org"   = "alice_acme"

[[git.rules]]
match = "~/labs/**"
owner = "alice-labs"

[[git.rules]]
match = "~/acme/**"
owner = "choose"
choices = ["acme-org", "alice"]
state = "exclude"

[[git.rules]]
match = "~/**"
owner = "alice"
```

### Setting accounts and rules in the app

Settings > GitHub accounts and repos (the web Settings screen) and
Settings > GitHub (the terminal Settings view, Enter opens the editor)
edit the same three things:

- **Owners:** GitHub owner (user or org) to the gh account that pushes
  for it. The account box offers what `gh auth status` lists (read only,
  names only, tokens are never read or shown) and marks each owner's
  account as logged in or not. Add, edit, remove.
- **Folder rules, in order:** a glob (with a picker of the known project
  folders), an owner or "choose per project" with the owners to choose
  from, and "keep this folder's conversations and .env out of the state
  repo" (`state = "exclude"`). Drag a rule (desktop) or use its up and
  down buttons (Shift-Up / Shift-Down or K / J in the terminal). The most
  specific glob wins and a tie goes to the higher rule, so the order
  decides between equally specific globs. A live preview shows how any
  folder you type resolves ("~/labs/team-hub -> alice-labs
  (alice)") and every known project (open tabs, the project
  index's repos, saved bindings) grouped by the rule that decides it,
  with the undefined ones highlighted.
- **Per project bindings:** each saved owner/repo/account with Edit and
  Unbind, and the projects still to decide (undefined, or a "choose"
  rule) with Define.

**Placeholders.** While `[git]` has no owners and no rules, the section
shows a suggested starter set, marked as placeholders, with a "Use
these" button. It comes from the first of:

1. `~/.godterm/github-suggestions.toml` (or `$GODTERM_HOME/...`), when
   present: your own starter set (below).
2. The gh accounts `gh auth status` lists: each account owns its own
   repos, an example org `your-org` is pushed by the active account, and
   rules `~/work/**` -> your-org, `~/**` -> the active account.
3. The shipped examples: owners `your-github-user` and `your-org` (both
   pushed by your-github-user), rules `~/work/**` -> your-org and `~/**`
   -> your-github-user.

`your-github-user`, `your-org` and the names in the file's
`placeholders` list stay marked "placeholder" (and are written with a
`# placeholder: set the real name` comment) until they are renamed;
renaming an owner in the web section renames it in the rules too.

`github-suggestions.toml` has the draft's shape: `[[owners]]` tables
with `owner` and `account`, `[[rules]]` tables like `[[git.rules]]`, and
an optional `placeholders` list. It is only a suggestion: nothing in it
is used until you press Use these and confirm.

```toml
placeholders = ["acme-org"]

[[owners]]
owner = "alice"
account = "alice"

[[owners]]
owner = "acme-org"
account = "alice_acme"

[[rules]]
match = "~/acme/**"
owner = "choose"
choices = ["acme-org", "alice"]
state = "exclude"

[[rules]]
match = "~/**"
owner = "alice"
```

**Saving** is the user's own and is never routed through the assistant.
Save (or Use these) sends the draft for a check (`git` frame op
`cfg_check`), which returns the errors, warnings, the preview and the
diff ("+ owner ...", "- rule ...", "~ ...", the new rule order, "+ bind
..."), plus a one time id when there is something to save and no error.
The confirmation shows that diff; only Confirm sends `cfg_save` with the
id (the terminal editor does the same with its Enter). Then:

- config.toml is edited with toml_edit: `[git.owners]` and
  `[[git.rules]]` are set to the draft, every other key, comment and
  layout stays, and a rule kept from before keeps its own comments. The
  result must read back as the draft or nothing is written. The app
  reloads it at once.
- binding edits go through the bindings store
  (`~/.godterm/git-bindings.json`), each checked as in the Git section.
- Refused: a glob that is not one (`~/[x/**`), a relative glob, a glob
  or folder in the private vault or one the wildcards could reach into
  (`/Volumes/Private/**`, `/Volumes/**`, `/**`) or in a private folder,
  owner and account names GitHub would not accept, "choose" with
  nothing to choose from, a state other than "exclude", an owner listed
  twice. Warned, not blocked: an account that is not logged in to gh, a
  rule owner with no account in the owners table, a placeholder owner.

**The assistant** reads all of it through `git_bindings` (owners, the
full rules, placeholder owners) and may suggest changes in words. No
tool saves owners, rules or bindings, and `set_setting` does not reach
`[git]` (tested in src/gitcfg_tests.rs). The `cfg_*` frames are web
socket frames from a browser, not control tools.

Code: src/git_cfg.rs (draft, checks, preview, diff, the toml_edit
writer), src/app_gitcfg.rs (the terminal editor, the `cfg_*` frames, gh
accounts), web/js/gitcfg.js (the web section).

Code: src/git_map.rs (rules, bindings), src/git_ops.rs (writing git, the
`forbidden` check every argument list passes), src/git_cred.rs
(`godterm git-credential`, gh calls), src/git_scan.rs (secrets, sizes,
.gitignore, symlinks), src/git_panel.rs (view, prepare, execute),
src/app_gitpanel.rs and src/ui_git.rs (the TUI section, tools, web frame),
web/js/git.js (the sheet).

## 0. Summary

- GitHub is the only hub. Normal use never needs another machine online: every machine pushes to and pulls from private GitHub repos. A machine that dies loses at most the last few seconds (state pushed about 5 s after every turn end and on every config or .env change).
- Two kinds of repos:
  1. Project repos: your code. Each project folder maps to a GitHub owner and a gh account through rules in config. A project no rule covers is "undefined": GodTerm asks and never guesses. Repos are created private, only after an explicit yes. Pushes happen only after a yes for that repo, or with that repo's saved auto sync opt-in.
  2. The machine state repo (`<owner>/godterm-state`) plus monthly conversation repos (`<owner>/godterm-convos-YYYY-MM`). They hold the portable config, the accounts list (no credentials), jobs, archive, projects and their bindings, per machine tab layouts, voice settings, each project's .env files and the agent conversations. All stored as-is (not encrypted): the GitHub account is fully trusted.
- Never synced: OAuth tokens (except GodTerm's own long-lived tokens, opt in per token and only into a repo checked private: 2.8), keychain items, `.credentials.json`, `.claude.json`, GodTerm's control and web tokens, shell snapshots, anything under `/Volumes/Private` (symlinks into it count too).
- One owner per conversation: a lease in the state repo. Heartbeat every 2 min, lease expires after 10 min without one. "Take over now" (explicit yes) takes a stale or live lease. A machine that lost a lease stops writing that session, and lines it never pushed become a forked session. History is never rewritten: no force push anywhere, ever.
- A Git section on every tab (TUI and web): status, branch, commits, diff summary, commit (assistant drafts, user confirms), push, pull, sync, branches, repo and account binding. A Sync screen per machine. Assistant tools for all of it, with confirmations for anything that changes something.

## 1. Project repos

### 1.1 Mapping rules (config.toml)

```toml
[git]
undefined = "ask"          # the only allowed value
repo_name = "folder"       # proposed name: folder name, lowercased, spaces to '-'

[git.owners]               # which gh account pushes for each GitHub owner
"alice"      = "alice"
"alice-labs" = "alice"
"acme-org"   = "alice_acme"
"bob"        = "bob"

# First matching rule wins (most specific glob first; ties: order here).
[[git.rules]]
match = "~/labs/**"
owner = "alice-labs"

[[git.rules]]
match = "~/acme/**"
owner = "choose"
choices = ["acme-org", "alice"]
state = "exclude"          # conversations and .env stay off the personal state repo

[[git.rules]]
match = "~/**"
owner = "alice"            # personal default; creating a repo still needs a yes
```

Resolution for a project folder P (canonicalized, symlinks resolved):
1. P inside `/Volumes/Private` or another never folder: refused, no binding possible.
2. P has an origin remote on github.com: binding taken from the URL (owner/repo); account from `[git.owners]`; unknown owner = "unknown account", user picks, saved.
3. Otherwise the first matching rule proposes an owner. `owner = "choose"` means pick from `choices`. No match = undefined: the Git section shows "No repo defined" and a Define button. Never inferred.
4. The repo name is always shown and editable before creation.

Bindings stored in `~/.godterm/git-bindings.json` (synced as `git/bindings.json`), portable paths:
```json
{"version":1,"projects":{
  "~/labs/team-hub": {"owner":"alice-labs","repo":"team-hub","account":"alice",
     "auto_sync":false,"wip_backup":false,"defined_at":"2026-10-25T21:00:00Z"}}}
```

### 1.2 Credentials per repo
- Every git call for a bound repo runs with `-c credential.helper= -c credential.https://github.com.helper="!<godterm> git-credential --account <acct>" -c credential.useHttpPath=true`.
- `godterm git-credential` speaks git's credential protocol on stdin/stdout, gets the token from `gh auth token --hostname github.com --user <acct>`, prints `username=x-access-token` and `password=<token>`. The token is never logged, never on a command line, never written to disk.
- `gh` calls (repo create, view, visibility) run with `GH_TOKEN` in the child env from the same lookup. Never `gh auth switch`.
- Optional per repo with a yes: write the helper into the repo's `.git/config` so plain `git push` uses the right account.
- SSH remotes are left alone. Non-interactive settings stay (BatchMode, GIT_TERMINAL_PROMPT=0).

### 1.3 Creating a private repo (Git section "Create repo", or tool `git_create_repo`)
1. Show folder, owner, name, account, "private". No public option anywhere in GodTerm.
2. `gh repo view owner/name`: if it exists, offer "use existing".
3. Not a repo: offer `git init -b main`.
4. Hygiene (1.4), full secret scan (1.5), large file check (1.6).
5. Explicit yes, then `gh repo create owner/name --private --source <dir> --remote origin` (no `--push`).
6. Verify `gh repo view --json visibility` = PRIVATE, then the first push, with its own yes in the same dialog ("Create and push").

### 1.4 .gitignore hygiene (before first commit or push)
- Ensure ignored (offer additions to review): `.env`, `.env.*` (except `.env.example`, `.env.sample`), `*.pem`, `*.key`, `*.p12`, `id_rsa*`, `id_ed25519*`, `.DS_Store`, `node_modules/`, `target/`, `dist/`, `build/`, `__pycache__/`, `.venv/`, `venv/`, `*.log`, `.godterm-*`.
- Tracked or staged files matching `guard::secret_name` or `.env*` block the push; fix offered: `git rm --cached` plus an ignore rule.
- A staged symlink whose target canonicalizes into a never folder blocks the push.

### 1.5 Secret scan (project repos: blocking)
- With gitleaks: first push `gitleaks git <dir> --redact --no-banner --report-format json --report-path -` (whole history); later pushes add `--log-opts=<upstream>..HEAD`.
- Without it: built-in regex patterns over the outgoing diff: AWS `AKIA[0-9A-Z]{16}`, GitHub `gh[pousr]_[A-Za-z0-9]{36,}` and `github_pat_[A-Za-z0-9_]{80,}`, Anthropic `sk-ant-[A-Za-z0-9_-]{20,}`, OpenAI `sk-(proj-)?[A-Za-z0-9_-]{20,}`, xAI `xai-[A-Za-z0-9]{20,}`, Google `AIza[0-9A-Za-z_-]{35}`, Slack `xox[abprs]-[A-Za-z0-9-]{10,}`, Stripe `[sr]k_live_[A-Za-z0-9]{20,}`, `-----BEGIN [A-Z ]*PRIVATE KEY-----`, generic `(?i)(api[_-]?key|secret|token|passw(or)?d)\s*[:=]\s*['"]?[A-Za-z0-9/+_.-]{16,}`.
- Findings (file, line, rule, redacted match) block the push. Ways forward: fix, `.gitleaksignore` entry, or type `push anyway` in the dialog. Assistant tools can never override; only a human.

### 1.6 Large file guard
- Outgoing blobs over 50 MB warn; over 95 MB block; a total push over 1.5 GB blocks with "split the push". No Git LFS for now.

### 1.7 Push, pull, sync
- push: `git push origin <branch>` (`-u` the first time). Never `--force` or `--force-with-lease` (a test checks every argument list GodTerm builds).
- pull: `git pull --ff-only`. Diverged: "Diverged: N local, M remote" with Rebase (yes, clean tree only) or "Push as a branch".
- sync: fetch, ff-only pull if behind, push if ahead.
- Diverged on return: never rewrite; push local commits to `godterm/<machine>/<branch>-<yyyymmdd>` and say so.
- No auto_sync and no wip_backup: by the user rule nothing pushes or commits a project repo automatically. Fetch (read only) may run in the background; push, pull, sync and commit run only from the Git section on the user's press and confirm. (The earlier per repo auto_sync / wip_backup opt-ins are dropped.)
- Projects not backed up are listed with warnings (no repo, no GitHub remote, local folder remote, unpushed commits, uncommitted changes); nothing is done to them.
- auto_sync (per repo, off by default): after a tab in that repo ends a turn (debounced 30 s) and every 15 min: fetch, ff-only pull when clean, push existing commits. Never commits on its own; every push runs 1.5 and 1.6; a finding turns auto_sync off for that repo and says why.
- wip_backup (per repo, off by default): after each turn end, a snapshot of the work tree (temp index, `git commit-tree`; branch and index untouched) to `refs/heads/godterm-wip/<machine>/<branch>` after the secret scan.

## 2. Machine state repo

### 2.1 Repos
- `<owner>/godterm-state` (private, small): everything but transcripts. Branches: `main` (state), `leases` (ownership).
- `<owner>/godterm-convos-YYYY-MM` (private): transcript chunks written that month; over 3 GB -> `-YYYY-MM-2`. Old months stop changing.
- Working copies: `~/.godterm/sync/state/`, `~/.godterm/sync/convos-YYYY-MM/`. Shards use partial clones (`--filter=blob:none`, sparse).

### 2.2 Layout of godterm-state
```
format.json                      {"version":1}
README.md                        generated: what this is, "private, never make public"
.gitattributes                   "* -text" (byte exact jsonl and .env)
config/portable.toml             portable config keys only (2.4)
accounts.json                    [{name,label,harness,color,args,permission_mode,...}] no config_dir, no creds
claude/<acct>/settings.json      CLI settings (scanned: env values warn)
claude/<acct>/CLAUDE.md, skills/, commands/, agents/
claude/<acct>/history.jsonl      prompt history
archive.json  jobs.json  projects.json  learned.json   keyed merge (2.6)
assistant/conversations/...      assistant panel history
memory/...                       project and session memory
git/bindings.json                1.1
env/<project-key>/<relpath>      .env files as-is (2.7)
machines/<machine-id>/machine.json   {label, os, home, user, first_seen, version}
machines/<machine-id>/state.json     that machine's tab layout (paths portable)
machines/<machine-id>/paths.json     path map (2.3)
sessions/<acct>/<session-id>.json    transcript manifest (2.5)
shards.json                      [{repo, month, bytes, sealed}]
```
Allowlist only. Never collected (a test enumerates a fixture home and checks): `.credentials.json`, `.claude.json`, keychain anything, `control.json`, `control.sock`, `web-token`, `tab-mcp/`, `godterm.lock*`, logs, `usage/`, `session-index.json`, `project-index.json`, `updates/`, `bin/`, `voice/` models, `trash/`, migration backups, `shell-snapshots/`, `session-env/`, `sessions/`, `statsig/`, `telemetry/`, `cache/`, `chrome/`, `paste-cache/`, `file-history/`, `plugins/` caches, `assistant/mcp.json`, `web-client.log`, `push` keys.

### 2.3 Paths across machines
- Under home stored as `~/...`; other absolute paths go through `[sync.paths]` in the machine's local config.
- Machine id: random UUID in `~/.godterm/machine.json` (never synced, never copied by restore). Label defaults to host name.
- Claude's project dir name computed from the mapped cwd (`state::encode_project_dir`). Transcript contents not rewritten; Phase 2 verifies whether claude needs cwd fields to match and rewrites only on placement, opt-in.
- Tab uids are per machine: restored layouts get fresh uids; jobs target sessions (account + session id), not uids.

### 2.4 Config split
`config/portable.toml` = every key except machine local ones: `*_bin`, `accounts[].config_dir`, `[web]`, `[supervisor]`, `[update]`, `remember_window`, `refresh_secs`, `pty_read_buffer_kb`, `writer_queue_kb`, voice device and model paths, `[sync.paths]`, and `[sync] machine_local = [...]`. Applied with `toml_edit` onto the local file (comments and local keys kept). Newest change per key wins. `trusted_dirs` and `new_tab_base` are path mapped.

### 2.5 Conversations
What: every session in a tab, closed tabs, the archive, or targeted by a job, plus every session written in the last 30 days. Main `<id>.jsonl` only; `subagents/` and `file-history/` left out by default (Phase 2 first checks `claude --resume` works without them). Left out: cwd in temp folders (`/private/var/folders`, `/tmp`), cwd in a never folder, content mentioning `/Volumes/Private` (overridable per session). Harnesses: Claude and Grok in Phase 2 (via `session_ops::related` and a grok equivalent in providers/grok.rs).

Chunks (append-only, byte exact): after each sync the new bytes since the last push become one immutable chunk `convos-YYYY-MM/<acct>/<session-id>/<offset:012>.jsonl`, at most 25 MB (split at line ends where possible). Concatenation = the file byte for byte.

Manifest `sessions/<acct>/<id>.json`:
```json
{"provider":"claude","account":"account1","session":"5dfa...","cwd":"~/notes-app",
 "title":"...","len":167772160,"sha256":"<of bytes 0..len>",
 "chunks":[{"repo":"godterm-convos-2026-10","path":"account1/5dfa.../000000000000.jsonl",
            "off":0,"len":26214400,"sha256":"..."}],
 "forked_from":null,"updated":"...","machine":"<id of last writer>"}
```
Push the chunk repo first, then the manifest. Compare local Ll vs remote Lr: equal + same hash: nothing; Ll < Lr and local is a prefix: extend locally; Ll > Lr and remote is a prefix: push the new bytes (needs the lease); otherwise diverged: never merge lines: the local file becomes a new session (fresh id via `session_ops` `Conflict::NewId`), named "<title> (fork from <machine>, <date>)" with `forked_from`, and the remote version is placed under the original id.
Transcript secret scan: warning only (gitleaks stdin or built-in patterns), count on the Sync screen.
Size: `godterm sync plan` dry run; first upload over 1.5 GB pushed in batches; dropping an old month repo is explicit (typed yes).

### 2.6 Keyed merge (archive, jobs, projects, learned, memory)
Entries keyed by id with `updated` (unix ms) and `deleted` tombstones (kept 90 days). Union; newest `updated` wins. Changes trigger a push (debounced 5 s). Entries lacking `updated` get it on load (`sync_merge::stamp`). Jobs run only on the machine owning the target session's lease; jobs without a session carry `machine`. Sync screen shows paused jobs of offline owners with Take over.

### 2.7 Project .env files (as-is)
- Captured: every `.env` and `.env.*` (not `.env.example`/`.env.sample`) at a bound or known project root and up to depth 3, never under a never folder; symlinks refused. Every known project is included (user rule), with or without a repo; changes are pushed within 5 s (mtime watch, project list refreshed every 60 s).
- Captured: every `.env` and `.env.*` (not `.env.example`/`.env.sample`) at a bound or known project root and up to depth 3, never under a never folder; symlinks into never folders refused. First adoption per project needs a yes; after that changes are pushed within 5 s (mtime watch plus the 60 s index tick).
- Stored at `env/<project-key>/<relpath>` (portable path slug + 8 hex of sha256). Never in the project's own repo: `git check-ignore` must say ignored, else warn and offer the rule.
- Restored next to the project with mode 0600, `O_NOFOLLOW`, temp file + rename. If a local file differs: show which keys changed (never values) and ask: keep local, take remote, keep both (`.env.from-<machine>`).
- Later (Phase 4): `[sync] encrypt_secrets` (age, keychain, Tailscale pairing); Doppler backend.

### 2.8 Long-lived tokens (opt in)

GodTerm's own long-lived tokens (`claude setup-token`, made from the
Accounts screen, kept in the keychain under `godterm-token`) may sync;
nothing else of a login ever does (the CLIs' `.credentials.json` and
keychain items stay never synced). Off by default, two switches on each
machine: Accounts > "Sync tokens to my other machines" (asks first) and
"Sync this token" per token. Choices live in `account-tokens.json`
(never synced itself, no token in it).

- `tokens/<account>.json` in the state repo: `{account, token, sha,
  created, last4, at_ms, machine, machine_id}`, or a tombstone `{account,
  deleted: true, sha, at_ms, machine, machine_id}`.
- A token file is written only when the state repo reads as private right
  then: `gh repo view <owner>/<repo> --json visibility` through the sync
  account's GH_TOKEN, `PRIVATE` (an answer is kept 5 minutes). Unknown or
  not private: no token is written, token files already in the working
  copy are taken out of the commit, everything else syncs, and the sync
  report (`token_warning`, the Sync screen, the Accounts screen) says so.
  A folder remote (tests) reads a `visibility` file in its bare repo.
- Per account the newest of this machine's token, its tombstone and the
  repo's entry wins. A newer token from another machine goes into this
  machine's keychain (never a file) when token sync is on here, and the
  Accounts screen marks it "from <machine>". Deleting a synced token
  writes a tombstone; other machines delete the same token (by its sha)
  at their next sync. Deleting never revokes it: revoke it at claude.ai.
- Tokens still never reach a browser or a log.

## 3. Restore on a new machine
`godterm sync init` (first machine): check gh login for the state repo owner; with a yes `gh repo create <owner>/godterm-state --private`; write machine id; first collection (`sync plan` shown first); push.
`godterm sync clone <owner>/godterm-state` (new machine): prerequisites (git, gh, CLIs; `gh auth status` lists needed accounts, missing ones print the `gh auth login` to run); clone state; name the machine; confirm path map; apply `portable.toml` onto a fresh config; accounts at default dirs with no login; projects checklist (all on except `state = "exclude"`), cloned to mapped paths with the right credential helper (existing folder with same remote: fetch only; different remote or non-repo: reported, never touched); restore .env; place the hot set of conversations (open tabs, archive, jobs, 7 days), the rest on demand; "Start with MacBook's tabs?" (fresh uids, nothing resumed until a lease is taken); login checklist per account (tab running the CLI's login; OAuth never copied); `godterm sync doctor`.

## 4. Ownership (leases)
- Branch `leases`: `leases/<session-id>.json` = `{"owner":"<machine-id>","gen":7,"acquired":"...","takeover":{"by":"<id>","at":"..."}|null,"released":{"len":...,"sha256":"..."}|null}`; `heartbeats/<machine-id>.json` = `{"at":"...","label":"MacBook"}`.
- Live while the owner's heartbeat is under 10 min old; heartbeat every 2 min (`[sync] heartbeat_s = 120`, `lease_s = 600`).
- Compare-and-swap: fetch `leases`, decide, commit, `git push` without rebase or force; non fast forward = someone else wrote first: fetch and decide again.
- Acquire before a tab resumes or starts a session (app_resume, restore, archive open, a job waking a tab) and when a new session id is detected; then pull the transcript first.
- Owned elsewhere and live: tab opens read-only with the transcript and "Owned by MacBook, active 1 min ago", buttons Request take over / Take over now.
  - Request: sets `takeover`; the owner sees it on its next lease fetch (2 min; 20 s while any request is pending); idle tab: push final bytes and release (`released` len+hash); busy: release after the turn. Requester then acquires.
  - Take over now (explicit yes, also for stale): CAS gen+1, owner = this machine; never waits.
- Lost lease: input to that tab blocked at once; the running turn may end, then the CLI is stopped (SIGINT, as takeover.rs); unpushed bytes pushed as a forked session; tab shows "Moved to <machine>; your unsent part is fork X".
- Dead machine returning: pull before resuming anything, check its leases, fork sessions with unpushed bytes it no longer owns, push diverged project commits as branches, never rewrite.
- Offline start: our last known lease resumes with banner "offline: will fork if someone else took over"; others need Take over now once online, or "Work offline anyway" (always forks on reconnect).
- Tab move to another account keeps session id and lease; copy/duplicate takes a new lease on the new id.

## 5. Cadence and transport
- State repo: pull at start before any resume; fetch `main` every 2 min and `leases` with each heartbeat; push ~5 s after a turn end (chunk + manifest), on config/.env/archive/jobs changes (5 s), on quit (best effort, 10 s cap).
- Project repos: fetch every 15 min with the helper; push only by a yes or auto_sync.
- Git CLI over https with the credential helper; `gh` only for API calls. No peer to peer.
- Offline: commits pile up locally; retry with backoff 30 s to 10 min; Sync screen shows "N changes waiting".
- Background pool (2 workers), never on the UI thread; non-interactive git settings of `project_git::git()`.

## 6. UI
### 6.1 Git section per tab (TUI tab options "Git...", web tab menu "Git")
Header: repo top, branch (or detached), remote owner/repo, account, binding source (remote, rule, manual), state line (`Facts::state_word`), fetched ago. Commits: last 20 with unpushed marks. Changes: `git diff --stat` staged/unstaged, untracked count, up to 10 files. Actions: Commit (assistant drafts from the diff, user edits and confirms; all or staged only), Push, Pull, Sync, Fetch, Branches (list, switch with clean tree or stash on a yes, create), Define repo / Change binding / Create private repo, Auto sync toggle, WIP backup toggle. Undefined project: only "No repo defined. Define..." and the rule picker. Web sheet `web/js/git.js` uses the same data through a `git` WebSocket message; a button press plus the sheet's confirm dialog is the user's yes.
### 6.2 Sync screen (TUI view, web Settings > Sync)
State repo, last pull/push, changes waiting, errors; machines (label, last heartbeat, version); conversations owned here / elsewhere / pending takeover (Release, Take over now); paused jobs; transcript secret warnings; size per month repo and `sync plan`; undefined and diverged projects.
### 6.3 Assistant tools
Read only: `git_status` (from project-git), `git_log`, `git_diff`, `git_branches`, `git_bindings`, `sync_status` (includes the projects not backed up), `sync_plan`. Through `control::planned`: `sync_init`, `sync_clone` (see "Turning it on from the app"). `sync_now` and `sync_pause` need no confirmation (it only touches GodTerm's own state repos). Project-changing git actions (commit, push, pull, sync, branch create/switch, bind, create repo) are UI-only actions: the assistant may draft a commit message or explain what is missing, but cannot run them, not even through `control::planned`; the user presses the button in the Git section and confirms. Through `control::planned`: `take_over {session}`, `release_session {session}` (Phase 3). No tool can override a secret finding, create a public repo, force push, or delete a repo.
Read only: `git_status` (from project-git), `git_log`, `git_diff`, `git_branches`, `git_bindings`, `sync_status`. Change something, through `control::planned` (asked in one turn, run only on the user's yes in a later turn): `git_commit {dir,message,all}`, `git_push`, `git_pull`, `git_sync`, `git_branch {create|switch}`, `git_bind {dir,owner,repo,account}`, `git_create_repo {dir,owner,name}` (always private, no visibility arg), `git_auto_sync {dir,on}`, `env_adopt {dir}`, `take_over {session}`, `release_session {session}`. `sync_now` needs no confirmation. No tool can override a secret finding, create a public repo, force push, or delete a repo.
### 6.4 CLI
`godterm sync init | clone <owner/repo> | status | now | plan | doctor | takeover <session> | release <session>`, `godterm git-credential --account <acct>` (internal helper).

## 7. Security model (adds to SECURITY.md)
- Trusted: the sync's GitHub account (`[sync] owner`, and orgs/accounts in `[git.owners]`) and machines logged into it. Whoever controls it can read every synced conversation, .env and config and write leases/state your machines apply. Accepted by the user. Mitigations: 2FA with a hardware key; repos private and verified private after creation; no public option.
- Pulled state is data, not instructions: synced transcripts, memory, learned rules are treated like tab text. Pulled config cannot set `*_bin`, `[web]`, `[supervisor]`, `[update]` or `args` with shell metacharacters. Pulled jobs show as "new from <machine>" and need a yes before their first run on this machine.
- Credentials never synced (allowlist + test). gh tokens read per call, never stored.
- Vault: `/Volumes/Private` joins `guard::private_dirs` and the sync never list; every path canonicalized before collection, binding, clone or .env restore; sessions in or mentioning the vault not synced by default.
- Secret scanning blocks project pushes, warns for transcripts. .env never into project repos.
- No force push in any code path (tested). Lease CAS relies on GitHub's atomic ref update.
- Revoking a machine: remove its gh login, rotate GitHub tokens, rotate .env secrets it held.

## 8. Phased plan
Dependency first: merge branch `project-git` (`src/project_git.rs`, app_projects.rs, project_index.rs, main.rs).

### Phase 1: Git section, mapping, private repo create and push
Add: `src/git_ops.rs` (writing git runner based on `project_git::git()`; commit/push/pull --ff-only/sync/branch/switch/log/diff-stat/fetch; argument builder that can never include `--force`; diverged fallback), `src/git_cred.rs` (git-credential subcommand + GH_TOKEN lookup), `src/git_map.rs` (`[git]` rules, glob precedence, owner->account, bindings store, resolution), `src/git_scan.rs` (gitleaks wrapper, built-in patterns, large file guard, .gitignore hygiene, secret names via `guard::secret_name`), `src/app_git.rs` (tools, planned confirmations, panel state), `src/ui_git.rs` (TUI panel), `web/js/git.js`.
Change: config.rs (`[git]`), guard.rs (`/Volumes/Private`, never list), app_tabopts.rs (`TabAct::Git`), menus.rs, main.rs (subcommand), web/ws.rs + web/app_web.rs (`git` message), web/js/tabs.js (menu entry), control.rs (tools), SECURITY.md, README.md, docs/SYNC.md, docs/WEB_APP.md.
Tests: glob precedence; a "choose" rule; no rule -> undefined; binding from remote URL; unknown owner -> ask; vault and symlink refusal; each secret pattern hits and redaction never shows the full value; gitleaks-absent fallback; gitleaks JSON fixture parse; 50/95 MB thresholds (sparse files); push/pull/sync against a local bare repo (file://); diverged pushes to `godterm/<machine>/...`; no `--force` in any argument list (property test over all ops); credential helper protocol with a stubbed gh; `gh repo create` args include `--private`, never `--public`; GH_TOKEN in env not argv; planned flow refuses commit/push without a later-turn yes; web `git` message round trip.

### Phase 2: State repo backup and restore
Add `src/sync/`: mod.rs (worker, debounce, retry), state_repo.rs, collect.rs (allowlist), portable.rs (path map, config split), chunks.rs (chunking, manifests, prefix compare, fork), shards.rs (month repos, partial clone), merge.rs (keyed merge, tombstones), env.rs (.env capture/restore), restore.rs (`sync clone`), plan.rs (dry run); `src/app_sync.rs`, `src/ui_sync.rs` (Sync screen), web manage.js Sync section.
Change: state.rs, archive.rs, jobs.rs, projects.json writers (`updated`, tombstones, push hooks), session_ops.rs (fork helper on `Conflict::NewId`), providers/grok.rs (session files), app_resume.rs (download on open), main.rs.
Tests: fixture home walk never yields a forbidden file (golden list); path map round trip /Users/alice <-> /home/alice plus encoded project dir; config split keeps local keys and comments and rejects unsafe pulled keys; chunker byte exact reassembly incl. a single 30 MB line; extend/ahead/diverged cases; fork gets new id with forked_from; keyed merge with tombstones and clock skew; .env restore 0600, refuses symlinks, check-ignore warning, keep-both; vault exclusion by cwd and content; end to end with two GODTERM_HOME fixtures and bare remotes standing in for GitHub (init then clone; second home resumes the session file at the mapped path); `sync plan` totals; manual check `claude --resume` works without subagents/.

### Phase 3: Leases and multi machine sync
Add `src/sync/lease.rs` (CAS, heartbeat, expiry with clock.rs injection), `src/sync/takeover.rs` (request, release, take over now, lost lease).
Change: app_resume.rs, app_startup.rs (pull before resume), app_jobs.rs (owner gate), app_tabmove.rs, takeover.rs (reuse graceful stop), read-only tab view in ui.rs and web chat.js/term.js, ui_sync.rs.
Tests: two clones race to acquire, exactly one wins; expiry at 10 min (injected clock), stale owner never blocks; take over now on a live lease: old owner's next heartbeat finds the loss, blocks input, forks unpushed bytes; dead machine returns with unpushed transcript bytes and project commits: fork + branch, remote refs only fast forward (`merge-base --is-ancestor` on every ref after each scenario); request then release when idle; jobs fire only on the owner; offline start banner and fork on reconnect; tab move keeps the lease.

### Phase 4 (later)
encrypt_secrets (age, keychain, Tailscale pairing), Doppler backend, LFS per repo, subagents sync option, Codex/Cursor/OpenCode transcripts.

## 9. Open questions (build with these defaults)
1. A work folder whose projects may be personal or the employer's. Default: choose per project; its conversations and .env excluded from the personal state repo (`state = "exclude"`).
2. Personal catch-all `~/** -> <your account>`: on; creating a repo always needs a yes and a name.
3. Repo names: `<owner>/godterm-state`, `<owner>/godterm-convos-YYYY-MM`.
4. Conversation window: hot set + last 30 days, main transcript only.
5. Sessions mentioning /Volumes/Private: excluded, overridable per session.
6. history.jsonl synced; paste-cache not.
7. WIP backup: off per repo.
8. auto_sync: off per repo.
9. Heartbeat 2 min / lease 10 min.
10. Pulled jobs need a yes before first run on a new machine.
11. `sync doctor` checks the sync account can create repos in the orgs it pushes for; report and ask.
12. Encryption and Doppler: off, Phase 4.

## Phase 2 as built (2026-10-25)

Code: `src/sync/` (mod, paths, git, portable, merge, collect, chunks, shards, files, env, projects, secrets, state_repo, restore, plan, worker, cli, tests), `src/app_sync.rs`, `src/ui_sync.rs` (Settings > Sync). Hooks: `[sync]` in config.rs, `/Volumes/Private` in guard.rs, `godterm sync ...` in main.rs, `sync_tick` in App::tick, `sync_status` / `sync_now` in control.rs.

What changed from the design while building it:
- `[sync]` in config.toml (all machine local): `enabled`, `owner`, `account` (both empty by default: `sync init --owner NAME` sets them, else the first gh login is used), `state_repo`, `convos_prefix`, `remote` ("github", or `file:///DIR` whose `<owner>/<name>.git` bare repos stand in for GitHub: tests and local mirrors), `window_days`, `hot_days`, `debounce_s`, `chunk_mb`, `shard_gb`, `heartbeat_s`, `lease_s`, `auto_create_shards` (set by init's yes: monthly repos are created private without asking again), `include_sessions`, `exclude_sessions`, `machine_local`, `paths` (`name = "/abs/folder"`, stored as `@name/rest`).
- The keyed merge keeps the stores' files untouched: what was last synced sits in `~/.godterm/sync/base/<store>.json` (hash and stamp per key). Loading stamps entries (unchanged keeps its stamp; changed gets max(now, base + 1), so an edit beats the version it edited even with a clock behind; gone becomes a tombstone). In the state repo each store is `{"version":1,"entries":{key:{updated,deleted,value}}}`. Bases are saved only after the push went through. Learned rules are keyed by `created` (two machines adding rule 5 renumber the later one the same way everywhere).
- File sets (Claude account files, skills/commands/agents, memory per project, assistant conversations, .env) use the same merge per file, with `<area>/.index.json` stamps and `<area>/.by.json` last writers (for `.env.from-<machine>`). history.jsonl is a line union in time order.
- Manifests: Claude keeps the design's top level `len/sha256/chunks`; a Grok session is a folder, so its manifest has `files: [{name,len,sha256,chunks}]` instead. Non-jsonl Grok files (summary.json) that are rewritten get a whole-file chunk `.../<name>/v<sha12>`.
- Conversation repos are bare partial clones (`--filter=blob:none`) written with plumbing on a temporary index (no checkout); a chunk is fetched on demand. One sync uploads at most 256 MB of transcript bytes; a first upload goes on in further syncs right away.
- A session open in a tab here is never extended or replaced under its running CLI: it is "held" (Phase 3 leases settle it). Diverged sessions fork as designed.
- Pulled jobs new to a machine arrive paused (enabled false) and stay paused there until the user turns them on; the paused copy is not pushed back.
- The credential helper is `godterm sync git-credential --account <acct>` for now (Phase 1's `godterm git-credential` can take it over).
- Every git write (commit, commit-tree, push, update-ref, merge, rebase, tag) is refused outside a repo carrying the `godterm-sync` mark, so no sync code path can write a project repo; `check_args` refuses every force, delete, mirror or `+refspec` push. A test audits every git call of the end to end run.
- `sync clone` clones bound projects (git/bindings.json, Phase 1) to their mapped folders (read only for existing ones: fetch); it never commits or pushes them.
- Not built in Phase 2: the web Manage > Sync section, the app_resume download-on-open hook (use `godterm sync get <id>`), gitleaks for transcripts (built-in patterns only), the `claude --resume without subagents/` manual check (not run: it would need real data).
- Leases: the `leases` branch exists with `leases/.keep` and `heartbeats/<machine-id>.json` (written at init and clone); Phase 3 adds the rest.

## Phase 3 as built (2026-10-25)

Code: `src/sync/lease.rs` (lease and heartbeat files, the board, the pure `decide`, the compare and swap repo), `src/sync/takeover.rs` (reconcile before a resume, release, and the lease worker's `Engine`), `src/app_lease.rs` (the worker thread, the resume gate, tab states, events, jobs gate, tools' halves), `src/ui_lease.rs` (the read only view and banner). Hooks: `App::launch` (gate), `detect_sessions` (new ids), `on_grid_key` and `Pane::write` (input blocked), `fire_job` (owner gate), `sync_tick` (starts the workers), `web_tabs` (`lease` per tab), `web/js/term.js` (banner and buttons), `web/js/settings.js` (Settings > Sync). Tests: `src/sync/lease_tests.rs` (two fixture homes, local bare repos), `src/lease_app_tests.rs` (the app with stub tabs), unit tests in lease.rs.

How it works:
- The leases branch is read and written through its own bare clone, `~/.godterm/sync/leases.git` (marked as a sync repo), so a lease never waits for a long sync. `leases/<id>.json` = `{owner, gen, acquired, takeover, released, label}`; `heartbeats/<machine>.json` = `{at, label}`. Every write fetches, decides with `decide()` (pure; the clock comes in as `Ctx::now_ms`), commits on top with a temporary index and pushes without rebase or force. A push that is not a fast forward, including the remote's "incorrect old value" when two pushes meet, decides again (up to 10 times). Any write that takes a lease carries a fresh heartbeat, so a winner never reads as stale.
- Statuses: Free (no lease), Mine, Released (the owner let go, with `len` and `sha256` of the transcript it pushed), Live (owned elsewhere, heartbeat under `lease_s`), Stale (older: never blocks; a plain acquire takes it). The board as last fetched is kept in `sync/leases.json` for offline starts.
- The lease worker (thread `sync-lease`) polls every `heartbeat_s` (heartbeat then), every 20 s while a request is pending, every 30 s while offline. Commands from the app (acquire, request, take over now, release, fork, work offline) are batched: every resume at startup goes out in one swap.
- Resume gate: with sync on, `App::launch` of a Resume on a Claude or Grok account waits (`TabLease::Waiting`) unless the session is already granted here. The worker acquires, then pulls first (`reconcile`): remote ahead is appended; local bytes the repo never got stay when this machine owns the session, otherwise they become a fork; after a release it waits until the transcript matches the released length and hash. A new session id seen in a running tab (a fresh start, a copy, a duplicate) takes its own lease without a pull. A tab moved to another account keeps its id and the grant, so it resumes at once.
- Owned elsewhere and live: the tab is read only (`TabLease::ReadOnly`): "Owned by MacBook, active 1 min ago", the last six exchanges of the transcript (kept current: the sync extends transcripts no CLI runs on), and keys r (Request take over), T then y (Take over now), o (Work offline anyway, offline only), f (open the fork), Enter (resume, when nobody live holds it). The web shows the same banner and buttons over the tab.
- Request: the requester writes `takeover`; the owner sees it on its next fetch (`Event::Asked`), blocks input, lets the turn end, stops the CLI, pushes the final bytes with a sync, checks the manifest has all of them, and releases with their length and hash. The requester's next poll (20 s) acquires and pulls.
- Lost lease (taken over, or found taken on return): input is blocked at once (`Pane::write` drops it, the web tap stops), the running turn may end (a permission prompt counts as the end: nobody can answer it), then SIGINT, a second SIGINT after 0.7 s, SIGTERM after 3 s, never SIGKILL (as takeover.rs). Once the CLI is gone the worker forks what was never pushed and puts the synced copy back under the id; the tab says "Moved to MacBook; your unsent part is fork 1a2b3c4d" and f opens the fork.
- Dead machine returning: its first sync fetches the board; a session it has local bytes for but no longer owns (Live, Stale or Released elsewhere) is never pushed under its id: when no CLI runs on it, the bytes become a fork right there (same fork path as diverged sessions), else the app's lost lease flow handles it.
- Offline start: an acquire that cannot reach the repo uses the last board: ours (or never leased) resumes with the banner "offline: will fork if someone else took over"; someone else's opens read only with "Work offline anyway". On reconnect every session resumed offline is checked: another owner, or the synced copy moved past ours, means the lost lease flow (block, stop after the turn, fork). If nobody touched it, it is acquired quietly and the session goes on without a fork (the design said "always forks on reconnect"; forking only when needed loses nothing).
- Held sessions (Phase 2): the sync now leaves a session alone only while its CLI runs here (the app shares that list through `Ctx::link`); a read only tab's transcript is extended, and only the owner (or anyone, for a session nobody leased) pushes new bytes.
- Jobs: a job whose session is Live or Stale elsewhere is skipped with "paused here: MacBook owns its conversation (take it over to run it here)"; a job with no session runs only on the machine in its new `machine` field (set when it is made with sync on). The Sync screen lists them.
- The archive and jobs race of Phase 2 is fixed: when the sync runs in the app, changes pulled for `archive.json` and `jobs.json` go to the app as `AppEvent::SyncApply`; the app applies them to its own copy (order kept) and saves; the sync waits for the answer (20 s) and, without one, keeps the old base so the next sync hands them over again. The sync never writes those two files under the running app. `projects.json` and `learned.json` are still written by the sync (the app loads them per use).
- The sync lock now also guards threads of one process (the lease worker and the sync worker share the repos).
- Assistant tools: `take_over {tab}` and `release_session {tab}` go through `control::planned` (asked, a later turn's yes runs them). `sync_lease {tab, op}` is the web buttons' tool and refuses the assistant. `sync_status` adds the state repo, the last sync, machines with heartbeats, conversations owned here and elsewhere, requests and paused jobs (the web Settings > Sync section reads it).
- Settings > Sync (TUI) adds: conversations owned here, owned elsewhere (with who and when), take over requests, paused jobs, machines with their last heartbeat.

Not built in Phase 3, or different:
- Project repos are never touched (user rule), so "push diverged project commits as branches" on a returning machine is not done by the sync; diverged projects show in the Git section and the warnings.
- No mouse buttons on the TUI read only view (keys only); the web has buttons. The web shows the banner over the terminal, not a transcript view.
- No `godterm sync takeover <session>` / `release <session>` CLI subcommands yet (the app, the web and the assistant's tools do it).
- Machine clocks are trusted: heartbeats compare one machine's write time with another's clock, so a clock several minutes off shortens or stretches the 10 minutes.
- A Grok session's lease works the same way; its read only view has no transcript preview yet.

## Turning it on from the app (as built 2026-10-26)

Sync can be enabled, joined, checked and paused without a shell: from
the web's Settings > Sync (docs/WEB_APP.md) or by asking the assistant.
Both use the same control tools (src/app_sync.rs), which reuse the CLI's
code paths (`plan::plan`, `restore::init`, `restore::clone`).

- `sync_plan {owner?, account?, repo?}` (read only, off the UI thread):
  the dry run plus `plan::preflight`: owner, gh account, state repo,
  the monthly repo pattern, the gh logins, whether the repo exists and
  its visibility, `can_init` / `can_clone`, and warnings (no gh login,
  there already, not private, .env files that go in as they are, .env
  files not ignored by git, conversations mentioning the vault left
  out). With `repo` it checks a state repo to join. The last plan is
  kept 10 minutes.
- `sync_init {owner?, account?}`: through `control::planned`. It needs
  a plan from the last 10 minutes for the same owner and account that
  says it can, then asks once ("Create the PRIVATE GitHub repo
  OWNER/godterm-state with the gh account ACCOUNT, push this machine's
  first backup (...)?"); only a clear yes in a later turn (or the web
  dialog's Yes, sent as the user's turn) runs it. The CLI's typed yes is
  replaced by that answer; everything else is `restore::init`, which
  only creates repos private and checks them private after.
- `sync_clone {repo, account?, label?}`: asked the same way, then
  `restore::clone`. Clone now refuses a state repo that reads as
  anything but PRIVATE (CLI too). Starting with another machine's tabs
  is CLI only (`--tabs`), since it would rewrite state.json under the
  running app.
- Both run on a background thread; `sync_status.op` has the kind, the
  repo, started and finished times, ok or the error, and a summary.
  The thread's context carries the app link, so pulled archive and jobs
  changes go through the app (`restore::reload` keeps the link now).
- `sync_pause {pause}`: `[sync] paused = true` stops the background
  sync (the watcher and the periodic runs); `sync_now` still runs one,
  and the lease worker keeps going (ownership must stay correct).
- `sync_status` adds `paused`, the worker, `next` (when the next sync
  runs), `repos` (state and monthly repos with size and visibility;
  GitHub is asked off the UI thread at most every 5 minutes, `CHECKING`
  until then), `machines`, `warnings`, `token_sync`, `op`, and the tab
  of each leased conversation.
- The assistant's prompt has a GodTerm Sync section: what sync does and
  never does, check with `sync_status`, turning it on is always
  `sync_plan` first and then `sync_init` or `sync_clone` with the
  user's yes (confirm_token), and `take_over` / `release_session` only
  when the user asks for that conversation.
- Tests: src/sync/setup_tests.rs (preflight, warnings, owner
  resolution, clone refuses a public repo) and src/sync/ui_tests.rs
  (the app: "enable sync" with no plan is refused, plan, one question,
  same turn and unclear answers refused, yes runs init in the
  background, status, pause and resume; join check, no, yes, clone;
  the web dialog's yes). Fixture homes and local bare repos (a
  `visibility` file in a bare repo stands in for GitHub's answer);
  without a fixture home the tools refuse in tests.
