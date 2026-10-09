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
- **`grok.rs`:** one `grok -p … --output-format streaming-messages-json --include-partial-messages` per turn, at the assistant's effort (`--reasoning-effort`, low by default; Grok has no "none"). The conversation continues with `--resume`.
  - **The primer:** each session is opened by `grok -p /session-info --session-id <id>`, a local command with no model call (about 0.25 s, on the first turn only). Grok 1.0.45 applies `--system-prompt-override` only to a resumed session: a session opened by the user's first turn got Grok's own coding agent prompt for that turn (it answered "I'm Grok, an autonomous coding agent", looked for browser tabs and reached for the shell). After the primer every real turn is a resume with GodTerm's prompt. If the primer fails or hangs (15 s), the turn opens the session itself, as before.
  - **Its own home and login:** it runs in `~/.godterm/assistant/grok-home`, with its own login (`grok login` there, via `assistant_login`). It never uses the user's Grok accounts.
  - **Isolated HOME:** `HOME` is set to that home too. Otherwise Grok imports the user's `~/.claude` and `~/.cursor` plugins, hooks and MCP servers.
  - **config.toml:** godterm's MCP server, with the `[compat]` imports off.
  - **Tools:** every built-in tool is removed (`--disallowed-tools`; the shell is `run_terminal_cmd` there, `run_terminal_command` alone left it in), the shell is denied (`--deny Bash(*)`), and godterm's tools are allowed (`--allow MCPTool(godterm__*)`; headless Grok cancels tool calls nobody approves). Grok reaches MCP tools through `search_tool` and `use_tool`; `parse_line` unwraps `use_tool` to the real tool.
  - **Refused login:** if the brain's own Grok login is refused (401, revoked), GodTerm says so once and switches back to Claude.

## Grok speed (measured)

grok-4.7-build-fast on a GodTerm demo account (`HOME` and `GROK_HOME` on its dir), GodTerm's full prompt (about 20,000 characters) and its 82 tools from a stand-in MCP server. 5 runs each; seconds from starting the process to the first text and to the result line, median / p90.

| Turn | Before | After |
| --- | --- | --- |
| Plain, first text | 1.59 / 1.87 | 1.16 / 1.89 |
| Plain, result | 1.74 / 2.03 | 1.25 / 2.00 |
| One tool call, first text | 3.07 / 3.98 | 2.33 / 3.29 |
| One tool call, result | 3.70 / 4.31 | 2.55 / 3.51 |
| First tool call of a session, result | 5.53 / 6.08 (low effort, on Grok's own prompt) | 4.06 / 4.25 (primer included) |

"Before" is grok's default effort; "after" is low effort, the primer and the shell removed. Grok's own log shows where a turn goes: about 0.25 s from start to the model request, the rest is the model. Low effort cuts the reasoning before a tool call from about 200 to 300 tokens to a few dozen.

What did not help:

- **A leader:** headless `grok -p` always runs its agent in process (`connect_target: embedded` in grok's log), even with `[cli] use_leader` set through `GROK_CONFIG` and `--leader-socket`. There is nothing for a leader to save beyond the 0.25 s start.
- **One process for the conversation:** `grok agent stdio` (ACP) keeps a process, but its turns were no quicker (plain 1.89 / 2.36, one tool 4.08 / 4.79), and after `session/set_model` it answered with Grok's own prompt, not `systemPromptOverride`. `--prompt-json` is still one turn per process, and headless mode does not read stdin.
- **Skipping `search_tool`:** Grok adds its own reminder to call `search_tool` before `use_tool`, so the first tool call of a session makes one search (about 1 s). Listing every tool's parameters in the prompt (4,400 characters) stopped the search but made the turn slower (5.11 / 5.98). Grok searches once per session; later calls go straight to `use_tool`.

## Adding a provider

1. Add `src/providers/<name>.rs` with a `pub const PROVIDER: Provider` and a `start(StartCtx) -> Result<Box<dyn Backend>>`. Translate the provider's stream into `BrainEvent`s; `parse_line` already handles anything in the Messages API stream shape. Give it only godterm's MCP tools, its own home if it doesn't use a GodTerm account, and no built-in tools.
2. Add it to `PROVIDERS` and to the `pub mod` list in `mod.rs`.
3. If it has its own login, set `own_home`. It then needs a login command in `App::assistant_grok_login`'s style.

Nothing in the UI, the prompt or the tools needs to change.
