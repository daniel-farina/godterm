# Assistant providers

The voice and typed assistant (the "brain") can run on different model providers. You switch between them any time: from the title menu of the assistant panel, by voice ("switch the assistant to Grok", "use Opus"), or with `assistant.provider` / `assistant.model` / `assistant.models.<id>` in config.toml (also `set_setting`). The conversation carries over the switch.

Every provider gets the same things:

- the same system prompt (sections and learned rules)
- the same godterm MCP tools, the confirm-token flow and the safety checks
- the follow-ups, the spoken sentence limit, and the output-token retry

All of that is GodTerm's, not the provider's.

## How it is built

`src/providers/mod.rs` holds the pieces:

- **`Backend`:** `send(text)`, `interrupt()`, `pid()`, `running()`. A backend streams `BrainEvent`s (text deltas, whole text blocks, tool calls, tool results, Done with cost and error) to the app as `AppEvent::Assistant(gen, event)`. The event shape is Claude Code's stream-json; `assistant::parse_line` reads it.
- **`Provider`:** an id, a name, and a few other fields:
  - `harness`: which GodTerm accounts can run it. None means it has its own login, in `own_home`.
  - `caps`: persistent process or one per turn, streaming, effort levels.
  - `models`: id, name and a short description each ("quickest"); the first one is the default.
  - `usage`: reads a fetched usage reply into buckets (label, percent left, reset time, which one binds). Claude has 5h and wk; Grok has wk and its products (Build). A provider that reports nothing returns none. The panel title and the account menu show them with no provider-specific code.
  - `bin`: the program to run.
  - `start`.
- **`PROVIDERS`:** the registry. The menus, `switch_assistant`, get_state's `providers` and the state block all read it.

## The two providers

- **`claude.rs`:** one long-lived `claude -p --input-format stream-json --output-format stream-json`.
  - It runs on a GodTerm Claude account's config dir.
  - It has no built-in tools; only godterm MCP tools, via `--mcp-config` with `--strict-mcp-config`.
- **`grok.rs`:** one `grok -p … --output-format streaming-messages-json --include-partial-messages` per turn. The conversation continues with `--session-id` on the first turn and `--resume` after that.
  - **Its own home and login:** it runs in `~/.godterm/assistant/grok-home`, with its own login (`grok login` there, via `assistant_login`). It never uses the user's Grok accounts.
  - **Isolated HOME:** `HOME` is set to that home too. Otherwise Grok imports the user's `~/.claude` and `~/.cursor` plugins, hooks and MCP servers.
  - **config.toml:** godterm's MCP server, with the `[compat]` imports off.
  - **Tools:** every built-in tool is removed (`--disallowed-tools`), the shell is denied (`--deny Bash(*)`), and godterm's tools are allowed (`--allow MCPTool(godterm__*)`; headless Grok cancels tool calls nobody approves). Grok reaches MCP tools through `search_tool` and `use_tool`; `parse_line` unwraps `use_tool` to the real tool.
  - **Refused login:** if the brain's own Grok login is refused (401, revoked), GodTerm says so once and switches back to Claude.

## Adding a provider

1. Add `src/providers/<name>.rs` with a `pub const PROVIDER: Provider` and a `start(StartCtx) -> Result<Box<dyn Backend>>`. Translate the provider's stream into `BrainEvent`s; `parse_line` already handles anything in the Messages API stream shape. Give it only godterm's MCP tools, its own home if it doesn't use a GodTerm account, and no built-in tools.
2. Add it to `PROVIDERS` and to the `pub mod` list in `mod.rs`.
3. If it has its own login, set `own_home`. It then needs a login command in `App::assistant_grok_login`'s style.

Nothing in the UI, the prompt or the tools needs to change.
