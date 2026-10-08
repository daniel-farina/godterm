//! The control API: godterm's own actions as tools, for the assistant
//! (through `godterm mcp`) and anything else on this Mac that holds the
//! per-launch token. Tools reuse the same actions the voice grammar, the
//! palette and the mouse use.
//!
//! Safety is enforced here, not only in the assistant's prompt:
//! - batch tools (close_tabs, stop_loops, send_prompt to several tabs,
//!   answer_prompt for several tabs, a deny or a destructive approve)
//!   resolve their targets to an exact plan and return
//!   `needs_confirmation` with a token and one question; the token runs
//!   that plan, only from the user's next turn (their yes);
//! - tool results carry ground truth: a prompt is "delivered" only once
//!   claude took it;
//! - nothing here changes permission modes, credentials or settings files;
//! - every call is logged.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
#[cfg(windows)]
use uds_windows::{UnixListener, UnixStream};

use crate::app::{App, AppEvent, View};
use crate::pane::Activity;
use crate::prompt::Choice;

/// One tool: name, description, JSON schema of its arguments.
pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: fn() -> Value,
}

const TAB_DOC: &str = "Tab id from get_state (\"t12\"), or \"current\" (focused), \"last\" (the tab you last used), a tab name or folder name; a list of those; or \"all\", \"waiting\", \"working\".";
const ACCOUNT_DOC: &str =
    "Account number (1 based), label or name, or \"best\" (most 5 hour quota left).";

pub(crate) fn props(extra: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": extra, "required": required})
}

pub(crate) fn tab_prop() -> Value {
    json!({"description": TAB_DOC, "type": ["string", "array"], "items": {"type": "string"}})
}

pub(crate) fn account_prop() -> Value {
    json!({"description": ACCOUNT_DOC, "type": ["integer", "string"]})
}

pub(crate) fn token_prop() -> Value {
    json!({"type": "string", "description": "Token from a needs_confirmation reply; send it alone, only after the user clearly said yes in a later message"})
}

pub(crate) fn reissue_prop() -> Value {
    json!({"type": "string", "description": "An expired token: asks the very same question again (same plan, same text), never a rebuilt one"})
}

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "get_state",
        description: "Everything at once: accounts (usage left, logged in), every tab (id, account, name, folder, state, what a waiting tab asks, queued prompts, loops), loops, the focused tab and the tab you last used. Cheap; call it whenever the snapshot in the message is not enough.",
        schema: || props(json!({}), &[]),
    },
    Tool {
        name: "read_tab",
        description: "What a tab shows: what=\"reply\" (default) is the last reply its claude wrote; what=\"screen\" is the last lines of its screen.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "what": {"type": "string", "enum": ["reply", "screen"]}, "lines": {"type": "integer"}}),
                &[],
            )
        },
    },
    Tool {
        name: "recent_turns",
        description: "The last n exchanges in a tab's conversation (default 3, at most 10): each prompt and the reply claude gave, shortened. The first place to look for the status of ongoing work (\"what's the latest with X\", \"did that get fixed\"), before files or code.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "n": {"type": "integer"}}),
                &[],
            )
        },
    },
    Tool {
        name: "restart_to_update",
        description: "Install the downloaded GodTerm update and restart into it once this answer ends; every tab resumes its session. Only when the state shows update_ready and the user asked (\"update now\"). If tabs are working it returns needs_confirmation: ask the user, then call again with confirm true.",
        schema: || props(json!({"confirm": {"type": "boolean"}}), &[]),
    },
    Tool {
        name: "account_capabilities",
        description: "What each account has: harness, login, plan, usage, permission mode, MCP servers (name, transport, scope, status connected / needs-auth / failed / unknown), plugins (enabled or not, and the servers they bring), marketplaces, skills and hooks. account: one, a list or \"all\" (default all). refresh true re-checks server statuses (claude mcp list) in the background.",
        schema: || props(json!({"account": {"description": "Account(s): number, label, a list, or \"all\"", "type": ["integer", "string", "array"]}, "refresh": {"type": "boolean"}}), &[]),
    },
    Tool {
        name: "mcp_catalog",
        description: "The built in catalog of well known MCP servers and connectors (Slack, GitHub, Linear, Notion, Sentry, Stripe, Figma, Atlassian, Asana, Vercel, Supabase, Context7, Playwright, filesystem, Google Drive...): id, transport, auth, plugin, url or command, needed env. query filters.",
        schema: || props(json!({"query": {"type": "string"}}), &[]),
    },
    Tool {
        name: "install_mcp",
        description: "Install an MCP server on one or more accounts (one confirmation for all). source: a catalog name (\"slack\"), an https URL (with transport http or sse) or command (an argv list for a local stdio server; give name). On Claude a catalog entry with a plugin installs the plugin (via \"mcp\" adds the server directly). env: variables the server needs (values only from the user's words). It verifies with mcp list, then (connect, default true) opens the OAuth sign in in the browser and waits, saying each step. Unknown sources say so in the question.",
        schema: || props(json!({"account": {"description": "Account(s): number, label, a list, or \"all\"", "type": ["integer", "string", "array"]}, "source": {"type": "string"}, "name": {"type": "string"}, "transport": {"type": "string", "enum": ["http", "sse"]}, "command": {"type": "array", "items": {"type": "string"}}, "env": {"type": "object"}, "via": {"type": "string", "enum": ["plugin", "mcp"]}, "connect": {"type": "boolean"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "connect_mcp",
        description: "Sign in to an installed remote MCP server that needs auth (name as account_capabilities shows it, e.g. plugin:slack:slack): opens the browser sign in and waits until it is connected.",
        schema: || props(json!({"account": {"description": "Account(s)", "type": ["integer", "string", "array"]}, "name": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &["name"]),
    },
    Tool {
        name: "remove_mcp",
        description: "Remove an MCP server (user scope) from one or more accounts; verifies it is gone.",
        schema: || props(json!({"account": {"description": "Account(s)", "type": ["integer", "string", "array"]}, "name": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "install_plugin",
        description: "Install a plugin (name@marketplace) on one or more accounts; marketplace (owner/repo, git URL or path) is added first when the account does not know it. Plugins from outside the official marketplace say so in the question.",
        schema: || props(json!({"account": {"description": "Account(s)", "type": ["integer", "string", "array"]}, "plugin": {"type": "string"}, "marketplace": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "remove_plugin",
        description: "Uninstall a plugin from one or more accounts.",
        schema: || props(json!({"account": {"description": "Account(s)", "type": ["integer", "string", "array"]}, "plugin": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "enable_plugin",
        description: "Turn an installed plugin on, on one or more accounts.",
        schema: || props(json!({"account": {"description": "Account(s)", "type": ["integer", "string", "array"]}, "plugin": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "disable_plugin",
        description: "Turn an installed plugin off, on one or more accounts.",
        schema: || props(json!({"account": {"description": "Account(s)", "type": ["integer", "string", "array"]}, "plugin": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "list_settings",
        description: "GodTerm's settings by the Settings screen's schema: key, label, value (secrets only as saved / not set), kind and allowed values, help. query searches label, key and help; section filters (General, Layout, Accounts, Voice, Assistant, Memory).",
        schema: || props(json!({"query": {"type": "string"}, "section": {"type": "string"}}), &[]),
    },
    Tool {
        name: "get_setting",
        description: "One setting's value and allowed values; account for a per account setting.",
        schema: || props(json!({"key": {"type": "string"}, "account": account_prop()}), &["key"]),
    },
    Tool {
        name: "set_setting",
        description: "Change any setting (validated against its kind, written to config.toml keeping comments, applied at once). account for per account keys. Risky changes (bypass permissions, update checks or signatures off, privacy off, pass_env, claude_bin / grok_bin) need a second yes that names the risk. A secret (an API key) only from the user's own dictated words; never read back.",
        schema: || props(json!({"key": {"type": "string"}, "value": {"type": ["string", "number", "boolean"]}, "account": account_prop(), "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "add_account",
        description: "Add a Claude or Grok account (label, harness claude|grok, optional color and folder), then start its login in a pane and guide it by voice: opens the login page, says when to sign in and approve, waits for a code (the user copies it and says \"paste it\": login_paste_code), and says when the account is configured or the login failed.",
        schema: || props(json!({"label": {"type": "string"}, "harness": {"type": "string", "enum": ["claude", "grok"]}, "color": {"type": "string"}, "folder": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "relogin_account",
        description: "Start the login of an existing account again (expired or failed), guided like add_account.",
        schema: || props(json!({"account": account_prop(), "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "logout_account",
        description: "Log out of an account (its tabs stop).",
        schema: || props(json!({"account": account_prop(), "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "login_paste_code",
        description: "Paste the login code from the clipboard into the login waiting for one. Only when the user just said to paste it; the code is never shown, spoken or logged.",
        schema: || props(json!({"account": account_prop()}), &[]),
    },
    Tool {
        name: "admin_history",
        description: "The admin actions taken lately (installs, settings, accounts), newest last.",
        schema: || props(json!({}), &[]),
    },
    Tool {
        name: "switch_assistant",
        description: "Change your own provider, account and model at once (\"switch the assistant to Grok\", \"use Opus\", \"run on account 2\"); each is optional. The conversation carries over. get_state's providers lists them. Runs at once; say the result.",
        schema: || props(json!({"provider": {"type": "string"}, "account": account_prop(), "model": {"type": "string"}, "end_remote_control": {"type": "boolean", "description": "true once the user knows the switch ends Remote Control"}}), &[]),
    },
    Tool {
        name: "enable_remote_control",
        description: "Turn on Claude Code Remote Control for yourself: then claude.ai/code and the Claude app (signed in to the same claude.ai account) can talk to you, with your admin tools. Only on Claude. One confirmation. It ends when you switch account, provider or model. name: the session name shown there.",
        schema: || props(json!({"name": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "disable_remote_control",
        description: "Turn Remote Control off.",
        schema: || props(json!({}), &[]),
    },
    Tool {
        name: "assistant_login",
        description: "Sign in a provider that has its own login for you (the assistant's Grok, in its own home: the user's Grok accounts are never used). Opens the sign in in the browser and waits.",
        schema: || props(json!({"provider": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "setup_status",
        description: "What GodTerm needs and what is installed: Claude Code (required), Grok Build (optional), the local voice pack (ffmpeg, whisper.cpp and a model, espeak-ng and the Kokoro files, the speaker model), each with its status, the exact install plan here, the download size and whether it needs sudo; the no install paths.",
        schema: || props(json!({}), &[]),
    },
    Tool {
        name: "install_dependency",
        description: "Install what is missing: targets \"voice pack\", \"claude\", \"grok\", \"missing\", or item ids. One confirmation shows the exact commands and sizes; it runs in the background with spoken progress and is picked up live (sudo steps run in a visible tab for the user's password). whisper_model: large-v3-turbo (recommended, 1.6 GB) or small.en.",
        schema: || props(json!({"targets": {"type": "string"}, "whisper_model": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "close_accounts",
        description: "Close accounts in the grid: they leave the view but stay configured and logged in, and their tabs keep running (this is NOT a log out). accounts: numbers, labels, a list, \"all\", or a harness (\"grok\", \"claude\"); except: ones to keep open. Runs at once (reversible); say the result.",
        schema: || props(json!({"accounts": {"type": ["integer", "string", "array"]}, "except": {"type": ["integer", "string", "array"]}}), &["accounts"]),
    },
    Tool {
        name: "open_accounts",
        description: "Open closed accounts in the grid again, in place. only true: show only these and close the rest (\"only account 2 and 3\").",
        schema: || props(json!({"accounts": {"type": ["integer", "string", "array"]}, "except": {"type": ["integer", "string", "array"]}, "only": {"type": "boolean"}}), &["accounts"]),
    },
    Tool {
        name: "set_layout",
        description: "Arrange the grid. mode: auto, grid (with grid like \"2x2\"), columns, rows, focus. Or tree, any split: {\"split\": \"columns\"|\"rows\", \"sizes\": [2, 1], \"children\": [...]}, leaves {\"account\": N} or {\"rest\": true} (every other open account, stacked across). \"account 1 big on the left, the rest stacked on the right\" = {\"split\": \"columns\", \"sizes\": [2, 1], \"children\": [{\"account\": 1}, {\"rest\": true}]}; \"just account 2\" = {\"account\": 2}. Too small to fit falls back to auto. Runs at once.",
        schema: || props(json!({"mode": {"type": "string"}, "grid": {"type": "string"}, "tree": {"type": "object"}}), &[]),
    },
    Tool {
        name: "stop_waiting",
        description: "Stop watching for the answer to a question you sent a tab (tab), or to every one (no tab); the state's pending_answers lists them.",
        schema: || props(json!({"tab": tab_prop()}), &[]),
    },
    Tool {
        name: "list_dir",
        description: "List folders locally, instantly, without spending quota: one tab, a list or \"all\" in ONE call (each tab's listing comes back together), or a path inside the tab's folder. match filters names (\"*.html\"). depth 1 to 3. Entries are relative to the folder, which is returned absolute.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "path": {"type": "string"}, "depth": {"type": "integer"}, "match": {"type": "string"}}),
                &[],
            )
        },
    },
    Tool {
        name: "read_file",
        description: "The first lines of a text file inside a tab's folder, read locally without spending quota.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "path": {"type": "string"}, "lines": {"type": "integer"}}),
                &["path"],
            )
        },
    },
    Tool {
        name: "open_path",
        description: "Open files or folders inside the tabs' folders with their default app (an .html file opens in the browser), or http(s) URLs (localhost included) in the browser. paths: a list, absolute or relative to tab. Local and free; use it instead of asking a tab when it is only opening known files.",
        schema: || {
            props(
                json!({"paths": {"type": "array", "items": {"type": "string"}}, "tab": tab_prop()}),
                &["paths"],
            )
        },
    },
    Tool {
        name: "sessions",
        description: "Every Claude Code and Grok coding session on this Mac (what the user means by \"my sessions\"): every account (Claude Code and Grok) and the main ~/.claude and ~/.grok. Main sessions only (subagents roll up into their parent) unless include_subagents. Filters: text (the session's name, title, prompts and folder together; misheard names are fine, \"rarion refactor\" finds RARIUM REFACTOR), harness (claude, grok, all), source (an account, \"main\" or all), since and until (\"today\", \"yesterday\", \"last week\", \"3 days\", 2026-10-01), project (folder: a substring, a path, or \"~\" for sessions run in the home folder itself; a location the user mentions (\"it's in the home directory\", \"in botmesh\") is a project filter, not text), state (active: open in a tab or written to in the last minutes; recent; all), sort (the Sessions view's columns: modified (default), created, messages, tokens, context (largest single turn), project, source, title; oldest = modified reversed) and reverse, limit (10). Scripted runs (claude -p, the SDK, grok headless) and sessions in temp folders are left out unless include_headless; the reply's headless_hidden says how many. Results are grouped Claude first, then Grok. When nothing matches, did_you_mean lists the closest sessions: offer those, never invent others. If the reply says indexing: true, the first scan is still running: say it is still indexing (a few seconds) and call again.",
        schema: || {
            props(
                json!({
                    "text": {"type": "string"}, "harness": {"type": "string", "enum": ["claude", "grok", "all"]}, "source": {"type": ["string", "integer"]},
                    "since": {"type": "string"}, "until": {"type": "string"}, "project": {"type": "string"},
                    "state": {"type": "string", "enum": ["active", "recent", "all"]}, "sort": {"type": "string", "enum": ["modified", "created", "messages", "tokens", "context", "project", "source", "title", "recent", "oldest"]},
                    "reverse": {"type": "boolean"}, "limit": {"type": "integer"}, "include_subagents": {"type": "boolean"}, "include_headless": {"type": "boolean"}
                }),
                &[],
            )
        },
    },
    Tool {
        name: "session_detail",
        description: "A short account of one session (from sessions): its first prompt, the last few exchanges, files it touched and how it ended. Snippets only.",
        schema: || props(json!({"id": {"type": "string"}}), &["id"]),
    },
    Tool {
        name: "take_over_session",
        description: "Bring sessions running in other terminals (sessions marks them external) into GodTerm, in ONE call for all of them: id is a session id, a list of ids, or \"idle\" (every external session where nothing runs: not busy, no subagents, no background shells; see activity in sessions). One confirmation names each session (title, folder, state). mode take (default: stop it there gracefully and continue here) or copy. if_busy: wait (default) or interrupt. account: where they continue (default: their own, or the best of the same agent). name: rename the tab (one session). After the yes, the reply waits until each is open as a tab and gives its tab id, so you can go on (rename, send a prompt) in the same turn. Loops are set up again.",
        schema: || props(json!({"id": {"type": ["string", "array"], "items": {"type": "string"}}, "name": {"type": "string"}, "mode": {"type": "string", "enum": ["take", "copy"]}, "if_busy": {"type": "string", "enum": ["wait", "interrupt"]}, "account": account_prop(), "close_terminal": {"type": "boolean"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}), &[]),
    },
    Tool {
        name: "open_session",
        description: "Resume a session (from sessions) in a tab: on its own account, or for a main ~/.claude or ~/.grok session, copied into account (default: the best account of the same harness) first. name: rename its tab at once.",
        schema: || {
            props(
                json!({"id": {"type": "string"}, "account": account_prop(), "name": {"type": "string"}}),
                &["id"],
            )
        },
    },
    Tool {
        name: "open_tab",
        description: "Open a new claude tab on an account and, with prompt, give its claude that task. name: a short task name; the tab gets a new folder of that name. dir: a folder (path or recent project name) instead; a path that does not exist yet is created when its parent folder exists under the home folder (its say: \"Created ~/x and opened a session there on Account 4.\"); when the parent is missing too it returns an error naming it: ask the user once, and on yes call again with create true. Waits until the tab is ready and the prompt is accepted; returns the tab id and the delivery status.",
        schema: || {
            props(
                json!({"account": account_prop(), "name": {"type": "string"}, "dir": {"type": "string"}, "create": {"type": "boolean", "description": "The user said yes to creating dir and its missing parents"}, "prompt": {"type": "string"}, "count": {"type": "integer", "description": "Open this many such tabs at once (1 to 10), each in its own folder name-1, name-2..."}}),
                &[],
            )
        },
    },
    Tool {
        name: "send_prompt",
        description: "Give claude in one or more tabs a prompt to work on. Returns per tab: delivered (claude started on it), queued (the tab is busy or starting; it is sent once ready) or failed (with the reason). More than one tab needs one confirmation for the whole set. expect_reply true: you are asking the tab's agent a question for the user; GodTerm watches for its answer and starts a turn for you to tell the user when it arrives (default: true when the text or the user's request is a question).",
        schema: || {
            props(
                json!({"tab": tab_prop(), "account": account_prop(), "text": {"type": "string"}, "expect_reply": {"type": "boolean"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}),
                &["text"],
            )
        },
    },
    Tool {
        name: "answer_prompt",
        description: "Answer tabs waiting for approval: choice yes, always or no. tab may be one tab, a list or \"waiting\" (every waiting tab). Several tabs, a no, or a destructive command need one confirmation for the whole set.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "account": account_prop(), "choice": {"type": "string", "enum": ["yes", "always", "no"]}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}),
                &["choice"],
            )
        },
    },
    Tool {
        name: "close_tabs",
        description: "Close tabs: tab (one, a list or \"all\"), group (a tab group's name) and/or account (every tab of it). One idle tab the user names closes at once (with undo); a set needs one confirmation. Pinned tabs are left out of sets unless include_pinned.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "group": {"type": "string"}, "account": account_prop(), "include_pinned": {"type": "boolean"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}),
                &[],
            )
        },
    },
    Tool {
        name: "stop_loops",
        description: "Stop scheduled prompts (loops): job ids, or every loop of tab / account, or all=true. Needs one confirmation for the whole set.",
        schema: || {
            props(
                json!({"jobs": {"type": "array", "items": {"type": "string"}}, "tab": tab_prop(), "account": account_prop(), "all": {"type": "boolean"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}),
                &[],
            )
        },
    },
    Tool {
        name: "show",
        description: "Change what the screen shows: focus a tab (tab), a view (grid, dashboard, settings, sessions, loops, overview, approvals, livemap: the animated map of every session), zoom one pane (zoom true) or every pane (zoom false), a pane layout (auto, grid, columns, rows, focus). Any combination.",
        schema: || {
            props(
                json!({
                    "tab": tab_prop(),
                    "view": {"type": "string", "enum": ["grid", "dashboard", "settings", "sessions", "loops", "overview", "approvals", "help", "livemap"]},
                    "zoom": {"type": "boolean"},
                    "layout": {"type": "string", "enum": ["auto", "grid", "columns", "rows", "focus"]}
                }),
                &[],
            )
        },
    },
    Tool {
        name: "press_key",
        description: "Press a key in a tab: escape (interrupts claude's current work), enter, or ctrl_c. Enter is refused while the tab shows a permission or trust prompt: answer those with answer_prompt.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "key": {"type": "string", "enum": ["escape", "enter", "ctrl_c"]}}),
                &["key"],
            )
        },
    },
    Tool {
        name: "rename_tab",
        description: "Rename a tab.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "name": {"type": "string"}}),
                &["name"],
            )
        },
    },
    Tool {
        name: "pause_listening",
        description: "The user wants you to stop listening for a while (\"hold on\", \"give me five minutes\", \"pause for a sec\", \"stop listening for a bit\"). seconds: how long, read from what they said (a sec = 10, a minute = 60, five minutes = 300); leave it out when they gave no duration (the default from Settings). GodTerm does the pausing: it drops what is said until the wake word or the time is up, so anything that reaches you later is for you. Confirm in one short sentence, e.g. \"Sure, I'll wait two minutes. Say hey god if you need me sooner.\"",
        schema: || {
            props(
                json!({"seconds": {"type": "integer"}, "reason": {"type": "string"}}),
                &[],
            )
        },
    },
    Tool {
        name: "speaker",
        description: "Your voice, at once (no yes needed): muted true when the user wants you silent (\"mute your voice\", \"be quiet\", \"stop talking to me\", \"text only\"), false to talk again (\"speak again\", \"you can talk\"); while muted your answers show as text and nothing is said, and the mic keeps listening. volume: 0 to 100 (\"volume 50%\", \"louder\" = current + 20). speak_typed: whether replies to typed messages are spoken too (\"don't talk when I type\" = false). Not pause_listening: that stops hearing, this stops talking.",
        schema: || {
            props(
                json!({"muted": {"type": "boolean"}, "volume": {"type": ["number", "string"]}, "speak_typed": {"type": "boolean"}}),
                &[],
            )
        },
    },
    Tool {
        name: "assistant_panel",
        description: "How your panel sits, at once: docked (beside the panes, they make room: \"dock the panel\"), overlay (floats over the right side, the panes keep their size: \"make the panel float\"), auto (docked while every pane keeps about 80 columns).",
        schema: || {
            props(
                json!({"mode": {"type": "string", "enum": ["docked", "overlay", "auto"]}}),
                &["mode"],
            )
        },
    },
    Tool {
        name: "reopen_closed",
        description: "Reopen what was closed last (a tab, or every tab of a closed window or close_tabs call), resuming each session in its folder on its account (or the best one of the same agent) with its loops. index (0 = newest) reopens one entry of the recently closed list instead; get_state lists them as closed.",
        schema: || props(json!({"index": {"type": "integer"}}), &[]),
    },
    Tool {
        name: "move_pane",
        description: "Move a pane (a window: slot number as Ctrl-a 1..9 counts, or an account, default the focused one) to another place in the layout: to = left, right, up, down, or a slot number. It swaps places with the pane there; its tabs keep running.",
        schema: || {
            props(
                json!({"pane": {"type": ["integer", "string"]}, "to": {"type": ["integer", "string"]}}),
                &["to"],
            )
        },
    },
    Tool {
        name: "move_tab",
        description: "Move tabs (one, a list, \"all\", or a whole group by its name) to another account (number, label or \"best\") and continue their conversations there, in one call; copy=true keeps the originals. Groups come along. Returns each tab's new id.",
        schema: || {
            props(
                json!({"tab": tab_prop(), "group": {"type": "string"}, "to": account_prop(), "copy": {"type": "boolean"}}),
                &["to"],
            )
        },
    },
    Tool {
        name: "copy_session",
        description: "Copy a past session (id or id prefix) to an account so it can be resumed there.",
        schema: || {
            props(
                json!({"session": {"type": "string"}, "account": account_prop()}),
                &["session", "account"],
            )
        },
    },
    Tool {
        name: "set_mode",
        description: "Switch GodTerm modes: memory_saver (bool), privacy (bool, hides emails), show_emails (bool), voice (off, push, wake, open), free_memory (true: trim memory now), assistant_account (account, or \"best\"; whose quota you spend), new_conversation (true: start over after this turn). Any combination.",
        schema: || {
            props(
                json!({
                    "memory_saver": {"type": "boolean"}, "privacy": {"type": "boolean"}, "show_emails": {"type": "boolean"},
                    "voice": {"type": "string", "enum": ["off", "push", "wake", "open"]}, "free_memory": {"type": "boolean"},
                    "assistant_account": account_prop(), "new_conversation": {"type": "boolean"}
                }),
                &[],
            )
        },
    },
    Tool {
        name: "history",
        description: "YOUR OWN past conversations with the user (this assistant's chat log), newest first, each with when, how many requests and a short summary of what was asked, done and left pending. Filters: text (words in it), since and until (\"today\", \"yesterday\", \"3 hours\", \"2026-10-06\"), limit (5), id. action=detail with id gives its turns; action=resume continues one. Not coding sessions: for Claude Code or Grok sessions use sessions; for tabs opened and closed use tab_history.",
        schema: || {
            props(
                json!({"action": {"type": "string", "enum": ["list", "detail", "resume"]}, "text": {"type": "string"}, "query": {"type": "string"}, "since": {"type": "string"}, "until": {"type": "string"}, "limit": {"type": "integer"}, "id": {"type": "string"}}),
                &[],
            )
        },
    },
    Tool {
        name: "system_check",
        description: "Read-only checks of this machine, to verify before you claim or act (\"is anything still running in that session?\", \"is port 3000 free?\"). helper process_tree (pid) gives a process, its parents and children; helper port_owner (port) what listens on it. Or command: an allowlisted read-only command as a list of words, run without a shell: ps, pgrep, lsof (-i/-p/-c), top -l 1, vm_stat, df, du -sh, uptime, netstat -an, ls, stat, file, wc, head/tail, git status/log/branch/diff --stat (with tab: runs in that tab's folder), launchctl list, pmset -g, sw_vers, which, system_profiler. No pipes, no writing, no sudo, no private files; anything else is refused (ask a tab agent instead).",
        schema: || props(json!({"helper": {"type": "string", "enum": ["process_tree", "port_owner"]}, "pid": {"type": "integer"}, "port": {"type": "integer"}, "command": {"type": ["array", "string"], "items": {"type": "string"}}, "tab": tab_prop()}), &[]),
    },
    Tool {
        name: "tab_history",
        description: "Every tab opened, closed, moved, renamed, restarted, crashed or taken over (kept for weeks), newest first: \"what tabs did I close yesterday\", \"the last 10 tabs\", \"the tab I had in botmesh this morning\". Filters: since and until (\"today\", \"yesterday\", \"3 hours\", a date), text (name or folder, misheard names are fine), account, event (open, close, move, rename, restart, crash, takeover), limit (10). Each record has a record id and the session id; reopen_tab brings one back.",
        schema: || {
            props(
                json!({"since": {"type": "string"}, "until": {"type": "string"}, "text": {"type": "string"}, "account": {"type": ["string", "integer"]}, "event": {"type": "string"}, "limit": {"type": "integer"}}),
                &[],
            )
        },
    },
    Tool {
        name: "reopen_tab",
        description: "Resume a tab from tab_history (its record id, a session id, or a tab id such as t12, which resolves by the session it held) in its folder on its account (or the best one of the same agent), with its loops. name: rename it at once.",
        schema: || props(json!({"id": {"type": "string"}, "name": {"type": "string"}}), &["id"]),
    },
    Tool {
        name: "find",
        description: "Search everything at once for something the user remembers: coding sessions (all accounts and the main installs), the tab history, and your own past conversations. query can be a project, folder, topic or tab name, misheard is fine; since narrows it (\"yesterday\"). Returns ranked hits, each with its type (session, tab, chat) and the id to act on: open_session for a session, reopen_tab for a tab, history detail for a chat.",
        schema: || {
            props(
                json!({"query": {"type": "string"}, "since": {"type": "string"}, "limit": {"type": "integer"}}),
                &["query"],
            )
        },
    },
    Tool {
        name: "learn",
        description: "The user corrected you or stated a lasting preference that generalizes: save one short, specific, behavior-level rule (text) with why (reason); scope global (default), account or project. It joins your instructions from the next turn. Not for one-off facts. Refused if it would loosen safety (confirmations, approvals, credentials, permissions, pauses) or if it did not come from the user's own words.",
        schema: || {
            props(
                json!({"text": {"type": "string"}, "reason": {"type": "string"}, "scope": {"type": "string", "enum": ["global", "account", "project"]}}),
                &["text", "reason"],
            )
        },
    },
    Tool {
        name: "update_learning",
        description: "Change a learned rule's text (id from list_learnings), e.g. to merge related rules or sharpen one, with why.",
        schema: || {
            props(
                json!({"id": {"type": "integer"}, "text": {"type": "string"}, "reason": {"type": "string"}}),
                &["id", "text", "reason"],
            )
        },
    },
    Tool {
        name: "forget_learning",
        description: "Turn a learned rule off (\"forget that rule\"); it stays in the history and can be reverted.",
        schema: || {
            props(
                json!({"id": {"type": "integer"}, "reason": {"type": "string"}}),
                &["id"],
            )
        },
    },
    Tool {
        name: "list_learnings",
        description: "The rules you learned from the user (\"what have you learned?\", \"why do you do X?\"), with their reasons.",
        schema: || props(json!({"include_disabled": {"type": "boolean"}}), &[]),
    },
    Tool {
        name: "learning_history",
        description: "Every change to the learned rules, newest first, with before and after (\"what did you change?\"). id: one rule; since; limit.",
        schema: || {
            props(
                json!({"id": {"type": "integer"}, "since": {"type": "string"}, "limit": {"type": "integer"}}),
                &[],
            )
        },
    },
    Tool {
        name: "revert_learning",
        description: "Undo one change to the learned rules (\"undo the last change\"): rev from learning_history.",
        schema: || props(json!({"rev": {"type": "integer"}}), &["rev"]),
    },
    Tool {
        name: "group_tabs",
        description: "Put tabs in a named group in their account's tab list (\"group these three tabs as api\"), creating it; color optional (sage, sand, clay, slate, mauve, stone, or green, red, blue...). tab: ids, a list or \"all\"; account narrows.",
        schema: || props(json!({"tab": tab_prop(), "account": account_prop(), "group": {"type": "string"}, "color": {"type": "string"}}), &["group"]),
    },
    Tool {
        name: "ungroup",
        description: "Dissolve a tab group; its tabs stay open, ungrouped.",
        schema: || props(json!({"group": {"type": "string"}, "account": account_prop()}), &["group"]),
    },
    Tool {
        name: "rename_group",
        description: "Rename a tab group (name: the new name).",
        schema: || props(json!({"group": {"type": "string"}, "name": {"type": "string"}, "account": account_prop()}), &["group", "name"]),
    },
    Tool {
        name: "set_group_color",
        description: "A tab group's color (its tabs' default accent): sage, sand, clay, slate, mauve, stone (or green, red, blue...), or default.",
        schema: || props(json!({"group": {"type": "string"}, "color": {"type": "string"}, "account": account_prop()}), &["group", "color"]),
    },
    Tool {
        name: "collapse_group",
        description: "Collapse a tab group in the list to one line (collapsed false expands it).",
        schema: || props(json!({"group": {"type": "string"}, "collapsed": {"type": "boolean"}, "account": account_prop()}), &["group"]),
    },
    Tool {
        name: "pin_tab",
        description: "Pin tabs: first in their list, a question before closing, and left out of \"close all\" (close_tabs include_pinned closes them too).",
        schema: || props(json!({"tab": tab_prop(), "account": account_prop()}), &[]),
    },
    Tool {
        name: "unpin_tab",
        description: "Unpin tabs.",
        schema: || props(json!({"tab": tab_prop(), "account": account_prop()}), &[]),
    },
    Tool {
        name: "set_tab_color",
        description: "A tab's own accent color (over its group's): sage, sand, clay, slate, mauve, stone (or green, red, blue...), or default.",
        schema: || props(json!({"tab": tab_prop(), "account": account_prop(), "color": {"type": "string"}}), &["color"]),
    },
    Tool {
        name: "sort_tabs",
        description: "Sort an account's tab list: by manual (drag order), recent (activity), opened, name, folder or state (needs approval first). Pinned tabs stay first.",
        schema: || props(json!({"account": account_prop(), "by": {"type": "string", "enum": ["manual", "recent", "opened", "name", "folder", "state"]}}), &["by"]),
    },
    Tool {
        name: "get_system_prompt",
        description: "Your own instructions (GodTerm's assistant prompt, owned by the user): its sections with id, title, locked and the text. section: one of them. Read it before edit_system_prompt.",
        schema: || props(json!({"section": {"type": "string"}}), &[]),
    },
    Tool {
        name: "edit_system_prompt",
        description: "Change one section of your instructions when the user asks you to fix your behavior or your prompt: section id, and new_text (the whole section) or patch {find, replace}, with why. It answers needs_confirmation with a short diff; ask that once, and after a yes send only confirm_token. It applies from the next request. Locked sections (identity, safety) are refused, as is anything that loosens safety or comes from tab or file text.",
        schema: || {
            props(
                json!({"section": {"type": "string"}, "new_text": {"type": "string"}, "patch": {"type": "object", "properties": {"find": {"type": "string"}, "replace": {"type": "string"}}}, "why": {"type": "string"}, "confirm_token": token_prop(), "reissue_token": reissue_prop()}),
                &[],
            )
        },
    },
    Tool {
        name: "prompt_history",
        description: "Every change to your instructions, newest first, with before and after (\"what did you change in your prompt?\"). section: one; limit.",
        schema: || props(json!({"section": {"type": "string"}, "limit": {"type": "integer"}}), &[]),
    },
    Tool {
        name: "revert_prompt",
        description: "Undo one change to your instructions: rev from prompt_history.",
        schema: || props(json!({"rev": {"type": "integer"}, "why": {"type": "string"}}), &["rev"]),
    },
    Tool {
        name: "reset_section",
        description: "Put one section of your instructions back to GodTerm's default.",
        schema: || props(json!({"section": {"type": "string"}, "why": {"type": "string"}}), &["section"]),
    },
    Tool {
        name: "remember_summary",
        description: "Save one to three lines about this conversation for your future self (a new process after a restart reads it): what was asked, what you did (tab ids, folders, sessions), and anything left pending or unanswered. Call it when a conversation reaches a natural end (thanks, that's all, a pause) or after a significant piece of work; write nothing else for it.",
        schema: || props(json!({"summary": {"type": "string"}}), &["summary"]),
    },
    Tool {
        name: "ignore",
        description: "Drop what was just heard without replying: background talk, TV, a fragment or noise that is not addressed to you and does not follow from the conversation. Write nothing else in that turn.",
        schema: || props(json!({"reason": {"type": "string"}}), &[]),
    },
    Tool {
        name: "speak",
        description: "Say something out loud right now (rarely needed: your reply is spoken anyway).",
        schema: || props(json!({"text": {"type": "string"}}), &["text"]),
    },
];

/// A pending confirmation: the exact plan the user is asked about.
#[derive(Debug, Clone)]
pub struct Pending {
    pub token: String,
    pub tool: String,
    /// The resolved targets and parameters; the confirmed call runs this.
    pub plan: Value,
    pub summary: String,
    pub turn: u64,
    pub at: Instant,
}

impl Pending {
    /// How long ago it was asked, time the Mac slept included.
    pub fn age(&self) -> Duration {
        crate::clock::age(self.at)
    }

    /// Still answerable with its token (else it can only be asked again).
    pub fn answerable(&self) -> bool {
        self.age() < CONFIRM_TTL
    }
}

fn ok(v: Value) -> Value {
    json!({"ok": true, "result": v})
}

fn err(msg: impl Into<String>) -> Value {
    json!({"ok": false, "error": msg.into()})
}

/// Where a prompt is on its way into a tab.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Waiting for the tab to be ready.
    Queued,
    /// Pasted; Enter follows once the tab shows the text in its input.
    Pasted(Instant),
    /// Enter sent (again, when `retried`); `echoed`: the text was seen
    /// in the input box before it.
    Submitted {
        at: Instant,
        retried: bool,
        echoed: bool,
    },
    Delivered,
    Failed(String),
}

/// A prompt sent to a tab, followed until claude has accepted it.
#[derive(Debug, Clone)]
pub struct Delivery {
    pub id: u64,
    pub uid: u64,
    pub text: String,
    pub stage: Stage,
    pub created: Instant,
    /// The tab's last user message before this one (to see a new one).
    before: Option<String>,
    /// Its final status went out in a tool reply or a note.
    pub reported: bool,
}

impl Delivery {
    pub fn in_flight(&self) -> bool {
        matches!(self.stage, Stage::Pasted(_) | Stage::Submitted { .. })
    }
    pub fn done(&self) -> bool {
        matches!(self.stage, Stage::Delivered | Stage::Failed(_))
    }
    pub fn status(&self) -> (&'static str, Option<String>) {
        match &self.stage {
            Stage::Queued => (
                "queued",
                Some("the tab is not ready yet; it gets the prompt once it is".into()),
            ),
            Stage::Pasted(_) | Stage::Submitted { .. } => ("sending", None),
            Stage::Delivered => ("delivered", None),
            Stage::Failed(r) => ("failed", Some(r.clone())),
        }
    }
}

/// A tool reply held until its tab is ready or its prompts are accepted.
pub struct Waiting {
    pub reply: Value,
    pub tx: Sender<Value>,
    pub since: Instant,
    /// New tabs that must come up first.
    pub ready_uids: Vec<u64>,
    /// Deliveries to report on.
    pub deliveries: Vec<u64>,
    /// Also wait for queued deliveries (a new tab's first prompt).
    pub wait_queued: bool,
    pub max: Duration,
    /// Sessions being taken over: the reply waits for their tabs.
    pub takeovers: Vec<String>,
    /// Work done off the UI thread (system_check): its result is the reply.
    pub job: Option<std::sync::mpsc::Receiver<Value>>,
}

/// A safe folder name from a task name: "Simple Calculator!" -> "simple-calculator".
pub fn slug(name: &str) -> String {
    let s: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    s.chars().take(40).collect()
}

impl App {
    /// The id a tab has now: a moved tab continues under its new id.
    pub fn current_uid(&self, mut uid: u64) -> u64 {
        for _ in 0..16 {
            match self.moved_ids.get(&uid) {
                Some(n) => uid = *n,
                None => break,
            }
        }
        uid
    }

    /// Where a tab is, following moves.
    pub fn locate_uid(&self, uid: u64) -> Option<(usize, usize)> {
        self.find_tab(self.current_uid(uid))
    }

    /// A tab that is moving away (its new copy takes over).
    pub fn moving_away(&self, uid: u64) -> bool {
        self.moved_ids.contains_key(&uid)
    }
}

/// A tab's stable id, as the assistant sees it.
pub fn tab_id(uid: u64) -> String {
    format!("t{uid}")
}

pub fn plural(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// A confirmation question stays answerable this long (clarifying
/// questions and noise in between do not use it up).
const CONFIRM_TTL: Duration = Duration::from_secs(120);
/// Plans are kept this long so they can be asked again unchanged.
const KEEP_PLANS: Duration = Duration::from_secs(600);

/// A delivery: paste, then Enter as a write of its own once the tab
/// shows the text in its input box (so the two never reach claude in one
/// read, where the CR would be a new line), at the latest after this...
const ECHO_WAIT: Duration = Duration::from_millis(2500);
/// ...Enter is pressed once more if claude has not taken it after this...
const RETRY_AFTER: Duration = Duration::from_millis(1500);
/// ...and it fails after this.
const FAIL_AFTER: Duration = Duration::from_millis(6000);

/// The start of a prompt as the tab's input box shows it, to look for:
/// its first line's first words (long pastes show as "[Pasted text").
fn echo_head(text: &str) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    first
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(20)
        .collect()
}

/// Whether a tab's screen shows the pasted prompt (claude draws it, or a
/// "[Pasted text #1 +3 lines]" stand in for a long one).
pub fn shows_prompt(screen: &str, text: &str) -> bool {
    let head = echo_head(text);
    let flat = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    (!head.is_empty() && flat.contains(&head)) || flat.contains("[Pasted text")
}

impl App {
    /// A new folder for an assistant made tab: `<new_tab_base>/<name>` (or
    /// the dated name of the New Tab dialog), never the base itself.
    pub fn assistant_folder(&self, acct: usize, name: Option<&str>) -> anyhow::Result<PathBuf> {
        let base = self.cfg.base_for(acct);
        let stem = match name.map(slug).filter(|s| !s.is_empty()) {
            Some(s) => s,
            None => crate::picker::dated_name(
                &self.cfg.new_tab_name_pattern,
                &base,
                chrono::Local::now(),
            ),
        };
        let mut p = base.join(&stem);
        let mut n = 2;
        let in_use =
            |p: &std::path::Path| self.panes.iter().any(|s| s.tabs.iter().any(|t| t.cwd == p));
        while (p.exists()
            && std::fs::read_dir(&p)
                .map(|mut d| d.next().is_some())
                .unwrap_or(true))
            || in_use(&p)
        {
            p = base.join(format!("{stem}-{n}"));
            n += 1;
        }
        std::fs::create_dir_all(&p)?;
        Ok(p)
    }

    /// Send a prompt to a tab and follow it until claude accepts it. A tab
    /// that is not ready gets it later. Returns the delivery id.
    pub fn deliver(&mut self, uid: u64, text: &str) -> u64 {
        self.delivery_seq += 1;
        let id = self.delivery_seq;
        let before = self
            .find_tab(uid)
            .and_then(|(s, t)| self.last_user_text(s, t));
        crate::log::info(&format!(
            "deliver: queued #{id} for {} ({} chars)",
            tab_id(uid),
            text.len()
        ));
        self.deliveries.push(Delivery {
            id,
            uid,
            text: text.to_string(),
            stage: Stage::Queued,
            created: Instant::now(),
            before,
            reported: false,
        });
        self.deliveries_tick();
        id
    }

    /// Paste now; Enter is a step of its own (deliveries_tick), never on a
    /// timer: a CR in the same read as the paste reads as a new line, and
    /// a busy machine bunches timed writes together.
    fn paste_prompt(&mut self, s: usize, t: usize, text: &str) {
        let tab = &mut self.panes[s].tabs[t];
        tab.reset_scroll();
        // Always bracketed: claude reads ESC [200~ even before it turns the
        // mode on, and a multi line prompt then stays one prompt.
        tab.write(&crate::keys::encode_paste(text, true));
    }

    fn tab_screen(&self, s: usize, t: usize) -> String {
        self.panes[s].tabs[t]
            .parser
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .screen()
            .contents()
    }

    /// The last prompt the user (or we) gave claude in a tab, from its transcript.
    pub fn last_user_text(&self, s: usize, t: usize) -> Option<String> {
        let p = self.transcript_path(s, t)?;
        crate::sessions::last_user_text(&p)
    }

    /// True once claude has taken the prompt: it works on it, its
    /// transcript has it, or the text it showed left its input box.
    fn accepted(&self, d: &Delivery, s: usize, t: usize, echoed: bool) -> bool {
        let tab = &self.panes[s].tabs[t];
        if matches!(tab.activity, Activity::Working | Activity::Permission) {
            return true;
        }
        if echoed
            && tab.activity != Activity::Exited
            && !shows_prompt(&self.tab_screen(s, t), &d.text)
        {
            return true;
        }
        let head: String = d
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(40)
            .collect();
        match self.last_user_text(s, t) {
            Some(u) if Some(&u) != d.before.as_ref() => u
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .starts_with(&head),
            _ => false,
        }
    }

    /// Move every delivery along: paste when the tab is ready, Enter, check,
    /// one more Enter, then delivered or failed.
    pub fn deliveries_tick(&mut self) {
        let mut ds = std::mem::take(&mut self.deliveries);
        for d in &mut ds {
            let now = self.current_uid(d.uid);
            if now != d.uid && !d.done() {
                crate::log::info(&format!(
                    "deliver: {} moved, following it to {}",
                    tab_id(d.uid),
                    tab_id(now)
                ));
                d.uid = now;
                if d.in_flight() {
                    // Typed into the old copy: type it into the new one.
                    d.stage = Stage::Queued;
                }
            }
            let Some((s, t)) = self.find_tab(d.uid) else {
                if !d.done() {
                    d.stage = Stage::Failed("the tab was closed".into());
                }
                continue;
            };
            match d.stage.clone() {
                Stage::Queued => {
                    let tab = &self.panes[s].tabs[t];
                    if tab.activity == Activity::Ready && tab.is_running() {
                        crate::log::info(&format!(
                            "deliver: pasting into {} ({} chars)",
                            tab_id(d.uid),
                            d.text.len()
                        ));
                        let text = d.text.clone();
                        self.paste_prompt(s, t, &text);
                        d.stage = Stage::Pasted(Instant::now());
                    } else if !tab.is_running()
                        && tab.pending.is_none()
                        && !tab.suspended
                        && d.created.elapsed() > Duration::from_secs(5)
                    {
                        d.stage = Stage::Failed("the tab is not running".into());
                    } else if d.created.elapsed() > Duration::from_secs(600) {
                        d.stage = Stage::Failed("the tab never became ready (10 minutes)".into());
                    }
                }
                Stage::Pasted(at) => {
                    // Enter once the text is in claude's input box.
                    let echoed = shows_prompt(&self.tab_screen(s, t), &d.text);
                    if echoed || at.elapsed() >= ECHO_WAIT {
                        if !echoed {
                            crate::log::info(&format!(
                                "deliver: {} shows no echo of the paste, pressing Enter anyway",
                                tab_id(d.uid)
                            ));
                        }
                        self.panes[s].tabs[t].write(b"\r");
                        d.stage = Stage::Submitted {
                            at: Instant::now(),
                            retried: false,
                            echoed,
                        };
                    }
                }
                Stage::Submitted {
                    at,
                    retried,
                    echoed,
                } => {
                    if self.accepted(d, s, t, echoed) {
                        crate::log::info(&format!(
                            "deliver: {} accepted the prompt",
                            tab_id(d.uid)
                        ));
                        d.stage = Stage::Delivered;
                    } else if !retried && at.elapsed() >= RETRY_AFTER {
                        // Still sitting in the input box: Enter once more.
                        crate::log::info(&format!(
                            "deliver: {} did not take it, pressing Enter again",
                            tab_id(d.uid)
                        ));
                        self.panes[s].tabs[t].write(b"\r");
                        d.stage = Stage::Submitted {
                            at,
                            retried: true,
                            echoed,
                        };
                    } else if at.elapsed() >= FAIL_AFTER {
                        crate::log::info(&format!(
                            "deliver: {} never started on the prompt",
                            tab_id(d.uid)
                        ));
                        d.stage = Stage::Failed("typed in, but claude did not start on it; the text may still be in its input box".into());
                    }
                }
                Stage::Delivered | Stage::Failed(_) => {}
            }
        }
        // Keep finished ones a while so replies and the state can report them.
        ds.retain(|d| !d.done() || d.created.elapsed() < Duration::from_secs(900));
        if ds.len() > 50 {
            let cut = ds.len() - 50;
            ds.drain(..cut);
        }
        self.deliveries = ds;
    }

    /// Prompts still waiting to go into a tab.
    pub fn queued_for(&self, uid: u64) -> usize {
        self.deliveries
            .iter()
            .filter(|d| d.uid == uid && !d.done())
            .count()
    }

    fn delivery_json(&self, id: u64) -> Value {
        match self.deliveries.iter().find(|d| d.id == id) {
            Some(d) => {
                let (status, reason) = d.status();
                let mut v = json!({"tab": tab_id(d.uid), "status": status});
                if let Some(r) = reason {
                    v["reason"] = json!(r);
                }
                if !d.done() {
                    v["age_s"] = json!(d.created.elapsed().as_secs());
                }
                v
            }
            None => json!({"status": "unknown"}),
        }
    }

    /// Deliver queued prompts and finish held replies once tabs are ready.
    pub fn control_tick(&mut self) {
        self.deliveries_tick();
        let ws = std::mem::take(&mut self.ctl_waits);
        for mut w in ws {
            if let Some(rx) = w.job.as_ref() {
                match rx.try_recv() {
                    Ok(v) => {
                        let _ = w.tx.send(if v.get("error").is_some() {
                            err(v["error"].as_str().unwrap_or("failed").to_string())
                        } else {
                            ok(v)
                        });
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) if w.since.elapsed() <= w.max => {
                        self.ctl_waits.push(w)
                    }
                    _ => {
                        let _ = w.tx.send(err("the check did not finish in time"));
                    }
                }
                continue;
            }
            let up = |app: &App, uid: u64| {
                app.locate_uid(uid).is_some_and(|(s, t)| {
                    let tab = &app.panes[s].tabs[t];
                    tab.is_running() && tab.activity != Activity::Starting
                })
            };
            let dead = |app: &App, uid: u64| match app.locate_uid(uid) {
                Some((s, t)) => {
                    !app.panes[s].tabs[t].is_running() && app.panes[s].tabs[t].pending.is_none()
                }
                None => true,
            };
            let all_up = w.ready_uids.iter().all(|u| up(self, *u) || dead(self, *u));
            let all_dead = !w.ready_uids.is_empty()
                && w.since.elapsed() > Duration::from_secs(3)
                && w.ready_uids.iter().all(|u| dead(self, *u));
            let busy = w.deliveries.iter().any(|id| {
                self.deliveries.iter().any(|d| {
                    d.id == *id && (d.in_flight() || (w.wait_queued && d.stage == Stage::Queued))
                })
            });
            // A take-over is done once its session runs in a tab here, or
            // the take-over itself ended (failed).
            let takeover_tab = |app: &App, sid: &str| {
                app.panes
                    .iter()
                    .flat_map(|p| p.tabs.iter())
                    .find(|t| {
                        t.session_id.as_deref() == Some(sid)
                            && t.is_running()
                            && t.activity != Activity::Starting
                    })
                    .map(|t| t.uid)
            };
            let takeovers_done = w.takeovers.iter().all(|sid| {
                takeover_tab(self, sid).is_some()
                    || !self.takeovers.iter().any(|t| {
                        t.live.session_id == *sid
                            && !matches!(
                                t.stage,
                                crate::takeover::Stage::Done(_) | crate::takeover::Stage::Failed(_)
                            )
                    })
            });
            let finished =
                (all_up && !busy && takeovers_done) || all_dead || w.since.elapsed() > w.max;
            if !finished {
                self.ctl_waits.push(w);
                continue;
            }
            if all_dead {
                let _ =
                    w.tx.send(err("the new tab did not start (see the tab for the error)"));
                continue;
            }
            let tabs: Vec<Value> = w
                .ready_uids
                .iter()
                .map(|u| {
                    let now = self.current_uid(*u);
                    json!({"tab": tab_id(now), "state": self.locate_uid(*u).map(|(s, t)| self.state_word(s, t)).unwrap_or("closed")})
                })
                .collect();
            let ds: Vec<Value> = w
                .deliveries
                .iter()
                .map(|id| self.delivery_json(*id))
                .collect();
            for id in &w.deliveries {
                if let Some(d) = self.deliveries.iter_mut().find(|d| d.id == *id) {
                    d.reported = d.done();
                }
            }
            if !w.takeovers.is_empty() {
                let mut tabs = vec![];
                let mut pending = vec![];
                for sid in &w.takeovers {
                    match takeover_tab(self, sid) {
                        Some(uid) => {
                            let (s, t) = self.locate_uid(uid).unwrap_or((0, 0));
                            let mut e = json!({"session": sid, "tab": tab_id(uid), "name": self.panes[s].tabs[t].name(), "state": self.state_word(s, t)});
                            if self.forced_stops.contains(sid) {
                                e["forced"] = json!(true);
                            }
                            tabs.push(e);
                        }
                        None => {
                            let why = self
                                .takeovers
                                .iter()
                                .find(|t| t.live.session_id == *sid)
                                .map(|t| format!("{:?}", t.stage))
                                .unwrap_or_else(|| "ended".into());
                            pending.push(json!({"session": sid, "stage": why}));
                        }
                    }
                }
                let said = match (tabs.len(), pending.len()) {
                    (n, 0) => format!("Took over {}: {}.", plural(n, "session"), tabs.iter().map(|t| format!("{} is now {}", t["name"].as_str().unwrap_or(""), t["tab"].as_str().unwrap_or(""))).collect::<Vec<_>>().join(", ")),
                    (n, p) => format!("{} here so far; {} still being stopped there (it opens as a tab when it has).", plural(n, "session"), plural(p, "session")),
                };
                let said = if tabs.iter().any(|t| t["forced"] == json!(true)) {
                    format!(
                        "{said} The old process did not answer Ctrl-C, so it was stopped by force."
                    )
                } else {
                    said
                };
                w.reply["tabs"] = json!(tabs);
                if !pending.is_empty() {
                    w.reply["still_moving"] = json!(pending);
                }
                w.reply["say"] = json!(said);
                let _ = w.tx.send(ok(w.reply));
                continue;
            }
            if w.ready_uids.len() == 1 {
                w.reply["state"] = tabs[0]["state"].clone();
                if let Some(d) = ds.first() {
                    w.reply["prompt"] = d.clone();
                }
            } else if !w.ready_uids.is_empty() {
                w.reply["tabs"] = json!(tabs);
            }
            if !ds.is_empty() {
                w.reply["deliveries"] = json!(ds);
            }
            let mut said = say_deliveries(&ds, w.ready_uids.len());
            if let Some(warn) = w.reply["warning"].as_str() {
                said = format!("{said} {warn}");
            }
            w.reply["say"] = json!(said);
            let _ = w.tx.send(ok(w.reply));
        }
        // Outcomes that came after the tool replied: tell the assistant
        // (and the user) so nothing is left unknown.
        let late: Vec<(String, String)> = self
            .deliveries
            .iter_mut()
            .filter(|d| d.done() && !d.reported)
            .map(|d| {
                d.reported = true;
                let (st, why) = d.status();
                (
                    tab_id(d.uid),
                    match why {
                        Some(w) if st == "failed" => format!("failed: {w}"),
                        _ => st.to_string(),
                    },
                )
            })
            .collect();
        for (tab, what) in late {
            crate::log::info(&format!("deliver: {tab} {what}"));
            self.assistant_note(format!("Prompt to {tab}: {what}"));
        }
    }

    /// An account from a tool argument: 1 based number, "a2", label or
    /// name, "best" or "current".
    pub fn account_arg(&self, v: &Value) -> Result<usize, String> {
        let n = self.cfg.accounts.len();
        let by_num = |k: usize| {
            k.checked_sub(1)
                .filter(|i| *i < n)
                .ok_or_else(|| format!("there is no account {k} (there are {n})"))
        };
        match v {
            Value::Number(x) => by_num(x.as_u64().unwrap_or(0) as usize),
            Value::String(s) => {
                let s = s.trim();
                let low = s.to_lowercase();
                if let Ok(k) = low
                    .trim_start_matches("account")
                    .trim_start_matches('a')
                    .trim()
                    .parse::<usize>()
                {
                    return by_num(k);
                }
                match low.as_str() {
                    "best" => self
                        .best_account()
                        .map(|(i, _)| i)
                        .ok_or_else(|| "no logged in account has usage data yet".to_string()),
                    "current" | "focused" | "this" => self
                        .panes
                        .get(self.focus)
                        .and_then(|p| p.account)
                        .ok_or_else(|| "the focused pane has no account".to_string()),
                    _ => match self.cfg.accounts.iter().position(|a| {
                        a.name.eq_ignore_ascii_case(s) || a.display().eq_ignore_ascii_case(s)
                    }) {
                        Some(i) => Ok(i),
                        // A misheard label: the closest account.
                        None => {
                            let idx: Vec<String> = (0..n).map(|i| i.to_string()).collect();
                            let labels: Vec<(&str, &str)> = self
                                .cfg
                                .accounts
                                .iter()
                                .enumerate()
                                .flat_map(|(i, a)| {
                                    [
                                        (idx[i].as_str(), a.name.as_str()),
                                        (idx[i].as_str(), a.display()),
                                    ]
                                })
                                .collect();
                            match crate::fuzzy::best(s, labels) {
                                crate::fuzzy::Match::Sure(i) => {
                                    let i: usize = i.parse().unwrap_or(0);
                                    self.corrections.borrow_mut().push((
                                        s.to_string(),
                                        self.cfg.accounts[i].display().to_string(),
                                    ));
                                    Ok(i)
                                }
                                crate::fuzzy::Match::Unsure(c) => Err(format!(
                                    "no account called {s}; did you mean: {}",
                                    c.iter()
                                        .filter_map(|c| c.value.parse::<usize>().ok())
                                        .map(|i| self.cfg.accounts[i].display().to_string())
                                        .collect::<Vec<_>>()
                                        .join(", ")
                                )),
                                crate::fuzzy::Match::None => Err(format!("no account called {s}")),
                            }
                        }
                    },
                }
            }
            _ => Err("account must be a number or a name".into()),
        }
    }

    /// Every tab, leaving out ones moving away (their new tab counts).
    pub fn all_tabs(&self) -> Vec<(usize, usize)> {
        (0..self.panes.len())
            .flat_map(|s| (0..self.panes[s].tabs.len()).map(move |t| (s, t)))
            .filter(|&(s, t)| !self.moving_away(self.panes[s].tabs[t].uid))
            .collect()
    }

    /// Tabs one reference names.
    fn tabs_named(&self, r: &str) -> Result<Vec<(usize, usize)>, String> {
        let r = r.trim();
        let low = r.to_lowercase();
        let all = self.all_tabs();
        let pick = |f: &dyn Fn(&crate::pane::Pane) -> bool| -> Vec<(usize, usize)> {
            all.iter()
                .copied()
                .filter(|&(s, t)| f(&self.panes[s].tabs[t]))
                .collect()
        };
        let v = match low.as_str() {
            "all" | "every" | "everything" => all.clone(),
            "current" | "focused" | "this" => vec![(self.focus, self.panes[self.focus].active)],
            "last" | "that" | "there" | "it" => {
                match self.last_target.and_then(|u| self.find_tab(u)) {
                    Some(x) => vec![x],
                    None => return Err("no tab was used yet; give a tab id".into()),
                }
            }
            "waiting" => pick(&|t| t.activity == Activity::Permission),
            "working" => pick(&|t| t.activity == Activity::Working),
            _ => {
                if let Some(uid) = low.strip_prefix('t').and_then(|n| n.parse::<u64>().ok()) {
                    return self
                        .locate_uid(uid)
                        .map(|x| vec![x])
                        .ok_or_else(|| format!("there is no tab {r} (closed?); call get_state"));
                }
                let by_name = pick(&|t| {
                    t.name().eq_ignore_ascii_case(r)
                        || t.cwd
                            .file_name()
                            .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(r))
                });
                if by_name.is_empty() {
                    // A misheard name: the closest tab name or folder.
                    let labels: Vec<(String, String)> = all
                        .iter()
                        .flat_map(|&(s, t)| {
                            let tab = &self.panes[s].tabs[t];
                            let id = tab.uid.to_string();
                            let folder = tab
                                .cwd
                                .file_name()
                                .map(|f| f.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            [(id.clone(), tab.name()), (id, folder)]
                        })
                        .collect();
                    return match crate::fuzzy::best(
                        r,
                        labels.iter().map(|(v, l)| (v.as_str(), l.as_str())),
                    ) {
                        crate::fuzzy::Match::Sure(uid) => {
                            let x = uid
                                .parse::<u64>()
                                .ok()
                                .and_then(|u| self.locate_uid(u))
                                .ok_or("that tab is gone")?;
                            self.corrections
                                .borrow_mut()
                                .push((r.to_string(), self.panes[x.0].tabs[x.1].name()));
                            Ok(vec![x])
                        }
                        crate::fuzzy::Match::Unsure(c) => {
                            let names: Vec<String> = c
                                .iter()
                                .filter_map(|c| {
                                    c.value.parse::<u64>().ok().and_then(|u| self.locate_uid(u))
                                })
                                .map(|(s, t)| {
                                    format!(
                                        "{} (t{})",
                                        self.panes[s].tabs[t].name(),
                                        self.panes[s].tabs[t].uid
                                    )
                                })
                                .collect();
                            Err(format!(
                                "no tab called {r}; did you mean: {}",
                                names.join(", ")
                            ))
                        }
                        crate::fuzzy::Match::None => {
                            Err(format!("no tab called {r}; use a tab id from get_state"))
                        }
                    };
                }
                by_name
            }
        };
        Ok(v)
    }

    /// Every tab the arguments name (`tab` and/or `account`), in order,
    /// without duplicates. `many` allows more than one.
    pub fn tab_targets(&self, args: &Value, many: bool) -> Result<Vec<(usize, usize)>, String> {
        // A group: all its tabs ("close the experiments group").
        if let Some(g) = args.get("group").and_then(Value::as_str) {
            let acct = match args.get("account") {
                Some(v) if !v.is_null() => Some(self.account_arg(v)?),
                _ => None,
            };
            let (s, gi) = self.group_named(g, acct)?;
            let out: Vec<(usize, usize)> = self.panes[s]
                .members(gi)
                .into_iter()
                .map(|t| (s, t))
                .collect();
            if out.is_empty() {
                return Err(format!(
                    "the {} group has no tabs",
                    self.panes[s].groups[gi].name
                ));
            }
            if !many && out.len() > 1 {
                return Err(format!(
                    "the {} group has {} tabs; pick one",
                    self.panes[s].groups[gi].name,
                    out.len()
                ));
            }
            return Ok(out);
        }
        let acct = match args.get("account") {
            Some(v) if !v.is_null() => Some(self.account_arg(v)?),
            _ => None,
        };
        let refs: Vec<String> = match args.get("tab") {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .map(|x| {
                    x.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| x.to_string())
                })
                .collect(),
            Some(Value::Number(n)) => {
                return Err(format!(
                    "tab {n} is ambiguous: use the tab id from get_state (\"t..\")"
                ));
            }
            _ => vec![],
        };
        let mut out: Vec<(usize, usize)> = vec![];
        if refs.is_empty() {
            match acct {
                // An account alone: every tab of it (batch), or its shown tab.
                Some(a) if many => {
                    out = self
                        .all_tabs()
                        .into_iter()
                        .filter(|&(s, _)| self.panes[s].account == Some(a))
                        .collect()
                }
                Some(a) => {
                    let s = self
                        .panes
                        .iter()
                        .position(|p| p.account == Some(a))
                        .ok_or_else(|| {
                            format!("{} is not open in a pane", self.cfg.accounts[a].display())
                        })?;
                    out.push((s, self.panes[s].active));
                }
                None if many => {
                    return Err("say which tabs: tab ids, \"all\", or an account".into());
                }
                None => out.push((self.focus, self.panes[self.focus].active)),
            }
        } else {
            for r in &refs {
                for x in self.tabs_named(r)? {
                    if acct.is_none_or(|a| self.panes[x.0].account == Some(a)) && !out.contains(&x)
                    {
                        out.push(x);
                    }
                }
            }
        }
        if out.is_empty() {
            return Err("no tab matches".into());
        }
        if !many && out.len() > 1 {
            let ids: Vec<String> = out
                .iter()
                .map(|&(s, t)| tab_id(self.panes[s].tabs[t].uid))
                .collect();
            return Err(format!(
                "that names {} tabs ({}); pick one",
                out.len(),
                ids.join(", ")
            ));
        }
        Ok(out)
    }

    fn one_tab(&mut self, args: &Value) -> Result<(usize, usize), String> {
        let x = self.tab_targets(args, false)?[0];
        self.last_target = Some(self.panes[x.0].tabs[x.1].uid);
        Ok(x)
    }

    pub fn state_word(&self, s: usize, t: usize) -> &'static str {
        let tab = &self.panes[s].tabs[t];
        match tab.activity {
            Activity::Working => "working",
            Activity::Permission => "waiting for approval",
            Activity::Ready => "ready",
            Activity::Starting => "starting",
            Activity::Exited => "ended",
            Activity::Idle if tab.suspended => "paused (memory saver)",
            Activity::Idle => "not started",
        }
    }

    fn tab_json(&self, s: usize, t: usize) -> Value {
        let slot = &self.panes[s];
        let tab = &slot.tabs[t];
        let mut v = json!({
            "id": tab_id(tab.uid),
            "account": slot.account.map(|a| a + 1),
            "account_label": slot.account.map(|a| self.cfg.accounts[a].display().to_string()),
            "position": format!("tab {} of {}", t + 1, slot.tabs.len()),
            "name": tab.name(),
            "folder": crate::config::tilde(&tab.cwd),
            "state": self.state_word(s, t),
        });
        if s == self.focus && t == slot.active {
            v["focused"] = json!(true);
        }
        if let Some(g) = slot.group_of(t) {
            v["group"] = json!(slot.groups[g].name);
        }
        if tab.pinned {
            v["pinned"] = json!(true);
        }
        if let Some(c) = slot.accent_of(t) {
            v["color"] = json!(c);
        }
        if self.last_target == Some(tab.uid) {
            v["last_used"] = json!(true);
        }
        let loops = self.loops_in(tab.uid);
        if loops > 0 {
            v["loops"] = json!(loops);
        }
        let q = self.queued_for(tab.uid);
        if q > 0 {
            v["queued_prompts"] = json!(q);
        }
        if tab.activity == Activity::Permission {
            if let Some(r) = self.request_of(s, t) {
                v["waiting_for"] = json!(r.summary());
            }
        }
        v
    }

    fn describe_set(&self, tabs: &[(usize, usize)]) -> String {
        let accts: std::collections::BTreeSet<Option<usize>> =
            tabs.iter().map(|&(s, _)| self.panes[s].account).collect();
        match tabs {
            [(s, t)] => format!(
                "the {} tab on {}",
                self.panes[*s].tabs[*t].name(),
                self.describe_tab(*s)
            ),
            _ if accts.len() == 1 => format!(
                "{} on {}",
                plural(tabs.len(), "tab"),
                self.describe_tab(tabs[0].0)
            ),
            _ => format!(
                "{} across {}",
                plural(tabs.len(), "tab"),
                plural(accts.len(), "account")
            ),
        }
    }

    /// Batch tools: resolve the arguments to an exact plan; ask once when
    /// it needs a yes; with the token (from the user's next turn) run the
    /// plan that was asked about. A confirmation never covers more than
    /// the user heard: new arguments must resolve to the same plan.
    pub(crate) fn planned(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        let turn = self.assistant_turn;
        self.pending_confirms.retain(|p| p.age() < KEEP_PLANS);
        let ask = |p: &Pending| {
            json!({
                "ok": false, "needs_confirmation": true, "token": p.token, "question": p.summary,
                "next": format!("Ask the user exactly once, as one short question: \"{}\" Then stop. Only after a clear yes in a later message, call {} again with only confirm_token; that runs the whole set at once.", p.summary, p.tool)
            })
        };
        // Ask the same question again (after an expiry): the same plan, so
        // nothing can drift.
        if let Some(tok) = args.get("reissue_token").and_then(Value::as_str) {
            let i = self.pending_confirms.iter().position(|p| p.token == tok && p.tool == tool).ok_or("that plan is gone (over 10 minutes old); build it again from what the user asked")?;
            let mut p = self.pending_confirms.remove(i);
            p.token = crate::session_ops::new_uuid()[..8].to_string();
            p.turn = turn;
            p.at = Instant::now();
            crate::log::info(&format!(
                "control: {tool} asked again as {}: {}",
                p.token, p.summary
            ));
            let mut reply = ask(&p);
            reply["reissued"] = json!(true);
            reply["next"] = json!(format!("The same plan, with a new token. Ask the user this exact question again now, once: \"{}\" Then stop. Do not call {tool} again (no fresh call, no new plan) until they answer.", p.summary));
            self.pending_confirms.push(p);
            return Ok(reply);
        }
        if let Some(tok) = args.get("confirm_token").and_then(Value::as_str) {
            let Some(i) = self
                .pending_confirms
                .iter()
                .position(|p| p.token == tok && p.tool == tool)
            else {
                crate::log::info(&format!("control: {tool} token {tok} refused: unknown"));
                if let Some((_, newer)) = self.superseded.iter().find(|(old, _)| old == tok) {
                    return Err(format!("that question was replaced by a newer one (token {newer}): confirm that one, which is what you asked last"));
                }
                return Err("that confirmation token is unknown; ask the user again".into());
            };
            let p = self.pending_confirms[i].clone();
            // Never in the turn that asked: the user must have heard it.
            if p.turn >= turn {
                crate::log::info(&format!(
                    "control: {tool} token {tok} refused: same turn it was issued"
                ));
                return Err("refused: that question was asked in this same turn, so the user has not answered it. Ask, then stop.".into());
            }
            if !p.answerable() {
                crate::log::info(&format!(
                    "control: {tool} token {tok} expired ({} s old)",
                    p.age().as_secs()
                ));
                return Err(format!(
                    "that confirmation expired (asked {} s ago); call {tool} with reissue_token \"{tok}\" to ask the very same question again",
                    p.age().as_secs()
                ));
            }
            let said = self.assistant.last_user.clone();
            if crate::instant::negative(&said) {
                self.pending_confirms.remove(i);
                crate::log::info(&format!(
                    "control: {tool} token {tok} declined by \"{said}\""
                ));
                return Ok(
                    json!({"ok": true, "result": {"cancelled": true}, "say": "Okay, I left them as they are."}),
                );
            }
            // Only a clear yes confirms; a question or anything unclear
            // leaves it pending (for two minutes from the question).
            if !crate::instant::affirmative(&said) {
                crate::log::info(&format!(
                    "control: {tool} token {tok} not confirmed by \"{said}\""
                ));
                return Ok(json!({
                    "ok": false, "needs_confirmation": true, "token": p.token, "not_confirmed": true,
                    "error": format!("the user did not clearly say yes (they said \"{said}\"); nothing was done"),
                    "next": format!("Ask once more, briefly: \"{} Say yes to go ahead.\" Then stop.", p.summary),
                }));
            }
            let mut rest = args.clone();
            if let Some(o) = rest.as_object_mut() {
                o.remove("confirm_token");
            }
            // (A prompt edit was checked when asked; the yes turn is not a
            // request to change anything, so it is not planned again.)
            if tool != "edit_system_prompt" && rest.as_object().is_some_and(|o| !o.is_empty()) {
                let (fresh, _, _) = self.plan_for(tool, &rest)?;
                if fresh != p.plan {
                    crate::log::info(&format!(
                        "control: {tool} token {tok} refused: different targets"
                    ));
                    return Err(format!(
                        "those arguments differ from what the user confirmed (\"{}\"); send only confirm_token",
                        p.summary
                    ));
                }
            }
            self.pending_confirms.remove(i);
            crate::log::info(&format!(
                "control: {tool} confirmed by \"{said}\": {}",
                p.summary
            ));
            return self.run_plan(tool, &p.plan);
        }
        let (plan, question, needs) = self.plan_for(tool, args)?;
        if !needs {
            return self.run_plan(tool, &plan);
        }
        Ok(self.ask_question(tool, plan, question))
    }

    /// Put `question` about `plan` to the user (a token to confirm with).
    pub(crate) fn ask_question(&mut self, tool: &str, plan: Value, question: String) -> Value {
        let turn = self.assistant_turn;
        let ask = |p: &Pending| {
            json!({
                "ok": false, "needs_confirmation": true, "token": p.token, "question": p.summary,
                "next": format!("Ask the user exactly once, as one short question: \"{}\" Then stop. Only after a clear yes in a later message, call {} again with only confirm_token; that runs the whole set at once.", p.summary, p.tool)
            })
        };
        let token = crate::session_ops::new_uuid()[..8].to_string();
        // A new question for the same tool replaces the old one: the brain
        // is told, and the old token says so if it comes back.
        let replaced: Vec<Pending> = self
            .pending_confirms
            .iter()
            .filter(|p| p.tool == tool)
            .cloned()
            .collect();
        for r in &replaced {
            self.superseded.push((r.token.clone(), token.clone()));
        }
        if self.superseded.len() > 20 {
            let n = self.superseded.len() - 20;
            self.superseded.drain(..n);
        }
        self.pending_confirms.retain(|p| p.tool != tool);
        let p = Pending {
            token: token.clone(),
            tool: tool.into(),
            plan,
            summary: question.clone(),
            turn,
            at: Instant::now(),
        };
        crate::log::info(&format!("control: {tool} asks ({token}): {question}"));
        let mut reply = ask(&p);
        if let Some(r) = replaced.last() {
            reply["replaces"] = json!({"token": r.token, "question": r.summary});
            reply["next"] = json!(format!("This replaces your earlier question (\"{}\"): put everything in this one, ask only this, once: \"{}\" Then stop.", r.summary, p.summary));
        }
        self.pending_confirms.push(p);
        reply
    }

    /// The no-confirmation tier for closing: a single tab, idle (nothing
    /// runs in it: no subagents, shells or loops), no prompt on its way,
    /// asked for in a direct command ("close it", "close the dan tab"),
    /// and neither confirm policy set to always.
    pub fn closes_without_a_yes(&self, (sl, t): (usize, usize)) -> bool {
        if self.cfg.assistant.confirm == "always" || self.cfg.confirm_close == "always" {
            return false;
        }
        let Some(tab) = self.panes.get(sl).and_then(|p| p.tabs.get(t)) else {
            return false;
        };
        let said = format!(" {} ", self.assistant.last_user.to_lowercase());
        let direct = [" close", " shut", " get rid of", " kill"]
            .iter()
            .any(|w| said.contains(w))
            && !said.trim_end().ends_with('?');
        direct
            && !tab.pinned
            && tab.pending.is_none()
            && !self.deliveries.iter().any(|d| d.uid == tab.uid)
            && self.tab_activity(sl, t).state == "idle"
    }

    /// (plan, question, needs a yes) for a batch tool's arguments.
    fn plan_for(&mut self, tool: &str, args: &Value) -> Result<(Value, String, bool), String> {
        if let Some(r) = self.admin_plan_for(tool, args) {
            return r;
        }
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let listy = |k: &[&str]| {
            matches!(args.get("tab"), Some(Value::Array(_)))
                || s("tab").is_some_and(|t| k.contains(&t.as_str()))
                || (args.get("tab").is_none()
                    && args.get("account").is_some()
                    && tool != "send_prompt")
        };
        match tool {
            "send_prompt" => {
                let text = s("text")
                    .filter(|t| !t.trim().is_empty())
                    .ok_or("text is required")?;
                let tabs = self.tab_targets(args, listy(&["all", "waiting", "working"]))?;
                let q = format!("Send that prompt to {}?", self.describe_set(&tabs));
                Ok((
                    json!({"uids": self.uids(&tabs), "text": text}),
                    q,
                    tabs.len() > 1,
                ))
            }
            "answer_prompt" => {
                let choice = s("choice").unwrap_or_default();
                let c = choice_of(&choice).ok_or("choice must be yes, always or no")?;
                let mut tabs = self.tab_targets(args, listy(&["all", "waiting"]))?;
                tabs.retain(|&(sl, t)| self.panes[sl].tabs[t].activity == Activity::Permission);
                if tabs.is_empty() {
                    return Err("none of those tabs is waiting for approval".into());
                }
                let reqs: Vec<String> = tabs
                    .iter()
                    .map(|&(sl, t)| {
                        self.request_of(sl, t)
                            .map(|r| r.summary())
                            .unwrap_or_default()
                    })
                    .collect();
                let policy = self.cfg.assistant.confirm.as_str();
                let needs = policy != "never"
                    && (policy == "always"
                        || tabs.len() > 1
                        || c == Choice::Deny
                        || reqs.iter().any(|r| crate::risk::needs_confirm(r)));
                let verb = match c {
                    Choice::Approve => "Approve",
                    Choice::Always => "Always allow",
                    Choice::Deny => "Deny",
                };
                let what = if tabs.len() == 1 {
                    format!(" \"{}\"", crate::sessions::snippet(&reqs[0], 80))
                } else {
                    String::new()
                };
                let q = format!("{verb}{what} in {}?", self.describe_set(&tabs));
                Ok((
                    json!({"uids": self.uids(&tabs), "choice": choice}),
                    q,
                    needs,
                ))
            }
            "edit_system_prompt" => self.plan_prompt_edit(args),
            "close_tabs" => {
                let mut tabs = self.tab_targets(args, true)?;
                // Pinned tabs stay out of batches ("close all", "close
                // everything except pinned") unless asked for by name or
                // with include_pinned.
                let batch = !matches!(args.get("tab"), Some(Value::String(t))
                    if !matches!(t.as_str(), "all" | "waiting" | "working" | "idle" | "ready"));
                let with_pinned = args
                    .get("include_pinned")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if batch && !with_pinned && tabs.len() > 1 {
                    let before = tabs.len();
                    tabs.retain(|&(sl, t)| !self.panes[sl].tabs[t].pinned);
                    if tabs.is_empty() {
                        return Err(format!(
                            "all {before} are pinned; say include pinned to close them too"
                        ));
                    }
                }
                let q = format!("Close {}?", self.describe_set(&tabs));
                // One idle tab the user named in a direct command closes
                // at once (with Undo); everything else asks once.
                let named = matches!(args.get("tab"), Some(Value::String(t))
                    if !matches!(t.as_str(), "all" | "waiting" | "working" | "idle" | "ready"));
                let needs = !(named && tabs.len() == 1 && self.closes_without_a_yes(tabs[0]));
                Ok((json!({"uids": self.uids(&tabs)}), q, needs))
            }
            "stop_loops" => {
                let all = args.get("all").and_then(Value::as_bool).unwrap_or(false)
                    || s("job").as_deref() == Some("all");
                let mut jobs: Vec<String> = args
                    .get("jobs")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|j| j.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(j) = s("job").filter(|j| j != "all") {
                    jobs.push(j);
                }
                let rows: Vec<crate::app_loops::LoopRow> = if all {
                    self.loops.clone()
                } else if !jobs.is_empty() {
                    self.loops
                        .iter()
                        .filter(|r| jobs.iter().any(|j| r.lp.id.starts_with(j.as_str())))
                        .cloned()
                        .collect()
                } else {
                    let uids: Vec<u64> = self
                        .tab_targets(args, true)?
                        .iter()
                        .map(|&(sl, t)| self.panes[sl].tabs[t].uid)
                        .collect();
                    self.loops
                        .iter()
                        .filter(|r| uids.contains(&r.uid))
                        .cloned()
                        .collect()
                };
                if rows.is_empty() {
                    return Err("no loop matches (see get_state)".into());
                }
                let tabs: std::collections::BTreeSet<u64> = rows.iter().map(|r| r.uid).collect();
                let q = format!(
                    "Stop {} in {}?",
                    plural(rows.len(), "loop"),
                    plural(tabs.len(), "tab")
                );
                Ok((
                    json!({"loops": rows.iter().map(|r| json!([r.uid, r.lp.id])).collect::<Vec<_>>()}),
                    q,
                    true,
                ))
            }
            "open_tab" => {
                let n = args.get("count").and_then(Value::as_u64).unwrap_or(1);
                let q = format!(
                    "Open {} in your whole home folder (the assistant can then read any file there that is not private)?",
                    plural(n as usize, "tab")
                );
                let mut a = args.clone();
                if let Some(o) = a.as_object_mut() {
                    o.remove("confirm_token");
                    o.remove("reissue_token");
                }
                Ok((json!({"args": a}), q, true))
            }
            "take_over_session" => {
                // One plan for every session asked for: ids, or "idle"
                // (nothing runs in it at all) / "all".
                let lives = self.live_sessions();
                let procs = crate::activity::process_table();
                let ids: Vec<String> = match args.get("id").or(args.get("ids")) {
                    Some(Value::Array(a)) => a
                        .iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect(),
                    Some(Value::String(x))
                        if matches!(x.as_str(), "idle" | "idle_no_background" | "all") =>
                    {
                        let want_idle = x != "all";
                        lives
                            .iter()
                            .filter(|l| !want_idle || crate::activity::of_live(l, &procs, 0).idle())
                            .map(|l| l.session_id.clone())
                            .collect()
                    }
                    Some(Value::String(x)) => vec![x.clone()],
                    _ => return Err("id is required: a session id, a list, or \"idle\"".into()),
                };
                if ids.is_empty() {
                    return Err("no session running elsewhere matches (none is idle with nothing running in the background)".into());
                }
                let ix = self
                    .session_index
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .rows
                    .clone();
                let mut items = vec![];
                let mut lines = vec![];
                let mut warnings = vec![];
                for id in &ids {
                    let (live, target) = self.takeover_target(id, args)?;
                    let act = crate::activity::of_live(&live, &procs, 0);
                    let title = live
                        .name
                        .clone()
                        .or_else(|| {
                            ix.iter()
                                .find(|r| r.info.id == live.session_id)
                                .map(|r| r.info.summary())
                        })
                        .or_else(|| {
                            crate::takeover::transcript_of(&live)
                                .and_then(|t| crate::sessions::parse_file_quick(&t))
                                .map(|i| i.summary())
                        })
                        .unwrap_or_else(|| "untitled".into());
                    let place = match (&live.terminal, &live.tty) {
                        (Some(t), Some(y)) => format!("{t} {y}"),
                        (None, Some(y)) => y.clone(),
                        (Some(t), None) => t.clone(),
                        _ => "another terminal".into(),
                    };
                    lines.push(format!(
                        "\"{}\" in {} ({}, {} in {place}, pid {}) to {}",
                        crate::sessions::snippet(&title, 50),
                        crate::config::tilde(&live.cwd),
                        act.state,
                        live.harness.name(),
                        live.pid,
                        self.cfg.accounts[target].display()
                    ));
                    if act.background() > 0 {
                        let n = |k: usize, one: &str| {
                            (k > 0).then(|| format!("{k} {one}{}", if k == 1 { "" } else { "s" }))
                        };
                        let what: Vec<String> = [
                            n(act.subagents_running, "subagent"),
                            n(act.background_shells, "shell"),
                            n(act.background_tasks, "background task"),
                        ]
                        .into_iter()
                        .flatten()
                        .collect();
                        warnings.push(format!(
                            "\"{}\" still has {} running, which stops with it",
                            crate::sessions::snippet(&title, 40),
                            what.join(", ")
                        ));
                    }
                    items.push(json!({"id": live.session_id, "pid": live.pid, "account": target, "title": title}));
                }
                let mut q = if lines.len() == 1 {
                    format!("Stop {} and continue it here?", lines[0])
                } else {
                    format!(
                        "Stop these {} and continue them here: {}?",
                        lines.len(),
                        lines
                            .iter()
                            .enumerate()
                            .map(|(k, l)| format!("{}) {l}", k + 1))
                            .collect::<Vec<_>>()
                            .join("; ")
                    )
                };
                if !warnings.is_empty() {
                    q.push_str(&format!(" Note: {}.", warnings.join("; ")));
                }
                let name = s("name").filter(|n| !n.trim().is_empty() && items.len() == 1);
                Ok((
                    json!({"items": items, "name": name, "interrupt": s("if_busy").as_deref() == Some("interrupt"), "close": args.get("close_terminal").and_then(Value::as_bool).unwrap_or(false)}),
                    q,
                    true,
                ))
            }
            other => Err(format!("{other} has no plan")),
        }
    }

    pub fn takeover_target_pub(
        &self,
        id: &str,
        args: &Value,
    ) -> Result<(crate::takeover::Live, usize), String> {
        self.takeover_target(id, args)
    }

    /// The live session `id` and where it should continue.
    fn takeover_target(
        &self,
        id: &str,
        args: &Value,
    ) -> Result<(crate::takeover::Live, usize), String> {
        let mut live = self.live_sessions().into_iter().find(|l| l.session_id == id || l.session_id.starts_with(id)).ok_or("that session is not running anywhere else (if it is in a GodTerm tab, it is already here)")?;
        live.terminal = crate::takeover::terminal_of(live.pid);
        let own =
            (0..self.cfg.accounts.len()).find(|&a| self.cfg.accounts[a].config_dir() == live.home);
        let target = match args.get("account") {
            Some(v) if !v.is_null() => self.account_arg(v)?,
            _ => own
                .or_else(|| {
                    (0..self.cfg.accounts.len())
                        .filter(|&a| {
                            self.cfg.accounts[a].harness() == live.harness
                                && self.accounts[a].login.logged_in()
                        })
                        .max_by(|a, b| {
                            self.accounts[*a]
                                .effective_left()
                                .unwrap_or(0.0)
                                .partial_cmp(&self.accounts[*b].effective_left().unwrap_or(0.0))
                                .unwrap_or(std::cmp::Ordering::Equal)
                        })
                })
                .ok_or_else(|| {
                    format!(
                        "no logged in {} account to continue it on",
                        live.harness.name()
                    )
                })?,
        };
        Ok((live, target))
    }

    /// Run a (confirmed or harmless) plan. Results carry what happened.
    fn run_plan(&mut self, tool: &str, plan: &Value) -> Result<Value, String> {
        if let Some(r) = self.admin_run_plan(tool, plan) {
            return r;
        }
        let uids: Vec<u64> = plan["uids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_u64)
            .map(|u| self.current_uid(u))
            .collect();
        let gone = |app: &App| {
            uids.iter()
                .filter(|u| app.find_tab(**u).is_none())
                .map(|u| tab_id(*u))
                .collect::<Vec<_>>()
        };
        match tool {
            "edit_system_prompt" => self.run_prompt_edit(plan),
            "send_prompt" => {
                let text = plan["text"].as_str().unwrap_or("").to_string();
                let mut ids = vec![];
                let mut failed: Vec<Value> = gone(self)
                    .into_iter()
                    .map(|t| json!({"tab": t, "status": "failed", "reason": "the tab was closed"}))
                    .collect();
                for u in &uids {
                    let Some((sl, t)) = self.find_tab(*u) else {
                        continue;
                    };
                    let tab = &self.panes[sl].tabs[t];
                    if !tab.is_running() && tab.pending.is_none() && !tab.suspended {
                        failed.push(json!({"tab": tab_id(*u), "status": "failed", "reason": "that tab is not running (it ended or was never started)"}));
                        continue;
                    }
                    ids.push(self.deliver(*u, &text));
                    self.last_target = Some(*u);
                }
                Ok(
                    json!({"_wait": {"deliveries": ids, "wait_queued": false, "max_ms": 6000}, "failed": failed}),
                )
            }
            "answer_prompt" => {
                let c = choice_of(plan["choice"].as_str().unwrap_or("")).ok_or("bad choice")?;
                let mut done = vec![];
                for u in &uids {
                    let Some((sl, t)) = self.find_tab(*u) else {
                        continue;
                    };
                    let msg = self.answer_tab(sl, t, c);
                    self.last_target = Some(*u);
                    done.push(json!({"tab": tab_id(*u), "result": msg}));
                }
                Ok(ok(
                    json!({"answered": done.len(), "tabs": done, "closed_meanwhile": gone(self)}),
                ))
            }
            "close_tabs" => {
                let mut closed = 0;
                let mut names: Vec<String> = vec![];
                let mut accts = std::collections::BTreeSet::new();
                // One group: one reopen_closed brings them all back.
                let group = self.next_close_group();
                for u in &uids {
                    let Some((sl, t)) = self.find_tab(*u) else {
                        continue;
                    };
                    names.push(self.panes[sl].tabs[t].name());
                    self.remember_closed(sl, t, group);
                    self.tab_event(sl, t, "close", Some("assistant"), None);
                    let fallback = self.panes[sl]
                        .account
                        .map(|a| self.cfg.accounts[a].work_dir())
                        .unwrap_or_else(crate::config::home_dir);
                    accts.insert(self.panes[sl].account);
                    self.panes[sl].close_tab(t, fallback);
                    self.deliveries.retain(|d| d.uid != *u);
                    closed += 1;
                }
                self.state_dirty = true;
                self.flash(format!(
                    "Closed {} across {}",
                    plural(closed, "tab"),
                    plural(accts.len(), "account")
                ));
                let say = match names.as_slice() {
                    [one] => format!("Closed {one}. Say undo to bring it back."),
                    _ => format!(
                        "Closed {}. Say undo to bring them back.",
                        plural(closed, "tab")
                    ),
                };
                Ok(ok(
                    json!({"closed": closed, "accounts": accts.len(), "undo": "reopen_closed", "say": say, "note": "each pane keeps one fresh empty tab"}),
                ))
            }
            "take_over_session" => {
                let lives = self.live_sessions();
                let mut started = vec![];
                let mut failed = vec![];
                let items: Vec<Value> = match plan["items"].as_array() {
                    Some(a) => a.clone(),
                    // A plan from before batches.
                    None => vec![
                        json!({"id": plan["id"], "pid": plan["pid"], "account": plan["account"], "title": ""}),
                    ],
                };
                for it in &items {
                    let id = it["id"].as_str().unwrap_or("").to_string();
                    let title = it["title"].as_str().unwrap_or("").to_string();
                    let Some(mut live) = lives.iter().find(|l| l.session_id == id).cloned() else {
                        failed.push(json!({"session": id, "title": title, "reason": "it is no longer running there (nothing was stopped)"}));
                        continue;
                    };
                    if live.pid as u64 != it["pid"].as_u64().unwrap_or(0) {
                        failed.push(json!({"session": id, "title": title, "reason": "a different process runs it now; ask again"}));
                        continue;
                    }
                    live.terminal = crate::takeover::terminal_of(live.pid);
                    let target = it["account"].as_u64().unwrap_or(0) as usize;
                    match self.start_takeover(
                        live,
                        target,
                        crate::takeover::Mode::Take,
                        plan["interrupt"].as_bool().unwrap_or(false),
                        plan["close"].as_bool().unwrap_or(false),
                    ) {
                        Ok(m) => {
                            if let Some(t) =
                                self.takeovers.iter_mut().find(|t| t.live.session_id == id)
                            {
                                t.name = plan["name"].as_str().map(str::to_string);
                            }
                            started.push(json!({"session": id, "title": title, "status": m}));
                        }
                        Err(e) => failed.push(json!({"session": id, "title": title, "reason": e})),
                    }
                }
                let sessions: Vec<Value> = started.iter().map(|s| s["session"].clone()).collect();
                // The reply waits until each is open as a tab here (or up to
                // 45 s), so the next step (a rename, a prompt) can use its id.
                Ok(
                    json!({"ok": !started.is_empty(), "_wait": {"takeovers": sessions, "deliveries": [], "max_ms": 45000}, "started": started, "failed": failed}),
                )
            }
            "open_tab" => {
                self.open_home_ok = true;
                let r = self.control_inner("open_tab", &plan["args"]);
                self.open_home_ok = false;
                r
            }
            "stop_loops" => {
                let mut n = 0;
                for x in plan["loops"].as_array().into_iter().flatten() {
                    if let (Some(uid), Some(id)) = (x[0].as_u64(), x[1].as_str()) {
                        self.stop_loops(uid, vec![id.to_string()]);
                        n += 1;
                    }
                }
                Ok(ok(
                    json!({"stopping": n, "note": "each tab is asked to stop them when it is idle"}),
                ))
            }
            other => Err(format!("{other} has no plan")),
        }
    }

    pub fn uids(&self, tabs: &[(usize, usize)]) -> Value {
        json!(tabs
            .iter()
            .map(|&(s, t)| self.panes[s].tabs[t].uid)
            .collect::<Vec<_>>())
    }

    /// A path a tool may read: inside the tab's folder.
    fn path_in_tab(&mut self, args: &Value) -> Result<PathBuf, String> {
        let (s, t) = self.one_tab(args)?;
        self.spoken_path(
            &self.panes[s].tabs[t].cwd.clone(),
            args.get("path").and_then(Value::as_str),
        )
    }

    /// A path inside a tab's folder as it may have been said: spoken
    /// punctuation resolved, and a misheard name matched to the closest
    /// entry, one component at a time (noted for the reply).
    pub fn spoken_path(
        &self,
        root: &std::path::Path,
        path: Option<&str>,
    ) -> Result<PathBuf, String> {
        let Some(said) = path.filter(|p| !p.trim().is_empty()) else {
            return path_under(root, None);
        };
        let p = crate::fuzzy::spoken_punctuation(said);
        let first = path_under(root, Some(&p));
        if first.is_ok()
            || p.starts_with('/')
            || p.starts_with('~')
            || std::path::Path::new(&p).is_absolute()
        {
            return first;
        }
        let mut cur = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        for comp in p.split('/').filter(|c| !c.is_empty() && *c != ".") {
            if comp == ".." {
                return first;
            }
            let exact = cur.join(comp);
            if exact.exists() {
                cur = exact;
                continue;
            }
            let names: Vec<String> = std::fs::read_dir(&cur)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            match crate::fuzzy::best(comp, names.iter().map(|n| (n.as_str(), n.as_str()))) {
                crate::fuzzy::Match::Sure(n) => cur = cur.join(n),
                crate::fuzzy::Match::Unsure(c) => {
                    return Err(format!(
                        "{comp} does not exist there; did you mean: {}",
                        c.iter()
                            .map(|c| c.value.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ));
                }
                crate::fuzzy::Match::None => return first,
            }
        }
        let fixed = path_under(root, Some(&cur.to_string_lossy()))?;
        let rel = fixed
            .strip_prefix(root.canonicalize().unwrap_or_else(|_| root.to_path_buf()))
            .map(|r| r.display().to_string())
            .unwrap_or_default();
        self.corrections.borrow_mut().push((said.to_string(), rel));
        Ok(fixed)
    }

    /// Tabs a read-only tool looks at: one, a list or "all".
    fn read_targets(&mut self, args: &Value) -> Result<Vec<(usize, usize)>, String> {
        let many = matches!(args.get("tab"), Some(Value::Array(_)))
            || args
                .get("tab")
                .and_then(Value::as_str)
                .is_some_and(|t| matches!(t, "all" | "working" | "waiting"));
        let v = self.tab_targets(args, many)?;
        if let Some(&(s, t)) = v.last() {
            self.last_target = Some(self.panes[s].tabs[t].uid);
        }
        Ok(v)
    }

    /// Folders the user makes tab workspaces in (new_tab_base and each
    /// account's): allowed even inside GodTerm's home.
    pub fn workspaces(&self) -> Vec<PathBuf> {
        let mut v = vec![crate::config::expand_tilde(&self.cfg.new_tab_base)];
        v.extend(
            self.cfg
                .accounts
                .iter()
                .filter_map(|a| a.new_tab_base.as_deref())
                .map(crate::config::expand_tilde),
        );
        v
    }

    /// Every open tab's folder (canonical), for open_path's scope.
    fn tab_roots(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self
            .panes
            .iter()
            .flat_map(|p| {
                p.tabs
                    .iter()
                    .map(|t| t.cwd.canonicalize().unwrap_or_else(|_| t.cwd.clone()))
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }
}

/// The first `n` lines of a regular file (never a FIFO, device or
/// folder), streamed, at most 12000 chars, read off the UI thread with a
/// timeout. Also the line count when the file is small enough to count.
fn read_head(p: &std::path::Path, n: usize) -> Result<(String, Option<usize>, bool), String> {
    let md = std::fs::metadata(p).map_err(|e| format!("{e}"))?;
    if md.is_dir() {
        return Err("that is a folder; use list_dir".into());
    }
    if !md.is_file() {
        return Err("that is not a regular file (a pipe, socket or device)".into());
    }
    let len = md.len();
    let path = p.to_path_buf();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{BufRead, Read};
        let r = (|| -> Result<(String, Option<usize>, bool), String> {
            let f = std::fs::File::open(&path).map_err(|e| format!("{e}"))?;
            let mut rd = BufReader::new(f);
            let mut out = String::new();
            let mut line = Vec::new();
            let mut lines = 0;
            let mut more = false;
            loop {
                line.clear();
                // A line longer than 64 KB is cut there.
                let got = (&mut rd)
                    .take(64 * 1024)
                    .read_until(b'\n', &mut line)
                    .map_err(|e| format!("{e}"))?;
                if got == 0 {
                    break;
                }
                if line.contains(&0) {
                    return Err("that is a binary file".into());
                }
                if lines == n || out.len() >= 12000 {
                    more = true;
                    break;
                }
                out.push_str(String::from_utf8_lossy(&line).trim_end_matches(['\n', '\r']));
                out.push('\n');
                lines += 1;
            }
            let out: String = out.trim_end().chars().take(12000).collect();
            // Count the lines of files up to 8 MB (streamed).
            let total = if !more {
                Some(lines)
            } else if len <= 8 << 20 {
                let mut f = std::fs::File::open(&path).map_err(|e| format!("{e}"))?;
                let mut buf = [0u8; 65536];
                let mut count = 0;
                loop {
                    let k = f.read(&mut buf).map_err(|e| format!("{e}"))?;
                    if k == 0 {
                        break;
                    }
                    count += buf[..k].iter().filter(|b| **b == b'\n').count();
                }
                Some(count)
            } else {
                None
            };
            Ok((out, total, more))
        })();
        let _ = tx.send(r);
    });
    rx.recv_timeout(Duration::from_secs(3))
        .map_err(|_| "reading it took too long (a slow disk or network folder)".to_string())?
}

/// A path inside `root` (relative to it, or absolute within it).
fn path_under(root: &std::path::Path, path: Option<&str>) -> Result<PathBuf, String> {
    {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let p = match path.map(str::trim).filter(|p| !p.is_empty()) {
            None => root.clone(),
            Some(p) => {
                let q = crate::config::expand_tilde(p);
                if q.is_absolute() {
                    q
                } else {
                    root.join(q)
                }
            }
        };
        let p = p
            .canonicalize()
            .map_err(|_| format!("{} does not exist", crate::config::tilde(&p)))?;
        if !p.starts_with(&root) {
            return Err(format!(
                "{} is outside the tab's folder {}",
                crate::config::tilde(&p),
                crate::config::tilde(&root)
            ));
        }
        Ok(p)
    }
}

/// "*.html", ".html", "index" or "" against a file name.
fn name_matches(name: &str, pat: &str) -> bool {
    let p = pat.trim().to_lowercase();
    let n = name.to_lowercase();
    if p.is_empty() {
        return true;
    }
    match p.strip_prefix('*') {
        Some(suffix) => n.ends_with(suffix),
        None if p.starts_with('.') => n.ends_with(&p),
        None => n.contains(&p),
    }
}

impl App {
    /// Run one tool. Returns {"ok": true, "result": ...} or an error /
    /// needs_confirmation object.
    pub fn control_call(&mut self, tool: &str, args: &Value) -> Value {
        // Demo mode's own tools (its driver script): not logged anywhere.
        if let Some(r) = crate::demo::control_tool(self, tool, args) {
            return r.unwrap_or_else(err);
        }
        crate::log::info(&format!(
            "control: {tool} {}",
            args.to_string().chars().take(300).collect::<String>()
        ));
        // Who is calling: the assistant's own brain (godterm mcp started by
        // it; in-process calls count as it too) or another client.
        let brain = args
            .get("_client")
            .and_then(Value::as_str)
            .is_none_or(|c| c == "brain");
        let mut args = args.clone();
        if let Some(o) = args.as_object_mut() {
            o.remove("_client");
        }
        let args = &args;
        // A tab id from the brain's memory that now names another tab:
        // refused once with where that tab is now (it may then use the id
        // on purpose).
        if brain && !self.assistant.stale_ids.is_empty() {
            let named: Vec<String> = match &args["tab"] {
                Value::String(t) => vec![t.to_lowercase()],
                Value::Array(a) => a
                    .iter()
                    .filter_map(|x| x.as_str().map(str::to_lowercase))
                    .collect(),
                _ => vec![],
            };
            for t in named {
                let Some(uid) = t.strip_prefix('t').and_then(|n| n.parse::<u64>().ok()) else {
                    continue;
                };
                if let Some(msg) = self.assistant.stale_ids.remove(&uid) {
                    crate::log::info(&format!("control: {tool} refused: stale {t}: {msg}"));
                    let v = err(format!(
                        "refused: {msg} Use the tab id from the current state."
                    ));
                    self.assistant_log_tool(tool, args, &v);
                    return v;
                }
            }
        }
        // Looking never changes anything: always allowed, never counted.
        let read_only = READ_ONLY.contains(&tool);
        if brain {
            self.assistant.last_event = Some(Instant::now());
            // Tab or file contents entered this turn (learn checks it).
            if matches!(
                tool,
                "read_tab"
                    | "recent_turns"
                    | "read_file"
                    | "list_dir"
                    | "session_detail"
                    | "history"
                    | "tab_history"
                    | "find"
            ) {
                self.assistant.turn_reads += 1;
            }
        }
        if brain && !read_only {
            self.assistant_calls += 1;
            if self.assistant.stale > 0 {
                crate::log::info(&format!(
                    "control: {tool} refused: the user cut that request off"
                ));
                let v = err(
                    "the user interrupted this request by talking over you: do nothing more for it",
                );
                self.assistant_log_tool(tool, args, &v);
                return v;
            }
            if self.assistant_calls > self.cfg.assistant.max_tool_calls {
                return err(format!(
                    "too many actions this turn (limit {}); tell the user what is left to do",
                    self.cfg.assistant.max_tool_calls
                ));
            }
        }
        self.corrections.borrow_mut().clear();
        let mut v = match self.control_inner(tool, args) {
            Ok(v) => v,
            Err(e) => err(e),
        };
        // Names matched for misheard ones: the model mentions them.
        let fixes = std::mem::take(&mut *self.corrections.borrow_mut());
        if !fixes.is_empty() && v["error"].is_null() {
            let list: Vec<Value> = fixes
                .iter()
                .map(|(from, to)| json!({"said": from, "used": to}))
                .collect();
            if v.get("result").is_some_and(Value::is_object) {
                v["result"]["corrected_from"] = json!(list);
            } else {
                v["corrected_from"] = json!(list);
            }
        }
        // QA-9: opening onto an account that is out says so.
        if matches!(tool, "open_tab" | "new_tab" | "new_tab_with_prompt")
            && v["error"].is_null()
            && v["needs_confirmation"].is_null()
        {
            let acct = match args.get("account") {
                Some(a) if !a.is_null() => self.account_arg(a).ok(),
                _ => self.panes.get(self.focus).and_then(|p| p.account),
            };
            if let Some(w) = acct.and_then(|a| self.out_warning(a)) {
                v["warning"] = json!(w);
                if let Some(s) = v["say"].as_str().map(str::to_string) {
                    v["say"] = json!(format!("{s} {w}"));
                }
                if let Some(s) = v["result"]["say"].as_str().map(str::to_string) {
                    v["result"]["say"] = json!(format!("{s} {w}"));
                }
            }
        }
        // A question to a tab: its answer is told when it comes.
        if brain && tool == "send_prompt" && v["error"].is_null() {
            self.follow_up_sends(args, &v);
        }
        self.assistant_log_tool(tool, args, &v);
        v
    }

    fn control_inner(&mut self, tool: &str, args: &Value) -> Result<Value, String> {
        if let Some(r) = self.speaker_tool(tool, args) {
            return r;
        }
        if let Some(r) = self.admin_tool(tool, args) {
            return r;
        }
        if let Some(r) = self.grid_tool(tool, args) {
            return r;
        }
        if tool == "disable_remote_control" {
            let say = self.disable_remote()?;
            return Ok(json!({"ok": true, "result": {"say": say}}));
        }
        if tool == "switch_assistant" {
            if let Some(r) = &self.assistant.remote {
                if args["end_remote_control"].as_bool() != Some(true) {
                    return Err(format!("switching ends Remote Control ('{}'): tell the user (\"Switching ends Remote Control; I can turn it back on after\") and call again with end_remote_control true if they still want it", r.name));
                }
            }
            let say = self.switch_assistant(
                args["provider"].as_str(),
                args.get("account"),
                args["model"].as_str(),
            )?;
            return Ok(json!({"ok": true, "result": {"say": say}}));
        }
        if let Some(r) = self.learned_tool(tool, args) {
            return r;
        }
        if tool == "edit_system_prompt" {
            return self.planned(tool, args);
        }
        if let Some(r) = self.prompt_tool(tool, args) {
            return r;
        }
        if let Some(r) = self.tabgroup_tool(tool, args) {
            return r;
        }
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        Ok(match tool {
            "get_state" => ok(self.state_json()),
            "recent_turns" => {
                let (sl, t) = self.one_tab(args)?;
                let id = tab_id(self.panes[sl].tabs[t].uid);
                let n = args
                    .get("n")
                    .and_then(Value::as_u64)
                    .unwrap_or(3)
                    .clamp(1, 10) as usize;
                let path = self
                    .transcript_path(sl, t)
                    .ok_or("no transcript for that tab yet (nothing was asked there)")?;
                let turns: Vec<Value> = crate::sessions::recent_turns(&path, n)
                    .into_iter()
                    .map(|x| {
                        json!({
                            "at": x.at,
                            "user": crate::sessions::first_sentences(&x.user, 300),
                            "reply": x.reply.map(|r| crate::sessions::first_sentences(&r, 700)),
                        })
                    })
                    .collect();
                ok(json!({"tab": id, "state": self.state_word(sl, t), "turns": turns}))
            }
            "restart_to_update" => {
                let Some(v) = self.update.ready() else {
                    return Err("no update is downloaded and ready".into());
                };
                let confirm = args.get("confirm").and_then(Value::as_bool) == Some(true);
                // The assistant's own turn is not a blocker: it restarts after it.
                let blockers: Vec<String> = self
                    .restart_blockers()
                    .into_iter()
                    .filter(|b| !b.starts_with("the assistant"))
                    .collect();
                if !blockers.is_empty() && !confirm {
                    return Ok(
                        json!({"ok": false, "needs_confirmation": format!("Restarting now interrupts this: {}. Ask the user; then call again with confirm true.", blockers.join(", "))}),
                    );
                }
                self.update.after_turn = true;
                ok(
                    json!({"say": format!("Restarting into version {v} when I finish this answer; your tabs come back.")}),
                )
            }
            "stop_waiting" => {
                let uid = match args.get("tab") {
                    Some(v) if !v.is_null() => {
                        let (sl, t) = self.one_tab(args)?;
                        Some(self.panes[sl].tabs[t].uid)
                    }
                    _ => None,
                };
                let n = self.cancel_follow_ups(uid);
                ok(json!({"stopped": n}))
            }
            "read_tab" => {
                let (sl, t) = self.one_tab(args)?;
                let id = tab_id(self.panes[sl].tabs[t].uid);
                if s("what").as_deref() == Some("screen") {
                    let n = args
                        .get("lines")
                        .and_then(Value::as_u64)
                        .unwrap_or(40)
                        .clamp(1, 200) as usize;
                    let text = self.panes[sl].tabs[t]
                        .parser
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .screen()
                        .contents();
                    let lines: Vec<&str> = text
                        .lines()
                        .map(str::trim_end)
                        .filter(|l| !l.is_empty())
                        .collect();
                    ok(
                        json!({"tab": id, "screen": lines[lines.len().saturating_sub(n)..].join("\n")}),
                    )
                } else {
                    let path = self.transcript_path(sl, t).ok_or(
                        "no transcript for that tab yet (nothing was asked there); try what=screen",
                    )?;
                    let text = crate::sessions::last_assistant_text(&path)
                        .ok_or("claude has not replied there yet")?;
                    ok(
                        json!({"tab": id, "state": self.state_word(sl, t), "reply": crate::sessions::first_sentences(&text, 4000)}),
                    )
                }
            }
            "list_dir" => {
                let tabs = self.read_targets(args)?;
                let depth = args
                    .get("depth")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 3) as usize;
                let pat = s("match").unwrap_or_default();
                let one = |app: &App, sl: usize, t: usize| -> Value {
                    match app
                        .spoken_path(
                            &app.panes[sl].tabs[t].cwd,
                            args.get("path").and_then(Value::as_str),
                        )
                        .and_then(|p| match crate::guard::denied(&p, &app.workspaces()) {
                            Some(why) => Err(format!("refused: {why}")),
                            None => Ok(p),
                        }) {
                        Ok(p) => {
                            let mut out = vec![];
                            list_dir(&p, &p, depth, &mut out);
                            out.retain(|e| {
                                e.ends_with('/')
                                    || name_matches(
                                        e.split(' ')
                                            .next()
                                            .unwrap_or("")
                                            .rsplit('/')
                                            .next()
                                            .unwrap_or(""),
                                        &pat,
                                    )
                            });
                            let more = out.len() > 200;
                            out.truncate(200);
                            json!({"tab": tab_id(app.panes[sl].tabs[t].uid), "folder": p.to_string_lossy(), "entries": out, "truncated": more, "empty": out.is_empty()})
                        }
                        Err(e) => json!({"tab": tab_id(app.panes[sl].tabs[t].uid), "error": e}),
                    }
                };
                if tabs.len() == 1 {
                    let v = one(self, tabs[0].0, tabs[0].1);
                    if let Some(e) = v["error"].as_str() {
                        return Err(e.to_string());
                    }
                    ok(v)
                } else {
                    // Every tab in one reply: no need to call again per tab.
                    ok(
                        json!({"tabs": tabs.iter().map(|&(sl, t)| one(self, sl, t)).collect::<Vec<_>>()}),
                    )
                }
            }
            "open_path" => {
                // Open files or folders inside the tabs' folders with their
                // default app, or http(s) URLs in the browser. Local, free.
                let items: Vec<String> = match args.get("paths").or(args.get("path")) {
                    Some(Value::Array(a)) => a
                        .iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect(),
                    Some(Value::String(x)) => vec![x.clone()],
                    _ => return Err("paths is required (files, folders or http URLs)".into()),
                };
                let roots = self.tab_roots();
                let base = match args.get("tab") {
                    Some(_) => {
                        let (sl, t) = self.one_tab(args)?;
                        Some(self.panes[sl].tabs[t].cwd.clone())
                    }
                    None => None,
                };
                let (mut opened, mut refused) = (vec![], vec![]);
                for it in items.iter().take(20) {
                    let low = it.to_lowercase();
                    if low.starts_with("http://") || low.starts_with("https://") {
                        opened.push(it.clone());
                        continue;
                    }
                    let raw = crate::config::expand_tilde(it);
                    // A relative path: in the given tab, else the tab that has it.
                    let p = if raw.is_absolute() {
                        raw
                    } else {
                        let found = || roots.iter().map(|r| r.join(&raw)).find(|c| c.exists());
                        match &base {
                            Some(b) => b.join(&raw),
                            None => found()
                                .unwrap_or_else(|| self.panes[self.focus].cur().cwd.join(&raw)),
                        }
                    };
                    match p.canonicalize() {
                        Ok(c) if crate::guard::denied(&c, &self.workspaces()).is_some() => refused.push(json!({"path": c.to_string_lossy(), "reason": crate::guard::denied(&c, &self.workspaces())})),
                        Ok(c) if crate::guard::open_refusal(&c).is_some() => {
                            let why = crate::guard::open_refusal(&c).unwrap_or_default();
                            crate::log::info(&format!("control: open_path refused {}: {why}", c.display()));
                            refused.push(json!({"path": c.to_string_lossy(), "reason": format!("{why}; only documents, folders and http(s) links are opened")}));
                        }
                        Ok(c) if roots.iter().any(|r| c.starts_with(r)) => opened.push(c.to_string_lossy().into_owned()),
                        Ok(c) => refused.push(json!({"path": c.to_string_lossy(), "reason": "outside every tab's folder"})),
                        Err(_) => refused.push(json!({"path": it, "reason": "does not exist"})),
                    }
                }
                for o in &opened {
                    crate::log::info(&format!("control: open {o}"));
                    if !cfg!(test) && std::env::var_os("GODTERM_NO_OPEN").is_none() {
                        let _ = std::process::Command::new("open")
                            .arg(o)
                            .stdin(std::process::Stdio::null())
                            .stdout(std::process::Stdio::null())
                            .stderr(std::process::Stdio::null())
                            .spawn();
                    }
                }
                self.opened_paths.extend(opened.iter().cloned());
                let say = match (opened.len(), refused.len()) {
                    (n, 0) => format!("Opened {}.", plural(n, "item")),
                    (n, r) => format!(
                        "Opened {}; {} could not be opened.",
                        plural(n, "item"),
                        plural(r, "item")
                    ),
                };
                ok(json!({"opened": opened, "refused": refused, "say": say}))
            }
            "read_file" => {
                let p = self.path_in_tab(args)?;
                if let Some(why) = crate::guard::denied(&p, &self.workspaces()) {
                    crate::log::info(&format!(
                        "control: read_file refused {}: {why}",
                        crate::config::tilde(&p)
                    ));
                    return Err(format!("refused: {why}"));
                }
                let n = args
                    .get("lines")
                    .and_then(Value::as_u64)
                    .unwrap_or(40)
                    .clamp(1, 200) as usize;
                let (head, total, more) = read_head(&p, n)?;
                let mut v = json!({"file": crate::config::tilde(&p), "lines": head});
                match total {
                    Some(t) => v["total_lines"] = json!(t),
                    None => v["more"] = json!(more),
                }
                ok(v)
            }
            "open_tab" | "new_tab" | "new_tab_with_prompt" => {
                // A confirmed (or re-asked) open in the home folder.
                if args.get("confirm_token").is_some() || args.get("reissue_token").is_some() {
                    return self.planned("open_tab", args);
                }
                let acct = match args.get("account") {
                    Some(v) if !v.is_null() => self.account_arg(v)?,
                    _ => self
                        .panes
                        .get(self.focus)
                        .and_then(|p| p.account)
                        .ok_or("no account on the focused pane")?,
                };
                if !self.accounts[acct].login.logged_in() {
                    return Err(format!(
                        "{} is not logged in",
                        self.cfg.accounts[acct].display()
                    ));
                }
                let mut created: Option<PathBuf> = None;
                let folder = match s("dir")
                    .map(|d| crate::fuzzy::spoken_punctuation(&d))
                    .filter(|d| !d.trim().is_empty())
                {
                    Some(d)
                        if d.starts_with('/')
                            || d.starts_with('~')
                            || std::path::Path::new(&d).is_absolute() =>
                    {
                        let p = crate::config::expand_tilde(&d);
                        if !p.is_dir() {
                            // New work in a folder that is not there yet:
                            // made (under the home folder, never a system
                            // or private place), then opened.
                            let create = args["create"].as_bool() == Some(true);
                            match new_folder(&p, create, &self.workspaces()) {
                                // Said as the user named it.
                                Ok(_) => created = Some(p.clone()),
                                Err(e) => return Err(e),
                            }
                        }
                        // Never system folders or private ones; the home
                        // folder itself only after the user says yes.
                        match crate::guard::tab_folder(&p, &self.workspaces()) {
                            Err(why) => {
                                crate::log::info(&format!("control: open_tab refused {d}: {why}"));
                                return Err(format!("refused: {why}"));
                            }
                            Ok(true) if !std::mem::take(&mut self.open_home_ok) => {
                                return self.planned("open_tab", args);
                            }
                            Ok(_) => {}
                        }
                        p
                    }
                    Some(d) => match self
                        .recents
                        .iter()
                        .find(|r| {
                            std::path::Path::new(&r.path)
                                .file_name()
                                .is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(&d))
                        })
                        .map(|r| PathBuf::from(&r.path))
                    {
                        Some(p) => p,
                        // Said aloud: the closest recent or session folder.
                        None => {
                            let mut known: Vec<String> =
                                self.recents.iter().map(|r| r.path.clone()).collect();
                            known.extend(
                                self.session_index
                                    .lock()
                                    .unwrap_or_else(|e| e.into_inner())
                                    .rows
                                    .iter()
                                    .map(|r| r.info.cwd.clone()),
                            );
                            known.sort();
                            known.dedup();
                            known.retain(|k| {
                                std::path::Path::new(k).is_dir()
                                    && crate::guard::tab_folder(
                                        std::path::Path::new(k),
                                        &self.workspaces(),
                                    ) == Ok(false)
                            });
                            match crate::fuzzy::best_folder(&d, known.iter().map(String::as_str)) {
                                crate::fuzzy::Match::Sure(f) => {
                                    self.corrections.borrow_mut().push((
                                        d.clone(),
                                        crate::config::tilde(std::path::Path::new(&f)),
                                    ));
                                    PathBuf::from(f)
                                }
                                crate::fuzzy::Match::Unsure(c) => {
                                    return Err(format!(
                                        "no folder called {d}; did you mean: {}",
                                        c.iter()
                                            .map(|c| crate::config::tilde(std::path::Path::new(
                                                &c.value
                                            )))
                                            .collect::<Vec<_>>()
                                            .join(", ")
                                    ));
                                }
                                crate::fuzzy::Match::None => {
                                    return Err(format!(
                                        "no recent folder called {d}; give a path, or a name for a new folder"
                                    ));
                                }
                            }
                        }
                    },
                    None => self
                        .assistant_folder(acct, s("name").as_deref())
                        .map_err(|e| format!("{e:#}"))?,
                };
                let slot = self.pane_for_account(acct);
                if self.panes[slot].account != Some(acct) {
                    return Err("that account has no pane".into());
                }
                self.panes[slot].hidden = false;
                let count = args
                    .get("count")
                    .and_then(Value::as_u64)
                    .unwrap_or(1)
                    .clamp(1, 10) as usize;
                let prompt = s("prompt").filter(|p| !p.trim().is_empty());
                let mut uids = vec![];
                let mut ids = vec![];
                let mut opened = vec![];
                for k in 0..count {
                    let name = s("name").filter(|n| !n.trim().is_empty()).map(|n| {
                        if count > 1 {
                            format!("{n}-{}", k + 1)
                        } else {
                            n
                        }
                    });
                    let dir = if count == 1 || s("dir").is_some() {
                        folder.clone()
                    } else {
                        if k == 0 {
                            // Made for the plain name; each gets name-N instead.
                            let _ = std::fs::remove_dir(&folder);
                        }
                        self.assistant_folder(acct, name.as_deref())
                            .map_err(|e| format!("{e:#}"))?
                    };
                    self.open_tab(slot, crate::picker::PickAction::NewIn(dir.clone()));
                    let ti = self.panes[slot].active;
                    if let Some(n) = &name {
                        self.panes[slot].tabs[ti].custom_name = Some(n.chars().take(40).collect());
                    }
                    let uid = self.panes[slot].tabs[ti].uid;
                    uids.push(uid);
                    opened.push(json!({"tab": tab_id(uid), "folder": crate::config::tilde(&dir)}));
                    if let Some(p) = &prompt {
                        ids.push(self.deliver(uid, p));
                    }
                }
                self.last_target = uids.last().copied();
                let mut v = json!({"_wait": {"ready_uids": uids, "deliveries": ids, "wait_queued": true, "max_ms": 18000},
                       "account": acct + 1, "account_label": self.cfg.accounts[acct].display()});
                if count == 1 {
                    v["tab"] = opened[0]["tab"].clone();
                    v["folder"] = opened[0]["folder"].clone();
                } else {
                    v["opened"] = json!(opened);
                }
                if let Some(c) = &created {
                    let f = crate::config::tilde(c);
                    v["created"] = json!(f);
                    v["say"] = json!(format!(
                        "Created {f} and opened a session there on {}.",
                        self.cfg.accounts[acct].display()
                    ));
                }
                v
            }
            "take_over_session" if s("mode").as_deref() == Some("copy") => {
                let id = s("id").ok_or("id is required")?;
                let (live, target) = self.takeover_target(&id, args)?;
                ok(
                    json!({"started": self.start_takeover(live, target, crate::takeover::Mode::Copy, false, false)?}),
                )
            }
            "send_prompt" | "answer_prompt" | "close_tabs" | "close_tab" | "stop_loops"
            | "stop_loop" | "broadcast" | "take_over_session" => {
                let tool = match tool {
                    "close_tab" => "close_tabs",
                    "stop_loop" => "stop_loops",
                    "broadcast" => "send_prompt",
                    t => t,
                };
                return self.planned(tool, args);
            }
            "show" | "focus" | "zoom" | "set_layout" | "show_view" => {
                if s("view").as_deref() == Some("help") {
                    self.modal = crate::app::Modal::Help;
                    return Ok(ok(json!("showing help")));
                }
                let mut did = vec![];
                if let Some(l) = s("layout") {
                    self.set_layout(&l);
                    did.push(format!("layout {l}"));
                }
                if args.get("tab").is_some() || args.get("account").is_some() {
                    let (sl, t) = self.one_tab(args)?;
                    self.panes[sl].hidden = false;
                    self.jump_to(sl, t);
                    did.push(format!("focused {}", tab_id(self.panes[sl].tabs[t].uid)));
                }
                if let Some(z) = args
                    .get("zoom")
                    .and_then(Value::as_bool)
                    .or(args.get("on").and_then(Value::as_bool))
                {
                    self.view = View::Grid;
                    self.zoom = z;
                    did.push(if z {
                        "zoomed".into()
                    } else {
                        "showing every pane".into()
                    });
                }
                if let Some(v) = s("view") {
                    self.modal = crate::app::Modal::None;
                    match v.as_str() {
                        "grid" => self.go_home(),
                        "dashboard" => self.view = View::Dashboard,
                        "settings" => self.view = View::Settings,
                        "sessions" => self.set_source(crate::app_sessions::SourceSel::All),
                        "loops" => {
                            self.view = View::Loops;
                            self.refresh_loops(true);
                        }
                        "overview" => {
                            self.view = View::Grid;
                            self.open_overview();
                        }
                        "approvals" => self.modal = crate::app::Modal::Approvals(0),
                        "livemap" | "live_map" | "map" => {
                            self.view = View::Grid;
                            self.open_livemap();
                        }
                        other => return Err(format!("unknown view {other}")),
                    }
                    did.push(format!("showing {v}"));
                }
                if did.is_empty() {
                    return Err("give tab, view, zoom or layout".into());
                }
                ok(json!(did.join(", ")))
            }
            "press_key" => {
                let (sl, t) = self.one_tab(args)?;
                let bytes: &[u8] = match s("key").as_deref() {
                    Some("escape") | Some("esc") => b"\x1b",
                    Some("enter") => b"\r",
                    Some("ctrl_c") => b"\x03",
                    _ => return Err("key must be escape, enter or ctrl_c".into()),
                };
                // Enter on a permission or trust prompt would approve it
                // without the user's yes: that goes through answer_prompt.
                let asking = self.panes[sl].tabs[t].activity == Activity::Permission
                    || self.request_of(sl, t).is_some();
                if bytes == b"\r" && asking {
                    crate::log::info(&format!(
                        "control: press_key enter refused on a prompt in {}",
                        tab_id(self.panes[sl].tabs[t].uid)
                    ));
                    return Err("refused: that tab is showing a permission or trust prompt; Enter would approve it. Use answer_prompt (it asks the user when needed).".into());
                }
                self.panes[sl].tabs[t].write(bytes);
                ok(
                    json!({"tab": tab_id(self.panes[sl].tabs[t].uid), "pressed": s("key"), "state": self.state_word(sl, t)}),
                )
            }
            "pause_listening" => {
                let secs =
                    self.pause_listening(args.get("seconds").and_then(Value::as_u64), s("reason"));
                let ww = self
                    .cfg
                    .voice
                    .wake_words
                    .first()
                    .cloned()
                    .unwrap_or_default();
                ok(
                    json!({"paused_s": secs, "say": format!("Sure, I'll wait {}. Say {ww} if you need me sooner.", crate::app_pause::spoken_duration(secs))}),
                )
            }
            "reopen_closed" => {
                let r = match args.get("index").and_then(Value::as_u64) {
                    Some(i) => self.reopen_closed_at(i as usize),
                    None => self.reopen_closed(),
                }?;
                ok(json!({"reopened": true, "say": format!("{r}.")}))
            }
            "move_pane" => {
                let p = match args.get("pane") {
                    None | Some(Value::Null) => self.focus,
                    Some(Value::Number(n)) => {
                        let vis = self.visible_panes();
                        *(n.as_u64().unwrap_or(0) as usize)
                            .checked_sub(1)
                            .and_then(|i| vis.get(i))
                            .ok_or(format!("there is no pane {n} ({} shown)", vis.len()))?
                    }
                    Some(v) => {
                        let a = self.account_arg(v)?;
                        self.panes
                            .iter()
                            .position(|s| s.account == Some(a) && !s.hidden)
                            .ok_or("that account has no pane on screen")?
                    }
                };
                let from = self.slot_number(p);
                let to = match args.get("to") {
                    Some(Value::Number(n)) => {
                        self.move_pane_to_slot(p, n.as_u64().unwrap_or(0) as usize)?
                    }
                    Some(Value::String(d)) => match d.trim().to_lowercase().as_str() {
                        "left" => self.move_pane(p, -1, 0)?,
                        "right" => self.move_pane(p, 1, 0)?,
                        "up" => self.move_pane(p, 0, -1)?,
                        "down" => self.move_pane(p, 0, 1)?,
                        x => match x.trim_start_matches("slot").trim().parse::<usize>() {
                            Ok(n) => self.move_pane_to_slot(p, n)?,
                            Err(_) => {
                                return Err(format!(
                                    "to: left, right, up, down or a slot number, not \"{d}\""
                                ));
                            }
                        },
                    },
                    _ => return Err("to is required".into()),
                };
                let n = self.slot_number(to);
                ok(
                    json!({"moved": true, "from_slot": from, "to_slot": n, "say": format!("Moved pane {from} to slot {n}.")}),
                )
            }
            "rename_tab" => {
                let (sl, t) = self.one_tab(args)?;
                ok(json!(self.rename_tab(
                    sl,
                    t,
                    &s("name").unwrap_or_default()
                )))
            }
            "copy_session" => {
                let sid = s("session").ok_or("session is required")?;
                let target = self.account_arg(args.get("account").ok_or("account is required")?)?;
                // Any source the sessions tool lists: accounts and the main
                // ~/.claude and ~/.grok (through the index, like open_session).
                self.kick_index(false);
                let r = {
                    let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
                    let building = ix.building && ix.built.is_none();
                    match ix
                        .rows
                        .iter()
                        .find(|r| {
                            r.info.parent.is_none()
                                && (r.info.id == sid || r.info.id.starts_with(&sid))
                        })
                        .cloned()
                    {
                        Some(r) => r,
                        None if building => {
                            return Err("still indexing sessions (a few seconds); try again".into());
                        }
                        None => {
                            return Err("no session with that id; call sessions to find it".into());
                        }
                    }
                };
                if self.cfg.accounts[target].harness() != r.harness {
                    return Err(format!(
                        "that is a {} session; copy it to a {} account",
                        r.harness.name(),
                        r.harness.name()
                    ));
                }
                let (from, to) = (self.src_dir(r.src), self.cfg.accounts[target].config_dir());
                if from == to {
                    return Err("it is already on that account".into());
                }
                let (copied, id) = match r.harness {
                    crate::harness::Harness::Claude => {
                        let c = crate::session_ops::copy_session(
                            &from,
                            &r.info.path,
                            &r.info.id,
                            &to,
                            crate::session_ops::Conflict::Skip,
                        )
                        .map_err(|e| format!("{e:#}"))?;
                        (!c.skipped, c.id)
                    }
                    crate::harness::Harness::Grok => {
                        crate::harness::grok::copy_session(
                            &r.info.path,
                            &from,
                            &to,
                            false,
                            &crate::config::app_home().join("trash"),
                        )
                        .map_err(|e| format!("{e:#}"))?;
                        (true, r.info.id.clone())
                    }
                };
                ok(
                    json!({"copied": copied, "id": id, "from": r.source, "to": self.cfg.accounts[target].display()}),
                )
            }
            "move_tab" => {
                let many = args.get("group").is_some()
                    || matches!(args.get("tab"), Some(Value::Array(_)))
                    || s("tab")
                        .is_some_and(|t| matches!(t.as_str(), "all" | "waiting" | "working"));
                let tabs = self.tab_targets(args, many)?;
                let to = match args.get("to") {
                    Some(Value::String(x)) if x.eq_ignore_ascii_case("best") => {
                        let from = tabs[0].0;
                        self.move_targets(from)
                            .into_iter()
                            .find(|m| m.disabled.is_none() && !m.exhausted())
                            .map(|m| m.account)
                            .ok_or("no other account with usage left")?
                    }
                    Some(v) => self.account_arg(v)?,
                    None => return Err("to is required".into()),
                };
                let copy = args.get("copy").and_then(Value::as_bool).unwrap_or(false);
                let uids: Vec<u64> = tabs
                    .iter()
                    .map(|&(sl, t)| self.panes[sl].tabs[t].uid)
                    .collect();
                // Their groups come along.
                let groups: Vec<(u64, crate::tab_groups::TabGroup)> = tabs
                    .iter()
                    .filter_map(|&(sl, t)| {
                        let g = self.panes[sl].group_of(t)?;
                        Some((self.panes[sl].tabs[t].uid, self.panes[sl].groups[g].clone()))
                    })
                    .collect();
                let (mut moved, mut there, mut later) = (vec![], vec![], vec![]);
                for uid in uids {
                    let Some((sl, t)) = self.find_tab(uid) else {
                        continue;
                    };
                    if self.panes[sl].account == Some(to) {
                        there.push(tab_id(uid));
                        continue;
                    }
                    if self.panes[sl].tabs[t].is_running()
                        && self.panes[sl].tabs[t].activity == Activity::Working
                    {
                        // Busy: it moves once its turn ends.
                        self.queued_moves.push(crate::app_tabmove::QueuedMove {
                            uid,
                            target: to,
                            copy,
                            same_folder: true,
                        });
                        later.push(tab_id(uid));
                        continue;
                    }
                    self.move_tab_now(sl, t, to, copy, true);
                    let new = self.moved_ids.get(&uid).copied().or_else(|| {
                        self.pending_moves
                            .last()
                            .filter(|m| m.source_uid == uid)
                            .map(|m| m.target_uid)
                    });
                    moved.push(json!({"from": tab_id(uid), "now": new.map(tab_id)}));
                    if let (Some(n), Some((_, g))) = (new, groups.iter().find(|(u, _)| *u == uid)) {
                        self.regroup_moved(&g.clone(), &[n]);
                    }
                    if let Some(n) = new {
                        self.last_target = Some(n);
                    }
                }
                let label = self.cfg.accounts[to].display().to_string();
                let mut say = vec![];
                if !moved.is_empty() {
                    say.push(format!(
                        "{} {} to {label}.",
                        if copy { "Copied" } else { "Moved" },
                        plural(moved.len(), "tab")
                    ));
                }
                let warning = self.out_warning(to);
                if let Some(w) = &warning {
                    say.push(w.clone());
                }
                if !there.is_empty() {
                    say.push(format!("{} already there.", plural(there.len(), "tab")));
                }
                if !later.is_empty() {
                    say.push(format!(
                        "{} busy; moving once done.",
                        plural(later.len(), "tab")
                    ));
                }
                ok(
                    json!({"moved": moved, "already_there": there, "after_current_work": later, "to": label, "warning": warning,
                          "note": "moved tabs continue under their new id (now); old ids still resolve to them", "say": say.join(" ")}),
                )
            }
            "set_mode" => {
                let b = |k: &str| args.get(k).and_then(Value::as_bool);
                let mut did = vec![];
                if let Some(on) = b("memory_saver") {
                    self.set_memory_saver(on);
                    did.push(format!("memory saver {}", if on { "on" } else { "off" }));
                }
                if let Some(on) = b("privacy") {
                    self.set_privacy(on);
                    did.push(format!("privacy {}", if on { "on" } else { "off" }));
                }
                if let Some(on) = b("show_emails") {
                    self.set_show_email(on);
                    did.push(format!("emails {}", if on { "shown" } else { "hidden" }));
                }
                if b("free_memory") == Some(true) {
                    self.free_now();
                    did.push("freed memory".into());
                }
                if let Some(v) = s("voice") {
                    let m = match v.as_str() {
                        "off" => 0,
                        "push" => 1,
                        "wake" => 2,
                        "open" => 3,
                        _ => return Err("voice must be off, push, wake or open".into()),
                    };
                    if m == 3 {
                        self.start_open_mic();
                    } else {
                        self.set_voice_mode(m);
                    }
                    did.push(format!("voice {v}"));
                }
                if let Some(a) = args.get("assistant_account").filter(|v| !v.is_null()) {
                    let which = match a.as_str() {
                        Some(x) if x.eq_ignore_ascii_case("best") => None,
                        _ => Some(self.account_arg(a)?),
                    };
                    self.assistant.resume_after = None;
                    self.set_assistant_account_after_turn(which);
                    did.push("assistant account changes after this answer".into());
                }
                if b("new_conversation") == Some(true) {
                    self.assistant.reset_after_turn = true;
                    did.push("a new conversation starts after this answer".into());
                }
                if did.is_empty() {
                    return Err("nothing to change".into());
                }
                ok(json!(did.join(", ")))
            }
            "system_check" => {
                // Checked here, run off the UI thread; every call logged.
                let job: Box<dyn FnOnce() -> Value + Send> = match s("helper").as_deref() {
                    Some("process_tree") => {
                        let pid = args
                            .get("pid")
                            .and_then(Value::as_u64)
                            .ok_or("pid is required")? as u32;
                        Box::new(move || crate::syscheck::process_tree(pid))
                    }
                    Some("port_owner") => {
                        let port = args
                            .get("port")
                            .and_then(Value::as_u64)
                            .filter(|p| *p > 0 && *p < 65536)
                            .ok_or("port is required")? as u16;
                        Box::new(move || crate::syscheck::port_owner(port))
                    }
                    Some(h) => {
                        return Err(format!("unknown helper {h}: process_tree or port_owner"))
                    }
                    None => {
                        let argv: Vec<String> = match args.get("command") {
                            Some(Value::Array(a)) => a
                                .iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect(),
                            Some(Value::String(c)) => {
                                c.split_whitespace().map(str::to_string).collect()
                            }
                            _ => return Err("command or helper is required".into()),
                        };
                        let cwd = match args.get("tab") {
                            Some(v) if !v.is_null() => {
                                let (sl, t) = self.one_tab(args)?;
                                Some(self.panes[sl].tabs[t].cwd.clone())
                            }
                            _ => None,
                        };
                        let c = crate::syscheck::check(&argv, cwd.as_deref(), &self.workspaces())
                            .inspect_err(|e| {
                            crate::log::info(&format!("system_check refused {argv:?}: {e}"))
                        })?;
                        Box::new(move || crate::syscheck::run(&c))
                    }
                };
                crate::log::info(&format!(
                    "system_check: {}",
                    args.to_string().chars().take(200).collect::<String>()
                ));
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(job());
                });
                self.pending_job = Some(rx);
                json!({"_wait": {"job": true, "deliveries": [], "max_ms": 7000}})
            }
            "tab_history" => {
                let now = chrono::Local::now();
                let when = |k: &str| s(k).and_then(|w| crate::session_index::parse_when(&w, now));
                let account = match args.get("account") {
                    Some(v) if !v.is_null() => {
                        Some(self.cfg.accounts[self.account_arg(v)?].name.clone())
                    }
                    _ => None,
                };
                let f = crate::tab_history::Filter {
                    since: when("since"),
                    until: when("until"),
                    text: s("text"),
                    account,
                    event: s("event"),
                    limit: args
                        .get("limit")
                        .and_then(Value::as_u64)
                        .unwrap_or(10)
                        .clamp(1, 50) as usize,
                };
                let recs = crate::tab_history::query(&crate::tab_history::read_all(), &f);
                let label = |a: &str| {
                    self.cfg
                        .accounts
                        .iter()
                        .find(|x| x.name == a)
                        .map(|x| x.display().to_string())
                        .unwrap_or_else(|| a.to_string())
                };
                let rows: Vec<Value> = recs
                    .iter()
                    .map(|r| json!({
                        "record": r.id, "at": r.at.chars().take(16).collect::<String>().replace('T', " "), "event": r.event, "tab": r.tab, "name": r.name,
                        "folder": crate::config::tilde(std::path::Path::new(&r.cwd)), "account": label(&r.account), "session": r.session_id, "by": r.by, "detail": r.detail,
                    }))
                    .collect();
                let say = if rows.is_empty() {
                    "No tab matches.".to_string()
                } else {
                    format!("{}.", plural(rows.len(), "record"))
                };
                ok(json!({"records": rows, "say": say}))
            }
            "reopen_tab" => {
                let id = s("id").ok_or("id is required")?;
                let msg = self.reopen_record(id.trim())?;
                if let Some(n) = s("name").filter(|n| !n.trim().is_empty()) {
                    let (p, t) = (self.focus, self.panes[self.focus].active);
                    self.rename_tab(p, t, &n);
                }
                let tab = self.panes.get(self.focus).map(|p| tab_id(p.cur().uid));
                ok(json!({"reopened": true, "tab": tab, "say": format!("{msg}.")}))
            }
            "find" => {
                let q = s("query")
                    .filter(|q| !q.trim().is_empty())
                    .ok_or("query is required")?;
                let since = s("since")
                    .and_then(|w| crate::session_index::parse_when(&w, chrono::Local::now()));
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(8)
                    .clamp(1, 30) as usize;
                ok(json!({"hits": self.find_everywhere(&q, since, limit)}))
            }
            "remember_summary" => {
                let text = s("summary")
                    .filter(|t| !t.trim().is_empty())
                    .ok_or("summary is required")?;
                let conv = self
                    .assistant
                    .conv
                    .get_or_insert_with(crate::assistant_history::ConvLog::new);
                conv.write(
                    "summary",
                    json!({"text": crate::sessions::snippet(&text, 600)}),
                );
                ok(json!({"saved": true}))
            }
            "history" => match s("action").as_deref() {
                Some("resume") => {
                    let id = s("id").ok_or("id is required")?;
                    if crate::assistant_history::ConvLog::open(&id).is_none() {
                        return Err(format!("no saved conversation {id}"));
                    }
                    self.assistant.resume_after = Some(id.clone());
                    ok(json!({"resuming": id, "note": "it continues after this answer"}))
                }
                Some("detail") => {
                    let id = s("id").ok_or("id is required")?;
                    let c = crate::assistant_history::ConvLog::open(&id)
                        .ok_or(format!("no saved conversation {id}"))?;
                    let turns: Vec<Value> = crate::assistant_history::read(&c.path)
                        .into_iter()
                        .filter(|e| {
                            matches!(
                                e["kind"].as_str(),
                                Some("user" | "reply" | "tool" | "summary")
                            )
                        })
                        .map(|e| {
                            let kind = e["kind"].as_str().unwrap_or("").to_string();
                            let at = e["at"]
                                .as_str()
                                .unwrap_or("")
                                .chars()
                                .skip(11)
                                .take(5)
                                .collect::<String>();
                            match kind.as_str() {
                                "tool" => json!({"at": at, "tool": e["name"], "args": e["args"]}),
                                _ => {
                                    let mut o = json!({"at": at});
                                    o[kind.as_str()] = json!(crate::sessions::snippet(
                                        e["text"].as_str().unwrap_or(""),
                                        400
                                    ));
                                    o
                                }
                            }
                        })
                        .collect();
                    let n = turns.len();
                    ok(
                        json!({"id": id, "events": turns.into_iter().rev().take(60).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>(), "total": n}),
                    )
                }
                _ => {
                    let now = chrono::Local::now();
                    let when =
                        |k: &str| s(k).and_then(|w| crate::session_index::parse_when(&w, now));
                    let (since, until) = (when("since"), when("until"));
                    let q = s("text")
                        .or_else(|| s("query"))
                        .map(|q| crate::fuzzy::spoken_punctuation(&q).to_lowercase())
                        .unwrap_or_default();
                    let limit = args
                        .get("limit")
                        .and_then(Value::as_u64)
                        .unwrap_or(5)
                        .clamp(1, 30) as usize;
                    let id = s("id");
                    let mut items: Vec<Value> = vec![];
                    for c in crate::assistant_history::list() {
                        if id.as_deref().is_some_and(|i| !c.id.starts_with(i)) {
                            continue;
                        }
                        let path = crate::assistant_history::dir().join(format!("{}.jsonl", c.id));
                        let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
                        let started =
                            chrono::NaiveDateTime::parse_from_str(&c.started, "%Y-%m-%d %H:%M")
                                .ok()
                                .and_then(|t| t.and_local_timezone(chrono::Local).single())
                                .map(std::time::SystemTime::from);
                        if since.is_some_and(|t| modified.is_some_and(|m| m < t))
                            || until.is_some_and(|t| started.is_some_and(|s0| s0 > t))
                        {
                            continue;
                        }
                        let words_match = q.is_empty()
                            || c.text.contains(&q)
                            || crate::fuzzy::key(&c.text).contains(&crate::fuzzy::key(&q))
                            || q.split_whitespace().all(|w| c.text.contains(w));
                        if !words_match {
                            continue;
                        }
                        let m = crate::assistant_memory::remember(&path);
                        items.push(json!({
                            "id": c.id, "started": c.started, "ended": m.as_ref().map(|m| m.ended.clone()), "requests": c.turns,
                            "summary": m.map(|m| m.summary).unwrap_or_else(|| crate::sessions::snippet(&c.first, 120)),
                        }));
                        if items.len() >= limit {
                            break;
                        }
                    }
                    let say = if items.is_empty() {
                        "No saved conversation matches.".to_string()
                    } else {
                        format!("{}.", plural(items.len(), "conversation"))
                    };
                    ok(json!({"conversations": items, "say": say}))
                }
            },
            "ignore" => {
                // Not for pauses: GodTerm drops speech while paused, so a
                // message that arrives is meant to be answered.
                let reason = s("reason").unwrap_or_default().to_lowercase();
                if !self.paused()
                    && (reason.contains("paus")
                        || reason.contains("wake word")
                        || reason.contains("waiting"))
                {
                    crate::log::info(&format!(
                        "control: ignore refused while listening is active: {reason}"
                    ));
                    return Err("refused: listening is active (any pause has ended); respond to the user normally".into());
                }
                self.assistant.ignored_turn = true;
                ok(json!("ignored; say nothing"))
            }
            "sessions" => ok(self.query_sessions(args)?),
            "session_detail" => {
                let id = s("id").ok_or("id is required")?;
                self.kick_index(false);
                let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
                let r = ix
                    .rows
                    .iter()
                    .find(|r| r.info.id == id || r.info.id.starts_with(&id))
                    .cloned()
                    .ok_or("no session with that id (call sessions first)")?;
                drop(ix);
                let mut d = match r.harness {
                    crate::harness::Harness::Claude => {
                        crate::session_index::detail_claude(&r.info.path)
                    }
                    crate::harness::Harness::Grok => {
                        crate::session_index::detail_grok(&r.info.path)
                    }
                };
                d["session"] = crate::session_index::row_json(&r, None, false);
                ok(d)
            }
            "open_session" => {
                let id = s("id").ok_or("id is required")?;
                self.kick_index(false);
                let r = self
                    .session_index
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .rows
                    .iter()
                    .find(|r| r.info.id == id || r.info.id.starts_with(&id))
                    .cloned()
                    .ok_or("no session with that id (call sessions first)")?;
                if r.info.parent.is_some() {
                    return Err("that is a subagent; open its parent session instead".into());
                }
                let acct = match r.src {
                    crate::app_sessions::Src::Account(a) => a,
                    _ => {
                        // A main install session: copy it into an account first.
                        let want = match args.get("account") {
                            Some(v) if !v.is_null() => self.account_arg(v)?,
                            _ => (0..self.cfg.accounts.len())
                                .filter(|&a| {
                                    self.cfg.accounts[a].harness() == r.harness
                                        && self.accounts[a].login.logged_in()
                                })
                                .max_by(|a, b| {
                                    self.accounts[*a]
                                        .effective_left()
                                        .unwrap_or(0.0)
                                        .partial_cmp(
                                            &self.accounts[*b].effective_left().unwrap_or(0.0),
                                        )
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                })
                                .ok_or_else(|| {
                                    format!(
                                        "no logged in {} account to open it on",
                                        r.harness.name()
                                    )
                                })?,
                        };
                        if self.cfg.accounts[want].harness() != r.harness {
                            return Err(format!(
                                "that is a {} session; pick a {} account",
                                r.harness.name(),
                                r.harness.name()
                            ));
                        }
                        let from = self.src_dir(r.src);
                        let to = self.cfg.accounts[want].config_dir();
                        match r.harness {
                            crate::harness::Harness::Claude => {
                                crate::session_ops::copy_session(
                                    &from,
                                    &r.info.path,
                                    &r.info.id,
                                    &to,
                                    crate::session_ops::Conflict::Skip,
                                )
                                .map_err(|e| format!("{e:#}"))?;
                            }
                            crate::harness::Harness::Grok => {
                                crate::harness::grok::copy_session(
                                    &r.info.path,
                                    &from,
                                    &to,
                                    false,
                                    &crate::config::app_home().join("trash"),
                                )
                                .map_err(|e| format!("{e:#}"))?;
                            }
                        }
                        if let Some(st) = self.accounts.get_mut(want) {
                            if !st.sessions.iter().any(|x| x.id == r.info.id) {
                                st.sessions.insert(0, r.info.clone());
                            }
                        }
                        want
                    }
                };
                self.resume(acct, r.info.id.clone());
                // A rename asked with it happens at once.
                if let Some(n) = s("name").filter(|n| !n.trim().is_empty()) {
                    let (p, t) = (self.focus, self.panes[self.focus].active);
                    self.rename_tab(p, t, &n);
                }
                let tab = self.panes.get(self.focus).map(|p| tab_id(p.cur().uid));
                let folder = self
                    .panes
                    .get(self.focus)
                    .map(|p| crate::config::tilde(&p.cur().cwd));
                if let Some(p) = self.panes.get(self.focus) {
                    self.last_target = Some(p.cur().uid);
                }
                // The folder too: remembered later, it finds the tab again.
                ok(
                    json!({"opened": r.info.id, "account": self.cfg.accounts[acct].display(), "tab": tab, "folder": folder, "say": format!("Opened it on {} in a new tab.", self.cfg.accounts[acct].display())}),
                )
            }
            "speak" => {
                let text = s("text").ok_or("text is required")?;
                if !self.reply_spoken() {
                    self.assistant.log.push(crate::app_assistant::Entry {
                        who: crate::app_assistant::Who::Reply,
                        text: text.clone(),
                    });
                    return Ok(ok(json!(
                        "shown as text, not said: the speaker is muted or the message was typed"
                    )));
                }
                self.speak_assistant(&text);
                ok(json!("spoken"))
            }
            other => return Err(format!("unknown tool {other}")),
        })
    }

    /// Where sessions live, for the index.
    pub fn index_sources(&self) -> Vec<crate::session_index::Source> {
        use crate::app_sessions::Src;
        use crate::harness::Harness;
        let mut v: Vec<crate::session_index::Source> = (0..self.cfg.accounts.len())
            .map(|a| {
                (
                    Src::Account(a),
                    self.cfg.accounts[a].harness(),
                    self.cfg.accounts[a].config_dir(),
                    self.cfg.accounts[a].display().to_string(),
                )
            })
            .collect();
        // Tests only look at the main installs when pointed at fixtures.
        let mains = crate::config::dirs().mains;
        if mains {
            v.push((
                Src::Main,
                Harness::Claude,
                crate::session_ops::main_dir(),
                "main ~/.claude".into(),
            ));
        }
        let g = Harness::Grok.main_home();
        if mains && g.join("sessions").is_dir() {
            v.push((Src::MainGrok, Harness::Grok, g, "main ~/.grok".into()));
        }
        v
    }

    /// Refresh the session index in the background when it is stale.
    pub fn kick_index(&mut self, force: bool) {
        let stale = {
            let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
            !ix.building
                && (force
                    || ix
                        .built
                        .is_none_or(|t| t.elapsed() > crate::session_index::STALE))
        };
        if !stale {
            return;
        }
        let (shared, sources) = (
            std::sync::Arc::clone(&self.session_index),
            self.index_sources(),
        );
        self.session_index
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .building = true;
        let work = move || crate::session_index::rebuild(&shared, &sources);
        if cfg!(test) {
            work();
        } else {
            std::thread::spawn(work);
        }
    }

    /// The sessions tool.
    fn query_sessions(&mut self, args: &Value) -> Result<Value, String> {
        use crate::harness::Harness;
        self.kick_index(false);
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        let now = chrono::Local::now();
        let since = s("since")
            .map(|w| {
                crate::session_index::parse_when(&w, now).ok_or(format!(
                    "since: cannot read \"{w}\" (try today, yesterday, 3 days, 2026-10-01)"
                ))
            })
            .transpose()?;
        let until = s("until")
            .map(|w| {
                crate::session_index::parse_when(&w, now)
                    .ok_or(format!("until: cannot read \"{w}\""))
            })
            .transpose()?;
        let harness = s("harness").filter(|h| h != "all").map(|h| Harness::of(&h));
        let source = match args.get("source") {
            None | Some(Value::Null) => None,
            Some(Value::String(x)) if x == "all" => None,
            Some(Value::String(x)) if x.starts_with("main") => Some("main".to_string()),
            Some(v) => Some(
                self.cfg.accounts[self.account_arg(v)?]
                    .display()
                    .to_string(),
            ),
        };
        let subs = args
            .get("include_subagents")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let with_headless = args
            .get("include_headless")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let reverse = args
            .get("reverse")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let sort = match s("sort").as_deref() {
            None | Some("recent") => None,
            Some("oldest") => Some((crate::sess_sort::SortKey::Modified, !reverse)),
            Some(k) => Some((crate::sess_sort::SortKey::parse(k).ok_or(format!("sort: unknown \"{k}\" (modified, created, messages, tokens, context, project, source, title)"))?, reverse)),
        };
        let state = s("state").unwrap_or_else(|| "all".into());
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(10)
            .clamp(1, 50) as usize;
        let open: std::collections::HashMap<String, u64> = self
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter_map(|t| t.session_id.clone().map(|id| (id, t.uid)))
            .collect();
        let external: std::collections::HashMap<String, crate::takeover::Live> = self
            .live_sessions()
            .into_iter()
            .map(|l| (l.session_id.clone(), l))
            .collect();
        // One process listing for every session running elsewhere.
        let procs = if external.is_empty() {
            vec![]
        } else {
            crate::activity::process_table()
        };
        let ix = self.session_index.lock().unwrap_or_else(|e| e.into_inner());
        let building = ix.building && ix.built.is_none();
        let hidden = std::cell::Cell::new(0usize);
        // The project as said: spoken punctuation resolved, compared
        // without spaces, dashes or dots; a misheard one ("lol slash pod
        // mesh") is matched to the closest known folder.
        let mut project = s("project").map(|p| crate::fuzzy::spoken_punctuation(&p));
        let mut project_fix: Option<(String, String)> = None;
        let mut did_you_mean: Option<Vec<crate::fuzzy::Candidate>> = None;
        // "~" / "home" means sessions run in the home folder itself; a
        // path starting with ~ is that folder and below.
        let home = crate::config::home_dir().to_string_lossy().into_owned();
        if let Some(p) = project.as_deref() {
            let t = p.trim().trim_end_matches('/').to_lowercase();
            if matches!(
                t.as_str(),
                "~" | "home"
                    | "home directory"
                    | "home folder"
                    | "the home directory"
                    | "my home folder"
            ) {
                project = Some("=".to_string() + &home);
            } else if let Some(rest) = p.strip_prefix('~') {
                project = Some(format!("{home}{rest}"));
            }
        }
        let in_project = |cwd: &str, p: &str| match p.strip_prefix('=') {
            Some(exact) => cwd.trim_end_matches('/') == exact,
            None => {
                cwd.to_lowercase().contains(&p.to_lowercase())
                    || crate::fuzzy::key(cwd).contains(&crate::fuzzy::key(p))
            }
        };
        if let Some(p) = project.clone().filter(|p| !p.starts_with('=')) {
            if !ix.rows.iter().any(|r| in_project(&r.info.cwd, &p)) {
                let mut folders: Vec<&str> = ix
                    .rows
                    .iter()
                    .map(|r| r.info.cwd.as_str())
                    .filter(|c| !c.is_empty())
                    .collect();
                folders.sort();
                folders.dedup();
                match crate::fuzzy::best_folder(&p, folders) {
                    crate::fuzzy::Match::Sure(f) => {
                        project_fix = Some((
                            s("project").unwrap_or_default(),
                            crate::config::tilde(std::path::Path::new(&f)),
                        ));
                        project = Some(f.to_lowercase());
                    }
                    crate::fuzzy::Match::Unsure(c) => {
                        did_you_mean = Some(
                            c.into_iter()
                                .map(|mut c| {
                                    c.value = crate::config::tilde(std::path::Path::new(&c.value));
                                    c
                                })
                                .collect(),
                        )
                    }
                    crate::fuzzy::Match::None => {}
                }
            }
        }
        let fresh = |r: &crate::session_index::Row| {
            r.info.modified.is_some_and(|m| {
                m.elapsed().unwrap_or_default() < std::time::Duration::from_secs(300)
            })
        };
        let mut hits: Vec<(usize, &crate::session_index::Row)> = ix
            .rows
            .iter()
            .filter(|r| subs || r.info.parent.is_none())
            .filter(|r| harness.is_none_or(|h| r.harness == h))
            .filter(|r| {
                source.as_deref().is_none_or(|src| {
                    if src == "main" {
                        r.source.starts_with("main")
                    } else {
                        r.source == src
                    }
                })
            })
            .filter(|r| since.is_none_or(|t| r.info.modified.is_some_and(|m| m >= t)))
            .filter(|r| until.is_none_or(|t| r.info.modified.is_some_and(|m| m <= t)))
            .filter(|r| {
                project
                    .as_deref()
                    .is_none_or(|p| in_project(&r.info.cwd, p))
            })
            .filter(|r| match state.as_str() {
                "active" => {
                    open.contains_key(&r.info.id) || external.contains_key(&r.info.id) || fresh(r)
                }
                "recent" => r.info.modified.is_some_and(|m| {
                    m.elapsed().unwrap_or_default() < std::time::Duration::from_secs(3 * 86_400)
                }),
                _ => true,
            })
            .filter_map(|r| {
                let live_name = external.get(&r.info.id).and_then(|l| l.name.as_deref());
                let hay = format!(
                    "{} {} {} {} {} {} {}",
                    r.info.name.as_deref().unwrap_or(""),
                    live_name.unwrap_or(""),
                    r.info.title.as_deref().unwrap_or(""),
                    r.info.first_prompt.as_deref().unwrap_or(""),
                    r.info.last_prompt.as_deref().unwrap_or(""),
                    r.info.cwd,
                    r.info.model.as_deref().unwrap_or("")
                );
                match s("text").map(|q| crate::fuzzy::spoken_punctuation(&q)) {
                    // Words found, or a name that sounds like it.
                    Some(q) => crate::session_index::score(&q, &hay)
                        .map(|sc| (sc, r))
                        .or_else(|| {
                            let f = crate::session_index::fuzzy_score(&q, &r.info, live_name);
                            (f >= 0.78).then_some(((f * 100.0) as usize, r))
                        }),
                    None => Some((0, r)),
                }
            })
            // Headless last, so the hidden count is of otherwise matching rows.
            .filter(|(_, r)| {
                let h = crate::sess_sort::headless(&r.info);
                if h && !with_headless {
                    hidden.set(hidden.get() + 1);
                }
                with_headless || !h
            })
            .collect();
        match sort {
            Some((k, rev)) => hits.sort_by(|a, b| {
                crate::sess_sort::compare(
                    k,
                    rev,
                    (&a.1.source, &a.1.info),
                    (&b.1.source, &b.1.info),
                )
            }),
            None if s("text").is_some() => hits.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then(b.1.info.modified.cmp(&a.1.info.modified))
            }),
            None if reverse => hits.sort_by_key(|(_, r)| r.info.modified),
            None => hits.sort_by_key(|(_, r)| std::cmp::Reverse(r.info.modified)),
        }
        // Nothing matched the text: the closest sessions, so the model can
        // offer them (or ask) instead of saying none.
        let text_dym: Option<Vec<Value>> = match s("text") {
            Some(q) if hits.is_empty() => {
                let mut c: Vec<(f64, &crate::session_index::Row)> = ix
                    .rows
                    .iter()
                    .filter(|r| {
                        r.info.parent.is_none()
                            && (with_headless || !crate::sess_sort::headless(&r.info))
                    })
                    .map(|r| {
                        (
                            crate::session_index::fuzzy_score(
                                &q,
                                &r.info,
                                external.get(&r.info.id).and_then(|l| l.name.as_deref()),
                            ),
                            r,
                        )
                    })
                    .filter(|(f, _)| *f >= 0.45)
                    .collect();
                c.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
                Some(c.into_iter().take(5).map(|(f, r)| json!({"id": r.info.id, "title": crate::sessions::snippet(&r.info.summary(), 80), "folder": crate::config::tilde(std::path::Path::new(&r.info.cwd)), "score": (f * 100.0).round() / 100.0})).collect())
            }
            _ => None,
        };
        let total = hits.len();
        hits.truncate(limit);
        // Grouped: Claude first, then Grok (each keeping its order).
        hits.sort_by_key(|(_, r)| r.harness != Harness::Claude);
        let rows: Vec<Value> = hits
            .iter()
            .map(|(_, r)| {
                let mut v = crate::session_index::row_json(r, open.get(&r.info.id).map(|u| tab_id(*u)), open.contains_key(&r.info.id) || external.contains_key(&r.info.id) || fresh(r));
                if let Some(l) = external.get(&r.info.id) {
                    // Idle only when nothing at all runs (see activity.rs).
                    let act = crate::activity::of_live(l, &procs, 0);
                    v["external"] = json!({"pid": l.pid, "tty": l.tty, "name": l.name, "terminal": crate::takeover::terminal_of(l.pid), "state": act.state, "activity": act.json(), "take_over": "take_over_session brings it here"});
                }
                v
            })
            .collect();
        let n = |h: Harness| hits.iter().filter(|(_, r)| r.harness == h).count();
        let say = match (n(Harness::Claude), n(Harness::Grok)) {
            (0, 0) => "No sessions match.".to_string(),
            (c, 0) => format!(
                "{} Claude {}.",
                c,
                if c == 1 { "session" } else { "sessions" }
            ),
            (0, g) => format!(
                "{} Grok {}.",
                g,
                if g == 1 { "session" } else { "sessions" }
            ),
            (c, g) => format!(
                "{c} Claude and {g} Grok {}.",
                if c + g == 1 { "session" } else { "sessions" }
            ),
        };
        let hidden = hidden.get();
        let say = if hidden > 0 {
            format!("{say} ({hidden} headless hidden.)")
        } else {
            say
        };
        let mut out = json!({"sessions": rows, "matched": total, "shown": rows.len(), "headless_hidden": hidden, "indexing": building, "indexed": ix.rows.len(), "say": say});
        if let Some((said, used)) = project_fix {
            out["say"] = json!(format!(
                "I took \"{said}\" as {used}. {}",
                out["say"].as_str().unwrap_or("")
            ));
            out["corrected_from"] = json!([{"said": said, "used": used}]);
        }
        if let Some(c) = did_you_mean {
            out["did_you_mean"] = json!(c);
        }
        if let Some(c) = text_dym.filter(|c| !c.is_empty()) {
            out["did_you_mean"] = json!(c);
            out["say"] = json!("No exact match; the closest sessions are in did_you_mean.");
        }
        Ok(out)
    }

    /// The whole state for get_state.
    pub fn state_json(&self) -> Value {
        let now = chrono::Utc::now();
        json!({
            "modes": self.modes_line().trim(),
            "pending_answers": self.pending_answers_json(),
            "providers": self.providers_json(),
            "remote_control": self.remote_json(),
            "listening_paused": self.voice.paused.as_ref().map(|p| json!({"seconds_left": self.pause_left(), "reason": p.reason, "announcements_held": p.queued.len()})),
            "closed": self.closed.iter().rev().take(10).enumerate().map(|(i, c)| json!({"index": i, "tab": c.label, "account": c.account, "cwd": c.cwd, "loops": c.loops.len()})).collect::<Vec<_>>(),
            "accounts": (0..self.cfg.accounts.len()).map(|a| json!({
                "account": a + 1,
                "label": self.cfg.accounts[a].display(),
                "logged_in": self.accounts[a].login.logged_in(),
                "five_hour_left_pct": self.accounts[a].five_hour_left().map(|x| x.round()),
                "five_hour_reset_passed": self.accounts[a].five_hour_reset_passed(),
                "week_left_pct": self.week_left(a).map(|x| x.round()),
                "buckets": self.accounts[a].usage.as_ref().map(|u| u.windows.iter().map(|w| json!({
                    "bucket": w.label, "left_pct": w.left_now().round(), "reset_passed": w.reset_passed(), "resets_at": w.resets_at.map(|t| t.with_timezone(&chrono::Local).format("%a %b %-d %H:%M").to_string()),
                })).collect::<Vec<_>>()),
                "extra_usage": self.accounts[a].usage.as_ref().and_then(|u| u.extra.as_ref()).map(|e| json!({"enabled": e.enabled, "used": e.used, "limit": e.limit})),
                "effective_left_pct": self.accounts[a].effective_left().map(|x| x.round()),
                "limited_by": self.accounts[a].binding().map(|b| b.1),
                // Why there may be no usage numbers (QA-15).
                "usage_error": self.accounts[a].usage_err.as_ref().map(|e| match e {
                    crate::usage::UsageError::Unauthorized => "unauthorized: the login expired or was revoked".to_string(),
                    crate::usage::UsageError::RateLimited(_) => "rate_limited: the usage API asked to wait".to_string(),
                    crate::usage::UsageError::NotLoggedIn => "not_logged_in".to_string(),
                    other => format!("{other:?}"),
                }),
                "available": self.accounts[a].login.logged_in()
                    && !matches!(self.accounts[a].usage_err, Some(crate::usage::UsageError::Unauthorized))
                    && self.accounts[a].effective_left().is_none_or(|e| e >= 2.0),
                "next_reset_that_unblocks": self.accounts[a].binding().filter(|b| b.0 < 2.0).and_then(|b| b.2).map(|t| t.with_timezone(&chrono::Local).format("%a %b %-d %H:%M").to_string()),
                "tabs": self.panes.iter().filter(|p| p.account == Some(a)).map(|p| p.tabs.len()).sum::<usize>(),
                "assistant_runs_here": self.assistant.brain.as_ref().and_then(|b| b.account) == Some(a),
            })).collect::<Vec<_>>(),
            "tabs": self.all_tabs().into_iter().map(|(s, t)| self.tab_json(s, t)).collect::<Vec<_>>(),
            "loops": self.loops.iter().map(|r| json!({
                "job": r.lp.id, "tab": tab_id(r.uid), "cadence": r.lp.cadence(),
                "prompt": r.lp.prompt.chars().take(160).collect::<String>(), "next": r.lp.next_fire(now).map(|t| crate::loops::rel(t, now)),
            })).collect::<Vec<_>>(),
            "recent_deliveries": self.deliveries.iter().rev().take(10).map(|d| self.delivery_json(d.id)).collect::<Vec<_>>(),
            "pending_confirmations": self.pending_confirms.iter().map(|p| json!({
                "tool": p.tool, "token": p.token, "question": p.summary, "asked_s_ago": p.age().as_secs(),
                "answerable": p.answerable(),
            })).collect::<Vec<_>>(),
            "last_used_tab": self.last_target.filter(|u| self.find_tab(*u).is_some()).map(tab_id),
        })
    }

    /// "Alpha is out for the week (resets Fri 17:29); <best> has the most
    /// left." when account `a` has under 2% left, else None.
    pub fn out_warning(&self, a: usize) -> Option<String> {
        let (left, which, resets) = self.accounts.get(a)?.binding()?;
        if left >= 2.0 {
            return None;
        }
        let name = self.cfg.accounts[a].display();
        let when = resets
            .map(|t| {
                format!(
                    " (resets {})",
                    chrono::DateTime::<chrono::Local>::from(t).format("%a %H:%M")
                )
            })
            .unwrap_or_default();
        let span = if which == "weekly" {
            "for the week"
        } else {
            "for now (5 hour limit)"
        };
        let best = self
            .best_account()
            .filter(|(b, _)| *b != a)
            .map(|(b, l)| {
                format!(
                    " {} has the most left ({l:.0}%).",
                    self.cfg.accounts[b].display()
                )
            })
            .unwrap_or_default();
        Some(format!(
            "Warning: {name} is out {span}{when}, so it will not answer until then.{best}"
        ))
    }

    pub fn describe_tab(&self, slot: usize) -> String {
        self.panes[slot]
            .account
            .map(|a| self.cfg.accounts[a].display().to_string())
            .unwrap_or_else(|| format!("pane {}", slot + 1))
    }
}

/// A factual sentence for the assistant to say about deliveries.
pub fn say_deliveries(ds: &[Value], opened: usize) -> String {
    let n = |st: &str| ds.iter().filter(|d| d["status"] == st).count();
    let mut parts = vec![];
    if opened > 0 {
        parts.push(format!("Opened {}.", plural(opened, "tab")));
    }
    if ds.is_empty() {
        return parts.join(" ");
    }
    let (ok_, q, f, s) = (n("delivered"), n("queued"), n("failed"), n("sending"));
    if ok_ > 0 {
        parts.push(format!("Claude started on it in {}.", plural(ok_, "tab")));
    }
    if q + s > 0 {
        parts.push(format!(
            "{} not ready yet; it goes in when ready.",
            plural(q + s, "tab")
        ));
    }
    if f > 0 {
        let why: Vec<String> = ds
            .iter()
            .filter(|d| d["status"] == "failed")
            .map(|d| {
                format!(
                    "{}: {}",
                    d["tab"].as_str().unwrap_or(""),
                    d["reason"].as_str().unwrap_or("failed")
                )
            })
            .collect();
        parts.push(format!(
            "Failed in {} ({}).",
            plural(f, "tab"),
            why.join("; ")
        ));
    }
    parts.join(" ")
}

fn choice_of(s: &str) -> Option<Choice> {
    match s {
        "yes" | "approve" => Some(Choice::Approve),
        "always" => Some(Choice::Always),
        "no" | "deny" => Some(Choice::Deny),
        _ => None,
    }
}

/// Entries under `dir`, as paths relative to `root` ("src/", "main.rs 2 KB").
fn list_dir(root: &std::path::Path, dir: &std::path::Path, depth: usize, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut es: Vec<_> = rd.flatten().collect();
    es.sort_by_key(|e| e.file_name());
    for e in es {
        if out.len() > 200 {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if matches!(
            name.as_str(),
            ".git" | "node_modules" | "target" | ".DS_Store"
        ) {
            if e.path().is_dir() {
                out.push(format!(
                    "{}/ (skipped)",
                    e.path().strip_prefix(root).unwrap_or(&e.path()).display()
                ));
            }
            continue;
        }
        // Logins, keys and tokens are never listed.
        if crate::guard::denied(&e.path(), &[root.to_path_buf()]).is_some() {
            continue;
        }
        let rel = e
            .path()
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or(name);
        let Ok(md) = e.metadata() else { continue };
        if md.file_type().is_symlink() {
            out.push(format!("{rel} -> (link)"));
            continue;
        }
        if md.is_dir() {
            out.push(format!("{rel}/"));
            if depth > 1 {
                list_dir(root, &e.path(), depth - 1, out);
            }
        } else {
            let kb = md.len().div_ceil(1024);
            out.push(format!("{rel} {kb} KB"));
        }
    }
}

/// Where the socket and its token live.
pub fn socket_path() -> PathBuf {
    let p = crate::config::app_home().join("control.sock");
    // Unix socket paths are limited (104 bytes on macOS): a long home gets
    // a short one in the temp dir (clients read it from control.json).
    if p.as_os_str().len() < 100 {
        return p;
    }
    use sha2::Digest;
    let h = sha2::Sha256::digest(p.to_string_lossy().as_bytes());
    let uid = crate::platform::uid();
    crate::platform::short_socket_dir().join(format!(
        "godterm-{uid}-{:x}.sock",
        u32::from_be_bytes([h[0], h[1], h[2], h[3]])
    ))
}

/// On exit: remove the socket and token file if they are still ours.
pub fn cleanup(token: &str) {
    let tp = token_path();
    let mine = std::fs::read_to_string(&tp)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v["token"] == token);
    if mine {
        let _ = std::fs::remove_file(socket_path());
        let _ = std::fs::remove_file(&tp);
    }
}

pub fn token_path() -> PathBuf {
    crate::config::app_home().join("control.json")
}

/// Start the socket server: each line is {"token", "tool", "args"}, each
/// reply one line. Calls run on the UI thread through AppEvent::Control.
pub fn serve(events: Sender<AppEvent>) -> Result<String> {
    use crate::platform::{OpenOptionsExt, PermissionsExt};
    let path = socket_path();
    // Never take over a socket another GodTerm is serving.
    if UnixStream::connect(&path).is_ok() {
        return Err(anyhow!("another GodTerm is serving {}", path.display()));
    }
    let _ = std::fs::remove_file(&path);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    // Created owner only from the start (no window with default modes).
    let bound = crate::platform::with_umask(0o177, || UnixListener::bind(&path));
    let listener = bound.map_err(|e| anyhow!("binding {}: {e}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let fresh = || {
        format!(
            "{}{}",
            crate::session_ops::new_uuid(),
            crate::session_ops::new_uuid()
        )
        .replace('-', "")
    };
    // Two tokens per launch (QA-26): read-write for the assistant's own
    // brain only (handed over in its mcp.json, never written where tabs
    // can read it), and read-only for every other local client, unless
    // GodTerm runs with --control-rw (or for tests).
    let (token, ro) = (fresh(), fresh());
    let _ = TOKENS.set((token.clone(), ro.clone()));
    let shared = if control_rw() {
        token.clone()
    } else {
        ro.clone()
    };
    let access = if control_rw() {
        "read-write"
    } else {
        "read-only"
    };
    let tp = token_path();
    let tmp = tp.with_extension("json.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(
            json!({"socket": path, "token": shared, "access": access, "pid": std::process::id()})
                .to_string()
                .as_bytes(),
        )?;
    }
    std::fs::rename(&tmp, &tp)?;
    let tok = token.clone();
    std::thread::Builder::new()
        .name("control".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let (ev, tok, ro) = (events.clone(), tok.clone(), ro.clone());
                std::thread::spawn(move || handle(conn, ev, &tok, &ro));
            }
        })?;
    Ok(token)
}

/// This launch's (read-write, read-only) control tokens.
static TOKENS: std::sync::OnceLock<(String, String)> = std::sync::OnceLock::new();

/// The read-write token, for the brain's mcp.json only.
pub fn brain_token() -> Option<String> {
    TOKENS.get().map(|t| t.0.clone())
}

/// Other local clients may change things too: --control-rw, or tests.
pub fn control_rw() -> bool {
    test_control()
        || crate::config::env_var("CONTROL_RW").is_some_and(|v| v == "1")
        || std::env::args().any(|a| a == "--control-rw")
}

/// Equal without an early exit on the first differing byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn handle(conn: UnixStream, events: Sender<AppEvent>, token: &str, ro_token: &str) {
    let Ok(read) = conn.try_clone() else { return };
    let mut out = conn;
    for line in BufReader::new(read).lines() {
        let Ok(line) = line else { break };
        let reply = match serde_json::from_str::<Value>(&line) {
            // The read-only token: looking only.
            Ok(req)
                if req
                    .get("token")
                    .and_then(Value::as_str)
                    .is_some_and(|t| same(t, ro_token))
                    && !READ_ONLY
                        .contains(&req.get("tool").and_then(Value::as_str).unwrap_or(""))
                    || (req
                        .get("token")
                        .and_then(Value::as_str)
                        .is_some_and(|t| same(t, ro_token))
                        && req.get("user_says").is_some()) =>
            {
                let tool = req.get("tool").and_then(Value::as_str).unwrap_or("");
                crate::log::info(&format!("control: {tool} refused for a read-only client"));
                err(format!(
                    "this control client is read-only ({} only); start GodTerm with --control-rw to let other programs change things",
                    READ_ONLY.join(", ")
                ))
            }
            Ok(req)
                if req
                    .get("token")
                    .and_then(Value::as_str)
                    .is_some_and(|t| same(t, token) || same(t, ro_token)) =>
            {
                // A client acting for the user can answer a confirmation
                // (godterm mcp, the assistant's path, never sends this).
                if let Some(u) = req.get("user_says").and_then(Value::as_str) {
                    // Only for test harnesses: otherwise any local client
                    // holding the token could confirm destructive plans.
                    if !test_control() {
                        crate::log::info(
                            "control: refused user_says (start with GODTERM_TEST_CONTROL=1 to allow it)",
                        );
                        if writeln!(out, "{}", err("user confirmations come from the user"))
                            .is_err()
                        {
                            break;
                        }
                        continue;
                    }
                    crate::log::info(&format!("control: user_says accepted (test control): {u}"));
                    if events.send(AppEvent::UserTurn(u.to_string())).is_err() {
                        break;
                    }
                }
                let tool = req
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut args = req.get("args").cloned().unwrap_or(json!({}));
                // Socket clients are "external" unless they say "brain".
                let client = if req.get("client").and_then(Value::as_str) == Some("brain") {
                    "brain"
                } else {
                    "external"
                };
                if let Some(o) = args.as_object_mut() {
                    o.insert("_client".into(), json!(client));
                } else {
                    args = json!({"_client": client});
                }
                let (tx, rx) = std::sync::mpsc::channel();
                if events.send(AppEvent::Control(tool, args, tx)).is_err() {
                    break;
                }
                rx.recv_timeout(Duration::from_secs(20))
                    .unwrap_or_else(|_| err("godterm did not answer in time"))
            }
            Ok(_) => err("bad token"),
            Err(e) => err(format!("bad request: {e}")),
        };
        if writeln!(out, "{reply}").is_err() {
            break;
        }
    }
}

/// GodTerm was started for scripted tests (GODTERM_TEST_CONTROL=1 or
/// --test-control): the socket may then carry the user's answers.
pub fn test_control() -> bool {
    crate::config::env_var("TEST_CONTROL").is_some_and(|v| v == "1")
        || std::env::args().any(|a| a == "--test-control")
}

/// Tools that only look: never refused after a barge-in, never counted
/// against the per turn limit.
pub const READ_ONLY: &[&str] = &[
    "get_state",
    "account_capabilities",
    "mcp_catalog",
    "get_setting",
    "list_settings",
    "admin_history",
    "setup_status",
    "read_tab",
    "recent_turns",
    "list_dir",
    "read_file",
    "sessions",
    "session_detail",
    "history",
    "tab_history",
    "find",
    "list_learnings",
    "learning_history",
    "get_system_prompt",
    "prompt_history",
];

/// Client side (used by `godterm mcp`): one call over the socket.
pub fn call(tool: &str, args: &Value) -> Result<Value> {
    let info: Value = serde_json::from_str(
        &std::fs::read_to_string(token_path()).map_err(|_| anyhow!("godterm is not running"))?,
    )?;
    let sock = info
        .get("socket")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("no socket"))?;
    // The brain gets its own token through its mcp.json env.
    let own = crate::config::env_var("TOKEN");
    let token = own
        .as_deref()
        .or(info.get("token").and_then(Value::as_str))
        .unwrap_or("");
    let mut s = UnixStream::connect(sock)
        .map_err(|e| anyhow!("cannot reach godterm ({e}); is it running?"))?;
    s.set_read_timeout(Some(Duration::from_secs(25)))?;
    // The assistant's own brain says so (its mcp.json sets GODTERM_CLIENT).
    let client = crate::config::env_var("CLIENT").unwrap_or_else(|| "external".into());
    writeln!(
        s,
        "{}",
        json!({"token": token, "tool": tool, "args": args, "client": client})
    )?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line)?;
    Ok(serde_json::from_str(&line)?)
}

#[cfg(test)]
mod tests {
    /// Enter waits for the paste to show in claude's input box: its
    /// first words (however the box wraps spaces), or a long paste's
    /// stand in.
    #[test]
    fn a_paste_shows_in_the_input_box() {
        let p = "Create a simple calculator app.\nKeep it tiny.";
        assert!(super::shows_prompt(
            "> Create a simple calculator app. NL Keep",
            p
        ));
        assert!(super::shows_prompt("> Create a\n  simple calculator", p));
        assert!(super::shows_prompt("> [Pasted text #1 +40 lines]", p));
        assert!(!super::shows_prompt("> \n? for shortcuts", p));
    }

    use super::*;

    #[test]
    fn read_only_token_only_looks() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel::<AppEvent>();
        let t = std::thread::spawn(move || handle(server, tx, "rw", "ro"));
        let mut rd = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        writeln!(
            client,
            "{}",
            json!({"token": "ro", "tool": "close_tabs", "args": {"tab": "all"}})
        )
        .unwrap();
        rd.read_line(&mut line).unwrap();
        assert!(line.contains("read-only"), "{line}");
        writeln!(
            client,
            "{}",
            json!({"token": "ro", "tool": "get_state", "args": {}, "user_says": "yes"})
        )
        .unwrap();
        line.clear();
        rd.read_line(&mut line).unwrap();
        assert!(
            line.contains("read-only"),
            "a read-only client never answers for the user: {line}"
        );
        // A look goes through to the app.
        writeln!(
            client,
            "{}",
            json!({"token": "ro", "tool": "get_state", "args": {}})
        )
        .unwrap();
        match rx.recv_timeout(Duration::from_secs(2)).unwrap() {
            AppEvent::Control(tool, _, reply) => {
                assert_eq!(tool, "get_state");
                reply.send(json!({"ok": true})).unwrap();
            }
            _ => panic!("expected a control call"),
        }
        line.clear();
        rd.read_line(&mut line).unwrap();
        assert!(line.contains("\"ok\":true"));
        writeln!(client, "{}", json!({"token": "nope", "tool": "get_state"})).unwrap();
        line.clear();
        rd.read_line(&mut line).unwrap();
        assert!(line.contains("bad token"));
        drop(client);
        drop(rd);
        t.join().unwrap();
    }

    #[test]
    fn user_says_is_refused_outside_test_control() {
        if test_control() {
            return; // the suite itself runs with test control on
        }
        let (mut client, server) = UnixStream::pair().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || handle(server, tx, "tok", "ro"));
        writeln!(
            client,
            "{}",
            json!({"token": "tok", "tool": "confirm", "args": {}, "user_says": "yes"})
        )
        .unwrap();
        let mut line = String::new();
        BufReader::new(client.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert!(
            line.contains("user confirmations come from the user"),
            "{line}"
        );
        drop(client);
        t.join().unwrap();
        // Neither the turn nor the tool reached the app.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn every_tool_has_a_schema() {
        for t in TOOLS {
            let s = (t.schema)();
            assert_eq!(s["type"], "object", "{}", t.name);
            assert!(!t.description.is_empty());
        }
        let names: std::collections::HashSet<_> = TOOLS.iter().map(|t| t.name).collect();
        assert_eq!(names.len(), TOOLS.len());
    }
}

/// Where new folders may be made: the home folder (tests: the temp
/// folder, never the real home).
fn new_folder_root() -> PathBuf {
    if cfg!(test) {
        std::env::temp_dir()
    } else {
        crate::config::home_dir()
    }
}

/// Make `p` for a new tab: when its parent exists (or `deep`, after the
/// user's yes, any missing parents), only under the home folder, never a
/// system or private place. Err says why, or asks.
pub fn new_folder(p: &std::path::Path, deep: bool, allowed: &[PathBuf]) -> Result<PathBuf, String> {
    let shown = crate::config::tilde(p);
    if p.exists() {
        return Err(format!("{shown} is a file, not a folder"));
    }
    let root = std::fs::canonicalize(new_folder_root()).unwrap_or_else(|_| new_folder_root());
    // The nearest folder that exists, and what is missing below it.
    let mut base = p.to_path_buf();
    let mut missing: Vec<std::ffi::OsString> = vec![];
    while !base.is_dir() {
        match (base.file_name(), base.parent()) {
            (Some(n), Some(up)) => {
                missing.push(n.to_os_string());
                base = up.to_path_buf();
            }
            _ => return Err(format!("{shown} is not a folder")),
        }
    }
    let mut full = std::fs::canonicalize(&base).map_err(|e| format!("{shown}: {e}"))?;
    for n in missing.iter().rev() {
        if n == ".." || n == "." {
            return Err(format!("{shown} is not a folder"));
        }
        full.push(n);
    }
    if !full.starts_with(&root) || full == root {
        return Err(format!(
            "{shown} does not exist, and new folders are only made inside the home folder"
        ));
    }
    crate::guard::tab_folder(&full, allowed).map_err(|why| format!("refused: {why}"))?;
    if missing.len() > 1 && !deep {
        let parent = full.parent().map(crate::config::tilde).unwrap_or_default();
        return Err(format!(
            "{shown} does not exist and neither does {parent}: ask the user once whether to create it, then call again with create true"
        ));
    }
    std::fs::create_dir_all(&full).map_err(|e| format!("could not create {shown}: {e}"))?;
    crate::log::info(&format!(
        "control: created {} for a new tab",
        full.display()
    ));
    Ok(full)
}
