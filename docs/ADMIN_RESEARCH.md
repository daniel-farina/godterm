# Admin assistant: how Claude Code and Grok Build manage MCP servers, plugins and logins

Research for the assistant's admin tools (src/admin.rs, src/app_admin.rs). Checked against Claude Code 2.1.293 and Grok Build 1.0.45 docs, read only, with a scratch `CLAUDE_CONFIG_DIR`.

## Per account

Every GodTerm slot has its own config dir: `CLAUDE_CONFIG_DIR` for Claude Code, `GROK_HOME` for Grok. Each admin command runs with the slot's dir in that variable, the same way its tabs do, and the environment is scrubbed the same way. So MCP servers, plugins, skills and logins all differ per account.

## Claude Code

### MCP servers

- **`claude mcp add [-t stdio|sse|http] [-s local|user|project] [-e K=V] [-H "K: V"] [--client-id ID] [--callback-port N] <name> <url | -- command args>`**
  - The default scope is `local`, which applies only to the current project.
  - `user` is stored in `$CLAUDE_CONFIG_DIR/.claude.json` under `mcpServers`, so it applies to every project of that account. GodTerm installs here.
  - `project` writes `.mcp.json` in the folder.
- **`claude mcp add-json <name> <json>`, `get <name>`, `remove [-s scope] <name>`.**
- **`claude mcp list`** health-checks every server and prints one line each:
  - `linear: https://mcp.linear.app/mcp (HTTP) - ✓ Connected`
  - `plugin:slack:slack: https://mcp.slack.com/mcp (HTTP) - ! Needs authentication`
  - `claude.ai Gmail: https://… - ✓ Connected` (a claude.ai connector)
  - `name: npx … - ✗ Failed to connect`
  - Unapproved project servers show `⏸ Pending approval`.
- **`claude mcp login <name>`** (new in 2.1.29x) runs the OAuth flow for an HTTP or SSE server, or a claude.ai connector. It opens the browser, waits for the callback on a local port, and stores the token. `claude mcp logout <name>` clears it. In the TUI the same flow is `/mcp`, then picking the server and Authenticate.
- **Pre-registered OAuth clients:** some providers only accept their own registered OAuth client. Slack is one: its plugin carries `"oauth": {"clientId": …, "callbackPort": 3118}`. A plain `mcp add` then needs `--client-id` and `--callback-port`.
- **Sign-in state:** `$CLAUDE_CONFIG_DIR/mcp-needs-auth-cache.json` holds the names of servers still waiting for a sign in.

### Plugins

- **Commands:** `claude plugin install|uninstall|enable|disable|update|list|details <name@marketplace>`, and `claude plugin marketplace add|list|remove|update <source>`. A source is `owner/repo`, a git URL or a path.
- **Built-in marketplace:** `anthropic-plugin-directory` (the Anthropic directory, which holds the claude.ai connectors as plugins).
- **Official marketplace:** `claude-plugins-official` (`anthropics/claude-plugins-official`) is the common one, with 300+ plugins. Many of them bring an MCP server in `.mcp.json`, among them slack, github, linear, notion, sentry, stripe, figma, atlassian, asana, vercel, supabase, context7 and playwright.
- **Where installs are recorded:**
  - `$CLAUDE_CONFIG_DIR/plugins/installed_plugins.json` (version 2: `plugins.{id: [{scope, installPath, version, …}]}`)
  - `plugins/known_marketplaces.json`
  - `enabledPlugins` in `settings.json`
- **Plugin servers:** a plugin's MCP server shows up as `plugin:<plugin>:<server>`.
- **Plugin contents:** plugins can carry skills, agents, commands, hooks and MCP servers. Hooks also live in `settings.json` under `hooks`, keyed by event, and skills in `$CLAUDE_CONFIG_DIR/skills/<name>/`.

### claude.ai connectors

Google Drive, Gmail, Calendar and similar are enabled on the web at claude.ai/settings/connectors, per Claude login. Claude Code lists them as `claude.ai <Name>` servers, and `claude mcp login` works for them. They are not installed locally, so the catalog marks them `connector` and the assistant sends the user to the web page.

### Accounts

- **Login:** a fresh config dir runs onboarding (text style, then the login method), which prints `https://claude.ai/oauth/authorize?…` (wrapped across lines on narrow screens), tries to open the browser, and waits at `Paste code here if prompted >`. It ends with `Login successful. Press Enter to continue`. An onboarded dir uses `/login`.
- **Logout:** `claude auth logout`.

## Grok Build

- **MCP config:** `$GROK_HOME/config.toml`.
  - Servers live under `[mcp_servers.<name>]`: `command`/`args`/`env` for stdio, `url`/`headers`/`bearer_token_file` for remote. `enabled = false` and the `disabled_mcp_servers` list switch servers off.
  - `.grok/config.toml` in a repo is the project scope.
- **CLI:** `grok mcp list [--json] | add [--transport http|sse] [--scope user|project] [-e K=V] [--header …] <name> <url | -- cmd> | remove | enable | disable | doctor [--json]`.
- **OAuth:** Grok runs the browser flow itself. Remote servers are authenticated in the TUI: `/mcps`, select the server, press `i`. There is no CLI login. Tokens go to `$GROK_HOME/mcp_credentials.json` (0600). So for Grok the assistant installs with `grok mcp add` and guides the user through `/mcps` for the sign in.
- **Plugins:**
  - `grok plugin marketplace add <owner/repo | git URL | path>`.
  - `grok plugin install <source> --trust` (without `--trust` it only shows what it would activate).
  - `grok plugin list [--json] | uninstall <name> --confirm | enable | disable | details`.
  - Plugins are off until listed in `[plugins].enabled`. Installs live in `$GROK_HOME/installed-plugins/`.
- **Login:** `grok login` (GodTerm already uses it for Grok slots). Logout: `grok logout`.

## What GodTerm does with it

- **`account_capabilities`** reads only these keys: `mcpServers` and project `mcpServers` from `.claude.json`, `settings.json` (enabledPlugins, hooks), `installed_plugins.json`, `known_marketplaces.json`, the skills dir, and Grok's `config.toml`. It never reads credentials, OAuth data, headers or env values, and URLs lose their query. Live status comes from `mcp list`, run in the background.
- **`install_mcp`:**
  - It takes a catalog entry from data/mcp_catalog.json, an https URL, or a command. A Claude account gets the plugin, or `mcp add --scope user`; a Grok account gets `grok mcp add`.
  - It checks the result with `mcp list`. Then, for an OAuth server, it runs `claude mcp login <name>`, opens the URL that command prints, waits up to 5 minutes, checks again, and says each step.
  - All of this happens after one confirmation that shows the exact commands and accounts.
- **Plugin tools:** `install_plugin`, `remove_plugin`, `enable_plugin` and `disable_plugin` wrap `claude/grok plugin …`. They add the marketplace when the account does not know it, and the question says when a plugin is not from the official marketplace.
- **`add_account`, `relogin_account`, `logout_account`:** the slot's login runs in a pane. GodTerm watches the screen, presses Enter for the onboarding defaults, joins the wrapped URL and opens it once it stops growing, and says each step. At the code prompt the user copies the code and says "paste it". `login_paste_code` pastes the clipboard, only in a login that is waiting for a code and only on those words; the code is never logged or spoken. GodTerm then says whether the login succeeded or failed.
- **Settings tools:** `set_setting`, `get_setting` and `list_settings` use the Settings screen's schema. Values are validated and written with toml_edit. Risky keys ask a second time:
  - permission_mode bypass
  - update.require_signature off
  - update.enabled off
  - privacy off
  - pass_env
  - claude_bin and grok_bin

  Secrets are only taken from the user's own dictated words and are never read back.
