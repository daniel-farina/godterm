# CLI harnesses

GodTerm can launch Claude Code, Grok Build, Codex CLI, Cursor CLI,
Antigravity CLI and OpenCode in interactive PTYs. Install the desired CLI
first, then choose it in **Add account** (keys 1–6), `godterm setup`, or
the account's `harness` setting. Missing binaries appear in Settings >
Setup with installation guidance. Slots can mix different harnesses.

Everything below marked **verified** was checked against the real CLI on
macOS (codex 0.153.4, cursor-agent 2026.10.19, agy 1.4.4, opencode 2.0.32),
in a throwaway folder, with the screens kept as test fixtures in
`tests/fixtures/screens/<harness>_<state>.txt`. **Unverified** items were
taken from `--help` or documentation only, or could not be reached
(OpenCode had no account, so its prompts to a model were not exercised).

## Configuration

```toml
# Optional executable paths; otherwise GodTerm uses PATH.
# Cursor detects cursor-agent first, then agent.
codex_bin = "~/.local/bin/codex"
cursor_bin = "~/.local/bin/cursor-agent"
antigravity_bin = "~/.local/bin/agy"
opencode_bin = "~/.local/bin/opencode"

[[account]]
name = "codex-work"
harness = "codex"
cwd = "~/projects/work"
permission_mode = "default"
args = []

[[account]]
name = "cursor-work"
harness = "cursor"
cwd = "~/projects/work"
permission_mode = "plan"

[[account]]
name = "antigravity-work"
harness = "antigravity"
cwd = "~/projects/work"
permission_mode = "default"

[[account]]
name = "opencode-work"
harness = "opencode"
cwd = "~/projects/work"
permission_mode = "default"
```

`args` passes additional native CLI options (for example `--model`) to
each launch. Binary paths and harness choices can also be changed in
Settings. Restart existing tabs after changing their harness or binary.

## Login and isolation

| Harness | Executable | Login | Config / credentials |
| --- | --- | --- | --- |
| Claude Code | `claude` | Onboarding / `/login` | Isolated `CLAUDE_CONFIG_DIR` |
| Grok Build | `grok` | `grok login` | Isolated `GROK_HOME` |
| Codex CLI | `codex` | `codex login` (verified: subcommand exists) | Isolated `CODEX_HOME` in the slot's `config_dir` |
| Cursor CLI | `cursor-agent` or `agent` (both verified, one binary) | `agent login` (verified: subcommand exists) | Native store, shared between slots |
| Antigravity CLI | `agy` | Its own sign-in screen when the tab starts (no login subcommand, verified) | Isolated: the slot's `config_dir` is the CLI's `HOME` (macOS and Linux; see "Antigravity CLI in detail") |
| OpenCode | `opencode` | `opencode auth login` (verified: subcommand exists, not run) | Native store, shared between slots |

Codex starts with a fresh home under `~/.godterm/accounts/<name>`; sign in
there and configure any desired Codex settings in that home's
`config.toml`. GodTerm does not copy your existing credentials or config.
Do not point multiple Codex slots at the same `config_dir` if you need
separate accounts. API-key authentication still uses the native CLI's
environment; `CODEX_HOME` does not isolate inherited API keys.

Cursor and OpenCode slots reuse native configuration and
authentication. Multiple slots can run concurrently, but they do not
provide separate identities. GodTerm does not override `HOME` for them or
invent unsupported home variables. Use the CLI to manage its login; GodTerm's
logout action refuses them to avoid clearing a shared
login. Codex logout is also CLI-managed in this first integration.
Antigravity is the exception: each slot has its own home and login, and
GodTerm can log it out (see "Antigravity CLI in detail").

GodTerm labels these logins as **CLI-managed** and lets the CLI handle
authentication errors or onboarding when starting a tab. It does not
infer authentication from an unrelated Claude credential file. A login
command may exit after signing in; press Enter to start the interactive
agent afterward.

Folder trust: Codex, Cursor and Antigravity ask "do you trust this folder?"
on the first run in a directory, and accepting writes that folder into the
CLI's own native config (verified: `~/.codex/config.toml`,
`~/.gemini/antigravity-cli/settings.json`). GodTerm does not answer these
for you (its automatic trust is Claude's); they show as "needs approval" and
the approvals queue and voice can answer them (Codex `1` / `2`, Cursor `a` /
`q`, Antigravity Enter / arrows). Codex also offers to update itself on
start; that menu is likewise left to you.

## Permission modes

| Mode | Codex | Cursor | Antigravity | OpenCode |
| --- | --- | --- | --- | --- |
| `default` | Native default | Native default | Native default | Native default |
| `bypass` | `--dangerously-bypass-approvals-and-sandbox` | `--force` | `--dangerously-skip-permissions` | `--auto` |
| `accept-edits` | `--sandbox workspace-write --ask-for-approval on-request` | Native default | `--mode accept-edits` | Native default |
| `plan` | `--sandbox read-only` | `--mode plan` | `--mode plan` | the plan agent, from a per-launch config file (see below) |
| `manual` | `--ask-for-approval on-request` | Native default | Native default | Native default |
| `dont-ask` | `--ask-for-approval never` | Native default | Native default | Native default |
| `auto` | Native default | Native default | Native default | Native default |

Verified against each CLI's `--help` and the running TUI: Codex
`--sandbox read-only` (the `/status` screen then says "Read Only (Ask for
approval)"), `-a on-request|never` (this version accepts only those two);
Cursor `--mode plan` (footer shows "Plan"); Antigravity `--mode
plan|accept-edits|auto` (footer shows "plan"). The bypass flags exist in
`--help` for every CLI; none was ever used in testing.

**Contradicted by OpenCode 2.x:** the PR passed `--agent plan`; opencode
2.0.32 answers "Unrecognized flag: --agent in command opencode" and
exits. `--agent` exists only on `opencode run`. GodTerm now sets
`default_agent` to `plan` in a small JSON file named by `OPENCODE_CONFIG`
(verified: the TUI then starts on the Plan agent). The environment variable
layers over your own config, which is never edited, but only a private
server reads it, so these launches add `--standalone` (verified: without it
the TUI talks to the shared background service and ignores the file). If your
own environment already sets `OPENCODE_CONFIG`, GodTerm leaves it alone
and adds neither the file nor the flag. OpenCode's permission policy belongs
in its own config; `--auto` (bypass) is verified in `--help` only.

Unsupported modes leave the CLI's native policy in effect; GodTerm never
passes Claude's `--permission-mode` to these harnesses. Codex's read-only
sandbox is a filesystem restriction, not a dedicated planning agent. The
global default `permission_mode` is `bypass` (chosen for Claude and Grok).
A Codex, Cursor, Antigravity or OpenCode slot does **not** inherit that
default, because for Codex, Cursor and Antigravity it would switch off the
CLI's sandbox or approvals: such a slot runs in `default` unless its own
`permission_mode` is set (a global `plan`, `manual` and so on still apply).
Set `permission_mode = "bypass"` on the slot to opt in.

## Sessions and resume

Resume by explicit id, all **verified** (the tab reopened with its earlier
conversation and kept the launch's permission mode):

| Harness | Flag | Id format | Where the CLI keeps it |
| --- | --- | --- | --- |
| Codex | `codex [options] resume <id>` | UUID (v7) | `~/.codex/sessions/YYYY/MM/DD/rollout-<time>-<id>.jsonl`, first line `session_meta` with `cwd` |
| Cursor | `--resume <id>` | UUID | `~/.cursor/chats/<md5 of the cwd>/<id>/{meta.json,store.db}` and `~/.cursor/projects/<slug>/agent-transcripts/<id>/<id>.jsonl` |
| Antigravity | `--conversation <id>` | UUID | `~/.gemini/antigravity-cli/conversations/<id>.db` (+ `brain/<id>`); the cwd is only in `history.jsonl` |
| OpenCode | `--session <id>` (also creates the id if missing) | `ses_<alnum>` | the shared database (`opencode debug paths`), listed by `opencode session list` |

GodTerm only passes ids that are plain identifiers (letters, digits and
`._:-`, not starting with `-`); anything else starts a fresh conversation,
so a stored id can never be read as an option.

**The session index** (the assistant's `sessions` tool, the web tab
filter's Other sessions) lists Codex and Cursor sessions too
(src/session_other.rs): each Codex slot's `CODEX_HOME/sessions` rollouts
(a rollout whose `session_meta.source` names a subagent is skipped) and
the shared Cursor store's `projects/<slug>/agent-transcripts/<id>/<id>.jsonl`
(read once, as the first Cursor slot's; the folder is the existing path
the slug spells). Opening one resumes it on that slot. The main
`~/.codex` is not indexed (a slot cannot resume it without a copy), nor
is OpenCode's database. Each Antigravity slot's conversations are indexed
from its `brain/<id>/.system_generated/logs/transcript.jsonl`, and so are
the ones made with plain `agy` in your real home, as the source
"Antigravity (plain terminal)" (see "Antigravity CLI in detail"). The Codex and Cursor readers follow the
formats above but were checked only against fixtures.

**How GodTerm learns the id.** It reads no transcripts for these CLIs (the
formats are their own databases and change between versions) and cannot
tell which of several sessions in one folder is a tab's. What is reliable
is the farewell every one of them prints when you quit it (all **verified**):
`codex resume <id>`, `agent --resume=<id>`, `agy --conversation=<id>`,
`opencode -s <id>`. GodTerm keeps that id when a tab exits by itself, so
Enter, a restart of GodTerm or "reopen closed tab" resume the same
conversation. A tab GodTerm closes or kills (or a GodTerm that quits)
prints nothing and starts fresh next time; use the CLI's own resume picker
for those. Codex's `/status` and OpenCode's `session list` also show ids.
Antigravity does better: the CLI reports its conversation id to GodTerm
while it runs, so a killed tab resumes too.

Because ids are only known after a clean quit, and a move between slots
would need GodTerm to copy a session between homes (Codex keeps one home per
slot; the others share a store), **move, copy and failover stay refused**,
and **archive** needs a recorded id.

## Available features and limitations

All six harnesses support interactive keyboard input, paste, terminal
rendering, tabs, layouts, per-slot working directories and sending prompts
through GodTerm's voice/typed assistant. The new harnesses can autostart
without GodTerm inspecting their credentials. The assistant itself still
uses the existing Claude/Grok providers and requires its own login.

Usage meters (also Antigravity, from what its CLI reports), transcript
browsing/copy/move/takeover, quota failover and
MCP/plugin administration remain available for Claude/Grok only. GodTerm
does not query a Claude usage endpoint or run Claude admin commands for
the new harnesses. Manage those features in the native CLI.

Typing a prompt: Codex treats a prompt and its Enter that arrive together as
a paste, so the Enter must come as its own write after the text shows
(verified); GodTerm already sends prompts that way.

## Screens, activity and approvals (all verified)

GodTerm reads each CLI's own screen (`src/prompt_cli.rs`):

| | Working | Ready | Approval |
| --- | --- | --- | --- |
| Codex | `• Working (1s • esc to interrupt)` | `› Ask Codex to do anything` | `Would you like to run the following command?` with `› 1. Yes, proceed (y)`, `2. Yes, and don't ask again ... (p)`, `3. No, and tell Codex what to do differently (esc)`; MCP tools: `Allow the godterm MCP server to run tool "x"?` with 1 Allow, 2 Allow for this session, 3 Always allow, 4 Cancel |
| Cursor | `⠘⠣ Working` and `ctrl+c to stop` | `→ Add a follow-up` / `→ Plan, search, build anything` | `Run this command?` / `Run this MCP tool?` with `→ Run (once) (y)`, `Add Shell(x) to allowlist? (tab)` (MCP: `Allowlist MCP Tool`), `Run Everything (shift+tab)`, `Skip & tell the agent what to do instead (esc or n)` |
| Antigravity | spinner line (`⣽  Generating...`) and `esc to cancel` | `>` prompt and `? for shortcuts` | `Requesting permission for: <cmd>` `Run this command?` with `> 1. Yes, run command`, `2. Yes, and always allow in this conversation ...`, `3. ... (Persist to settings.json)`, `4. No, cancel` |
| OpenCode | spinner and `esc interrupt` | `Ask anything…` and `ctrl+p commands` | not captured (unverified) |

Keys GodTerm presses: Codex `y` approve, `p` always, Esc deny (digits for its
MCP prompt); Cursor `y`, Tab, `n`; Antigravity `1`, `2`, `4` (1 and 4 were pressed; 2 is unverified). "Always" means
the narrow one (this command, this conversation), and Cursor's "Run
Everything" is never chosen. After a Cursor `n` the CLI asks "Tell the agent
what to do instead": type a note and Enter, or Enter to send nothing. Interrupt
a turn with Esc (Codex, Antigravity, OpenCode) or Ctrl-C (Cursor). Quit with
Ctrl-C (once for Codex and OpenCode, twice for Cursor and Antigravity); each then
prints its resume line.

Folder trust and menus (Codex update, Cursor trust, Antigravity trust) count
as "needs approval". A screen GodTerm cannot read (OpenCode's permission
prompt) is answered with the keyboard, and the voice says so.

## The web app per harness (verified)

Checked with the real CLIs through GodTerm Web (claude 2.1.310, grok
1.0.69, codex 0.153.4, agy 1.4.4, opencode 2.0.32; Cursor showed only its
login screen inside the sandbox, so its rows are from its docs and the
demo) and kept as `scripts/e2e/web_matrix.py` (docs/TESTING.md):

| | New line in the prompt | Screen at the prompt (scrolling) | An image pasted as its path |
| --- | --- | --- | --- |
| Claude Code | ESC CR (sent) and Ctrl+J | main screen, no mouse: local history | `[Image #1]` |
| Grok Build | ESC CR (sent) | alternate screen with mouse (1003, SGR): wheel to the app | `Image: <path>` stays text (Grok's convention) |
| Codex | Ctrl+J (sent) and ESC CR | main screen, no mouse: local history | `[Image #1]` |
| Cursor | ESC CR (sent; unverified, login only) | not seen | not seen |
| Antigravity | ESC CR (sent) and Ctrl+J | main screen at the prompt (alternate for the trust menu): local history | the path stays text (also with `@`) |
| OpenCode | Ctrl+J (sent) and ESC CR | alternate screen with mouse: wheel to the app | `[Image 1]` |

The phone's Return sends the new line, Send and a desktop's Enter send
CR, Shift+Enter on a desktop sends the new line. A prompt typed into the
web app's Prompt box goes as a paste and its Enter as a separate write
once the tab shows the text: read together, Claude Code takes the CR as a
new line in the paste. All six turn on bracketed paste.

In a phone sized tab (about 42 to 50 columns) the CLIs wrap their prompts
and options; GodTerm reads Antigravity's folder trust, Codex's and
Antigravity's wrapped numbered options and Cursor's wrapped or cut
options (tests/fixtures/screens/*_narrow, *_wrapped, *_cut). A prompt it
cannot read (Codex's update menu) shows no Yes / No in the web app, only
Show, to answer it in the tab.

## Per-tab MCP (job and browser tools)

Each tab can have GodTerm's tab-scoped `godterm mcp` server (7 tools: the
job tools, `list_browsers`, `browser_status`), limited to that tab. It is
added per launch, never by editing your global config:

| Harness | Mechanism | Verified |
| --- | --- | --- |
| Claude | `--mcp-config` file | existing |
| Grok | GodTerm-owned `config.toml` in the slot | existing |
| Codex | `-c mcp_servers.godterm.command/args/env/env_vars` on the command line; the tab and token come from the environment (`env_vars`, so the token is not in `ps`); `default_tools_approval_mode="approve"`, because GodTerm asks you itself; the hint as `-c developer_instructions=...` (appended to the slot's own) | `/mcp` lists `godterm: connected (7 tools)`; the env reaches the server; a call ran without a prompt; the model quoted the instruction |
| Cursor | `--plugin-dir ~/.godterm/tab-mcp/cursor-t<uid>` with a `.cursor-plugin/plugin.json` and an `mcp.json` | `/mcp` lists `Godterm (plugin) - enabled`, 7 tools; env delivered; a call asked "Run this MCP tool?" (answer `y`) and returned. No rules/skills channel for a hint was found to load, so Cursor gets none; the tool descriptions say to prefer them over loops |
| OpenCode | `OPENCODE_CONFIG=~/.godterm/tab-mcp/opencode-t<uid>.json` (`mcp.servers.godterm`) plus `--standalone` | the TUI shows `1 MCP`, godterm `Connected`; env delivered; the shared service ignores the file (hence `--standalone`). The hint is unverified (no model), so none is sent |
| Antigravity | the slot's own `<config_dir>/.gemini/config/mcp_config.json` gets a `godterm` server (other servers in the file are kept, a server of that name that is not GodTerm's is left alone); the tab and token come from the tab's environment, which the CLI passes on to stdio servers. No prompt hint is sent. Not on Windows, and not for a slot whose `config_dir` is your real home | agy 1.5.18: a stdio server from that file was started with the tab's environment plus the file's `env`. A real tool call needs a signed-in account (unverified) |

Turn it off with `[tabs] jobs_tools = false`.

## Interaction with GodTerm's per-tab features

| Feature | Claude, Grok | Codex, Cursor, OpenCode | Antigravity |
| --- | --- | --- | --- |
| Model bar (Ctrl-a #, `set_tab_model`) | model picker | a bar with the loops and Browser buttons; "the model is chosen inside the CLI" | same |
| Browser button, Ctrl-a i, tab browser binding | yes | yes: the dialog binds the Chrome profile and the default profile; the agent learns it from `list_browsers` / `browser_status` | same as Codex (on Windows the tab tools are unsupported and the dialog says so) |
| Route (proxy per tab, web app) | proxy variables set; both document `HTTPS_PROXY`, `HTTP_PROXY`, `NO_PROXY` (no SOCKS: GodTerm gives an HTTP endpoint) | variables set; whether the CLI honours them is not checked | same |
| Tab job tools and prompt hint | yes | yes (hint: Codex only) | tools yes, no hint |
| BrowserSkill install | copies the skill into the account | skipped with a note: GodTerm copies it only into Claude and Grok homes. `bsk install-skill` itself knows Codex (`~/.agents/skills`) and Cursor (`~/.cursor/skills`), not Antigravity or OpenCode (`bsk install-skill --list`) | skipped |
| Move / copy to another slot, failover | same harness | refused (see Sessions) | refused |
| Archive | yes | only with a recorded session id; otherwise refused | same |
| Recently closed (reopen) | resumes the session | resumes when an id was recorded (a quit tab), else a fresh tab | resumes the session (the id is known while the tab runs) |
| Login state, email, plan, logout | yes | CLI-managed | signed in or not from the slot's token file; email and plan once a tab of the slot has run; logout removes the slot's token file |
| Usage in the account bar | yes | no | the quota the CLI last reported in a tab of the slot; nothing until then |
| Working / ready / needs you | screen | screen | the CLI's own state report, with the screen as the fallback |
| Voice or button approvals | keys read from the prompt | keys read from the prompt (above) | same |
| Message receipts | transcript (Claude), delivered or not (Grok) | delivered or not | same |

Receipts stay "delivered or not": Cursor's `agent-transcripts/*.jsonl` is a
plain `role`/`message` log and Codex's rollout is JSON lines too, but both
need to know which file belongs to the tab, and GodTerm only knows a session
id after a clean quit, so a reader would be guessing among sessions in the
same folder.

## Antigravity CLI in detail

Checked against agy 1.5.18 on macOS. "Verified" means seen with the real
binary in a throwaway home, with no Google account: the CLI was run in
its Gemini API key mode against a local stand-in for the model endpoint, so
every screen, file and report below except the signed-in ones is the real
CLI's. "Documented" means the official docs or the CLI's own `changelog`
say so. "Unverified" needs a signed-in account and is listed at the end.

### Where it keeps things, and one account per slot

- Everything is under the home directory: `~/.gemini/antigravity-cli/`
  (settings.json, keybindings.json, conversations, brain, log, cache) and
  `~/.gemini/config/` (mcp_config.json, hooks.json, plugins, projects).
  Documented, and verified.
- There is no variable or flag that moves it (documented: none; verified:
  `ANTIGRAVITY_GEMINI_DIR`, `ANTIGRAVITY_APP_DATA_DIR` and
  `ANTIGRAVITY_CONFIG_REL_DIR`, names found in the binary, change nothing).
  The only thing that moves it is `HOME` (verified).
- So a slot's `config_dir` (default `~/.godterm/accounts/<name>`) is given
  to the CLI as `HOME`. Its `.gemini` is the slot's own: login, settings,
  conversations, MCP servers, hooks and folder trust are per slot.
- The sign-in token would still be shared, because on macOS the CLI keeps
  it in the Keychain, which does not depend on `HOME`. GodTerm sets
  `AGY_CLI_FORCE_FILE_TOKEN_STORAGE=1`, and the CLI then keeps the token in
  a file in the slot (verified: its log says "Using file-based token storage
  because AGY_CLI_FORCE_FILE_TOKEN_STORAGE is set"). The variable is in the
  binary and the changelog speaks of file based storage, but it is not in
  the docs.
- The trade-off of a different `HOME`: commands the agent runs see it too.
  To keep git, ssh, npm, gh and the like working, every entry of your real
  home is linked into the slot's home by name when a tab starts (names only,
  nothing is read; `.gemini`, `.Trash` and GodTerm's own folder are left
  out; links whose target is gone are removed). What follows from that:
  an existing file or folder under `~` is your real one; a new top level
  file the agent creates directly in `~` lands in the slot's home, not in
  your real home; `~/.gemini/GEMINI.md` and your own `~/.gemini/config` are
  not seen by the slot; each slot shows the CLI's first run screens (colour
  scheme, terms) and asks to trust each folder once.
- To use your existing native login instead, set the slot's
  `config_dir = "~"`: nothing is isolated then, the Keychain is used as
  usual, GodTerm does not touch `mcp_config.json`, shows the login as
  CLI-managed and refuses to log out.
- Windows: no isolation (the CLI does not use `HOME` there), the state
  report and the tab tools are off, and screens are read as before.

### Login

- Documented: with no saved session the CLI shows its sign-in screen and
  opens the browser (Google OAuth), or prints a link and takes a pasted
  code when there is no browser; `agy acp --login` signs in by pasted code
  and exits; `GEMINI_API_KEY` needs `"modelProvider": "gemini"` in
  settings.json; `AGY_ADC_AUTH=true` uses Application Default Credentials;
  `/logout` removes the tokens.
- Verified: a signed out CLI starts on "Select login method: 1. Google
  OAuth, 2. Use a Google Cloud project". A Login tab is therefore just the
  CLI itself; once you are signed in the same tab is a normal session.
- Signed in or not: GodTerm only checks that the slot's token file
  (`.gemini/antigravity-cli/antigravity-oauth-token`) exists; it never
  reads it. Email and plan come from the CLI's state report (below) and
  are kept in `<config_dir>/.godterm-agy/account.json`, so they show after a
  tab of the slot has run once.
- Log out in GodTerm stops the slot's tabs and deletes that token file and
  the record.

### Conversations

- Verified: a conversation is `conversations/<id>.db` plus
  `brain/<id>/.system_generated/logs/transcript.jsonl` (one JSON object
  per step: `step_index`, `source`, `type` such as `USER_INPUT` and
  `PLANNER_RESPONSE`, `status`, `created_at`, `content`, and for model steps
  `model` and token counts), a title in `annotations/<id>.pbtxt`, and the
  last conversation of each folder in `cache/last_conversations.json`.
- The id is a UUID the CLI makes with the first prompt. A caller cannot
  choose it: `--conversation <unknown id>` starts a fresh conversation
  with another id (verified). `--conversation <id>` (also `--resume`, `-r`)
  resumes; `-c` continues the folder's last one. `/clear` (alias `/new`)
  starts a new id in the same process (verified); `/fork` makes a new id
  (documented).
- How GodTerm learns the id without guessing: the CLI runs a `title`
  command whenever its state changes and pipes its session as JSON to it
  (documented for the status line and the title; the two share one
  payload). GodTerm passes, for the session only, `--settings
  {"title":{"type":"command","command":...}}` whose command writes that
  JSON to a file named by the tab's own environment
  (`GODTERM_AGY_STATE`). The file carries `conversation_id`, so each tab
  knows its own conversation, also after `/clear`, and never looks at
  folders. The command prints nothing, so the window title is not changed.
  If the slot's `args` already have `--settings`, GodTerm adds nothing and
  falls back to the farewell line and the screen.
- The session index lists each slot's transcripts with the title, the
  first prompt, the model and the folder (the folder the tab reported, else
  the one in `last_conversations.json`, else none). Opening one resumes it
  on that slot.
- Move, copy and failover to another slot stay refused.

### Conversations made with plain `agy`

- Conversations you made by running `agy` in a plain terminal live in your
  real home (`~/.gemini/antigravity-cli`), which an isolated slot cannot
  see. GodTerm indexes that home as its own source, "Antigravity (plain
  terminal)": the Sessions page (Agent: Antigravity, Account: Main) and New
  tab > Existing sessions list them with title, first prompt, folder and
  time. It is left out when a slot has `config_dir = "~"` (that slot
  already lists them as its own), in demo mode and on a home inside the
  private vault. Only transcripts, titles, `cache/last_conversations.json`
  and `history.jsonl` are read; the token is never touched.
- The folder of such a conversation: `last_conversations.json` knows only
  the last conversation of each folder, so for the others GodTerm matches
  the conversation's prompts against `history.jsonl` (one line per typed
  prompt: `display`, `timestamp` in milliseconds, `workspace`; verified)
  by text and time (within two minutes). A conversation started with
  `-p` is not in the history and may show no folder; it then resumes in the
  slot's work folder.
- Opening one copies it into an Antigravity slot and resumes it there with
  `--conversation <id>`. GodTerm asks first and names the slot; the
  original is never changed, and nothing is ever written into the real
  home. Copied: `conversations/<id>.db` (and its `-wal`/`-shm` files if
  present), `brain/<id>/` and `annotations/<id>.pbtxt`. Not copied: the
  token, settings, the shared `conversation_summaries.db`, caches, symlinks.
  If the slot has no `config/projects/default-cli-project.json` yet, the
  one the CLI itself writes on first use is created.
- Verified with agy 1.5.18 between two throwaway homes (API key mode, local
  stand-in endpoint): `conversations/<id>.db` alone is what the CLI resumes
  from (with only `brain/<id>` it starts a fresh conversation under a new
  id); after the copy, `agy --conversation <id>` in the second home showed
  the earlier exchange, sent the earlier turns to the model with the next
  prompt and kept the id, in print mode and interactively. Without the
  default project file a brand new home showed a one time "Invalid
  Project" notice and still continued; with it there was none.
- Unverified: a conversation that belongs to a named project (not the
  default CLI project), and one made by a signed-in account resumed under
  another Google account.
- If the slot already has a conversation with that id (copied before), that
  copy opens and nothing is copied again; turns added later in the plain
  terminal are not merged.

### Working, ready, needs you

- The same JSON has `agent_state` (`idle`, `working`, `tool_use`,
  `reviewing`, `authenticating`, `initializing`, `error`) and
  `tool_confirmation_pending` (documented; `idle`, `initializing`,
  `authenticating` verified). GodTerm maps `working` and `tool_use` to
  working, `idle` to ready, a pending confirmation to needs approval, and
  leaves the rest to the screen. A prompt GodTerm can read on the screen
  always wins (that is what the approval keys come from), and a report
  older than 10 seconds loses to the screen when they disagree.
- The CLI also has terminal status reports over OSC 7501 (in its
  changelog; verified with `AGY_CLI_OSC7501=on`:
  `state=idle|working|done|blocked`, `kind=permission|question|auth`, a
  base64 `msg`). GodTerm does not use them: the CLI only sends them to a
  terminal that answers its query, and the title command says more.
- Hooks (`~/.gemini/config/hooks.json`: SessionStart, PreInvocation,
  PostInvocation, PreToolUse, PostToolUse, Stop, with `conversationId` on
  stdin) work in the CLI (verified) but GodTerm installs none: there is no
  hook for a permission prompt, and the title command already covers the
  rest.

### Models, effort, modes

- `agy models` lists the models of the signed-in account; `--model` and
  `--effort low|medium|high|xhigh|max` set them for a launch, `/model` and
  `/effort` inside the CLI (`/model <model> <prompt>` is for one prompt
  only). Put the flags in the slot's `args`. GodTerm has no model bar for
  Antigravity: there is no command that switches the model for the rest of
  a session without its picker.
- Modes: `--mode accept-edits|auto|plan` and
  `--dangerously-skip-permissions` (see Permission modes); Shift+Tab cycles
  them in the CLI.

### Usage

- Documented: the state JSON has `quota`, a map of bucket to
  `remaining_fraction` and `reset_time`. GodTerm shows those buckets in the
  account bar after a tab of the slot reported them, and nothing before.
  It never calls a quota endpoint itself. `/usage` in the CLI shows the
  same. An API key has no quota to show.

### Proxy, keys, input

- Proxy: not documented. The binary reads `HTTPS_PROXY` (Go's standard
  behaviour) and has `AGY_PROXY_STRICT_SSL`; GodTerm sets its route
  variables as for every tab. Whether sign-in and model traffic follow them
  is unverified (a test in API key mode through `HTTP_PROXY` reached no
  proxy).
- Esc stops a turn; Ctrl+C stops a turn, twice at an empty prompt quits
  and prints `agy --conversation=<id>`; Ctrl+D quits at an empty prompt.
- New line: Shift+Enter, Ctrl+J, Alt+Enter. Bracketed paste is on.
- A message typed while it works is queued until the turn ends
  (`queuedMessages: "queue"`, the default; `"send-immediately"`
  interrupts). GodTerm's voice and typed prompts therefore keep waiting for
  the turn to end; steering mid turn is documented only for `agy acp`.
- Slash commands of use here: `/clear` or `/new`, `/resume`, `/fork`,
  `/rename <name>`, `/compact`, `/model`, `/effort`, `/usage`, `/mcp`,
  `/logout`.

### Unverified (needs a signed-in account)

- That Google sign-in completes with the token kept in the slot's file
  (the forced file storage itself is verified, the file name is the one the
  CLI uses when it falls back from the Keychain).
- `email`, `plan_tier` and `quota` in the state JSON (documented, absent in
  API key mode), and therefore the email, plan and usage in the account
  bar.
- `tool_confirmation_pending` on a real approval prompt; a real call of a
  `godterm` tab tool.
- Proxy routes for sign-in and model traffic.
- `agy models` for a signed-in account (so no model list is shown).

## CLI references

- [Codex commands](https://developers.openai.com/codex/cli/reference/)
- [Cursor parameters](https://cursor.com/docs/cli/reference/parameters)
- [Antigravity getting started](https://antigravity.google/docs/getting-started?tab=cli),
  [CLI reference](https://antigravity.google/docs/cli/reference),
  [install and sign-in](https://antigravity.google/docs/install?tab=cli),
  [settings](https://antigravity.google/docs/settings?tab=cli),
  [conversations](https://antigravity.google/docs/cli/conversations),
  [status line](https://antigravity.google/docs/cli/statusline),
  [title](https://antigravity.google/docs/cli/title),
  [hooks](https://antigravity.google/docs/hooks),
  [MCP](https://antigravity.google/docs/mcp),
  [headless](https://antigravity.google/docs/cli/headless),
  [ACP](https://antigravity.google/docs/cli/acp),
  [usage](https://antigravity.google/docs/cli/commands/usage)
  (each page is also served as markdown at the same address plus `.md`)
- [OpenCode CLI](https://opencode.ai/docs/cli/)
- [OpenCode permissions](https://opencode.ai/docs/permissions/)
