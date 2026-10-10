# CLI harnesses

GodTerm can launch Claude Code, Grok Build, Codex CLI, Cursor CLI,
Antigravity CLI and OpenCode in interactive PTYs. Install the desired CLI
first, then choose it in **Add account** (keys 1–6), `godterm setup`, or
the account's `harness` setting. Missing binaries appear in Settings >
Setup with installation guidance. Slots can mix different harnesses.

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
| Codex CLI | `codex` | `codex login` | Isolated `CODEX_HOME` in the slot's `config_dir` |
| Cursor CLI | `cursor-agent` or `agent` | `agent login` | Native store, shared between slots |
| Antigravity CLI | `agy` | Native onboarding / `/login` | Native store, shared between slots |
| OpenCode | `opencode` | Native `/connect` | Native store, shared between slots |

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

## Permission modes

| Mode | Codex | Cursor | Antigravity | OpenCode |
| --- | --- | --- | --- | --- |
| `default` | Native default | Native default | Native default | Native default |
| `bypass` | `--dangerously-bypass-approvals-and-sandbox` | `--force` | `--dangerously-skip-permissions` | Native default |
| `accept-edits` | `--sandbox workspace-write --ask-for-approval on-request` | Native default | `--mode accept-edits` | Native default |
| `plan` | `--sandbox read-only` | `--mode plan` | `--mode plan` | `--agent plan` |
| `manual` | `--ask-for-approval on-request` | Native default | Native default | Native default |
| `dont-ask` | `--ask-for-approval never` | Native default | Native default | Native default |
| `auto` | Native default | Native default | Native default | Native default |

Unsupported modes leave the CLI's native policy in effect; GodTerm never
passes Claude's `--permission-mode` to these harnesses. OpenCode permission
policy belongs in its native config. Codex's read-only sandbox is a
filesystem restriction, not a dedicated planning agent. Modes with
approximate equivalents retain the CLI's semantics. GodTerm's existing
global default is `bypass`; set a per-slot mode explicitly when desired.

## Available features and limitations

All six harnesses support interactive keyboard input, paste, terminal
rendering, tabs, layouts, per-slot working directories and sending prompts
through GodTerm's voice/typed assistant. The new harnesses can autostart
without GodTerm inspecting their credentials. The assistant itself still
uses the existing Claude/Grok providers and requires its own login.

Native resume arguments are `codex resume <id>`, `agent --resume <id>`,
`agy --conversation <id>` and `opencode --session <id>`. GodTerm uses these
when a tab already has an explicit session ID. This first integration
does **not** discover new session IDs or parse the four CLIs' transcripts.
Use the CLI's resume picker or `args` for existing native sessions;
restoring a fresh tab without a recorded ID starts a fresh conversation.

Usage meters, transcript browsing/copy/move/takeover, quota failover and
MCP/plugin administration remain available for Claude/Grok only. GodTerm
does not query a Claude usage endpoint or run Claude admin commands for
the new harnesses. Manage those features in the native CLI. Approval
detection and guided voice approvals also depend on each CLI's screen
format; use keyboard input for prompts GodTerm does not recognize.

## CLI references

- [Codex commands](https://developers.openai.com/codex/cli/reference/)
- [Cursor parameters](https://cursor.com/docs/cli/reference/parameters)
- [Antigravity getting started](https://antigravity.google/docs/getting-started?tab=cli)
- [OpenCode CLI](https://opencode.ai/docs/cli/)
- [OpenCode permissions](https://opencode.ai/docs/permissions/)
