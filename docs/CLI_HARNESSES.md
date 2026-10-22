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
| Antigravity CLI | `agy` | None: onboarding and `/login` inside the CLI (no login subcommand, verified) | Native store, shared between slots |
| OpenCode | `opencode` | `opencode auth login` (verified: subcommand exists, not run) | Native store, shared between slots |

Codex starts with a fresh home under `~/.godterm/accounts/<name>`; sign in
there and configure any desired Codex settings in that home's
`config.toml`. GodTerm does not copy your existing credentials or config.
Do not point multiple Codex slots at the same `config_dir` if you need
separate accounts. API-key authentication still uses the native CLI's
environment; `CODEX_HOME` does not isolate inherited API keys.

Cursor, Antigravity and OpenCode slots reuse native configuration and
authentication. Multiple slots can run concurrently, but they do not
provide separate identities. GodTerm does not override `HOME` or invent
unsupported home variables. Use the CLI to manage its login; GodTerm's
logout action refuses these new harnesses to avoid clearing a shared
login. Codex logout is also CLI-managed in this first integration.

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

Usage meters, transcript browsing/copy/move/takeover, quota failover and
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
| Antigravity | none per launch: `agy mcp add` writes the user-level `mcp_config.json`, and plugins install globally; there is no flag or environment variable (checked `--help` and the binary) | refused: "Antigravity CLI tabs have no GodTerm job or browser tools: the CLI only takes MCP servers from its global mcp_config.json, which GodTerm does not edit" |

Turn it off with `[tabs] jobs_tools = false`.

## Interaction with GodTerm's per-tab features

| Feature | Claude, Grok | Codex, Cursor, OpenCode | Antigravity |
| --- | --- | --- | --- |
| Model bar (Ctrl-a #, `set_tab_model`) | model picker | a bar with the loops and Browser buttons; "the model is chosen inside the CLI" | same |
| Browser button, Ctrl-a i, tab browser binding | yes | yes: the dialog binds the Chrome profile and the default profile; the agent learns it from `list_browsers` / `browser_status` | button present; its hint and the dialog say the tab tools are unsupported |
| Tab job tools and prompt hint | yes | yes (hint: Codex only) | refused, with the reason above |
| BrowserSkill install | copies the skill into the account | skipped with a note: GodTerm copies it only into Claude and Grok homes. `bsk install-skill` itself knows Codex (`~/.agents/skills`) and Cursor (`~/.cursor/skills`), not Antigravity or OpenCode (`bsk install-skill --list`) | skipped |
| Move / copy to another slot, failover | same harness | refused (see Sessions) | refused |
| Archive | yes | only with a recorded session id; otherwise refused | same |
| Recently closed (reopen) | resumes the session | resumes when an id was recorded (a quit tab), else a fresh tab | same |
| Voice or button approvals | keys read from the prompt | keys read from the prompt (above) | same |
| Message receipts | transcript (Claude), delivered or not (Grok) | delivered or not | same |

Receipts stay "delivered or not": Cursor's `agent-transcripts/*.jsonl` is a
plain `role`/`message` log and Codex's rollout is JSON lines too, but both
need to know which file belongs to the tab, and GodTerm only knows a session
id after a clean quit, so a reader would be guessing among sessions in the
same folder.

## CLI references

- [Codex commands](https://developers.openai.com/codex/cli/reference/)
- [Cursor parameters](https://cursor.com/docs/cli/reference/parameters)
- [Antigravity getting started](https://antigravity.google/docs/getting-started?tab=cli)
- [OpenCode CLI](https://opencode.ai/docs/cli/)
- [OpenCode permissions](https://opencode.ai/docs/permissions/)
